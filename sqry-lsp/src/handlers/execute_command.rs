use crate::handlers::code_action::{
    COMMAND_EXPLAIN_SYMBOL, COMMAND_SHOW_CALLERS, COMMAND_SHOW_REFERENCES,
};
use crate::handlers::references;
use crate::session::SessionManager;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tower_lsp::lsp_types::{
    PartialResultParams, Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};

/// Execute sqry-specific LSP commands (callers/references/explain).
///
/// # Errors
///
/// Returns an error when command arguments are malformed or when the
/// underlying handlers fail.
pub fn execute(session: &SessionManager, command: &str, args: Vec<Value>) -> Result<Option<Value>> {
    match command {
        COMMAND_SHOW_CALLERS => execute_references(session, command, args, false),
        COMMAND_SHOW_REFERENCES => execute_references(session, command, args, true),
        COMMAND_EXPLAIN_SYMBOL => execute_explain(session, args),
        _ => Err(anyhow!("unsupported command: {command}")),
    }
}

/// Execute references command using graph nodes for symbol resolution.
fn execute_references(
    session: &SessionManager,
    command: &str,
    args: Vec<Value>,
    include_declaration: bool,
) -> Result<Option<Value>> {
    let (uri, position) = parse_location_args(args)?;

    // Use graph-native node lookup for symbol metadata
    let symbol_json = session.node_at(&uri, position)?.map(|node| {
        let language = node.language.as_deref().unwrap_or("unknown").to_string();
        let name = node.name.clone();
        let qualified_name = node.display_qualified_name_or_name();
        json!({
            "name": name,
            "qualifiedName": qualified_name,
            "language": language,
        })
    });

    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration,
        },
    };
    let locations = references::handle(session, &params)?.unwrap_or_default();

    let payload = json!({
        "command": command,
        "context": {
            "uri": uri,
            "position": { "line": position.line, "character": position.character },
        },
        "symbol": symbol_json.unwrap_or(Value::Null),
        "results": {
            "count": locations.len(),
            "includeDeclaration": include_declaration,
            "locations": locations,
            "nextPageToken": Value::Null,
        }
    });

    Ok(Some(payload))
}

/// Execute explain command using graph nodes for symbol resolution.
fn execute_explain(session: &SessionManager, args: Vec<Value>) -> Result<Option<Value>> {
    let (uri, position) = parse_location_args(args)?;

    // Use graph-native node lookup for symbol metadata
    let Some(node) = session.node_at(&uri, position)? else {
        return Ok(Some(Value::Null));
    };

    let language = node
        .language
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let documentation = node.documentation.clone().unwrap_or_default();
    let signature = node
        .signature
        .clone()
        .unwrap_or_else(|| node.display_qualified_name_or_name());
    Ok(Some(json!({
        "name": node.name,
        "qualifiedName": node.display_qualified_name_or_name(),
        "language": language,
        "signature": signature,
        "documentation": documentation,
    })))
}

fn parse_location_args(args: Vec<Value>) -> Result<(Url, Position)> {
    let first = args
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("missing argument"))?;
    let uri = first
        .get("uri")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("uri missing"))?;
    let position = first
        .get("position")
        .ok_or_else(|| anyhow!("position missing"))?;
    // LSP uses u32 for positions; values >u32::MAX are invalid; clamp to max
    let line = position
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("line missing"))?
        .try_into()
        .unwrap_or(u32::MAX);
    let character = position
        .get("character")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("character missing"))?
        .try_into()
        .unwrap_or(u32::MAX);

    Ok((Url::parse(uri)?, Position::new(line, character)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── parse_location_args ───────────────────────────────────────────────────

    #[test]
    fn parse_location_args_valid_returns_uri_and_position() {
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs",
            "position": { "line": 5, "character": 10 }
        })];
        let (url, pos) = parse_location_args(args).unwrap();
        assert!(url.as_str().contains("main.rs"));
        assert_eq!(pos.line, 5);
        assert_eq!(pos.character, 10);
    }

    #[test]
    fn parse_location_args_empty_args_returns_error() {
        let result = parse_location_args(vec![]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("missing argument"));
    }

    #[test]
    fn parse_location_args_missing_uri_returns_error() {
        let args = vec![json!({
            "position": { "line": 0, "character": 0 }
        })];
        let result = parse_location_args(args);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("uri missing"));
    }

    #[test]
    fn parse_location_args_missing_position_returns_error() {
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs"
        })];
        let result = parse_location_args(args);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("position missing"));
    }

    #[test]
    fn parse_location_args_missing_line_returns_error() {
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs",
            "position": { "character": 0 }
        })];
        let result = parse_location_args(args);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line missing"));
    }

    #[test]
    fn parse_location_args_missing_character_returns_error() {
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs",
            "position": { "line": 0 }
        })];
        let result = parse_location_args(args);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("character missing"));
    }

    #[test]
    fn parse_location_args_line_zero_and_char_zero() {
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs",
            "position": { "line": 0, "character": 0 }
        })];
        let (_, pos) = parse_location_args(args).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);
    }

    #[test]
    fn parse_location_args_large_values_clamp_to_u32_max() {
        let u64_max = u64::MAX;
        let args = vec![json!({
            "uri": "file:///workspace/src/main.rs",
            "position": { "line": u64_max, "character": u64_max }
        })];
        let (_, pos) = parse_location_args(args).unwrap();
        assert_eq!(pos.line, u32::MAX);
        assert_eq!(pos.character, u32::MAX);
    }
}

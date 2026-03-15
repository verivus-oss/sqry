use crate::session::SessionManager;
use anyhow::Result;
use serde_json::json;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse, Command,
    Position,
};

pub const COMMAND_SHOW_CALLERS: &str = "sqry.showCallers";
pub const COMMAND_SHOW_REFERENCES: &str = "sqry.showReferences";
pub const COMMAND_EXPLAIN_SYMBOL: &str = "sqry.explainSymbol";

/// Build sqry-specific code actions for the current symbol.
///
/// # Errors
///
/// Returns an error when symbol lookup fails (propagated from the session).
pub fn handle(
    session: &SessionManager,
    params: &CodeActionParams,
) -> Result<Option<CodeActionResponse>> {
    let uri = &params.text_document.uri;
    let position = params.range.start;
    let Some(node) = session.node_at(uri, position)? else {
        return Ok(None);
    };

    let mut actions = Vec::new();
    actions.push(code_action(
        format!("Find Callers of {}", node.name),
        CodeActionKind::REFACTOR,
        COMMAND_SHOW_CALLERS,
        uri,
        position,
        true,
    ));
    actions.push(code_action(
        format!("Show References for {}", node.name),
        CodeActionKind::REFACTOR,
        COMMAND_SHOW_REFERENCES,
        uri,
        position,
        false,
    ));
    actions.push(code_action(
        format!("Explain {}", node.name),
        CodeActionKind::EMPTY,
        COMMAND_EXPLAIN_SYMBOL,
        uri,
        position,
        false,
    ));

    Ok(Some(actions))
}

fn code_action(
    title: String,
    kind: CodeActionKind,
    command: &str,
    uri: &tower_lsp::lsp_types::Url,
    position: Position,
    preferred: bool,
) -> CodeActionOrCommand {
    let arguments = json!({
        "uri": uri,
        "position": { "line": position.line, "character": position.character }
    });

    let command = Command {
        title: title.clone(),
        command: command.into(),
        arguments: Some(vec![arguments]),
    };

    #[allow(deprecated)]
    let code_action = CodeAction {
        title,
        kind: Some(kind),
        diagnostics: None,
        edit: None,
        command: Some(command),
        is_preferred: Some(preferred),
        disabled: None,
        data: None,
    };

    CodeActionOrCommand::CodeAction(code_action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::{Position, Url};

    fn test_uri() -> Url {
        Url::parse("file:///workspace/src/main.rs").unwrap()
    }

    // ── COMMAND_* constants ───────────────────────────────────────────────────

    #[test]
    fn command_constants_non_empty() {
        assert!(!COMMAND_SHOW_CALLERS.is_empty());
        assert!(!COMMAND_SHOW_REFERENCES.is_empty());
        assert!(!COMMAND_EXPLAIN_SYMBOL.is_empty());
    }

    #[test]
    fn command_constants_have_sqry_prefix() {
        assert!(COMMAND_SHOW_CALLERS.starts_with("sqry."));
        assert!(COMMAND_SHOW_REFERENCES.starts_with("sqry."));
        assert!(COMMAND_EXPLAIN_SYMBOL.starts_with("sqry."));
    }

    // ── code_action helper ────────────────────────────────────────────────────

    #[test]
    fn code_action_returns_code_action_variant() {
        let uri = test_uri();
        let pos = Position::new(0, 0);
        let result = code_action(
            "Test Title".to_string(),
            CodeActionKind::REFACTOR,
            "sqry.test",
            &uri,
            pos,
            false,
        );
        assert!(matches!(result, CodeActionOrCommand::CodeAction(_)));
    }

    #[test]
    fn code_action_title_preserved() {
        let uri = test_uri();
        let pos = Position::new(0, 0);
        let result = code_action(
            "My Action".to_string(),
            CodeActionKind::REFACTOR,
            "sqry.test",
            &uri,
            pos,
            false,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            assert_eq!(ca.title, "My Action");
        } else {
            panic!("expected CodeAction variant");
        }
    }

    #[test]
    fn code_action_kind_preserved() {
        let uri = test_uri();
        let pos = Position::new(0, 0);
        let result = code_action(
            "T".to_string(),
            CodeActionKind::EMPTY,
            "sqry.test",
            &uri,
            pos,
            false,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            assert_eq!(ca.kind, Some(CodeActionKind::EMPTY));
        } else {
            panic!("expected CodeAction variant");
        }
    }

    #[test]
    fn code_action_is_preferred_true_when_set() {
        let uri = test_uri();
        let pos = Position::new(3, 5);
        let result = code_action(
            "Preferred".to_string(),
            CodeActionKind::REFACTOR,
            "sqry.test",
            &uri,
            pos,
            true,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            assert_eq!(ca.is_preferred, Some(true));
        } else {
            panic!("expected CodeAction variant");
        }
    }

    #[test]
    fn code_action_is_preferred_false_when_not_set() {
        let uri = test_uri();
        let pos = Position::new(0, 0);
        let result = code_action(
            "T".to_string(),
            CodeActionKind::REFACTOR,
            "sqry.test",
            &uri,
            pos,
            false,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            assert_eq!(ca.is_preferred, Some(false));
        } else {
            panic!("expected CodeAction variant");
        }
    }

    #[test]
    fn code_action_command_name_set() {
        let uri = test_uri();
        let pos = Position::new(0, 0);
        let result = code_action(
            "T".to_string(),
            CodeActionKind::REFACTOR,
            COMMAND_SHOW_CALLERS,
            &uri,
            pos,
            false,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            let cmd = ca.command.expect("should have command");
            assert_eq!(cmd.command, COMMAND_SHOW_CALLERS);
        } else {
            panic!("expected CodeAction variant");
        }
    }

    #[test]
    fn code_action_command_arguments_contain_uri_and_position() {
        let uri = test_uri();
        let pos = Position::new(10, 4);
        let result = code_action(
            "T".to_string(),
            CodeActionKind::REFACTOR,
            "sqry.test",
            &uri,
            pos,
            false,
        );
        if let CodeActionOrCommand::CodeAction(ca) = result {
            let cmd = ca.command.expect("should have command");
            let args = cmd.arguments.expect("should have arguments");
            assert!(!args.is_empty());
            let arg = &args[0];
            let uri_val = arg.get("uri").expect("argument should contain 'uri'");
            assert!(uri_val.to_string().contains("main.rs"));
            let position_val = arg
                .get("position")
                .expect("argument should contain 'position'");
            let line = position_val
                .get("line")
                .expect("position should have 'line'")
                .as_u64()
                .unwrap();
            assert_eq!(line, 10);
        } else {
            panic!("expected CodeAction variant");
        }
    }
}

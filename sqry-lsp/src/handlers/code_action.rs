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

use crate::session::SessionManager;
use anyhow::Result;
use tower_lsp::lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

/// Provide hover information for the symbol at the requested position.
///
/// Uses graph builders to extract node information.
///
/// # Errors
///
/// Returns an error when symbol lookup or range conversion fails.
pub fn handle(session: &SessionManager, params: &HoverParams) -> Result<Option<Hover>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // Use graph-native node lookup at position
    let Some(node) = session.node_at(uri, position)? else {
        return Ok(None);
    };

    let language_str = node
        .language
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let signature = node
        .signature
        .clone()
        .unwrap_or_else(|| node.display_qualified_name_or_name());

    let mut sections = vec![format!("```{}\n{}\n```", language_str, signature)];

    if let Some(doc) = node.documentation.as_ref() {
        let trimmed = doc.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_string());
        }
    }

    sections.push(format!(
        "**Qualified**: {}",
        node.display_qualified_name_or_name()
    ));

    let value = sections.join("\n\n");

    // Convert byte-offset columns to UTF-16 for LSP
    let range = super::node_range_lsp(session, &node)?;

    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    }))
}

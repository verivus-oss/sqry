use crate::protocol::{RelationKind, SqryRelationParams};
use crate::session::SessionManager;
use anyhow::Result;
use tower_lsp::lsp_types::{Location, ReferenceParams};

/// Return reference locations for the requested symbol.
///
/// # Errors
///
/// Returns an error when symbol lookup fails or when converting ranges to LSP
/// coordinates fails.
pub fn handle(session: &SessionManager, params: &ReferenceParams) -> Result<Option<Vec<Location>>> {
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;

    let node = session.node_at(uri, position)?;
    let Some(node) = node else {
        return Ok(Some(Vec::new()));
    };

    let relation_params = SqryRelationParams {
        relation: RelationKind::Callers,
        target: node.qualified_name_or_name().to_string(),
        path: None,
        limit: None,
    };

    let response = crate::handlers::relations::execute(session, relation_params)?;
    let mut locations: Vec<Location> = response
        .results
        .into_iter()
        .map(|item| item.location)
        .collect();

    if params.context.include_declaration
        && let Ok(location) = crate::handlers::definition::node_location(session, &node)
    {
        locations.push(location);
    }

    Ok(Some(locations))
}

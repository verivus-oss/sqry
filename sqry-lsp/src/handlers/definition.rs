use crate::session::{NodeMatch, SessionManager};
use anyhow::{Result, anyhow};
use tower_lsp::lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Url};

/// Resolve the definition location for a symbol.
///
/// Uses graph builders to find the node at the cursor position.
/// This approach works for both saved and unsaved buffers via staging.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the symbol position or
/// when URI conversion fails.
pub fn handle(
    session: &SessionManager,
    params: &GotoDefinitionParams,
) -> Result<Option<GotoDefinitionResponse>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // Use graph-native node lookup at position
    let Some(node) = session.node_at(uri, position)? else {
        return Ok(None);
    };

    let location = node_location(session, &node)?;
    Ok(Some(GotoDefinitionResponse::Scalar(location)))
}

/// Convert a node into an LSP `Location`.
///
/// # Errors
///
/// Returns an error when the file path cannot be converted to a URI or when
/// range conversion fails.
pub fn node_location(session: &SessionManager, node: &NodeMatch) -> Result<Location> {
    let uri = Url::from_file_path(&node.file_path).map_err(|()| {
        anyhow!(
            "failed to convert '{}' into file URI",
            node.file_path.display()
        )
    })?;

    // Convert byte-offset columns from tree-sitter to UTF-16 for LSP
    let range = super::node_range_lsp(session, node)?;
    Ok(Location::new(uri, range))
}

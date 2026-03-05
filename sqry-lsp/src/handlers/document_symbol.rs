use crate::session::{NodeMatch, SessionManager};
use crate::utils::symbol_kind::node_kind_to_symbol_kind;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};

type SymbolKey = (usize, usize);
type DocumentChildrenMap = BTreeMap<Option<SymbolKey>, Vec<SymbolKey>>;
use tower_lsp::lsp_types::{DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse};

/// Return the document symbol tree for a file.
///
/// # Errors
///
/// Returns an error when symbol extraction fails or when range conversion is
/// not possible.
pub fn handle(
    session: &SessionManager,
    params: &DocumentSymbolParams,
) -> Result<Option<DocumentSymbolResponse>> {
    let uri = &params.text_document.uri;
    let mut nodes = session.nodes_in_document(uri)?;

    nodes.sort_by_key(|node| {
        (
            node.start_line,
            node.start_column,
            node.qualified_name_or_name().to_string(),
        )
    });

    let mut symbol_map: HashMap<SymbolKey, DocumentSymbol> = HashMap::with_capacity(nodes.len());
    let mut children_map: DocumentChildrenMap = BTreeMap::new();

    // First pass: build parent name -> key lookup
    let parent_lookup: HashMap<String, SymbolKey> = nodes
        .iter()
        .map(|node| {
            (
                node.qualified_name_or_name().to_string(),
                (node.start_line as usize, node.start_column as usize),
            )
        })
        .collect();

    for node in &nodes {
        let key = (node.start_line as usize, node.start_column as usize);
        let parent_key = node
            .qualified_name
            .as_deref()
            .and_then(parent_qualified_name)
            .and_then(|parent_name| parent_lookup.get(&parent_name).copied());

        let doc = document_symbol_from(session, node)?;
        symbol_map.insert(key, doc);
        children_map.entry(parent_key).or_default().push(key);
    }

    let roots = build_document_symbol_tree(None, &mut symbol_map, &children_map);
    Ok(Some(DocumentSymbolResponse::Nested(roots)))
}

fn document_symbol_from(session: &SessionManager, node: &NodeMatch) -> Result<DocumentSymbol> {
    let detail = node.signature.clone();
    let range = super::node_range_lsp(session, node)?;
    let display_name = display_symbol_name(node);
    #[allow(deprecated)]
    Ok(DocumentSymbol {
        name: display_name,
        detail,
        kind: node_kind_to_symbol_kind(node.kind),
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    })
}

fn display_symbol_name(node: &NodeMatch) -> String {
    let full = node.qualified_name.as_deref().unwrap_or(&node.name);
    if let Some(pos) = full.rfind("::") {
        return full[(pos + 2)..].to_string();
    }
    if let Some(pos) = full.rfind('.') {
        return full[(pos + 1)..].to_string();
    }
    node.name.clone()
}

fn parent_qualified_name(qualified_name: &str) -> Option<String> {
    if let Some(pos) = qualified_name.rfind("::")
        && pos > 0
    {
        return Some(qualified_name[..pos].to_string());
    }
    if let Some(pos) = qualified_name.rfind('.')
        && pos > 0
    {
        return Some(qualified_name[..pos].to_string());
    }
    None
}

fn build_document_symbol_tree(
    parent: Option<SymbolKey>,
    symbol_map: &mut HashMap<SymbolKey, DocumentSymbol>,
    children_map: &DocumentChildrenMap,
) -> Vec<DocumentSymbol> {
    let mut nodes = Vec::new();
    if let Some(child_keys) = children_map.get(&parent) {
        for key in child_keys {
            if let Some(mut node) = symbol_map.remove(key) {
                let child_nodes = build_document_symbol_tree(Some(*key), symbol_map, children_map);
                if !child_nodes.is_empty() {
                    node.children = Some(child_nodes);
                }
                nodes.push(node);
            }
        }
    }
    nodes
}

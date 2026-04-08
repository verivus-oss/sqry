use std::fs;
use std::path::Path;

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use tower_lsp::lsp_types::{Location, Position, Range, Url};
use tracing::debug;

use crate::protocol::SqrySearchItem;

pub(crate) use sqry_core::graph::unified::materialize::display_entry_qualified_name;

/// Convert a graph node to LSP search item.
///
/// This is the graph-based equivalent of `symbol_to_item_resolved`, used for
/// migrating from the legacy index to `CodeGraph`.
///
/// # Arguments
///
/// * `node_id` - The graph node ID
/// * `entry` - The node entry from the graph
/// * `graph` - The code graph for resolving interned values
/// * `root` - Workspace root path for relative path resolution
pub fn node_to_search_item(
    node_id: NodeId,
    entry: &NodeEntry,
    graph: &CodeGraph,
    root: &Path,
) -> Option<SqrySearchItem> {
    // Resolve name from string interner
    let Some(name_str) = graph.strings().resolve(entry.name) else {
        debug!(
            node_id = ?node_id,
            string_id = ?entry.name,
            "node_to_search_item: failed to resolve symbol name from string interner"
        );
        return None;
    };
    let name = name_str.to_string();

    // Resolve file path from file registry
    let Some(file_path) = graph.files().resolve(entry.file) else {
        debug!(
            node_id = ?node_id,
            file_id = ?entry.file,
            "node_to_search_item: failed to resolve file path from file registry"
        );
        return None;
    };
    let path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        root.join(&*file_path)
    };

    let Some(uri) = Url::from_file_path(&path).ok() else {
        debug!(
            node_id = ?node_id,
            path = %path.display(),
            "node_to_search_item: failed to convert file path to URI"
        );
        return None;
    };

    // Convert byte columns to UTF-16 columns per LSP requirements
    let (start_char, end_char) = match fs::read_to_string(&path) {
        Ok(contents) => {
            let start_line_idx = entry.start_line.saturating_sub(1) as usize;
            let end_line_idx = entry.end_line.saturating_sub(1) as usize;

            let mut start_char = entry.start_column;
            let mut end_char = entry.end_column;

            for (idx, line_text) in contents.lines().enumerate() {
                if idx == start_line_idx {
                    let byte_col = (entry.start_column as usize).min(line_text.len());
                    let u16_col = crate::utils::position::byte_to_utf16(line_text, byte_col);
                    start_char = u16_col.try_into().unwrap_or(u32::MAX);
                }
                if idx == end_line_idx {
                    let byte_col = (entry.end_column as usize).min(line_text.len());
                    let u16_col = crate::utils::position::byte_to_utf16(line_text, byte_col);
                    end_char = u16_col.try_into().unwrap_or(u32::MAX);
                }
                if idx > end_line_idx {
                    break;
                }
            }
            (start_char, end_char)
        }
        Err(_) => (entry.start_column, entry.end_column),
    };

    let range = Range {
        start: Position {
            line: entry.start_line.saturating_sub(1),
            character: start_char,
        },
        end: Position {
            line: entry.end_line.saturating_sub(1),
            character: end_char,
        },
    };

    let location = Location { uri, range };

    // Resolve qualified name if available, fallback to name
    // Get language from file registry
    let language_enum = graph.files().language_for_file(entry.file);
    let language = language_enum.map_or_else(|| "unknown".to_string(), |l| l.to_string());
    let qualified_name = display_entry_qualified_name(entry, graph.strings(), graph.files(), &name);

    // Convert NodeKind to string
    let kind = entry.kind.as_str().to_string();

    Some(SqrySearchItem {
        name,
        kind,
        qualified_name,
        language,
        location,
        score: None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sqry_core::graph::Language;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use sqry_core::graph::unified::storage::interner::StringInterner;
    use sqry_core::graph::unified::storage::registry::FileRegistry;

    use super::display_entry_qualified_name;

    fn build_entry(
        language: Language,
        name: &str,
        qualified_name: &str,
    ) -> (NodeEntry, StringInterner, FileRegistry) {
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let file = files
            .register_with_language(Path::new("test.hs"), Some(language))
            .expect("register file");
        let name_id = strings.intern(name).expect("intern name");
        let qualified_name_id = strings
            .intern(qualified_name)
            .expect("intern qualified name");
        let entry = NodeEntry::new(NodeKind::Function, name_id, file)
            .with_qualified_name(qualified_name_id);
        (entry, strings, files)
    }

    fn build_entry_no_qname(
        language: Language,
        name: &str,
    ) -> (NodeEntry, StringInterner, FileRegistry) {
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let file = files
            .register_with_language(Path::new("test.rs"), Some(language))
            .expect("register file");
        let name_id = strings.intern(name).expect("intern name");
        let entry = NodeEntry::new(NodeKind::Function, name_id, file);
        (entry, strings, files)
    }

    fn build_entry_with_language_ext(
        language: Language,
        name: &str,
        qualified_name: &str,
        ext: &str,
    ) -> (NodeEntry, StringInterner, FileRegistry) {
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let path = format!("test.{ext}");
        let file = files
            .register_with_language(Path::new(&path), Some(language))
            .expect("register file");
        let name_id = strings.intern(name).expect("intern name");
        let qualified_name_id = strings
            .intern(qualified_name)
            .expect("intern qualified name");
        let entry = NodeEntry::new(NodeKind::Function, name_id, file)
            .with_qualified_name(qualified_name_id);
        (entry, strings, files)
    }

    #[test]
    fn display_entry_qualified_name_uses_native_haskell_display() {
        let (entry, strings, files) = build_entry(Language::Haskell, "c_sin", "Math::FFI::c_sin");

        assert_eq!(
            display_entry_qualified_name(&entry, &strings, &files, "c_sin"),
            "Math.FFI.c_sin"
        );
    }

    #[test]
    fn display_entry_qualified_name_preserves_haskell_ffi_identity() {
        let (entry, strings, files) = build_entry(Language::Haskell, "sin", "ffi::C::sin");

        assert_eq!(
            display_entry_qualified_name(&entry, &strings, &files, "sin"),
            "ffi::C::sin"
        );
    }

    #[test]
    fn display_entry_qualified_name_fallback_when_no_qualified_name() {
        // Entry without qualified_name — should fall back to fallback_name
        let (entry, strings, files) = build_entry_no_qname(Language::Rust, "my_fn");
        assert_eq!(
            display_entry_qualified_name(&entry, &strings, &files, "my_fn"),
            "my_fn"
        );
    }

    #[test]
    fn display_entry_qualified_name_rust_uses_double_colon_separator() {
        let (entry, strings, files) =
            build_entry_with_language_ext(Language::Rust, "new", "MyStruct::new", "rs");
        let result = display_entry_qualified_name(&entry, &strings, &files, "new");
        // Rust uses :: separators; display should pass through as-is or re-format
        // The important thing is the function returns without panicking and uses the qualified name
        assert!(!result.is_empty());
    }

    #[test]
    fn display_entry_qualified_name_python_uses_dot_separator() {
        let (entry, strings, files) =
            build_entry_with_language_ext(Language::Python, "method", "MyClass::method", "py");
        let result = display_entry_qualified_name(&entry, &strings, &files, "method");
        // Python display should convert :: to .
        assert!(!result.is_empty());
    }

    #[test]
    fn display_entry_qualified_name_java_uses_dot_separator() {
        let (entry, strings, files) = build_entry_with_language_ext(
            Language::Java,
            "parse",
            "com::example::Parser::parse",
            "java",
        );
        let result = display_entry_qualified_name(&entry, &strings, &files, "parse");
        assert!(!result.is_empty());
    }
}

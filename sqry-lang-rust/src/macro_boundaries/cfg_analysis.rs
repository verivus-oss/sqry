//! cfg/cfg_attr conditional compilation analysis (4.5e).
//!
//! Parses `#[cfg()]` and `#[cfg_attr()]` attributes into structured predicates,
//! stores cfg metadata on gated items, and emits `MacroExpansion{CfgGate}` edges
//! for `cfg_attr` (which conditionally applies another attribute).
//!
//! # Predicate Model
//!
//! The parser treats ALL cfg keys as opaque strings — it does not validate key
//! names against a whitelist. Custom cfg keys (`#[cfg(my_custom_flag)]`) work
//! automatically. Validation only happens at evaluation time when
//! `active_cfg_flags` is provided.
//!
//! # cfg_attr Nesting
//!
//! `cfg_attr` can nest recursively:
//! ```ignore
//! #[cfg_attr(feature = "serde", cfg_attr(feature = "json", derive(Serialize)))]
//! ```
//!
//! The parser handles arbitrary nesting depth.

use sqry_core::graph::Span;
use sqry_core::graph::node::Position;
use sqry_core::graph::unified::{
    GraphBuildHelper, MacroExpansionKind, NodeId, NodeKind, NodeMetadataStore,
};
use tree_sitter::Node;

/// A structured representation of a cfg predicate.
///
/// Covers all forms recognized by rustc:
/// - Platform flags: `unix`, `windows`, `target_os`, etc.
/// - Build flags: `test`, `debug_assertions`, `proc_macro`, `doctest`
/// - Feature flags: `feature = "name"`
/// - Custom flags from `--cfg` and `Cargo.toml` `[features]`
/// - Compound: arbitrary nesting of `all()`, `any()`, `not()`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgPredicate {
    /// Simple flag: `cfg(test)`, `cfg(unix)`, `cfg(doctest)`
    Flag(String),
    /// Key-value: `cfg(feature = "serde")`, `cfg(target_arch = "x86_64")`
    KeyValue { key: String, value: String },
    /// Negation: `cfg(not(...))`
    Not(Box<CfgPredicate>),
    /// All: `cfg(all(a, b, c))`
    All(Vec<CfgPredicate>),
    /// Any: `cfg(any(a, b, c))`
    Any(Vec<CfgPredicate>),
}

impl CfgPredicate {
    /// Evaluate the predicate against a set of active cfg flags and features.
    ///
    /// Returns `true` if the predicate is satisfied by the given configuration.
    ///
    /// # Arguments
    ///
    /// * `active_cfg_flags` — flags like `"unix"`, `"test"`, `"debug_assertions"`
    /// * `active_features` — feature names like `"serde"`, `"json"`
    #[must_use]
    pub fn evaluate(&self, active_cfg_flags: &[String], active_features: &[String]) -> bool {
        match self {
            Self::Flag(flag) => active_cfg_flags.iter().any(|f| f == flag),
            Self::KeyValue { key, value } => {
                if key == "feature" {
                    active_features.iter().any(|f| f == value)
                } else {
                    // For non-feature key-values (e.g., target_arch = "x86_64"),
                    // check if the full key=value pair is in active_cfg_flags.
                    // Accept multiple formats to be robust:
                    //   key="value"  (rustc format)
                    //   key = "value" (spaced format)
                    //   key=value (unquoted)
                    active_cfg_flags.iter().any(|f| {
                        let normalized = f.replace(' ', "");
                        let target_noquote = format!("{key}={value}");
                        let target_quoted = format!("{key}=\"{value}\"");
                        normalized == target_noquote || normalized == target_quoted
                    })
                }
            }
            Self::Not(inner) => !inner.evaluate(active_cfg_flags, active_features),
            Self::All(predicates) => predicates
                .iter()
                .all(|p| p.evaluate(active_cfg_flags, active_features)),
            Self::Any(predicates) => predicates
                .iter()
                .any(|p| p.evaluate(active_cfg_flags, active_features)),
        }
    }

    /// Format the predicate as a human-readable string.
    #[must_use]
    pub fn to_condition_string(&self) -> String {
        match self {
            Self::Flag(flag) => flag.clone(),
            Self::KeyValue { key, value } => format!("{key} = \"{value}\""),
            Self::Not(inner) => format!("not({})", inner.to_condition_string()),
            Self::All(predicates) => {
                let parts: Vec<_> = predicates.iter().map(Self::to_condition_string).collect();
                format!("all({})", parts.join(", "))
            }
            Self::Any(predicates) => {
                let parts: Vec<_> = predicates.iter().map(Self::to_condition_string).collect();
                format!("any({})", parts.join(", "))
            }
        }
    }
}

/// Analyze cfg and cfg_attr attributes on an item node.
///
/// For each `#[cfg(...)]` attribute:
/// - Parses the predicate into a structured [`CfgPredicate`]
/// - Stores `cfg_condition` and optionally `cfg_active` in metadata
///
/// For each `#[cfg_attr(...)]` attribute:
/// - Parses the predicate
/// - Stores metadata
/// - Emits `MacroExpansion{CfgGate}` edge (conditional attribute application)
///
/// # Arguments
///
/// * `item_node` — tree-sitter node for the gated item
/// * `content` — source file bytes
/// * `item_id` — the graph `NodeId` already assigned to this item
/// * `helper` — graph build helper for creating nodes and edges
/// * `metadata_store` — sparse metadata store
/// * `active_cfg_flags` — currently active cfg flags for evaluation
/// * `active_features` — currently active feature flags for evaluation
pub fn analyze_cfg_attributes(
    item_node: Node,
    content: &[u8],
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
    active_cfg_flags: &[String],
    active_features: &[String],
) {
    let mut sibling = item_node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() != "attribute_item" {
            break;
        }

        let attr_text = match sib.utf8_text(content) {
            Ok(text) => text,
            Err(_) => {
                sibling = sib.prev_sibling();
                continue;
            }
        };

        // Check for #[cfg(...)]
        if let Some(predicate_text) = extract_cfg_content(attr_text, "cfg")
            && let Some(predicate) = parse_predicate(predicate_text)
        {
            let condition_str = predicate.to_condition_string();

            let cfg_active = if active_cfg_flags.is_empty() && active_features.is_empty() {
                None // No config provided — cannot evaluate
            } else {
                Some(predicate.evaluate(active_cfg_flags, active_features))
            };

            let meta = metadata_store.get_or_insert_default(item_id);
            meta.cfg_condition = Some(condition_str.clone());
            meta.cfg_active = cfg_active;

            log::debug!(
                "cfg({}) on node {:?}, active: {:?}",
                condition_str,
                item_id,
                cfg_active
            );
        }

        // Check for #[cfg_attr(...)]
        if let Some(cfg_attr_content) = extract_cfg_content(attr_text, "cfg_attr") {
            process_cfg_attr(
                sib,
                cfg_attr_content,
                item_id,
                helper,
                metadata_store,
                active_cfg_flags,
                active_features,
            );
        }

        sibling = sib.prev_sibling();
    }
}

/// Extract the content inside `#[kind(...)]` from the full attribute text.
///
/// For `#[cfg(test)]`, with kind="cfg", returns `Some("test")`.
/// For `#[cfg_attr(feature = "serde", derive(Serialize))]`, with kind="cfg_attr",
/// returns `Some("feature = \"serde\", derive(Serialize)")`.
fn extract_cfg_content<'a>(attr_text: &'a str, kind: &str) -> Option<&'a str> {
    let trimmed = attr_text.trim();
    let inner = trimmed.strip_prefix("#[")?.strip_suffix(']')?;
    let after_kind = inner.strip_prefix(kind)?;
    let content = after_kind.strip_prefix('(')?;
    // Find the matching closing paren, accounting for nesting.
    let close_pos = find_matching_close_paren(content)?;
    Some(&content[..close_pos])
}

/// Find the position of the matching closing parenthesis, handling nesting.
fn find_matching_close_paren(s: &str) -> Option<usize> {
    let mut depth = 0u32;
    let mut in_string = false;
    let mut prev_char = '\0';

    for (i, ch) in s.char_indices() {
        if ch == '"' && prev_char != '\\' {
            in_string = !in_string;
        } else if !in_string {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
        }
        prev_char = ch;
    }
    // If no explicit close paren found but we consumed the whole string,
    // the outer paren was at the end.
    None
}

/// Parse a cfg predicate string into a structured [`CfgPredicate`].
///
/// Handles:
/// - Simple flags: `"test"`, `"unix"`
/// - Key-value: `"feature = \"serde\""`
/// - Negation: `"not(test)"`
/// - Conjunction: `"all(unix, test)"`
/// - Disjunction: `"any(unix, windows)"`
/// - Compound nesting: `"all(target_arch = \"x86_64\", any(unix, target_os = \"macos\"))"`
pub fn parse_predicate(text: &str) -> Option<CfgPredicate> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Check for compound forms: not(...), all(...), any(...)
    if let Some(inner) = strip_compound(trimmed, "not") {
        let inner_pred = parse_predicate(inner)?;
        return Some(CfgPredicate::Not(Box::new(inner_pred)));
    }

    if let Some(inner) = strip_compound(trimmed, "all") {
        let parts = split_top_level_commas(inner);
        let predicates: Vec<_> = parts.iter().filter_map(|p| parse_predicate(p)).collect();
        if predicates.is_empty() {
            return None;
        }
        return Some(CfgPredicate::All(predicates));
    }

    if let Some(inner) = strip_compound(trimmed, "any") {
        let parts = split_top_level_commas(inner);
        let predicates: Vec<_> = parts.iter().filter_map(|p| parse_predicate(p)).collect();
        if predicates.is_empty() {
            return None;
        }
        return Some(CfgPredicate::Any(predicates));
    }

    // Check for key = "value" pattern.
    if let Some((key, value)) = parse_key_value(trimmed) {
        return Some(CfgPredicate::KeyValue {
            key: key.to_string(),
            value: value.to_string(),
        });
    }

    // Simple flag.
    if trimmed.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(CfgPredicate::Flag(trimmed.to_string()));
    }

    // Unrecognized pattern — log and return None.
    log::debug!("Unrecognized cfg predicate: '{}'", trimmed);
    None
}

/// Strip a compound prefix like `"not("` from a predicate string.
///
/// Returns the inner content if the prefix matches, e.g.:
/// `strip_compound("not(test)", "not")` → `Some("test")`
fn strip_compound<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let after = text.strip_prefix(prefix)?;
    let inner = after.strip_prefix('(')?;
    let close = find_matching_close_paren(inner)?;
    Some(&inner[..close])
}

/// Split a string on commas at the top level (not inside parentheses or quotes).
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0u32;
    let mut in_string = false;
    let mut prev_char = '\0';
    let mut start = 0;

    for (i, ch) in s.char_indices() {
        if ch == '"' && prev_char != '\\' {
            in_string = !in_string;
        } else if !in_string {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                }
                ',' if depth == 0 => {
                    results.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        prev_char = ch;
    }

    if start <= s.len() {
        let remainder = s[start..].trim();
        if !remainder.is_empty() {
            results.push(&s[start..]);
        }
    }

    results
}

/// Parse a `key = "value"` pattern.
fn parse_key_value(text: &str) -> Option<(&str, &str)> {
    let eq_pos = text.find('=')?;
    let key = text[..eq_pos].trim();
    let value_part = text[eq_pos + 1..].trim();
    // Strip surrounding quotes.
    let value = value_part.strip_prefix('"')?.strip_suffix('"')?;
    Some((key, value))
}

/// Process a `#[cfg_attr(...)]` attribute.
///
/// Format: `cfg_attr(predicate, attr1, attr2, ...)`
/// The first argument is the condition, subsequent arguments are attributes
/// to apply when the condition is true.
fn process_cfg_attr(
    attr_node: Node,
    content: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
    active_cfg_flags: &[String],
    active_features: &[String],
) {
    // Split the cfg_attr content into predicate and attributes.
    let parts = split_top_level_commas(content);
    if parts.is_empty() {
        return;
    }

    let predicate_text = parts[0].trim();
    let predicate = match parse_predicate(predicate_text) {
        Some(p) => p,
        None => return,
    };

    let condition_str = predicate.to_condition_string();

    let cfg_active = if active_cfg_flags.is_empty() && active_features.is_empty() {
        None
    } else {
        Some(predicate.evaluate(active_cfg_flags, active_features))
    };

    // Store the cfg metadata.
    let meta = metadata_store.get_or_insert_default(item_id);
    meta.cfg_condition = Some(condition_str.clone());
    meta.cfg_active = cfg_active;

    // Emit MacroExpansion{CfgGate} edge — cfg_attr conditionally applies
    // macro attributes.
    let span = Span::new(
        Position::new(
            attr_node.start_position().row,
            attr_node.start_position().column,
        ),
        Position::new(
            attr_node.end_position().row,
            attr_node.end_position().column,
        ),
    );

    let cfg_gate_qualified = format!(
        "cfg_attr::{}@{}:{}",
        condition_str,
        attr_node.start_position().row + 1,
        attr_node.start_position().column
    );
    let gate_id = helper.add_node(&cfg_gate_qualified, Some(span), NodeKind::Macro);
    helper.add_macro_expansion_edge(item_id, gate_id, MacroExpansionKind::CfgGate, false);

    // Handle nested cfg_attr: if any of the conditional attributes is itself
    // a cfg_attr, recurse.
    for attr_part in &parts[1..] {
        let trimmed = attr_part.trim();
        if let Some(nested_content) = trimmed.strip_prefix("cfg_attr(")
            && let Some(close_pos) = find_matching_close_paren(nested_content)
        {
            let nested = &nested_content[..close_pos];
            process_cfg_attr(
                attr_node,
                nested,
                item_id,
                helper,
                metadata_store,
                active_cfg_flags,
                active_features,
            );
        }
    }

    log::debug!(
        "cfg_attr({}) on node {:?}, active: {:?}",
        condition_str,
        item_id,
        cfg_active
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::Language;
    use sqry_core::graph::unified::{NodeKind, StagingGraph};
    use std::path::Path;
    use tree_sitter::Parser;

    fn parse_rust(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    fn find_node_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node_by_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_cfg_test() {
        let source = r#"
#[cfg(test)]
mod tests {
    fn test_something() {}
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let mod_node = find_node_by_kind(tree.root_node(), "mod_item").unwrap();
        let mod_id = helper.add_node("tests", None, NodeKind::Module);

        analyze_cfg_attributes(
            mod_node,
            source.as_bytes(),
            mod_id,
            &mut helper,
            &mut metadata_store,
            &["test".to_string()],
            &[],
        );

        let meta = metadata_store.get(mod_id).expect("metadata should exist");
        assert_eq!(meta.cfg_condition.as_deref(), Some("test"));
        assert_eq!(meta.cfg_active, Some(true));
    }

    #[test]
    fn test_cfg_feature() {
        let source = r#"
#[cfg(feature = "serde")]
fn serde_support() {}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("serde_support", None, NodeKind::Function);

        analyze_cfg_attributes(
            func,
            source.as_bytes(),
            func_id,
            &mut helper,
            &mut metadata_store,
            &[],
            &["serde".to_string()],
        );

        let meta = metadata_store.get(func_id).expect("metadata should exist");
        assert_eq!(meta.cfg_condition.as_deref(), Some("feature = \"serde\""));
        assert_eq!(meta.cfg_active, Some(true));
    }

    #[test]
    fn test_cfg_not() {
        let pred = parse_predicate("not(test)").unwrap();
        assert_eq!(
            pred,
            CfgPredicate::Not(Box::new(CfgPredicate::Flag("test".to_string())))
        );
        assert!(!pred.evaluate(&["test".to_string()], &[]));
        assert!(pred.evaluate(&[], &[]));
    }

    #[test]
    fn test_cfg_all() {
        let pred = parse_predicate("all(unix, test)").unwrap();
        match &pred {
            CfgPredicate::All(parts) => assert_eq!(parts.len(), 2),
            other => panic!("Expected All, got {other:?}"),
        }
        assert!(pred.evaluate(&["unix".to_string(), "test".to_string()], &[]));
        assert!(!pred.evaluate(&["unix".to_string()], &[]));
    }

    #[test]
    fn test_cfg_any() {
        let pred = parse_predicate("any(unix, windows)").unwrap();
        match &pred {
            CfgPredicate::Any(parts) => assert_eq!(parts.len(), 2),
            other => panic!("Expected Any, got {other:?}"),
        }
        assert!(pred.evaluate(&["unix".to_string()], &[]));
        assert!(pred.evaluate(&["windows".to_string()], &[]));
        assert!(!pred.evaluate(&[], &[]));
    }

    #[test]
    fn test_cfg_attr_simple() {
        let source = r#"
#[cfg_attr(feature = "serde", derive(Serialize))]
struct MyStruct { x: u32 }
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let struct_node = find_node_by_kind(tree.root_node(), "struct_item").unwrap();
        let struct_id = helper.add_node("MyStruct", None, NodeKind::Struct);

        analyze_cfg_attributes(
            struct_node,
            source.as_bytes(),
            struct_id,
            &mut helper,
            &mut metadata_store,
            &[],
            &["serde".to_string()],
        );

        let meta = metadata_store
            .get(struct_id)
            .expect("metadata should exist");
        assert_eq!(meta.cfg_condition.as_deref(), Some("feature = \"serde\""));
        assert_eq!(meta.cfg_active, Some(true));

        // Should have emitted a CfgGate MacroExpansion edge (creates a Macro node).
        let macro_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Macro)
            .count();
        assert!(
            macro_count > 0,
            "cfg_attr should create a CfgGate Macro node"
        );
    }

    #[test]
    fn test_cfg_active_evaluation() {
        let pred = CfgPredicate::KeyValue {
            key: "feature".to_string(),
            value: "json".to_string(),
        };

        assert!(pred.evaluate(&[], &["json".to_string()]));
        assert!(!pred.evaluate(&[], &["xml".to_string()]));
        assert!(!pred.evaluate(&[], &[]));
    }

    #[test]
    fn test_cfg_on_mod() {
        let source = r#"
#[cfg(unix)]
mod unix_only {
    pub fn platform_specific() {}
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let mod_node = find_node_by_kind(tree.root_node(), "mod_item").unwrap();
        let mod_id = helper.add_node("unix_only", None, NodeKind::Module);

        analyze_cfg_attributes(
            mod_node,
            source.as_bytes(),
            mod_id,
            &mut helper,
            &mut metadata_store,
            &[],
            &[],
        );

        let meta = metadata_store.get(mod_id).expect("metadata should exist");
        assert_eq!(meta.cfg_condition.as_deref(), Some("unix"));
        // No active_cfg_flags provided, so cfg_active should be None.
        assert_eq!(meta.cfg_active, None);
    }

    #[test]
    fn test_cfg_target_arch() {
        let pred = parse_predicate("target_arch = \"x86_64\"").unwrap();
        assert_eq!(
            pred,
            CfgPredicate::KeyValue {
                key: "target_arch".to_string(),
                value: "x86_64".to_string(),
            }
        );
    }

    #[test]
    fn test_cfg_compound_nested() {
        let pred =
            parse_predicate("all(target_arch = \"x86_64\", any(unix, target_os = \"macos\"))")
                .unwrap();
        match &pred {
            CfgPredicate::All(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    CfgPredicate::KeyValue { key, value } => {
                        assert_eq!(key, "target_arch");
                        assert_eq!(value, "x86_64");
                    }
                    other => panic!("Expected KeyValue, got {other:?}"),
                }
                match &parts[1] {
                    CfgPredicate::Any(inner) => assert_eq!(inner.len(), 2),
                    other => panic!("Expected Any, got {other:?}"),
                }
            }
            other => panic!("Expected All, got {other:?}"),
        }
    }

    #[test]
    fn test_predicate_to_condition_string() {
        assert_eq!(
            CfgPredicate::Flag("test".into()).to_condition_string(),
            "test"
        );
        assert_eq!(
            CfgPredicate::KeyValue {
                key: "feature".into(),
                value: "serde".into()
            }
            .to_condition_string(),
            "feature = \"serde\""
        );
        assert_eq!(
            CfgPredicate::Not(Box::new(CfgPredicate::Flag("test".into()))).to_condition_string(),
            "not(test)"
        );
        assert_eq!(
            CfgPredicate::All(vec![
                CfgPredicate::Flag("unix".into()),
                CfgPredicate::Flag("test".into()),
            ])
            .to_condition_string(),
            "all(unix, test)"
        );
    }

    #[test]
    fn test_extract_cfg_content() {
        assert_eq!(extract_cfg_content("#[cfg(test)]", "cfg"), Some("test"));
        assert_eq!(
            extract_cfg_content("#[cfg(feature = \"serde\")]", "cfg"),
            Some("feature = \"serde\"")
        );
        assert_eq!(
            extract_cfg_content("#[cfg_attr(test, derive(Debug))]", "cfg_attr"),
            Some("test, derive(Debug)")
        );
        assert_eq!(extract_cfg_content("#[inline]", "cfg"), None);
        assert_eq!(extract_cfg_content("#[cfg_attr(test)]", "cfg"), None);
    }

    #[test]
    fn test_split_top_level_commas() {
        let parts = split_top_level_commas("a, b, c");
        assert_eq!(parts.len(), 3);

        let parts = split_top_level_commas("all(a, b), c");
        assert_eq!(parts.len(), 2);

        let parts = split_top_level_commas("feature = \"serde\", derive(Serialize)");
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_cfg_attr_nested() {
        let source = r#"
#[cfg_attr(feature = "serde", cfg_attr(feature = "json", derive(Serialize)))]
struct Nested { x: u32 }
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let struct_node = find_node_by_kind(tree.root_node(), "struct_item").unwrap();
        let struct_id = helper.add_node("Nested", None, NodeKind::Struct);

        analyze_cfg_attributes(
            struct_node,
            source.as_bytes(),
            struct_id,
            &mut helper,
            &mut metadata_store,
            &[],
            &["serde".to_string(), "json".to_string()],
        );

        // Should have metadata set (from the outer cfg_attr).
        let meta = metadata_store
            .get(struct_id)
            .expect("metadata should exist");
        assert!(meta.cfg_condition.is_some());
    }
}

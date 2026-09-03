//! Pulumi `GraphBuilder` implementation for unified graph extraction.
//!
//! Extracts nodes and edges from Pulumi YAML/JSON stack files:
//! - Resources, outputs, config, variables, packages, and type references.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Position, Span,
    unified::{GraphBuildHelper, StagingGraph},
};
use tree_sitter::{Node, Tree};

use crate::{PulumiFormat, detect_format};
use sqry_core::graph::unified::node::NodeKind;

#[derive(Debug, Default)]
pub struct PulumiGraphBuilder;

impl GraphBuilder for PulumiGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let format = detect_format(content);
        let Some(root_value) = parse_root_value(tree, content, format) else {
            return Ok(());
        };

        let Value::Map(root_entries) = root_value else {
            return Ok(());
        };

        let mut helper = GraphBuildHelper::new(staging, file, Language::Pulumi);
        let module_id = helper.add_module("<module>", None);
        // issue #394: real declaration; opt dual-use bare helper into is_definition
        helper.mark_definition(module_id);

        if let Some(package_entry) = find_entry(&root_entries, "package")
            && let Some(package_name) = package_entry.value.as_str()
        {
            let package_node = format!("package.{package_name}");
            // `package_entry.span` is the mapping KEY's extent, literally the
            // `package` token, which the referenced package does not own
            // (issue #748).
            let package_id = match package_entry.span {
                Some(span) => helper.add_call_site_node(&package_node, span, NodeKind::Module),
                None => helper.add_module(&package_node, None),
            };
            helper.add_import_edge(module_id, package_id);
        }

        if let Some(resources_entry) = find_entry(&root_entries, "resources")
            && let Value::Map(resources) = &resources_entry.value
        {
            for resource_entry in resources {
                let resource_node = format!("resources.{}", resource_entry.key);
                let resource_id = helper.add_resource(&resource_node, resource_entry.span);
                helper.add_defines_edge(module_id, resource_id);

                if let Value::Map(resource_body) = &resource_entry.value {
                    apply_resource_edges(resource_id, resource_body, &mut helper);
                }
            }
        }

        if let Some(config_entry) = find_entry(&root_entries, "config")
            && let Value::Map(config_entries) = &config_entry.value
        {
            for entry in config_entries {
                let config_node = format!("config.{}", entry.key);
                let config_id = helper.add_variable(&config_node, entry.span);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(config_id);
                helper.add_defines_edge(module_id, config_id);
            }
        }

        if let Some(vars_entry) = find_entry(&root_entries, "variables")
            && let Value::Map(var_entries) = &vars_entry.value
        {
            for entry in var_entries {
                let var_node = format!("variables.{}", entry.key);
                let var_id = helper.add_variable(&var_node, entry.span);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(var_id);
                helper.add_defines_edge(module_id, var_id);
            }
        }

        if let Some(outputs_entry) = find_entry(&root_entries, "outputs")
            && let Value::Map(outputs) = &outputs_entry.value
        {
            for output_entry in outputs {
                let output_node = format!("outputs.{}", output_entry.key);
                let output_id = helper.add_variable(&output_node, output_entry.span);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(output_id);
                helper.add_defines_edge(module_id, output_id);
                helper.add_export_edge(module_id, output_id);

                if let Value::Map(output_body) = &output_entry.value
                    && let Some(value_entry) = find_entry(output_body, "value")
                {
                    let refs = collect_interpolation_references(&value_entry.value);
                    add_reference_edges(output_id, refs, &mut helper);
                }
            }
        }

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Pulumi
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(pulumi_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Pulumi YAML/JSON stack files.
///
/// Pulumi stacks are pure data (YAML or JSON); the grammar has no function/method
/// definition and no control-flow node, and this plugin emits no `NodeKind::Function`
/// or `NodeKind::Method`, so the build seam never attaches a descriptor for a Pulumi
/// file. The mapping is still implemented (AC-1: no plugin omitted) over the YAML
/// grammar (the canonical Pulumi format); its control-flow table is honestly empty
/// because YAML has no control-flow kinds. The coverage test asserts both halves of
/// the declarative contract: the map registers no buckets, and no eligible
/// function-with-body node is produced. Shared via [`pulumi_shape_mapping`].
pub struct PulumiShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl PulumiShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-yaml grammar.
    fn build() -> Self {
        // `tree_sitter_yaml::language()` returns a `Language` directly (not a
        // `LanguageFn`), so no `.into()` here.
        let lang: tree_sitter::Language = tree_sitter_yaml::language();
        let count = lang.node_kind_count();
        let mut cf_by_kind_id = vec![None; count];
        for (id, slot) in cf_by_kind_id.iter_mut().enumerate() {
            let Ok(kind_id) = u16::try_from(id) else {
                break;
            };
            if !lang.node_kind_is_named(kind_id) {
                continue;
            }
            if let Some(name) = lang.node_kind_for_id(kind_id) {
                *slot = cf_bucket_for_pulumi_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for PulumiShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, _fn_node: Node, _src: &[u8]) -> SignatureShape {
        // Pulumi stack documents have no parameter lists; the signature shape is
        // empty by construction (honest minimal impl for a data-only language).
        SignatureShape::default()
    }
}

/// Map one tree-sitter-yaml grammar node-kind name to its canonical control-flow
/// bucket. YAML is a pure data language with no control flow, so this is an honest
/// total `None`; the function exists to keep the build seam (`cf_bucket_for_*`)
/// uniform across plugins.
fn cf_bucket_for_pulumi_kind(_name: &str) -> Option<CfBucket> {
    None
}

/// The process-wide Pulumi shape mapping, built once on first use.
#[must_use]
pub fn pulumi_shape_mapping() -> &'static PulumiShapeMapping {
    static MAPPING: OnceLock<PulumiShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(PulumiShapeMapping::build)
}

#[derive(Debug, Clone)]
struct MapEntry {
    key: String,
    span: Option<Span>,
    value: Value,
}

#[derive(Debug, Clone)]
enum Value {
    Map(Vec<MapEntry>),
    Seq(Vec<Value>),
    Str(String),
    Number,
    Bool,
    Null,
}

impl Value {
    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PulumiReference {
    Resource(String),
    Config(String),
}

fn apply_resource_edges(
    resource_id: sqry_core::graph::unified::NodeId,
    entries: &[MapEntry],
    helper: &mut GraphBuildHelper,
) {
    if let Some(type_entry) = find_entry(entries, "type")
        && let Some(type_name) = type_entry.value.as_str()
    {
        let type_node = format!("type.{type_name}");
        let type_id = helper.add_type(&type_node, type_entry.span);
        helper.add_typeof_edge(resource_id, type_id);
    }

    let mut all_refs = Vec::new();

    if let Some(depends_entry) = find_entry(entries, "dependsOn") {
        all_refs.extend(collect_depends_on_references(&depends_entry.value));
    }

    if let Some(properties_entry) = find_entry(entries, "properties") {
        all_refs.extend(collect_interpolation_references(&properties_entry.value));
    }

    add_reference_edges(resource_id, all_refs, helper);
}

fn find_entry<'a>(entries: &'a [MapEntry], key: &str) -> Option<&'a MapEntry> {
    entries.iter().find(|entry| entry.key == key)
}

fn parse_root_value(tree: &Tree, content: &[u8], format: PulumiFormat) -> Option<Value> {
    let root = tree.root_node();
    let value_node = find_root_value_node(root, format)?;

    match format {
        PulumiFormat::Json => parse_json_value(value_node, content),
        PulumiFormat::Yaml => parse_yaml_value(value_node, content),
    }
}

fn find_root_value_node(root: Node<'_>, format: PulumiFormat) -> Option<Node<'_>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if matches_root_kind(kind, format) {
            return Some(node);
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

fn matches_root_kind(kind: &str, format: PulumiFormat) -> bool {
    match format {
        PulumiFormat::Json => {
            matches!(
                kind,
                "object" | "array" | "string" | "number" | "true" | "false" | "null"
            )
        }
        PulumiFormat::Yaml => matches!(
            kind,
            "block_mapping"
                | "flow_mapping"
                | "block_sequence"
                | "flow_sequence"
                | "string_scalar"
                | "plain_scalar"
                | "double_quote_scalar"
                | "single_quote_scalar"
                | "integer_scalar"
                | "float_scalar"
                | "true"
                | "false"
                | "null"
                | "null_scalar"
        ),
    }
}

fn parse_yaml_value(node: Node<'_>, content: &[u8]) -> Option<Value> {
    match node.kind() {
        "block_mapping" | "flow_mapping" => Some(Value::Map(parse_yaml_mapping(node, content))),
        "block_sequence" | "flow_sequence" => Some(Value::Seq(parse_yaml_sequence(node, content))),
        "string_scalar" | "plain_scalar" | "double_quote_scalar" | "single_quote_scalar" => {
            Some(Value::Str(decode_yaml_string(node, content)))
        }
        "integer_scalar" | "float_scalar" => Some(Value::Number),
        "true" | "false" => Some(Value::Bool),
        "null" | "null_scalar" => Some(Value::Null),
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(value) = parse_yaml_value(child, content) {
                    return Some(value);
                }
            }
            None
        }
    }
}

fn parse_yaml_mapping(node: Node<'_>, content: &[u8]) -> Vec<MapEntry> {
    let mut entries = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind().ends_with("mapping_pair")
            && let Some(entry) = parse_yaml_mapping_pair(child, content)
        {
            entries.push(entry);
        }
    }
    entries
}

fn parse_yaml_mapping_pair(node: Node<'_>, content: &[u8]) -> Option<MapEntry> {
    let mut cursor = node.walk();
    let mut children = node.named_children(&mut cursor);
    let key_node = children.next()?;
    let key_text = node_text(key_node, content)?;
    let key = sanitize_scalar(&key_text);
    let value_node = children.next();
    let value = value_node
        .and_then(|child| parse_yaml_value(child, content))
        .unwrap_or(Value::Null);

    Some(MapEntry {
        key,
        span: Some(span_from_node(key_node)),
        value,
    })
}

fn parse_yaml_sequence(node: Node<'_>, content: &[u8]) -> Vec<Value> {
    let mut values = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind().ends_with("sequence_item") {
            if let Some(item) = child
                .named_child(0)
                .and_then(|n| parse_yaml_value(n, content))
            {
                values.push(item);
            }
        } else if let Some(value) = parse_yaml_value(child, content) {
            values.push(value);
        }
    }
    values
}

fn parse_json_value(node: Node<'_>, content: &[u8]) -> Option<Value> {
    match node.kind() {
        "object" => Some(Value::Map(parse_json_object(node, content))),
        "array" => Some(Value::Seq(parse_json_array(node, content))),
        "string" => Some(Value::Str(decode_json_string(node, content))),
        "number" => Some(Value::Number),
        "true" | "false" => Some(Value::Bool),
        "null" => Some(Value::Null),
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(value) = parse_json_value(child, content) {
                    return Some(value);
                }
            }
            None
        }
    }
}

fn parse_json_object(node: Node<'_>, content: &[u8]) -> Vec<MapEntry> {
    let mut entries = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "pair"
            && let Some(entry) = parse_json_pair(child, content)
        {
            entries.push(entry);
        }
    }
    entries
}

fn parse_json_pair(node: Node<'_>, content: &[u8]) -> Option<MapEntry> {
    let mut cursor = node.walk();
    let mut children = node.named_children(&mut cursor);
    let key_node = children.next()?;
    let key = decode_json_string(key_node, content);
    let value_node = children.next();
    let value = value_node
        .and_then(|child| parse_json_value(child, content))
        .unwrap_or(Value::Null);

    Some(MapEntry {
        key,
        span: Some(span_from_node(key_node)),
        value,
    })
}

fn parse_json_array(node: Node<'_>, content: &[u8]) -> Vec<Value> {
    let mut values = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(value) = parse_json_value(child, content) {
            values.push(value);
        }
    }
    values
}

fn node_text(node: Node<'_>, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(str::to_string)
}

fn sanitize_scalar(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        let last = trimmed.as_bytes()[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn decode_yaml_string(node: Node<'_>, content: &[u8]) -> String {
    node_text(node, content)
        .map(|text| sanitize_scalar(&text))
        .unwrap_or_default()
}

fn decode_json_string(node: Node<'_>, content: &[u8]) -> String {
    let Some(text) = node_text(node, content) else {
        return String::new();
    };
    let trimmed = text.trim();
    if trimmed.len() < 2 {
        return trimmed.to_string();
    }
    let bytes = trimmed.as_bytes();
    if bytes[0] != b'"' || bytes[trimmed.len() - 1] != b'"' {
        return trimmed.to_string();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut output = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                match escaped {
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    '\\' => output.push('\\'),
                    '"' => output.push('"'),
                    _ => output.push(escaped),
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

fn collect_depends_on_references(value: &Value) -> Vec<PulumiReference> {
    let mut references = Vec::new();
    collect_depends_on_references_inner(value, &mut references);
    references
}

fn collect_depends_on_references_inner(value: &Value, references: &mut Vec<PulumiReference>) {
    match value {
        Value::Str(text) => {
            if let Some(reference) = parse_depends_on_reference(text) {
                references.push(reference);
            }
        }
        Value::Seq(values) => {
            for item in values {
                collect_depends_on_references_inner(item, references);
            }
        }
        _ => {}
    }
}

fn parse_depends_on_reference(text: &str) -> Option<PulumiReference> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("resources.") {
        let name = rest.split('.').next().unwrap_or("").trim();
        if !name.is_empty() {
            return Some(PulumiReference::Resource(name.to_string()));
        }
    }
    Some(PulumiReference::Resource(trimmed.to_string()))
}

fn collect_interpolation_references(value: &Value) -> Vec<PulumiReference> {
    let mut refs = Vec::new();
    collect_interpolation_references_inner(value, &mut refs);
    refs
}

fn collect_interpolation_references_inner(value: &Value, refs: &mut Vec<PulumiReference>) {
    match value {
        Value::Str(text) => {
            refs.extend(extract_interpolations(text));
        }
        Value::Map(entries) => {
            for entry in entries {
                collect_interpolation_references_inner(&entry.value, refs);
            }
        }
        Value::Seq(values) => {
            for item in values {
                collect_interpolation_references_inner(item, refs);
            }
        }
        _ => {}
    }
}

/// Extract Pulumi interpolation references from a string.
///
/// Supported patterns:
/// - `${resources.<name>.*}` → `PulumiReference::Resource`
/// - `${config.<key>.*}` → `PulumiReference::Config`
///
/// Rejected patterns:
/// - `$${...}` (escaped, skipped)
/// - `${unclosed` (malformed, skipped)
/// - `${a ${b}}` (nested, skipped)
fn extract_interpolations(text: &str) -> Vec<PulumiReference> {
    let bytes = text.as_bytes();
    let mut refs = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'$' && bytes[index + 1] == b'{' {
            if index > 0 && bytes[index - 1] == b'$' {
                index += 2;
                continue;
            }
            let start = index + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end >= bytes.len() {
                break;
            }
            if let Ok(inner) = std::str::from_utf8(&bytes[start..end])
                && !inner.contains("${")
                && let Some(reference) = parse_interpolation_reference(inner.trim())
            {
                refs.push(reference);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    refs
}

fn parse_interpolation_reference(inner: &str) -> Option<PulumiReference> {
    if let Some(rest) = inner.strip_prefix("resources.") {
        let name = rest.split('.').next().unwrap_or("").trim();
        if !name.is_empty() {
            return Some(PulumiReference::Resource(name.to_string()));
        }
    }
    if let Some(rest) = inner.strip_prefix("config.") {
        let name = rest.split('.').next().unwrap_or("").trim();
        if !name.is_empty() {
            return Some(PulumiReference::Config(name.to_string()));
        }
    }
    None
}

fn add_reference_edges(
    source: sqry_core::graph::unified::NodeId,
    references: Vec<PulumiReference>,
    helper: &mut GraphBuildHelper,
) {
    let mut seen = HashSet::new();
    for reference in references {
        let (target_id, key) = match reference {
            PulumiReference::Resource(name) => {
                let node = helper.add_resource(&format!("resources.{name}"), None);
                (node, format!("resource:{name}"))
            }
            PulumiReference::Config(name) => {
                let node = helper.add_variable(&format!("config.{name}"), None);
                (node, format!("config:{name}"))
            }
        };
        if seen.insert(key) {
            helper.add_reference_edge(source, target_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn reference_keys(references: &[PulumiReference]) -> HashSet<String> {
        references
            .iter()
            .map(|reference| match reference {
                PulumiReference::Resource(name) => format!("resource:{name}"),
                PulumiReference::Config(name) => format!("config:{name}"),
            })
            .collect()
    }

    #[test]
    fn test_extract_interpolations_skips_escaped() {
        let refs = extract_interpolations("$${resources.skip} ${resources.keep.id}");
        let keys = reference_keys(&refs);
        assert!(!keys.contains("resource:skip"));
        assert!(keys.contains("resource:keep"));
    }

    #[test]
    fn test_extract_interpolations_handles_multiple() {
        let refs = extract_interpolations("${resources.app.id} ${config.env}");
        let keys = reference_keys(&refs);
        assert!(keys.contains("resource:app"));
        assert!(keys.contains("config:env"));
    }

    #[test]
    fn test_extract_interpolations_ignores_unclosed() {
        let refs = extract_interpolations("${resources.broken");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_interpolations_empty_string() {
        let refs = extract_interpolations("");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_interpolations_no_interpolations() {
        let refs = extract_interpolations("just plain text without any references");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_parse_depends_on_bare_name() {
        let reference = parse_depends_on_reference("myRes");
        assert_eq!(
            reference,
            Some(PulumiReference::Resource("myRes".to_string()))
        );
    }

    #[test]
    fn test_parse_depends_on_with_prefix() {
        let reference = parse_depends_on_reference("resources.myRes");
        assert_eq!(
            reference,
            Some(PulumiReference::Resource("myRes".to_string()))
        );
    }

    // ----- body-shape descriptor coverage -----

    const SHAPE_SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/iac/Pulumi.yaml"
    ));

    #[test]
    fn builder_advertises_shape_mapping() {
        assert!(
            PulumiGraphBuilder.shape_mapping().is_some(),
            "Pulumi builder must advertise a ShapeMapping (AC-1: no plugin omitted)"
        );
    }

    #[test]
    fn yaml_control_flow_map_is_honestly_empty() {
        // YAML is a pure data language: no node kind is a control-flow construct, so
        // the mapping registers zero buckets. This asserts the map is honestly total
        // `None` rather than carrying spurious arms.
        let mapping = pulumi_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_yaml::language();
        let mut any_bucket = false;
        for id in 0..lang.node_kind_count() {
            if let Ok(kid) = u16::try_from(id)
                && mapping.cf_bucket(kid).is_some()
            {
                any_bucket = true;
            }
        }
        assert!(
            !any_bucket,
            "YAML has no control-flow kinds; the Pulumi map must register no buckets"
        );
    }

    #[test]
    fn declarative_no_function_or_method_body_nodes() {
        use sqry_core::graph::unified::build::body_hash::has_valid_body_span;
        use sqry_core::graph::unified::node::NodeKind;
        use std::path::PathBuf;

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_yaml::language())
            .expect("load yaml grammar");
        let tree = parser.parse(SHAPE_SAMPLE, None).expect("parse yaml");

        let mut staging = StagingGraph::new();
        let builder = PulumiGraphBuilder;
        let file = PathBuf::from("Pulumi.yaml");
        builder
            .build_graph(&tree, SHAPE_SAMPLE.as_bytes(), &file, &mut staging)
            .unwrap();

        // The honest declarative contract: Pulumi emits no Function/Method node at
        // all, so no node carries a body span the seam would fingerprint.
        let eligible = staging
            .nodes()
            .filter(|n| {
                matches!(n.entry.kind, NodeKind::Function | NodeKind::Method)
                    && has_valid_body_span(n.entry)
            })
            .count();
        assert_eq!(
            eligible, 0,
            "Pulumi is declarative data: no Function/Method node should carry a body span"
        );
    }
}

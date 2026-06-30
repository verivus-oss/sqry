//! Graph builder for `ServiceNow` XML record extraction.

use std::path::Path;
use std::sync::OnceLock;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{GraphBuilder, GraphResult};
use tree_sitter::{Node, Tree};

use crate::detection::RecordType;
use crate::extraction::{extract_scripts, extract_table_definition, extract_table_schema};
use crate::metadata::RecordMetadata;
use crate::replay::ReplayState;

/// Maximum XML file size to parse.
///
/// Override: `SQRY_SN_XML_MAX_FILE_SIZE` (bytes). Clamped to 1 MB – 50 MB.
const DEFAULT_MAX_XML_FILE_SIZE: usize = 10 * 1024 * 1024; // 10 MB
const MIN_MAX_XML_FILE_SIZE: usize = 1024 * 1024; // 1 MB
const MAX_MAX_XML_FILE_SIZE: usize = 50 * 1024 * 1024; // 50 MB

/// Maximum number of record elements per XML file.
///
/// Override: `SQRY_SN_XML_MAX_RECORDS` (count). Clamped to 10 – 5 000.
const DEFAULT_MAX_RECORDS_PER_FILE: usize = 500;
const MIN_MAX_RECORDS_PER_FILE: usize = 10;
const MAX_MAX_RECORDS_PER_FILE: usize = 5_000;

fn max_xml_file_size() -> usize {
    std::env::var("SQRY_SN_XML_MAX_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_XML_FILE_SIZE)
        .clamp(MIN_MAX_XML_FILE_SIZE, MAX_MAX_XML_FILE_SIZE)
}

fn max_records_per_file() -> usize {
    std::env::var("SQRY_SN_XML_MAX_RECORDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_RECORDS_PER_FILE)
        .clamp(MIN_MAX_RECORDS_PER_FILE, MAX_MAX_RECORDS_PER_FILE)
}

/// Graph builder for `ServiceNow` XML update set files.
#[derive(Debug, Default)]
pub struct ServiceNowXmlGraphBuilder;

impl GraphBuilder for ServiceNowXmlGraphBuilder {
    fn build_graph(
        &self,
        _tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Size guard
        if content.len() > max_xml_file_size() {
            return Ok(());
        }

        // Fast pre-check: scan for "record_update" before expensive parse
        if !crate::detection::fast_precheck(content) {
            return Ok(());
        }

        // UTF-8 only. ServiceNow exports are UTF-8.
        let Ok(xml_str) = std::str::from_utf8(content) else {
            return Ok(());
        };

        // Parse with roxmltree (malformed → empty graph)
        let Ok(doc) = roxmltree::Document::parse(xml_str) else {
            return Ok(());
        };

        // Verify root element is record_update
        let root = doc.root_element();
        if root.tag_name().name() != "record_update" {
            return Ok(());
        }

        let table = root.attribute("table").unwrap_or("");
        let Some(record_type) = RecordType::from_table(table) else {
            return Ok(());
        };

        // Collect record elements (with budget to prevent resource exhaustion)
        let record_elems: Vec<_> = root
            .children()
            .filter(roxmltree::Node::is_element)
            .collect();
        if record_elems.len() > max_records_per_file() {
            return Ok(());
        }

        match &record_type {
            RecordType::Script(script_fields) => {
                self.build_script_graph(&record_elems, script_fields, table, file, staging)?;
            }
            RecordType::TableSchema => {
                let mut helper = GraphBuildHelper::new(staging, file, Language::ServiceNow);
                let module_id = helper.add_module("<module>", None);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(module_id);

                for record_elem in &record_elems {
                    extract_table_schema(record_elem, module_id, &mut helper);
                }
            }
            RecordType::TableDefinition => {
                let mut helper = GraphBuildHelper::new(staging, file, Language::ServiceNow);
                let module_id = helper.add_module("<module>", None);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(module_id);

                for record_elem in &record_elems {
                    extract_table_definition(record_elem, module_id, &mut helper);
                }
            }
        }

        Ok(())
    }

    fn language(&self) -> Language {
        Language::ServiceNow
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(servicenow_xml_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for `ServiceNow` XML update-set records.
///
/// This plugin is declarative: it extracts `ServiceNow` record metadata and table
/// schemas from XML, and delegates any embedded server-side script to the
/// ServiceNow-Xanadu (JavaScript) builder. The XML plugin itself emits no
/// `NodeKind::Function`/`Method` whose span resolves inside its own (HTML) parse
/// tree, so the build seam never attaches a descriptor through this mapping (a
/// delegated JS function node's span does not point at an HTML subtree, so the
/// seam's exact-span match returns `None`). The mapping is still implemented (AC-1:
/// no plugin omitted) over the HTML grammar this plugin's `language()` parses; its
/// control-flow table is honestly empty because HTML/XML markup has no control flow.
/// Shared via [`servicenow_xml_shape_mapping`].
pub struct ServiceNowXmlShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl ServiceNowXmlShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-html grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_html::LANGUAGE.into();
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
                *slot = cf_bucket_for_xml_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for ServiceNowXmlShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, _fn_node: Node, _src: &[u8]) -> SignatureShape {
        // XML markup has no parameter list; the signature shape is empty by
        // construction (honest minimal impl for a declarative markup language).
        SignatureShape::default()
    }
}

/// Map one tree-sitter-html grammar node-kind name to its canonical control-flow
/// bucket. HTML/XML markup is pure structure with no control flow, so this is an
/// honest total `None`; the function exists to keep the build seam uniform across
/// plugins.
fn cf_bucket_for_xml_kind(_name: &str) -> Option<CfBucket> {
    None
}

/// The process-wide `ServiceNow` XML shape mapping, built once on first use.
#[must_use]
pub fn servicenow_xml_shape_mapping() -> &'static ServiceNowXmlShapeMapping {
    static MAPPING: OnceLock<ServiceNowXmlShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(ServiceNowXmlShapeMapping::build)
}

impl ServiceNowXmlGraphBuilder {
    /// Handle script-bearing records: single-record delegates directly,
    /// multi-record uses separate staging per delegation with replay.
    #[allow(clippy::unused_self)] // Method uses `self` for trait implementation consistency
    fn build_script_graph(
        &self,
        record_elems: &[roxmltree::Node<'_, '_>],
        script_fields: &[&str],
        table: &str,
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let sn_plugin = sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin::new();
        let sn_builder = sqry_lang_servicenow_xanadu::ServiceNowGraphBuilder;

        if record_elems.len() == 1 {
            // Common case: single record → delegate directly into main staging
            let metadata = RecordMetadata::extract(&record_elems[0], table);
            extract_scripts(
                &record_elems[0],
                script_fields,
                &metadata,
                file,
                0,
                false,
                &sn_plugin,
                &sn_builder,
                staging,
            )?;
        } else {
            // Rare case: multi-record → separate staging per delegation + replay
            let mut replay_state = ReplayState::new(staging);
            for (idx, record_elem) in record_elems.iter().enumerate() {
                let metadata = RecordMetadata::extract(record_elem, table);
                let mut del_staging = StagingGraph::new();
                extract_scripts(
                    record_elem,
                    script_fields,
                    &metadata,
                    file,
                    idx,
                    true,
                    &sn_plugin,
                    &sn_builder,
                    &mut del_staging,
                )?;

                if !del_staging.is_empty() {
                    replay_state.replay(staging, &mut del_staging)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod shape_tests {
    use super::{
        CfBucket, GraphBuilder, ServiceNowXmlGraphBuilder, ShapeMapping,
        servicenow_xml_shape_mapping,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/iac/update_set.xml"
    ));

    #[test]
    fn builder_advertises_shape_mapping() {
        // Cheap, deterministic check (no parse): the ShapeMapping impl is present so
        // AC-1 holds. This runs in the default suite; the body-walking coverage test
        // below is the HighWallClock one that is gated out.
        assert!(
            ServiceNowXmlGraphBuilder.shape_mapping().is_some(),
            "ServiceNow XML builder must advertise a ShapeMapping (AC-1: no plugin omitted)"
        );
        // The fixture is consumed by the gated test below; reference it here so the
        // include is load-bearing even when the gated test is filtered out.
        assert!(SAMPLE.contains("record_update"));
    }

    /// HighWallClock coverage: ServiceNow XML is a high-cost plugin (HTML parse +
    /// roxmltree + JS delegation), excluded from the default fast roster, so this
    /// body-walking test is gated out of the default suite. Run it explicitly with
    /// `cargo test -p sqry-lang-servicenow-xml -- --ignored`.
    #[test]
    #[ignore = "HighWallClock: servicenow-xml runs under the high-cost gate, not the default suite"]
    fn xml_mapping_is_honestly_empty_and_no_own_function_bodies() {
        use sqry_core::graph::unified::build::body_hash::has_valid_body_span;
        use sqry_core::graph::unified::build::staging::StagingGraph;
        use sqry_core::graph::unified::node::NodeKind;
        use std::path::PathBuf;

        // 1. The control-flow map over the HTML grammar registers no buckets: XML
        //    markup has no control flow.
        let mapping = servicenow_xml_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_html::LANGUAGE.into();
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
            "HTML markup has no control-flow kinds; the XML map must register no buckets"
        );

        // Cross-check the canonical bucket set is reachable (sanity on the enum import).
        assert_eq!(CfBucket::Branch.index(), 0);

        // 2. Build the graph for a script-bearing ServiceNow record. The embedded JS
        //    is delegated to the Xanadu builder, whose Function nodes carry spans in
        //    the SCRIPT coordinate system, not the XML plugin's HTML parse tree. The
        //    build seam therefore never resolves an XML-tree subtree for them, so the
        //    XML mapping attaches no descriptor of its own. Here we assert the XML
        //    plugin's own extraction produces no Function/Method-with-body node beyond
        //    whatever the delegated JS builder contributes; the honest contract is
        //    that the descriptor surface for embedded scripts belongs to the Xanadu
        //    JS mapping (covered in that crate), not to this declarative XML plugin.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_html::LANGUAGE.into())
            .expect("load html grammar");
        let tree = parser.parse(SAMPLE, None).expect("parse html dummy tree");

        let mut staging = StagingGraph::new();
        let builder = ServiceNowXmlGraphBuilder;
        let file = PathBuf::from("update_set.xml");
        builder
            .build_graph(&tree, SAMPLE.as_bytes(), &file, &mut staging)
            .unwrap();

        // Any Function/Method node present came from the delegated JS builder; verify
        // none of them resolves against the XML plugin's HTML tree (exact-span match),
        // which is what guarantees the XML mapping never mis-fingerprints a JS body.
        for node in staging.nodes() {
            if matches!(node.entry.kind, NodeKind::Function | NodeKind::Method)
                && has_valid_body_span(node.entry)
            {
                let start = tree_sitter::Point {
                    row: node.entry.start_line.saturating_sub(1) as usize,
                    column: node.entry.start_column as usize,
                };
                let end = tree_sitter::Point {
                    row: node.entry.end_line.saturating_sub(1) as usize,
                    column: node.entry.end_column as usize,
                };
                let resolved = tree
                    .root_node()
                    .descendant_for_point_range(start, end)
                    .filter(|n| n.start_position() == start && n.end_position() == end);
                assert!(
                    resolved.is_none(),
                    "a delegated JS function span must NOT resolve to an XML/HTML subtree, \
                     so the XML mapping never fingerprints it"
                );
            }
        }
    }
}

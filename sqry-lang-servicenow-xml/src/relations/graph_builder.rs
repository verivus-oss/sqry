//! Graph builder for ServiceNow XML record extraction.

use std::path::Path;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::{GraphBuilder, GraphResult};
use tree_sitter::Tree;

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

/// Graph builder for ServiceNow XML update set files.
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
        let record_elems: Vec<_> = root.children().filter(|n| n.is_element()).collect();
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

                for record_elem in &record_elems {
                    extract_table_schema(record_elem, module_id, &mut helper);
                }
            }
            RecordType::TableDefinition => {
                let mut helper = GraphBuildHelper::new(staging, file, Language::ServiceNow);
                let module_id = helper.add_module("<module>", None);

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
}

impl ServiceNowXmlGraphBuilder {
    /// Handle script-bearing records: single-record delegates directly,
    /// multi-record uses separate staging per delegation with replay.
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

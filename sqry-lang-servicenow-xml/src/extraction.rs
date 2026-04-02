//! Script and table schema extraction from ServiceNow XML records.

use std::path::Path;

use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::{GraphBuilder, GraphResult};
use sqry_core::plugin::LanguagePlugin;

use crate::metadata::{RecordMetadata, child_text, synthetic_path};
use crate::replay::ReplayState;

/// Maximum script size to extract.
///
/// Override: `SQRY_SN_XML_MAX_SCRIPT_SIZE` (bytes). Clamped to 64 KB – 10 MB.
const DEFAULT_MAX_SCRIPT_SIZE: usize = 1_048_576; // 1 MB
const MIN_MAX_SCRIPT_SIZE: usize = 64 * 1024; // 64 KB
const MAX_MAX_SCRIPT_SIZE: usize = 10 * 1024 * 1024; // 10 MB

/// Maximum length for schema table/element names.
///
/// Override: `SQRY_SN_XML_MAX_SCHEMA_NAME_LEN` (chars). Clamped to 64 – 1 024.
const DEFAULT_MAX_SCHEMA_NAME_LEN: usize = 256;
const MIN_MAX_SCHEMA_NAME_LEN: usize = 64;
const MAX_MAX_SCHEMA_NAME_LEN: usize = 1_024;

/// Get the max script size, respecting environment variable override.
pub fn max_script_size() -> usize {
    std::env::var("SQRY_SN_XML_MAX_SCRIPT_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SCRIPT_SIZE)
        .clamp(MIN_MAX_SCRIPT_SIZE, MAX_MAX_SCRIPT_SIZE)
}

fn max_schema_name_len() -> usize {
    std::env::var("SQRY_SN_XML_MAX_SCHEMA_NAME_LEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SCHEMA_NAME_LEN)
        .clamp(MIN_MAX_SCHEMA_NAME_LEN, MAX_MAX_SCHEMA_NAME_LEN)
}

/// Extract scripts from a record element and delegate to ServiceNow JS builder.
///
/// Each script field delegation creates a fresh `StagingGraph` and replays it
/// into the main staging with remapped `StringId`s/`NodeId`s. This prevents
/// duplicate local `StringId` collisions when multiple script fields (e.g.
/// `script` + `client_script`) exist in the same record — each inner
/// `GraphBuildHelper` starts its local counter at 0, so without separate
/// staging the second field would re-emit `StringId(local:0)`.
#[allow(clippy::too_many_arguments)]
pub fn extract_scripts(
    record: &roxmltree::Node<'_, '_>,
    script_fields: &[&str],
    metadata: &RecordMetadata,
    xml_path: &Path,
    record_idx: usize,
    multi_record: bool,
    sn_plugin: &sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin,
    sn_builder: &sqry_lang_servicenow_xanadu::ServiceNowGraphBuilder,
    staging: &mut StagingGraph,
) -> GraphResult<()> {
    let synth_path = synthetic_path(xml_path, metadata, record_idx, multi_record);

    let mut replay_state = ReplayState::new(staging);

    for field_name in script_fields {
        let Some(script_text) = child_text(record, field_name) else {
            continue;
        };
        let trimmed = script_text.trim();
        if trimmed.is_empty() || trimmed.len() > max_script_size() {
            continue;
        }
        let script_bytes = trimmed.as_bytes();

        // Parse extracted JS with JavaScript tree-sitter grammar
        let Ok(js_tree) = sn_plugin.parse_ast(script_bytes) else {
            continue; // Malformed JS → skip
        };

        // Validate the tree is actually a JavaScript AST
        if js_tree.root_node().kind() != "program" {
            continue;
        }

        // Delegate into a fresh staging to avoid StringId collisions.
        // Each build_graph() call creates a GraphBuildHelper that starts
        // next_string_id at 0, so sharing one staging across multiple
        // delegations produces duplicate InternString local IDs.
        let mut del_staging = StagingGraph::new();
        sn_builder.build_graph(&js_tree, script_bytes, &synth_path, &mut del_staging)?;

        // Attach body hashes immediately while we have the exact script bytes.
        // Must happen per-field so spans and hash input stay aligned.
        // The pipeline's later attach_body_hashes() against XML bytes will skip
        // nodes that already have hashes (body_hash.is_none() check).
        del_staging.attach_body_hashes(script_bytes);

        if !del_staging.is_empty() {
            replay_state.replay(staging, &mut del_staging)?;
        }
    }

    Ok(())
}

/// Extract table schema from a sys_dictionary record.
pub fn extract_table_schema(
    record: &roxmltree::Node<'_, '_>,
    module_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let Some(table_name) = child_text(record, "name") else {
        return;
    };
    let table_name = table_name.trim();
    if table_name.is_empty() || table_name.len() > max_schema_name_len() {
        return;
    }

    let element = child_text(record, "element")
        .unwrap_or("")
        .trim()
        .to_string();
    if element.len() > max_schema_name_len() {
        return;
    }

    // Table → Resource node
    let table_qname = format!("table.{table_name}");
    let table_id = helper.add_resource(&table_qname, None);
    helper.add_defines_edge(module_id, table_id);

    // Field → Variable node
    if !element.is_empty() {
        let field_qname = format!("table.{table_name}.{element}");
        let field_id = helper.add_variable(&field_qname, None);
        helper.add_contains_edge(table_id, field_id);
    }
}

/// Extract table definition from a sys_db_object record.
pub fn extract_table_definition(
    record: &roxmltree::Node<'_, '_>,
    module_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let Some(table_name) = child_text(record, "name") else {
        return;
    };
    let table_name = table_name.trim();
    if table_name.is_empty() || table_name.len() > max_schema_name_len() {
        return;
    }

    let table_qname = format!("table.{table_name}");
    let table_id = helper.add_resource(&table_qname, None);
    helper.add_defines_edge(module_id, table_id);
}

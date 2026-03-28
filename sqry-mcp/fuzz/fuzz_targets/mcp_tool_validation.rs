#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_mcp::tool_validation::*;

fuzz_target!(|data: &[u8]| {
    // Try to parse as JSON and then validate with each tool validator
    // We don't care if validation succeeds or fails, only that it doesn't panic

    if let Ok(json_str) = std::str::from_utf8(data) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
            // Fuzz all validation functions with arbitrary JSON
            let _ = validate_semantic_search_args(&value);
            let _ = validate_relation_query_args(&value);
            let _ = validate_explain_code_args(&value);
            let _ = validate_show_dependencies_args(&value);
            let _ = validate_export_graph_args(&value);
            let _ = validate_cross_language_edges_args(&value);
            let _ = validate_trace_path_args(&value);
            let _ = validate_subgraph_args(&value);
            let _ = validate_search_similar_args(&value);
            let _ = validate_get_index_status_args(&value);
            let _ = validate_dependency_impact_args(&value);
            let _ = validate_semantic_diff_args(&value);
        }
    }
});

//! Shared response-shape builder for MCP tool results.
//!
//! Both the rmcp-backed `SqryServer` (HTTP/stdio) and the daemon adapter
//! (in-memory JSON-RPC, driven by sqry-daemon) need to render the same
//! `ToolExecution<T>` envelope. The only wire-level difference is whether
//! the protocol `version` field is included — rmcp callers set it to the
//! MCP protocol version, the daemon path omits it (the daemon has its
//! own protocol version in `ResponseEnvelope::meta`).

use crate::execution::ToolExecution;
use rmcp::ErrorData as McpError;
use serde::Serialize;
use serde_json::json;

/// Build a JSON response object from a `ToolExecution<T>`, preserving all
/// execution metadata.
///
/// When `include_version` is true the object carries an MCP protocol-version
/// field (`"version": "2024-11-05"`); when false it is omitted.
///
/// This function is the sole builder of the tool-response envelope; both
/// `SqryServer::build_response` and the daemon adapter delegate here so the
/// wire shape cannot drift.
pub(crate) fn build_tool_response<T: Serialize>(
    execution: ToolExecution<T>,
    include_version: bool,
) -> Result<serde_json::Value, McpError> {
    let mut response = serde_json::Map::new();

    // Protocol version (rmcp path only; daemon transport carries its own
    // protocol version in `ResponseEnvelope::meta`).
    if include_version {
        response.insert("version".to_string(), json!("2024-11-05"));
    }

    // Serialize the main data
    let data = serde_json::to_value(&execution.data)
        .map_err(|e| McpError::internal_error(format!("Failed to serialize result: {e}"), None))?;
    response.insert("data".to_string(), data);

    // Execution metadata
    response.insert("execution_ms".to_string(), json!(execution.execution_ms));

    if execution.used_index {
        response.insert("used_index".to_string(), json!(true));
    }

    if execution.used_graph {
        response.insert("used_graph".to_string(), json!(true));
    }

    if let Some(metadata) = execution.graph_metadata {
        let metadata_value = serde_json::to_value(&metadata).map_err(|e| {
            McpError::internal_error(format!("Failed to serialize graph_metadata: {e}"), None)
        })?;
        response.insert("graph_metadata".to_string(), metadata_value);
    }

    // Pagination metadata
    if let Some(token) = execution.next_page_token {
        response.insert("next_page_token".to_string(), json!(token));
    }

    if let Some(total) = execution.total {
        response.insert("total".to_string(), json!(total));
    }

    if let Some(truncated) = execution.truncated {
        response.insert("truncated".to_string(), json!(truncated));
    }

    if let Some(scanned) = execution.candidates_scanned {
        response.insert("candidates_scanned".to_string(), json!(scanned));
    }

    // Workspace path
    if !execution.workspace_path.is_empty() {
        response.insert(
            "workspace_path".to_string(),
            json!(execution.workspace_path),
        );
    }

    Ok(serde_json::Value::Object(response))
}

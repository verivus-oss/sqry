//! T2 `relation_query` surface for the new `ChannelPeers` / `Instantiations`
//! relation types (`02_DESIGN.md` §7.7). End-to-end through the MCP handler:
//! index a Go fixture, issue the query, assert the returned edges.

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{PaginationArgs, RelationQueryArgs, RelationType};
use sqry_mcp::tool_handlers::execute_relation_query;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the path-resolver / engine / telemetry caches once per binary.
fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

fn write_go(source: &str) -> Result<TempDir> {
    let temp = TempDir::new()?;
    fs::write(temp.path().join("q.go"), source)?;
    Ok(temp)
}

fn index(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

fn query(temp: &TempDir, symbol: &str, relation: RelationType) -> Result<Vec<serde_json::Value>> {
    let args = RelationQueryArgs {
        symbol: symbol.to_string(),
        relation,
        path: workspace_arg(temp),
        max_depth: 4,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;
    Ok(result
        .data
        .relations
        .into_iter()
        .map(|edge| {
            serde_json::json!({
                "type": edge.relation_type,
                "from": edge.from.map(|f| f.name),
                "to": edge.to.map(|t| t.name),
                "metadata": edge.metadata,
            })
        })
        .collect())
}

#[test]
fn channel_peers_relation_returns_send_and_receive() -> Result<()> {
    let temp = write_go(
        r#"
package q

func main() {
    ch := make(chan int, 4)
    ch <- 1
    x := <-ch
    _ = x
}
"#,
    )?;
    index(temp.path())?;

    let edges = query(&temp, "main", RelationType::ChannelPeers)?;
    assert!(
        !edges.is_empty(),
        "expected ChannelPeer edges from main, got none"
    );

    let directions: Vec<String> = edges
        .iter()
        .filter_map(|e| e["metadata"]["direction"].as_str().map(str::to_string))
        .collect();
    assert!(
        directions.iter().any(|d| d == "send"),
        "expected a send ChannelPeer (got {directions:?})"
    );
    assert!(
        directions.iter().any(|d| d == "receive"),
        "expected a receive ChannelPeer (got {directions:?})"
    );
    // Every edge is typed channel_peers and targets the q::main::ch channel.
    assert!(edges.iter().all(|e| e["type"] == "channel_peers"));
    assert!(
        edges
            .iter()
            .all(|e| e["to"].as_str().is_some_and(|t| t.contains("ch"))),
        "edges should target the channel node (got {edges:?})"
    );
    Ok(())
}

#[test]
fn instantiations_relation_returns_resolved_type_args() -> Result<()> {
    let temp = write_go(
        r#"
package q

func Map[K comparable, V any](m map[K]V) []K { return nil }

func main() {
    _ = Map[string, int](nil)
}
"#,
    )?;
    index(temp.path())?;

    let edges = query(&temp, "Map", RelationType::Instantiations)?;
    assert!(
        !edges.is_empty(),
        "expected an Instantiates edge into Map, got none"
    );

    let edge = &edges[0];
    assert_eq!(edge["type"], "instantiations");
    assert_eq!(edge["metadata"]["inference_kind"], "explicit");
    let names: Vec<String> = edge["metadata"]["type_args"]
        .as_array()
        .expect("type_args array")
        .iter()
        .filter_map(|a| a["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        names,
        vec!["string".to_string(), "int".to_string()],
        "type_args must resolve to the explicit [string, int]"
    );
    Ok(())
}

#[test]
fn legacy_callers_relation_excludes_new_edge_kinds() -> Result<()> {
    // Guard against §7.7 inadvertently widening the live Callers/Callees
    // filters: a Callers query must never surface channel_peers/instantiations.
    let temp = write_go(
        r#"
package q

func main() {
    ch := make(chan int)
    ch <- 1
}
"#,
    )?;
    index(temp.path())?;

    let edges = query(&temp, "main", RelationType::Callers)?;
    assert!(
        edges
            .iter()
            .all(|e| e["type"] != "channel_peers" && e["type"] != "instantiations"),
        "Callers must not return channel_peers / instantiations edges (got {edges:?})"
    );
    Ok(())
}

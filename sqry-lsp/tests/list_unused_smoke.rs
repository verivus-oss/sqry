mod common;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_plugin_registry::create_plugin_manager;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::jsonrpc::Request;

#[derive(Debug, Deserialize)]
struct ListSymbolsResultWire {
    symbols: Vec<SearchItemWire>,
    total: usize,
}

#[derive(Debug, Deserialize)]
struct UnusedResultWire {
    symbols: Vec<SearchItemWire>,
}

#[derive(Debug, Deserialize)]
struct SearchItemWire {
    name: String,
    kind: String,
    qualified_name: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn rust_fixture_workspace() -> Result<TempDir> {
    let tmp = tempfile::tempdir()?;
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir)?;
    fs::copy(
        repo_root().join("test-fixtures/cross-language/rust/fields.rs"),
        src_dir.join("lib.rs"),
    )
    .context("copy Rust field fixture")?;
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lsp-unused-field-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    Ok(tmp)
}

fn node_label(snapshot: &GraphSnapshot, entry: &NodeEntry) -> String {
    let name = snapshot
        .strings()
        .resolve(entry.name)
        .expect("node name must resolve");
    entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .unwrap_or(name)
        .to_string()
}

fn ledger_mutable_field(snapshot: &GraphSnapshot) -> Result<NodeId> {
    let matches = snapshot
        .iter_nodes()
        .filter(|(_, entry)| entry.kind == NodeKind::Property)
        .filter(|(_, entry)| node_label(snapshot, entry) == "Ledger::mutable_field")
        .map(|(node_id, _)| node_id)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [node_id] => Ok(*node_id),
        [] => anyhow::bail!("missing Ledger::mutable_field Property node"),
        _ => anyhow::bail!(
            "ambiguous Ledger::mutable_field Property nodes: {}",
            matches.len()
        ),
    }
}

fn assert_rust_fixture_has_typeof_only_field_root(workspace: &Path) -> Result<()> {
    let plugins = create_plugin_manager();
    let graph = build_unified_graph(workspace, &plugins, &BuildConfig::default())
        .context("build Rust fixture graph")?;
    let snapshot = graph.snapshot();
    let field_id = ledger_mutable_field(&snapshot)?;

    let has_field_typeof = snapshot
        .edges()
        .edges_from(field_id)
        .into_iter()
        .any(|edge| {
            if let EdgeKind::TypeOf { context, name, .. } = edge.kind {
                let edge_name = name.and_then(|id| snapshot.strings().resolve(id));
                context == Some(TypeOfContext::Field)
                    && edge_name.as_deref() == Some("mutable_field")
            } else {
                false
            }
        });
    assert!(
        has_field_typeof,
        "Ledger::mutable_field must carry a TypeOf{{Field}} edge named mutable_field"
    );

    let incoming_reference_count = snapshot
        .edges()
        .edges_to(field_id)
        .into_iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::References))
        .count();
    assert_eq!(
        incoming_reference_count, 0,
        "fixture must keep Ledger::mutable_field live without incoming Reference edges"
    );

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn test_req_r0015_lsp_unused_symbols_no_false_positive_on_typeof_field() -> Result<()> {
    let workspace = rust_fixture_workspace()?;
    assert_rust_fixture_has_typeof_only_field_root(workspace.path())?;

    let mut server = common::TestServer::new(workspace.path());

    let initialize = Request::build("initialize".to_string())
        .params(json!({
            "processId": null,
            "rootUri": format!("file://{}", workspace.path().display()),
            "capabilities": {}
        }))
        .id(0i64)
        .finish();
    let _ = server
        .send_request(initialize)
        .await?
        .expect("initialize response");

    let list_symbols_request = Request::build("sqry/listSymbols".to_string())
        .params(json!({ "kind": "property", "limit": 1000 }))
        .id(1i64)
        .finish();
    let list_symbols_response = server
        .send_request(list_symbols_request)
        .await?
        .expect("listSymbols response");
    let (_, list_symbols_body) = list_symbols_response.into_parts();
    let list_symbols_value = list_symbols_body.expect("listSymbols result");
    let symbols: ListSymbolsResultWire = serde_json::from_value(list_symbols_value)?;
    assert!(
        symbols.total > 0,
        "listSymbols must prove the workspace graph is indexed"
    );
    assert!(
        symbols.symbols.iter().any(|symbol| {
            symbol.name == "mutable_field"
                && symbol.kind == "property"
                && symbol.qualified_name == "Ledger::mutable_field"
        }),
        "listSymbols must expose Ledger::mutable_field before the unused assertion: {:?}",
        symbols.symbols
    );

    let request = Request::build("sqry/listUnusedSymbols".to_string())
        .params(json!({ "scope": "public", "limit": 1000 }))
        .id(2i64)
        .finish();
    let response = server
        .send_request(request)
        .await?
        .expect("listUnusedSymbols response");
    let (_, body) = response.into_parts();
    let value = body.expect("listUnusedSymbols result");
    let result: UnusedResultWire = serde_json::from_value(value)?;

    assert!(
        result
            .symbols
            .iter()
            .all(|symbol| !symbol.qualified_name.ends_with("Ledger::mutable_field")),
        "public Rust field referenced through TypeOf{{Field}} must not be reported unused: {:?}",
        result.symbols
    );

    Ok(())
}

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia, TypeOfContext};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::persistence::{GraphStorage, save_to_path};
use sqry_core::graph::unified::storage::arena::NodeEntry;
use tempfile::TempDir;
use tower_lsp::jsonrpc::{Error as RpcError, ErrorCode, Request};
use tower_lsp::lsp_types::{InitializeParams, Location};

mod common;

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Deserialize)]
struct CircularResultWire {
    cycles: Vec<CycleWire>,
    total_cycles: usize,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct CycleWire {
    cycle_id: String,
    depth: usize,
    members: Vec<String>,
    cycle_type: String,
    member_locations: Option<Vec<CycleMemberLocationWire>>,
}

#[derive(Debug, Deserialize)]
struct CycleMemberLocationWire {
    name: String,
    file: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct UnusedResultWire {
    symbols: Vec<SearchItemWire>,
    total: usize,
    truncated: bool,
    scope: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SearchItemWire {
    name: String,
    kind: String,
    qualified_name: String,
    language: String,
    location: Location,
}

fn fixture_source_root() -> PathBuf {
    common::fixture_path("sqry-lsp/tests/fixtures/graph-analysis-workspace")
}

fn copy_fixture_to_temp() -> Result<TempDir> {
    fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if entry.file_name() == ".sqry" {
                continue;
            }
            if path.is_dir() {
                copy_dir_all(&path, &target)?;
            } else {
                fs::copy(&path, &target)
                    .with_context(|| format!("copy {} -> {}", path.display(), target.display()))?;
            }
        }
        Ok(())
    }

    let temp_dir = tempfile::tempdir()?;
    copy_dir_all(&fixture_source_root(), temp_dir.path())?;
    Ok(temp_dir)
}

fn line_and_column(path: &Path, needle: &str) -> Result<(u32, u32)> {
    let content = fs::read_to_string(path)?;
    for (index, line) in content.lines().enumerate() {
        if let Some(column) = line.find(needle) {
            return Ok(((index + 1) as u32, column as u32));
        }
    }
    anyhow::bail!("could not find `{needle}` in {}", path.display());
}

fn build_fixture_graph(root: &Path) -> Result<()> {
    fn intern(graph: &mut CodeGraph, text: &str) -> Result<sqry_core::graph::unified::StringId> {
        graph
            .strings_mut()
            .intern(text)
            .with_context(|| format!("intern `{text}`"))
    }

    fn register_file(
        graph: &mut CodeGraph,
        path: &Path,
    ) -> Result<sqry_core::graph::unified::FileId> {
        graph
            .files_mut()
            .register_with_language(path, Some(sqry_core::graph::node::Language::Rust))
            .with_context(|| format!("register {}", path.display()))
    }

    fn add_node(
        graph: &mut CodeGraph,
        file: sqry_core::graph::unified::FileId,
        name: &str,
        qualified_name: &str,
        kind: NodeKind,
        location: (u32, u32),
        visibility: Option<&str>,
    ) -> Result<sqry_core::graph::unified::NodeId> {
        let name_id = intern(graph, name)?;
        let qualified_name_id = intern(graph, qualified_name)?;
        let mut entry = NodeEntry::new(kind, name_id, file)
            .with_qualified_name(qualified_name_id)
            .with_location(
                location.0,
                location.1,
                location.0,
                location.1.saturating_add(1),
            );
        if let Some(vis) = visibility {
            entry = entry.with_visibility(intern(graph, vis)?);
        }
        graph.nodes_mut().alloc(entry).context("alloc node")
    }

    fn add_edge(
        graph: &mut CodeGraph,
        source: sqry_core::graph::unified::NodeId,
        target: sqry_core::graph::unified::NodeId,
        kind: EdgeKind,
        file: sqry_core::graph::unified::FileId,
    ) {
        graph.edges_mut().add_edge(source, target, kind, file);
    }

    let mut graph = CodeGraph::new();

    let main_path = root.join("src/main.rs");
    let cycle_ab_path = root.join("src/cycle_ab.rs");
    let cycle_ba_path = root.join("src/cycle_ba.rs");
    let mod_cycle_a_path = root.join("src/mod_cycle_a.rs");
    let mod_cycle_b_path = root.join("src/mod_cycle_b.rs");
    let reachability_path = root.join("src/reachability.rs");
    let self_loop_path = root.join("src/self_loop.rs");
    let unused_bulk_path = root.join("src/unused_bulk.rs");
    let utf16_ident_path = root.join("src/utf16_ident.rs");

    let main_file = register_file(&mut graph, &main_path)?;
    let cycle_ab_file = register_file(&mut graph, &cycle_ab_path)?;
    let cycle_ba_file = register_file(&mut graph, &cycle_ba_path)?;
    let mod_cycle_a_file = register_file(&mut graph, &mod_cycle_a_path)?;
    let mod_cycle_b_file = register_file(&mut graph, &mod_cycle_b_path)?;
    let reachability_file = register_file(&mut graph, &reachability_path)?;
    let self_loop_file = register_file(&mut graph, &self_loop_path)?;
    let unused_bulk_file = register_file(&mut graph, &unused_bulk_path)?;
    let utf16_ident_file = register_file(&mut graph, &utf16_ident_path)?;

    let main = add_node(
        &mut graph,
        main_file,
        "main",
        "main",
        NodeKind::Function,
        line_and_column(&main_path, "fn main")?,
        None,
    )?;
    let drive_imports = add_node(
        &mut graph,
        reachability_file,
        "drive_imports",
        "reachability::drive_imports",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn drive_imports")?,
        None,
    )?;
    let drive_references = add_node(
        &mut graph,
        reachability_file,
        "drive_references",
        "reachability::drive_references",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn drive_references")?,
        None,
    )?;
    let drive_type_of = add_node(
        &mut graph,
        reachability_file,
        "drive_type_of",
        "reachability::drive_type_of",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn drive_type_of")?,
        None,
    )?;
    let imported_only_symbol = add_node(
        &mut graph,
        reachability_file,
        "imported_only_symbol",
        "reachability::imported_only_symbol",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn imported_only_symbol")?,
        Some("private"),
    )?;
    let referenced_only_const = add_node(
        &mut graph,
        reachability_file,
        "REFERENCED_ONLY_CONST",
        "reachability::REFERENCED_ONLY_CONST",
        NodeKind::Constant,
        line_and_column(&reachability_path, "REFERENCED_ONLY_CONST")?,
        Some("private"),
    )?;
    let used_via_type_of = add_node(
        &mut graph,
        reachability_file,
        "UsedViaTypeOf",
        "reachability::UsedViaTypeOf",
        NodeKind::Struct,
        line_and_column(&reachability_path, "UsedViaTypeOf;")?,
        Some("private"),
    )?;

    let cycle_ab_start = add_node(
        &mut graph,
        cycle_ab_file,
        "cycle_ab_start",
        "cycle_ab_start",
        NodeKind::Function,
        line_and_column(&cycle_ab_path, "fn cycle_ab_start")?,
        None,
    )?;
    let cycle_ba_partner = add_node(
        &mut graph,
        cycle_ba_file,
        "cycle_ba_partner",
        "cycle_ba_partner",
        NodeKind::Function,
        line_and_column(&cycle_ba_path, "fn cycle_ba_partner")?,
        None,
    )?;
    let mod_cycle_a_entry = add_node(
        &mut graph,
        mod_cycle_a_file,
        "mod_cycle_a_entry",
        "mod_cycle_a_entry",
        NodeKind::Function,
        line_and_column(&mod_cycle_a_path, "fn mod_cycle_a_entry")?,
        None,
    )?;
    let mod_cycle_b_entry = add_node(
        &mut graph,
        mod_cycle_b_file,
        "mod_cycle_b_entry",
        "mod_cycle_b_entry",
        NodeKind::Function,
        line_and_column(&mod_cycle_b_path, "fn mod_cycle_b_entry")?,
        None,
    )?;
    let reach_cycle_left = add_node(
        &mut graph,
        reachability_file,
        "reach_cycle_left",
        "reach_cycle_left",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn reach_cycle_left")?,
        None,
    )?;
    let reach_cycle_right = add_node(
        &mut graph,
        reachability_file,
        "reach_cycle_right",
        "reach_cycle_right",
        NodeKind::Function,
        line_and_column(&reachability_path, "fn reach_cycle_right")?,
        None,
    )?;
    let bulk_cycle_alpha = add_node(
        &mut graph,
        unused_bulk_file,
        "bulk_cycle_alpha",
        "bulk_cycle_alpha",
        NodeKind::Function,
        line_and_column(&unused_bulk_path, "fn bulk_cycle_alpha")?,
        Some("private"),
    )?;
    let bulk_cycle_beta = add_node(
        &mut graph,
        unused_bulk_file,
        "bulk_cycle_beta",
        "bulk_cycle_beta",
        NodeKind::Function,
        line_and_column(&unused_bulk_path, "fn bulk_cycle_beta")?,
        Some("private"),
    )?;
    let recurse_self_loop = add_node(
        &mut graph,
        self_loop_file,
        "recurse_self_loop",
        "recurse_self_loop",
        NodeKind::Function,
        line_and_column(&self_loop_path, "fn recurse_self_loop")?,
        None,
    )?;
    let utf16_cycle_start = add_node(
        &mut graph,
        utf16_ident_file,
        "utf16_cycle_start",
        "utf16_ident::utf16_cycle_start",
        NodeKind::Function,
        line_and_column(&utf16_ident_path, "fn utf16_cycle_start")?,
        Some("private"),
    )?;
    let utf16_cycle_end = add_node(
        &mut graph,
        utf16_ident_file,
        "utf16_cycle_end",
        "utf16_ident::utf16_cycle_end",
        NodeKind::Function,
        line_and_column(&utf16_ident_path, "fn utf16_cycle_end")?,
        Some("private"),
    )?;
    let _utf16_unused_marker = add_node(
        &mut graph,
        utf16_ident_file,
        "utf16_unused_marker",
        "utf16_ident::utf16_unused_marker",
        NodeKind::Function,
        line_and_column(&utf16_ident_path, "fn utf16_unused_marker")?,
        Some("private"),
    )?;
    let orphan_struct = add_node(
        &mut graph,
        unused_bulk_file,
        "OrphanStruct",
        "unused_bulk::OrphanStruct",
        NodeKind::Struct,
        line_and_column(&unused_bulk_path, "OrphanStruct")?,
        Some("private"),
    )?;
    let mod_cycle_a_module = add_node(
        &mut graph,
        main_file,
        "mod_cycle_a",
        "mod_cycle_a",
        NodeKind::Module,
        line_and_column(&main_path, "mod mod_cycle_a;")?,
        None,
    )?;
    let mod_cycle_b_module = add_node(
        &mut graph,
        main_file,
        "mod_cycle_b",
        "mod_cycle_b",
        NodeKind::Module,
        line_and_column(&main_path, "mod mod_cycle_b;")?,
        None,
    )?;

    add_edge(
        &mut graph,
        main,
        drive_imports,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        main_file,
    );
    add_edge(
        &mut graph,
        main,
        drive_references,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        main_file,
    );
    add_edge(
        &mut graph,
        main,
        drive_type_of,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        main_file,
    );
    add_edge(
        &mut graph,
        drive_imports,
        imported_only_symbol,
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        reachability_file,
    );
    add_edge(
        &mut graph,
        drive_references,
        referenced_only_const,
        EdgeKind::References,
        reachability_file,
    );
    add_edge(
        &mut graph,
        drive_type_of,
        used_via_type_of,
        EdgeKind::TypeOf {
            context: Some(TypeOfContext::Return),
            index: None,
            name: None,
        },
        reachability_file,
    );

    for (source, target, file) in [
        (cycle_ab_start, cycle_ba_partner, cycle_ab_file),
        (cycle_ba_partner, cycle_ab_start, cycle_ba_file),
        (mod_cycle_a_entry, mod_cycle_b_entry, mod_cycle_a_file),
        (mod_cycle_b_entry, mod_cycle_a_entry, mod_cycle_b_file),
        (reach_cycle_left, reach_cycle_right, reachability_file),
        (reach_cycle_right, reach_cycle_left, reachability_file),
        (bulk_cycle_alpha, bulk_cycle_beta, unused_bulk_file),
        (bulk_cycle_beta, bulk_cycle_alpha, unused_bulk_file),
        (utf16_cycle_start, utf16_cycle_end, utf16_ident_file),
        (utf16_cycle_end, utf16_cycle_start, utf16_ident_file),
    ] {
        add_edge(
            &mut graph,
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
    }
    add_edge(
        &mut graph,
        recurse_self_loop,
        recurse_self_loop,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        self_loop_file,
    );
    add_edge(
        &mut graph,
        mod_cycle_a_module,
        mod_cycle_b_module,
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        main_file,
    );
    add_edge(
        &mut graph,
        mod_cycle_b_module,
        mod_cycle_a_module,
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        main_file,
    );

    for index in 1..=101 {
        let name = format!("_unused_{index:03}");
        let qualified_name = format!("unused_bulk::{name}");
        add_node(
            &mut graph,
            unused_bulk_file,
            &name,
            &qualified_name,
            NodeKind::Function,
            line_and_column(&unused_bulk_path, &name)?,
            Some("private"),
        )?;
    }

    // Keep the struct node live so it is not optimized away by the builder.
    let _ = orphan_struct;

    let storage = GraphStorage::new(root);
    fs::create_dir_all(storage.graph_dir())?;
    save_to_path(&graph, storage.snapshot_path())?;

    // SGA06 — the LSP graph acquisition path now routes through
    // `FilesystemGraphProvider`, which verifies the snapshot SHA-256
    // against the manifest when one is present. The synthetic fixture
    // overwrites the snapshot bytes; we must rewrite the manifest's
    // `snapshot_sha256` to match so the provider's integrity check
    // succeeds. This preserves the production contract (manifest SHA
    // is checked) while keeping the synthetic fixture loadable.
    let snapshot_bytes = fs::read(storage.snapshot_path())?;
    let new_sha_hex = hex_lower(&Sha256::digest(&snapshot_bytes));
    let manifest_path = storage.manifest_path();
    if manifest_path.exists() {
        let mut manifest = sqry_core::graph::unified::persistence::Manifest::load(manifest_path)
            .with_context(|| {
                format!(
                    "load manifest at {} for SHA refresh",
                    manifest_path.display()
                )
            })?;
        manifest.snapshot_sha256 = new_sha_hex;
        manifest
            .save(manifest_path)
            .with_context(|| format!("save refreshed manifest at {}", manifest_path.display()))?;
    }
    Ok(())
}

async fn initialize_server(server: &mut common::TestServer) -> Result<()> {
    let initialize = Request::build("initialize")
        .params(serde_json::to_value(InitializeParams::default())?)
        .id(0i64)
        .finish();
    let _ = server
        .send_request(initialize)
        .await?
        .expect("initialize response");
    let initialized = Request::build("initialized").finish();
    let _ = server.send_request(initialized).await?;
    Ok(())
}

fn manual_test_server(root: &Path) -> common::TestServer {
    let session = sqry_lsp::session::SessionManager::new(common::options_for(root));
    let service = sqry_lsp::build_test_service(&session);
    common::TestServer { session, service }
}

async fn prepare_fixture_server() -> Result<(TempDir, common::TestServer)> {
    let temp_dir = copy_fixture_to_temp()?;
    common::ensure_index(temp_dir.path())?;
    build_fixture_graph(temp_dir.path())?;
    let mut server = manual_test_server(temp_dir.path());
    initialize_server(&mut server).await?;
    Ok((temp_dir, server))
}

async fn request_ok<T: for<'de> Deserialize<'de>, P: Serialize>(
    server: &mut common::TestServer,
    method: &str,
    params: P,
) -> Result<T> {
    let request = Request::build(method.to_string())
        .params(serde_json::to_value(params)?)
        .id(1i64)
        .finish();
    let response = server.send_request(request).await?.expect("response");
    let (_, body) = response.into_parts();
    Ok(serde_json::from_value(body?)?)
}

async fn request_error(
    server: &mut common::TestServer,
    method: &str,
    params: Value,
) -> Result<RpcError> {
    let request = Request::build(method.to_string())
        .params(params)
        .id(1i64)
        .finish();
    let response = server.send_request(request).await?.expect("response");
    let (_, body) = response.into_parts();
    Ok(body.expect_err("expected rpc error"))
}

fn simple_name(name: &str) -> String {
    name.rsplit("::").next().unwrap_or(name).to_string()
}

fn cycle_name_sets(result: &CircularResultWire) -> HashSet<Vec<String>> {
    result
        .cycles
        .iter()
        .map(|cycle| {
            let mut members = cycle
                .members
                .iter()
                .map(|member| simple_name(member))
                .collect::<Vec<_>>();
            members.sort();
            members
        })
        .collect()
}

fn expected_call_cycle_sets() -> HashSet<Vec<String>> {
    [
        vec![
            "bulk_cycle_alpha".to_string(),
            "bulk_cycle_beta".to_string(),
        ],
        vec!["cycle_ab_start".to_string(), "cycle_ba_partner".to_string()],
        vec![
            "mod_cycle_a_entry".to_string(),
            "mod_cycle_b_entry".to_string(),
        ],
        vec![
            "reach_cycle_left".to_string(),
            "reach_cycle_right".to_string(),
        ],
        vec![
            "utf16_cycle_end".to_string(),
            "utf16_cycle_start".to_string(),
        ],
    ]
    .into_iter()
    .collect()
}

fn expected_module_cycle_set() -> HashSet<Vec<String>> {
    [vec!["mod_cycle_a".to_string(), "mod_cycle_b".to_string()]]
        .into_iter()
        .collect()
}

fn unused_simple_names(result: &UnusedResultWire) -> HashSet<String> {
    result
        .symbols
        .iter()
        .map(|item| simple_name(&item.qualified_name))
        .collect()
}

fn find_cycle<'a>(result: &'a CircularResultWire, symbol: &str) -> &'a CycleWire {
    result
        .cycles
        .iter()
        .find(|cycle| {
            cycle
                .members
                .iter()
                .any(|member| simple_name(member) == symbol)
        })
        .unwrap_or_else(|| panic!("missing cycle containing {symbol}"))
}

fn find_cycle_member<'a>(cycle: &'a CycleWire, symbol: &str) -> &'a CycleMemberLocationWire {
    cycle
        .member_locations
        .as_ref()
        .expect("member locations")
        .iter()
        .find(|member| simple_name(&member.name) == symbol)
        .unwrap_or_else(|| panic!("missing cycle member location for {symbol}"))
}

fn find_unused_symbol<'a>(result: &'a UnusedResultWire, symbol: &str) -> &'a SearchItemWire {
    result
        .symbols
        .iter()
        .find(|item| simple_name(&item.qualified_name) == symbol)
        .unwrap_or_else(|| panic!("missing unused symbol {symbol}"))
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_calls_default() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    assert_eq!(result.total_cycles, 5);
    assert!(!result.truncated);
    assert_eq!(result.cycles.len(), 5);
    assert_eq!(cycle_name_sets(&result), expected_call_cycle_sets());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_imports() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "circular_type": "imports" }),
    )
    .await?;
    assert_eq!(result.total_cycles, 1);
    assert!(!result.truncated);
    assert_eq!(cycle_name_sets(&result), expected_module_cycle_set());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_modules() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "circular_type": "modules" }),
    )
    .await?;
    assert_eq!(result.total_cycles, 1);
    assert!(!result.truncated);
    assert_eq!(cycle_name_sets(&result), expected_module_cycle_set());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_self_loops_included() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "should_include_self_loops": true }),
    )
    .await?;
    assert_eq!(result.total_cycles, 6);
    assert!(!result.truncated);
    let cycle_sets = cycle_name_sets(&result);
    assert!(cycle_sets.contains(&vec!["recurse_self_loop".to_string()]));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_self_loops_excluded() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "should_include_self_loops": false }),
    )
    .await?;
    let cycle_sets = cycle_name_sets(&result);
    assert!(!cycle_sets.contains(&vec!["recurse_self_loop".to_string()]));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_limit_triggers_truncation() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "limit": 2 }),
    )
    .await?;
    assert_eq!(result.cycles.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.total_cycles, 3);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_no_limit_applies_default() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    assert_eq!(result.cycles.len(), 5);
    assert_eq!(result.total_cycles, 5);
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_over_max_limit_clamped() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert_eq!(result.cycles.len(), 5);
    assert_eq!(result.total_cycles, 5);
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_invalid_type_returns_invalid_params() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let error = request_error(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "circular_type": "bogus" }),
    )
    .await?;
    assert_eq!(error.code, ErrorCode::InvalidParams);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_limit_wrong_type() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let error = request_error(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "limit": "not-a-number" }),
    )
    .await?;
    assert_eq!(error.code, ErrorCode::InvalidParams);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_empty_graph() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    fs::create_dir_all(temp_dir.path().join("src"))?;
    fs::write(temp_dir.path().join("src/main.rs"), "fn main() {}\n")?;
    let mut server = manual_test_server(temp_dir.path());
    initialize_server(&mut server).await?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    assert!(result.cycles.is_empty());
    assert_eq!(result.total_cycles, 0);
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_member_locations_shape() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    let cycle = find_cycle(&result, "utf16_cycle_start");
    assert_eq!(cycle.depth, 2);
    assert_eq!(cycle.cycle_type, "calls");
    let member = find_cycle_member(cycle, "utf16_cycle_start");
    assert!(
        member
            .file
            .as_ref()
            .is_some_and(|file| file.starts_with("file://"))
    );
    assert_eq!(member.line, Some(0));
    assert!(member.column.is_some());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_default_all() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire =
        request_ok(&mut server, "sqry/listUnusedSymbols", json!({})).await?;
    assert_eq!(result.scope, "all");
    assert_eq!(result.total, 101);
    assert!(result.truncated);
    assert_eq!(result.symbols.len(), 100);
    assert!(
        result
            .symbols
            .iter()
            .all(|symbol| symbol.location.uri.scheme() == "file")
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_scope_public() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": "public" }),
    )
    .await?;
    assert_eq!(result.total, 0);
    assert!(result.symbols.is_empty());
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_scope_private() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": "private", "limit": 10_000 }),
    )
    .await?;
    let names = unused_simple_names(&result);
    assert!(names.contains("OrphanStruct"));
    assert!(names.contains("_unused_001"));
    assert!(result.total >= 110);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_scope_function() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": "function", "limit": 10_000 }),
    )
    .await?;
    let names = unused_simple_names(&result);
    assert!(names.contains("_unused_001"));
    assert!(!names.contains("OrphanStruct"));
    assert!(result.total >= 110);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_scope_struct() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": "struct", "limit": 10_000 }),
    )
    .await?;
    let names = unused_simple_names(&result);
    assert_eq!(names, HashSet::from(["OrphanStruct".to_string()]));
    assert_eq!(result.total, 1);
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_limit_triggers_truncation() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire =
        request_ok(&mut server, "sqry/listUnusedSymbols", json!({ "limit": 2 })).await?;
    assert_eq!(result.symbols.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.total, 3);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_no_limit_applies_default() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire =
        request_ok(&mut server, "sqry/listUnusedSymbols", json!({})).await?;
    assert_eq!(result.symbols.len(), 100);
    assert_eq!(result.total, 101);
    assert!(result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_over_max_limit_clamped() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert_eq!(result.total, result.symbols.len());
    assert!(!result.truncated);
    assert!(result.total > 100);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_invalid_scope_returns_invalid_params() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let error = request_error(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": "bogus" }),
    )
    .await?;
    assert_eq!(error.code, ErrorCode::InvalidParams);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_scope_wrong_type() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let error = request_error(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "scope": 42 }),
    )
    .await?;
    assert_eq!(error.code, ErrorCode::InvalidParams);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_limit_wrong_type() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let error = request_error(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": "huge" }),
    )
    .await?;
    assert_eq!(error.code, ErrorCode::InvalidParams);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_empty_graph() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    fs::create_dir_all(temp_dir.path().join("src"))?;
    fs::write(temp_dir.path().join("src/main.rs"), "fn main() {}\n")?;
    let mut server = manual_test_server(temp_dir.path());
    initialize_server(&mut server).await?;
    let result: UnusedResultWire =
        request_ok(&mut server, "sqry/listUnusedSymbols", json!({})).await?;
    assert!(result.symbols.is_empty());
    assert_eq!(result.total, 0);
    assert!(!result.truncated);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_entry_point_excluded() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert!(!unused_simple_names(&result).contains("main"));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_non_call_reachability_excludes_imported_symbol() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert!(!unused_simple_names(&result).contains("imported_only_symbol"));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_non_call_reachability_excludes_references() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert!(!unused_simple_names(&result).contains("REFERENCED_ONLY_CONST"));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_non_call_reachability_excludes_type_of() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    assert!(!unused_simple_names(&result).contains("UsedViaTypeOf"));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_unused_location_utf16_columns() -> Result<()> {
    let (temp_dir, mut server) = prepare_fixture_server().await?;
    let result: UnusedResultWire = request_ok(
        &mut server,
        "sqry/listUnusedSymbols",
        json!({ "limit": 10_000 }),
    )
    .await?;
    let item = find_unused_symbol(&result, "utf16_unused_marker");
    let line_text = fs::read_to_string(temp_dir.path().join("src/utf16_ident.rs"))?
        .lines()
        .nth(1)
        .expect("utf16 line 2")
        .to_string();
    let raw_column = line_text.find("fn utf16_unused_marker").expect("needle") as u32;
    let utf16_column =
        sqry_lsp::utils::position::line_byte_to_utf16_col(&line_text, raw_column as usize) as u32;
    assert_ne!(raw_column, utf16_column);
    assert_eq!(item.location.range.start.line, 1);
    assert_eq!(item.location.range.start.character, utf16_column);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_total_cycles_sentinel() -> Result<()> {
    let (_temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire = request_ok(
        &mut server,
        "sqry/listCircularDependencies",
        json!({ "limit": 2 }),
    )
    .await?;
    assert_eq!(result.total_cycles, 3);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_column_is_utf16() -> Result<()> {
    let (temp_dir, mut server) = prepare_fixture_server().await?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    let cycle = find_cycle(&result, "utf16_cycle_start");
    let member = find_cycle_member(cycle, "utf16_cycle_start");
    let line_text = fs::read_to_string(temp_dir.path().join("src/utf16_ident.rs"))?
        .lines()
        .next()
        .expect("utf16 line 1")
        .to_string();
    let raw_column = line_text.find("fn utf16_cycle_start").expect("needle") as u32;
    let utf16_column =
        sqry_lsp::utils::position::line_byte_to_utf16_col(&line_text, raw_column as usize) as u32;
    assert_ne!(raw_column, utf16_column);
    assert_eq!(member.column, Some(utf16_column));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_column_none_when_file_unreadable() -> Result<()> {
    let (temp_dir, mut server) = prepare_fixture_server().await?;
    fs::remove_file(temp_dir.path().join("src/utf16_ident.rs"))?;
    let result: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    let cycle = find_cycle(&result, "utf16_cycle_start");
    let member = find_cycle_member(cycle, "utf16_cycle_start");
    assert!(member.file.is_some());
    assert_eq!(member.line, Some(0));
    assert_eq!(member.column, None);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_list_circular_deps_cycle_id_stable() -> Result<()> {
    let (temp_dir, mut server) = prepare_fixture_server().await?;
    let first: CircularResultWire =
        request_ok(&mut server, "sqry/listCircularDependencies", json!({})).await?;
    build_fixture_graph(temp_dir.path())?;
    let mut rebuilt_server = manual_test_server(temp_dir.path());
    initialize_server(&mut rebuilt_server).await?;
    let second: CircularResultWire = request_ok(
        &mut rebuilt_server,
        "sqry/listCircularDependencies",
        json!({}),
    )
    .await?;
    let mut first_ids = first
        .cycles
        .iter()
        .map(|cycle| cycle.cycle_id.clone())
        .collect::<Vec<_>>();
    let mut second_ids = second
        .cycles
        .iter()
        .map(|cycle| cycle.cycle_id.clone())
        .collect::<Vec<_>>();
    first_ids.sort();
    second_ids.sort();
    assert_eq!(first_ids, second_ids);
    Ok(())
}

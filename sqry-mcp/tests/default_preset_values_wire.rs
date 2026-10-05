//! What the standalone server's default redaction preset keeps, as a client
//! receives it (round 7, surfaces round three, findings 1 and 2).
//!
//! Every test spawns `sqry-mcp --no-daemon` with no redaction variable set,
//! so the preset is the one an operator who configured nothing gets.
//!
//! Finding 1: redaction used to run every value under `source` and `target`
//! through the path pipeline. Every `query_too_broad` refusal's documented
//! `details.source` (`static_estimate`, `runtime_budget`) reached the client
//! as `<external>/static_estimate`, and the symbol a `direct_callees` or
//! `direct_callers` answer is about as `<source_root_id>/alpha`. Now only
//! paths change; the absolute paths in the same responses are still gone.
//!
//! Finding 2: the recorded-cache refusal's remedy, `details.reset_arguments`,
//! carried the canonical root, which the preset redacted to a placeholder
//! naming no workspace, so sending the remedy back was refused. It now
//! echoes the caller's own `path` as it was sent, and the round trip
//! succeeds.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use common::{McpTestClient, StderrMode, unwrap_mcp_content};
use serde_json::{Value, json};
use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsRequest};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_plugin_registry::{PluginSelectionConfig, UnreadableManifestPolicy};

const LIB_RS: &str = "pub fn alpha() { beta(); }\npub fn beta() {}\n";

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A canonical workspace root with a Cargo project, beside its guard.
fn workspace(lib_rs: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"r7s3\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    fs::write(root.join("src").join("lib.rs"), lib_rs).expect("lib.rs");
    (tmp, root)
}

fn index(root: &Path, request: &MacroOptionsRequest) {
    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        root,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::Refuse,
        "test:r7s3_default_preset",
        &BuildConfig::default(),
        request,
        sqry_core::progress::no_op_reporter(),
    )
    .expect("the fixture index builds");
}

/// The server under its default preset, `preset` `None`, or under the
/// preset named, with `root` as the workspace it resolves without a path.
fn server(root: &Path, preset: Option<&str>) -> McpTestClient {
    let workspace = ("SQRY_MCP_WORKSPACE_ROOT".to_string(), path_text(root));
    let mut client = match preset {
        None => McpTestClient::new_with_default_redaction(&[workspace], StderrMode::Null),
        Some(preset) => McpTestClient::new_with_env_and_stderr_mode(
            &[
                workspace,
                ("SQRY_REDACTION_PRESET".to_string(), preset.to_string()),
            ],
            StderrMode::Null,
        ),
    }
    .expect("spawn sqry-mcp");
    client.initialize().expect("initialize");
    client
}

fn call_tool(client: &mut McpTestClient, name: &str, arguments: Value, id: i64) -> Value {
    let response = client
        .call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            id,
        )
        .expect("a response");
    println!("{name} {arguments}: {response}");
    response
}

fn error_of(response: &Value) -> &Value {
    assert!(
        response.get("result").is_none(),
        "the call must be refused: {response}"
    );
    response.get("error").expect("an error object")
}

/// A refusal with the runtime budget's row count masked: the parallel
/// evaluation examines one or a few rows past the limit before it stops, so
/// the count (in `details.examined` and the message) differs between runs
/// of the same request; everything else is compared byte for byte.
fn without_the_row_count(error: &Value) -> Value {
    let mut masked = error.clone();
    if masked["data"]["details"]["source"] == "runtime_budget" {
        let examined = masked["data"]["details"]["examined"]
            .as_u64()
            .expect("a row count");
        let limit = masked["data"]["details"]["limit"]
            .as_u64()
            .expect("a limit");
        assert!(examined >= limit, "{error}");
        masked["data"]["details"]["examined"] = Value::Null;
        masked["message"] = Value::String(
            masked["message"]
                .as_str()
                .expect("a message")
                .replace(&format!("examined {examined} rows"), "examined <n> rows"),
        );
    }
    masked
}

/// Every `query_too_broad` emitter keeps `details.source` under the default
/// preset: the executor's static gate and the planner's static gate say
/// `static_estimate`, the runtime row budget says `runtime_budget`. Each
/// refusal is byte-identical to the same refusal under the preset `none`,
/// so the preset changed nothing in it. The graph holds more than the
/// gate's 50,000-node threshold, which the static gates need to refuse.
#[test]
fn query_too_broad_keeps_details_source_under_the_default_preset() -> Result<()> {
    let mut lib_rs = String::new();
    let mut modules = Vec::new();
    for module in 0..34 {
        lib_rs.push_str(&format!("pub mod m{module};\n"));
        let body: String = (0..1500)
            .map(|function| format!("pub fn f{module}_{function}() {{}}\n"))
            .collect();
        modules.push((format!("m{module}.rs"), body));
    }
    let (_tmp, root) = workspace(&lib_rs);
    for (name, body) in modules {
        fs::write(root.join("src").join(name), body)?;
    }
    index(&root, &MacroOptionsRequest::empty());

    let cases = [
        (
            "semantic_search",
            json!({ "query": "name~=/.*/", "path": path_text(&root) }),
            "static_estimate",
        ),
        (
            "sqry_query",
            json!({ "query": "name:*a*", "path": path_text(&root) }),
            "static_estimate",
        ),
        (
            "semantic_search",
            json!({ "query": "kind:function", "budget_rows": 1, "path": path_text(&root) }),
            "runtime_budget",
        ),
    ];
    let mut default_preset = server(&root, None);
    let mut no_preset = server(&root, Some("none"));
    for (id, (tool, arguments, source)) in (1..).zip(cases) {
        let redacted = call_tool(&mut default_preset, tool, arguments.clone(), id);
        let plain = call_tool(&mut no_preset, tool, arguments.clone(), id);
        let error = error_of(&redacted);
        assert_eq!(error["code"], -32602, "{tool}: {error}");
        assert_eq!(error["data"]["kind"], "query_too_broad", "{tool}: {error}");
        assert_eq!(
            error["data"]["details"]["source"], source,
            "{tool} {arguments}: {error}"
        );
        assert_eq!(
            without_the_row_count(error),
            without_the_row_count(error_of(&plain)),
            "{tool}: the default preset changed a refusal that names no path"
        );
    }
    Ok(())
}

/// The symbol a `direct_callees` answer is about (`data.source`) and the one
/// a `direct_callers` answer is about (`data.target`) are names, and the
/// default preset keeps them; the absolute paths in the same answers are
/// still redacted, where the preset `none` shows them (the other side).
#[test]
fn symbol_names_under_source_and_target_survive_and_paths_do_not() -> Result<()> {
    let (_tmp, root) = workspace(LIB_RS);
    index(&root, &MacroOptionsRequest::empty());
    let mut default_preset = server(&root, None);
    let mut no_preset = server(&root, Some("none"));
    let cases = [
        ("direct_callees", "alpha", "source"),
        ("direct_callers", "beta", "target"),
    ];
    for (id, (tool, symbol, key)) in (1..).zip(cases) {
        let arguments = json!({ "symbol": symbol, "path": path_text(&root) });
        let redacted = call_tool(&mut default_preset, tool, arguments.clone(), id);
        let payload = unwrap_mcp_content(&redacted)?;
        assert_eq!(payload["data"][key], symbol, "{tool}: {payload}");
        assert!(
            !payload.to_string().contains(&path_text(&root)),
            "{tool}: no absolute path survives the default preset: {payload}"
        );
        let plain = unwrap_mcp_content(&call_tool(&mut no_preset, tool, arguments, id))?;
        assert_eq!(plain["data"][key], symbol, "{tool}: {plain}");
        assert!(
            plain.to_string().contains(&path_text(&root)),
            "{tool}: the preset none shows the absolute paths: {plain}"
        );
    }
    Ok(())
}

/// Hand-edit the manifest at `root` so its record names `dir` as given.
fn record_expand_cache_as(root: &Path, dir: &str) {
    let storage = GraphStorage::new(root);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"] = json!({ "cfg_flags": ["test"], "expand_cache_dir": dir });
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");
}

fn recorded_macro_options(root: &Path) -> Option<Value> {
    let manifest: Value = serde_json::from_slice(
        &fs::read(GraphStorage::new(root).manifest_path()).expect("manifest"),
    )
    .expect("json");
    manifest
        .get("macro_options")
        .filter(|record| !record.is_null())
        .cloned()
}

/// The recorded-cache refusal's `details.reset_arguments` can be sent back
/// as it stands under the default preset, and the reset succeeds: with the
/// `path` spelled as the caller spelled it (not canonical), and with no
/// `path` at all (the server's default `.` echoed). The rest of the refusal
/// is still redacted: `details.root` is the workspace placeholder.
#[test]
fn the_reset_remedy_round_trips_under_the_default_preset() -> Result<()> {
    let (_tmp, root) = workspace(LIB_RS);
    let cache = root.join("expand-cache");
    fs::create_dir(&cache)?;
    index(
        &root,
        &MacroOptionsRequest::from_flags(&["test".to_string()], Some(&cache), false),
    );
    fs::remove_dir_all(&cache)?;
    let mut client = server(&root, None);

    // Spelled the caller's way, through `.`, which the canonical root is not.
    let as_sent = format!("{}/.", path_text(&root));
    let refused = call_tool(
        &mut client,
        "rebuild_index",
        json!({ "path": as_sent, "force": true }),
        1,
    );
    let error = error_of(&refused);
    assert_eq!(
        error["data"]["kind"], "rebuild_macro_options_unavailable",
        "{error}"
    );
    assert_eq!(error["data"]["details"]["origin"], "recorded", "{error}");
    assert_eq!(error["data"]["details"]["root"], "<workspace>", "{error}");
    let reset = error["data"]["details"]["reset_arguments"].clone();
    assert_eq!(
        reset,
        json!({ "path": as_sent, "force": true, "reset_macro_options": true }),
        "{error}"
    );
    assert!(recorded_macro_options(&root).is_some(), "precondition");
    let accepted = call_tool(&mut client, "rebuild_index", reset, 2);
    let payload = unwrap_mcp_content(&accepted)?;
    assert_eq!(payload["data"]["success"], true, "{payload}");
    assert_eq!(
        recorded_macro_options(&root),
        None,
        "the reset dropped the record"
    );

    // No `path`: the server reads its default `.`, which it echoes.
    record_expand_cache_as(&root, "missing-cache");
    let refused = call_tool(&mut client, "rebuild_index", json!({ "force": true }), 3);
    let error = error_of(&refused);
    let reset = error["data"]["details"]["reset_arguments"].clone();
    assert_eq!(
        reset,
        json!({ "path": ".", "force": true, "reset_macro_options": true }),
        "{error}"
    );
    let accepted = call_tool(&mut client, "rebuild_index", reset, 4);
    let payload = unwrap_mcp_content(&accepted)?;
    assert_eq!(payload["data"]["success"], true, "{payload}");
    assert_eq!(recorded_macro_options(&root), None);
    Ok(())
}

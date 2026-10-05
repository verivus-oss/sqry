//! The standalone `rebuild_index` refusals as a client receives them.
//!
//! Every test spawns the `sqry-mcp --no-daemon` binary and reads the raw
//! JSON-RPC response from its stdout, so what is asserted is the wire:
//! `error.code`, `error.message` and the whole `error.data` object. The
//! function-level tests in `shared_graph_acquisition.rs` render the
//! executor's error with `{:#}`, a format no client receives; before this
//! file every refusal below reached the client as
//! `{"code":-32603,"message":"Failed to build and persist unified graph"}`
//! with no `data`.
//!
//! The expected envelopes are the daemon-hosted MCP's for the same request
//! (`DaemonMcpHandler::handle_rebuild_index` and `daemon_err_to_mcp`):
//! `sqry-daemon/tests/macro_options_anchoring.rs` compares the two hosts
//! directly.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use common::{McpTestClient, StderrMode, unwrap_mcp_content};
use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::GraphStorage;

/// A workspace with one Rust file whose canonical root is returned beside
/// the guard. The root holds no `.sqry`; tests that need an index build it
/// through the server.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("lib.rs"),
        "#[cfg(test)]\npub fn gated() {}\npub fn plain() {}\n",
    )
    .expect("write lib.rs");
    let root = tmp.path().canonicalize().expect("canonical root");
    (tmp, root)
}

/// The prefixes the in-string pattern matched before round 8.
const OLD_PREFIXES: [&str; 7] = ["/home", "/Users", "/var", "/srv", "/opt", "/tmp", "/etc"];

/// [`workspace`], built under a directory outside every [`OLD_PREFIXES`]
/// entry (`/dev/shm` on Linux, `/private/tmp` on macOS, whichever is a
/// writable directory), whatever the host's `TMPDIR` is, so a path the
/// redactor finds only by its prefix fails the test that uses it. Fails,
/// never skips, when neither base is usable. Unix only: a Windows path is
/// pinned by the redaction crate's own tests.
#[cfg(unix)]
fn workspace_outside_the_old_prefixes() -> (tempfile::TempDir, PathBuf) {
    for base in ["/dev/shm", "/private/tmp"] {
        if !Path::new(base).is_dir() {
            continue;
        }
        let Ok(tmp) = tempfile::Builder::new()
            .prefix("sqry-r8-redaction-")
            .tempdir_in(base)
        else {
            continue;
        };
        fs::write(
            tmp.path().join("lib.rs"),
            "#[cfg(test)]\npub fn gated() {}\npub fn plain() {}\n",
        )
        .expect("write lib.rs");
        let root = tmp.path().canonicalize().expect("canonical root");
        assert!(
            !OLD_PREFIXES.iter().any(|prefix| root.starts_with(prefix)),
            "{} is under an old prefix",
            root.display()
        );
        return (tmp, root);
    }
    panic!("no writable /dev/shm or /private/tmp to hold the workspace");
}

fn server_for(root: &Path) -> McpTestClient {
    let mut client = McpTestClient::new_with_env_and_stderr_mode(
        &[(
            "SQRY_MCP_WORKSPACE_ROOT".to_string(),
            root.to_string_lossy().into_owned(),
        )],
        StderrMode::Null,
    )
    .expect("spawn sqry-mcp");
    client.initialize().expect("initialize");
    client
}

/// The raw response to one `rebuild_index` call with `arguments`.
fn call(client: &mut McpTestClient, arguments: Value, id: i64) -> Value {
    client
        .call(
            "tools/call",
            json!({ "name": "rebuild_index", "arguments": arguments }),
            id,
        )
        .expect("a response")
}

/// The `error` object of a refused call, printed for the record.
fn error_of(response: &Value) -> &Value {
    println!("response: {response}");
    assert!(
        response.get("result").is_none(),
        "the call must be refused: {response}"
    );
    response.get("error").expect("an error object")
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Manifest and snapshot bytes, to show a refusal wrote nothing.
fn index_bytes(root: &Path) -> (Vec<u8>, Vec<u8>) {
    let storage = GraphStorage::new(root);
    (
        fs::read(storage.manifest_path()).expect("manifest bytes"),
        fs::read(storage.snapshot_path()).expect("snapshot bytes"),
    )
}

fn build_index(client: &mut McpTestClient, root: &Path, id: i64) {
    let response = call(
        client,
        json!({ "path": path_text(root), "force": true }),
        id,
    );
    let payload = unwrap_mcp_content(&response).expect("a successful rebuild");
    assert_eq!(payload["data"]["success"], true, "{payload}");
}

fn recorded_expand_cache(root: &Path) -> Option<String> {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
        .and_then(|record| record.expand_cache_dir)
}

/// The envelope the daemon-hosted MCP gives `DaemonError::InvalidArgument`.
fn invalid_argument(reason: &str) -> (i64, String, Value) {
    (
        -32602,
        format!("invalid argument: {reason}"),
        json!({
            "kind": "validation_error",
            "retryable": false,
            "retry_after_ms": null,
            "details": { "reason": reason },
        }),
    )
}

/// The `rebuild_macro_options_unavailable` envelope for a directory the
/// request named (`expand_cache`), written out here rather than derived
/// from the constructor: the reason by shape, and the remedy for an MCP
/// caller (a directory that exists; no CLI command, no reset).
fn requested_cache_unavailable(root: &Path, dir: &Path, reason: &str) -> (i64, String, Value) {
    (
        -32602,
        format!(
            "rebuild of {} refused: {reason}; the request named it: pass expand_cache naming a \
             directory that exists, or omit expand_cache",
            root.display()
        ),
        json!({
            "kind": "rebuild_macro_options_unavailable",
            "retryable": false,
            "retry_after_ms": null,
            "details": {
                "root": path_text(root),
                "expand_cache_dir": path_text(dir),
                "origin": "requested",
            },
        }),
    )
}

/// The envelope for a directory the index manifest records: the remedy
/// names `reset_macro_options`, and `details.reset_arguments` are the
/// `rebuild_index` arguments that drop the record.
fn recorded_cache_unavailable(root: &Path, dir: &Path, reason: &str) -> (i64, String, Value) {
    (
        -32602,
        format!(
            "rebuild of {} refused: {reason}; the index manifest records it: drop the record \
             with reset_macro_options: true and force: true, or pass expand_cache naming a \
             directory that exists",
            root.display()
        ),
        json!({
            "kind": "rebuild_macro_options_unavailable",
            "retryable": false,
            "retry_after_ms": null,
            "details": {
                "root": path_text(root),
                "expand_cache_dir": path_text(dir),
                "origin": "recorded",
                "reset_arguments": {
                    "path": path_text(root),
                    "force": true,
                    "reset_macro_options": true,
                },
            },
        }),
    )
}

fn not_a_directory(dir: &Path) -> String {
    format!(
        "expand cache directory {} does not exist or is not a directory",
        dir.display()
    )
}

/// The `force=false` refusal of macro arguments, worded so it is true on
/// both hosts (an index on disk here, possibly a resident graph on the
/// daemon host).
fn need_force_reason(root: &Path) -> String {
    format!(
        "rebuild_index: cfg_flags, expand_cache and reset_macro_options need force=true when a \
         graph already exists at {} (an index on disk or a workspace loaded in the daemon); \
         nothing was built",
        root.display()
    )
}

/// Hand-edit the manifest at `root` so its record names `dir` as given.
fn record_expand_cache_as(root: &Path, dir: &str) {
    let storage = GraphStorage::new(root);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"] = json!({ "cfg_flags": [], "expand_cache_dir": dir });
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");
}

fn assert_envelope(response: &Value, expected: &(i64, String, Value)) {
    let error = error_of(response);
    assert_eq!(error["code"], expected.0, "code: {error}");
    assert_eq!(error["message"], expected.1.as_str(), "message: {error}");
    assert_eq!(error["data"], expected.2, "data: {error}");
}

/// Macro arguments beside `force=false` over an existing index reach no
/// build: each of the three is refused with `-32602` and the daemon host's
/// reason, and the index is untouched. The control without them reports
/// the existing index.
#[test]
fn standalone_rebuild_index_refuses_macro_arguments_without_force_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);
    let reason = need_force_reason(&root);
    for (id, extra) in [
        (2, json!({ "cfg_flags": ["test"] })),
        (3, json!({ "expand_cache": "cache" })),
        (4, json!({ "reset_macro_options": true })),
    ] {
        let mut arguments = json!({ "path": path_text(&root), "force": false });
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = call(&mut client, arguments, id);
        assert_envelope(&response, &invalid_argument(&reason));
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false }),
        5,
    );
    let payload = unwrap_mcp_content(&response)?;
    assert_eq!(
        payload["data"]["message"],
        "Index already exists. Use force=true to rebuild."
    );
    Ok(())
}

/// An empty expand cache and an empty or blank cfg flag name nothing; each
/// is refused with `-32602` and the request check's message, and nothing
/// is written. A real flag is the accepted control.
#[test]
fn standalone_rebuild_index_refuses_empty_macro_values_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);

    let empty_cache = format!(
        "rebuild of {} refused: the expand cache directory is empty; pass --expand-cache <DIR> \
         (or the expand_cache field) naming a directory, or omit it to keep the recorded one",
        root.display()
    );
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "expand_cache": "" }),
        2,
    );
    assert_envelope(&response, &invalid_argument(&empty_cache));

    let empty_flag = format!(
        "rebuild of {} refused: a cfg flag is empty; an empty flag names no predicate, so pass \
         --cfg <PREDICATE> (or a non-empty cfg_flags item), or omit it to keep the recorded flags",
        root.display()
    );
    for (id, flags) in [
        (3, json!([""])),
        (4, json!(["  "])),
        (5, json!(["test", ""])),
    ] {
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": true, "cfg_flags": flags }),
            id,
        );
        assert_envelope(&response, &invalid_argument(&empty_flag));
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "cfg_flags": ["test"] }),
        6,
    );
    unwrap_mcp_content(&response)?;
    let record = GraphStorage::new(&root)
        .load_manifest()?
        .macro_options
        .expect("the flag is recorded");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    Ok(())
}

/// A missing directory, a file and a dangling link the request names are
/// refused with `rebuild_macro_options_unavailable`, described by shape
/// ("does not exist or is not a directory", never only "does not exist"),
/// with the remedy for a requested directory: one that exists, no CLI
/// command and no reset (S10, round 7). Nothing is written.
#[test]
fn standalone_rebuild_index_refuses_a_missing_or_file_expand_cache_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    fs::write(root.join("a-file"), b"not a directory")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("gone"), root.join("dangling"))?;
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);
    let mut names = vec![(2, "missing-cache"), (3, "a-file")];
    if cfg!(unix) {
        names.push((4, "dangling"));
    }
    for (id, name) in names {
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": true, "expand_cache": name }),
            id,
        );
        let dir = root.join(name);
        assert_envelope(
            &response,
            &requested_cache_unavailable(&root, &dir, &not_a_directory(&dir)),
        );
        let message = error_of(&response)["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("sqry daemon rebuild") && !message.contains("reset_macro_options"),
            "no CLI command and no reset for a requested directory: {message}"
        );
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");
    Ok(())
}

/// A directory the index manifest records that is missing, a file, or the
/// empty path is refused with the remedy for a recorded directory (drop the
/// record with `reset_macro_options`, or name one that exists) and the
/// arguments that drop it; the empty path is described as naming no
/// directory, not as one "that does not exist" (S10). The reset itself is
/// the accepted control.
#[test]
fn standalone_rebuild_index_refuses_a_recorded_unusable_expand_cache_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    fs::write(root.join("a-file"), b"not a directory")?;
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let cases = [
        (2, "missing-cache", root.join("missing-cache")),
        (3, "a-file", root.join("a-file")),
        (4, "", PathBuf::new()),
    ];
    for (id, recorded, dir) in &cases {
        record_expand_cache_as(&root, recorded);
        let before = index_bytes(&root);
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": true }),
            *id,
        );
        let reason = if recorded.is_empty() {
            "the expand cache directory is empty, so it names no directory".to_string()
        } else {
            not_a_directory(dir)
        };
        assert_envelope(&response, &recorded_cache_unavailable(&root, dir, &reason));
        assert_eq!(
            index_bytes(&root),
            before,
            "{recorded:?}: nothing was written"
        );
    }

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "reset_macro_options": true }),
        5,
    );
    unwrap_mcp_content(&response)?;
    assert_eq!(
        recorded_expand_cache(&root),
        None,
        "the reset dropped the record"
    );
    Ok(())
}

/// A cfg flag with leading or trailing whitespace is refused by name with
/// the request check's message (round 7); the trimmed flag is the accepted
/// control and is recorded as given.
#[test]
fn standalone_rebuild_index_refuses_a_padded_cfg_flag_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);
    for (id, flag) in [(2, " test"), (3, "test "), (4, "\ttest")] {
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": true, "cfg_flags": ["unix", flag] }),
            id,
        );
        let reason = format!(
            "rebuild of {} refused: cfg flag {flag:?} has leading or trailing whitespace, so it is \
             not the predicate it names; pass --cfg <PREDICATE> (or a cfg_flags item) without the \
             surrounding whitespace",
            root.display()
        );
        assert_envelope(&response, &invalid_argument(&reason));
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "cfg_flags": ["test"] }),
        5,
    );
    unwrap_mcp_content(&response)?;
    let record = GraphStorage::new(&root)
        .load_manifest()?
        .macro_options
        .expect("recorded");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    Ok(())
}

/// A UTF-8 link into a directory whose name is not valid UTF-8 is refused
/// with `-32602` carrying the resolver's message (design W4-D11).
#[cfg(unix)]
#[test]
fn standalone_rebuild_index_refuses_a_non_utf8_expand_cache_on_the_wire() -> Result<()> {
    use std::os::unix::ffi::OsStringExt;

    let (_tmp, root) = workspace();
    let mut name = b"cache-".to_vec();
    name.push(0xff);
    let target = root.join(std::ffi::OsString::from_vec(name));
    fs::create_dir(&target)?;
    std::os::unix::fs::symlink(&target, root.join("cache-link"))?;
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "expand_cache": "cache-link" }),
        2,
    );
    let reason = format!(
        "rebuild of {} refused: expand cache directory {} is not valid UTF-8; the graph manifest \
         records the directory as JSON text, so it cannot be recorded and reused as given; pass \
         --expand-cache <DIR> (or the expand_cache field) naming a directory whose path is valid \
         UTF-8, or --no-macro-options (reset_macro_options) to drop the recorded macro options",
        root.display(),
        target.display()
    );
    assert_envelope(&response, &invalid_argument(&reason));
    assert_eq!(index_bytes(&root), before, "nothing was written");
    Ok(())
}

/// A readable manifest naming a plugin id this binary did not compile is
/// refused with the daemon host's `workspace_incompatible_graph` envelope,
/// with and without `force`, and nothing is written.
#[test]
fn standalone_rebuild_index_refuses_an_uncompiled_plugin_id_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let storage = GraphStorage::new(&root);
    let mut manifest: Value = serde_json::from_slice(&fs::read(storage.manifest_path())?)?;
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(json!("r7-uncompiled-plugin"));
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let before = index_bytes(&root);
    for (id, force) in [(2, false), (3, true)] {
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": force }),
            id,
        );
        let error = error_of(&response);
        assert_eq!(error["code"], -32603, "{error}");
        assert_eq!(
            error["data"]["kind"], "workspace_incompatible_graph",
            "{error}"
        );
        assert_eq!(error["data"]["retryable"], false, "{error}");
        assert!(error["data"]["retry_after_ms"].is_null(), "{error}");
        assert_eq!(
            error["data"]["details"]["root"],
            path_text(&root),
            "{error}"
        );
        let reason = error["data"]["details"]["reason"]
            .as_str()
            .expect("a reason");
        assert!(
            reason.contains("r7-uncompiled-plugin")
                && reason.contains(&path_text(storage.manifest_path())),
            "the reason names the id and the manifest: {reason}"
        );
        assert_eq!(
            error["message"],
            format!(
                "workspace {} graph is incompatible with this binary: {reason}",
                root.display()
            ),
            "{error}"
        );
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");
    Ok(())
}

/// An unreadable manifest: `force=false` is refused with the daemon host's
/// `workspace_not_ready` envelope naming the file and the repair; a forced
/// rebuild falls back, records the fallback and succeeds (surface parity
/// W1, design D9), also when the server already holds an engine for the
/// root (whose freshness refresh would otherwise refuse first).
#[test]
fn standalone_rebuild_index_unreadable_manifest_refuses_without_force_and_falls_back_with_it()
-> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    // Cache an engine for the root before the manifest goes bad.
    let status = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
        2,
    )?;
    unwrap_mcp_content(&status)?;
    let storage = GraphStorage::new(&root);
    fs::write(storage.manifest_path(), b"{")?;

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false }),
        3,
    );
    let error = error_of(&response);
    assert_eq!(error["code"], -32603, "{error}");
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{error}");
    assert_eq!(error["data"]["retryable"], false, "{error}");
    assert!(error["data"]["retry_after_ms"].is_null(), "{error}");
    let details = &error["data"]["details"];
    assert_eq!(details["root"], path_text(&root));
    assert_eq!(details["manifest_path"], path_text(storage.manifest_path()));
    assert_eq!(
        details["repair_command"],
        format!("sqry index --force {}", root.display())
    );
    let reason = details["reason"].as_str().expect("a reason");
    assert_eq!(
        error["message"],
        format!(
            "manifest at {} cannot be read ({reason}); repair with: sqry index --force {}",
            storage.manifest_path().display(),
            root.display()
        )
    );
    assert_eq!(
        fs::read(storage.manifest_path())?,
        b"{",
        "the refusal wrote nothing"
    );

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true }),
        4,
    );
    let payload = unwrap_mcp_content(&response)?;
    assert_eq!(payload["data"]["success"], true, "{payload}");
    let manifest = storage.load_manifest()?;
    assert_eq!(
        manifest
            .plugin_selection
            .as_ref()
            .and_then(|selection| selection.high_cost_mode.as_deref()),
        Some("fast_path_default"),
        "the fallback selection is recorded"
    );
    Ok(())
}

/// Unknown arguments are refused: a misspelled macro field would otherwise
/// be dropped and the rebuild would run with the recorded options. The
/// correctly spelled field is the accepted control.
#[test]
fn standalone_rebuild_index_refuses_unknown_arguments() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let before = index_bytes(&root);
    for (id, field) in [(2, "cfg_flag"), (3, "expandCache"), (4, "reset")] {
        let response = call(
            &mut client,
            json!({ "path": path_text(&root), "force": true, field: ["test"] }),
            id,
        );
        let error = error_of(&response);
        assert_eq!(error["code"], -32602, "{error}");
        let message = error["message"].as_str().expect("a message");
        assert!(
            message.contains("unknown field") && message.contains(field),
            "the refusal names the field: {message}"
        );
    }
    assert_eq!(index_bytes(&root), before, "nothing was written");

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "cfg_flags": ["test"] }),
        5,
    );
    unwrap_mcp_content(&response)?;
    Ok(())
}

/// A relative `expand_cache` resolves against the directory `path` names:
/// the subdirectory itself, or a file's parent directory, and never the
/// server's working directory. `src` exists in the server's working
/// directory (the `sqry-mcp` crate, where the test binary spawns it) and not
/// under the indexed directory, so it must be refused naming the indexed
/// directory's `src`. The directory the request names is recorded, so the
/// conversion from the wire parameters cannot drop it.
#[test]
fn standalone_rebuild_index_anchors_a_relative_expand_cache_to_the_indexed_directory() -> Result<()>
{
    assert!(
        Path::new("src").is_dir(),
        "precondition: the server's working directory holds src"
    );
    let (_tmp, root) = workspace();
    let sub = root.join("sub");
    fs::create_dir_all(sub.join("cache-here"))?;
    fs::write(sub.join("lib.rs"), "pub fn in_sub() {}\n")?;
    let mut client = server_for(&root);

    let response = call(
        &mut client,
        json!({ "path": path_text(&sub), "force": true, "expand_cache": "cache-here" }),
        1,
    );
    unwrap_mcp_content(&response)?;
    assert_eq!(
        recorded_expand_cache(&sub).as_deref(),
        Some(path_text(&sub.join("cache-here")).as_str()),
        "the subdirectory's cache is recorded"
    );

    let response = call(
        &mut client,
        json!({ "path": path_text(&sub.join("lib.rs")), "force": true, "expand_cache": "cache-here" }),
        2,
    );
    unwrap_mcp_content(&response)?;
    assert_eq!(
        recorded_expand_cache(&sub).as_deref(),
        Some(path_text(&sub.join("cache-here")).as_str()),
        "a file's parent directory anchors the cache"
    );

    let before = index_bytes(&sub);
    let response = call(
        &mut client,
        json!({ "path": path_text(&sub), "force": true, "expand_cache": "src" }),
        3,
    );
    assert_envelope(
        &response,
        &requested_cache_unavailable(&sub, &sub.join("src"), &not_a_directory(&sub.join("src"))),
    );
    assert_eq!(index_bytes(&sub), before, "nothing was written");
    Ok(())
}

/// Any existing directory is an expand cache, the indexed directory
/// included: `.` records its canonical path and `..` its parent's.
#[test]
fn standalone_rebuild_index_records_dot_and_dotdot_as_the_canonical_directory() -> Result<()> {
    let (_tmp, root) = workspace();
    let sub = root.join("sub");
    fs::create_dir_all(&sub)?;
    fs::write(sub.join("lib.rs"), "pub fn in_sub() {}\n")?;
    let mut client = server_for(&root);
    for (id, input, expected) in [(1, ".", sub.clone()), (2, "..", root.clone())] {
        let response = call(
            &mut client,
            json!({ "path": path_text(&sub), "force": true, "expand_cache": input }),
            id,
        );
        unwrap_mcp_content(&response)?;
        assert_eq!(
            recorded_expand_cache(&sub).as_deref(),
            Some(path_text(&expected).as_str()),
            "{input}"
        );
    }
    Ok(())
}

/// The server's fallback for an error that is not an `RpcError` renders
/// the whole chain: a read tool over a cached engine whose manifest went
/// bad is refused by the engine's refresh with a context
/// ("Failed to parse manifest at ...") over the parser's reason, and the
/// client now sees both, where it saw only the context.
#[test]
fn the_server_fallback_renders_the_whole_error_chain() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let status = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
        2,
    )?;
    unwrap_mcp_content(&status)?;
    let storage = GraphStorage::new(&root);
    fs::write(storage.manifest_path(), b"{")?;
    let response = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
        3,
    )?;
    let error = error_of(&response);
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains(&format!(
            "Failed to parse manifest at {}",
            storage.manifest_path().display()
        )) && message.contains("EOF while parsing"),
        "the context and its cause: {message}"
    );
    Ok(())
}

/// The server under its own default redaction preset (`minimal`): no
/// redaction variable is set, as for an operator who configured nothing.
fn default_preset_server_for(root: &Path) -> McpTestClient {
    let mut client = McpTestClient::new_with_default_redaction(
        &[(
            "SQRY_MCP_WORKSPACE_ROOT".to_string(),
            root.to_string_lossy().into_owned(),
        )],
        StderrMode::Null,
    )
    .expect("spawn sqry-mcp");
    client.initialize().expect("initialize");
    client
}

/// `true` when `text` names the absolute directory `dir` or anything under
/// it.
fn names(text: &str, dir: &Path) -> bool {
    text.contains(&path_text(dir))
}

/// S3 (round 7): under the default preset every refusal is redacted the way
/// a successful response is: no absolute path survives in the message or
/// anywhere in `data` (`details.root`, `expand_cache_dir`, `manifest_path`,
/// the reason text), while the envelope keeps its code and `kind`. The one
/// path kept is `details.reset_arguments.path`, the caller's own `path`
/// echoed as it was sent, so the remedy can be sent back (round three,
/// finding 2). The refusals that used to bypass the
/// redactor are the `RpcError` ones; the resolution and nested-index
/// refusals are raised before a workspace is bound and are redacted too.
/// The other side: the same refusal from a server with the preset `none`
/// names the absolute directory. On Unix the workspace sits outside every
/// prefix the in-string pattern listed before round 8 (B3).
#[test]
fn standalone_refusals_are_redacted_under_the_default_preset() -> Result<()> {
    #[cfg(unix)]
    let (tmp, root) = workspace_outside_the_old_prefixes();
    #[cfg(not(unix))]
    let (tmp, root) = workspace();
    fs::create_dir_all(root.join("sub"))?;
    fs::write(root.join("sub").join("m.rs"), "pub fn in_sub() {}\n")?;
    let mut client = default_preset_server_for(&root);
    build_index(&mut client, &root, 1);
    let success = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
        2,
    )?;
    assert!(
        !names(&success.to_string(), &root),
        "precondition: a success is redacted: {success}"
    );

    let mut refusals = vec![
        (
            json!({ "path": path_text(&root), "force": true, "expand_cache": "missing-cache" }),
            "rebuild_macro_options_unavailable",
        ),
        (
            json!({ "path": path_text(&root), "force": false, "cfg_flags": ["test"] }),
            "validation_error",
        ),
        (
            json!({ "path": path_text(&root), "force": true, "cfg_flags": [" test"] }),
            "validation_error",
        ),
        (
            json!({ "path": path_text(&root.join("nope")), "force": true }),
            "validation_error",
        ),
        (
            json!({ "path": path_text(&root.join("sub")), "force": false }),
            "validation_error",
        ),
    ];
    let mut id = 3;
    for (arguments, kind) in refusals.drain(..) {
        let response = call(&mut client, arguments.clone(), id);
        id += 1;
        let error = error_of(&response);
        assert_eq!(error["data"]["kind"], kind, "{arguments}: {error}");
        assert!(
            !names(&error.to_string(), tmp.path()) && !names(&error.to_string(), &root),
            "{arguments}: no absolute path survives: {error}"
        );
    }
    // A refusal a tool returns is redacted with the redactor bound to the
    // request's workspace, so the root reads `<workspace>` in the message,
    // as it does in a success, not only as an opaque external name.
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true, "expand_cache": "missing-cache" }),
        id,
    );
    id += 1;
    let error = error_of(&response);
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("rebuild of <workspace> refused: ")),
        "{error}"
    );
    assert_eq!(error["data"]["details"]["root"], "<workspace>", "{error}");

    // A recorded cache, an unreadable manifest and an uncompiled plugin id:
    // the details that carry paths (`root`, `expand_cache_dir`,
    // `manifest_path`, `repair_command`, the reason) are redacted as well;
    // `reset_arguments.path` echoes the caller's own `path` as it was sent.
    record_expand_cache_as(&root, "missing-cache");
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true }),
        id,
    );
    id += 1;
    let error = error_of(&response);
    assert_eq!(error["data"]["details"]["origin"], "recorded", "{error}");
    assert_eq!(
        error["data"]["details"]["reset_arguments"]["path"],
        path_text(&root),
        "{error}"
    );
    let mut without_the_echo = error.clone();
    without_the_echo["data"]["details"]["reset_arguments"]["path"] = Value::Null;
    assert!(!names(&without_the_echo.to_string(), &root), "{error}");
    record_expand_cache_as(&root, "");
    let storage = GraphStorage::new(&root);
    let mut manifest: Value = serde_json::from_slice(&fs::read(storage.manifest_path())?)?;
    manifest["macro_options"] = Value::Null;
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("ids")
        .push(json!("r7-uncompiled-plugin"));
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true }),
        id,
    );
    id += 1;
    let error = error_of(&response);
    assert_eq!(
        error["data"]["kind"], "workspace_incompatible_graph",
        "{error}"
    );
    assert!(!names(&error.to_string(), &root), "{error}");
    fs::write(storage.manifest_path(), b"{")?;
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false }),
        id,
    );
    let error = error_of(&response);
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{error}");
    assert!(!names(&error.to_string(), &root), "{error}");
    drop(client);

    // The other side: the preset `none` shows the absolute directory.
    let (_tmp2, root2) = workspace();
    let mut plain = server_for(&root2);
    build_index(&mut plain, &root2, 1);
    let response = call(
        &mut plain,
        json!({ "path": path_text(&root2), "force": true, "expand_cache": "missing-cache" }),
        2,
    );
    assert!(names(&error_of(&response).to_string(), &root2));
    Ok(())
}

/// S5 (round 7): a `path` that names no workspace is the request's argument
/// refused, `-32602` `validation_error` naming the path, as the
/// daemon-hosted MCP refuses it (it answered `-32600` with no data): an
/// absolute path that does not exist, and a relative one that resolves
/// under no root, both before and after the session has resolved a
/// workspace. A relative path that does resolve, on a session that has
/// resolved nothing yet, is the accepted control (the standalone server
/// resolves relative paths by design; the daemon host, with no client
/// working directory, refuses them, issue #566).
#[test]
fn standalone_rebuild_index_refuses_a_path_naming_no_workspace_as_an_invalid_argument() -> Result<()>
{
    let (_tmp, root) = workspace();
    fs::create_dir_all(root.join("sub"))?;
    fs::write(root.join("sub").join("m.rs"), "pub fn in_sub() {}\n")?;
    let mut client = server_for(&root);
    let assert_refused = |response: &Value, named: &str| {
        let error = error_of(response);
        assert_eq!(error["code"], -32602, "{error}");
        assert_eq!(error["data"]["kind"], "validation_error", "{error}");
        let message = error["message"].as_str().expect("a message");
        assert!(
            message.starts_with(
                "invalid argument: rebuild_index: cannot resolve the workspace for this request: "
            ) && message.contains(named),
            "{message}"
        );
        assert_eq!(
            format!(
                "invalid argument: {}",
                error["data"]["details"]["reason"].as_str().unwrap()
            ),
            message
        );
    };
    // Before any workspace was resolved: the relative path is joined onto
    // the server's workspace root and does not exist there.
    let response = call(
        &mut client,
        json!({ "path": "nope/deeper", "force": true }),
        1,
    );
    assert_refused(&response, "nope");
    let missing = root.join("nope");
    let response = call(
        &mut client,
        json!({ "path": path_text(&missing), "force": true }),
        2,
    );
    assert_refused(&response, &path_text(&missing));
    // The whole chain: the context naming the path and the cause below it.
    assert!(
        error_of(&response)["message"]
            .as_str()
            .is_some_and(|message| message.contains("No such file or directory")),
        "{response}"
    );

    build_index(&mut client, &root, 3);
    // After one: the relative path is resolved under the session's roots
    // and the last workspace, and matches none.
    let response = call(
        &mut client,
        json!({ "path": "nope/deeper", "force": true }),
        4,
    );
    assert_refused(&response, "nope");

    drop(client);

    let mut fresh = server_for(&root);
    let response = call(&mut fresh, json!({ "path": "sub", "force": true }), 1);
    let payload = unwrap_mcp_content(&response)?;
    assert_eq!(payload["data"]["success"], true, "{payload}");
    assert!(GraphStorage::new(&root.join("sub")).exists());
    Ok(())
}

/// S5 (round 7): `force=false` on a subdirectory of an indexed project is
/// refused before anything is built (cluster E, design E.3: `force` is the
/// MCP opt-in to a nested index), as an invalid argument naming the
/// ancestor index and `force`, not the CLI's `--allow-nested` (it was
/// `-32603` with no data). `force=true` builds the nested index (the
/// accepted side).
#[test]
fn standalone_rebuild_index_refuses_a_nested_index_without_force() -> Result<()> {
    let (_tmp, root) = workspace();
    let sub = root.join("sub");
    fs::create_dir_all(&sub)?;
    fs::write(sub.join("m.rs"), "pub fn in_sub() {}\n")?;
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let response = call(
        &mut client,
        json!({ "path": path_text(&sub), "force": false }),
        2,
    );
    let error = error_of(&response);
    assert_eq!(error["code"], -32602, "{error}");
    assert_eq!(error["data"]["kind"], "validation_error", "{error}");
    let details = &error["data"]["details"];
    assert_eq!(details["root"], path_text(&sub), "{error}");
    assert_eq!(
        details["ancestor_graph"],
        path_text(GraphStorage::new(&root).graph_dir()),
        "{error}"
    );
    let message = error["message"].as_str().expect("a message");
    assert!(
        message
            .starts_with("invalid argument: rebuild_index: refusing to build a nested index at ")
            && message.contains("pass force: true")
            && !message.contains("--allow-nested"),
        "{message}"
    );
    assert!(!GraphStorage::new(&sub).exists(), "nothing was built");

    let response = call(
        &mut client,
        json!({ "path": path_text(&sub), "force": true }),
        3,
    );
    unwrap_mcp_content(&response)?;
    assert!(
        GraphStorage::new(&sub).exists(),
        "force builds the nested index"
    );
    Ok(())
}

/// S9 (round 7): a rebuild that fails (not a refusal) is the daemon host's
/// `workspace_build_failed` envelope: `-32603`, `workspace_not_ready`,
/// retryable after 2000 ms, `details` naming the root and the whole cause.
/// The graph directory is read-only, so persisting fails with the operating
/// system's reason.
#[cfg(unix)]
#[test]
fn standalone_rebuild_index_reports_a_failed_build_as_retryable() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    fs::set_permissions(&graph_dir, fs::Permissions::from_mode(0o555))?;
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true }),
        2,
    );
    fs::set_permissions(&graph_dir, fs::Permissions::from_mode(0o755))?;
    let error = error_of(&response);
    assert_eq!(error["code"], -32603, "{error}");
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{error}");
    assert_eq!(error["data"]["retryable"], true, "{error}");
    assert_eq!(error["data"]["retry_after_ms"], 2000, "{error}");
    assert_eq!(
        error["data"]["details"]["root"],
        path_text(&root),
        "{error}"
    );
    let reason = error["data"]["details"]["reason"]
        .as_str()
        .expect("a reason");
    assert!(reason.contains("Permission denied"), "{reason}");
    assert_eq!(
        error["message"],
        format!("workspace build failed: {reason}"),
        "{error}"
    );
    Ok(())
}

/// S5 (round 7), every tool: the resolution refusal is the server's, not
/// `rebuild_index`'s, so a read tool (`get_index_status`, served through the
/// other request wrapper) refuses a path that names no workspace the same
/// way, `-32602` `validation_error` naming the path and its cause; a path
/// that resolves is the accepted control.
#[test]
fn every_tool_refuses_a_path_naming_no_workspace_as_an_invalid_argument() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let missing = root.join("nope");
    let response = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&missing) } }),
        2,
    )?;
    let error = error_of(&response);
    assert_eq!(error["code"], -32602, "{error}");
    assert_eq!(error["data"]["kind"], "validation_error", "{error}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with(
            "invalid argument: get_index_status: cannot resolve the workspace for this request: "
        ) && message.contains(&path_text(&missing))
            && message.contains("No such file or directory"),
        "{message}"
    );
    let status = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
        3,
    )?;
    unwrap_mcp_content(&status)?;
    Ok(())
}

/// A manifest whose snapshot is gone is no index (round 7 residual, parity
/// with the daemon host's `a_manifest_without_its_snapshot_is_built_not_reported`):
/// `force=false` builds it instead of answering a cache hit, keeps the
/// recorded selection, applies a macro option rather than refusing it, and
/// answers "Index built successfully." as the daemon host does. An
/// unreadable manifest with no snapshot is still refused without `force`,
/// and nothing is written. A snapshot beside the manifest is the control:
/// it is reported, not rebuilt.
#[test]
fn a_manifest_without_its_snapshot_is_built_not_reported() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let storage = GraphStorage::new(&root);

    fs::remove_file(storage.snapshot_path())?;
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false, "cfg_flags": ["test"] }),
        2,
    );
    let payload = unwrap_mcp_content(&response)?;
    assert_eq!(payload["data"]["success"], true, "{payload}");
    assert_eq!(payload["data"]["message"], "Index built successfully.");
    assert!(storage.snapshot_exists(), "the build wrote the snapshot");
    let manifest = storage.load_manifest()?;
    assert_eq!(
        manifest
            .macro_options
            .as_ref()
            .map(|options| options.cfg_flags.clone()),
        Some(vec!["test".to_string()]),
        "the macro option was applied by the build"
    );

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false }),
        3,
    );
    let payload = unwrap_mcp_content(&response)?;
    assert_eq!(
        payload["data"]["message"], "Index already exists. Use force=true to rebuild.",
        "a snapshot beside the manifest is reported"
    );

    fs::remove_file(storage.snapshot_path())?;
    fs::write(storage.manifest_path(), b"{")?;
    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": false }),
        4,
    );
    let error = error_of(&response);
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{error}");
    assert_eq!(
        fs::read(storage.manifest_path())?,
        b"{",
        "nothing was written"
    );
    assert!(!storage.snapshot_exists(), "nothing was built");
    Ok(())
}

/// A recorded cfg flag no request could make (a hand-edited record) is
/// refused through `macro_options_refusal`'s `RecordedCfgFlagInvalid` arm
/// (round 7 residual): `-32602` with the resolver's message, and nothing is
/// written. Replacing the flags with `cfg_flags`, or dropping the record
/// with `reset_macro_options`, are the accepted controls the message names.
#[test]
fn standalone_rebuild_index_refuses_a_recorded_invalid_cfg_flag_on_the_wire() -> Result<()> {
    let (_tmp, root) = workspace();
    let mut client = server_for(&root);
    build_index(&mut client, &root, 1);
    let storage = GraphStorage::new(&root);
    let mut manifest: Value = serde_json::from_slice(&fs::read(storage.manifest_path())?)?;
    manifest["macro_options"] = json!({ "cfg_flags": [" test"] });
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let before = index_bytes(&root);

    let response = call(
        &mut client,
        json!({ "path": path_text(&root), "force": true }),
        2,
    );
    let refused = sqry_core::graph::unified::build::macro_options::MacroOptionsError::RecordedCfgFlagInvalid {
        flag: " test".to_string(),
    };
    assert_envelope(
        &response,
        &invalid_argument(&format!("rebuild of {} refused: {refused}", root.display())),
    );
    assert_eq!(index_bytes(&root), before, "nothing was written");

    for (id, extra) in [
        (3, json!({ "cfg_flags": ["test"] })),
        (4, json!({ "reset_macro_options": true })),
    ] {
        let mut arguments = json!({ "path": path_text(&root), "force": true });
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let payload = unwrap_mcp_content(&call(&mut client, arguments, id))?;
        assert_eq!(payload["data"]["success"], true, "{extra}: {payload}");
    }
    Ok(())
}

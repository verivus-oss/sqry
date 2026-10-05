//! A `path` that names nothing, as every tool's client receives it (round 7,
//! surfaces round three, finding 4).
//!
//! The server resolves every tool's `path` through one check before any
//! tool runs. Before it, a relative `path` naming nothing, sent before the
//! session had resolved a workspace and with no client roots, reached the
//! tools, which answered it three ways: `semantic_search` with `-32603`
//! "Failed to canonicalize path" and no data, `list_files` with `-32603`
//! "subtree ... was not found", and `get_definition` with an answer that
//! ignored the path. Now every tool answers both an absolute and a relative
//! `path` naming nothing, on the session's first call and on a later one,
//! with `-32602` `validation_error` naming the path.
//!
//! The tools are read from the server's own `tools/list`, so a tool added
//! later is covered without editing this file; its arguments are built from
//! its input schema.
//!
//! A client whose own `roots/list` fails is not the caller's argument
//! refused: that stays an invalid request (`-32600`), as it was before
//! resolution refusals became invalid arguments.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use common::{McpTestClient, StderrMode};
use serde_json::{Map, Value, json};

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// An indexed Rust workspace, canonical, beside its guard.
fn indexed_workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"r7s3\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn alpha() { beta(); }\npub fn beta() {}\n",
    )
    .expect("lib.rs");
    common::ensure_graph_snapshot(&root).expect("the fixture index builds");
    (tmp, root)
}

/// A fresh server whose workspace without a `path` is `root` and which has
/// no client roots: the session has resolved nothing yet.
fn fresh_server(root: &Path) -> McpTestClient {
    let mut client = McpTestClient::new_with_env_and_stderr_mode(
        &[("SQRY_MCP_WORKSPACE_ROOT".to_string(), path_text(root))],
        StderrMode::Null,
    )
    .expect("spawn sqry-mcp");
    client.initialize().expect("initialize");
    client
}

/// A value of the shape `schema` describes, for an argument the tool
/// requires: the first enumerated value, the first non-null alternative,
/// a symbol name the fixture defines for a string, one element for an
/// array, and the required properties of an object. `$ref`s resolve
/// against `defs`.
fn value_for(schema: &Value, defs: &Map<String, Value>) -> Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().expect("a ref name");
        return value_for(&defs[name], defs);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values[0].clone();
    }
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(alternatives) = schema.get(key).and_then(Value::as_array) {
            let chosen = alternatives
                .iter()
                .find(|alternative| alternative.get("type") != Some(&json!("null")))
                .expect("a non-null alternative");
            return value_for(chosen, defs);
        }
    }
    let kind = match schema.get("type") {
        Some(Value::Array(kinds)) => kinds
            .iter()
            .find(|kind| *kind != "null")
            .and_then(Value::as_str)
            .unwrap_or("string"),
        Some(kind) => kind.as_str().expect("a type name"),
        None if schema.get("properties").is_some() => "object",
        None => "string",
    };
    match kind {
        "string" => json!("alpha"),
        "integer" | "number" => json!(
            schema
                .get("minimum")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1)
        ),
        "boolean" => json!(false),
        "array" => json!([value_for(schema.get("items").unwrap_or(&json!({})), defs)]),
        "object" => arguments_for(schema, defs),
        other => panic!("no value for schema type {other}: {schema}"),
    }
}

/// The required arguments of a tool whose input schema is `schema`.
fn arguments_for(schema: &Value, defs: &Map<String, Value>) -> Value {
    let mut arguments = Map::new();
    let properties = schema.get("properties").and_then(Value::as_object);
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let property = &properties.expect("properties")[name];
        arguments.insert(name.to_string(), value_for(property, defs));
    }
    Value::Object(arguments)
}

/// The argument sets to try for a tool whose input schema is `schema`, in
/// order: its required arguments, then those plus one optional argument at
/// a time. A tool whose own check needs one of several optional arguments
/// (`export_graph` needs one of `file_path`, `symbol_name` or `symbols`),
/// which a schema cannot say, accepts one of the later sets.
fn argument_sets_for(schema: &Value, defs: &Map<String, Value>) -> Vec<Value> {
    let required = arguments_for(schema, defs);
    let mut sets = vec![required.clone()];
    for (name, property) in schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if name == "path" || required.get(name).is_some() {
            continue;
        }
        let mut set = required.clone();
        set[name.as_str()] = value_for(property, defs);
        sets.push(set);
    }
    sets
}

/// Every tool the server lists whose schema takes `path`, with arguments it
/// accepts beside a `path` naming the workspace, and the number of tools
/// listed: the first of [`argument_sets_for`] the tool's own check does not
/// refuse as invalid (`-32602`). A tool that refuses every set fails the
/// test, naming the tool, so a new tool whose arguments cannot be built
/// from its schema is seen, not skipped.
fn tools_taking_a_path(root: &Path) -> (Vec<(String, Value)>, usize) {
    let mut client = fresh_server(root);
    let listed = client.call("tools/list", json!({}), 1).expect("tools/list");
    let tools = listed["result"]["tools"].as_array().expect("tools").clone();
    let mut taking_a_path = Vec::new();
    let mut id = 2;
    for tool in &tools {
        let name = tool["name"].as_str().expect("a name").to_string();
        let schema = &tool["inputSchema"];
        if schema["properties"].get("path").is_none() {
            continue;
        }
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut accepted = None;
        for arguments in argument_sets_for(schema, &defs) {
            id += 1;
            let response = call(&mut client, &name, &arguments, &path_text(root), id);
            if response["error"]["code"] != -32602 {
                accepted = Some(arguments);
                break;
            }
        }
        let arguments = accepted.unwrap_or_else(|| {
            panic!("{name}: no arguments built from its schema are accepted beside a valid path")
        });
        taking_a_path.push((name, arguments));
    }
    (taking_a_path, tools.len())
}

fn call(client: &mut McpTestClient, tool: &str, arguments: &Value, path: &str, id: i64) -> Value {
    let mut arguments = arguments.clone();
    arguments["path"] = json!(path);
    client
        .call(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            id,
        )
        .expect("a response")
}

/// The refusal of a `path` naming nothing: `-32602` `validation_error`,
/// the tool's name, and the path as the request gave it.
fn assert_refused(tool: &str, response: &Value, path: &str, when: &str) {
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("{tool} ({when}, {path}) must refuse: {response}"));
    assert_eq!(error["code"], -32602, "{tool} ({when}): {error}");
    assert_eq!(
        error["data"]["kind"], "validation_error",
        "{tool} ({when}): {error}"
    );
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with(&format!(
            "invalid argument: {tool}: cannot resolve the workspace for this request: "
        )) && message.contains(path),
        "{tool} ({when}, {path}): {message}"
    );
}

/// A relative `path` read against the workspace the server resolved
/// without it is refused naming that workspace as well as the path.
fn assert_names_the_workspace(tool: &str, response: &Value, path: &str, root: &Path) {
    let message = response["error"]["message"].as_str().expect("a message");
    let named = format!(
        "`path` {path:?} names nothing under the workspace {}",
        root.display()
    );
    assert!(message.contains(&named), "{tool} ({path}): {message}");
}

/// Whether `response` is the refusal of its `path`, the control's
/// forbidden answer.
fn is_a_path_refusal(response: &Value) -> bool {
    response["error"]["message"]
        .as_str()
        .is_some_and(|message| {
            message.contains("cannot resolve the workspace")
                || message.contains("names nothing")
                || message.contains("names no workspace")
        })
}

#[test]
fn every_tool_refuses_a_path_naming_nothing_as_an_invalid_argument() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    let (tools, listed) = tools_taking_a_path(&root);
    // Every listed tool takes `path` today; if one stops taking it, the
    // count of tools checked says so here rather than shrinking silently.
    assert_eq!(tools.len(), listed, "every listed tool takes `path`");
    assert!(listed >= 39, "the registry lists every tool: {listed}");
    let relative = "r7s3-no-such-dir/deeper";
    let absolute = path_text(&root.join("r7s3-no-such-abs"));
    for (tool, arguments) in &tools {
        println!("{tool} {arguments}");
        let mut client = fresh_server(&root);
        // The session's first calls: no client roots and nothing resolved
        // (a refused call resolves nothing either).
        let response = call(&mut client, tool, arguments, relative, 1);
        assert_refused(tool, &response, relative, "first call, relative");
        // The relative refusal names the workspace it read the path against,
        // so the caller sees where it looked.
        assert_names_the_workspace(tool, &response, relative, &root);
        let response = call(&mut client, tool, arguments, &absolute, 2);
        assert_refused(tool, &response, &absolute, "first call, absolute");
        // The control: the same arguments with a `path` that names the
        // workspace are not refused for their path (the tool may answer
        // anything else, a missing symbol included).
        let response = call(&mut client, tool, arguments, &path_text(&root), 3);
        assert!(
            !is_a_path_refusal(&response),
            "{tool}: a path naming the workspace is accepted: {response}"
        );
        // Later calls, after the session has resolved a workspace.
        let response = call(&mut client, tool, arguments, relative, 4);
        assert_refused(tool, &response, relative, "later call, relative");
        let response = call(&mut client, tool, arguments, &absolute, 5);
        assert_refused(tool, &response, &absolute, "later call, absolute");
    }
    Ok(())
}

/// A path argument with leading or trailing whitespace is refused by every
/// tool before it runs (`-32602` `validation_error`, naming the argument as
/// sent), relative and absolute, on the first call and after one that
/// resolved the workspace. Before, `" src "` passed the shared check, which
/// read it trimmed, and then named nothing in the tool, which read it as
/// sent (`-32603` "Failed to canonicalize path" in most tools). `file_path`
/// and the `expand_files` entries, which the shared check also reads, are
/// refused the same way in every tool that takes them. The unpadded control
/// is not refused for its path.
#[test]
fn every_tool_refuses_a_padded_path_argument_as_an_invalid_argument() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    let (tools, listed) = tools_taking_a_path(&root);
    assert_eq!(tools.len(), listed, "every listed tool takes `path`");
    let absolute = path_text(&root);
    let padded_paths = [
        " src ".to_string(),
        "src ".to_string(),
        "\tsrc\n".to_string(),
        "   ".to_string(),
        format!(" {absolute}"),
        format!("{absolute} "),
    ];
    let mut file_path_tools = 0;
    let mut expand_files_tools = 0;
    let mut client = fresh_server(&root);
    let listed_tools = client.call("tools/list", json!({}), 1)?["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    for (tool, arguments) in &tools {
        let properties = &listed_tools
            .iter()
            .find(|listed| listed["name"] == tool.as_str())
            .expect("a listed tool")["inputSchema"]["properties"];
        let mut client = fresh_server(&root);
        let mut id = 1;
        // A refused call resolves nothing, so each of these is a first call.
        for padded in &padded_paths {
            id += 1;
            let response = call(&mut client, tool, arguments, padded, id);
            assert_padded_refused(tool, &response, "`path`", padded, "first call");
        }
        // The control, which also resolves the session's workspace: the
        // unpadded relative path is not refused for its path (the tool may
        // answer anything else).
        id += 1;
        let response = call(&mut client, tool, arguments, "src", id);
        assert!(!is_a_path_refusal(&response), "{tool}: {response}");
        for padded in &padded_paths {
            id += 1;
            let response = call(&mut client, tool, arguments, padded, id);
            assert_padded_refused(tool, &response, "`path`", padded, "later call");
        }

        if properties.get("file_path").is_some() {
            file_path_tools += 1;
            let mut with_file = arguments.clone();
            with_file["file_path"] = json!(" src/lib.rs ");
            id += 1;
            let response = call(&mut client, tool, &with_file, &absolute, id);
            assert_padded_refused(tool, &response, "`file_path`", " src/lib.rs ", "file_path");
        }
        if properties.get("expand_files").is_some() {
            expand_files_tools += 1;
            let mut with_files = arguments.clone();
            with_files["expand_files"] = json!(["src/lib.rs", "src/lib.rs "]);
            id += 1;
            let response = call(&mut client, tool, &with_files, &absolute, id);
            assert_padded_refused(
                tool,
                &response,
                "`expand_files` entry",
                "src/lib.rs ",
                "expand_files",
            );
        }
    }
    // The tools that take the other two path arguments today; a change in
    // either count is seen here rather than shrinking the check silently.
    assert_eq!(file_path_tools, 8, "tools taking `file_path`");
    assert_eq!(expand_files_tools, 1, "tools taking `expand_files`");
    Ok(())
}

/// The refusal of a padded path argument: `-32602` `validation_error`
/// naming the argument (`argument` as the message labels it) and its text
/// as sent.
fn assert_padded_refused(tool: &str, response: &Value, argument: &str, padded: &str, when: &str) {
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("{tool} ({when}, {padded:?}) must refuse: {response}"));
    assert_eq!(error["code"], -32602, "{tool} ({when}): {error}");
    assert_eq!(
        error["data"]["kind"], "validation_error",
        "{tool} ({when}): {error}"
    );
    let message = error["message"].as_str().expect("a message");
    let named = format!("{argument} {padded:?} has leading or trailing whitespace");
    assert!(
        message.starts_with(&format!(
            "invalid argument: {tool}: cannot resolve the workspace for this request: "
        )) && message.contains(&named),
        "{tool} ({when}, {padded:?}): {message}"
    );
}

/// The relative control on a first call: a relative `path` that names a
/// directory under the workspace the server resolves without it is
/// accepted (the shared check reads it there, as the tools do).
#[test]
fn a_relative_path_naming_a_directory_is_accepted_on_the_first_call() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    let mut client = fresh_server(&root);
    let response = client.call(
        "tools/call",
        json!({ "name": "list_files", "arguments": { "path": "src" } }),
        1,
    )?;
    let payload = common::unwrap_mcp_content(&response)?;
    assert!(
        payload.to_string().contains("lib.rs"),
        "the subtree is listed: {payload}"
    );
    Ok(())
}

/// A client that advertises roots and whose `roots/list` fails: the
/// request is refused as an invalid request (`-32600`, no data, the whole
/// chain naming the failed `roots/list`), not as the caller's argument; the
/// same client with roots it can list is the accepted control.
#[test]
fn a_failed_client_roots_list_is_an_invalid_request() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    let mut failing =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    failing.fail_roots_list(json!({ "code": -32603, "message": "roots are unavailable" }));
    failing.initialize_with_capabilities(json!({ "roots": { "listChanged": true } }))?;
    let response = failing.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": {} }),
        1,
    )?;
    println!("{response}");
    let error = response.get("error").expect("refused");
    assert_eq!(error["code"], -32600, "{error}");
    assert!(error.get("data").is_none_or(Value::is_null), "{error}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with("Client advertised roots support, but `roots/list` failed")
            && message.contains("roots are unavailable"),
        "{message}"
    );

    let mut listing =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    listing.initialize_with_roots(std::slice::from_ref(&root))?;
    let response = listing.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": {} }),
        1,
    )?;
    common::unwrap_mcp_content(&response)?;
    Ok(())
}

/// A root the client lists that cannot be used refuses the request as an
/// invalid request (`-32600`, no data), not as the caller's argument: a URI
/// that does not parse, a `file:` URI naming a missing directory, and one
/// naming another host (no local path). One such root refuses the request
/// even beside a usable one. A root of another scheme and a root naming a
/// file are skipped, not refused: listed beside the workspace, they leave
/// it the session's only root, so a call with no `path` resolves it.
#[test]
fn an_unusable_client_root_is_an_invalid_request_and_a_non_directory_root_is_skipped() -> Result<()>
{
    let (_tmp, root) = indexed_workspace();
    let missing = root.join("r7s4-no-such-root");
    let missing_uri = url::Url::from_file_path(&missing)
        .expect("a file URI")
        .to_string();
    let root_uri = url::Url::from_file_path(&root)
        .expect("a file URI")
        .to_string();
    let file_uri = url::Url::from_file_path(root.join("Cargo.toml"))
        .expect("a file URI")
        .to_string();
    let cases: [(&[&str], &str); 4] = [
        (&["not a uri"], "Invalid MCP root URI: not a uri"),
        (&[&missing_uri], "Failed to canonicalize MCP root"),
        (
            &["file://another-host/srv/x"],
            "MCP root URI is not a valid file path: file://another-host/srv/x",
        ),
        (
            &[&root_uri, &missing_uri],
            "Failed to canonicalize MCP root",
        ),
    ];
    for (uris, expected) in cases {
        let mut client =
            McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
        client.set_root_uris(uris);
        client.initialize_with_capabilities(json!({ "roots": { "listChanged": true } }))?;
        let response = client.call(
            "tools/call",
            json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
            1,
        )?;
        let error = response
            .get("error")
            .unwrap_or_else(|| panic!("{uris:?} must refuse: {response}"));
        assert_eq!(error["code"], -32600, "{uris:?}: {error}");
        assert!(error.get("data").is_none_or(Value::is_null), "{error}");
        let message = error["message"].as_str().expect("a message");
        assert!(message.contains(expected), "{uris:?}: {message}");
    }

    let mut skipping =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    skipping.set_root_uris(&["https://example.com/x", &root_uri, &file_uri]);
    skipping.initialize_with_capabilities(json!({ "roots": { "listChanged": true } }))?;
    let response = skipping.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": {} }),
        1,
    )?;
    let payload = common::unwrap_mcp_content(&response)?;
    assert_eq!(
        payload["workspace_path"],
        path_text(&root).replace('\\', "/"),
        "the workspace is the one root left: {payload}"
    );
    Ok(())
}

/// Round 8 (decision D-i8-22): a client that lists roots of which none is a
/// usable local directory bounds the session to nothing, so every request
/// is refused as the client's roots (`-32600`, naming the skipped roots),
/// whatever its `path`: an absolute path to an indexed workspace, a
/// relative one, none at all, and a `rebuild_index` of a fresh directory
/// (which indexes nothing). Before, the skipped roots left an empty list
/// that read as "no client roots", and each call ran with no bound: the
/// configured root answered, and the fresh directory was indexed. The cases
/// are only a root naming a file, only `https://` roots, and a mix of both.
/// The accepted control: a client that lists no root at all sets no bound,
/// and the absolute path and the call with no path answer.
#[test]
fn a_client_whose_every_listed_root_is_skipped_has_every_request_refused() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    let file_uri = url::Url::from_file_path(root.join("Cargo.toml"))
        .expect("a file URI")
        .to_string();
    let lib_uri = url::Url::from_file_path(root.join("src").join("lib.rs"))
        .expect("a file URI")
        .to_string();
    let fresh_tmp = tempfile::tempdir()?;
    let fresh = fresh_tmp.path().canonicalize()?;
    fs::write(fresh.join("lib.rs"), "pub fn fresh() {}\n")?;
    let calls = |fresh: &Path| {
        [
            json!({ "name": "get_index_status", "arguments": { "path": path_text(&root) } }),
            json!({ "name": "get_index_status", "arguments": { "path": "src" } }),
            json!({ "name": "get_index_status", "arguments": {} }),
            json!({ "name": "rebuild_index", "arguments": { "path": path_text(fresh) } }),
        ]
    };
    let cases: [&[&str]; 3] = [
        &[&file_uri],
        &["https://example.com/x", "https://example.com/y/z"],
        &["https://example.com/x", &lib_uri, &file_uri],
    ];
    for uris in cases {
        let mut client = McpTestClient::new_with_env_and_stderr_mode(
            &[("SQRY_MCP_WORKSPACE_ROOT".to_string(), path_text(&root))],
            StderrMode::Null,
        )?;
        client.set_root_uris(uris);
        client.initialize_with_capabilities(json!({ "roots": { "listChanged": true } }))?;
        for (id, call) in calls(&fresh).into_iter().enumerate() {
            let response = client.call("tools/call", call.clone(), id as i64 + 1)?;
            let error = response
                .get("error")
                .unwrap_or_else(|| panic!("{uris:?} {call}: must refuse: {response}"));
            assert_eq!(error["code"], -32600, "{uris:?} {call}: {error}");
            assert!(error.get("data").is_none_or(Value::is_null), "{error}");
            let message = error["message"].as_str().expect("a message");
            assert!(
                message.starts_with("The client listed roots, but none names a local directory (")
                    && uris.iter().all(|uri| message.contains(uri)),
                "{uris:?} {call}: {message}"
            );
        }
        assert!(
            !fresh.join(".sqry").exists(),
            "{uris:?}: nothing is indexed in the fresh directory"
        );
    }

    // The control: an empty roots list is no bound, so the absolute path
    // and the call with no path answer the indexed workspace.
    let mut client = McpTestClient::new_with_env_and_stderr_mode(
        &[("SQRY_MCP_WORKSPACE_ROOT".to_string(), path_text(&root))],
        StderrMode::Null,
    )?;
    client.set_root_uris(&[]);
    client.initialize_with_capabilities(json!({ "roots": { "listChanged": true } }))?;
    let [absolute, _, no_path, _] = calls(&fresh);
    for (id, call) in [absolute, no_path].into_iter().enumerate() {
        let response = client.call("tools/call", call.clone(), id as i64 + 1)?;
        let payload = common::unwrap_mcp_content(&response)
            .unwrap_or_else(|err| panic!("{call} answers with no roots listed: {err}"));
        assert_eq!(
            payload["workspace_path"],
            path_text(&root).replace('\\', "/"),
            "{call}: {payload}"
        );
    }
    Ok(())
}

/// A relative `path` that is a link to nothing names nothing, read on the
/// session's first call against the workspace the server resolves without
/// it: refused naming the path and that workspace. A link to a directory is
/// the accepted side (the check follows a link, as the tools do).
#[test]
fn a_relative_dangling_link_names_nothing_and_a_link_to_a_directory_is_accepted() -> Result<()> {
    let (_tmp, root) = indexed_workspace();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("r7s4-missing"), root.join("dangling"))?;
        std::os::unix::fs::symlink(root.join("src"), root.join("linksrc"))?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(root.join("r7s4-missing"), root.join("dangling"))?;
        std::os::windows::fs::symlink_dir(root.join("src"), root.join("linksrc"))?;
    }
    let mut client = fresh_server(&root);
    let response = call(&mut client, "list_files", &json!({}), "dangling", 1);
    assert_refused(
        "list_files",
        &response,
        "dangling",
        "first call, dangling link",
    );
    assert_names_the_workspace("list_files", &response, "dangling", &root);

    let mut client = fresh_server(&root);
    let response = call(&mut client, "list_files", &json!({}), "linksrc", 1);
    let payload = common::unwrap_mcp_content(&response)?;
    assert!(
        payload.to_string().contains("lib.rs"),
        "the linked subtree is listed: {payload}"
    );
    Ok(())
}

/// With client roots, a `path` that resolves outside every root is refused
/// before any tool runs (`-32602` `validation_error`, naming the path, where
/// it resolved and the roots), so nothing is indexed or read there: `..`
/// above the root, an absolute path beside it, and a link inside the root
/// that leads out of it. Before, `get_definition` with `..` indexed the
/// root's parent and answered a symbol defined there. The accepted side: the
/// root itself, by absolute path and with no path, answers its own symbol,
/// and a directory inside it is not refused.
#[test]
fn a_path_resolving_outside_every_client_root_is_refused_and_indexes_nothing() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let parent = tmp.path().canonicalize()?;
    let root = parent.join("ws");
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"r7s4\"\nversion = \"0.1.0\"\n",
    )?;
    fs::write(root.join("src").join("lib.rs"), "pub fn alpha() {}\n")?;
    common::ensure_graph_snapshot(&root).expect("the root's index builds");
    fs::write(parent.join("secret.rs"), "pub fn parent_secret() {}\n")?;
    let beside = parent.join("beside");
    fs::create_dir_all(&beside)?;
    fs::write(beside.join("lib.rs"), "pub fn beside_secret() {}\n")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&beside, root.join("out"))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&beside, root.join("out"))?;

    let cases = [
        ("parent_secret", "..".to_string(), path_text(&parent)),
        ("beside_secret", path_text(&beside), path_text(&beside)),
        ("beside_secret", "out".to_string(), path_text(&beside)),
    ];
    for (symbol, path, resolved) in &cases {
        let mut client =
            McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
        client.initialize_with_roots(std::slice::from_ref(&root))?;
        let response = client.call(
            "tools/call",
            json!({ "name": "get_definition", "arguments": { "symbol": symbol, "path": path } }),
            1,
        )?;
        assert_refused("get_definition", &response, path, "outside the client root");
        let message = response["error"]["message"].as_str().expect("a message");
        let named = format!(
            "`path` {path:?} resolves to {resolved}, outside every client root ({})",
            root.display()
        );
        assert!(message.contains(&named), "{path}: {message}");
    }
    assert!(
        !parent.join(".sqry").exists(),
        "nothing is indexed in the root's parent"
    );
    assert!(
        !beside.join(".sqry").exists(),
        "nothing is indexed beside the root"
    );

    let mut client =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    client.initialize_with_roots(std::slice::from_ref(&root))?;
    for (id, arguments) in [
        json!({ "symbol": "alpha", "path": path_text(&root) }),
        json!({ "symbol": "alpha" }),
    ]
    .into_iter()
    .enumerate()
    {
        let response = client.call(
            "tools/call",
            json!({ "name": "get_definition", "arguments": arguments }),
            10 + id as i64,
        )?;
        let payload = common::unwrap_mcp_content(&response)?;
        assert!(
            payload.to_string().contains("alpha"),
            "the root answers its own symbol: {payload}"
        );
    }
    // A directory inside the root, relative and absolute, is inside the
    // bound too: the check does not refuse it (the tool may answer anything
    // else; how a relative path is read is #905's).
    for (id, path) in [
        "src".to_string(),
        path_text(&root.join("src")),
        format!("{}/../ws/src", path_text(&root)),
    ]
    .into_iter()
    .enumerate()
    {
        let response = client.call(
            "tools/call",
            json!({ "name": "get_definition", "arguments": { "symbol": "alpha", "path": path } }),
            20 + id as i64,
        )?;
        assert!(
            !is_a_path_refusal(&response),
            "{path} is inside the client root: {response}"
        );
    }
    Ok(())
}

/// The documented semantics on a first call with no workspace to read a
/// relative `path` against (no client root, no earlier workspace, no
/// configured root, no index at or above the working directory): a
/// relative `path` naming a directory under the working directory is
/// refused, naming the path and saying what it is read against and that an
/// absolute path names a directory directly; that absolute path is
/// accepted. A relative path is never read against the working directory
/// itself.
#[test]
fn with_no_workspace_a_relative_path_is_refused_saying_why_and_its_absolute_form_is_accepted()
-> Result<()> {
    let tmp = tempfile::tempdir()?;
    let cwd = tmp.path().canonicalize()?;
    let ancestors_with_an_index: Vec<_> = cwd
        .ancestors()
        .filter(|dir| dir.join(".sqry").join("graph").exists())
        .collect();
    assert!(
        ancestors_with_an_index.is_empty(),
        "the test needs no index at or above its temporary directory: {ancestors_with_an_index:?}"
    );
    let proj = cwd.join("proj");
    fs::create_dir_all(proj.join("src"))?;
    fs::write(proj.join("src").join("lib.rs"), "pub fn alpha() {}\n")?;

    let mut client = McpTestClient::new_without_workspace_env_in_dir(&cwd, StderrMode::Null)?;
    client.initialize()?;
    let response = client.call(
        "tools/call",
        json!({ "name": "get_definition", "arguments": { "symbol": "alpha", "path": "proj" } }),
        1,
    )?;
    assert_refused(
        "get_definition",
        &response,
        "proj",
        "no workspace, relative",
    );
    let message = response["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains(
            "`path` \"proj\" names no workspace: a relative `path` is read against the workspace \
             the session resolves without it (a client root, the last workspace, the configured \
             root, or an index at or above the working directory), and there is none; an \
             absolute `path` names its directory directly"
        ),
        "{message}"
    );
    assert!(!proj.join(".sqry").exists(), "the refusal indexes nothing");

    let mut client = McpTestClient::new_without_workspace_env_in_dir(&cwd, StderrMode::Null)?;
    client.initialize()?;
    let response = client.call(
        "tools/call",
        json!({ "name": "get_definition", "arguments": { "symbol": "alpha", "path": path_text(&proj) } }),
        1,
    )?;
    let payload = common::unwrap_mcp_content(&response)?;
    assert!(
        payload.to_string().contains("alpha"),
        "the absolute form names the directory: {payload}"
    );
    Ok(())
}

/// When the server cannot resolve a workspace without a `path` at all (its
/// configured root is gone), a relative `path` it would have read there is
/// refused naming that path, so the caller sees which argument went
/// unresolved; the same call with no `path` is refused without naming one
/// (the other side).
#[test]
fn a_relative_path_with_no_workspace_to_read_it_against_is_named() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let gone = tmp.path().join("gone");
    let mut client = McpTestClient::new_with_env_and_stderr_mode(
        &[("SQRY_MCP_WORKSPACE_ROOT".to_string(), path_text(&gone))],
        StderrMode::Null,
    )?;
    client.initialize()?;
    let named = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": { "path": "r7s3-rel" } }),
        1,
    )?;
    assert_refused("get_index_status", &named, "r7s3-rel", "no workspace");
    assert!(
        named["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("`path` \"r7s3-rel\" names no workspace")),
        "{named}"
    );
    let unnamed = client.call(
        "tools/call",
        json!({ "name": "get_index_status", "arguments": {} }),
        2,
    )?;
    assert_eq!(unnamed["error"]["code"], -32602, "{unnamed}");
    assert!(
        unnamed["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.contains("`path`")),
        "{unnamed}"
    );
    Ok(())
}

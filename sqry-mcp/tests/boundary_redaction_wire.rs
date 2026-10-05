//! The errors the standalone server raises outside any tool, redacted at
//! their own boundary (S3, round 7; plants O22 and O23 of round three).
//!
//! `resources/read` and `prompts/get` never reach the tool wrapper, so only
//! their own boundary can redact what they send. Each test asks for a
//! resource or a prompt whose name is an absolute path: under the server's
//! default preset the path is gone from the error, and under the preset
//! `none` (the other side) it is there, so the absence is the redactor's
//! doing and not the error's wording.

mod common;

use std::path::{Path, PathBuf};

use common::{McpTestClient, StderrMode};
use serde_json::{Value, json};

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    (tmp, root)
}

/// A server with `root` as its workspace, under its default preset when
/// `preset` is `None`, or under the preset named.
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

/// The `error` of `method` with `params` from the default-preset server
/// and from the `none` server, in that order.
fn both_errors(root: &Path, method: &str, params: &Value) -> (Value, Value) {
    let mut errors = Vec::new();
    for preset in [None, Some("none")] {
        let mut client = server(root, preset);
        let response = client.call(method, params.clone(), 1).expect("a response");
        println!("{preset:?} {method}: {response}");
        errors.push(response.get("error").cloned().expect("refused"));
    }
    let plain = errors.pop().expect("none");
    let redacted = errors.pop().expect("default");
    (redacted, plain)
}

/// An unknown resource named by an absolute `file://` URI: the default
/// preset redacts the path out of `unknown resource: ...`, the preset
/// `none` keeps it, and both keep the code.
#[test]
fn an_unknown_resource_error_is_redacted_under_the_default_preset() {
    let (_tmp, root) = workspace();
    let secret = root.join("secret.md");
    let uri = format!("file://{}", path_text(&secret));
    let (redacted, plain) = both_errors(&root, "resources/read", &json!({ "uri": uri }));
    assert_eq!(redacted["code"], plain["code"], "{redacted} {plain}");
    let message = redacted["message"].as_str().expect("a message");
    assert!(
        message.starts_with("unknown resource: ") && !message.contains(&path_text(&root)),
        "the default preset redacts the path: {message}"
    );
    assert!(
        plain["message"]
            .as_str()
            .is_some_and(|message| message.contains(&path_text(&secret))),
        "the preset none keeps it: {plain}"
    );
}

/// An unknown prompt named by an absolute path: the default preset
/// redacts it out of the message, the list of prompts it carries is kept,
/// and the preset `none` keeps the path.
#[test]
fn an_unknown_prompt_error_is_redacted_under_the_default_preset() {
    let (_tmp, root) = workspace();
    let name = path_text(&root.join("secret-prompt"));
    let (redacted, plain) = both_errors(&root, "prompts/get", &json!({ "name": name }));
    assert_eq!(redacted["code"], plain["code"], "{redacted} {plain}");
    assert!(
        !redacted.to_string().contains(&path_text(&root)),
        "the default preset redacts the path: {redacted}"
    );
    assert_eq!(
        redacted["data"]["available_prompts"], plain["data"]["available_prompts"],
        "the prompt list is not a path and is kept"
    );
    assert!(
        plain["message"]
            .as_str()
            .is_some_and(|message| message.contains(&name)),
        "the preset none keeps it: {plain}"
    );
}

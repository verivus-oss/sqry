//! Round 8 (decision D-i8-20): a redaction preset name never turns
//! redaction off.
//!
//! `SqryServer::create_redactor` disabled redaction for any preset outside
//! the five exact lowercase names, and for a configuration the redactor
//! refused, and `McpConfig::load_or_default` copied `SQRY_REDACTION_PRESET`
//! verbatim with no check, so `sqry-mcp` started with `Strict` (or a typo,
//! or an over-long `SQRY_HASH_SALT`) answered every call unredacted. These
//! tests run the real binary: a miscased or padded name is that preset, and
//! an unknown name or a refused configuration stops the server at startup,
//! naming what it refused.

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use common::{McpTestClient, StderrMode};
use serde_json::json;

/// Start `sqry-mcp --no-daemon` with `envs` over a cleared redaction
/// environment and wait for it to exit (its stdin is closed at once, so a
/// server that starts also exits, when its transport closes). Returns
/// whether it exited successfully and its stderr.
fn start_and_wait(envs: &[(&str, &str)]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sqry-mcp"));
    command
        .arg("--no-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for key in [
        "SQRY_REDACTION_PRESET",
        "SQRY_HASH_SALT",
        "SQRY_PRESERVE_PATHS",
        "SQRY_HASH_FILENAMES",
    ] {
        command.env_remove(key);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn sqry-mcp");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("sqry-mcp with {envs:?} neither started nor exited within 60 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    (status.success(), stderr)
}

/// The environment a case starts the server with, and the texts its
/// stderr must carry.
type Case<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str]);

/// An unknown preset, padded or not, and a configuration the redactor
/// refuses each stop the server at startup with an error naming them; the
/// accepted control, a miscased known preset, starts.
#[test]
fn an_unknown_preset_or_a_refused_configuration_stops_the_server() {
    let long_salt = "s".repeat(300);
    let cases: [Case<'_>; 4] = [
        (
            &[("SQRY_REDACTION_PRESET", "bogus")],
            &[
                "SQRY_REDACTION_PRESET",
                "\"bogus\"",
                "none, minimal, relative, standard, strict",
            ],
        ),
        (
            &[("SQRY_REDACTION_PRESET", "")],
            &["SQRY_REDACTION_PRESET", "\"\""],
        ),
        (
            &[("SQRY_REDACTION_PRESET", " strictest ")],
            &["SQRY_REDACTION_PRESET", "\" strictest \""],
        ),
        (
            &[
                ("SQRY_REDACTION_PRESET", "minimal"),
                ("SQRY_HASH_SALT", long_salt.as_str()),
            ],
            &["refusing to serve unredacted", "Salt length 300"],
        ),
    ];
    for (envs, expected) in cases {
        let (succeeded, stderr) = start_and_wait(envs);
        assert!(!succeeded, "{envs:?} must stop the server: {stderr}");
        for text in expected {
            assert!(stderr.contains(text), "{envs:?}: {text} not in {stderr}");
        }
    }
    // The control: a padded, miscased known preset gets past startup to the
    // transport (which then closes, as its stdin is closed) and logs the
    // canonical preset it redacts under.
    let (_, stderr) = start_and_wait(&[("SQRY_REDACTION_PRESET", " Strict ")]);
    assert!(
        stderr.contains("Response redaction enabled")
            && stderr.contains("\"strict\"")
            && !stderr.contains("Invalid value"),
        "a miscased known preset starts: {stderr}"
    );
}

/// `Strict`, miscased, is the strict preset: the absolute workspace root in
/// a refusal is redacted (it was served as it stands, with redaction off).
#[test]
fn a_miscased_preset_redacts_as_that_preset() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().canonicalize()?;
    std::fs::write(root.join("lib.rs"), "pub fn plain() {}\n")?;
    let shown = root.to_string_lossy().into_owned();
    let mut client = McpTestClient::new_with_env_and_stderr_mode(
        &[
            ("SQRY_MCP_WORKSPACE_ROOT".to_string(), shown.clone()),
            ("SQRY_REDACTION_PRESET".to_string(), "Strict".to_string()),
        ],
        StderrMode::Null,
    )?;
    client.initialize()?;
    let response = client.call(
        "tools/call",
        json!({
            "name": "rebuild_index",
            "arguments": { "path": shown, "force": true, "expand_cache": "missing-cache" }
        }),
        1,
    )?;
    let error = response.get("error").expect("a refusal");
    assert_eq!(error["code"], -32602, "{error}");
    assert!(
        !error.to_string().contains(&shown),
        "the strict preset redacts the root: {error}"
    );
    Ok(())
}

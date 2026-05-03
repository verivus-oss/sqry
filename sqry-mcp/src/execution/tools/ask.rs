//! Natural language translation tool execution (P2-18).
//!
//! This module implements the `sqry_ask` MCP tool which translates
//! natural language queries into sqry commands using the sqry-nl crate.
//!
//! The MCP server performs translation only - command execution is
//! the responsibility of the MCP client.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use sqry_nl::{DisambiguationOption, TranslationResponse, Translator, TranslatorConfig};

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::execution::utils::duration_to_ms;
use crate::execution::{NlDisambiguationOption, NlTranslationData, ToolExecution};
use crate::tools::SqryAskParams;

/// Check if a command has a `--path` flag as an actual CLI argument.
///
/// This checks for `--path` appearing outside of quoted strings to avoid
/// false positives when the query text itself contains "--path".
fn has_path_flag_outside_quotes(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let path_pattern: Vec<char> = "--path".chars().collect();
    let mut state = QuoteScanState::default();

    for i in 0..chars.len() {
        let c = chars[i];

        if state.advance(c) {
            continue;
        }

        // Check for --path at this position (only outside quotes)
        if !state.in_quotes()
            && matches_path_flag_at(&chars, i, &path_pattern)
            && has_path_flag_boundaries(&chars, i, path_pattern.len())
        {
            return true;
        }
    }
    false
}

#[derive(Default)]
struct QuoteScanState {
    in_double_quotes: bool,
    in_single_quotes: bool,
    prev_was_escape: bool,
}

impl QuoteScanState {
    fn advance(&mut self, c: char) -> bool {
        // Handle escape sequences
        if self.prev_was_escape {
            self.prev_was_escape = false;
            return true;
        }
        if c == '\\' {
            self.prev_was_escape = true;
            return true;
        }

        // Track quote state
        if c == '"' && !self.in_single_quotes {
            self.in_double_quotes = !self.in_double_quotes;
            return true;
        }
        if c == '\'' && !self.in_double_quotes {
            self.in_single_quotes = !self.in_single_quotes;
            return true;
        }

        false
    }

    fn in_quotes(&self) -> bool {
        self.in_double_quotes || self.in_single_quotes
    }
}

fn matches_path_flag_at(chars: &[char], offset: usize, pattern: &[char]) -> bool {
    if offset + pattern.len() > chars.len() {
        return false;
    }

    chars[offset..offset + pattern.len()]
        .iter()
        .zip(pattern.iter())
        .all(|(a, b)| a == b)
}

fn has_path_flag_boundaries(chars: &[char], offset: usize, pattern_len: usize) -> bool {
    // Check word boundary before (start of string or whitespace)
    let before_ok = offset == 0 || chars[offset - 1].is_whitespace();
    // Check word boundary after (end of string, whitespace, or '=')
    let after_pos = offset + pattern_len;
    let after_ok =
        after_pos == chars.len() || chars[after_pos].is_whitespace() || chars[after_pos] == '=';

    before_ok && after_ok
}

/// Augment a sqry command with path scope if it differs from workspace root.
///
/// When the user specifies a path that's a subdirectory of the workspace,
/// this appends `--path "<path>"` to the command so execution is scoped
/// to that directory.
fn augment_command_with_path(command: &str, scoped_path: &Path, workspace_root: &Path) -> String {
    // Only augment if path differs from workspace root
    if scoped_path == workspace_root {
        return command.to_string();
    }

    // Get relative path from workspace root for cleaner commands
    let relative_path = scoped_path
        .strip_prefix(workspace_root)
        .unwrap_or(scoped_path);

    // Don't add path flag if a real --path CLI flag is already present
    // (not just --path appearing inside a quoted query string)
    if has_path_flag_outside_quotes(command) {
        return command.to_string();
    }

    // Append --path flag with quoted path (handles spaces in path)
    format!(
        "{} --path \"{}\"",
        command,
        crate::execution::symbol_utils::path_to_forward_slash(relative_path)
    )
}

/// Resolve workspace path from args.path parameter.
///
/// If path is "." (default), returns None to trigger discovery.
/// Otherwise returns Some(path) for explicit workspace resolution.
fn resolve_workspace_path(path: &str) -> Option<PathBuf> {
    if path == "." {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Execute the `sqry_ask` natural language translation tool.
///
/// Translates a natural language query into a sqry command using the
/// sqry-nl translation pipeline. Returns structured response data
/// suitable for MCP clients.
///
/// The path parameter is canonicalized relative to the workspace root
/// to ensure translations are scoped correctly.
pub fn execute_sqry_ask(args: &SqryAskParams) -> Result<ToolExecution<NlTranslationData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root();

    // Canonicalize the path relative to workspace (validates it's within workspace)
    let scoped_path = canonicalize_in_workspace(&args.path, workspace_root)?;

    tracing::debug!(
        query = %args.query,
        path = %args.path,
        scoped_path = %scoped_path.display(),
        workspace = %workspace_root.display(),
        "Executing sqry_ask tool (standalone path — building translator per-call)"
    );

    // Create translator with configuration scoped to the requested path.
    // Standalone path (non-daemon, non-LSP) builds a fresh translator
    // per call — there is no per-process cache here. The daemon and LSP
    // surfaces use `execute_sqry_ask_with_translator` to share a
    // long-lived `Arc<Translator>` (NL07).
    let translator = build_translator(&scoped_path, args)?;

    // Translate the query — `translate_shared` does not require &mut.
    let response = translator.translate_shared(&args.query);

    // Convert to MCP response, augmenting commands with path scope if needed
    let mut data = build_translation_data(response, &scoped_path, workspace_root);

    // Optionally execute the translated command
    if args.execute
        && let Some(cmd_str) = &data.command
    {
        tracing::debug!(command = %cmd_str, "Executing translated sqry command");

        // Split command into binary and args
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if !parts.is_empty() {
            let bin = parts[0];
            let cmd_args = &parts[1..];

            let output = Command::new(bin)
                .args(cmd_args)
                .current_dir(workspace_root)
                .output();

            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

                    if out.status.success() {
                        data.execution_output = Some(stdout);
                    } else {
                        data.execution_output = Some(format!("Error: {stderr}\n{stdout}"));
                    }
                }
                Err(e) => {
                    data.execution_output = Some(format!("Failed to execute command: {e}"));
                }
            }
        }
    }

    tracing::debug!(
        response_type = %data.response_type,
        "sqry_ask tool completed"
    );

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: false,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// NL07 daemon/LSP path: execute `sqry_ask` against a pre-built,
/// per-process [`sqry_nl::Translator`] held in an [`Arc`].
///
/// The daemon caches the translator on `LoadedWorkspace` (lazy
/// `OnceCell<Arc<Translator>>`) so the classifier pool's N model
/// sessions are loaded exactly once per workspace lifetime — not once
/// per `sqry_ask` call. The LSP holds an analogous cell on
/// `SessionManager`.
///
/// Behavioural parity with [`execute_sqry_ask`]:
/// - Same workspace canonicalisation (`canonicalize_in_workspace`).
/// - Same path-augmentation contract for the rendered command.
/// - Same response shape (Execute / Confirm / Disambiguate / Reject).
/// - Optional `args.execute` runs the translated command in
///   `workspace_root` exactly as the standalone path does.
///
/// # Errors
///
/// Returns the same set of errors as [`execute_sqry_ask`] for
/// workspace resolution / canonicalisation. Translator construction
/// errors do NOT surface here — those happened earlier when the
/// caller (daemon `mcp_host` / LSP session) populated the `OnceCell`.
///
/// # Concurrency
///
/// Safe to call from multiple threads concurrently against the SAME
/// `Arc<Translator>`. The classifier pool serialises individual
/// inference calls per slot but fans out across `N` slots for
/// parallel translates. Async callers MUST wrap this in
/// [`tokio::task::spawn_blocking`] because pool acquire is sync.
pub fn execute_sqry_ask_with_translator(
    translator: Arc<Translator>,
    args: &SqryAskParams,
) -> Result<ToolExecution<NlTranslationData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root();

    let scoped_path = canonicalize_in_workspace(&args.path, workspace_root)?;

    tracing::debug!(
        query = %args.query,
        path = %args.path,
        scoped_path = %scoped_path.display(),
        workspace = %workspace_root.display(),
        "Executing sqry_ask tool (daemon/LSP path — shared translator)"
    );

    // Translate using the caller-provided translator. The pool inside
    // the translator manages slot acquisition + release.
    let response = translator.translate_shared(&args.query);

    let mut data = build_translation_data(response, &scoped_path, workspace_root);

    // Optional execute side-effect — same contract as
    // `execute_sqry_ask`. Daemon/LSP callers should generally pass
    // `execute=false`; the agent decides whether to run.
    if args.execute
        && let Some(cmd_str) = &data.command
    {
        tracing::debug!(command = %cmd_str, "Executing translated sqry command (shared-translator path)");
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if !parts.is_empty() {
            let bin = parts[0];
            let cmd_args = &parts[1..];
            let output = Command::new(bin)
                .args(cmd_args)
                .current_dir(workspace_root)
                .output();
            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    if out.status.success() {
                        data.execution_output = Some(stdout);
                    } else {
                        data.execution_output = Some(format!("Error: {stderr}\n{stdout}"));
                    }
                }
                Err(e) => {
                    data.execution_output = Some(format!("Failed to execute command: {e}"));
                }
            }
        }
    }

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: false,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Build a `TranslatorConfig` scoped to a workspace path + caller args.
///
/// Public so the daemon `mcp_host` can mint a fresh translator inside
/// the `OnceCell` initialiser without duplicating the env-var/toggle
/// resolution logic.
#[must_use]
pub fn build_translator_config_for_path(
    scoped_path: &Path,
    args: &SqryAskParams,
) -> TranslatorConfig {
    let allow_unverified_model =
        args.allow_unverified_model || env_flag_truthy("SQRY_NL_ALLOW_UNVERIFIED_MODEL");
    let allow_model_download =
        args.allow_model_download || env_flag_truthy("SQRY_NL_ALLOW_DOWNLOAD");
    TranslatorConfig {
        working_directory: Some(crate::execution::symbol_utils::path_to_forward_slash(
            scoped_path,
        )),
        model_dir_override: args.model_dir.as_ref().map(PathBuf::from),
        allow_unverified_model,
        allow_model_download,
        ..TranslatorConfig::default()
    }
}

fn env_flag_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            v.eq_ignore_ascii_case("1")
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        }
        Err(_) => false,
    }
}

fn build_translator(scoped_path: &Path, args: &SqryAskParams) -> Result<Translator> {
    let config = build_translator_config_for_path(scoped_path, args);
    match Translator::new(config) {
        Ok(t) => Ok(t),
        // NL08: surface ONNX-Runtime-missing as the structured
        // `RpcError::onnx_runtime_missing` so the server-side
        // `execute_tool_with_timeout` can render the canonical
        // `{ code: "ONNX_RUNTIME_MISSING", message, retriable: false }`
        // envelope inside the MCP error payload via downcast. Other
        // failures fall through as plain anyhow (mapped to a generic
        // internal error by the server).
        Err(sqry_nl::NlError::OnnxRuntimeMissing { hint }) => Err(anyhow::Error::new(
            crate::error::RpcError::onnx_runtime_missing(hint),
        )),
        Err(e) => Err(anyhow::Error::new(e)),
    }
}

fn build_translation_data(
    response: TranslationResponse,
    scoped_path: &Path,
    workspace_root: &Path,
) -> NlTranslationData {
    match response {
        TranslationResponse::Execute {
            command,
            confidence,
            intent,
            ..
        } => build_execute_data(
            &command,
            confidence,
            intent.as_str(),
            scoped_path,
            workspace_root,
        ),
        TranslationResponse::Confirm {
            command,
            confidence,
            prompt,
        } => build_confirm_data(&command, confidence, &prompt, scoped_path, workspace_root),
        TranslationResponse::Disambiguate { options, prompt } => {
            build_disambiguate_data(options, prompt, scoped_path, workspace_root)
        }
        TranslationResponse::Reject {
            reason,
            suggestions,
        } => build_reject_data(reason, suggestions),
    }
}

fn build_execute_data(
    command: &str,
    confidence: f32,
    intent: &str,
    scoped_path: &Path,
    workspace_root: &Path,
) -> NlTranslationData {
    let scoped_command = augment_command_with_path(command, scoped_path, workspace_root);
    NlTranslationData {
        response_type: "execute".to_string(),
        command: Some(scoped_command),
        confidence: Some(confidence),
        intent: Some(intent.to_string()),
        prompt: None,
        reason: None,
        suggestions: Vec::new(),
        options: Vec::new(),
        execution_output: None,
    }
}

fn build_confirm_data(
    command: &str,
    confidence: f32,
    prompt: &str,
    scoped_path: &Path,
    workspace_root: &Path,
) -> NlTranslationData {
    let scoped_command = augment_command_with_path(command, scoped_path, workspace_root);
    // Update the prompt to include the scoped command
    let scoped_prompt = prompt.replace(command, &scoped_command);
    NlTranslationData {
        response_type: "confirm".to_string(),
        command: Some(scoped_command),
        confidence: Some(confidence),
        intent: None,
        prompt: Some(scoped_prompt),
        reason: None,
        suggestions: Vec::new(),
        options: Vec::new(),
        execution_output: None,
    }
}

fn build_disambiguate_data(
    options: Vec<DisambiguationOption>,
    prompt: String,
    scoped_path: &Path,
    workspace_root: &Path,
) -> NlTranslationData {
    // Low confidence - needs user selection
    let nl_options = build_disambiguation_options(options, scoped_path, workspace_root);
    NlTranslationData {
        response_type: "disambiguate".to_string(),
        command: None,
        confidence: None,
        intent: None,
        prompt: Some(prompt),
        reason: None,
        suggestions: Vec::new(),
        options: nl_options,
        execution_output: None,
    }
}

fn build_disambiguation_options(
    options: Vec<DisambiguationOption>,
    scoped_path: &Path,
    workspace_root: &Path,
) -> Vec<NlDisambiguationOption> {
    options
        .into_iter()
        .map(|opt| {
            let scoped_command =
                augment_command_with_path(&opt.command, scoped_path, workspace_root);
            NlDisambiguationOption {
                command: scoped_command,
                intent: opt.intent.as_str().to_string(),
                description: opt.description,
                confidence: opt.confidence,
            }
        })
        .collect()
}

fn build_reject_data(reason: String, suggestions: Vec<String>) -> NlTranslationData {
    // Cannot translate
    NlTranslationData {
        response_type: "reject".to_string(),
        command: None,
        confidence: None,
        intent: None,
        prompt: None,
        reason: Some(reason),
        suggestions,
        options: Vec::new(),
        execution_output: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ===== augment_command_with_path unit tests =====

    #[test]
    fn test_augment_command_same_as_workspace() {
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace");
        let command = "sqry query \"kind:function\"";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert_eq!(
            result, command,
            "Should not modify command when path == workspace"
        );
    }

    #[test]
    fn test_augment_command_with_subdirectory() {
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace/src/lib");
        let command = "sqry query \"kind:function\"";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert_eq!(
            result, "sqry query \"kind:function\" --path \"src/lib\"",
            "Should append relative --path"
        );
    }

    #[test]
    fn test_augment_command_already_has_path() {
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace/src");
        let command = "sqry query \"kind:function\" --path \"other\"";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert_eq!(result, command, "Should not add --path if already present");
    }

    #[test]
    fn test_augment_command_with_spaces_in_path() {
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace/my project/src");
        let command = "sqry query \"kind:function\"";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert!(
            result.contains("--path \"my project/src\""),
            "Path with spaces should be quoted: {result}"
        );
    }

    #[test]
    fn test_augment_command_path_in_query_text() {
        // Regression test: --path inside quoted query should NOT prevent path augmentation
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace/src/lib");
        // User is searching for code containing "--path"
        let command = "sqry query \"find --path flag usage\"";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert!(
            result.contains("--path \"src/lib\""),
            "Should append --path when it only appears inside query: {result}"
        );
    }

    #[test]
    fn test_augment_command_path_in_single_quotes() {
        // --path inside single quotes should also NOT prevent augmentation
        let workspace = PathBuf::from("/workspace");
        let scoped = PathBuf::from("/workspace/src");
        let command = "sqry query 'find --path'";

        let result = augment_command_with_path(command, &scoped, &workspace);
        assert!(
            result.contains("--path \"src\""),
            "Should append --path when it only appears inside single quotes: {result}"
        );
    }

    // ===== has_path_flag_outside_quotes unit tests =====

    #[test]
    fn test_has_path_flag_no_path() {
        assert!(!has_path_flag_outside_quotes(
            "sqry query \"kind:function\""
        ));
    }

    #[test]
    fn test_has_path_flag_real_flag() {
        assert!(has_path_flag_outside_quotes(
            "sqry query \"kind:function\" --path \"src\""
        ));
    }

    #[test]
    fn test_has_path_flag_with_equals() {
        assert!(has_path_flag_outside_quotes(
            "sqry query \"kind:function\" --path=\"src\""
        ));
    }

    #[test]
    fn test_has_path_flag_inside_double_quotes() {
        // --path inside double quotes is NOT a real flag
        assert!(!has_path_flag_outside_quotes(
            "sqry query \"find --path usage\""
        ));
    }

    #[test]
    fn test_has_path_flag_inside_single_quotes() {
        // --path inside single quotes is NOT a real flag
        assert!(!has_path_flag_outside_quotes("sqry query 'find --path'"));
    }

    #[test]
    fn test_has_path_flag_escaped_quote() {
        // Escaped quote shouldn't change quote state
        assert!(!has_path_flag_outside_quotes(
            "sqry query \"find \\\"--path\\\" usage\""
        ));
    }

    #[test]
    fn test_has_path_flag_partial_match() {
        // --pathlike should NOT match
        assert!(!has_path_flag_outside_quotes(
            "sqry query \"kind:function\" --pathlike \"src\""
        ));
    }

    // ===== execute_sqry_ask integration tests =====

    #[test]
    #[serial_test::serial(engine_cache)]
    #[serial_test::serial(workspace_env)]
    fn test_execute_sqry_ask_basic() {
        // Initialize engine cache before use
        crate::engine::init_engine_cache(std::num::NonZeroUsize::new(4).unwrap());

        // Skip if no .sqry workspace available (e.g., CI without index)
        if engine_for_workspace(None).is_err() {
            return;
        }

        let args = SqryAskParams {
            query: "find public functions".to_string(),
            path: ".".to_string(),
            execute: false,
            model_dir: None,
            allow_unverified_model: false,
            allow_model_download: false,
        };

        let result = execute_sqry_ask(&args);
        // Translation should not error (may return any response type)
        assert!(result.is_ok());

        let execution = result.unwrap();
        // Verify response type is one of the valid types
        let valid_types = ["execute", "confirm", "disambiguate", "reject"];
        assert!(
            valid_types.contains(&execution.data.response_type.as_str()),
            "Unexpected response type: {}",
            execution.data.response_type
        );
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    #[serial_test::serial(workspace_env)]
    fn test_execute_sqry_ask_response_types() {
        // Initialize engine cache before use
        crate::engine::init_engine_cache(std::num::NonZeroUsize::new(4).unwrap());

        // Skip if no .sqry workspace available (e.g., CI without index)
        if engine_for_workspace(None).is_err() {
            return;
        }

        // Test various query patterns to exercise different response types
        let test_cases = vec![
            ("find all public functions", "execute"),
            ("show me methods", "execute"),
            // These may produce different response types depending on confidence
            ("xyz123", "reject"), // Likely rejected due to no clear intent
        ];

        for (query, _expected_type) in test_cases {
            let args = SqryAskParams {
                query: query.to_string(),
                path: ".".to_string(),
                execute: false,
                model_dir: None,
                allow_unverified_model: false,
                allow_model_download: false,
            };

            let result = execute_sqry_ask(&args);
            // All queries should produce a valid response (even if rejected)
            assert!(
                result.is_ok(),
                "Query '{}' should not error: {:?}",
                query,
                result.err()
            );

            let execution = result.unwrap();
            let valid_types = ["execute", "confirm", "disambiguate", "reject"];
            assert!(
                valid_types.contains(&execution.data.response_type.as_str()),
                "Query '{}' produced invalid response type: {}",
                query,
                execution.data.response_type
            );
        }
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    #[serial_test::serial(workspace_env)]
    fn test_execute_sqry_ask_path_validation() {
        // Initialize engine cache before use
        crate::engine::init_engine_cache(std::num::NonZeroUsize::new(4).unwrap());

        // Skip if no .sqry workspace available (e.g., CI without index)
        if engine_for_workspace(None).is_err() {
            return;
        }

        // Valid path should work
        let args = SqryAskParams {
            query: "find functions".to_string(),
            path: ".".to_string(),
            execute: false,
            model_dir: None,
            allow_unverified_model: false,
            allow_model_download: false,
        };
        assert!(execute_sqry_ask(&args).is_ok());

        // Path outside workspace should fail (path traversal attempt)
        let args_bad = SqryAskParams {
            query: "find functions".to_string(),
            path: "/etc/passwd".to_string(),
            execute: false,
            model_dir: None,
            allow_unverified_model: false,
            allow_model_download: false,
        };
        // This should fail due to path canonicalization
        assert!(execute_sqry_ask(&args_bad).is_err());
    }
}

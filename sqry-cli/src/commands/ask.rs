//! Natural language query command handler.
//!
//! Translates natural language descriptions into sqry commands using
//! the sqry-nl translation pipeline.

use anyhow::Result;
use colored::Colorize;
use std::io::{self, Write};

use crate::args::Cli;
use crate::output::OutputStreams;

/// Return true when an environment variable is set to a truthy value.
///
/// Recognises `1`, `true`, `yes`, `on` (case-insensitive) — anything else is
/// treated as off. Unset variables return false.
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

/// Configuration for response handling behavior.
struct ResponseConfig<'a> {
    cli: &'a Cli,
    path: &'a str,
    auto_execute: bool,
    dry_run: bool,
}

/// Format and write an execute response in JSON format.
fn write_execute_json(
    streams: &mut OutputStreams,
    command: &str,
    confidence: f32,
    intent: &str,
    dry_run: bool,
    auto_execute: bool,
) -> Result<()> {
    let output = if dry_run {
        serde_json::json!({
            "type": "execute",
            "command": command,
            "confidence": confidence,
            "intent": intent,
            "dry_run": true
        })
    } else if auto_execute {
        serde_json::json!({
            "type": "execute",
            "command": command,
            "confidence": confidence,
            "intent": intent,
            "auto_execute": true
        })
    } else {
        serde_json::json!({
            "type": "confirm",
            "command": command,
            "confidence": confidence,
            "intent": intent
        })
    };
    streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    Ok(())
}

/// Format and write an execute response in text format.
fn write_execute_text(
    streams: &mut OutputStreams,
    command: &str,
    confidence: f32,
    intent: &str,
    dry_run: bool,
    auto_execute: bool,
) -> Result<()> {
    if dry_run {
        streams.write_result(&format!(
            "{} {}\n{}: {:.0}%\n{}: {}\n",
            "Command:".bold(),
            command.green(),
            "Confidence".dimmed(),
            confidence * 100.0,
            "Intent".dimmed(),
            intent
        ))?;
    } else if auto_execute {
        streams.write_result(&format!(
            "{} {} ({:.0}% confidence)\n",
            "Executing:".green().bold(),
            command,
            confidence * 100.0
        ))?;
    } else {
        streams.write_result(&format!(
            "{} {}\n{}: {:.0}%\n",
            "Generated command:".bold(),
            command.cyan(),
            "Confidence".dimmed(),
            confidence * 100.0
        ))?;
    }
    Ok(())
}

/// Handle the Execute response tier.
fn handle_execute_response(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    command: &str,
    confidence: f32,
    intent: &str,
) -> Result<()> {
    if config.cli.json {
        write_execute_json(
            streams,
            command,
            confidence,
            intent,
            config.dry_run,
            config.auto_execute,
        )?;
    } else {
        write_execute_text(
            streams,
            command,
            confidence,
            intent,
            config.dry_run,
            config.auto_execute,
        )?;
    }

    if config.dry_run {
        return Ok(());
    }

    if config.auto_execute {
        execute_generated_command(command, config.path, config.cli)?;
    } else if !config.cli.json {
        // Interactive confirmation in text mode
        if prompt_confirmation("Execute this command?")? {
            execute_generated_command(command, config.path, config.cli)?;
        } else {
            streams.write_diagnostic("Cancelled.\n")?;
        }
    }

    Ok(())
}

/// Format and write a confirm response in JSON format.
fn write_confirm_json(
    streams: &mut OutputStreams,
    command: &str,
    confidence: f32,
    prompt: &str,
    dry_run: bool,
    auto_execute: bool,
) -> Result<()> {
    let output = serde_json::json!({
        "type": "confirm",
        "command": command,
        "confidence": confidence,
        "prompt": prompt,
        "dry_run": dry_run,
        "auto_execute": auto_execute
    });
    streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    Ok(())
}

/// Format and write a confirm response in text format.
fn write_confirm_text(
    streams: &mut OutputStreams,
    command: &str,
    confidence: f32,
    prompt: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        streams.write_result(&format!(
            "{} {}\n{}: {:.0}%\n{}\n",
            "Command:".bold(),
            command.yellow(),
            "Confidence".dimmed(),
            confidence * 100.0,
            "(Medium confidence - would require confirmation)".dimmed()
        ))?;
    } else {
        streams.write_result(&format!(
            "{}\n{} {}\n",
            prompt.yellow(),
            "Command:".bold(),
            command.cyan()
        ))?;
    }
    Ok(())
}

/// Handle the Confirm response tier.
fn handle_confirm_response(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    command: &str,
    confidence: f32,
    prompt: &str,
) -> Result<()> {
    if config.cli.json {
        write_confirm_json(
            streams,
            command,
            confidence,
            prompt,
            config.dry_run,
            config.auto_execute,
        )?;
    } else {
        write_confirm_text(streams, command, confidence, prompt, config.dry_run)?;
    }

    if config.dry_run {
        return Ok(());
    }

    // Execute if auto_execute or user confirms
    let should_execute = if config.cli.json {
        config.auto_execute
    } else {
        config.auto_execute || prompt_confirmation("")?
    };

    if should_execute {
        execute_generated_command(command, config.path, config.cli)?;
    } else if !config.cli.json {
        streams.write_diagnostic("Cancelled.\n")?;
    }

    Ok(())
}

/// Handle the Disambiguate response tier.
fn handle_disambiguate_response(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    options: &[sqry_nl::DisambiguationOption],
    prompt: &str,
) -> Result<()> {
    let best_option = select_best_disambiguation(options);

    if config.cli.json {
        handle_disambiguate_json(streams, config, options, prompt, best_option)?;
    } else {
        handle_disambiguate_text(streams, config, options, prompt, best_option)?;
    }

    Ok(())
}

fn select_best_disambiguation(
    options: &[sqry_nl::DisambiguationOption],
) -> Option<&sqry_nl::DisambiguationOption> {
    options.iter().max_by(|a, b| {
        a.confidence
            .partial_cmp(&b.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn handle_disambiguate_json(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    options: &[sqry_nl::DisambiguationOption],
    prompt: &str,
    best_option: Option<&sqry_nl::DisambiguationOption>,
) -> Result<()> {
    let output = serde_json::json!({
        "type": "disambiguate",
        "prompt": prompt,
        "options": options.iter().map(|opt| {
            serde_json::json!({
                "command": opt.command,
                "intent": opt.intent.as_str(),
                "description": opt.description,
                "confidence": opt.confidence
            })
        }).collect::<Vec<_>>(),
        "auto_execute": config.auto_execute,
        "dry_run": config.dry_run
    });
    streams.write_result(&serde_json::to_string_pretty(&output)?)?;

    if let Some(selected) = best_option.filter(|_| config.auto_execute && !config.dry_run) {
        execute_generated_command(&selected.command, config.path, config.cli)?;
    }

    Ok(())
}

fn handle_disambiguate_text(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    options: &[sqry_nl::DisambiguationOption],
    prompt: &str,
    best_option: Option<&sqry_nl::DisambiguationOption>,
) -> Result<()> {
    streams.write_result(&format!("{}\n\n", prompt.yellow()))?;

    for (i, opt) in options.iter().enumerate() {
        streams.write_result(&format!(
            "  {}. {} - {}\n     {}\n\n",
            i + 1,
            opt.description.bold(),
            format!("{:.0}%", opt.confidence * 100.0).dimmed(),
            opt.command.cyan()
        ))?;
    }

    if config.dry_run || options.is_empty() {
        return Ok(());
    }

    if config.auto_execute {
        if let Some(selected) = best_option {
            streams.write_result(&format!(
                "\n{} {}\n",
                "Auto-executing highest confidence:".green().bold(),
                selected.command
            ))?;
            execute_generated_command(&selected.command, config.path, config.cli)?;
        }
        return Ok(());
    }

    execute_disambiguation_choice(streams, config, options)
}

fn execute_disambiguation_choice(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    options: &[sqry_nl::DisambiguationOption],
) -> Result<()> {
    let choice = prompt_choice(options.len())?;
    if let Some(idx) = choice {
        let selected = &options[idx];
        streams.write_result(&format!(
            "\n{} {}\n",
            "Executing:".green().bold(),
            selected.command
        ))?;
        execute_generated_command(&selected.command, config.path, config.cli)?;
    } else {
        streams.write_diagnostic("Cancelled.\n")?;
    }
    Ok(())
}

/// Handle the Reject response tier.
/// Returns the error message to be used for bailing after streams are finished.
fn handle_reject_response(
    streams: &mut OutputStreams,
    config: &ResponseConfig,
    reason: &str,
    suggestions: &[String],
) -> Result<String> {
    if config.cli.json {
        let output = serde_json::json!({
            "type": "reject",
            "reason": reason,
            "suggestions": suggestions
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        streams.write_diagnostic(&format!(
            "{} {}\n",
            "Cannot translate:".red().bold(),
            reason
        ))?;

        if !suggestions.is_empty() {
            streams.write_diagnostic(&format!("\n{}:\n", "Suggestions".yellow()))?;
            for suggestion in suggestions {
                streams.write_diagnostic(&format!("  • {suggestion}\n"))?;
            }
        }
    }
    Ok(format!("Translation rejected: {reason}"))
}

/// Run the `sqry ask` natural language command.
///
/// Translates a natural language query into a sqry command and optionally
/// executes it based on confidence level and user preferences.
///
/// # Errors
/// Returns an error if translation fails, output cannot be written, or execution fails.
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn run_ask(
    cli: &Cli,
    query: &str,
    path: &str,
    auto_execute: bool,
    dry_run: bool,
    threshold: f32,
    model_dir_override: Option<&std::path::Path>,
    allow_unverified_model_flag: bool,
    allow_model_download_flag: bool,
) -> Result<()> {
    use sqry_nl::{TranslationResponse, Translator, TranslatorConfig};

    let mut streams = OutputStreams::with_pager(cli.pager_config());

    // Honour env-var overrides for the trust toggles (FR-14): a CLI flag
    // *or* the matching env var being set turns the option on.
    let allow_unverified_model =
        allow_unverified_model_flag || env_flag_truthy("SQRY_NL_ALLOW_UNVERIFIED_MODEL");
    let allow_model_download =
        allow_model_download_flag || env_flag_truthy("SQRY_NL_ALLOW_DOWNLOAD");

    // Create translator with configured thresholds and resolver inputs.
    let translator_config = TranslatorConfig {
        execute_threshold: threshold,
        confirm_threshold: threshold * 0.75, // Confirm threshold at 75% of execute
        model_dir_override: model_dir_override.map(std::path::Path::to_path_buf),
        allow_unverified_model,
        allow_model_download,
        ..Default::default()
    };

    // NL08: detect the OnnxRuntimeMissing variant before wrapping in
    // anyhow context so the CLI main loop can surface a multi-line
    // platform-specific install hint and exit with code 65.
    let mut translator = match Translator::new(translator_config) {
        Ok(t) => t,
        Err(sqry_nl::NlError::OnnxRuntimeMissing { hint }) => {
            return Err(crate::error::CliError::OnnxRuntimeMissing { hint }.into());
        }
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context("Failed to initialize natural language translator")
            );
        }
    };

    // Translate the query
    let response = translator.translate(query);

    // Create response handling configuration
    let config = ResponseConfig {
        cli,
        path,
        auto_execute,
        dry_run,
    };

    // Handle response based on tier using extracted handlers
    let reject_error = match response {
        TranslationResponse::Execute {
            command,
            confidence,
            intent,
            ..
        } => {
            handle_execute_response(&mut streams, &config, &command, confidence, intent.as_str())?;
            None
        }

        TranslationResponse::Confirm {
            command,
            confidence,
            prompt,
        } => {
            handle_confirm_response(&mut streams, &config, &command, confidence, &prompt)?;
            None
        }

        TranslationResponse::Disambiguate { options, prompt } => {
            handle_disambiguate_response(&mut streams, &config, &options, &prompt)?;
            None
        }

        TranslationResponse::Reject {
            reason,
            suggestions,
        } => {
            let error_msg = handle_reject_response(&mut streams, &config, &reason, &suggestions)?;
            Some(error_msg)
        }
    };

    streams.finish_checked()?;

    // Return error after streams are finished for reject case
    if let Some(error_msg) = reject_error {
        anyhow::bail!("{error_msg}");
    }

    Ok(())
}

/// Parsed command arguments from a generated sqry command.
#[derive(Debug, Default)]
struct ParsedCommandArgs {
    /// The primary argument (symbol name, pattern, etc.)
    primary: String,
    /// Language filter (e.g., "rust")
    language: Option<String>,
    /// Kind filter (e.g., "function")
    kind: Option<String>,
    /// Limit for results
    limit: Option<u32>,
    /// Path filter
    path_filter: Option<String>,
    /// Second symbol for trace-path commands
    secondary: Option<String>,
    /// Max depth for graph commands
    max_depth: Option<u32>,
}

/// Extract a flag value from a command string, properly handling quoted values.
///
/// For `--path "src/api services"`, this returns `Some("src/api services")`.
/// For `--limit 50`, this returns `Some("50")`.
fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    // Find the flag in the command
    let flag_pos = command.find(flag)?;
    let after_flag = &command[flag_pos + flag.len()..];

    // Skip whitespace after the flag
    let trimmed = after_flag.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    // Check if the value is quoted
    if let Some(stripped) = trimmed.strip_prefix('"') {
        // Find the closing quote
        if let Some(end) = stripped.find('"') {
            return Some(stripped[..end].to_string());
        }
        // No closing quote found, return everything
        return Some(stripped.to_string());
    }

    // Not quoted - return up to next whitespace
    let value = trimmed.split_whitespace().next()?;
    Some(value.to_string())
}

/// Parse a generated sqry command into structured arguments.
fn parse_generated_command(command: &str) -> Result<ParsedCommandArgs> {
    let mut args = ParsedCommandArgs::default();

    // Extract all quoted strings in order
    let mut quoted_strings = Vec::new();
    let mut in_quote = false;
    let mut current_quoted = String::new();

    for c in command.chars() {
        if c == '"' {
            if in_quote {
                quoted_strings.push(current_quoted.clone());
                current_quoted.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            current_quoted.push(c);
        }
    }

    // First quoted string is the primary argument
    if let Some(primary) = quoted_strings.first() {
        args.primary.clone_from(primary);
    }

    // Second quoted string (if present) is secondary (for trace-path)
    if let Some(secondary) = quoted_strings.get(1) {
        args.secondary = Some(secondary.clone());
    }

    // Extract path using the helper function that properly handles quoted values
    // This must be done before split_whitespace since paths can contain spaces
    args.path_filter = extract_flag_value(command, "--path");

    // Parse other flags using split_whitespace (they don't typically have spaces)
    let parts: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "--language" if i + 1 < parts.len() => {
                args.language = Some(parts[i + 1].to_string());
                i += 2;
            }
            "--kind" if i + 1 < parts.len() => {
                args.kind = Some(parts[i + 1].to_string());
                i += 2;
            }
            "--limit" if i + 1 < parts.len() => {
                args.limit = parts[i + 1].parse().ok();
                i += 2;
            }
            "--path" => {
                // Path already extracted above with proper quote handling
                // Skip the flag and its value
                i += 2;
            }
            "--max-depth" if i + 1 < parts.len() => {
                args.max_depth = parts[i + 1].parse().ok();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    if args.primary.is_empty() {
        anyhow::bail!("Could not extract primary argument from command: {command}");
    }

    Ok(args)
}

/// Build a query expression with embedded predicates from parsed arguments.
///
/// Note: The primary expression from NL assembler already contains predicates like
/// `kind:function` and `visibility:public`. We only add predicates here that aren't
/// already in the expression.
fn build_query_expression(args: &ParsedCommandArgs) -> String {
    let mut expr_parts = vec![args.primary.clone()];

    // Note: kind is now included in the primary expression from NL assembler (e.g., "kind:function spawn")
    // so we don't add it again here

    // Add language predicate if present and not already in expression
    if let Some(lang) = &args.language
        && !args.primary.contains("lang:")
        && !args.primary.contains("language:")
    {
        expr_parts.push(format!("language:{lang}"));
    }

    // Add path predicate if present - quote if contains spaces
    if let Some(path) = &args.path_filter
        && !args.primary.contains("path:")
    {
        if path.contains(' ') {
            // Quote path values with spaces and escape any embedded quotes
            let escaped = path.replace('"', "\\\"");
            expr_parts.push(format!("path:\"{escaped}\""));
        } else {
            expr_parts.push(format!("path:{path}"));
        }
    }

    // Note: limit is NOT a query predicate - it's passed to run_query as result_limit parameter

    expr_parts.join(" ")
}

/// Execute a generated sqry command.
fn execute_generated_command(command: &str, path: &str, cli: &Cli) -> Result<()> {
    // Parse the command to extract the subcommand and arguments
    let parts: Vec<&str> = command.split_whitespace().collect();

    if parts.is_empty() || parts[0] != "sqry" {
        anyhow::bail!("Invalid generated command: {command}");
    }

    if parts.len() < 2 {
        anyhow::bail!("Generated command missing subcommand: {command}");
    }

    let subcommand = parts[1];

    match subcommand {
        "query" => {
            // Parse all arguments including filters
            let parsed = parse_generated_command(command)?;
            // Build query expression with embedded predicates
            let query_expr = build_query_expression(&parsed);
            // Pass limit as result_limit parameter (not as query predicate)
            let result_limit = parsed.limit.map(|l| l as usize);
            super::run_query(
                cli,
                &query_expr,
                path,
                false,
                false,
                false,
                false,
                None,
                result_limit,
                &[],
            )?;
        }
        "search" => {
            let parsed = parse_generated_command(command)?;
            // For search, just use the primary pattern
            super::run_search(cli, &parsed.primary, path)?;
        }
        "graph" => {
            // Graph commands need more parsing
            if parts.len() < 3 {
                anyhow::bail!("Graph command missing operation: {command}");
            }
            // For now, print what would be executed
            eprintln!(
                "{}",
                format!("Graph commands not yet auto-executable: {command}").yellow()
            );
        }
        "index" => {
            if command.contains("--status") {
                super::run_index_status(cli, path, crate::args::MetricsFormat::Json)?;
            } else {
                eprintln!(
                    "{}",
                    format!("Index build not auto-executable: {command}").yellow()
                );
            }
        }
        _ => {
            anyhow::bail!("Unsupported generated command: {subcommand}");
        }
    }

    Ok(())
}

/// Extract a quoted argument from a command string.
#[cfg(test)]
fn extract_quoted_arg(command: &str, _position: usize) -> Result<String> {
    // Find first quoted string
    if let Some(start) = command.find('"')
        && let Some(end) = command[start + 1..].find('"')
    {
        return Ok(command[start + 1..start + 1 + end].to_string());
    }
    // Fallback: try to get the argument after the subcommand
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() > 2 {
        // Remove quotes if present
        let arg = parts[2].trim_matches('"');
        return Ok(arg.to_string());
    }
    anyhow::bail!("Could not extract argument from: {command}")
}

/// Prompt user for yes/no confirmation.
fn prompt_confirmation(message: &str) -> Result<bool> {
    if message.is_empty() {
        eprint!("[y/N] ");
    } else {
        eprint!("{message} [y/N] ");
    }
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y") || input.trim().eq_ignore_ascii_case("yes"))
}

/// Prompt user to choose from options.
fn prompt_choice(max: usize) -> Result<Option<usize>> {
    eprint!("Enter choice (1-{max}) or 'c' to cancel: ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("c") || trimmed.is_empty() {
        return Ok(None);
    }

    match trimmed.parse::<usize>() {
        Ok(n) if n >= 1 && n <= max => Ok(Some(n - 1)),
        _ => {
            eprintln!("Invalid choice");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_quoted_arg() {
        let cmd = r#"sqry query "kind:function""#;
        let arg = extract_quoted_arg(cmd, 2).unwrap();
        assert_eq!(arg, "kind:function");
    }

    #[test]
    fn test_extract_quoted_arg_with_spaces() {
        let cmd = r#"sqry search "hello world""#;
        let arg = extract_quoted_arg(cmd, 2).unwrap();
        assert_eq!(arg, "hello world");
    }

    #[test]
    fn test_parse_generated_command_basic() {
        let cmd = r#"sqry query "authenticate" --limit 100"#;
        let parsed = parse_generated_command(cmd).unwrap();
        assert_eq!(parsed.primary, "authenticate");
        assert_eq!(parsed.limit, Some(100));
        assert!(parsed.language.is_none());
        assert!(parsed.kind.is_none());
    }

    #[test]
    fn test_parse_generated_command_with_all_flags() {
        let cmd = r#"sqry query "login" --language rust --kind function --limit 50"#;
        let parsed = parse_generated_command(cmd).unwrap();
        assert_eq!(parsed.primary, "login");
        assert_eq!(parsed.language.as_deref(), Some("rust"));
        assert_eq!(parsed.kind.as_deref(), Some("function"));
        assert_eq!(parsed.limit, Some(50));
    }

    #[test]
    fn test_parse_generated_command_trace_path() {
        let cmd = r#"sqry graph trace-path "source" "target" --max-depth 5"#;
        let parsed = parse_generated_command(cmd).unwrap();
        assert_eq!(parsed.primary, "source");
        assert_eq!(parsed.secondary.as_deref(), Some("target"));
        assert_eq!(parsed.max_depth, Some(5));
    }

    #[test]
    fn test_build_query_expression_basic() {
        let args = ParsedCommandArgs {
            primary: "authenticate".to_string(),
            ..Default::default()
        };
        let expr = build_query_expression(&args);
        assert_eq!(expr, "authenticate");
    }

    #[test]
    fn test_build_query_expression_with_predicates() {
        // Note: kind is now already in the primary expression from NL assembler
        // and limit is passed to run_query as result_limit, not in the expression
        let args = ParsedCommandArgs {
            primary: "kind:function login".to_string(), // kind already in primary from NL assembler
            language: Some("rust".to_string()),
            kind: Some("function".to_string()),
            limit: Some(50), // not added to expression, passed to run_query
            ..Default::default()
        };
        let expr = build_query_expression(&args);
        assert!(expr.contains("login"));
        assert!(expr.contains("kind:function"));
        assert!(expr.contains("language:rust"));
        // limit is NOT in expression - it's passed to run_query as result_limit parameter
        assert!(!expr.contains("limit:"));
    }

    #[test]
    fn test_build_query_expression_with_path() {
        let args = ParsedCommandArgs {
            primary: "test".to_string(),
            path_filter: Some("src/lib.rs".to_string()),
            ..Default::default()
        };
        let expr = build_query_expression(&args);
        assert!(expr.contains("path:src/lib.rs"));
    }

    #[test]
    fn test_build_query_expression_with_path_spaces() {
        let args = ParsedCommandArgs {
            primary: "login".to_string(),
            path_filter: Some("src/api services".to_string()),
            language: Some("rust".to_string()),
            ..Default::default()
        };
        let expr = build_query_expression(&args);
        // Path with spaces should be quoted to preserve as single predicate
        assert!(expr.contains(r#"path:"src/api services""#));
        assert!(expr.contains("language:rust"));
    }

    #[test]
    fn test_extract_flag_value_unquoted() {
        let cmd = r#"sqry query "test" --limit 50"#;
        assert_eq!(extract_flag_value(cmd, "--limit"), Some("50".to_string()));
    }

    #[test]
    fn test_extract_flag_value_quoted() {
        let cmd = r#"sqry query "test" --path "src/api services""#;
        assert_eq!(
            extract_flag_value(cmd, "--path"),
            Some("src/api services".to_string())
        );
    }

    #[test]
    fn test_extract_flag_value_not_present() {
        let cmd = r#"sqry query "test""#;
        assert_eq!(extract_flag_value(cmd, "--limit"), None);
    }

    #[test]
    fn test_parse_generated_command_with_path_spaces() {
        let cmd = r#"sqry query "login" --path "src/api services" --language rust"#;
        let parsed = parse_generated_command(cmd).unwrap();
        assert_eq!(parsed.primary, "login");
        assert_eq!(parsed.path_filter.as_deref(), Some("src/api services"));
        assert_eq!(parsed.language.as_deref(), Some("rust"));
    }
}

//! Natural language translation handler for LSP.
//!
//! Translates natural language queries to sqry commands using sqry-nl.

use std::path::PathBuf;

use anyhow::Result;
use sqry_nl::{TranslationResponse, TranslatorConfig};

use crate::protocol::{SqryAskDisambiguationOption, SqryAskParams, SqryAskResult};
use crate::session::SessionManager;

/// Recognise truthy environment variable values for the trust toggles.
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

/// Execute natural language translation.
///
/// Translates a natural language query into a sqry command.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the query is empty,
/// or the translator fails to initialize.
pub fn execute(session: &SessionManager, params: &SqryAskParams) -> Result<SqryAskResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let query = params.query.trim();

    if query.is_empty() {
        anyhow::bail!("query cannot be empty");
    }

    log::debug!(
        "Executing ask translation: query='{}', root={}",
        query,
        root.display()
    );

    // Honour env-var overrides for the trust toggles (FR-14): a request
    // parameter *or* the matching env var being set turns the option on.
    let allow_unverified_model =
        params.allow_unverified_model || env_flag_truthy("SQRY_NL_ALLOW_UNVERIFIED_MODEL");
    let allow_model_download =
        params.allow_model_download || env_flag_truthy("SQRY_NL_ALLOW_DOWNLOAD");

    // NL07: per-session lazy translator. The first ask call pays the
    // pool-init cost; subsequent calls cheap-clone the `Arc`. The
    // session config used for `get_or_init_translator` is captured at
    // first init — later calls keep the same effective config.
    // Per-call `model_dir_override` / trust toggles are reconciled by
    // making them stable across calls (the LSP server doesn't expose
    // changing model dirs mid-session).
    let config = TranslatorConfig {
        working_directory: Some(root.display().to_string()),
        model_dir_override: params.model_dir.as_ref().map(PathBuf::from),
        allow_unverified_model,
        allow_model_download,
        ..TranslatorConfig::default()
    };

    let translator = session.get_or_init_translator(config)?;

    // Translate the query — `translate_shared` does not require &mut.
    // The pool's `acquire()` is sync; tower_lsp dispatches LSP requests
    // on a tokio runtime, so callers MUST wrap this function in
    // `tokio::task::spawn_blocking`. The handler entrypoint at the
    // server layer is responsible for the wrap; this body itself is
    // plain sync.
    let response = translator.translate_shared(query);

    // Convert to LSP result
    let result = match response {
        TranslationResponse::Execute {
            command,
            confidence,
            intent,
            ..
        } => SqryAskResult {
            response_type: "execute".to_string(),
            command: Some(command),
            confidence: Some(confidence),
            intent: Some(intent.as_str().to_string()),
            prompt: None,
            reason: None,
            suggestions: Vec::new(),
            options: Vec::new(),
        },
        TranslationResponse::Confirm {
            command,
            confidence,
            prompt,
        } => SqryAskResult {
            response_type: "confirm".to_string(),
            command: Some(command),
            confidence: Some(confidence),
            intent: None,
            prompt: Some(prompt),
            reason: None,
            suggestions: Vec::new(),
            options: Vec::new(),
        },
        TranslationResponse::Disambiguate { options, prompt } => {
            let nl_options: Vec<SqryAskDisambiguationOption> = options
                .into_iter()
                .map(|opt| SqryAskDisambiguationOption {
                    command: opt.command,
                    intent: opt.intent.as_str().to_string(),
                    description: opt.description,
                    confidence: opt.confidence,
                })
                .collect();

            SqryAskResult {
                response_type: "disambiguate".to_string(),
                command: None,
                confidence: None,
                intent: None,
                prompt: Some(prompt),
                reason: None,
                suggestions: Vec::new(),
                options: nl_options,
            }
        }
        TranslationResponse::Reject {
            reason,
            suggestions,
        } => SqryAskResult {
            response_type: "reject".to_string(),
            command: None,
            confidence: None,
            intent: None,
            prompt: None,
            reason: Some(reason),
            suggestions,
            options: Vec::new(),
        },
    };

    Ok(result)
}

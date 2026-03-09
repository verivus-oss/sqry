//! Natural language translation handler for LSP.
//!
//! Translates natural language queries to sqry commands using sqry-nl.

use anyhow::{Context, Result};
use sqry_nl::{TranslationResponse, Translator, TranslatorConfig};

use crate::protocol::{SqryAskDisambiguationOption, SqryAskParams, SqryAskResult};
use crate::session::SessionManager;

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

    // Create translator scoped to the workspace
    let config = TranslatorConfig {
        working_directory: Some(root.display().to_string()),
        ..TranslatorConfig::default()
    };

    let mut translator = Translator::new(config).context("failed to create translator")?;

    // Translate the query
    let response = translator.translate(query);

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

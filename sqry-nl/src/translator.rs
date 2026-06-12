//! Main Translator API for natural language to sqry command translation.
//!
//! This module ties together all the components of the translation pipeline:
//! preprocess → extract → classify → assemble → validate → cache

use crate::assembler;
use crate::cache::{CacheConfig, CacheKey, CachedResult, TranslationCache};
use crate::error::{AssemblerError, NlResult};
use crate::extractor;
use crate::preprocess;
use crate::types::{
    DisambiguationOption, ExtractedEntities, Intent, TranslationResponse, ValidationStatus,
};
use crate::validator;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Confidence thresholds for response tiers.
const EXECUTE_THRESHOLD: f32 = 0.85;
const CONFIRM_THRESHOLD: f32 = 0.65;

/// Default cache capacity (number of entries).
const DEFAULT_CACHE_CAPACITY: usize = 128;

/// Default result limit for cache key generation.
const DEFAULT_RESULT_LIMIT: u32 = 100;

/// Configuration for the Translator.
#[derive(Debug, Clone)]
pub struct TranslatorConfig {
    /// Path to model directory (for classifier feature).
    /// Directory should contain: `intent_classifier.onnx`, `tokenizer.json`, and optionally
    /// `calibration.json` or `temperature.json` for confidence calibration
    pub model_dir: Option<String>,
    /// Context: current working directory for relative paths.
    pub working_directory: Option<String>,
    /// Custom confidence thresholds.
    pub execute_threshold: f32,
    pub confirm_threshold: f32,
    /// Cache configuration. Set to None to disable caching.
    pub cache_config: Option<CacheConfig>,
    /// Default result limit (affects cache key generation).
    pub default_limit: u32,
    /// Languages to restrict searches to (affects cache key generation).
    pub languages: Vec<String>,
    /// Optional override for the resolved model directory (NL02 resolver chain entry).
    ///
    /// When set, the resolver short-circuits the rest of the lookup chain and uses
    /// this path directly. Distinct from the legacy [`Self::model_dir`] field, which
    /// remains in place for backward compatibility.
    pub model_dir_override: Option<PathBuf>,
    /// Permit loading a model whose checksums cannot be verified.
    ///
    /// Defaults to `false`. NL04 enforces strict integrity by default; this flag is
    /// the operator escape hatch for development workflows.
    pub allow_unverified_model: bool,
    /// Permit fetching the model from the network when not present locally.
    ///
    /// Defaults to `false`. NL03 wires the gated downloader behind this flag.
    pub allow_model_download: bool,
    /// Optional cache directory for downloaded model artifacts.
    ///
    /// Defaults to `None`, in which case NL03 selects a platform-appropriate cache root.
    pub model_cache_dir: Option<PathBuf>,
    /// Optional override for the classifier worker pool size.
    ///
    /// Defaults to `None`. NL07 resolves this to the `SQRY_NL_CLASSIFIER_POOL_SIZE`
    /// environment variable, falling back to `2` workers.
    pub classifier_pool_size: Option<usize>,
}

impl Default for TranslatorConfig {
    fn default() -> Self {
        Self {
            model_dir: None,
            working_directory: None,
            execute_threshold: EXECUTE_THRESHOLD,
            confirm_threshold: CONFIRM_THRESHOLD,
            cache_config: Some(CacheConfig {
                capacity: DEFAULT_CACHE_CAPACITY,
                ..Default::default()
            }),
            default_limit: DEFAULT_RESULT_LIMIT,
            languages: Vec::new(),
            model_dir_override: None,
            allow_unverified_model: false,
            allow_model_download: false,
            model_cache_dir: None,
            classifier_pool_size: None,
        }
    }
}

/// The main Translator struct that provides the `translate()` API.
///
/// `Debug` is implemented manually because the inner classifier pool
/// (`ort::Session`-bearing) does not implement `Debug`. The manual
/// impl renders enough state for daemon `LoadedWorkspace` /
/// `WorkspaceManager` debug output without dumping model internals.
pub struct Translator {
    config: TranslatorConfig,
    /// Translation counter for stats
    translations: AtomicU64,
    /// Translation cache for repeated queries (Step 7)
    cache: Option<TranslationCache>,
    /// NL07 — bounded pool of `N` independently-loaded classifier
    /// sessions. `None` when the resolver chain produced no model
    /// directory (rule-based fallback only).
    #[cfg(feature = "classifier")]
    classifier_pool: Option<crate::classifier::ClassifierPool>,
}

impl Translator {
    /// Create a new Translator with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the classifier fails to load (when classifier feature is enabled).
    pub fn new(config: TranslatorConfig) -> NlResult<Self> {
        #[cfg(feature = "classifier")]
        let classifier_pool = {
            use crate::classifier::{
                BAKED_MANIFEST, ClassifierPool, RealDirs, ResolverLevel, TrustMode,
                ensure_model_in_cache, resolve_model_dir, resolve_pool_size,
            };
            use std::ffi::OsString;
            use std::path::{Path, PathBuf};

            // NL02 5-level resolver:
            // 1. CLI / programmatic override
            // 2. Legacy `TranslatorConfig::model_dir`
            // 3. SQRY_NL_MODEL_DIR env var
            // 4. XDG cache dir / sqry/models
            // 5. <exe-dir>/models
            let cli_override: Option<&Path> = config.model_dir_override.as_deref();
            let legacy_path: Option<&Path> = config.model_dir.as_deref().map(Path::new);
            let env_value: Option<OsString> = std::env::var_os("SQRY_NL_MODEL_DIR");
            let env_ref = env_value.as_deref();
            let exe = std::env::current_exe().ok();
            let exe_ref = exe.as_deref();

            let resolved =
                resolve_model_dir(cli_override, legacy_path, env_ref, &RealDirs, exe_ref);

            // NL03 gated downloader path. Only fires when:
            //   1. The resolver missed every level (no on-disk model anywhere
            //      sqry knows to look), AND
            //   2. The operator opted in via `allow_model_download = true`.
            // The download path lands the model under our managed cache
            // directory — structurally equivalent to a Level 4 (XDG) hit,
            // so the resulting trust mode is `Trusted`.
            let resolved: Option<(PathBuf, ResolverLevel)> = match resolved {
                Some(hit) => Some(hit),
                None if config.allow_model_download => {
                    let cache_dir: PathBuf = config
                        .model_cache_dir
                        .clone()
                        .or_else(|| dirs::cache_dir().map(|p| p.join("sqry/models")))
                        .ok_or_else(|| {
                            crate::error::NlError::Config(
                                "no platform cache_dir available for model download".to_string(),
                            )
                        })?;
                    let dir = ensure_model_in_cache(&cache_dir, &BAKED_MANIFEST, true)?;
                    Some((dir, ResolverLevel::XdgCache))
                }
                None => None,
            };

            match resolved {
                Some((model_dir, level)) => {
                    let trust_mode = TrustMode::from(level);
                    // FR-14: Custom mode means integrity is rooted in
                    // a user-supplied `manifest.json`. Emit a single
                    // loud warn at Translator init time so the operator
                    // is aware their model directory is the trust root.
                    if matches!(trust_mode, TrustMode::Custom) {
                        tracing::warn!(
                            target: "sqry_nl::classifier",
                            model_dir = %model_dir.display(),
                            resolver_level = ?level,
                            "Loading NL classifier under custom trust mode — \
                             integrity rooted in user-supplied manifest.json. \
                             For trusted defaults use the XDG cache or the \
                             binary-adjacent install location."
                        );
                    }

                    // NL07: build a pool of N independently-loaded
                    // classifier sessions. The closure below is invoked
                    // exactly N times by `ClassifierPool::new` — that
                    // is the load-counter invariant the
                    // `n_concurrent_translates_use_n_distinct_sessions`
                    // integration test asserts.
                    let pool_size = resolve_pool_size(config.classifier_pool_size);
                    tracing::info!(
                        target: "sqry_nl::classifier",
                        model_dir = %model_dir.display(),
                        pool_size,
                        "Initialising NL classifier pool"
                    );
                    let model_dir_for_loader = model_dir.clone();
                    let pool = ClassifierPool::new(pool_size, move || {
                        crate::classifier::IntentClassifier::load(
                            &model_dir_for_loader,
                            config.allow_unverified_model,
                            trust_mode,
                        )
                        .map_err(crate::error::NlError::from)
                    })?;
                    Some(pool)
                }
                None => None,
            }
        };

        // Initialize cache if configured
        let cache = config
            .cache_config
            .as_ref()
            .map(|cfg| TranslationCache::with_config(cfg.clone()));

        Ok(Self {
            config,
            translations: AtomicU64::new(0),
            cache,
            #[cfg(feature = "classifier")]
            classifier_pool,
        })
    }

    /// Create a Translator with default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    pub fn load_default() -> NlResult<Self> {
        Self::new(TranslatorConfig::default())
    }

    /// Translate a natural language query to a sqry command.
    ///
    /// # Arguments
    ///
    /// * `input` - The natural language query to translate
    ///
    /// # Returns
    ///
    /// A `TranslationResponse` indicating:
    /// - `Execute`: High confidence, safe to run automatically
    /// - `Confirm`: Medium confidence, ask user to confirm
    /// - `Disambiguate`: Low confidence, need user clarification
    /// - `Reject`: Cannot translate safely
    ///
    /// # Note
    ///
    /// NL07: this method now only needs `&self` because the classifier
    /// is held inside a [`crate::classifier::ClassifierPool`] (each
    /// pool slot wraps the underlying session in an
    /// `Arc<parking_lot::Mutex<_>>`). The `&mut self` signature is
    /// preserved for backwards compatibility with the pre-NL07 public
    /// API. Concurrent callers can use [`Self::translate_shared`] to
    /// drop the redundant `mut` requirement.
    pub fn translate(&mut self, input: &str) -> TranslationResponse {
        self.translate_shared(input)
    }

    /// Translate without requiring `&mut self`.
    ///
    /// Functionally identical to [`Self::translate`]; exposed because
    /// the underlying pool architecture supports concurrent shared
    /// access. Use this from multi-threaded callers (daemon, LSP) so
    /// they don't need to hold a `&mut Translator` across calls.
    pub fn translate_shared(&self, input: &str) -> TranslationResponse {
        self.translations.fetch_add(1, Ordering::Relaxed);
        self.translate_impl(input)
    }

    /// Internal translation implementation.
    fn translate_impl(&self, input: &str) -> TranslationResponse {
        let start_time = Instant::now();

        // Create cache key from input and context
        let cache_key = CacheKey::new(
            input,
            &self.config.languages,
            self.config.working_directory.clone(),
            self.config.default_limit,
        );

        // Check cache first
        if let Some(cached_response) = self.cached_response(&cache_key, start_time) {
            return cached_response;
        }

        // Step 1: Preprocess
        let preprocessed = match preprocess::preprocess_input(input) {
            Ok(p) => p,
            Err(e) => {
                return TranslationResponse::Reject {
                    reason: format!("Preprocessing failed: {e}"),
                    suggestions: vec!["Try simplifying your query".to_string()],
                };
            }
        };

        // Step 2: Extract entities
        let entities = extractor::extract_entities(&preprocessed.text);

        // Step 3: Classify intent
        let (intent, confidence) = self.classify_intent(&preprocessed.text, &entities);

        // Step 4: Assemble command
        let command = match assembler::assemble_command(&intent, &entities) {
            Ok(cmd) => cmd,
            Err(e) => return Self::handle_assembly_error(e, &entities),
        };

        // Step 5: Validate command
        self.handle_validation_result(
            command, confidence, intent, &entities, cache_key, start_time,
        )
    }

    fn cached_response(
        &self,
        cache_key: &CacheKey,
        start_time: Instant,
    ) -> Option<TranslationResponse> {
        let cache = self.cache.as_ref()?;
        let cached = cache.get(cache_key)?;
        Some(TranslationResponse::Execute {
            command: cached.command,
            confidence: cached.confidence,
            intent: cached.intent,
            cached: true,
            latency_ms: Self::elapsed_ms(start_time),
        })
    }

    fn handle_validation_result(
        &self,
        command: String,
        confidence: f32,
        intent: Intent,
        entities: &ExtractedEntities,
        cache_key: CacheKey,
        start_time: Instant,
    ) -> TranslationResponse {
        match validator::validate_command(&command) {
            ValidationStatus::Valid => {
                let latency_ms = Self::elapsed_ms(start_time);

                if confidence >= self.config.execute_threshold
                    && let Some(ref cache) = self.cache
                {
                    cache.put(
                        cache_key,
                        CachedResult {
                            command: command.clone(),
                            intent,
                            confidence,
                            created_at: Instant::now(),
                        },
                    );
                }

                self.create_response_with_latency(command, confidence, intent, entities, latency_ms)
            }
            ValidationStatus::RejectedMetachar => TranslationResponse::Reject {
                reason: "Command contains disallowed shell characters".to_string(),
                suggestions: vec![
                    "Avoid special characters like ;, |, &, $".to_string(),
                    "Use quoted strings for literal values".to_string(),
                ],
            },
            ValidationStatus::RejectedEnvVar => TranslationResponse::Reject {
                reason: "Command contains environment variable references".to_string(),
                suggestions: vec![
                    "Use literal paths instead of $HOME, ${VAR}".to_string(),
                    "Specify the full path explicitly".to_string(),
                ],
            },
            ValidationStatus::RejectedPathTraversal => TranslationResponse::Reject {
                reason: "Command contains path traversal patterns".to_string(),
                suggestions: vec![
                    "Use relative paths within the project".to_string(),
                    "Avoid .. in paths".to_string(),
                ],
            },
            ValidationStatus::RejectedTooLong => TranslationResponse::Reject {
                reason: "Generated command exceeds maximum length".to_string(),
                suggestions: vec![
                    "Try a simpler query".to_string(),
                    "Reduce the number of filters".to_string(),
                ],
            },
            ValidationStatus::RejectedWriteMode => TranslationResponse::Reject {
                reason: "Command attempts write operation".to_string(),
                suggestions: vec![
                    "NL translation only supports read operations".to_string(),
                    "Use CLI directly for write operations".to_string(),
                ],
            },
            ValidationStatus::RejectedUnknown => {
                let template_names = assembler::templates::TEMPLATES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ");
                let template_examples = ["query", "search", "trace-path"]
                    .into_iter()
                    .filter_map(assembler::templates::get_template)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
                    .join(" | ");

                TranslationResponse::Reject {
                    reason: "Command does not match any allowed template".to_string(),
                    suggestions: vec![
                        format!("Use supported command templates: {template_names}"),
                        format!("Examples: {template_examples}"),
                        "Try rephrasing your query".to_string(),
                    ],
                }
            }
        }
    }

    fn elapsed_ms(start_time: Instant) -> u64 {
        u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Classify the intent of the query.
    ///
    /// NL07: dispatches through the classifier pool. The pool guard
    /// holds one of `N` loaded sessions for the duration of this call;
    /// the slot is automatically returned on guard drop, including any
    /// panic during `classify`.
    #[allow(clippy::unused_self)] // Uses self when classifier feature is enabled.
    fn classify_intent(&self, text: &str, entities: &ExtractedEntities) -> (Intent, f32) {
        #[cfg(feature = "classifier")]
        if let Some(ref pool) = self.classifier_pool {
            // Acquire blocks the current thread until a slot is free.
            // Async callers MUST wrap this in `spawn_blocking` — the
            // pool is sync by design (no tokio dependency in sqry-nl).
            let guard = pool.acquire();
            let mut classifier = guard.classifier().lock();
            match classifier.classify(text) {
                Ok(result) => return (result.intent, result.confidence),
                Err(e) => {
                    // Log and fall back to rules. The guard's `Drop`
                    // returns the slot regardless of the classify
                    // outcome, so this fallback is panic-safe.
                    tracing::warn!(
                        target: "sqry_nl::classifier",
                        error = %e,
                        "Classifier failed, using rule-based fallback"
                    );
                }
            }
            // Explicit drop so the pool slot is back in the channel
            // before we run the rule-based fallback below.
            drop(classifier);
            drop(guard);
        }

        // Fallback: rule-based classification
        Self::classify_intent_rules(text, entities)
    }

    /// Rule-based intent classification fallback.
    fn classify_intent_rules(text: &str, entities: &ExtractedEntities) -> (Intent, f32) {
        let text_lower = text.to_lowercase();

        if let Some(intent) = Self::classify_graph_intent(&text_lower) {
            return intent;
        }

        if let Some(intent) = Self::classify_index_intent(&text_lower) {
            return intent;
        }

        if let Some(intent) = Self::classify_text_search_intent(&text_lower, text) {
            return intent;
        }

        if let Some(intent) = Self::classify_symbol_query_intent(&text_lower, entities) {
            return intent;
        }

        if Self::is_ambiguous(&text_lower) {
            return (Intent::Ambiguous, 0.3);
        }

        (Intent::SymbolQuery, 0.5)
    }

    fn classify_graph_intent(text_lower: &str) -> Option<(Intent, f32)> {
        if Self::matches_callers(text_lower) {
            return Some((Intent::FindCallers, 0.85));
        }

        if Self::matches_callees(text_lower) {
            return Some((Intent::FindCallees, 0.85));
        }

        if Self::matches_trace_path(text_lower) {
            return Some((Intent::TracePath, 0.8));
        }

        if Self::matches_visualize(text_lower) {
            return Some((Intent::Visualize, 0.8));
        }

        None
    }

    fn matches_callers(text_lower: &str) -> bool {
        text_lower.contains("callers")
            || text_lower.contains("who calls")
            || text_lower.contains("what calls")
            || text_lower.contains("who uses")
            || text_lower.contains("who depends")
            || text_lower.contains("find usages")
            || text_lower.contains("find all references")
            || text_lower.contains("where is") && text_lower.contains("used")
    }

    fn matches_callees(text_lower: &str) -> bool {
        text_lower.contains("callees")
            || text_lower.contains("what does") && text_lower.contains("call")
            || text_lower.contains("functions called by")
            || text_lower.contains("methods called by")
            || text_lower.contains("dependencies of")
            || text_lower.contains("outgoing calls")
            || text_lower.contains("what functions does")
            || text_lower.contains("what methods does")
    }

    fn matches_trace_path(text_lower: &str) -> bool {
        text_lower.contains("trace")
            || text_lower.contains("path from")
            || text_lower.contains("path to")
            || text_lower.contains("path between")
            || text_lower.contains("call chain")
            || text_lower.contains("call sequence")
            || (text_lower.contains("how does") && text_lower.contains("reach"))
            || (text_lower.contains("how does") && text_lower.contains("flow"))
    }

    fn matches_visualize(text_lower: &str) -> bool {
        text_lower.contains("visualize")
            || text_lower.contains("diagram")
            || text_lower.contains("draw")
            || text_lower.contains("mermaid")
            || text_lower.contains("dot graph")
            || (text_lower.contains("generate") && text_lower.contains("graph"))
            || (text_lower.contains("show") && text_lower.contains("visual"))
    }

    fn classify_index_intent(text_lower: &str) -> Option<(Intent, f32)> {
        if (text_lower.contains("index") && text_lower.contains("status"))
            || text_lower.starts_with("index status")
            || text_lower.contains("is index")
            || text_lower.contains("check index")
            || text_lower.contains("index info")
            || text_lower.contains("index stat")
            || text_lower.contains("indexed")
            || text_lower.contains("what files are indexed")
            || text_lower.contains("how many symbols")
            || text_lower.contains("when was index")
        {
            return Some((Intent::IndexStatus, 0.85));
        }

        None
    }

    fn classify_text_search_intent(text_lower: &str, text: &str) -> Option<(Intent, f32)> {
        let is_predicate_query = Self::is_predicate_query(text_lower);

        if text_lower.starts_with("grep")
            || text_lower.starts_with("search for")
            || text_lower.contains("grep for")
            || text_lower.contains("grep ")
            || text_lower.contains("look for")
            || (text_lower.contains("search") && !text_lower.contains("code search"))
            || text_lower.contains("todo")
            || text_lower.contains("fixme")
            || text_lower.contains("deprecated")
            || text_lower.contains("copyright")
            || text_lower.contains("hardcoded")
            || text.contains('!')
            || (!is_predicate_query && text_lower.contains("unsafe"))
            || text_lower.contains(" pub ")
            || text_lower.contains(" mut ")
            || (!is_predicate_query && text_lower.contains("async"))
            || text_lower.contains("unsafe blocks")
            || text_lower.contains("impl blocks")
            || text_lower.contains("import")
            || text_lower.contains("use statement")
            || text_lower.contains("require")
        {
            return Some((Intent::TextSearch, 0.8));
        }

        None
    }

    fn classify_symbol_query_intent(
        text_lower: &str,
        entities: &ExtractedEntities,
    ) -> Option<(Intent, f32)> {
        if text_lower.starts_with("find")
            || text_lower.starts_with("show")
            || text_lower.starts_with("list")
            || text_lower.starts_with("where is")
            || text_lower.starts_with("where are")
            || text_lower.contains("function")
            || text_lower.contains("method")
            || text_lower.contains("class")
            || text_lower.contains("struct")
            || text_lower.contains("enum")
            || text_lower.contains("trait")
            || text_lower.contains("interface")
            || text_lower.contains("module")
            || text_lower.contains("constant")
            || text_lower.contains("variable")
            || text_lower.contains("public")
            || text_lower.contains("private")
            || text_lower.contains("defined")
        {
            return Some((Intent::SymbolQuery, 0.8));
        }

        if entities.kind.is_some() {
            return Some((Intent::SymbolQuery, 0.85));
        }

        if !entities.symbols.is_empty() {
            return Some((Intent::SymbolQuery, 0.7));
        }

        if !entities.languages.is_empty() {
            return Some((Intent::SymbolQuery, 0.65));
        }

        None
    }

    fn is_predicate_query(text_lower: &str) -> bool {
        text_lower.contains("functions")
            || text_lower.contains("methods")
            || text_lower.contains("function")
            || text_lower.contains("method")
    }

    fn is_ambiguous(text_lower: &str) -> bool {
        text_lower.split_whitespace().count() <= 2
    }

    /// Create the appropriate response based on confidence level (with latency tracking).
    fn create_response_with_latency(
        &self,
        command: String,
        confidence: f32,
        intent: Intent,
        entities: &ExtractedEntities,
        latency_ms: u64,
    ) -> TranslationResponse {
        if confidence >= self.config.execute_threshold {
            TranslationResponse::Execute {
                command,
                confidence,
                intent,
                cached: false,
                latency_ms,
            }
        } else if confidence >= self.config.confirm_threshold {
            let prompt = format!(
                "I'll run: {}\nConfidence: {:.0}%. Proceed? [y/N]",
                command,
                confidence * 100.0
            );
            TranslationResponse::Confirm {
                command,
                confidence,
                prompt,
            }
        } else {
            // Disambiguate - present options to user
            let options = Self::generate_disambiguation_options(entities);
            TranslationResponse::Disambiguate {
                options,
                prompt: "I'm not sure what you mean. Did you want to:".to_string(),
            }
        }
    }

    /// Create the appropriate response based on confidence level.
    #[allow(dead_code)]
    fn create_response(
        &self,
        command: String,
        confidence: f32,
        intent: Intent,
        entities: &ExtractedEntities,
    ) -> TranslationResponse {
        self.create_response_with_latency(command, confidence, intent, entities, 0)
    }

    /// Generate disambiguation options when confidence is low.
    fn generate_disambiguation_options(entities: &ExtractedEntities) -> Vec<DisambiguationOption> {
        let mut options = Vec::new();

        if let Some(symbol) = entities.primary_symbol() {
            options.push(DisambiguationOption {
                command: format!("sqry query \"{symbol}\""),
                intent: Intent::SymbolQuery,
                description: format!("Search for symbol \"{symbol}\""),
                confidence: 0.5,
            });
            options.push(DisambiguationOption {
                command: format!("sqry graph direct-callers \"{symbol}\""),
                intent: Intent::FindCallers,
                description: format!("Find callers of \"{symbol}\""),
                confidence: 0.4,
            });
        } else {
            options.push(DisambiguationOption {
                command: "sqry query \"<symbol>\"".to_string(),
                intent: Intent::SymbolQuery,
                description: "Search for a specific symbol".to_string(),
                confidence: 0.3,
            });
        }

        options
    }

    /// Handle assembly errors with helpful suggestions.
    fn handle_assembly_error(
        error: AssemblerError,
        entities: &ExtractedEntities,
    ) -> TranslationResponse {
        match error {
            AssemblerError::MissingSymbol => {
                let suggestions = if entities.languages.is_empty() {
                    vec![
                        "Specify what symbol or pattern you're looking for".to_string(),
                        "Example: find \"authenticate\" in rust".to_string(),
                    ]
                } else {
                    vec![
                        format!(
                            "Try: find <symbol name> in {}",
                            entities.languages.join(", ")
                        ),
                        "Specify what you're looking for in quotes".to_string(),
                    ]
                };
                TranslationResponse::Reject {
                    reason: "Could not determine what to search for".to_string(),
                    suggestions,
                }
            }
            AssemblerError::AmbiguousIntent => TranslationResponse::Disambiguate {
                options: vec![
                    DisambiguationOption {
                        command: "sqry query \"<symbol>\"".to_string(),
                        intent: Intent::SymbolQuery,
                        description: "Search for symbols matching a pattern".to_string(),
                        confidence: 0.3,
                    },
                    DisambiguationOption {
                        command: "sqry graph direct-callers \"<symbol>\"".to_string(),
                        intent: Intent::FindCallers,
                        description: "Find callers of a function".to_string(),
                        confidence: 0.3,
                    },
                ],
                prompt: "Please clarify what you'd like to do:".to_string(),
            },
            AssemblerError::MissingTracePath => TranslationResponse::Reject {
                reason: "Trace path requires both source and target symbols".to_string(),
                suggestions: vec![
                    "Specify two symbols: trace path from X to Y".to_string(),
                    "Example: trace path from login to database".to_string(),
                ],
            },
            AssemblerError::CommandTooLong { .. } => TranslationResponse::Reject {
                reason: "Generated command is too long".to_string(),
                suggestions: vec![
                    "Try a simpler query".to_string(),
                    "Reduce the number of filters".to_string(),
                ],
            },
            AssemblerError::NoTemplate(intent_name) => TranslationResponse::Reject {
                reason: format!("No template available for intent: {intent_name}"),
                suggestions: vec![
                    "Try a different query type".to_string(),
                    "Supported queries: symbol search, callers, callees, trace path".to_string(),
                ],
            },
        }
    }

    /// Get translation count.
    #[must_use]
    pub fn translation_count(&self) -> u64 {
        self.translations.load(Ordering::Relaxed)
    }

    /// Get cache statistics.
    ///
    /// Returns `None` if caching is disabled.
    #[must_use]
    pub fn cache_stats(&self) -> Option<crate::cache::CacheStats> {
        self.cache
            .as_ref()
            .map(super::cache::TranslationCache::stats)
    }

    /// Get cache hit rate (0.0-1.0).
    ///
    /// Returns `None` if caching is disabled.
    #[must_use]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.cache
            .as_ref()
            .map(super::cache::TranslationCache::hit_rate)
    }

    /// Clear the translation cache.
    ///
    /// Does nothing if caching is disabled.
    pub fn clear_cache(&self) {
        if let Some(ref cache) = self.cache {
            cache.clear();
        }
    }
}

impl std::fmt::Debug for Translator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Translator");
        debug
            .field("config", &self.config)
            .field("translations", &self.translations.load(Ordering::Relaxed))
            .field("cache_enabled", &self.cache.is_some());
        #[cfg(feature = "classifier")]
        debug.field("classifier_pool", &self.classifier_pool);
        debug.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translator_creation() {
        let translator = Translator::load_default().unwrap();
        assert_eq!(translator.translation_count(), 0);
    }

    #[test]
    fn test_translate_simple_query() {
        let mut translator = Translator::load_default().unwrap();
        let response = translator.translate("find authentication functions");

        // Should not be a Reject response for missing symbol
        if let TranslationResponse::Reject { reason, .. } = &response {
            // May reject due to validation, but not missing symbol
            assert!(!reason.contains("Could not determine"));
        }
        assert_eq!(translator.translation_count(), 1);
    }

    #[test]
    fn test_translate_with_language() {
        let mut translator = Translator::load_default().unwrap();
        let response = translator.translate("find authentication in rust");

        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                assert!(command.contains("--language rust"));
            }
            _ => {} // Disambiguate or Reject is ok for rule-based fallback
        }
    }

    #[test]
    fn test_translate_callers() {
        let mut translator = Translator::load_default().unwrap();
        let response = translator.translate("who calls authenticate");

        match response {
            TranslationResponse::Execute { intent, .. } => {
                assert_eq!(intent, Intent::FindCallers);
            }
            TranslationResponse::Confirm { command, .. } => {
                // Confirm doesn't carry intent, but should have graph direct-callers command
                assert!(
                    command.contains("graph direct-callers") || command.contains("authenticate")
                );
            }
            _ => {}
        }
    }

    #[test]
    fn test_custom_thresholds() {
        let config = TranslatorConfig {
            execute_threshold: 0.99,
            confirm_threshold: 0.90,
            ..Default::default()
        };
        let mut translator = Translator::new(config).unwrap();

        // With high thresholds, most queries should need confirmation or disambiguation
        let response = translator.translate("find foo");
        assert!(!matches!(response, TranslationResponse::Execute { .. }));
    }

    #[test]
    fn test_kind_only_query() {
        let mut translator = Translator::load_default().unwrap();

        // Kind-only queries should work (e.g., "list all traits")
        let response = translator.translate("list all traits");
        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                // kind is now part of the query expression as a predicate
                assert!(command.contains("kind:trait"));
            }
            _ => panic!("Expected Execute or Confirm response"),
        }
    }

    #[test]
    fn test_snake_case_symbol() {
        let mut translator = Translator::load_default().unwrap();

        // Snake_case symbols should be extracted correctly
        let response = translator.translate("find user_id variable");
        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                assert!(command.contains("user_id"));
            }
            _ => panic!("Expected Execute or Confirm response"),
        }
    }
}

// Predicate translation regression tests
#[cfg(test)]
mod predicate_translation_tests {
    use super::*;

    #[test]
    fn test_async_functions_translation() {
        let config = TranslatorConfig::default();
        let mut translator = Translator::new(config).expect("Translator init failed");

        let response = translator.translate("find async functions");
        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                assert!(command.contains("async:true"));
            }
            _ => panic!("should execute or confirm"),
        }
    }

    #[test]
    fn test_unsafe_functions_translation() {
        let config = TranslatorConfig::default();
        let mut translator = Translator::new(config).expect("Translator init failed");

        let response = translator.translate("find unsafe functions");
        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                assert!(command.contains("unsafe:true"));
            }
            _ => panic!("should execute or confirm"),
        }
    }

    #[test]
    fn test_public_async_functions_translation() {
        let config = TranslatorConfig::default();
        let mut translator = Translator::new(config).expect("Translator init failed");

        let response = translator.translate("find public async functions");
        match response {
            TranslationResponse::Execute { command, .. }
            | TranslationResponse::Confirm { command, .. } => {
                assert!(command.contains("visibility:public"));
                assert!(command.contains("async:true"));
            }
            _ => panic!("should execute or confirm"),
        }
    }
}

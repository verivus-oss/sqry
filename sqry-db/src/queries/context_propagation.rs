//! `ContextPropagationQuery` — T3.7 (Cluster E).
//!
//! Detects Go call-sites that leak `context.Context` propagation: a
//! caller has a `ctx context.Context` parameter (or is shaped like an
//! `http.HandlerFunc`, or is a goroutine launch), the callee accepts
//! `context.Context`, but the call-site does not thread the available
//! context through. Three classifications are surfaced:
//!
//! - `BreakSite` — sync caller has ctx, callee accepts ctx, ctx is
//!   not threaded into the call.
//! - `UnthreadedGoroutine` — `go callee(...)` where callee accepts
//!   ctx and ctx is not threaded.
//! - `HttpHandlerLeak` — caller is
//!   `func(http.ResponseWriter, *http.Request)` and downstream callee
//!   accepts ctx but `r.Context()` is not threaded.
//!
//! `context.Background()` / `context.TODO()` at the call site is
//! treated as a leak (matching the `contextcheck` convention; see
//! 01_SPEC §6.2 AC-T3.7-5 and 02_DESIGN §4.2 / §4.2.a).
//!
//! References:
//! - 01_SPEC.md §3.2, §5.2, §6.2 (T3.7 acceptance criteria).
//! - 02_DESIGN.md §3.2, §4.2, §5.2 (algorithm + cache contract).
//! - 03_IMPLEMENTATION_PLAN.md §Cluster E.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sqry_core::graph::node::{Position, Span};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::file::id::FileId;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Scope filter for [`ContextPropagationQuery`].
///
/// `Global` walks every `Calls` edge in the snapshot. `File(FileId)`
/// restricts to edges whose caller function lives in that file —
/// resolved via `snapshot.get_node(caller).file`, NOT
/// `StoreEdgeRef::file` (CSR-backed edges drop file IDs after
/// compaction; see Codex iter-1 BLOCKER-2). The Phase-1 spec does
/// not include a `Module(NodeId)` variant — that is deferred to
/// Phase 2 (01_SPEC §9.4).
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextScope {
    /// All `Calls` edges in the snapshot.
    Global,
    /// Only `Calls` edges whose caller function lives in this file.
    File(FileId),
}

/// Mode filter for [`ContextPropagationQuery`].
///
/// `All` returns every classified leak; the three specific variants
/// restrict the result set to one classification.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextModeFilter {
    All,
    BreakSite,
    UnthreadedGoroutine,
    HttpHandlerLeak,
}

/// Concrete classification of a single context-leak finding.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextMode {
    /// Caller has `context.Context` parameter; callee accepts ctx;
    /// the call-site does not pass it.
    BreakSite,
    /// `go callee(...)` (or any `is_async == true` Calls edge) where
    /// callee accepts ctx without it being threaded.
    UnthreadedGoroutine,
    /// Caller signature matches `func(http.ResponseWriter, *http.Request)`
    /// (HTTP handler shape); downstream callee accepts ctx without
    /// `r.Context()` being threaded.
    HttpHandlerLeak,
}

/// Key for [`ContextPropagationQuery`]. Two queries are cache-equal iff
/// their `(scope, mode)` tuple is structurally identical.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextPropagationKey {
    pub scope: ContextScope,
    pub mode: ContextModeFilter,
}

/// A single context-propagation leak finding.
///
/// `call_span` is the byte-range location of the failing call-site
/// (from the `StoreEdgeRef::spans[0]` on the underlying `Calls` edge).
/// The Go plugin does not emit `NodeKind::CallSite` handles, so the
/// span — not a NodeId — is the user-facing call-site identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextLeak {
    /// Source location of the leaking call-site (from `Calls.spans[0]`).
    pub call_span: Span,
    /// The caller function (source of the `Calls` edge).
    pub caller: NodeId,
    /// The callee function (target of the `Calls` edge).
    pub callee: NodeId,
    /// Why this call is a leak.
    pub mode: ContextMode,
    /// The caller's ctx parameter NodeId, when one can be unambiguously
    /// identified. Per 01_SPEC §5.2.a this is "for IDE jump-to"; the
    /// Go plugin does not emit dedicated parameter-binding NodeIds at
    /// emit time, so this field is currently always `None` for Go.
    /// Future plugins or a Phase-2 enhancement may populate it.
    pub caller_ctx_param: Option<NodeId>,
}

/// Result of [`ContextPropagationQuery::execute`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextLeakSet {
    pub leaks: Vec<ContextLeak>,
}

/// `DerivedQuery` implementation surfacing context-propagation leaks.
///
/// - `QUERY_TYPE_ID = 0x0010` (first slot above the existing built-ins;
///   see 02_DESIGN §5.2).
/// - `TRACKS_EDGE_REVISION = true` — any `Calls` / `TypeOf` change
///   invalidates the cached set.
/// - `TRACKS_METADATA_REVISION = false` — reads no `NodeMetadata`.
/// - `PERSISTENT = true` — `ContextLeakSet` is fully postcard-encodable.
pub struct ContextPropagationQuery;

impl DerivedQuery for ContextPropagationQuery {
    type Key = ContextPropagationKey;
    type Value = Arc<ContextLeakSet>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CONTEXT_PROPAGATION;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &Self::Key, _db: &QueryDb, snapshot: &GraphSnapshot) -> Self::Value {
        let mut executor = ContextPropagationExecutor::new(snapshot);
        executor.run(key)
    }
}

// ---------------------------------------------------------------------------
// Internal executor
// ---------------------------------------------------------------------------

/// Compiled once per process — the `\bcontext\s*\.\s*(Background|TODO)\s*\(\s*\)`
/// pattern detects an explicit fresh-context literal at a call site
/// (AC-T3.7-5). The pattern is bounded and has no unbounded `.*`; ReDoS
/// surface is none.
fn ctx_background_or_todo_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\bcontext\s*\.\s*(Background|TODO)\s*\(\s*\)")
            .expect("static literal regex compiles")
    })
}

/// Stdlib import paths the query cares about (matched against the
/// quoted-string in `import "<path>"`).
const CONTEXT_IMPORT_PATH: &str = "context";
const HTTP_IMPORT_PATH: &str = "net/http";

/// The simple type names we recognise in stdlib `context` and
/// `net/http` packages. Per 01_SPEC §3.2 these are the spec-pinned
/// hard-coded names — user-defined wrappers are out of scope, so we
/// require BOTH a matching simple name AND a matching package
/// identifier from the file's import statements.
const CONTEXT_SIMPLE_NAME: &str = "Context";
const HTTP_RESPONSE_WRITER_SIMPLE_NAME: &str = "ResponseWriter";
const HTTP_REQUEST_SIMPLE_NAME: &str = "Request";

/// Split a qualified type name on its package separator (`::` or
/// `.`). Returns `(prefix, simple)`. When the name has no separator,
/// returns `("", full_name)` — the empty prefix is the canonical
/// "dot-import / bare" marker matched against `ImportAliases::accepts_*`.
fn split_qualified(name: &str) -> (&str, &str) {
    if let Some(idx) = name.find("::") {
        return (&name[..idx], &name[idx + 2..]);
    }
    if let Some(idx) = name.find('.') {
        return (&name[..idx], &name[idx + 1..]);
    }
    ("", name)
}

/// Per-file alias map for the stdlib import paths the query cares
/// about. Built once per `FileId` by scanning the file's import
/// section.
///
/// Each `HashSet<String>` holds the identifiers under which the
/// corresponding package can be referenced FROM THAT FILE:
///
/// - Default import (`import "context"`) → `"context"` is in the set.
/// - Aliased import (`import c "context"`) → `"c"` is in the set.
/// - Dot import (`import . "context"`) → the empty string `""` is in
///   the set, indicating "the package's exported symbols are usable
///   bare".
/// - Side-effect import (`import _ "context"`) → NOT recorded; the
///   `_` identifier cannot be used in source.
///
/// Multiple aliases per file (e.g. one file with `import "context"`
/// AND `import c "context"`) are all recorded.
///
/// Per Codex iter-2 review (`docs/reviews/go-error-context-buildtags/
/// 2026-05-17/codex-iter2-cluster-e.md`): without this map, the
/// strict qualified-name check applied in iter-1 produced false
/// negatives for aliased and dot-imported stdlib types.
#[derive(Debug, Clone, Default)]
struct ImportAliases {
    context_idents: std::collections::HashSet<String>,
    http_idents: std::collections::HashSet<String>,
}

impl ImportAliases {
    /// Returns `true` when `ident` (possibly the empty string for a
    /// dot-import) can refer to the stdlib `context` package in the
    /// surrounding file.
    fn accepts_context(&self, ident: &str) -> bool {
        self.context_idents.contains(ident)
    }
    /// Returns `true` when `ident` can refer to the stdlib `net/http`
    /// package in the surrounding file.
    fn accepts_http(&self, ident: &str) -> bool {
        self.http_idents.contains(ident)
    }
}

/// Parse a Go source file's leading `import` section and extract the
/// alias map for the two stdlib packages this query cares about.
///
/// Recognised import-spec shapes (cmd/go grammar — single-line or
/// inside an `import ( … )` block):
///
/// ```text
/// import "context"             // default ident = "context"
/// import c "context"           // alias = "c"
/// import . "context"           // dot-import; bare symbols usable
/// import _ "context"           // side-effect-only; not recorded
/// ```
///
/// Lines that are blank, comments, or anything else are ignored. The
/// scan stops at the first top-level construct (`func`, `type`, `var`,
/// `const`) outside of an open import block, which is sufficient
/// because Go requires all imports to precede top-level declarations.
fn extract_go_import_aliases(content: &[u8]) -> ImportAliases {
    let Ok(text) = std::str::from_utf8(content) else {
        return ImportAliases::default();
    };
    let mut aliases = ImportAliases::default();
    let mut in_block = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if !in_block {
            if line == "import (" || line.starts_with("import (") {
                in_block = true;
                continue;
            }
            if let Some(rest) = line.strip_prefix("import ") {
                record_go_import_spec(&mut aliases, rest.trim());
                continue;
            }
            // Once we see a top-level decl outside an import block,
            // imports are done.
            if line.starts_with("func ")
                || line.starts_with("type ")
                || line.starts_with("var ")
                || line.starts_with("const ")
            {
                break;
            }
        } else if line == ")" {
            in_block = false;
        } else if !line.is_empty() && !line.starts_with("//") {
            record_go_import_spec(&mut aliases, line);
        }
    }

    aliases
}

/// Parse a single import-spec body (everything after `import ` or
/// inside an `import (…)` block) and update `aliases` accordingly.
///
/// Handles real Go-style annotations: trailing line-comments
/// (`import c "context" // alias`), trailing block-comments
/// (`import "context" /* used by handlers */`), and inline block
/// comments between alias and path (`import c /* keep */ "context"`).
/// Codex iter-3 BLOCKER flagged that the earlier exact-quote-suffix
/// requirement dropped commented forms entirely.
///
/// Algorithm: locate the import path (a double-quoted string literal)
/// by scanning for the first two `"` characters in the spec. Everything
/// after the closing `"` is ignored (trailing comments, whitespace).
/// Everything before the opening `"` is the alias-token, with any
/// inline `/* … */` block comments stripped before identifier check.
fn record_go_import_spec(aliases: &mut ImportAliases, spec: &str) {
    let spec = spec.trim();
    let Some(open) = spec.find('"') else {
        return;
    };
    let Some(close_rel) = spec[open + 1..].find('"') else {
        return;
    };
    let close = open + 1 + close_rel;
    let path = &spec[open + 1..close];

    let alias_token = spec[..open].trim();
    let alias: Option<String> = if alias_token.is_empty() {
        None
    } else {
        let cleaned = strip_inline_block_comment(alias_token);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    let ident = match alias.as_deref() {
        Some("_") => return, // side-effect import; identifier unusable
        Some(".") => String::new(),
        Some(other) => other.to_string(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    };
    match path {
        CONTEXT_IMPORT_PATH => {
            aliases.context_idents.insert(ident);
        }
        HTTP_IMPORT_PATH => {
            aliases.http_idents.insert(ident);
        }
        _ => {}
    }
}

/// Strip a single `/* … */` block comment embedded inside the alias
/// portion (e.g. `c /* keep */`). Returns the input unchanged when no
/// block comment is found.
fn strip_inline_block_comment(s: &str) -> std::borrow::Cow<'_, str> {
    if let Some(start) = s.find("/*")
        && let Some(end_rel) = s[start + 2..].find("*/")
    {
        let end = start + 2 + end_rel + 2;
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..start]);
        out.push(' ');
        out.push_str(&s[end..]);
        return std::borrow::Cow::Owned(out);
    }
    std::borrow::Cow::Borrowed(s)
}

/// Cached per-file source content + line-start byte index, populated
/// lazily inside [`ContextPropagationExecutor`] for each `FileId` we
/// need to extract a call-site span text from.
struct FileText {
    bytes: Vec<u8>,
    /// `line_starts[i]` is the byte offset of the start of source
    /// line `i` (0-indexed). `line_starts.len() == number_of_lines`.
    line_starts: Vec<usize>,
}

impl FileText {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        let mut line_starts = vec![0usize];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { bytes, line_starts }
    }

    /// Convert a `(line, column)` `Position` (0-indexed, byte-column)
    /// into a byte offset into `bytes`. Returns `None` when the
    /// position is past the file or otherwise out of range — the
    /// caller treats that as "cannot resolve span" and bails out of
    /// the threaded-check (the call is reported as a leak per the
    /// design's false-positive-over-silent-miss convention).
    fn byte_offset(&self, pos: Position) -> Option<usize> {
        let line_start = *self.line_starts.get(pos.line)?;
        let offset = line_start.checked_add(pos.column)?;
        if offset > self.bytes.len() {
            return None;
        }
        Some(offset)
    }

    fn slice(&self, span: Span) -> Option<&str> {
        let start = self.byte_offset(span.start)?;
        let end = self.byte_offset(span.end)?;
        if end < start || end > self.bytes.len() {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..end]).ok()
    }
}

/// Per-execute state. Created once per `execute` call. All caches are
/// keyed on `NodeId` / `FileId` and never persist across calls — the
/// `DerivedQuery` cache (managed by `QueryDb`) is the persistent layer.
struct ContextPropagationExecutor<'a> {
    snapshot: &'a GraphSnapshot,
    /// `node → Some(param_name_if_known)` when the node accepts a
    /// `context.Context` parameter, `None` otherwise. The inner
    /// `Option<String>` distinguishes "has ctx, name is `ctx`" from
    /// "has ctx but the edge did not carry a parameter name".
    ctx_param_cache: HashMap<NodeId, Option<String>>,
    /// `node → Some(request_param_name)` when the node's signature is
    /// an HTTP handler; the inner `String` is the identifier the source
    /// used for the `*http.Request` parameter (typically `r`), which
    /// `call_threads_context` looks for as `<r>.Context()` to detect
    /// threaded handlers. `None` means "not an HTTP handler".
    http_handler_cache: HashMap<NodeId, Option<String>>,
    /// `file → Some(FileText)` when the source was successfully read;
    /// `file → None` when reading or path resolution failed for that
    /// file. Per-file reads are cached so a workspace with thousands
    /// of `Calls` edges across N files pays at most N read costs.
    file_text_cache: HashMap<FileId, Option<FileText>>,
    /// `file → ImportAliases` map of which identifiers refer to the
    /// stdlib `context` / `net/http` packages in that file. Lazily
    /// populated by scanning each file's import block exactly once.
    import_alias_cache: HashMap<FileId, ImportAliases>,
}

impl<'a> ContextPropagationExecutor<'a> {
    fn new(snapshot: &'a GraphSnapshot) -> Self {
        Self {
            snapshot,
            ctx_param_cache: HashMap::new(),
            http_handler_cache: HashMap::new(),
            file_text_cache: HashMap::new(),
            import_alias_cache: HashMap::new(),
        }
    }

    /// Returns the import-alias map for `file`, parsing the file's
    /// source the first time the file is queried and reusing the
    /// cached map thereafter.
    fn import_aliases_for(&mut self, file: FileId) -> ImportAliases {
        if let Some(cached) = self.import_alias_cache.get(&file) {
            return cached.clone();
        }
        // Resolve the file's path independently of `file_text_cache`
        // to avoid borrowing &mut self in two places; the source is
        // cheap to re-read and the result is cached on this branch.
        let aliases = self
            .snapshot
            .files()
            .resolve(file)
            .and_then(|path| std::fs::read(path.as_ref()).ok())
            .map(|bytes| extract_go_import_aliases(&bytes))
            .unwrap_or_default();
        self.import_alias_cache.insert(file, aliases.clone());
        aliases
    }

    fn run(&mut self, key: &ContextPropagationKey) -> Arc<ContextLeakSet> {
        let mut leaks: Vec<ContextLeak> = Vec::new();

        for edge in self.snapshot.edges().all_live_forward_edges() {
            let (argument_count, is_async) = match &edge.kind {
                EdgeKind::Calls {
                    argument_count,
                    is_async,
                    // `resolved_via` (master's C indirect-call precision) is
                    // irrelevant to context-leak detection — we only need the
                    // call's arity + async-ness here.
                    ..
                } => (*argument_count, *is_async),
                _ => continue,
            };

            // The caller's NodeEntry carries the file (per
            // `NodeEntry::file` at sqry-core/src/graph/unified/storage/arena.rs:177).
            // Codex iter-1 BLOCKER-2: `StoreEdgeRef::file` is lost to
            // `FileId::INVALID` for CSR-backed edges after compaction /
            // persistence, so we MUST derive the caller's file from the
            // node instead of trusting `edge.file`. The caller's file
            // is also the right scope discriminator: a `Calls` edge
            // belongs to the file containing the caller function.
            let caller = edge.source;
            let callee = edge.target;
            let Some(caller_file) = self.snapshot.get_node(caller).map(|entry| entry.file) else {
                continue;
            };

            // Scope filter (file vs. global) — now keyed on caller_file.
            if let ContextScope::File(fid) = key.scope
                && caller_file != fid
            {
                continue;
            }

            // Tier-1 file dependency: every Calls edge we INSPECT
            // contributes (even negative results) so the cache
            // invalidates correctly on edits to that file.
            record_file_dep(caller_file);

            // Callee must accept context.Context. Cheap fast-path.
            if self.ctx_param_name(callee).is_none() {
                continue;
            }

            let caller_ctx_name = self.ctx_param_name(caller).cloned();
            let caller_has_ctx = caller_ctx_name.is_some();
            let request_param_name = self.http_handler_request_param(caller);
            let caller_is_handler = request_param_name.is_some();

            // Classification priority (01_SPEC §3.2):
            //   - is_async (goroutine launch) → UnthreadedGoroutine
            //   - HTTP handler caller         → HttpHandlerLeak
            //   - sync caller with ctx        → BreakSite
            //   - neither                     → not a leak
            let mode = if is_async {
                ContextMode::UnthreadedGoroutine
            } else if caller_is_handler {
                ContextMode::HttpHandlerLeak
            } else if caller_has_ctx {
                ContextMode::BreakSite
            } else {
                continue;
            };

            // Threaded-call suppression. Both BreakSite/Goroutine paths
            // pass `caller_ctx_name`; HttpHandlerLeak passes
            // `request_param_name` so `r.Context()` is recognised as
            // threading (01_SPEC §3.2 / §6.2 AC-T3.7-4 negative case).
            if argument_count > 0
                && self.call_threads_context(
                    &edge.spans,
                    caller_file,
                    caller_ctx_name.as_deref(),
                    request_param_name.as_deref(),
                )
            {
                continue;
            }

            // Apply mode filter.
            let pass_filter = match key.mode {
                ContextModeFilter::All => true,
                ContextModeFilter::BreakSite => mode == ContextMode::BreakSite,
                ContextModeFilter::UnthreadedGoroutine => mode == ContextMode::UnthreadedGoroutine,
                ContextModeFilter::HttpHandlerLeak => mode == ContextMode::HttpHandlerLeak,
            };
            if !pass_filter {
                continue;
            }

            let call_span = edge.spans.first().copied().unwrap_or_default();
            leaks.push(ContextLeak {
                call_span,
                caller,
                callee,
                mode,
                caller_ctx_param: None,
            });
        }

        Arc::new(ContextLeakSet { leaks })
    }

    /// Returns `Some(param_name)` when the function-node `node` has an
    /// outbound `TypeOf{context: Parameter}` edge pointing at a node
    /// whose type is the stdlib `context.Context` — recognised via
    /// the function-file's import-alias map so aliased and dot-imported
    /// stdlib references are accepted. `param_name` is the
    /// `TypeOf.name` field when the Go plugin recorded it, else an
    /// empty string.
    fn ctx_param_name(&mut self, node: NodeId) -> Option<&String> {
        if !self.ctx_param_cache.contains_key(&node) {
            let computed = self.compute_ctx_param_name(node);
            self.ctx_param_cache.insert(node, computed);
        }
        self.ctx_param_cache.get(&node)?.as_ref()
    }

    fn compute_ctx_param_name(&mut self, node: NodeId) -> Option<String> {
        // Resolve the function's file once — its import-alias map
        // determines which identifiers refer to the stdlib `context`
        // package (per Codex iter-2 follow-up).
        let function_file = self.snapshot.get_node(node).map(|e| e.file)?;
        let aliases = self.import_aliases_for(function_file);
        for edge in self.snapshot.edges().edges_from(node) {
            let EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                name,
                ..
            } = edge.kind
            else {
                continue;
            };
            if !self.is_context_type_node_with_aliases(edge.target, &aliases) {
                continue;
            }
            let param_name = name
                .and_then(|sid| self.snapshot.strings().resolve(sid))
                .map_or_else(String::new, |arc| arc.to_string());
            return Some(param_name);
        }
        None
    }

    /// Returns `true` when the type-node `target` is the stdlib
    /// `context.Context` AS REFERENCED FROM A FILE WHOSE IMPORT MAP
    /// IS `aliases`.
    ///
    /// The Go plugin stores the lexical type-text on each Type node:
    /// stdlib refs land as `qualified_name = "context::Context"`,
    /// aliased refs as `qualified_name = "<alias>::Context"`, and
    /// dot-imports / user-defined refs as bare `name = "Context"`
    /// with `qualified_name = None`. Disambiguating user-defined
    /// `type Context` from a dot-imported stdlib reference REQUIRES
    /// consulting the file's import statements (Codex iter-2 review:
    /// strict qualified-name-only match produces false negatives).
    fn is_context_type_node_with_aliases(&self, target: NodeId, aliases: &ImportAliases) -> bool {
        let Some(entry) = self.snapshot.get_node(target) else {
            return false;
        };
        // Prefer qualified_name (set when the type-text carried a
        // package prefix); fall back to the bare simple name.
        let qualified = entry
            .qualified_name
            .and_then(|sid| self.snapshot.strings().resolve(sid));
        if let Some(q) = qualified.as_deref() {
            let stripped = q.strip_prefix('*').unwrap_or(q);
            let (prefix, simple) = split_qualified(stripped);
            if simple == CONTEXT_SIMPLE_NAME && aliases.accepts_context(prefix) {
                return true;
            }
            return false;
        }
        // No qualified_name: type-text was a bare identifier. Match
        // ONLY when the file dot-imports the `context` package.
        let bare = self.snapshot.strings().resolve(entry.name);
        bare.as_deref() == Some(CONTEXT_SIMPLE_NAME) && aliases.accepts_context("")
    }

    /// Returns `Some(request_param_name)` when the function-node `node`
    /// has the shape `func(http.ResponseWriter, *http.Request)`,
    /// `None` otherwise.
    ///
    /// The `request_param_name` is the identifier the source used for
    /// the `*http.Request` parameter (read from the
    /// `TypeOf{Parameter, index=1, name=...}` edge's `name` field —
    /// typically `"r"`). Passing it into `call_threads_context`
    /// enables recognising a threaded handler call like
    /// `Work(r.Context())` and suppressing the leak per
    /// 01_SPEC §3.2 "http-handler-leak ... without `r.Context()` being
    /// threaded".
    fn http_handler_request_param(&mut self, node: NodeId) -> Option<String> {
        if !self.http_handler_cache.contains_key(&node) {
            let computed = self.compute_http_handler_request_param(node);
            self.http_handler_cache.insert(node, computed);
        }
        self.http_handler_cache.get(&node).and_then(Clone::clone)
    }

    fn compute_http_handler_request_param(&mut self, node: NodeId) -> Option<String> {
        // Per Codex iter-2: the recognizer must honour aliased and
        // dot-imported `net/http` references. Resolve the function's
        // file once and use its import-alias map.
        let function_file = self.snapshot.get_node(node).map(|e| e.file)?;
        let aliases = self.import_aliases_for(function_file);

        let mut p0_target: Option<NodeId> = None;
        let mut p1_target: Option<NodeId> = None;
        let mut p1_param_name: Option<String> = None;
        for edge in self.snapshot.edges().edges_from(node) {
            let EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                index,
                name,
            } = edge.kind
            else {
                continue;
            };
            match index {
                Some(0) => p0_target = Some(edge.target),
                Some(1) => {
                    p1_target = Some(edge.target);
                    p1_param_name = name
                        .and_then(|sid| self.snapshot.strings().resolve(sid))
                        .map(|arc| arc.to_string());
                }
                _ => {}
            }
        }
        let p0_ok = p0_target.is_some_and(|t| {
            self.matches_http_simple_name(t, HTTP_RESPONSE_WRITER_SIMPLE_NAME, &aliases)
        });
        let p1_ok = p1_target
            .is_some_and(|t| self.matches_http_simple_name(t, HTTP_REQUEST_SIMPLE_NAME, &aliases));
        if !(p0_ok && p1_ok) {
            return None;
        }
        Some(p1_param_name.unwrap_or_default())
    }

    /// Returns `true` when the type-node `target` refers to the stdlib
    /// `net/http.<expected_simple>` AS REFERENCED FROM A FILE WHOSE
    /// IMPORT MAP IS `aliases`. Mirrors `is_context_type_node_with_aliases`
    /// for the HTTP package: strips a leading `*` (for `*http.Request`),
    /// splits on `::` / `.`, and matches the prefix against the
    /// file's `http_idents`.
    fn matches_http_simple_name(
        &self,
        target: NodeId,
        expected_simple: &str,
        aliases: &ImportAliases,
    ) -> bool {
        let Some(entry) = self.snapshot.get_node(target) else {
            return false;
        };
        if let Some(qualified) = entry
            .qualified_name
            .and_then(|sid| self.snapshot.strings().resolve(sid))
        {
            let stripped = qualified.strip_prefix('*').unwrap_or(&qualified);
            let (prefix, simple) = split_qualified(stripped);
            return simple == expected_simple && aliases.accepts_http(prefix);
        }
        // Bare type-text (no qualified_name): dot-import or
        // user-defined. Strip leading `*` (defensive — bare `*Request`
        // never realistically appears but the spec doesn't forbid it).
        let bare = self.snapshot.strings().resolve(entry.name);
        let Some(bare) = bare.as_deref() else {
            return false;
        };
        let stripped = bare.strip_prefix('*').unwrap_or(bare);
        stripped == expected_simple && aliases.accepts_http("")
    }

    /// Best-effort source-span re-walk for the `ctx_threaded`
    /// predicate (02_DESIGN §4.2.a):
    ///
    /// 1. Resolve the caller's `FileId` to a path and read the file.
    /// 2. Slice the call-site `Span` text.
    /// 3. If the text contains `context.Background()` / `context.TODO()`,
    ///    return `false` (NOT threaded → leak per AC-T3.7-5).
    /// 4. If `caller_ctx_name` is non-empty and appears as a
    ///    word-boundary literal, return `true` (threaded).
    /// 5. If `request_param_name` is non-empty and the text contains
    ///    `<request>.Context()` (allowing whitespace inside the call),
    ///    return `true` (threaded handler — Codex iter-1 BLOCKER-1).
    /// 6. Otherwise return `false` (not threaded → leak).
    ///
    /// Any IO / parse failure yields `false` (conservative false-
    /// positive-over-silent-miss).
    fn call_threads_context(
        &mut self,
        spans: &[Span],
        file: FileId,
        caller_ctx_name: Option<&str>,
        request_param_name: Option<&str>,
    ) -> bool {
        let Some(span) = spans.first().copied() else {
            return false;
        };
        let Some(file_text) = self.read_file(file) else {
            return false;
        };
        let Some(text) = file_text.slice(span) else {
            return false;
        };
        // Step 3: explicit Background()/TODO() literal at the call site.
        if ctx_background_or_todo_regex().is_match(text) {
            return false;
        }
        // Step 4: caller's ctx-param identifier appears as a word.
        if let Some(ident) = caller_ctx_name.filter(|n| !n.is_empty()) {
            let pattern = format!(r"\b{}\b", regex::escape(ident));
            if let Ok(re) = Regex::new(&pattern)
                && re.is_match(text)
            {
                return true;
            }
        }
        // Step 5: `<request>.Context()` for the HttpHandlerLeak path.
        // The Go plugin records the `*http.Request` parameter's source
        // identifier on the `TypeOf{Parameter,index=1,name=…}` edge.
        // A handler that calls `Work(r.Context())` is the canonical
        // threaded-handler shape (01_SPEC §3.2).
        if let Some(req) = request_param_name.filter(|n| !n.is_empty()) {
            let pattern = format!(r"\b{}\s*\.\s*Context\s*\(\s*\)", regex::escape(req),);
            if let Ok(re) = Regex::new(&pattern)
                && re.is_match(text)
            {
                return true;
            }
        }
        false
    }

    fn read_file(&mut self, file: FileId) -> Option<&FileText> {
        if !self.file_text_cache.contains_key(&file) {
            let loaded = self
                .snapshot
                .files()
                .resolve(file)
                .and_then(|path| std::fs::read(path.as_ref()).ok())
                .map(FileText::from_bytes);
            self.file_text_cache.insert(file, loaded);
        }
        self.file_text_cache.get(&file)?.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Unit tests — pure-Rust StagingGraph + build_unified_graph fixtures
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use postcard::{from_bytes, to_allocvec};

    // ----- key + value postcard round-trip ---------------------------------

    #[test]
    fn context_propagation_key_postcard_roundtrip() {
        for key in [
            ContextPropagationKey {
                scope: ContextScope::Global,
                mode: ContextModeFilter::All,
            },
            ContextPropagationKey {
                scope: ContextScope::File(FileId::new(7)),
                mode: ContextModeFilter::BreakSite,
            },
            ContextPropagationKey {
                scope: ContextScope::File(FileId::new(123_456)),
                mode: ContextModeFilter::UnthreadedGoroutine,
            },
            ContextPropagationKey {
                scope: ContextScope::Global,
                mode: ContextModeFilter::HttpHandlerLeak,
            },
        ] {
            let bytes = to_allocvec(&key).expect("serialize");
            let decoded: ContextPropagationKey = from_bytes(&bytes).expect("deserialize");
            assert_eq!(decoded, key);
        }
    }

    #[test]
    fn context_leak_set_postcard_roundtrip() {
        let leaks = ContextLeakSet {
            leaks: vec![
                ContextLeak {
                    call_span: Span::default(),
                    caller: NodeId::new(1, 1),
                    callee: NodeId::new(2, 1),
                    mode: ContextMode::BreakSite,
                    caller_ctx_param: None,
                },
                ContextLeak {
                    call_span: Span::new(Position::new(3, 4), Position::new(3, 10)),
                    caller: NodeId::new(10, 2),
                    callee: NodeId::new(20, 2),
                    mode: ContextMode::HttpHandlerLeak,
                    caller_ctx_param: Some(NodeId::new(42, 1)),
                },
                ContextLeak {
                    call_span: Span::default(),
                    caller: NodeId::new(7, 1),
                    callee: NodeId::new(8, 1),
                    mode: ContextMode::UnthreadedGoroutine,
                    caller_ctx_param: None,
                },
            ],
        };
        let bytes = to_allocvec(&leaks).expect("serialize");
        let decoded: ContextLeakSet = from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded, leaks);
    }

    // ----- file-text helpers ------------------------------------------------

    #[test]
    fn file_text_line_starts_for_unix_endings() {
        let ft = FileText::from_bytes(b"abc\ndef\nghi".to_vec());
        // Lines 0,1,2 start at 0, 4, 8.
        assert_eq!(ft.line_starts, vec![0, 4, 8]);
    }

    #[test]
    fn file_text_byte_offset_resolves_line_col() {
        let ft = FileText::from_bytes(b"abc\ndef\nghi".to_vec());
        assert_eq!(ft.byte_offset(Position::new(0, 0)), Some(0));
        assert_eq!(ft.byte_offset(Position::new(0, 2)), Some(2));
        assert_eq!(ft.byte_offset(Position::new(1, 0)), Some(4));
        assert_eq!(ft.byte_offset(Position::new(1, 2)), Some(6));
        assert_eq!(ft.byte_offset(Position::new(2, 3)), Some(11));
        // Past-end-of-file column is rejected.
        assert_eq!(ft.byte_offset(Position::new(2, 99)), None);
        // Past-end-of-file line is rejected.
        assert_eq!(ft.byte_offset(Position::new(99, 0)), None);
    }

    #[test]
    fn file_text_slice_returns_span_text() {
        let ft = FileText::from_bytes(b"abc\ndef\nghi".to_vec());
        let span = Span::new(Position::new(1, 0), Position::new(1, 3));
        assert_eq!(ft.slice(span), Some("def"));
    }

    // ----- Background/TODO regex -------------------------------------------

    #[test]
    fn background_regex_matches_canonical_form() {
        let re = ctx_background_or_todo_regex();
        assert!(re.is_match("Callee(context.Background())"));
        assert!(re.is_match("Callee(context.TODO())"));
        assert!(re.is_match("Callee(\n\tcontext . Background ( )\n)"));
        assert!(!re.is_match("Callee(ctx)"));
        assert!(!re.is_match("Callee(myctx.Background())"));
    }

    // ----- import-alias extractor -------------------------------------------

    #[test]
    fn extract_plain_import_records_default_ident() {
        let src = b"package main\n\nimport \"context\"\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("context"));
        assert!(!aliases.accepts_context(""));
        assert!(!aliases.accepts_context("c"));
    }

    #[test]
    fn extract_aliased_import_records_alias() {
        let src = b"package main\n\nimport c \"context\"\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("c"));
        assert!(!aliases.accepts_context("context"));
        assert!(!aliases.accepts_context(""));
    }

    #[test]
    fn extract_dot_import_records_empty_string() {
        let src = b"package main\n\nimport . \"context\"\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context(""));
        assert!(!aliases.accepts_context("context"));
    }

    #[test]
    fn extract_underscore_import_is_unusable() {
        let src = b"package main\n\nimport _ \"context\"\n";
        let aliases = extract_go_import_aliases(src);
        assert!(!aliases.accepts_context(""));
        assert!(!aliases.accepts_context("context"));
        assert!(!aliases.accepts_context("_"));
    }

    #[test]
    fn extract_block_import_handles_all_forms() {
        let src = b"package main\n\nimport (\n    \"context\"\n    h \"net/http\"\n    . \"errors\"\n    _ \"unsafe\"\n)\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("context"));
        assert!(aliases.accepts_http("h"));
    }

    #[test]
    fn extract_stops_at_top_level_decl() {
        // A `func` outside a block ends the import section.
        let src = b"package main\n\nimport \"context\"\n\nfunc f() {}\n// import \"net/http\"  // commented, ignored\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("context"));
        assert!(!aliases.accepts_http("http"));
        assert!(!aliases.accepts_http("net/http"));
    }

    #[test]
    fn extract_handles_trailing_line_comments() {
        // Real-world Go style: `import c "context" // canonical alias`.
        // Codex iter-3 BLOCKER pinned this case.
        let src = b"package main\n\nimport c \"context\" // canonical alias\n";
        let aliases = extract_go_import_aliases(src);
        assert!(
            aliases.accepts_context("c"),
            "trailing `//` comment must not drop the alias spec",
        );
    }

    #[test]
    fn extract_handles_block_comments_inside_import_block() {
        let src = b"package main\n\nimport (\n    \"context\" // stdlib\n    h \"net/http\" /* used by handler */\n)\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("context"));
        assert!(aliases.accepts_http("h"));
    }

    #[test]
    fn extract_handles_inline_block_comment_between_alias_and_path() {
        let src = b"package main\n\nimport c /* note */ \"context\"\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("c"));
    }

    #[test]
    fn extract_handles_multiple_aliases_for_same_path() {
        let src = b"package main\n\nimport (\n    \"context\"\n    c \"context\"\n)\n";
        let aliases = extract_go_import_aliases(src);
        assert!(aliases.accepts_context("context"));
        assert!(aliases.accepts_context("c"));
    }

    #[test]
    fn extract_ignores_unrelated_packages() {
        let src = b"package main\n\nimport (\n    \"fmt\"\n    \"github.com/foo/context\"\n)\n";
        let aliases = extract_go_import_aliases(src);
        // `"github.com/foo/context"` is NOT the stdlib path — must not
        // be recorded under context_idents even though the path ends
        // in `context`.
        assert!(!aliases.accepts_context("context"));
        assert!(!aliases.accepts_context(""));
    }

    // ----- split_qualified -------------------------------------------------

    #[test]
    fn split_qualified_handles_both_separators() {
        assert_eq!(split_qualified("context::Context"), ("context", "Context"));
        assert_eq!(split_qualified("c.Context"), ("c", "Context"));
        assert_eq!(split_qualified("Context"), ("", "Context"));
    }

    // ----- End-to-end ACs (build_unified_graph + GoPlugin) ------------------
    //
    // The remaining ACs are covered by `sqry-db/tests/context_propagation.rs`,
    // which spins up a real temp workspace + GoPlugin and runs the query
    // against the resulting CodeGraph. Keeping unit tests pure-Rust so the
    // module compiles without a temp-workspace dependency.
}

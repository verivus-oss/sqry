//! T3 Cluster C — Go `Wraps` edge emitter (02_DESIGN §4.1).
//!
//! Emits [`EdgeKind::Wraps`] edges at the four Go error-chain emission
//! sites identified in 01_SPEC §3.1 / §5.1:
//!
//! 1. **`fmt.Errorf("...%w...", ...)`** — one edge per `%w` verb, keyed
//!    by [`WrapKind::ErrorfVerb`]. `chain_position` is `None` for a
//!    single `%w`, else `Some(i)` (0-based verb index, skipping `%%`).
//! 2. **`Unwrap() error` / `Unwrap() []error`** method bodies — edges
//!    from the receiver type to each wrapped expression, keyed by
//!    [`WrapKind::UnwrapMethod`] / [`WrapKind::UnwrapMultiMethod`].
//! 3. **`errors.Is(err, sentinel)`**, **`errors.As(err, &target)`**,
//!    **`errors.AsType[E](err)`** (Go 1.26+) — single-target inspection
//!    edges keyed by [`WrapKind::ErrorsIs`] / [`ErrorsAs`] / [`ErrorsAsType`].
//! 4. **`errors.Join(a, b, c, ...)`** — one edge per variadic argument
//!    keyed by [`WrapKind::ErrorsJoin`] with positional `chain_position`.
//!
//! # Deferred shapes (codex iter-1 NIT-1, [DESIGN-DELTA])
//!
//! Custom user-type methods `Is(target error) bool` and
//! `As(any) bool` (02_DESIGN §4.1.d, paragraph at line 608+) are NOT
//! emitted by Cluster C. The design specifies a `Wraps{UnwrapMethod,
//! None}` self-loop on receiver types that override traversal via
//! these methods (and only when the type does not also declare a real
//! `Unwrap`). The Cluster C emitter recognises only `Unwrap` (exact
//! name + `error` / `[]error` return); custom `Is` / `As` overrides
//! ride a later cluster's follow-up. The deferral preserves the
//! minimum-risk contract: any incorrect self-loop emission would
//! pollute `relation_query` results with traversal-override semantics
//! that consumers cannot distinguish from real wrap relationships.
//!
//! The source NodeId for `fmt.Errorf` / `errors.*` edges is the **caller
//! function** (the same convention every existing Go-plugin edge family
//! uses; see 02_DESIGN §4.1.a "Reconciling with 01_SPEC §6.1 AC-T3.6-1").
//! The user-visible "call site" identity is expressed via the edge's
//! `spans: Vec<Span>` field, which points at the wrap-call expression.
//!
//! Target resolution is intentionally minimum-risk: every target arg is
//! resolved to a placeholder NodeId via [`GraphBuildHelper::ensure_callee`]
//! (call-compatible kinds reuse covers Function/Method/Constant/
//! LambdaTarget — sentinels and locals routinely register as Functions
//! through the existing call-edge path). Phase 4c-prime cross-file
//! unification then merges placeholders with their canonical definitions
//! via the standard pipeline.
//!
//! [`EdgeKind::Wraps`]: sqry_core::graph::unified::edge::EdgeKind::Wraps
//! [`WrapKind::ErrorfVerb`]: sqry_core::graph::unified::edge::WrapKind::ErrorfVerb
//! [`WrapKind::UnwrapMethod`]: sqry_core::graph::unified::edge::WrapKind::UnwrapMethod
//! [`WrapKind::UnwrapMultiMethod`]: sqry_core::graph::unified::edge::WrapKind::UnwrapMultiMethod
//! [`WrapKind::ErrorsIs`]: sqry_core::graph::unified::edge::WrapKind::ErrorsIs
//! [`ErrorsAs`]: sqry_core::graph::unified::edge::WrapKind::ErrorsAs
//! [`ErrorsAsType`]: sqry_core::graph::unified::edge::WrapKind::ErrorsAsType
//! [`WrapKind::ErrorsJoin`]: sqry_core::graph::unified::edge::WrapKind::ErrorsJoin
//! [`GraphBuildHelper::ensure_callee`]: sqry_core::graph::unified::build::helper::GraphBuildHelper::ensure_callee

use std::collections::HashSet;

use sqry_core::graph::Span;
use sqry_core::graph::unified::NodeId as UnifiedNodeId;
use sqry_core::graph::unified::build::helper::{CalleeKindHint, GraphBuildHelper};
use sqry_core::graph::unified::edge::WrapKind;
use tree_sitter::Node;

/// Recorded `%w` verb position in a `fmt.Errorf` format string.
///
/// The `w_index` is 0-based **within the recorded `%w` set** (so if a
/// format string has two `%w` verbs, they get `w_index` 0 and 1 even
/// if non-`%w` verbs appear between them). This is the value that
/// flows into `EdgeKind::Wraps { chain_position }` per 02_DESIGN
/// §4.1.a step 5.b. The `arg_position` is the 0-based index into the
/// positional argument list **after** the format-string argument and
/// counts ALL consuming verbs (`%s`, `%w`, `*` modifiers, etc.) so
/// the format-string scanner can pair each `%w` with the correct
/// variadic argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WrapVerb {
    /// 0-based index within the recorded `%w` set only (drives
    /// `chain_position`).
    pub w_index: u16,
    /// 0-based index into the call's positional arguments after the
    /// format string (drives argument lookup).
    pub arg_position: u16,
}

/// Scan a `fmt.Errorf` format-string literal and return the positions
/// of every `%w` verb (02_DESIGN §4.1.a step 3).
///
/// Grammar:
/// - `%%` consumes two source characters, produces no verb.
/// - `%w` is recorded; `w_index` is its 0-based position within the
///   recorded `%w` set (per 02_DESIGN §4.1.a step 5.b — the value
///   that becomes `chain_position`). `arg_position` is its 0-based
///   position within ALL consuming `%`-verbs (including non-`%w`
///   verbs that bind to positional arguments and `*` modifiers).
/// - Every other `%`-verb (`%v`, `%s`, `%d`, `%T`, width/precision
///   modifiers including `*`, etc.) consumes its bytes and advances
///   the arg cursor without recording a `%w`.
///
/// Go's `fmt` supports explicit-argument-index verbs like `%[N]w`.
/// The scanner recognises `[N]` in the modifier prefix and reassigns
/// the running positional index for the verb that follows. Subsequent
/// implicit verbs continue from `N` (1-based) / `N-1` (0-based). See
/// the `format_string_scan_indexed_w_*` unit tests for the full
/// contract.
///
/// The scanner is byte-oriented; non-ASCII bytes inside the format
/// string are passed through unchanged (no UTF-8 awareness needed
/// since `%` is ASCII).
#[must_use]
pub(crate) fn scan_w_verbs(format: &str) -> Vec<WrapVerb> {
    let bytes = format.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // `arg_position` counts ALL consuming verbs (including `*` width
    // modifiers and non-`%w` verbs that bind to positional args).
    let mut next_arg_position: u16 = 0;
    // `w_index` counts ONLY recorded `%w` verbs — this is what flows
    // into `EdgeKind::Wraps { chain_position }` per 02_DESIGN §4.1.a.
    let mut next_w_index: u16 = 0;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        if i + 1 >= bytes.len() {
            // Trailing lone `%` — invalid Go but harmless: emit nothing.
            break;
        }
        let next = bytes[i + 1];
        if next == b'%' {
            // Literal percent — no verb, no arg.
            i += 2;
            continue;
        }
        // Skip width / precision / flag modifiers between `%` and the
        // verb letter. Per `fmt` grammar, these are: `+`, `-`, `#`, ` `,
        // `0`, digits, `.`, `*`, AND the explicit-argument-index form
        // `[N]` where N is a 1-based positional index (re-uses or
        // reorders args). The verb letter is the first ASCII letter
        // encountered after the modifier prefix; `*` consumes a
        // positional argument, but `[N]` REASSIGNS the next positional
        // index without consuming a separate arg slot.
        //
        // ChatGPT-Codex PR #279 bot finding: prior to this branch the
        // scanner stopped at `[` and missed `%[N]w` entirely. Real Go
        // code does e.g. `fmt.Errorf("first=%v second=%[1]w", a, err)`
        // — the `[1]` rewinds to arg 0 (1-based 1), so the `%w`
        // binds to `a` not `err`. Either way, the verb IS a `%w` and
        // must surface as a Wraps edge.
        let mut j = i + 1;
        let mut indexed_position: Option<u16> = None;
        loop {
            if j >= bytes.len() {
                break;
            }
            let c = bytes[j];
            match c {
                b'+' | b'-' | b'#' | b' ' | b'0' | b'.' | b'1'..=b'9' => {
                    j += 1;
                }
                b'*' => {
                    // Width / precision from a positional argument.
                    next_arg_position = next_arg_position.saturating_add(1);
                    j += 1;
                }
                b'[' => {
                    // Explicit-argument-index: `[N]` where N is a
                    // 1-based positional index. Parse the digits and
                    // the closing `]`; if the syntax is malformed,
                    // stop the modifier scan and let the next byte
                    // decide whether it's the verb letter.
                    let mut k = j + 1;
                    let digit_start = k;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k == digit_start || k >= bytes.len() || bytes[k] != b']' {
                        // Malformed `[...]` — treat as the end of the
                        // modifier prefix and let the verb detection
                        // below run on the original `[`. Real Go code
                        // would not compile in this state, but tolerate
                        // gracefully.
                        break;
                    }
                    // Parse the 1-based index into a 0-based position.
                    // Per Go `fmt`: `%[N]v` sets the NEXT positional
                    // arg to (N-1), so subsequent verbs continue from
                    // (N-1)+1, etc.
                    let n_str = std::str::from_utf8(&bytes[digit_start..k]).unwrap_or("0");
                    if let Ok(n) = n_str.parse::<u32>()
                        && n >= 1
                    {
                        indexed_position = u16::try_from(n - 1).ok();
                    }
                    j = k + 1;
                }
                _ => break,
            }
        }
        if j >= bytes.len() {
            // Format string ends with modifiers, no verb letter — break.
            break;
        }
        // If an explicit `[N]` index appeared in the modifier prefix,
        // it overrides the running counter for THIS verb's argument
        // binding. Subsequent verbs continue from N (i.e. the running
        // counter is updated to N before the verb consumes its arg).
        if let Some(pos) = indexed_position {
            next_arg_position = pos;
        }
        let verb_letter = bytes[j];
        if verb_letter == b'w' {
            out.push(WrapVerb {
                w_index: next_w_index,
                arg_position: next_arg_position,
            });
            next_w_index = next_w_index.saturating_add(1);
        }
        // Every recognised verb (including `%w`) consumes exactly one
        // positional argument.
        next_arg_position = next_arg_position.saturating_add(1);
        i = j + 1;
    }

    out
}

/// Extract a string-literal value from a Go AST node, returning `Some`
/// only when the node is a single interpreted or raw string literal
/// (per 02_DESIGN §4.1.a step 2's "single string-literal after constant-
/// fold of `+`-only chains of literals" rule, conservatively
/// implemented as "exactly one literal" — `+`-concat folding is left
/// to a follow-up).
///
/// Returns the *content* of the literal (quotes stripped, escape
/// sequences NOT unescaped — `%w` is a literal byte sequence so no
/// unescape is required).
pub(crate) fn extract_format_string_literal(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "interpreted_string_literal" | "raw_string_literal" => {
            let text = node.utf8_text(content).ok()?;
            let trimmed = text
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('`')
                .trim_end_matches('`');
            Some(trimmed.to_string())
        }
        _ => None,
    }
}

/// Try to emit a `Wraps` edge for a `type_conversion_expression` node
/// that is actually a Go-1.26+ `errors.AsType[E](err)` generic call.
///
/// tree-sitter-go (current grammar) can't disambiguate generic-function
/// calls from generic-type conversions, so `errors.AsType[*fs.PathError](err)`
/// parses as `(type_conversion_expression (generic_type ...) (operand))`.
/// We detect the shape by checking the `type` field's `generic_type`
/// for a `qualified_type` whose `package.name` is `errors.AsType`. When
/// it matches, the first type argument (after pointer-stripping) becomes
/// the edge target. Non-`errors.AsType` conversions are a silent no-op.
pub(crate) fn try_emit_wraps_for_type_conversion(
    type_conv_node: Node<'_>,
    content: &[u8],
    imports: &StdlibWrapImports,
    caller_function_node_id: UnifiedNodeId,
    package: &str,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let Some(type_node) = type_conv_node.child_by_field_name("type") else {
        return false;
    };
    if type_node.kind() != "generic_type" {
        return false;
    }
    let Some(base_type) = type_node.child_by_field_name("type") else {
        return false;
    };
    if !matches!(base_type.kind(), "qualified_type" | "type_identifier") {
        return false;
    }
    let base_text = base_type.utf8_text(content).ok().unwrap_or("").trim();
    if imports
        .canonical_wrap_callee(base_text, base_text)
        .as_deref()
        != Some("errors.AsType")
    {
        return false;
    }
    let Some(type_args) = type_node.child_by_field_name("type_arguments") else {
        return false;
    };
    let mut cursor = type_args.walk();
    let Some(first_arg) = type_args.named_children(&mut cursor).next() else {
        return false;
    };
    // type_arguments wraps each arg in `type_elem`; unwrap to the
    // actual type expression.
    let actual = if first_arg.kind() == "type_elem" {
        let mut tc = first_arg.walk();
        first_arg
            .named_children(&mut tc)
            .next()
            .unwrap_or(first_arg)
    } else {
        first_arg
    };
    let Some(type_qn) = extract_type_qualified_name(actual, content, package) else {
        return false;
    };
    let call_site_span = span_from_node(type_conv_node);
    let target_id = helper.ensure_callee(&type_qn, call_site_span, CalleeKindHint::Function);
    helper.add_wraps_edge(
        caller_function_node_id,
        target_id,
        WrapKind::ErrorsAsType,
        None,
        Some(call_site_span),
    );
    true
}

/// Try to emit `Wraps` edges for a `call_expression` whose callee
/// qualified name has already been resolved.
///
/// Returns `true` when at least one edge was emitted, `false` when the
/// callee qualified name does not match any of the `Wraps` triggers
/// (the caller should treat this as "not a wrap site" and continue).
///
/// `caller_function_node_id` is the source NodeId for the emitted
/// edges (per §4.1.a — the caller function, NOT the call site).
/// `call_site_span` is attached to each emitted edge's span vector so
/// MCP / IDE surfaces can render the user-visible "wrap occurs at
/// `<file>:<line>:<col>`" identity.
pub(crate) fn try_emit_wraps_for_call(
    call_node: Node<'_>,
    content: &[u8],
    callee_qualified: &str,
    callee_source: &str,
    imports: &StdlibWrapImports,
    caller_function_node_id: UnifiedNodeId,
    package: &str,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let call_site_span = span_from_node(call_node);
    let Some(callee) = imports.canonical_wrap_callee(callee_source, callee_qualified) else {
        return false;
    };

    match callee.as_str() {
        "fmt.Errorf" => emit_wraps_for_fmt_errorf(
            call_node,
            content,
            caller_function_node_id,
            package,
            call_site_span,
            helper,
        ),
        "errors.Is" => emit_wraps_for_errors_is(
            call_node,
            content,
            caller_function_node_id,
            package,
            call_site_span,
            helper,
        ),
        "errors.As" => emit_wraps_for_errors_as(
            call_node,
            content,
            caller_function_node_id,
            package,
            call_site_span,
            helper,
        ),
        "errors.AsType" => emit_wraps_for_errors_as_type(
            call_node,
            content,
            caller_function_node_id,
            package,
            call_site_span,
            helper,
        ),
        "errors.Join" => emit_wraps_for_errors_join(
            call_node,
            content,
            caller_function_node_id,
            package,
            call_site_span,
            helper,
        ),
        _ => false,
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StdlibWrapImports {
    fmt_aliases: HashSet<String>,
    errors_aliases: HashSet<String>,
}

impl StdlibWrapImports {
    pub(crate) fn from_root(root: Node<'_>, content: &[u8]) -> Self {
        let mut imports = Self::default();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() == "import_declaration" {
                imports.record_import_declaration(child, content);
            }
        }
        imports
    }

    pub(crate) fn canonical_wrap_callee(
        &self,
        callee_source: &str,
        callee_qualified: &str,
    ) -> Option<String> {
        if is_stdlib_wrap_callee(callee_qualified) {
            return Some(callee_qualified.to_string());
        }
        self.canonical_from_source(callee_source)
    }

    fn canonical_from_source(&self, callee_source: &str) -> Option<String> {
        let source = callee_source.trim();
        if let Some((prefix, field)) = source.split_once('.') {
            if self.fmt_aliases.contains(prefix) && field == "Errorf" {
                return Some("fmt.Errorf".to_string());
            }
            if self.errors_aliases.contains(prefix) && is_errors_wrap_func(field) {
                return Some(format!("errors.{field}"));
            }
            return None;
        }
        if self.fmt_aliases.contains(".") && source == "Errorf" {
            return Some("fmt.Errorf".to_string());
        }
        if self.errors_aliases.contains(".") && is_errors_wrap_func(source) {
            return Some(format!("errors.{source}"));
        }
        None
    }

    fn record_import_declaration(&mut self, node: Node<'_>, content: &[u8]) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "import_spec" {
                self.record_import_spec(child, content);
            } else if child.kind() == "import_spec_list" {
                let mut spec_cursor = child.walk();
                for spec_child in child.children(&mut spec_cursor) {
                    if spec_child.kind() == "import_spec" {
                        self.record_import_spec(spec_child, content);
                    }
                }
            }
        }
    }

    fn record_import_spec(&mut self, node: Node<'_>, content: &[u8]) {
        let spec_text = node.utf8_text(content).ok();
        let mut alias: Option<String> = spec_text.and_then(import_alias_from_spec_text);
        let mut path: Option<String> = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "package_identifier" | "." | "_" => {
                    alias = child.utf8_text(content).ok().map(|s| s.trim().to_string());
                }
                "interpreted_string_literal" | "raw_string_literal" => {
                    path = child.utf8_text(content).ok().map(strip_go_import_quotes);
                }
                _ => {}
            }
        }
        let Some(path) = path else {
            return;
        };
        let alias = alias.unwrap_or_else(|| default_import_alias(&path));
        if alias == "_" {
            return;
        }
        match path.as_str() {
            "fmt" => {
                self.fmt_aliases.insert(alias);
            }
            "errors" => {
                self.errors_aliases.insert(alias);
            }
            _ => {}
        }
    }
}

fn strip_go_import_quotes(text: &str) -> String {
    text.trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('`')
        .trim_end_matches('`')
        .to_string()
}

fn import_alias_from_spec_text(spec: &str) -> Option<String> {
    let quote_start = spec.find(['"', '`'])?;
    let prefix = spec[..quote_start].trim();
    prefix
        .split_whitespace()
        .last()
        .filter(|alias| !alias.is_empty())
        .map(ToString::to_string)
}

fn default_import_alias(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn is_stdlib_wrap_callee(callee: &str) -> bool {
    matches!(
        callee,
        "fmt.Errorf" | "errors.Is" | "errors.As" | "errors.AsType" | "errors.Join"
    )
}

fn is_errors_wrap_func(name: &str) -> bool {
    matches!(name, "Is" | "As" | "AsType" | "Join")
}

/// `fmt.Errorf` emission (02_DESIGN §4.1.a). Returns `true` iff at
/// least one `%w` was found AND its argument resolved to a non-nil
/// target.
fn emit_wraps_for_fmt_errorf(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let args = match call_arguments(call_node) {
        Some(a) => a,
        None => return false,
    };
    if args.is_empty() {
        return false;
    }
    let format_node = args[0];
    let Some(format_str) = extract_format_string_literal(format_node, content) else {
        // Non-literal format string — per 02_DESIGN §4.1.a step 2,
        // emit no edges.
        return false;
    };
    let verbs = scan_w_verbs(&format_str);
    if verbs.is_empty() {
        return false;
    }
    let recorded_count = verbs.len();
    let mut emitted_any = false;
    for verb in verbs {
        // The format string is `args[0]`; positional args start at
        // `args[1]`. `verb.arg_position` is 0-based into that tail.
        let arg_index_in_args = (verb.arg_position as usize).saturating_add(1);
        let Some(arg_node) = args.get(arg_index_in_args).copied() else {
            // Format string references more args than the call provided
            // — invalid Go but harmless: skip this verb silently.
            continue;
        };
        let Some(target_id) =
            resolve_arg_target(arg_node, content, package, call_site_span, helper)
        else {
            // `nil` literal or unresolvable expression — skip per
            // 02_DESIGN §4.1.a step 5.a (literal `nil` emits no edge).
            continue;
        };
        let chain_position = if recorded_count == 1 {
            None
        } else {
            Some(verb.w_index)
        };
        helper.add_wraps_edge(
            caller,
            target_id,
            WrapKind::ErrorfVerb,
            chain_position,
            Some(call_site_span),
        );
        emitted_any = true;
    }
    emitted_any
}

/// `errors.Is(err, sentinel)` emission (02_DESIGN §4.1.d).
fn emit_wraps_for_errors_is(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    // sentinel is the 2nd positional argument (index 1).
    emit_single_arg_wrap(
        call_node,
        content,
        caller,
        package,
        call_site_span,
        WrapKind::ErrorsIs,
        /*arg_index=*/ 1,
        helper,
    )
}

/// `errors.As(err, &target)` emission (02_DESIGN §4.1.d).
///
/// The target is the **type of** the dereferenced second arg, NOT the
/// variable binding (codex iter-1 F-2). For `&pe` where
/// `var pe *fs.PathError`, the edge target is the `fs.PathError` Type
/// node — we resolve this by walking up the call's enclosing function
/// body to find a `var_declaration` / `short_var_declaration` for `pe`,
/// extracting its type expression, and stripping the leading pointer.
///
/// When the type cannot be resolved (no var declaration found, or the
/// arg shape isn't `&ident`), the helper emits no edge rather than a
/// fictional target — preserving the §4.1 best-effort cap.
fn emit_wraps_for_errors_as(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let args = match call_arguments(call_node) {
        Some(a) => a,
        None => return false,
    };
    let Some(target_arg) = args.get(1).copied() else {
        return false;
    };
    // Expect `&ident` shape.
    let target_text = target_arg.utf8_text(content).ok().unwrap_or("").trim();
    let stripped_ident = target_text.trim_start_matches('&').trim();
    if stripped_ident.is_empty() || stripped_ident == "nil" {
        return false;
    }
    // Walk up the AST to find the enclosing function/method body, then
    // resolve `stripped_ident`'s declared type expression.
    let type_qn =
        match resolve_local_var_type_qualified(call_node, content, stripped_ident, package) {
            Some(qn) => qn,
            None => return false,
        };
    let target_id = helper.ensure_callee(&type_qn, call_site_span, CalleeKindHint::Function);
    helper.add_wraps_edge(
        caller,
        target_id,
        WrapKind::ErrorsAs,
        None,
        Some(call_site_span),
    );
    true
}

/// `errors.AsType[E](err)` emission (02_DESIGN §4.1.d, Go 1.26+).
///
/// The type argument lives on the call's `type_arguments` field
/// (tree-sitter Go's generic-call shape). The target is the **type
/// itself** (E), with pointer stripping and qualified-type handling so
/// `errors.AsType[*fs.PathError](err)` resolves to `fs.PathError`
/// (codex iter-1 F-2).
fn emit_wraps_for_errors_as_type(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let Some(type_args) = call_node.child_by_field_name("type_arguments") else {
        return false;
    };
    // Take the first type argument. tree-sitter-go wraps each type
    // arg in either a direct `type_identifier`, a `qualified_type`,
    // a `pointer_type`, or a `generic_type`. Resolve to a canonical
    // qualified-name string by extracting the underlying type text
    // and stripping a leading `*` if present.
    let mut cursor = type_args.walk();
    let Some(first_arg) = type_args.named_children(&mut cursor).next() else {
        return false;
    };
    let Some(type_qn) = extract_type_qualified_name(first_arg, content, package) else {
        return false;
    };
    let target_id = helper.ensure_callee(&type_qn, call_site_span, CalleeKindHint::Function);
    helper.add_wraps_edge(
        caller,
        target_id,
        WrapKind::ErrorsAsType,
        None,
        Some(call_site_span),
    );
    true
}

/// Walk up the AST from `call_node` to find the enclosing function /
/// method body and return the type expression text declared for
/// `ident`'s most-relevant `var_declaration` / `short_var_declaration`.
///
/// Returns the qualified-name form of the dereferenced type (e.g.
/// `*fs.PathError` → `fs.PathError`; bare `MyError` → `<package>.MyError`).
/// Returns `None` when no declaration is found OR when the declaration
/// uses `:=` without an explicit type (we can't synthesise a type
/// without binding-plane info).
fn resolve_local_var_type_qualified(
    call_node: Node<'_>,
    content: &[u8],
    ident: &str,
    package: &str,
) -> Option<String> {
    // Climb to the enclosing `function_declaration` / `method_declaration`.
    let mut scope = call_node.parent();
    while let Some(s) = scope {
        match s.kind() {
            "function_declaration" | "method_declaration" => break,
            _ => scope = s.parent(),
        }
    }
    let func = scope?;
    let body = func.child_by_field_name("body")?;
    let (var_decl, ident_idx) = find_var_decl_for_ident(body, content, ident, call_node)?;
    let type_text = var_decl_type_text(var_decl, content, ident_idx)?;
    extract_qualified_name_from_type_text(&type_text, package)
}

/// Find a `var_spec` / `short_var_declaration` within `scope` whose
/// declared name list contains `ident`. Returns the type-bearing AST
/// node (the var_spec for `var`-form, the short_var_declaration node
/// itself for `:=` form), along with the 0-based index of `ident`
/// within the LHS name list. The index lets `var_decl_type_text`
/// pick the right element of a multi-binding `:=` declaration
/// (`a, b := ...`) when extracting from the RHS expression list.
///
/// Codex multi-LLM iter-1 finding 2: the earlier implementation did a
/// LIFO walk over the whole function body and returned the first
/// match, which respected neither lexical scope nor declaration
/// order. The fix here climbs **upward** from the call site's
/// containing block to the function body, scanning each enclosing
/// scope's direct-child declarations for the ident. This naturally
/// models Go's lexical scoping: declarations in sibling branches
/// (an `else` block, a later `if`, a `for` body) are invisible
/// because they're not ancestors of the call site.
///
/// At each scope (from innermost outward), collect candidate decls
/// whose `start_byte < call_node.start_byte()` and whose enclosing
/// statement contains or precedes the call site at that scope level.
/// Stop at the first scope that yields a match — innermost wins
/// (inner-block shadowing). If a scope yields multiple decls, prefer
/// the one with the highest `start_byte` (most recent declaration
/// the call site could see).
///
/// Still a heuristic — does NOT model:
/// - goto / labeled statements that skip forward.
/// - parameter declarations on the enclosing function (binding plane
///   handles those separately via the existing local-scope resolver).
///
/// `scope` is the outermost scope to search (typically the enclosing
/// function body). The walker stops at `scope` and never escapes it.
fn find_var_decl_for_ident<'tree>(
    scope: Node<'tree>,
    content: &[u8],
    ident: &str,
    call_node: Node<'tree>,
) -> Option<(Node<'tree>, usize)> {
    let call_start = call_node.start_byte();
    // Climb from the call site up to (and including) `scope`. At
    // each enclosing block, scan its direct-child statements for
    // a decl of `ident` whose start_byte < call_start.
    let mut cur = call_node;
    loop {
        // For each child of `cur` (when `cur` is a block-like node),
        // check if it directly declares `ident`. Take the latest one
        // (highest start_byte) that precedes the call.
        let mut best: Option<(Node<'tree>, usize)> = None;
        let mut cursor = cur.walk();
        for child in cur.named_children(&mut cursor) {
            // Only consider statements that occur textually before
            // the call site at this scope level. (A statement that
            // CONTAINS the call site is not a sibling declaration —
            // we'll descend through it on a different path of the
            // climb.)
            if child.end_byte() > call_start {
                continue;
            }
            let candidate = decl_idx_for_ident(child, content, ident);
            if let Some((decl_node, idx)) = candidate
                && best
                    .as_ref()
                    .is_none_or(|(b, _)| decl_node.start_byte() > b.start_byte())
            {
                best = Some((decl_node, idx));
            }
        }
        if best.is_some() {
            return best;
        }
        if cur.id() == scope.id() {
            return None;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// If `node` is itself a var/const/short-var decl that declares
/// `ident`, return the type-bearing AST node + the LHS position. If
/// `node` is a wrapper (e.g. `var_declaration` containing multiple
/// `var_spec`s, or an `expression_statement` containing the short
/// var decl), descend one level to look inside. Returns `None` if
/// the node is unrelated.
fn decl_idx_for_ident<'tree>(
    node: Node<'tree>,
    content: &[u8],
    ident: &str,
) -> Option<(Node<'tree>, usize)> {
    match node.kind() {
        "var_spec" | "const_spec" => {
            var_spec_ident_position(node, content, ident).map(|idx| (node, idx))
        }
        "short_var_declaration" => {
            short_var_ident_position(node, content, ident).map(|idx| (node, idx))
        }
        // `var_declaration` / `const_declaration` wrap one or more
        // `var_spec` / `const_spec` children — recurse one level.
        "var_declaration" | "const_declaration" => {
            let mut cursor = node.walk();
            let mut best: Option<(Node<'tree>, usize)> = None;
            for child in node.named_children(&mut cursor) {
                if let Some(found) = decl_idx_for_ident(child, content, ident)
                    && best
                        .as_ref()
                        .is_none_or(|(b, _)| found.0.start_byte() > b.start_byte())
                {
                    best = Some(found);
                }
            }
            best
        }
        _ => None,
    }
}

fn var_spec_ident_position(spec: Node<'_>, content: &[u8], ident: &str) -> Option<usize> {
    // var_spec has a `name` field that may be a single identifier or
    // an identifier_list — tree-sitter-go's exact shape varies. We
    // walk the spec's named identifier children in source order and
    // return the 0-based position of `ident` if found.
    let mut cursor = spec.walk();
    let mut idx: usize = 0;
    for child in spec.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            if child.utf8_text(content).ok().unwrap_or("").trim() == ident {
                return Some(idx);
            }
            idx += 1;
        }
    }
    None
}

fn short_var_ident_position(decl: Node<'_>, content: &[u8], ident: &str) -> Option<usize> {
    // short_var_declaration has a `left` field of type expression_list.
    let left = decl.child_by_field_name("left")?;
    let mut cursor = left.walk();
    for (idx, child) in left.named_children(&mut cursor).enumerate() {
        if child.kind() == "identifier"
            && child.utf8_text(content).ok().unwrap_or("").trim() == ident
        {
            return Some(idx);
        }
    }
    None
}

/// Extract the type expression's source text from a `var_spec` or
/// `short_var_declaration` for the binding at LHS position `ident_idx`.
///
/// Three recognised paths (covering 02_DESIGN §4.1.d's "TypeOf-resolution"
/// best-effort cap and codex iter-2 F-2's `:=` requirement):
///
/// 1. **Explicit `type` field** (`var pe *fs.PathError` or
///    `var pe *fs.PathError = nil`). Returned verbatim.
/// 2. **`:=` with composite-literal RHS at the matching position**
///    (`pe := &fs.PathError{...}` or `pe := fs.PathError{...}`).
///    Strips the leading `&` from a `unary_expression`, then reads
///    the composite_literal's `type` field. This is the common
///    `errors.As`-with-`:=` pattern.
/// 3. **`:=` with parenthesised type-conversion RHS whose inner
///    expression is unambiguously type-shaped**
///    (`pe := (*fs.PathError)(nil)`, `xs := ([]int)(nil)`,
///    `f := (func() int)(g)`). The inner expression must begin with
///    `*`, `[`, or one of the type keywords (`func`, `chan`, `map`,
///    `interface`, `struct`) — anything else is treated as an
///    ordinary function call (parens are just grouping). Bare
///    `SomeType(x)` is intentionally not recognised because it is
///    syntactically indistinguishable from a regular function call;
///    treating it as a conversion would emit fictional targets.
///
/// Other `:=` shapes (`pe := someFn()`, `pe := someVar`, untyped
/// literals like `pe := nil`, parenthesised non-type expressions like
/// `(fn)(x)`) return `None` — the type cannot be synthesised without
/// proper binding-plane analysis, and emitting a fictional target
/// would violate the best-effort cap.
fn var_decl_type_text(node: Node<'_>, content: &[u8], ident_idx: usize) -> Option<String> {
    // Path 1: explicit `type` field.
    if let Some(type_node) = node.child_by_field_name("type") {
        let text = type_node.utf8_text(content).ok()?;
        return Some(text.trim().to_string());
    }

    // Paths 2 & 3: short_var_declaration without a type — inspect the
    // RHS expression at the matching position.
    if node.kind() != "short_var_declaration" {
        return None;
    }
    let right = node.child_by_field_name("right")?;
    let mut cursor = right.walk();
    let mut rhs_iter = right.named_children(&mut cursor);
    let rhs_expr = rhs_iter.nth(ident_idx)?;

    type_text_from_rhs_expression(rhs_expr, content)
}

/// Best-effort extract a type-expression source text from a `:=` RHS
/// expression at the relevant LHS position. See [`var_decl_type_text`]
/// for the recognition cap.
fn type_text_from_rhs_expression(rhs: Node<'_>, content: &[u8]) -> Option<String> {
    // `&Expr` — strip the leading reference operator and recurse on
    // the operand. tree-sitter-go produces `unary_expression` with
    // `operator: "&"`.
    if rhs.kind() == "unary_expression" {
        // Drop the leading `&` from the source text to find the actual
        // operand expression. The operand is the first named child
        // that isn't the operator token.
        let mut cursor = rhs.walk();
        let operand = rhs.named_children(&mut cursor).next()?;
        return type_text_from_rhs_expression(operand, content);
    }
    // `*Expr` — pointer dereference; recurse.
    if rhs.kind() == "pointer_type" {
        let mut cursor = rhs.walk();
        let operand = rhs.named_children(&mut cursor).next()?;
        return type_text_from_rhs_expression(operand, content);
    }
    // Composite literal: `Type{...}` or `&Type{...}` after `&` strip.
    if rhs.kind() == "composite_literal" {
        let type_node = rhs.child_by_field_name("type")?;
        let text = type_node.utf8_text(content).ok()?;
        return Some(text.trim().to_string());
    }
    // Type conversion (parenthesized type-shaped form): tree-sitter-go
    // (≥0.23) does NOT produce a `type_conversion_expression` node
    // kind for `T(x)` shapes (the design's original text named this
    // kind speculatively). Real conversions parse as `call_expression`
    // whose `function` is a `parenthesized_expression`, but **not
    // every** such call is a type conversion — `(fn)(x)` and
    // `(*funcPtr)(x)` parse the same way and are real function calls.
    //
    // We can only treat a parenthesized call as a type conversion when
    // the inner expression is unambiguously **type-shaped**. The
    // canonical Go syntax for parenthesised type conversion requires
    // the inner expression to begin with a token that cannot start
    // an ordinary expression: `*` (pointer type), `[` (array or
    // slice type), or one of the type keywords `func` / `chan` /
    // `map` / `interface` / `struct`. Bare `(X)(y)` where `X` is an
    // identifier or selector is a regular function call (parens are
    // just grouping); treating it as a conversion would emit
    // fictional `main::fn` targets and contradict the no-bare-form
    // contract documented just below.
    //
    // Bare `MyType("x")` (no parens) is likewise intentionally NOT
    // recognised — it is syntactically indistinguishable from a
    // regular function call.
    if rhs.kind() == "call_expression" {
        let function = rhs.child_by_field_name("function")?;
        if function.kind() == "parenthesized_expression" {
            let mut cursor = function.walk();
            let inner = function.named_children(&mut cursor).next()?;
            let text = inner.utf8_text(content).ok()?;
            let trimmed = text.trim();
            if is_type_shaped_expression_text(trimmed) {
                return Some(trimmed.to_string());
            }
        }
    }
    // Anything else (regular function call, identifier, literal,
    // unrecognised kind) — cannot synthesise a type without
    // binding-plane support; return None.
    None
}

/// Heuristic: is `text` an expression that can ONLY be a type
/// expression in Go (not also a valid value expression)?
///
/// Returns true when `text` starts with a token that can only begin a
/// type expression — pointer (`*`), array/slice (`[`), or one of the
/// type keywords (`func`, `chan`, `map`, `interface`, `struct`).
/// Plain identifiers and selector expressions return false (those can
/// be either types OR values; we leave them to the regular
/// function-call path).
fn is_type_shaped_expression_text(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return false;
    }
    // Pointer types: `*T` or `*pkg.T`.
    if t.starts_with('*') {
        return true;
    }
    // Array (`[N]T`) and slice (`[]T`) types both begin with `[`.
    // No Go value expression starts with `[` (that would be a
    // composite literal which is its own AST kind, not a unary `[`).
    if t.starts_with('[') {
        return true;
    }
    // Type keyword prefixes — must be followed by a non-identifier
    // character to avoid matching an identifier like `funcName`.
    for kw in ["func", "chan", "map", "interface", "struct"] {
        if let Some(rest) = t.strip_prefix(kw)
            && rest
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        {
            return true;
        }
    }
    false
}

/// Convert a type-expression text like `*fs.PathError`, `MyError`, or
/// `pkg.SomeType` to a qualified-name string suitable for
/// `ensure_callee` lookup. Strips a single leading `*` (pointer
/// dereference). Bare identifiers are package-qualified.
fn extract_qualified_name_from_type_text(type_text: &str, package: &str) -> Option<String> {
    let trimmed = type_text.trim();
    let no_ptr = trimmed.trim_start_matches('*').trim();
    if no_ptr.is_empty() || no_ptr == "nil" {
        return None;
    }
    if no_ptr.contains('.') {
        Some(no_ptr.to_string())
    } else {
        Some(qualify_identifier(no_ptr, package))
    }
}

/// Extract a qualified-name string from a tree-sitter-go type argument
/// node. Handles `type_identifier`, `qualified_type`, `pointer_type`,
/// and `generic_type` shapes; falls back to the node's full text run
/// through `extract_qualified_name_from_type_text` for other shapes.
fn extract_type_qualified_name(
    type_node: Node<'_>,
    content: &[u8],
    package: &str,
) -> Option<String> {
    let text = type_node.utf8_text(content).ok()?;
    extract_qualified_name_from_type_text(text, package)
}

/// `errors.Join(a, b, c, ...)` emission (02_DESIGN §4.1.d).
fn emit_wraps_for_errors_join(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let args = match call_arguments(call_node) {
        Some(a) => a,
        None => return false,
    };
    let mut emitted_any = false;
    for (i, arg_node) in args.iter().copied().enumerate() {
        let Some(target_id) =
            resolve_arg_target(arg_node, content, package, call_site_span, helper)
        else {
            continue;
        };
        let chain_position = u16::try_from(i).ok();
        helper.add_wraps_edge(
            caller,
            target_id,
            WrapKind::ErrorsJoin,
            chain_position,
            Some(call_site_span),
        );
        emitted_any = true;
    }
    emitted_any
}

/// Shared helper for single-target inspection emissions
/// (`errors.Is`, `errors.As`). Resolves `args[arg_index]` and emits a
/// single edge with `chain_position: None`.
fn emit_single_arg_wrap(
    call_node: Node<'_>,
    content: &[u8],
    caller: UnifiedNodeId,
    package: &str,
    call_site_span: Span,
    kind: WrapKind,
    arg_index: usize,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let args = match call_arguments(call_node) {
        Some(a) => a,
        None => return false,
    };
    let Some(arg_node) = args.get(arg_index).copied() else {
        return false;
    };
    let Some(target_id) = resolve_arg_target(arg_node, content, package, call_site_span, helper)
    else {
        return false;
    };
    helper.add_wraps_edge(caller, target_id, kind, None, Some(call_site_span));
    true
}

/// Resolve a call argument expression to a target `NodeId` for a
/// `Wraps` edge.
///
/// The shape-recognition cap is deliberately modest (the §4.1.a/§4.1.d
/// design text accepts best-effort target resolution): we recognise
/// `identifier`, `selector_expression`, and `unary_expression` (for
/// `&x` argument forms used by `errors.As`). Literal `nil` and any
/// other expression shape return `None` (per §4.1.a step 5.a and
/// §5.1.e nil-error guards).
fn resolve_arg_target(
    arg_node: Node<'_>,
    content: &[u8],
    package: &str,
    call_site_span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> Option<UnifiedNodeId> {
    let text = arg_node.utf8_text(content).ok()?.trim();
    if text == "nil" {
        return None;
    }
    let qualified = match arg_node.kind() {
        "identifier" => qualify_identifier(text, package),
        "selector_expression" => text.to_string(),
        "unary_expression" => {
            // `&x` — strip the leading `&` and recurse on the operand
            // text as if it were an identifier.
            let stripped = text.trim_start_matches('&').trim();
            if stripped.is_empty() || stripped == "nil" {
                return None;
            }
            if stripped.contains('.') {
                stripped.to_string()
            } else {
                qualify_identifier(stripped, package)
            }
        }
        _ => return None,
    };
    if qualified.is_empty() {
        return None;
    }
    Some(helper.ensure_callee(&qualified, call_site_span, CalleeKindHint::Function))
}

/// Walk a `call_expression`'s `arguments` field and return its
/// positional argument nodes in source order.
fn call_arguments<'tree>(call_node: Node<'tree>) -> Option<Vec<Node<'tree>>> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut out = Vec::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        // Skip variadic-spread markers (`...`) and other non-expression
        // tree-sitter nodes if any.
        if child.kind() == "variadic_argument" {
            // tree-sitter-go may surface `errors.Join(errs...)` as a
            // `variadic_argument` whose only child is the slice
            // expression. Treat it as a single argument.
            let mut vc = child.walk();
            for inner in child.named_children(&mut vc) {
                out.push(inner);
            }
            continue;
        }
        out.push(child);
    }
    Some(out)
}

// Note: `first_named_descendant_of_kind` was used by the early
// `emit_wraps_for_errors_as_type` implementation that walked
// `type_arguments` looking for a bare `type_identifier`. That dispatch
// is unreachable in production today because tree-sitter-go parses
// `errors.AsType[E](err)` as a `type_conversion_expression` rather
// than a `call_expression`; the type-conversion dispatcher uses the
// generic_type's `type_arguments` field directly. Helper removed.

/// Qualify a bare identifier with the current package, mirroring the
/// existing Go-plugin convention in `resolve_callee_qualified_name`
/// (graph_builder.rs:1417).
fn qualify_identifier(text: &str, package: &str) -> String {
    if text.contains('.') {
        text.to_string()
    } else {
        format!("{package}.{text}")
    }
}

/// Local copy of `graph_builder::span_from_node` to avoid an
/// inter-module dependency (the module is otherwise standalone).
fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

// ============================================================================
// Unwrap() method body analyser (02_DESIGN §4.1.b / §4.1.c)
// ============================================================================

/// Whether a method's name + result-type-text matches the Go `Unwrap`
/// contract.
///
/// Returns `Some(false)` for `Unwrap() error` (single-error variant),
/// `Some(true)` for `Unwrap() []error` (multi-error variant, Go 1.20+),
/// and `None` for anything else.
pub(crate) fn classify_unwrap_method(method_name: &str, result_text: &str) -> Option<bool> {
    if method_name != "Unwrap" {
        return None;
    }
    let trimmed = result_text.trim();
    match trimmed {
        "error" => Some(false),
        "[]error" => Some(true),
        _ => None,
    }
}

/// Try to emit `Wraps` edges for a recognised `Unwrap` method body.
///
/// Walks the method body looking for return statements with one of the
/// three documented shapes (02_DESIGN §4.1.b / §4.1.c):
///
/// - `return e.field` → single edge to `<package>.<receiver>.<field>`
///   (`UnwrapMethod` for `error`, `UnwrapMultiMethod` for `[]error`).
/// - `return ident` (local binding or parameter) → single edge to
///   `<package>.<ident>`.
/// - `return []error{a, b, ...}` → one edge per slice-literal element
///   with `chain_position: Some(i)`, kind `UnwrapMultiMethod`.
/// - `return errors.New(...)` / `return fmt.Errorf(...)` / `return nil`
///   → no edge (newly-constructed / nil returns have no target).
///
/// Anything else is logged at debug and skipped.
pub(crate) fn try_emit_wraps_for_unwrap_body(
    method_body: Node<'_>,
    content: &[u8],
    receiver_type_node_id: UnifiedNodeId,
    receiver_qualified_name: &str,
    package: &str,
    is_multi: bool,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let mut emitted_any = false;
    let return_kind = if is_multi {
        WrapKind::UnwrapMultiMethod
    } else {
        WrapKind::UnwrapMethod
    };

    let returns = collect_return_statements(method_body);
    for return_node in returns {
        emitted_any |= process_unwrap_return(
            return_node,
            content,
            receiver_type_node_id,
            receiver_qualified_name,
            package,
            return_kind,
            helper,
        );
    }
    emitted_any
}

fn collect_return_statements<'tree>(root: Node<'tree>) -> Vec<Node<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_statement" {
            out.push(n);
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn process_unwrap_return(
    return_node: Node<'_>,
    content: &[u8],
    receiver_type_node_id: UnifiedNodeId,
    receiver_qualified_name: &str,
    package: &str,
    return_kind: WrapKind,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    let span = span_from_node(return_node);
    // tree-sitter-go: return_statement has an `expression_list` of
    // returned expressions.
    let mut cursor = return_node.walk();
    let Some(expr_list) = return_node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "expression_list")
    else {
        return false;
    };
    // Take the first (and typically only) returned expression — Unwrap
    // returns exactly one value (`error` or `[]error`).
    let mut elc = expr_list.walk();
    let Some(returned) = expr_list.named_children(&mut elc).next() else {
        return false;
    };

    let text = returned.utf8_text(content).ok().unwrap_or("").trim();
    if text == "nil" {
        return false;
    }

    // Filter newly-constructed-error returns (no target).
    if returned.kind() == "call_expression" {
        if is_error_constructor_call(returned, content) {
            return false;
        }
        // Other call expressions (e.g. `return e.cause()`) — no edge,
        // we can't reasonably resolve the call result.
        return false;
    }

    // Slice-literal: `[]error{a, b, c}` → one edge per element.
    if returned.kind() == "composite_literal" {
        return emit_unwrap_slice_literal(
            returned,
            content,
            receiver_type_node_id,
            receiver_qualified_name,
            package,
            return_kind,
            span,
            helper,
        );
    }

    // Selector expression: `e.field` → resolve to `<recv>.<field>`.
    if returned.kind() == "selector_expression" {
        let Some(field_node) = returned.child_by_field_name("field") else {
            return false;
        };
        let field_name = field_node.utf8_text(content).ok().unwrap_or("").trim();
        if field_name.is_empty() {
            return false;
        }
        let target_qn = format!("{receiver_qualified_name}.{field_name}");
        let target_id = helper.ensure_callee(&target_qn, span, CalleeKindHint::Function);
        helper.add_wraps_edge(
            receiver_type_node_id,
            target_id,
            return_kind,
            None,
            Some(span),
        );
        return true;
    }

    // Plain identifier: `return inner` (parameter or local binding).
    if returned.kind() == "identifier" {
        if text.is_empty() {
            return false;
        }
        let target_qn = qualify_identifier(text, package);
        let target_id = helper.ensure_callee(&target_qn, span, CalleeKindHint::Function);
        helper.add_wraps_edge(
            receiver_type_node_id,
            target_id,
            return_kind,
            None,
            Some(span),
        );
        return true;
    }

    false
}

fn emit_unwrap_slice_literal(
    composite_literal: Node<'_>,
    content: &[u8],
    receiver_type_node_id: UnifiedNodeId,
    receiver_qualified_name: &str,
    package: &str,
    return_kind: WrapKind,
    span: Span,
    helper: &mut GraphBuildHelper<'_>,
) -> bool {
    // tree-sitter-go composite_literal has a `body` field of
    // literal_value containing the elements.
    let Some(body) = composite_literal.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let mut emitted_any = false;
    let mut idx: u16 = 0;
    for element in body.named_children(&mut cursor) {
        // tree-sitter-go: each element is wrapped in a
        // `literal_element` or appears directly as the expression.
        let actual = if element.kind() == "literal_element" {
            let mut ec = element.walk();
            element.named_children(&mut ec).next().unwrap_or(element)
        } else if element.kind() == "keyed_element" {
            // Keyed slice literal like `[]error{0: foo}` — rare, skip.
            continue;
        } else {
            element
        };
        let text = actual.utf8_text(content).ok().unwrap_or("").trim();
        if text == "nil" {
            // Per pkg/errors: nil entries in Unwrap() []error are invalid
            // but we just skip them rather than emit an edge.
            idx = idx.saturating_add(1);
            continue;
        }
        let target_qn = if actual.kind() == "selector_expression" {
            // `recv.field` — resolve the field against the receiver
            // type (the selector's operand is the receiver binding;
            // the field name binds to the receiver type's field set).
            // This mirrors the single-return Unwrap path's
            // `<recv_qn>.<field>` shape, dropping the variable-name
            // operand which is irrelevant to graph resolution.
            let field_node = actual.child_by_field_name("field");
            let field_text = field_node
                .and_then(|f| f.utf8_text(content).ok())
                .unwrap_or("")
                .trim();
            if field_text.is_empty() {
                idx = idx.saturating_add(1);
                continue;
            }
            format!("{receiver_qualified_name}.{field_text}")
        } else if actual.kind() == "identifier" {
            qualify_identifier(text, package)
        } else {
            idx = idx.saturating_add(1);
            continue;
        };
        let target_id = helper.ensure_callee(&target_qn, span, CalleeKindHint::Function);
        helper.add_wraps_edge(
            receiver_type_node_id,
            target_id,
            return_kind,
            Some(idx),
            Some(span),
        );
        emitted_any = true;
        idx = idx.saturating_add(1);
    }
    emitted_any
}

fn is_error_constructor_call(call_node: Node<'_>, content: &[u8]) -> bool {
    let Some(fn_node) = call_node.child_by_field_name("function") else {
        return false;
    };
    let text = fn_node.utf8_text(content).ok().unwrap_or("").trim();
    matches!(text, "errors.New" | "fmt.Errorf")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Format-string scanner -----

    #[test]
    fn format_string_scan_single_w() {
        let verbs = scan_w_verbs("ctx: %w");
        assert_eq!(verbs.len(), 1);
        assert_eq!(verbs[0].w_index, 0);
        assert_eq!(verbs[0].arg_position, 0);
    }

    #[test]
    fn format_string_scan_multi_w() {
        let verbs = scan_w_verbs("a=%w b=%w c=%w");
        assert_eq!(verbs.len(), 3);
        assert_eq!(verbs[0].w_index, 0);
        assert_eq!(verbs[0].arg_position, 0);
        assert_eq!(verbs[1].w_index, 1);
        assert_eq!(verbs[1].arg_position, 1);
        assert_eq!(verbs[2].w_index, 2);
        assert_eq!(verbs[2].arg_position, 2);
    }

    #[test]
    fn format_string_scan_skips_percent_percent() {
        // %% is literal and does not consume a positional arg.
        let verbs = scan_w_verbs("100%% errored: %w");
        assert_eq!(verbs.len(), 1);
        assert_eq!(verbs[0].w_index, 0);
        assert_eq!(verbs[0].arg_position, 0);
    }

    #[test]
    fn format_string_scan_mixed_with_other_verbs() {
        // %s consumes one positional arg, %w consumes another. The
        // `arg_position` of `%w` skips the %s positional arg, but the
        // `w_index` is 0 because it is the FIRST recorded %w (codex
        // iter-1 F-1: `chain_position` must index within the %w-only
        // set, NOT across all verbs).
        let verbs = scan_w_verbs("user=%s wrapped=%w");
        assert_eq!(verbs.len(), 1);
        assert_eq!(
            verbs[0].w_index, 0,
            "first recorded %w gets w_index 0 regardless of preceding non-%w verbs"
        );
        assert_eq!(
            verbs[0].arg_position, 1,
            "arg_position skips the %s positional arg"
        );
    }

    #[test]
    fn format_string_scan_mixed_verb_w_indices_only_count_w() {
        // Codex iter-1 F-1 regression pin: "%s %w %w" must yield
        // w_index = 0, 1 — NOT 1, 2 (the pre-fix bug used the
        // all-verbs cursor for chain_position).
        let verbs = scan_w_verbs("%s %w %w");
        assert_eq!(verbs.len(), 2);
        assert_eq!(verbs[0].w_index, 0);
        assert_eq!(verbs[0].arg_position, 1);
        assert_eq!(verbs[1].w_index, 1);
        assert_eq!(verbs[1].arg_position, 2);
    }

    #[test]
    fn format_string_scan_skips_width_modifiers() {
        // %5w is not valid Go but the lexical scanner must not mistake
        // the digit `5` for a verb letter; it should still recognise
        // the trailing `w` as the verb.
        let verbs = scan_w_verbs("padded=%5w");
        assert_eq!(verbs.len(), 1);
    }

    #[test]
    fn format_string_scan_star_modifier_consumes_arg() {
        // `%*d` consumes 2 positional args (width from `*`, value from
        // `d`); the trailing `%w` then binds to the third variadic arg
        // (arg_position == 2 since arg_position is 0-based into the
        // tail after the format string itself).
        let verbs = scan_w_verbs("[%*d] wrapped=%w");
        assert_eq!(verbs.len(), 1);
        assert_eq!(
            verbs[0].arg_position, 2,
            "star (+1) + d (+1) consumed args; %w lands at arg_position 2"
        );
    }

    #[test]
    fn format_string_scan_no_w_returns_empty() {
        let verbs = scan_w_verbs("no errors here, just %v %s %d");
        assert!(verbs.is_empty());
    }

    #[test]
    fn format_string_scan_trailing_lone_percent_is_safe() {
        let verbs = scan_w_verbs("ends with %");
        assert!(verbs.is_empty());
    }

    // ----- Explicit-argument-index verbs (`%[N]w`) -----
    // ChatGPT-Codex PR #279 review finding (P2): the scanner must
    // recognise Go's `[N]` explicit-argument-index modifier; the
    // pre-fix scanner stopped at `[` and missed every indexed `%w`,
    // silently dropping reverse-error-chain edges.

    #[test]
    fn format_string_scan_indexed_w_binds_to_explicit_position() {
        // `fmt.Errorf("first=%v second=%[1]w", a, err)`:
        // - `%v` consumes arg 0 (a).
        // - `[1]` resets the counter to 0 (1-based 1 → 0-based 0),
        //   so `%w` binds to arg 0 (a) too — NOT arg 1 (err).
        // This is unusual but a real Go corner: the binding follows
        // the index, not source order. We only care that the WRAP
        // edge is emitted; correctness of which-arg-is-the-target
        // is the caller's job (it indexes the call_expression's args
        // by `arg_position`).
        let verbs = scan_w_verbs("first=%v second=%[1]w");
        assert_eq!(verbs.len(), 1, "indexed %w must be recognised: {verbs:?}");
        assert_eq!(verbs[0].w_index, 0);
        assert_eq!(
            verbs[0].arg_position, 0,
            "[1] resets to 1-based 1 (0-based 0)",
        );
    }

    #[test]
    fn format_string_scan_indexed_w_with_modifiers() {
        // `%-5.2[3]w` — flag (`-`), width (`5`), precision (`.2`),
        // index (`[3]`), verb (`w`). Index 3 → 0-based 2.
        let verbs = scan_w_verbs("padded=%-5.2[3]w");
        assert_eq!(verbs.len(), 1);
        assert_eq!(verbs[0].arg_position, 2, "[3] sets 0-based position 2");
    }

    #[test]
    fn format_string_scan_indexed_w_then_implicit_resumes_from_index() {
        // After `%[2]w` the running counter advances past arg 1 to
        // arg 2, so the next implicit verb binds to arg 2.
        let verbs = scan_w_verbs("%[2]w %w");
        assert_eq!(verbs.len(), 2);
        assert_eq!(verbs[0].arg_position, 1, "[2] → 0-based 1");
        assert_eq!(
            verbs[1].arg_position, 2,
            "implicit verb after `[2]w` continues from position 2",
        );
    }

    #[test]
    fn format_string_scan_malformed_index_falls_through_gracefully() {
        // `%[abc]w` — non-digit between brackets. Real Go would not
        // compile, but the scanner must NOT panic and must NOT emit
        // a spurious Wraps edge for the `w` that follows the
        // unmatched `[`. Defensive: treat the modifier scan as
        // ending at the `[`; the `[` is then not a verb letter so
        // the scanner advances past it without recording anything.
        let verbs = scan_w_verbs("malformed=%[abc]w trailing=%w");
        // The trailing `%w` (after the malformed prefix) MUST still
        // surface — the scanner recovers and continues.
        assert_eq!(
            verbs.len(),
            1,
            "malformed [N] must not consume the trailing valid %w: {verbs:?}",
        );
    }

    #[test]
    fn format_string_scan_indexed_v_does_not_emit_w_edge() {
        // `%[2]v` is a non-wrap verb with explicit index — must NOT
        // emit a Wraps edge but MUST still update the running counter.
        let verbs = scan_w_verbs("indexed_v=%[2]v then=%w");
        assert_eq!(verbs.len(), 1, "only the `%w` should surface");
        assert_eq!(
            verbs[0].arg_position, 2,
            "[2]v sets counter to 1, then `%w` consumes arg at position 2",
        );
    }

    // ----- Unwrap method classifier -----

    #[test]
    fn classify_unwrap_method_single() {
        assert_eq!(classify_unwrap_method("Unwrap", "error"), Some(false));
        assert_eq!(classify_unwrap_method("Unwrap", " error "), Some(false));
    }

    #[test]
    fn classify_unwrap_method_multi() {
        assert_eq!(classify_unwrap_method("Unwrap", "[]error"), Some(true));
        assert_eq!(classify_unwrap_method("Unwrap", " []error "), Some(true));
    }

    #[test]
    fn classify_unwrap_method_rejects_other_names() {
        assert_eq!(classify_unwrap_method("unwrap", "error"), None);
        assert_eq!(classify_unwrap_method("UnwrapAll", "error"), None);
        assert_eq!(classify_unwrap_method("Cause", "error"), None);
    }

    #[test]
    fn classify_unwrap_method_rejects_other_return_types() {
        assert_eq!(classify_unwrap_method("Unwrap", "string"), None);
        assert_eq!(classify_unwrap_method("Unwrap", "*Error"), None);
        assert_eq!(classify_unwrap_method("Unwrap", ""), None);
    }
}

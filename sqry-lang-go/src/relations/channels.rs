//! T2.4 channel-pairing alias analysis (Phase 1, rules 1-3).
//!
//! Pure analysis over the file's tree-sitter tree: no graph mutation, no
//! `GraphBuildHelper`. `graph_builder.rs` owns emission and calls
//! [`resolve_channel`] at each channel-operation site to learn the canonical
//! `Channel` qualified name (and buffer classifier) the operation acts on, or
//! `None` when none of the Phase 1 rules prove an alias (the AC-4 zero-false-
//! positive fence).
//!
//! The three statically-derivable rules (`02_DESIGN.md` §3.4):
//!
//! 1. **Named local** — the operand is an identifier whose root binding is a
//!    single `make(chan T, ?N)` in the same function body. One alias-rename hop
//!    is followed (`x := y`). A reassignment of the name before the operation
//!    site invalidates the alias ([`GoReassignmentMap`]).
//! 2. **Single-parameter pass-through** — the operand is a channel parameter of
//!    the enclosing function, and exactly one local call site passes a
//!    rule-1/rule-3-rooted channel into that parameter position
//!    ([`FileLocalRule2Table`]). The operation stages its edge under that
//!    make-rooted qualified name directly; multi-candidate or cross-file cases
//!    emit nothing (Phase 2).
//! 3. **Struct field** — the operand is a `recv.field` selector whose receiver
//!    resolves (via the method receiver or a local binding) to a struct type;
//!    the canonical name is `{package}.{Struct}.{field}`.

use std::collections::{HashMap, HashSet};

use sqry_core::graph::unified::edge::kind::ChannelBufferKind;
use tree_sitter::Node;

/// The canonical channel an operation site acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelOrigin {
    /// `{package}.{function}.{var}` / make-rooted (rule 1-2) or
    /// `{package}.{Struct}.{field}` (rule 3).
    pub qualified_name: String,
    /// Buffer classifier of the originating `make(chan T, N)`, or `Unknown`
    /// when the channel is reached through a parameter / struct field where the
    /// resolver did not see the `make`.
    pub buffer_kind: ChannelBufferKind,
    /// Constant capacity for a `Buffered` channel, else `None`.
    pub capacity: Option<u32>,
}

// ===========================================================================
// Reassignment tracking (Go-plugin-local, `02_DESIGN.md` §3.6)
// ===========================================================================

/// Byte offsets at which each identifier name is reassigned via an
/// `assignment_statement` (`=` / `op=`, distinct from a `:=` fresh binding).
/// Per file; the resolver consults it to break the alias chain after a
/// reassignment (spec §7.1 "Reassignment").
pub(crate) struct GoReassignmentMap {
    by_name: HashMap<String, Vec<usize>>,
}

impl GoReassignmentMap {
    pub(crate) fn build(root: Node, content: &[u8]) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        collect_reassignments(root, content, &mut by_name);
        Self { by_name }
    }

    /// True when `name` is reassigned strictly after `decl_byte` and at or
    /// before `usage_byte` — i.e. the binding in effect at the operation site
    /// is no longer the original `make`.
    fn reassigned_between(&self, name: &str, decl_byte: usize, usage_byte: usize) -> bool {
        self.by_name.get(name).is_some_and(|offsets| {
            offsets
                .iter()
                .any(|&off| off > decl_byte && off <= usage_byte)
        })
    }
}

fn collect_reassignments(node: Node, content: &[u8], out: &mut HashMap<String, Vec<usize>>) {
    if node.kind() == "assignment_statement"
        && let Some(left) = node.child_by_field_name("left")
    {
        let mut cursor = left.walk();
        for target in left.children(&mut cursor) {
            if target.kind() == "identifier"
                && let Ok(name) = target.utf8_text(content)
            {
                out.entry(name.to_string())
                    .or_default()
                    .push(target.start_byte());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_reassignments(child, content, out);
    }
}

// ===========================================================================
// make(chan ...) detection + buffer classifier
// ===========================================================================

/// Named children of `node`, in source order.
fn named_children(node: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            out.push(child);
        }
    }
    out
}

/// If `call` is `make(chan T, ?N)`, return its buffer classifier.
fn make_channel_buffer(call: Node, content: &[u8]) -> Option<(ChannelBufferKind, Option<u32>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" || function.utf8_text(content).ok()? != "make" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let arg_nodes = named_children(args);
    let first = arg_nodes.first()?;
    if first.kind() != "channel_type" {
        return None;
    }
    if arg_nodes.len() == 1 {
        Some((ChannelBufferKind::Unbuffered, None))
    } else {
        let cap_node = arg_nodes[1];
        if cap_node.kind() == "int_literal"
            && let Ok(text) = cap_node.utf8_text(content)
            && let Ok(n) = text.trim().parse::<u32>()
        {
            Some((ChannelBufferKind::Buffered, Some(n)))
        } else {
            Some((ChannelBufferKind::Unknown, None))
        }
    }
}

/// Resolved root of a named-local channel: the variable name whose binding is
/// the single `make`, plus the buffer classifier.
struct MakeRoot {
    root_var: String,
    buffer_kind: ChannelBufferKind,
    capacity: Option<u32>,
}

/// Find the closest preceding `name := make(chan ...)` binding within
/// `func_node`, following at most one alias-rename hop (`x := y`).
fn find_make_root(
    func_node: Node,
    name: &str,
    usage_byte: usize,
    content: &[u8],
    reassign: &GoReassignmentMap,
    depth: usize,
) -> Option<MakeRoot> {
    if depth > 1 {
        return None;
    }
    let mut best: Option<(usize, Option<Rhs>)> = None;
    find_decl_rhs(func_node, name, usage_byte, content, &mut best);
    let (decl_byte, rhs) = best?;
    // A reassignment between the binding and the use breaks the alias chain.
    if reassign.reassigned_between(name, decl_byte, usage_byte) {
        return None;
    }
    let rhs = rhs?;
    match rhs {
        Rhs::Make(buffer_kind, capacity) => Some(MakeRoot {
            root_var: name.to_string(),
            buffer_kind,
            capacity,
        }),
        Rhs::Alias(other) => {
            find_make_root(func_node, &other, decl_byte, content, reassign, depth + 1)
        }
    }
}

enum Rhs {
    Make(ChannelBufferKind, Option<u32>),
    Alias(String),
}

/// True when a declaration statement is lexically in scope at `usage_byte`:
/// its nearest enclosing `block` must contain the usage. A binding inside a
/// nested closure body (a deeper `block`) is NOT visible to operations outside
/// that closure — without this guard the byte-ordered scan would attribute a
/// `make` inside a `func() {...}` literal to an outer same-named operation,
/// a false positive that breaks the AC-4 fence (Codex iter-2 finding).
pub(crate) fn decl_visible_at(decl_node: Node, usage_byte: usize) -> bool {
    let mut node = decl_node;
    while let Some(parent) = node.parent() {
        if parent.kind() == "block" {
            return parent.start_byte() <= usage_byte && usage_byte < parent.end_byte();
        }
        node = parent;
    }
    true
}

/// Scan `func_node` for the closest `name := <rhs>` short-var binding before
/// `usage_byte`, recording the relevant RHS shape (a `make` or a single alias
/// identifier).
fn find_decl_rhs(
    node: Node,
    name: &str,
    usage_byte: usize,
    content: &[u8],
    best: &mut Option<(usize, Option<Rhs>)>,
) {
    if node.kind() == "short_var_declaration"
        && let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        )
    {
        let lhs = named_children(left);
        let rhs = named_children(right);
        for (idx, target) in lhs.iter().enumerate() {
            if target.utf8_text(content).ok() == Some(name)
                && target.start_byte() < usage_byte
                && decl_visible_at(node, usage_byte)
                && let Some(rhs_expr) = rhs.get(idx)
            {
                let decl_byte = target.start_byte();
                if best.as_ref().is_none_or(|(b, _)| decl_byte > *b) {
                    let parsed = if let Some((bk, cap)) = make_channel_buffer(*rhs_expr, content) {
                        Some(Rhs::Make(bk, cap))
                    } else if rhs_expr.kind() == "identifier" {
                        rhs_expr
                            .utf8_text(content)
                            .ok()
                            .map(|t| Rhs::Alias(t.to_string()))
                    } else {
                        None
                    };
                    *best = Some((decl_byte, parsed));
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_decl_rhs(child, name, usage_byte, content, best);
    }
}

// ===========================================================================
// Parameter helpers (rule 2)
// ===========================================================================

/// Count the `name`-field identifiers a parameter declaration introduces.
fn param_decl_name_count(decl: Node) -> usize {
    let mut cursor = decl.walk();
    decl.children_by_field_name("name", &mut cursor).count()
}

/// The 0-based call-argument position at which `func_node` declares a parameter
/// named `name`, expanding multi-name declarations, or `None`.
fn parameter_index(func_node: Node, name: &str, content: &[u8]) -> Option<usize> {
    let params = func_node.child_by_field_name("parameters")?;
    let mut index = 0usize;
    let mut cursor = params.walk();
    for decl in params.children(&mut cursor) {
        if decl.kind() != "parameter_declaration" && decl.kind() != "variadic_parameter_declaration"
        {
            continue;
        }
        let mut name_cursor = decl.walk();
        let names: Vec<Node> = decl
            .children_by_field_name("name", &mut name_cursor)
            .collect();
        if names.is_empty() {
            index += 1;
            continue;
        }
        for n in names {
            if n.utf8_text(content).ok() == Some(name) {
                return Some(index);
            }
            index += 1;
        }
    }
    None
}

/// True when `func_node`'s parameter at call-position `index` has channel type.
fn parameter_is_channel(func_node: Node, index: usize) -> bool {
    let Some(params) = func_node.child_by_field_name("parameters") else {
        return false;
    };
    let mut pos = 0usize;
    let mut cursor = params.walk();
    for decl in params.children(&mut cursor) {
        if decl.kind() != "parameter_declaration" && decl.kind() != "variadic_parameter_declaration"
        {
            continue;
        }
        let is_channel = decl
            .child_by_field_name("type")
            .is_some_and(|t| t.kind() == "channel_type");
        let reps = param_decl_name_count(decl).max(1);
        if index < pos + reps {
            return is_channel;
        }
        pos += reps;
    }
    false
}

// ===========================================================================
// File-local rule-2 candidate table (`02_DESIGN.md` §3.5)
// ===========================================================================

/// Maps `(local_callee_name, param_index)` to the set of make-rooted /
/// struct-field-rooted channel qualified names observed at local call sites in
/// the file. A unique candidate authorises rule-2 emission.
pub(crate) struct FileLocalRule2Table {
    candidates: HashMap<(String, usize), HashSet<String>>,
}

impl FileLocalRule2Table {
    pub(crate) fn collect(
        root: Node,
        content: &[u8],
        package: &str,
        reassign: &GoReassignmentMap,
    ) -> Self {
        // Local function/method declarations keyed by simple name, so call
        // sites can check whether the callee's parameter at a position is a
        // channel.
        let mut local_funcs: HashMap<String, Node> = HashMap::new();
        let mut top_cursor = root.walk();
        for child in root.children(&mut top_cursor) {
            if child.kind() == "function_declaration"
                && let Some(name_node) = child.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                local_funcs.insert(name.to_string(), child);
            }
        }

        let mut candidates: HashMap<(String, usize), HashSet<String>> = HashMap::new();
        // Walk each top-level function body and harvest its call sites.
        let mut fn_cursor = root.walk();
        for func in root.children(&mut fn_cursor) {
            if func.kind() != "function_declaration" {
                continue;
            }
            let Some(caller_name) = func
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
            else {
                continue;
            };
            harvest_call_sites(
                func,
                func,
                caller_name,
                content,
                package,
                reassign,
                &local_funcs,
                &mut candidates,
            );
        }

        Self { candidates }
    }

    /// The unique make-rooted qualified name passed into `(callee, index)`, or
    /// `None` when zero or ≥ 2 distinct candidates exist (AC-4 fence applied to
    /// rule 2).
    pub(crate) fn unique_qn(&self, callee: &str, param_index: usize) -> Option<&str> {
        let set = self.candidates.get(&(callee.to_string(), param_index))?;
        if set.len() == 1 {
            set.iter().next().map(String::as_str)
        } else {
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn harvest_call_sites(
    node: Node,
    caller_func: Node,
    caller_name: &str,
    content: &[u8],
    package: &str,
    reassign: &GoReassignmentMap,
    local_funcs: &HashMap<String, Node>,
    candidates: &mut HashMap<(String, usize), HashSet<String>>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && function.kind() == "identifier"
        && let Ok(callee) = function.utf8_text(content)
        && let Some(callee_decl) = local_funcs.get(callee)
        && let Some(args) = node.child_by_field_name("arguments")
    {
        for (idx, arg) in named_children(args).iter().enumerate() {
            if !parameter_is_channel(*callee_decl, idx) {
                continue;
            }
            if let Some(qn) =
                rooted_channel_qn(*arg, content, package, caller_func, caller_name, reassign)
            {
                candidates
                    .entry((callee.to_string(), idx))
                    .or_default()
                    .insert(qn);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        harvest_call_sites(
            child,
            caller_func,
            caller_name,
            content,
            package,
            reassign,
            local_funcs,
            candidates,
        );
    }
}

/// Resolve a call-site argument to its make-rooted qualified name via rule 1
/// only (used when building the rule-2 candidate table).
fn rooted_channel_qn(
    arg: Node,
    content: &[u8],
    package: &str,
    func_node: Node,
    func_name: &str,
    reassign: &GoReassignmentMap,
) -> Option<String> {
    if arg.kind() != "identifier" {
        return None;
    }
    let name = arg.utf8_text(content).ok()?;
    let root = find_make_root(func_node, name, arg.start_byte(), content, reassign, 0)?;
    Some(format!("{package}.{func_name}.{}", root.root_var))
}

// ===========================================================================
// The resolver (rules 1-3)
// ===========================================================================

/// Resolve the channel an operation site acts on, or `None` under the AC-4
/// fence. `func_name` is the simple name of the enclosing function/method;
/// `receiver_name` / `receiver_type` carry the method receiver (rule 3).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_channel(
    operand: Node,
    content: &[u8],
    package: &str,
    func_node: Node,
    func_name: &str,
    usage_byte: usize,
    reassign: &GoReassignmentMap,
    rule2: &FileLocalRule2Table,
    receiver_name: Option<&str>,
    receiver_type: Option<&str>,
) -> Option<ChannelOrigin> {
    match operand.kind() {
        "identifier" => {
            let name = operand.utf8_text(content).ok()?;
            // Rule 1: named local rooted at a make.
            if let Some(root) = find_make_root(func_node, name, usage_byte, content, reassign, 0) {
                return Some(ChannelOrigin {
                    qualified_name: format!("{package}.{func_name}.{}", root.root_var),
                    buffer_kind: root.buffer_kind,
                    capacity: root.capacity,
                });
            }
            // Rule 2: single-parameter pass-through.
            if let Some(index) = parameter_index(func_node, name, content)
                && let Some(qn) = rule2.unique_qn(func_name, index)
            {
                return Some(ChannelOrigin {
                    qualified_name: qn.to_string(),
                    buffer_kind: ChannelBufferKind::Unknown,
                    capacity: None,
                });
            }
            None
        }
        "selector_expression" => {
            // Rule 3: struct field via `recv.field`.
            let field = operand.child_by_field_name("field")?;
            let field_name = field.utf8_text(content).ok()?;
            let recv = operand.child_by_field_name("operand")?;
            if recv.kind() != "identifier" {
                return None;
            }
            let recv_name = recv.utf8_text(content).ok()?;
            // Only the method-receiver case is in Phase 1 scope: `recv_name`
            // must be the receiver and resolve to a named struct type.
            if receiver_name != Some(recv_name) {
                return None;
            }
            let struct_name = receiver_type.map(strip_type_prefixes)?;
            if struct_name.is_empty() || !is_plain_ident(struct_name) {
                return None;
            }
            Some(ChannelOrigin {
                qualified_name: format!("{package}.{struct_name}.{field_name}"),
                buffer_kind: ChannelBufferKind::Unknown,
                capacity: None,
            })
        }
        _ => None,
    }
}

/// Strip leading `*` / `[]` / `...` modifiers from a Go type expression.
fn strip_type_prefixes(raw: &str) -> &str {
    let mut s = raw.trim();
    loop {
        if let Some(rest) = s.strip_prefix('*') {
            s = rest.trim_start();
        } else if let Some(rest) = s.strip_prefix("[]") {
            s = rest.trim_start();
        } else if let Some(rest) = s.strip_prefix("...") {
            s = rest.trim_start();
        } else {
            break;
        }
    }
    s
}

fn is_plain_ident(raw: &str) -> bool {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

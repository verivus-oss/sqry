//! Integration tests for U11 — Phase 3 commit + Phase 4 post-unification
//! application of C indirect-call side tables.
//!
//! Covers the DAG acceptance criteria for `IMP:c-icall-precision-011`:
//!
//! * `phase4_post_unification_address_taken`: a 2-file C workspace where
//!   file A defines a callback `cb_alpha` AND takes its address via
//!   `void (*p)(void) = &cb_alpha;`, and file B defines a same-named
//!   `cb_alpha`. After build, the resolved (canonical, post-unification
//!   in the call-compatible-kind sense) NodeId(s) named `cb_alpha` must
//!   carry `is_address_taken == true`.
//!
//! * `struct_field_fnptr_merge_across_files`: a 2-file workspace where
//!   one file declares `struct file_operations { int (*read)(int); }`
//!   and another file initialises an instance. The merged
//!   `struct_field_fnptr` table contains the canonical
//!   `(struct file_operations, read)` signature.
//!
//! * `bindings_by_field_cross_file_merge`: a cross-file binding produces
//!   one entry under the `(struct_tag_id, field_name_id)` key in the
//!   merged side table regardless of which file emitted the staging vec.
//!
//! These tests build a real workspace through `build_unified_graph` with
//! the real C plugin registered, so they exercise the full Phase 1 →
//! Phase 3 → Phase 4c-prime → Phase 4c-prime-post drain + apply pipeline
//! end-to-end.

use std::fs;
use std::path::PathBuf;

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::plugin::PluginManager;
use tempfile::TempDir;

/// Build the workspace under `root` with only the C plugin registered.
///
/// C-only registration is deliberate: U11's drain path runs unconditionally
/// (it's `None` for non-C chunks), and adding the other 36 plugins would
/// just add parse-time cost without exercising any new behaviour.
fn build_c_only_workspace(root: &std::path::Path) -> CodeGraph {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build must succeed for U11 fixture")
}

/// Build the workspace under `root` with BOTH the C and Rust plugins
/// registered.
///
/// Used by the negative regression
/// [`phase4_post_unification_address_taken_skips_non_c_namesake`] —
/// drives the U11 applier against a workspace that contains same-named
/// callables in two languages so the C-language-scope filter in
/// `apply_deferred_address_taken_marks` can be exercised end-to-end.
fn build_c_and_rust_workspace(root: &std::path::Path) -> CodeGraph {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
    plugins.register_builtin(Box::new(sqry_lang_rust::RustPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build must succeed for U11 fixture")
}

/// Look up canonical NodeIds for a C function by name through the
/// post-unification AuxiliaryIndices. Mirrors the resolution path used
/// by [`sqry_core::graph::unified::build::entrypoint::apply_deferred_address_taken_marks`]:
/// `by_qualified_name` ∪ `by_name`, filtered to CALL_COMPATIBLE_KINDS,
/// deduped.
fn resolve_c_function_node_ids(graph: &CodeGraph, name: &str) -> Vec<NodeId> {
    let Some(str_id) = graph.strings().get(name) else {
        return Vec::new();
    };
    let by_qn = graph.indices().by_qualified_name(str_id).to_vec();
    let by_nm = graph.indices().by_name(str_id).to_vec();
    let mut seen: std::collections::HashSet<(u32, u64)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    let arena = graph.nodes();
    for nid in by_qn.into_iter().chain(by_nm.into_iter()) {
        if !seen.insert((nid.index(), nid.generation())) {
            continue;
        }
        let Some(entry) = arena.get(nid) else {
            continue;
        };
        // Phase A only marks address-taken on `Function` / `Method` /
        // `Macro` / `Constant` / `LambdaTarget` (the CALL_COMPATIBLE_KINDS
        // set used by Phase 4c-prime).
        match entry.kind {
            NodeKind::Function
            | NodeKind::Method
            | NodeKind::Macro
            | NodeKind::Constant
            | NodeKind::LambdaTarget => out.push(nid),
            _ => {}
        }
    }
    out
}

/// `phase4_post_unification_address_taken` — DAG acceptance criterion 1.
///
/// File A defines `cb_alpha` AND takes its address (`void (*p)(void) =
/// &cb_alpha;`). File B defines an unrelated function — chosen so the
/// `cb_alpha` resolves uniquely after Phase 4c-prime; the U11 application
/// step must mark the canonical NodeId as address-taken regardless of
/// which file the address-take site lives in.
#[test]
fn phase4_post_unification_address_taken_unique_definition() {
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        // Define cb_alpha + take its address via a bare initializer.
        // `init_declarator_value` (DESIGN §2.6 row 8) is the
        // type-guarded pattern: `void (*p)(void) = &cb_alpha;` —
        // declarator chain reaches `pointer_declarator >
        // function_declarator`, so the guard fires.
        "void cb_alpha(void) {}\n\
         void (*p_alpha)(void) = &cb_alpha;\n",
    )
    .expect("write a.c");

    let file_b: PathBuf = root.join("b.c");
    fs::write(
        &file_b,
        // Unrelated function — exists so the workspace contains multiple
        // C TUs (exercising the chunked Phase 3 drain accumulation path).
        "int helper(void) { return 0; }\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let cb_alpha_ids = resolve_c_function_node_ids(&graph, "cb_alpha");
    assert!(
        !cb_alpha_ids.is_empty(),
        "post-build: cb_alpha must resolve to at least one CALL_COMPATIBLE_KINDS NodeId — \
         got empty set; full node arena ({} nodes)",
        graph.nodes().slot_count()
    );

    let metadata = graph.macro_metadata();
    for nid in &cb_alpha_ids {
        assert!(
            metadata.is_address_taken(*nid),
            "cb_alpha NodeId {nid:?} must carry NodeFlags::ADDRESS_TAKEN after U11 application \
             — Phase 4c-prime-post did not mark it. flags: {:?}",
            metadata.get_flags(*nid)
        );
    }
}

/// `phase4_post_unification_address_taken` — DAG acceptance criterion 1
/// (cross-file two-definitions variant).
///
/// Both files define `cb_alpha`; only file A takes the address. Per SPEC
/// §3.1.2 ("is this function ever address-taken?"), every matching
/// canonical NodeId must be marked, so both definitions surface as
/// address-taken even though only one site took the address.
///
/// Phase 4c-prime only unifies nodes whose `entry.qualified_name` is
/// `Some(_)` (see `parallel_commit.rs:867`). C functions like
/// `cb_alpha` have bare names where `semantic_name == canonical_qualified_name`
/// and therefore leave `qualified_name = None` (see
/// `helper.rs:853-856` and `sqry-lang-c/.../graph_builder.rs:230`).
/// The U11 lookup MUST fall back to `by_name` to find these nodes,
/// otherwise the mark never lands — this test is precisely the
/// regression guard for the `by_qualified_name`-only path.
#[test]
fn phase4_post_unification_address_taken_two_definitions() {
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        "void cb_alpha(void) {}\n\
         void (*p_alpha)(void) = &cb_alpha;\n",
    )
    .expect("write a.c");

    let file_b: PathBuf = root.join("b.c");
    fs::write(&file_b, "void cb_alpha(void) {}\n").expect("write b.c");

    let graph = build_c_only_workspace(root);

    let cb_alpha_ids = resolve_c_function_node_ids(&graph, "cb_alpha");
    assert!(
        !cb_alpha_ids.is_empty(),
        "post-build: cb_alpha must resolve to at least one NodeId; got 0"
    );

    let metadata = graph.macro_metadata();
    for nid in &cb_alpha_ids {
        assert!(
            metadata.is_address_taken(*nid),
            "cb_alpha NodeId {nid:?} must carry NodeFlags::ADDRESS_TAKEN; \
             SPEC §3.1.2 mandates every match be marked on cross-file ambiguity"
        );
    }
}

/// `struct_field_fnptr_merge_across_files` — DAG acceptance criterion 2.
///
/// File A declares `struct file_operations { int (*read)(void); };` so
/// the C plugin's signature builder stages a `(struct_tag, field_name,
/// signature)` triple. File B initialises an instance, exercising the
/// cross-file commit + apply path.
///
/// Asserts the merged `struct_field_fnptr` map contains the
/// `(file_operations, read)` key (interned through the canonical
/// post-Phase-4a graph interner). The struct tag is the bare tree-sitter
/// name as collected by `handle_struct_specifier`
/// (graph_builder.rs:1146); the plugin's signature emission path
/// (`push_struct_field_fnptr_signature`, graph_builder.rs:2261) does not
/// add the `"struct "` prefix.
#[test]
fn struct_field_fnptr_merge_across_files() {
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    fs::write(
        root.join("a.c"),
        "struct file_operations {\n  int (*read)(void);\n};\n",
    )
    .expect("write a.c");

    fs::write(
        root.join("b.c"),
        "int do_read(void) { return 0; }\n\
         struct file_operations ops = { .read = &do_read };\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let tables = graph
        .c_indirect_tables()
        .expect("workspace has C plugin-staged side tables");

    let strings = graph.strings();
    // Find the (struct_tag_id, field_name_id) key whose two legs resolve
    // to "struct file_operations" and "read".
    let found = tables.struct_field_fnptr.keys().any(|(st_id, fn_id)| {
        let st = strings.resolve(*st_id);
        let fn_ = strings.resolve(*fn_id);
        st.as_deref() == Some("file_operations") && fn_.as_deref() == Some("read")
    });
    assert!(
        found,
        "post-build: struct_field_fnptr must contain a key whose legs resolve to \
         (\"file_operations\", \"read\"). Got keys: {:?}",
        tables
            .struct_field_fnptr
            .keys()
            .map(|(s, f)| (
                strings
                    .resolve(*s)
                    .map(|a| a.to_string())
                    .unwrap_or_default(),
                strings
                    .resolve(*f)
                    .map(|a| a.to_string())
                    .unwrap_or_default(),
            ))
            .collect::<Vec<_>>()
    );
}

/// `bindings_by_field_cross_file_merge` — DAG acceptance criterion 3.
///
/// File A declares the struct AND defines the target function. File B
/// declares an instance with a designated initializer binding the field
/// to that function. The merged `bindings_by_field` table must contain
/// exactly one entry under the `(struct_tag_id, field_name_id)` key —
/// the order of file emission must NOT produce multiple entries for the
/// same logical binding.
#[test]
fn bindings_by_field_cross_file_merge() {
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    fs::write(
        root.join("a.c"),
        "struct ops_table {\n  int (*read)(void);\n};\n\
         int real_read(void) { return 42; }\n",
    )
    .expect("write a.c");

    fs::write(
        root.join("b.c"),
        // The plugin needs to see the struct + target visible from this
        // TU to classify the binding correctly — include the header-like
        // forward declarations directly. The designated-initializer
        // value MUST be a bare identifier (`real_read`), not
        // `&real_read`, because `extract_initializer_list_targets`
        // (graph_builder.rs:947) only captures bindings when the RHS
        // node kind is `identifier`. The `&foo` form is handled
        // separately by `classify_unary_amp` for address-taken marking,
        // not for binding-table population.
        "struct ops_table {\n  int (*read)(void);\n};\n\
         extern int real_read(void);\n\
         struct ops_table the_ops = { .read = real_read };\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let tables = graph
        .c_indirect_tables()
        .expect("workspace has C plugin-staged side tables");

    let strings = graph.strings();
    let mut matching_keys: Vec<((u32, u32), usize)> = Vec::new();
    for ((st_id, fn_id), entries) in &tables.bindings_by_field {
        let st = strings.resolve(*st_id);
        let fn_ = strings.resolve(*fn_id);
        if st.as_deref() == Some("ops_table") && fn_.as_deref() == Some("read") {
            matching_keys.push(((st_id.index(), fn_id.index()), entries.len()));
        }
    }

    assert_eq!(
        matching_keys.len(),
        1,
        "post-build: bindings_by_field must contain exactly ONE \
         (struct_tag_id, field_name_id) key for (\"ops_table\", \"read\") — \
         got {} key(s): {:?}",
        matching_keys.len(),
        matching_keys
    );

    let (_, entry_count) = matching_keys[0];
    assert!(
        entry_count >= 1,
        "post-build: matching bindings_by_field key must hold at least one BindingEntry \
         for the .read = &real_read designation; got {entry_count}"
    );
}

/// Resolve every CALL_COMPATIBLE-kind NodeId matching `name`, partitioned
/// by the owning file's language. Used by the negative regression test
/// below to assert that the U11 by_name fallback constrains marks to C
/// nodes only, independent of which file the address-take site lives in.
fn resolve_callable_nodes_by_language(
    graph: &CodeGraph,
    name: &str,
) -> std::collections::HashMap<Option<Language>, Vec<NodeId>> {
    let mut by_lang: std::collections::HashMap<Option<Language>, Vec<NodeId>> =
        std::collections::HashMap::new();
    let Some(str_id) = graph.strings().get(name) else {
        return by_lang;
    };
    let by_qn = graph.indices().by_qualified_name(str_id).to_vec();
    let by_nm = graph.indices().by_name(str_id).to_vec();
    let mut seen: std::collections::HashSet<(u32, u64)> = std::collections::HashSet::new();
    let arena = graph.nodes();
    let files = graph.files();
    for nid in by_qn.into_iter().chain(by_nm.into_iter()) {
        if !seen.insert((nid.index(), nid.generation())) {
            continue;
        }
        let Some(entry) = arena.get(nid) else {
            continue;
        };
        match entry.kind {
            NodeKind::Function
            | NodeKind::Method
            | NodeKind::Macro
            | NodeKind::Constant
            | NodeKind::LambdaTarget => {}
            _ => continue,
        }
        let lang = files.language_for_file(entry.file);
        by_lang.entry(lang).or_default().push(nid);
    }
    by_lang
}

/// `phase4_post_unification_address_taken_skips_non_c_namesake` — codex
/// iter-1 MAJOR regression guard for the C-language scope on U11's
/// by_name fallback.
///
/// Before the iter-1 fix, the workspace-global `by_name` fallback unioned
/// every same-named callable across the entire workspace and filtered
/// only by `CALL_COMPATIBLE_KINDS`, so a Rust `fn cb_alpha` sharing the
/// bare name with a C `cb_alpha` would be silently swept into the
/// address-taken set whenever the C side staged an address-take. This
/// violated the SPEC §3.1.2 line 163 contract ("Every C
/// `NodeKind::Function`...") and the DESIGN §8.2 "(function_qualified_name,
/// file_id)" deferred-payload mandate.
///
/// This test pins the corrected behaviour from both directions:
///
/// 1. **C address-take side** — when the C file takes the address of
///    `cb_alpha`, only the C `cb_alpha` definition is marked
///    address-taken. The Rust `cb_alpha` definition (which is itself
///    address-taken from Rust source, but that's out of scope for
///    Phase A) remains unmarked by Phase A's U11 mark application,
///    because Phase A's marking pipeline is C-scoped.
///
/// 2. **Rust address-take side** — same construction, but with the
///    address-take site living only on the Rust side. The Rust plugin
///    does not stage a `CIndirectStagingPayload`, so U11's drain is
///    empty for Rust files; the C `cb_alpha` is NOT marked even
///    though a same-named function elsewhere is address-taken in
///    another language.
///
/// Subscribers to `NodeFlags::ADDRESS_TAKEN` should therefore see
/// exactly the C node flagged when the address-take site is in C, and
/// exactly nothing flagged from U11 when the address-take site is only
/// in Rust.
#[test]
fn phase4_post_unification_address_taken_skips_non_c_namesake() {
    // ---- Scenario 1: C address-take, Rust namesake must NOT be marked.
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    fs::write(
        root.join("a.c"),
        // C file: defines cb_alpha AND takes its address. U11 must
        // mark this specific C `cb_alpha` as address-taken.
        "void cb_alpha(void) {}\n\
         void (*p_alpha)(void) = &cb_alpha;\n",
    )
    .expect("write a.c");

    fs::write(
        root.join("lib.rs"),
        // Rust file: defines a same-named function `cb_alpha`. This
        // node MUST NOT be flagged ADDRESS_TAKEN by U11's mark
        // application — U11 is C-scoped per SPEC §3.1.2. (Rust's own
        // address-take semantics are an entirely separate downstream
        // pass and are out of scope for Phase A.)
        "pub fn cb_alpha() {}\n\
         pub fn use_alpha() { let _p: fn() = cb_alpha; }\n",
    )
    .expect("write lib.rs");

    let graph = build_c_and_rust_workspace(root);
    let by_lang = resolve_callable_nodes_by_language(&graph, "cb_alpha");

    let c_nodes: Vec<NodeId> = by_lang.get(&Some(Language::C)).cloned().unwrap_or_default();
    let rust_nodes: Vec<NodeId> = by_lang
        .get(&Some(Language::Rust))
        .cloned()
        .unwrap_or_default();

    assert!(
        !c_nodes.is_empty(),
        "Scenario 1: workspace must contain at least one C `cb_alpha` node \
         (full by-language breakdown: {by_lang:?})"
    );
    assert!(
        !rust_nodes.is_empty(),
        "Scenario 1: workspace must contain at least one Rust `cb_alpha` node \
         (full by-language breakdown: {by_lang:?}) — without this the negative \
         assertion below is vacuous"
    );

    let metadata = graph.macro_metadata();
    for nid in &c_nodes {
        assert!(
            metadata.is_address_taken(*nid),
            "Scenario 1: C `cb_alpha` NodeId {nid:?} MUST be marked ADDRESS_TAKEN \
             (the C file takes &cb_alpha); flags: {:?}",
            metadata.get_flags(*nid)
        );
    }
    for nid in &rust_nodes {
        assert!(
            !metadata.is_address_taken(*nid),
            "Scenario 1 (codex iter-1 regression): Rust `cb_alpha` NodeId {nid:?} \
             MUST NOT be marked ADDRESS_TAKEN by U11 — the by_name fallback must be \
             constrained to C-language nodes per SPEC §3.1.2 line 163 + DESIGN §8.2 \
             lines 1239-1241. flags: {:?}",
            metadata.get_flags(*nid)
        );
    }

    drop(workspace); // release tempdir before next scenario.

    // ---- Scenario 2: Rust address-take only, C namesake must NOT be
    // marked by U11. The U11 drain is empty for Rust files (only the
    // C plugin populates `CIndirectStagingPayload`), so this case
    // exercises the absence of staged C-side marks — the C `cb_alpha`
    // must remain unmarked even though a same-named Rust function is
    // address-taken in Rust source.
    let workspace2 = TempDir::new().expect("tempdir");
    let root2 = workspace2.path();

    fs::write(
        root2.join("a.c"),
        // C file: defines cb_alpha but does NOT take its address.
        "void cb_alpha(void) {}\n",
    )
    .expect("write a.c");

    fs::write(
        root2.join("lib.rs"),
        // Rust file: defines AND address-takes cb_alpha.
        "pub fn cb_alpha() {}\n\
         pub fn use_alpha() { let _p: fn() = cb_alpha; }\n",
    )
    .expect("write lib.rs");

    let graph2 = build_c_and_rust_workspace(root2);
    let by_lang2 = resolve_callable_nodes_by_language(&graph2, "cb_alpha");

    let c_nodes2: Vec<NodeId> = by_lang2
        .get(&Some(Language::C))
        .cloned()
        .unwrap_or_default();
    assert!(
        !c_nodes2.is_empty(),
        "Scenario 2: workspace must contain at least one C `cb_alpha` node \
         (full by-language breakdown: {by_lang2:?})"
    );

    let metadata2 = graph2.macro_metadata();
    for nid in &c_nodes2 {
        assert!(
            !metadata2.is_address_taken(*nid),
            "Scenario 2 (codex iter-1 regression): C `cb_alpha` NodeId {nid:?} \
             MUST NOT be marked ADDRESS_TAKEN by U11 — the C side has no \
             address-take site, and U11's drain only carries C-staged marks. \
             flags: {:?}",
            metadata2.get_flags(*nid)
        );
    }
}

# sqryd Daemon Implementation Plan

> **Historical / non-current.** The sqry-nl / `sqry ask` / `sqry_ask` / ONNX /
> classifier / embedding-model surface mentioned below was removed from sqry
> (see `docs/reviews/sqry-nl-removal/2026-06-14/`). Those mentions are a record
> of past work and do not describe shipped behavior.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `sqryd`, a long-lived daemon that owns code graphs in memory, watches for file changes, and serves LSP/MCP/CLI clients over a shared Unix domain socket.

**Architecture:** Single process with `ArcSwap`-based lock-free reads, LRU eviction by memory budget, hybrid incremental/full rebuild on file changes. Thin stdio↔UDS shims for LSP/MCP. New `sqry-daemon` crate + modifications to sqry-core, sqry-cli, sqry-lsp, sqry-mcp.

**Tech Stack:** Rust 1.94+ (Edition 2024), `arc-swap`, `notify` (file watching), `ignore` (gitignore), `tokio` (async IPC server), `serde`/`serde_json` (JSON-RPC), `flock`/`fs2` (pidfile locking), `toml` (config parsing)

**Spec:** `docs/superpowers/specs/2026-03-19-sqryd-daemon-design.md` (Codex-approved)
**Amendment (MANDATORY):** `docs/superpowers/specs/2026-04-09-sqryd-daemon-design-amendment.md`
— adds per-file node buckets + tombstone compaction to Task 4, tree-hash divergence
check to Task 2, standard `meta` response envelope + `stale_serve_max_age_hours`
cap to Tasks 5/6/8. All sub-steps below marked with **[A2026-04-09]** come from
the amendment and are not optional.

---

## Phase 1: sqry-core Foundations

These tasks add primitives to sqry-core that the daemon depends on. No daemon code yet — just library additions with full test coverage.

---

### Task 1: GraphMemorySize Trait

Add a trait for accurate heap memory tracking on graph data structures.

**Files:**
- Create: `sqry-core/src/graph/unified/memory.rs`
- Modify: `sqry-core/src/graph/unified/mod.rs` (add `pub mod memory;`)
- Modify: `sqry-core/src/graph/unified/storage/arena.rs:359+` (impl trait on NodeArena)
- Modify: `sqry-core/src/graph/unified/storage/csr.rs:54+` (impl trait on CsrGraph)
- Modify: `sqry-core/src/graph/unified/storage/interner.rs:107+` (impl trait on StringInterner)
- Modify: `sqry-core/src/graph/unified/storage/registry.rs:104+` (impl trait on FileRegistry)
- Modify: `sqry-core/src/graph/unified/edge/bidirectional.rs:52+` (impl trait on BidirectionalEdgeStore)
- Modify: `sqry-core/src/graph/unified/concurrent/graph.rs:43+` (impl trait on CodeGraph)
- Test: `sqry-core/src/graph/unified/memory.rs` (inline tests)

- [ ] **Step 1: Write the trait definition and tests**

Create `sqry-core/src/graph/unified/memory.rs`:

```rust
/// Trait for reporting heap memory usage of graph data structures.
/// Implementations should sum `Vec::capacity() * size_of::<T>()` for all
/// heap-allocated collections, plus any Box/Arc overhead.
pub trait GraphMemorySize {
    /// Returns estimated heap bytes owned by this structure (excludes stack/inline).
    fn heap_bytes(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vec_memory_size() {
        // A Vec<u64> with capacity 100 should report 800 bytes
        let v: Vec<u64> = Vec::with_capacity(100);
        assert_eq!(v.capacity() * std::mem::size_of::<u64>(), 800);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p sqry-core graph::unified::memory --lib`

- [ ] **Step 3: Implement GraphMemorySize on NodeArena**

In `sqry-core/src/graph/unified/storage/arena.rs`, add:

```rust
impl GraphMemorySize for NodeArena {
    fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<NodeEntry>()
            + self.generations.capacity() * std::mem::size_of::<u32>()
    }
}
```

- [ ] **Step 4: Implement GraphMemorySize on CsrGraph, BidirectionalEdgeStore, StringInterner, FileRegistry**

Each implementation sums `capacity() * size_of::<T>()` for all internal Vecs and HashMaps. For HashMaps, use `capacity() * (size_of::<K>() + size_of::<V>() + 8)` as an estimate (8 bytes for bucket metadata).

- [ ] **Step 5: Implement GraphMemorySize on CodeGraph**

Sum all sub-component heap_bytes:

```rust
impl GraphMemorySize for CodeGraph {
    fn heap_bytes(&self) -> usize {
        self.nodes().heap_bytes()
            + self.edges().heap_bytes()
            + self.strings().heap_bytes()
            + self.files().heap_bytes()
            + self.indices().heap_bytes()
    }
}
```

- [ ] **Step 6: Add integration test with a real graph**

```rust
#[test]
fn test_codegraph_memory_size_nonzero() {
    let graph = build_test_graph(); // Use existing test helper
    let bytes = graph.heap_bytes();
    assert!(bytes > 0, "graph should report nonzero heap bytes");
    assert!(bytes < 100 * 1024 * 1024, "test graph should be under 100 MB");
}
```

- [ ] **Step 7: Run full test suite and commit**

Run: `cargo test -p sqry-core --lib`
Commit: `feat(core): add GraphMemorySize trait for heap memory tracking`

---

### Task 2: SourceTreeWatcher

A recursive, gitignore-filtered file watcher with git state detection. Distinct from the existing `FileWatcher` in `sqry-core/src/watch.rs`.

**Files:**
- Create: `sqry-core/src/watch/source_tree.rs`
- Create: `sqry-core/src/watch/git_state.rs`
- Modify: `sqry-core/src/watch.rs` → refactor into `sqry-core/src/watch/mod.rs` (re-export existing + new)
- Test: `sqry-core/tests/source_tree_watcher.rs`

- [ ] **Step 1: Refactor watch.rs into watch/ module**

Move `sqry-core/src/watch.rs` to `sqry-core/src/watch/mod.rs`. Add `pub mod source_tree;` and `pub mod git_state;`. Verify existing `FileWatcher` tests still pass.

- [ ] **Step 2: Run tests to verify refactor is clean**

Run: `cargo test -p sqry-core watch`

- [ ] **Step 3: Write GitStateWatcher**

Create `sqry-core/src/watch/git_state.rs`:

```rust
/// Watches .git/ internal state for branch switches, ref updates, and index changes.
/// Any git state change signals a "large change" requiring full rebuild.
pub struct GitStateWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Result<notify::Event, notify::Error>>,
}

impl GitStateWatcher {
    /// Start watching git internals at the given repo root.
    /// Watches: .git/HEAD, .git/refs/heads/, .git/packed-refs, .git/index
    pub fn new(repo_root: &Path) -> notify::Result<Self> { ... }

    /// Returns true if any git state change has occurred since last check.
    /// Drains all pending events.
    pub fn poll_changed(&self) -> bool { ... }
}
```

- [ ] **Step 4: Write tests for GitStateWatcher**

```rust
#[test]
fn test_git_state_detects_head_change() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let watcher = GitStateWatcher::new(dir.path()).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(watcher.poll_changed());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p sqry-core watch::git_state`

- [ ] **Step 6: Write SourceTreeWatcher**

Create `sqry-core/src/watch/source_tree.rs`:

```rust
/// Recursive source-tree watcher with .gitignore filtering and git state detection.
/// Unlike FileWatcher (index-file invalidation), this watches the full source tree.
pub struct SourceTreeWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Result<notify::Event, notify::Error>>,
    root: PathBuf,
    ignore_matcher: ignore::gitignore::Gitignore,
    git_state: GitStateWatcher,
}

/// Result of waiting for file changes with debounce.
pub struct ChangeSet {
    pub changed_files: Vec<PathBuf>,
    pub git_state_changed: bool,
}

impl SourceTreeWatcher {
    pub fn new(root: &Path) -> Result<Self> { ... }
    /// Sliding-window debounce: waits for `debounce` duration of quiet after last event.
    pub fn wait_for_changes(&self, debounce: Duration) -> Result<ChangeSet> { ... }
    /// Non-blocking poll.
    pub fn poll_changes(&self) -> Result<Option<ChangeSet>> { ... }
}
```

- [ ] **Step 7: Write integration tests for SourceTreeWatcher**

Test: create temp dir with .gitignore, write files, verify only non-ignored files appear in changeset. Test sliding debounce behavior.

- [ ] **Step 8: [A2026-04-09] Tree-hash divergence discrimination**

Replace the naive "any `.git/` event → full rebuild" signal from Step 3 with the
classifier defined in §B of the 2026-04-09 amendment.

Add to `GitStateWatcher`:

```rust
pub struct LastIndexedGitState {
    pub head_tree_oid: Option<String>,
    pub head_ref: Option<String>,
}

pub enum GitChangeClass {
    BranchSwitch,      // .git/HEAD content changed
    TreeDiverged,      // refs/heads/<current> moved AND HEAD^{tree} != last.head_tree_oid
    LocalCommit,       // refs/heads/<current> moved AND HEAD^{tree} == last.head_tree_oid
    Noise,             // packed-refs / index / logs / MERGE_* etc.
}

impl GitStateWatcher {
    pub fn classify(&self, last: &LastIndexedGitState) -> GitChangeClass { ... }
}
```

Only `BranchSwitch` and `TreeDiverged` signal a full rebuild. `LocalCommit` and
`Noise` produce no rebuild (the source-tree watcher already handled any working-
tree edits that preceded the commit).

Tree-hash lookup uses `git rev-parse HEAD^{tree}` via `std::process::Command`.
On any uncertainty (git command fails, unreadable refs) fall back to
`BranchSwitch` (full rebuild) — correctness over optimization.

Support `.git` as a file (worktrees) by resolving the real gitdir before
attaching.

- [ ] **Step 9: [A2026-04-09] Tests for git change classification**

- Commit test: modify a file, `git commit -am`, assert the watcher reports
  `LocalCommit` (no rebuild triggered by git events themselves).
- Checkout test: `git checkout` to a branch with a different tree, assert
  `BranchSwitch` or `TreeDiverged`.
- GC test: `git gc`, assert `Noise`.
- Staging test: `git add` + `git reset`, assert `Noise`.
- Worktree test: repo with `.git` as a file pointing to an external gitdir,
  verify watcher attaches correctly.

- [ ] **Step 9b: [A2 §I] Editor save pattern matrix + Windows rename coalescing pass**

(from Amendment 2 §I, approved 2026-04-09 — pre-implementation gate item 5: this step must be complete before Task 2 is marked done.)

Add `sqry-core/tests/support/editor_patterns.rs`:

```rust
pub enum EditorSavePattern {
    DirectWrite,         // std::fs::write
    VimAtomicRename,     // write .foo.swp, rename over foo
    JetBrainsAtomicSave, // write tmp, fsync, rename
    VscodeSafeSave,      // rename original to .bak, write new, delete .bak
    EmacsBackup,         // write foo, leave foo~ behind
}

pub fn simulate_save(path: &Path, content: &[u8], pattern: EditorSavePattern);
```

**Cross-platform contract (normalized outcomes).** All five patterns MUST normalize to "exactly one logical changed file in the debounced `ChangeSet`" on Linux, macOS, and Windows. Per-OS expected outcomes:

| Pattern | Linux | macOS | Windows |
|---|---|---|---|
| `DirectWrite` | 1 event, 1 changed file | same | same |
| `VimAtomicRename` | 1 event after debounce, 1 changed file (swap file filtered by gitignore/pattern rule) | same | same (ReadDirectoryChangesW reports rename as remove+create; watcher must coalesce) |
| `JetBrainsAtomicSave` | 1 event, 1 changed file | 1 event, 1 changed file (Rename-only events coalesced with preceding Create) | same |
| `VscodeSafeSave` | 1 event, 1 changed file, `.bak` never surfaces | same | same |
| `EmacsBackup` | 1 event for `foo`; `foo~` filtered by gitignore rule | same | same |

If a platform's native watcher fundamentally cannot distinguish a pattern, the watcher implementation MUST add an explicit coalescing pass — there is no "document and degrade" escape hatch. The Windows rename coalescing pass (collapsing remove+create on the same path within the debounce window into a single logical modify) is part of this step and is unit-tested in isolation.

**Bulk git scenarios (watcher-level assertions).**
- `git checkout` across a branch with 100+ file diffs: exactly one debounced `ChangeSet`; git-state classifier returns `BranchSwitch` or `TreeDiverged`.
- `git stash` → `git stash pop`: two debounced `ChangeSet`s.
- `git gc`: zero events that survive classification (all `Noise`).
- `git commit` of a previously-edited file: zero *additional* `ChangeSet`s beyond the one already produced by the original edit.

**No `std::fs::write`-only coverage.** Any watcher test that exercises only `DirectWrite` is incomplete and must be paired with at least one editor-pattern test covering the same scenario. Test module headers carry a mandatory cross-reference list enforced by review.

All five patterns × three OSes pass on CI (Linux, macOS, Windows).

- [ ] **Step 10: Run full watch module tests and commit**

Run: `cargo test -p sqry-core watch`
Commit: `feat(core): add SourceTreeWatcher with gitignore filtering and tree-divergence git state detection`

---

### Task 3: Reverse-Dependency Index

Add a `reverse_import_index` method to CodeGraph for incremental rebuild closure computation.

**Files:**
- Modify: `sqry-core/src/graph/unified/concurrent/graph.rs:43+`
- Create: `sqry-core/src/graph/unified/build/incremental.rs`
- Modify: `sqry-core/src/graph/unified/build/mod.rs` (add `pub mod incremental;`)
- Test: `sqry-core/src/graph/unified/build/incremental.rs` (inline tests)

- [ ] **Step 1: Add reverse_import_index to CodeGraph**

```rust
impl CodeGraph {
    /// Returns FileIds of all files that import symbols exported by the given file.
    /// Derived from Pass 4 cross-file Imports edges.
    pub fn reverse_import_index(&self, file_id: FileId) -> Vec<FileId> {
        // Iterate edges, find Imports edges where target node's file == file_id
        // Return source node's file (deduplicated)
    }
}
```

- [ ] **Step 2: Write test for reverse_import_index**

Use a multi-file test fixture where file A imports from file B. Verify `reverse_import_index(B)` returns `[A]`.

- [ ] **Step 3: Write compute_reverse_dep_closure**

Create `sqry-core/src/graph/unified/build/incremental.rs`:

```rust
/// Compute the transitive reverse-dependency closure for a set of changed files.
/// Returns all files that directly or transitively import from the changed files.
pub fn compute_reverse_dep_closure(
    changed_files: &[FileId],
    graph: &CodeGraph,
) -> HashSet<FileId> {
    let mut closure = HashSet::from_iter(changed_files.iter().copied());
    let mut frontier: VecDeque<FileId> = changed_files.iter().copied().collect();
    while let Some(file_id) = frontier.pop_front() {
        for importer in graph.reverse_import_index(file_id) {
            if closure.insert(importer) {
                frontier.push_back(importer);
            }
        }
    }
    closure
}
```

- [ ] **Step 4: Write tests for closure computation**

Test: A→B→C import chain. Change C → closure should be {A, B, C}. Change A → closure should be {A} only.

- [ ] **Step 5: Write incremental_rebuild function skeleton**

```rust
/// Perform an incremental rebuild for the given changed files.
/// Returns a new CodeGraph with updated nodes/edges for affected files.
pub fn incremental_rebuild(
    current_graph: &CodeGraph,
    changed_files: &[PathBuf],
    closure: &HashSet<FileId>,
    plugins: &PluginManager,
    config: &BuildConfig,
) -> GraphResult<CodeGraph> {
    // 1. Clone current graph
    // 2. Remove nodes/edges for closure files
    // 3. Re-parse closure files (Pass 1-3)
    // 4. Rebuild ExportMap for closure files
    // 5. Re-run Pass 4 for closure files
    // 6. Re-run Pass 5 if FFI/HTTP markers present
    // 7. Rebuild analysis artifacts (CSR, SCC)
    todo!("Implementation in Task 4")
}
```

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p sqry-core graph::unified::build::incremental`
Commit: `feat(core): add reverse-dependency closure computation for incremental rebuild`

---

### Task 4: Incremental Rebuild Engine

Wire up the full incremental rebuild pipeline.

**Files:**
- Modify: `sqry-core/src/graph/unified/build/incremental.rs`
- Modify: `sqry-core/src/graph/unified/concurrent/graph.rs` (add node/edge removal by file)
- Create: `sqry-core/src/graph/unified/rebuild.rs` (`RebuildGraph`, `clone_for_rebuild`, `finalize`)
- Create: `sqry-core/src/graph/unified/rebuild/coverage.rs` (`NodeIdBearing` `assert_impl_all!` matrix)
- Test: `sqry-core/tests/incremental_rebuild.rs`
- Test: `sqry-core/tests/incremental_equivalence.rs` ([A2 §E] property harness, CI gate)
- Test: `sqry-core/tests/rebuild_graph_compile_fail/` ([A2 §H] trybuild fixtures)

**[A2 PRE-IMPLEMENTATION GATE]** Per Amendment 2 §Pre-Implementation Gate, this task has hard sequencing requirements that override the natural step order:
1. §E equivalence harness must exist, run green against a stub `incremental_rebuild` that delegates to `full_rebuild`, and detect a planted bug **before** the real engine is implemented (Step 0a).
2. §K `NodeIdBearing` trait + `coverage.rs` matrix must exist before `finalize()` is implemented (Step 0b).
3. §H `RebuildGraph` type with the complete field partition + compile-time exhaustiveness must exist before any `clone_for_rebuild` caller (Step 0c).
4. §F bijection + tombstone-residue invariants must be wired into every publish site (Step 0d, then enforced in finalize Step 4b).

A Task 4 commit that lacks any of the above is incomplete and must be rejected in review.

- [ ] **Step 0a: [A2 §E] Property-based semantic equivalence harness (lands FIRST, against a stub)**

(from Amendment 2 §E, approved 2026-04-09 — pre-implementation gate item 1.)

Create `sqry-core/tests/incremental_equivalence.rs`. CI gate, no `#[ignore]`.

Implement the semantic-key comparator `assert_graph_semantically_equivalent(graph_a, graph_b)`. It compares (as unordered sets):
- **Node set**: `{ NodeSemKey from graph_a } == { NodeSemKey from graph_b }`.
- **Edge set**: `{ EdgeSemKey from graph_a } == { EdgeSemKey from graph_b }`.
- **Per-file bucket content**: for every file, the set of `NodeSemKey`s in that file matches.
- **Export map**: `{ (file_path, exported_symbol_qualified_name) }` sets match.
- **Cross-language edges**: `EdgeSemKey` set filtered to Pass 5 edge kinds matches.
- **String interner reachability**: every `StringId` referenced by either graph's live nodes/edges resolves to the same string under both graphs' snapshots. Raw `StringId` values are NOT compared.

Raw `NodeId`, `EdgeId`, arena slot index, CSR row offset, and interner `StringId` values are explicitly NOT compared. The comparator is order-independent, rebuild-strategy-independent, and stable.

**Per-step equivalence property** — state is carried forward and asserted after every edit:

```rust
fn prop_incremental_matches_full(fixture: Fixture, edits: Vec<EditOp>) {
    let mut fixture_for_incr = fixture.clone();
    let mut fixture_for_full = fixture.clone();
    let mut graph_incr = full_rebuild(&fixture_for_incr).expect("baseline");
    for edit in edits {
        edit.apply(&mut fixture_for_incr);
        edit.apply(&mut fixture_for_full);

        let graph_full = full_rebuild(&fixture_for_full);
        let graph_incr_next = incremental_rebuild(&graph_incr, &[edit.clone()]);

        match (graph_incr_next, graph_full) {
            (Ok(a), Ok(b)) => {
                assert_graph_semantically_equivalent(&a, &b);
                graph_incr = a;
            }
            (Err(ea), Err(eb)) => {
                assert_build_errors_equivalent(&ea, &eb);
            }
            (ok, err) => panic!(
                "incremental/full divergence on edit {edit:?}: {ok:?} vs {err:?}"
            ),
        }
    }
}
```

`assert_build_errors_equivalent` compares error kind and the set of affected files — not exact message strings. This is the "both paths fail the same way" assertion.

**Grammar-aware edit generator (closed operator catalog).** Edits come from a fixture-aware operator set, not random byte noise. Proptest shrinks within this set, not over raw bytes:

| Operator | Validity | Targets |
|---|---|---|
| `AddFunction { file, name, body }` | valid | Pass 1-3 |
| `RemoveFunction { file, name }` | valid | Pass 1-3 + reverse-dep closure |
| `RenameSymbol { file, from, to }` | valid | closure + export map |
| `AddImport { file, target }` | valid | Pass 4 cross-file |
| `RemoveImport { file, target }` | valid | Pass 4 cross-file |
| `AddExternBlock { file, decl }` | valid | Pass 5 FFI |
| `AddHttpRoute { file, route }` | valid | Pass 5 HTTP |
| `AddFile { path, content }` | valid | full Pass 1-5 |
| `RemoveFile { path }` | valid | closure cleanup |
| `RenameFile { from, to }` | valid | closure + import fixups |
| `WhitespaceEdit { file, byte_range }` | valid no-op | debounce / bucket stability |
| `InvalidSyntaxEdit { file }` | invalid | error-equivalence path |

Each fixture declares which operators are valid for its languages; proptest samples from the declared subset.

**Fixture set:** `rust_small`, `multi_lang_ffi`, `ts_http_routes`, `java_enterprise`, `monorepo_mixed`. Checked into `sqry-core/tests/fixtures/incremental/`.

**CI budgets:**
- Default: 256 proptest cases × 5 fixtures × edit sequences of length 1..=20.
- Nightly: 4096 cases × edit sequences up to length 50.

**Stub-first requirement.** The harness file must exist and run green against a stub `incremental_rebuild` that simply delegates to `full_rebuild` BEFORE the real engine in Step 4 is implemented. A planted bug must be exercised once during development and verified to be caught by the harness; the bug is then reverted. Both the stub-green run and the planted-bug catch are required artifacts of this step.

- [ ] **Step 0b: [A2 §K] `NodeIdBearing` trait + master compaction coverage matrix**

(from Amendment 2 §K, approved 2026-04-09 — pre-implementation gate item 7.)

Create `sqry-core/src/graph/unified/rebuild/coverage.rs`:

```rust
pub trait NodeIdBearing {
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_>;
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool);
}
```

**K.A — Current publish-visible NodeId-bearing structures (must always be compacted):**

| # | Structure | Field on `CodeGraph` | Compaction method |
|---|---|---|---|
| 1 | Node arena | `nodes` | Tombstone flag + generation bump in `compact_tombstoned` |
| 2 | Forward edges | `edges` (forward adjacency) | `retain_nodes` linear filter |
| 3 | Reverse edges | `edges` (reverse adjacency) | `retain_nodes` linear filter |
| 4 | Kind index | `indices.kind_index` | `retain_nodes` per BTreeMap value |
| 5 | Name index | `indices.name_index` | `retain_nodes` linear filter |
| 6 | Qualified-name index | `indices.qualified_name_index` | `retain_nodes` linear filter |
| 7 | File index | `indices.file_index` | `retain_nodes` linear filter (actual field name; rev 2 incorrectly called it `file_symbol_index`) |
| 8 | Macro metadata store | `macro_metadata` | `NodeMetadataStore::retain_nodes` |
| 9 | CSR adjacency | derived from `edges` | Rebuilt from compacted edges in finalize step 9; never mutated in place |
| 10 | Node provenance store | `node_provenance` | `NodeProvenanceStore::retain_nodes` — clears slots whose reconstructed `NodeId` fails `keep`; dense slot alignment with the arena is preserved |
| 11 | Scope arena | `scope_arena` | `ScopeArena::retain_nodes` — frees every scope whose introducing `Scope.node` fails `keep` (advances slot generation) |
| 12 | Alias table | `alias_table` | `AliasTable::retain_nodes` — drops every entry whose `import_node` fails `keep`, reassigns dense `AliasEntryId`s, rebuilds `by_scope` range index |
| 13 | Shadow table | `shadow_table` | `ShadowTable::retain_nodes` — drops every entry whose `node` fails `keep`, reassigns dense `ShadowEntryId`s, rebuilds `chains` range index |

**Other `CodeGraph` fields that are intentionally NOT in K.A.** These are publish-visible but hold no `NodeId` payload (or are build-time scratch that is always drained before publish), so they do not need a `NodeIdBearing` impl:

- `strings: Arc<StringInterner>` — keyed by `StringId`.
- `edge_provenance: Arc<EdgeProvenanceStore>` — keyed by `EdgeId`.
- `scope_provenance_store: Arc<ScopeProvenanceStore>` — keyed by `ScopeId`.
- `file_segments: Arc<FileSegmentTable>` — holds `(start_slot, slot_count)` ranges.
- `c_indirect_tables: Option<CIndirectSideTables>` — Phase A C indirect-call side tables (U09); inner maps key on `NodeId`/`StringId`/`FileId` for the resolver's binding-plane + type-match lookups but do NOT carry NodeId payloads onto the publish boundary. `pass5b_c_indirect_resolve` consumes these tables in-pass to emit precise `Calls` edges; the tables themselves are not walked by Gate 0b's K.A NodeId compaction. If a future Phase B variant routes NodeId-bearing extensions through this slot, add a K.A row in the same commit.
- `fact_epoch: u64`, `epoch: u64`, `confidence: HashMap<String, _>` — scalars / per-language metadata.
- `go_hints: GoHints` — build-time scratch side-channel populated by the Go plugin's Phase-1 parse and drained by the post-Phase-4e `pass_go_method_set_satisfaction` pass before Pass 5 runs (see `docs/development/go-implements-and-promotion/02_DESIGN.md` §3.2 + §6). Holds `NodeId`s only between Phase 1 and Pass 5; never reaches the publish boundary with un-drained hints, so the Gate 0c finalize-residue check does not need to enumerate it.

If any of those fields ever gains a `NodeId`-bearing payload (e.g., `ScopeProvenanceStore` adds the introducing node), a new K.A row must be added to this table and to `coverage.rs` in the same commit.

**K.B — Structures introduced by the daemon plan (added in the same commit as the structure itself):**

| # | Structure | Added by | Compaction method |
|---|---|---|---|
| B1 | `FileRegistry.per_file_nodes` bucket | Amendment 1 §A (Task 4 Step 1) | `take_nodes` at remove, `record_node` at build; `retain_nodes` in finalize as a guard |
| B2 | Reverse-import index | Task 3 | `retain_nodes` linear filter |
| B3 | Future Pass 5 persistent link tables | future task | `retain_nodes` linear filter |

Pass 4's `ExportMap` and Pass 5's cross-language link tables are currently build-time only and do NOT live on `CodeGraph`. If a future task promotes them to publish-visible state, they get a row in K.B and a `NodeIdBearing` impl.

**Compile-time enforcement (partial — honest scope):**

```rust
// sqry-core/src/graph/unified/rebuild/coverage.rs
use static_assertions::assert_impl_all;
// K.A entries
assert_impl_all!(NodeArena: NodeIdBearing);
assert_impl_all!(BidirectionalEdgeStore: NodeIdBearing);
assert_impl_all!(AuxiliaryIndices: NodeIdBearing);
assert_impl_all!(NodeMetadataStore: NodeIdBearing);
assert_impl_all!(NodeProvenanceStore: NodeIdBearing);
assert_impl_all!(ScopeArena: NodeIdBearing);
assert_impl_all!(AliasTable: NodeIdBearing);
assert_impl_all!(ShadowTable: NodeIdBearing);
// K.B entries (added as tasks land)
assert_impl_all!(FileRegistry: NodeIdBearing);
// assert_impl_all!(ReverseImportIndex: NodeIdBearing); // Task 3
```

`AuxiliaryIndices::retain_nodes` is the single entry point that internally iterates `kind_index`, `name_index`, `qualified_name_index`, and `file_index`; its visit-all-four behavior is covered by unit tests.

**What `assert_impl_all!` does NOT guarantee:** that the listed set is exhaustive. A new NodeId-bearing field added without a new `assert_impl_all!` entry will compile cleanly. Exhaustiveness is enforced by:
1. **Code-owner rule** on `sqry-core/src/graph/unified/concurrent/graph.rs`, `storage/indices.rs`, and `rebuild/coverage.rs`: any PR touching these files needs explicit reviewer sign-off confirming K.A/K.B and `coverage.rs` are in sync.
2. **PR template checklist** for the above files: added field to K.A or K.B; added `assert_impl_all!` entry; added `retain_nodes` call in `finalize()`; extended `assert_no_tombstone_residue_for`; added a `retain_nodes` unit test for the new type.
3. **CI grep** counting `assert_impl_all!` entries in `coverage.rs` against the row count of K.A + active K.B and failing on divergence.
4. **§E equivalence harness** as the semantic backstop: a stale reference after finalize trips an incremental-vs-full divergence and blocks merge.

**Tests:** trait coverage builds and passes; every K.A/K.B entry has at least one unit test exercising its `retain_nodes`; integration test calls `finalize` with a non-empty tombstone set, asserts §F residue check passes against all listed structures.

**When a future task adds a NodeId-bearing field:** add `impl NodeIdBearing` → add `assert_impl_all!` line → add row to K.B → extend `finalize()` → extend `assert_no_tombstone_residue_for`. Code review for any PR touching `CodeGraph`/`RebuildGraph` verifies all five steps.

- [ ] **Step 0c: [A2 §H] `RebuildGraph` type split + complete `finalize()` contract**

(from Amendment 2 §H, approved 2026-04-09 — pre-implementation gate item 4.)

The current `CodeGraph` has fields: `nodes: Arc<NodeArena>`, `edges: Arc<BidirectionalEdgeStore>`, `strings: Arc<StringInterner>`, `files: Arc<FileRegistry>`, `indices: Arc<AuxiliaryIndices>`, `macro_metadata: Arc<NodeMetadataStore>`, `epoch: u64`, `confidence: HashMap<String, ConfidenceMetadata>`.

**Complete field partition (no placeholder/example fields):**

| Field | Class | Rebuild semantics |
|---|---|---|
| `nodes` | Owned | Deep-cloned via `Arc::make_mut`; rebuild marks tombstones, finalize compacts |
| `edges` | Owned | Deep-cloned; finalize compacts tombstoned endpoints |
| `strings` | CoW snapshot + rebuild-local builder | Rebuild receives an immutable snapshot + a builder seeded from it; finalize freezes builder into a new immutable snapshot |
| `files` | Owned | Deep-cloned; per-file buckets mutated via `take_nodes`/`record_node`; finalize validates bijection |
| `indices` | Owned | Deep-cloned; finalize runs linear tombstone compaction |
| `macro_metadata` | Owned | Deep-cloned; finalize removes entries for tombstoned NodeIds |
| `epoch` | Scalar | Incremented in finalize |
| `confidence` | Owned | Deep-cloned; updated per-language from rebuild results |

**Single-source-of-truth field declaration via macro.** Both `CodeGraph` and `RebuildGraph` are declared from one `macro_rules!` invocation so adding a field extends both atomically:

```rust
macro_rules! sqry_graph_fields {
    ($code_name:ident, $rebuild_name:ident) => {
        #[derive(Clone)]
        pub struct $code_name {
            pub(crate) nodes: Arc<NodeArena>,
            pub(crate) edges: Arc<BidirectionalEdgeStore>,
            pub(crate) strings: Arc<StringInterner>,
            pub(crate) files: Arc<FileRegistry>,
            pub(crate) indices: Arc<AuxiliaryIndices>,
            pub(crate) macro_metadata: Arc<NodeMetadataStore>,
            pub(crate) epoch: u64,
            pub(crate) confidence: HashMap<String, ConfidenceMetadata>,
        }

        pub struct $rebuild_name {
            pub(crate) nodes: NodeArena,
            pub(crate) edges: BidirectionalEdgeStore,
            pub(crate) string_snapshot: Arc<StringInterner>,
            pub(crate) string_builder: StringInternerBuilder,
            pub(crate) files: FileRegistry,
            pub(crate) indices: AuxiliaryIndices,
            pub(crate) macro_metadata: NodeMetadataStore,
            pub(crate) prior_epoch: u64,
            pub(crate) confidence: HashMap<String, ConfidenceMetadata>,
            /// Active tombstone set during finalize steps 2–7.
            pub(crate) tombstones: HashSet<NodeId>,
            /// Snapshot of tombstones taken at step 8, kept for the
            /// debug residue check in step 14.
            pub(crate) drained_tombstones: HashSet<NodeId>,
        }
    };
}

sqry_graph_fields!(CodeGraph, RebuildGraph);
```

**Compile-time exhaustiveness via destructuring.** `clone_for_rebuild` exhaustively destructures `Self`; missing field is a hard compile error:

```rust
impl CodeGraph {
    fn clone_for_rebuild_inner(&self) -> RebuildGraph {
        let Self {
            nodes, edges, strings, files, indices, macro_metadata, epoch, confidence,
        } = self;
        RebuildGraph {
            nodes: (**nodes).clone(),
            edges: (**edges).clone(),
            string_snapshot: strings.clone(),
            string_builder: StringInternerBuilder::from_snapshot(strings),
            files: (**files).clone(),
            indices: (**indices).clone(),
            macro_metadata: (**macro_metadata).clone(),
            prior_epoch: *epoch,
            confidence: confidence.clone(),
            tombstones: HashSet::new(),
            drained_tombstones: HashSet::new(),
        }
    }
}
```

This is genuine compile-time enforcement in stable Rust — no reflection or proc-macro dependency.

**`finalize()` contract — 14 ordered steps:**

```rust
impl RebuildGraph {
    pub fn finalize(mut self) -> GraphResult<CodeGraph> {
        // 1. Freeze the rebuild's interner builder.
        let new_strings = self.string_builder.freeze();
        // 2. Compact NodeArena: mark tombstoned slots dead, update live_node_count.
        self.nodes.compact_tombstoned(&self.tombstones);
        // 3. Compact BidirectionalEdgeStore (forward + reverse) via NodeIdBearing.
        self.edges.retain_nodes(&|nid| !self.tombstones.contains(&nid));
        // 4. Compact every field of AuxiliaryIndices (kind/name/qualified_name/file_index).
        self.indices.retain_nodes(&|nid| !self.tombstones.contains(&nid));
        // 5. Compact NodeMetadataStore.
        self.macro_metadata.retain_nodes(&|nid| !self.tombstones.contains(&nid));
        // 6. Compact FileRegistry.per_file_nodes buckets.
        self.files.retain_nodes(&|nid| !self.tombstones.contains(&nid));
        // 7. Compact §K-listed additional NodeId-bearing structures added by later
        //    tasks (reverse_import_index, future Pass 5 link tables). Enforced via
        //    NodeIdBearing trait coverage (§K).
        // 8. Drain tombstones into drained_tombstones for the step-14 residue check.
        self.drained_tombstones = std::mem::take(&mut self.tombstones);
        // 9. Rebuild CSR adjacency from the compacted edge store. CSR is derived,
        //    never mutated in place.
        let csr_cache = self.edges.rebuild_csr();
        // 10. Update per-language confidence from rebuild-local data.
        // 11. Increment epoch.
        let new_epoch = self.prior_epoch + 1;
        // 12. Assemble the immutable CodeGraph.
        let graph = CodeGraph {
            nodes: Arc::new(self.nodes),
            edges: Arc::new(self.edges.with_csr_cache(csr_cache)),
            strings: Arc::new(new_strings),
            files: Arc::new(self.files),
            indices: Arc::new(self.indices),
            macro_metadata: Arc::new(self.macro_metadata),
            epoch: new_epoch,
            confidence: self.confidence,
        };
        // 13. (debug) Bucket bijection check (§F.1) on assembled graph.
        #[cfg(any(debug_assertions, test))]
        graph.assert_bucket_bijection();
        // 14. (debug) Tombstone residue check (§F.2) — SINGLE call site —
        //     uses the drained set from step 8.
        #[cfg(any(debug_assertions, test))]
        graph.assert_no_tombstone_residue_for(&self.drained_tombstones);
        Ok(graph)
    }
}
```

In release builds, steps 13 and 14 compile out.

**Type-enforced publish path.** `ArcSwap<CodeGraph>::store` only accepts `Arc<CodeGraph>`. The only Rust path from `RebuildGraph` to `Arc<CodeGraph>` is `RebuildGraph::finalize().map(Arc::new)`. A compile-fail (`trybuild`) test verifies no public API constructs a `CodeGraph` from a `RebuildGraph` otherwise.

**Interner growth bound — explicit compaction trigger.** Every `LoadedWorkspace` tracks `interner_live_ratio: f32 = live_string_count / snapshot_string_count`, computed at the end of every successful `finalize()`. When `interner_live_ratio < interner_compaction_threshold` (config, default `0.5`), the next debounce tick triggers a **mandatory full rebuild** regardless of incremental eligibility. This is a housekeeping rebuild scheduled via the rebuild dispatcher like any other. Persisted snapshots store only the compacted interner.

**Placement and feature gate for `clone_for_rebuild`.** `clone_for_rebuild` and the `RebuildGraph` type are gated behind a cargo feature `rebuild-internals` on `sqry-core`:

```toml
# sqry-core/Cargo.toml
[features]
default = []
rebuild-internals = []
```

```rust
// sqry-core/src/graph/unified/rebuild.rs
#[cfg(feature = "rebuild-internals")]
pub use self::rebuild_graph::{clone_for_rebuild, RebuildGraph};
```

`sqry-daemon/Cargo.toml` enables the feature; with cargo resolver v2, features are not unified across workspace members that do not enable them, so `sqry-cli` (which does not enable it) cannot resolve `clone_for_rebuild` or `RebuildGraph`. Cargo does NOT *reserve* the feature to `sqry-daemon` — that is a CI-policy check: a CI step greps every `Cargo.toml` for `rebuild-internals` and only `sqry-daemon/Cargo.toml` is whitelisted; a code-owner rule on `sqry-core/Cargo.toml` requires review before the feature definition itself can change.

**Placement rule:** `clone_for_rebuild` is called only on the rebuild dispatcher's background tokio task, never on the query path. The dispatcher obtains a stable `Arc<CodeGraph>` via `ArcSwap::load_full()`, then calls `clone_for_rebuild` on that Arc. Since the Arc was just loaded and held only by the rebuild task, `Arc::get_mut`/`Arc::make_mut` is O(1) in the common case.

**Latency budget:** `clone_for_rebuild` on a 384k-node / 1.3M-edge reference graph must complete in < 50 ms. A benchmark records latency; warning threshold 50 ms, hard record threshold 200 ms. Exceeding warning logs but does not fail the rebuild.

**Tests:**
- **Field exhaustiveness**: compile-fail test proves adding a field to `CodeGraph` without extending `RebuildGraph` breaks the build.
- **Finalize completeness**: after finalize, no publish-visible structure contains a tombstoned NodeId (enforced by §F residue check).
- **Interner compaction trigger**: synthetic test drives `interner_live_ratio` below threshold; asserts the next rebuild is full even though the change set would be incremental-eligible.
- **Concurrency**: 10 reader threads querying `ArcSwap<CodeGraph>` during an incremental rebuild; readers see a consistent old snapshot throughout and the new snapshot only after `ArcSwap::store` completes.
- **Latency gate**: benchmark records `clone_for_rebuild` time on the reference fixture; CI records but does not fail (warning 50 ms, record 200 ms).

- [ ] **Step 0d: [A2 §F] Bijective bucket invariant + pre-reuse tombstone residue invariants**

(from Amendment 2 §F, approved 2026-04-09 — pre-implementation gate item 2.)

Rev 1 invariants were cardinality-only (`sum(bucket_lens) == live_count`) and tombstone-residue checks fired only after slot reuse. Both gaps admit the bug class they were meant to prevent. This step replaces them with bijective and pre-reuse checks.

**Bijective bucket membership.** Three things must be true at every publish boundary:

```rust
#[cfg(any(debug_assertions, test))]
impl CodeGraph {
    pub fn assert_bucket_bijection(&self) {
        // a) Every live node appears in exactly one bucket.
        let mut seen: HashMap<NodeId, FileId> = HashMap::new();
        for (file_id, bucket) in self.files.per_file_nodes().iter() {
            for node_id in bucket {
                assert!(self.nodes.is_live(*node_id),
                    "dead node {node_id:?} in bucket {file_id:?}");
                let prior = seen.insert(*node_id, *file_id);
                assert!(prior.is_none(),
                    "node {node_id:?} in multiple buckets: {prior:?} and {file_id:?}");
                // b) The bucket's FileId must match the node's actual FileId.
                let node_file = self.nodes.get(*node_id).file_id();
                assert_eq!(node_file, *file_id,
                    "node {node_id:?} misfiled: in bucket {file_id:?}, actually {node_file:?}");
            }
        }
        // c) Every live node in the arena is accounted for by `seen`.
        for node_id in self.nodes.iter_live() {
            assert!(seen.contains_key(&node_id),
                "live node {node_id:?} absent from all buckets");
        }
    }
}
```

This proves: no node duplicated across buckets, no node misfiled, no live node missing from the index.

**Pre-reuse tombstone residue check.** Iterates every publish-visible NodeId-bearing structure and asserts none contains a node present in the rebuild's tombstone set, *regardless of arena generation state*:

```rust
#[cfg(any(debug_assertions, test))]
impl RebuildGraph {
    pub fn assert_no_tombstone_residue(&self) {
        let dead: &HashSet<NodeId> = &self.tombstones;
        if dead.is_empty() { return; }
        for nid in self.indices.all_node_ids() {
            assert!(!dead.contains(&nid), "tombstone {nid:?} in auxiliary index");
        }
        for nid in self.edges.all_node_ids() {
            assert!(!dead.contains(&nid), "tombstone {nid:?} in edge store");
        }
        for nid in self.macro_metadata.all_node_ids() {
            assert!(!dead.contains(&nid), "tombstone {nid:?} in macro metadata");
        }
        for nid in self.export_map.all_node_ids() {
            assert!(!dead.contains(&nid), "tombstone {nid:?} in export map");
        }
        for nid in self.files.all_bucket_node_ids() {
            assert!(!dead.contains(&nid), "tombstone {nid:?} in per-file bucket");
        }
    }
}
```

Every NodeId-bearing structure must expose `all_node_ids()` via the §K `NodeIdBearing` trait. `finalize()` refuses to produce a `CodeGraph` if this check fails in debug builds.

**Invariant call sites (single source of truth):**
- The bijection check fires in debug/test builds at:
  1. End of `build_and_persist_graph` (full rebuild) before returning.
  2. Step 13 of `RebuildGraph::finalize()`.
  3. Inside `WorkspaceManager::publish_graph()` — a single helper that wraps every `ArcSwap::store` call — before the store.
  4. Every test in the §E equivalence harness.
- The tombstone residue check fires at **exactly one site**: step 14 of `RebuildGraph::finalize()`, on the just-assembled `CodeGraph`, against the drained tombstone set stashed at step 8. §F and §H refer to the same call site; there is no disagreement.

**Release build semantics.** Checks compile to no-ops in release. The §E harness running in CI is the release-time guarantee — release builds do not pay the invariant cost; CI certifies drift freedom before any release.

**Tests:**
- Negative: deliberately misfile a node, assert the bijection check fires with the expected message.
- Negative: deliberately leave a tombstoned NodeId in the edge store, assert residue check fires.
- Positive: every full and incremental rebuild in the §E harness passes both checks.

- [ ] **Step 1: [A2026-04-09] Add per-file node bucket to FileRegistry**

Per §A of the 2026-04-09 amendment, removal must be O(B) not O(N·B). Add an
owned reverse index:

```rust
pub struct FileRegistry {
    // ... existing fields ...
    per_file_nodes: HashMap<FileId, Vec<NodeId>>,
}

impl FileRegistry {
    pub fn record_node(&mut self, file_id: FileId, node_id: NodeId) { ... }
    pub fn take_nodes(&mut self, file_id: FileId) -> Vec<NodeId> { ... }
    pub fn nodes_for_file(&self, file_id: FileId) -> &[NodeId] { ... }
}
```

Pass 1 pushes every newly created node into the bucket. The bucket is
reconstructed from a single arena pass when loading from snapshot (or
persisted inline — implementer's call, but the reconstruction path must exist).

Invariant: outside an active rebuild,
`sum(per_file_nodes.values().map(Vec::len)) == arena.live_node_count()`.

- [ ] **Step 2: [A2026-04-09] Tombstone-based remove_file**

```rust
impl CodeGraph {
    /// Mark all nodes from the given file as tombstoned. Does NOT touch
    /// auxiliary indices — those are compacted in a single linear pass
    /// after all closure files are marked.
    pub fn remove_file(&mut self, file_id: FileId) -> usize {
        let nodes = self.file_registry.take_nodes(file_id);
        for nid in &nodes {
            self.tombstones.insert(*nid);
        }
        nodes.len()
    }

    /// Drain tombstones by running one linear compaction pass per
    /// auxiliary index. Must be called before the new graph is published.
    pub fn compact_tombstones(&mut self) { ... }
}
```

Auxiliary indices that MUST be compacted: `name_index`, `kind_index`,
`qualified_name_index`, `file_index` (note: §K corrects rev 2's
`file_symbol_index` typo — the actual field is `file_index`), and every
other NodeId-bearing structure listed in the §K master matrix (Step 0b).
The tombstone set is local to the rebuild and must be empty before
`ArcSwap::store`. In the §H model, `compact_tombstones` is subsumed by
`RebuildGraph::finalize()` — the legacy entrypoint is retained for the
full-rebuild path only.

- [ ] **Step 3: [A2026-04-09] Tests for bucket + tombstone correctness**

- Build a graph with 1000 files × 200 nodes. Remove 20 files. Assert
  `per_file_nodes` shrank by exactly `20 * 200` entries.
- After `compact_tombstones`, assert no auxiliary index contains any of the
  removed NodeIds.
- Invariant test: bucket sum == live node count outside active rebuild.
- Benchmark (recorded, not a CI gate): incremental removal of one 10k-node
  file completes in < 10 ms on the reference fixture.

- [ ] **Step 4: Implement incremental_rebuild against `RebuildGraph` (§H)**

Replaces the rev 1 sketch. The incremental rebuild operates exclusively on a `RebuildGraph` value obtained via `clone_for_rebuild` and converts back to `CodeGraph` only via `RebuildGraph::finalize()` (Step 0c). This is the only Rust path to a publishable graph.

1. `let prior = workspace.graph.load_full();` — stable snapshot.
2. `let mut rebuild = clone_for_rebuild(&prior);` — gated by the `rebuild-internals` feature; called only on the dispatcher's background task.
3. For each file in the closure, call `RebuildGraph::remove_file(file_id)` which routes through `FileRegistry::take_nodes` and inserts into `rebuild.tombstones`.
4. Re-parse closure files using the plugin manager (same as Pass 1 in `entrypoint.rs`); push new nodes through `record_node` and the rebuild's interner builder (NOT the snapshot).
5. Run Pass 2 enrichment on new nodes.
6. Run Pass 3 intra-file edges on new nodes.
7. Rebuild ExportMap entries for closure files (reuse `pass4_cross.rs` logic).
8. Run Pass 4 cross-file linking for closure files.
9. Run Pass 5 cross-language linking if needed.
10. **At each pass boundary** (after Pass 1, 2, 3, 4, 5 and immediately before `finalize()`), poll the §J `rebuild_cancelled` atomic; on detection, drop the `RebuildReservation` and exit without publishing.
11. Call `RebuildGraph::finalize()` — this is where compaction (steps 1–7 of the 14-step sequence in §H), the bijection check (§F), and the tombstone-residue check (§F at finalize step 14) all run.
12. Recompute `GraphMemorySize::heap_bytes()` for the assembled `CodeGraph`.
13. Return the new `CodeGraph` to the dispatcher, which calls `WorkspaceManager::publish_graph` (the §F single-helper wrapper around `ArcSwap::store`) and updates `memory_bytes`, `memory_high_water_bytes`, `total_memory`, `total_memory_high_water` per Amendment 1 §D, plus admission accounting per §G.

- [ ] **Step 5: Wire `incremental_rebuild` into the §E equivalence harness**

Replace the stub from Step 0a with the real `incremental_rebuild`. The §E harness must continue to pass — every property-test step compares the incremental result against a fresh full rebuild via `assert_graph_semantically_equivalent`. No ad-hoc "same node count, same edge count" tests substitute for the harness; the harness is the acceptance criterion.

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p sqry-core incremental`
Commit: `feat(core): implement incremental rebuild engine with per-file buckets and tombstone compaction`

---

## Phase 2: sqry-daemon Crate

### Task 5: Crate Scaffolding and Configuration

**Files:**
- Create: `sqry-daemon/Cargo.toml`
- Create: `sqry-daemon/src/lib.rs`
- Create: `sqry-daemon/src/config.rs`
- Modify: `Cargo.toml` (add workspace member)

- [ ] **Step 1: Create crate with Cargo.toml**

```toml
[package]
name = "sqry-daemon"
version.workspace = true
edition.workspace = true
description = "sqry daemon — persistent code graph service"

[dependencies]
sqry-core = { path = "../sqry-core" }
sqry-plugin-registry = { path = "../sqry-plugin-registry" }
arc-swap = "1"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tracing = "0.1"
dirs = "6"
fs2 = "0.4"
```

- [ ] **Step 2: Add workspace member to root Cargo.toml**

Add `"sqry-daemon"` to the `[workspace] members` list.

- [ ] **Step 3: Write DaemonConfig**

Create `sqry-daemon/src/config.rs`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub memory_limit_mb: u64,           // default: 2048
    pub idle_timeout_minutes: u64,      // default: 30
    pub debounce_ms: u64,               // default: 2000
    pub incremental_threshold: usize,   // default: 20
    pub closure_limit_percent: u32,     // default: 30
    pub stale_serve_max_age_hours: u32, // [A2026-04-09] default: 24, 0 = unlimited
    pub rebuild_drain_timeout_ms: u64,    // [A2 §G] default: 5000 — retention reaper warning threshold (NOT an accounting deadline)
    pub interner_compaction_threshold: f32, // [A2 §H] default: 0.5 — below this ratio, next debounce tick triggers a mandatory full rebuild
    pub log_file: Option<PathBuf>,
    pub log_level: String,              // default: "info"
    pub log_max_size_mb: u64,           // default: 50
    pub socket: SocketConfig,
    pub workspaces: Vec<WorkspaceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SocketConfig {
    pub path: Option<String>,
    pub pipe_name: Option<String>,      // Windows
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceConfig {
    pub path: PathBuf,
    pub pinned: Option<bool>,
    pub exclude: Option<bool>,
}
```

- [ ] **Step 4: Implement config loading with defaults and env overrides**

```rust
impl DaemonConfig {
    pub fn load() -> Result<Self> { ... }        // ~/.config/sqry/daemon.toml
    pub fn socket_path(&self) -> PathBuf { ... }  // Platform-specific default
    pub fn pid_path(&self) -> PathBuf { ... }
    pub fn lock_path(&self) -> PathBuf { ... }
}
```

- [ ] **Step 4b: [A2 §G.6] Working-set multiplier constants**

(from Amendment 2 §G.6, approved 2026-04-09.) Admission control must size reservations against the rebuild **working set**, not the final-graph estimate. Add the following `const` values to `sqry-daemon/src/config.rs` (revisable via benchmarking; must be conservative — err high):

```rust
/// Covers duplicated index/edge structures held during rebuild before finalize.
pub const WORKING_SET_MULTIPLIER: f64 = 1.5;
/// Bounded growth headroom for the rebuild-local interner builder, expressed
/// as a fraction of the seed snapshot's bytes.
pub const INTERNER_BUILDER_OVERHEAD_RATIO: f64 = 0.25;
```

`working_set_estimate = new_graph_final_estimate * WORKING_SET_MULTIPLIER + staging_overhead + interner_builder_overhead`, where `interner_builder_overhead = interner_snapshot_bytes * INTERNER_BUILDER_OVERHEAD_RATIO` and `staging_overhead = staging_node_count * sizeof(StagingNode)`. For full rebuild, `new_graph_final_estimate = file_count * avg_bytes_per_file`; for incremental, `current_bytes + closure.len() * avg_bytes_per_file`. WorkspaceManager (Task 6) consumes these constants.

- [ ] **Step 5: Write tests for config parsing and defaults**

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p sqry-daemon config`
Commit: `feat(daemon): add sqry-daemon crate with configuration`

---

### Task 6: WorkspaceManager

**Files:**
- Create: `sqry-daemon/src/workspace.rs`
- Create: `sqry-daemon/src/workspace/state.rs`
- Create: `sqry-daemon/src/workspace/manager.rs`
- Test: `sqry-daemon/src/workspace/manager.rs` (inline tests)

- [ ] **Step 1: Define WorkspaceState and WorkspaceKey**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WorkspaceState {
    Unloaded = 0,
    Loading = 1,
    Loaded = 2,
    Rebuilding = 3,
    Evicted = 4,
    Failed = 5,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct WorkspaceKey {
    pub index_root: PathBuf,
    pub root_mode: ProjectRootMode,
    pub config_fingerprint: u64,
}
```

- [ ] **Step 2: Define LoadedWorkspace**

```rust
pub struct LoadedWorkspace {
    pub graph: ArcSwap<CodeGraph>,
    pub watcher: Mutex<Option<SourceTreeWatcher>>,
    pub state: AtomicU8,
    pub last_accessed: RwLock<Instant>,
    pub memory_bytes: AtomicUsize,
    pub memory_high_water_bytes: AtomicUsize, // [A2026-04-09] peak over loaded lifetime
    pub pinned: bool,
    pub key: WorkspaceKey,
    pub last_error: RwLock<Option<WorkspaceError>>,
    pub last_good_at: RwLock<Option<SystemTime>>, // [A2026-04-09] stamped on build success
    pub retry_count: AtomicU32,
}
```

- [ ] **Step 3: Write WorkspaceManager**

```rust
pub struct WorkspaceManager {
    workspaces: RwLock<HashMap<WorkspaceKey, Arc<LoadedWorkspace>>>,
    config: Arc<DaemonConfig>,
    plugins: Arc<PluginManager>,
    total_memory: AtomicUsize,
    total_memory_high_water: AtomicUsize, // [A2026-04-09] peak aggregate
}

impl WorkspaceManager {
    pub fn new(config: Arc<DaemonConfig>, plugins: Arc<PluginManager>) -> Self { ... }
    pub async fn get_or_load(&self, key: &WorkspaceKey) -> Result<Arc<CodeGraph>> { ... }
    pub async fn evict_lru(&self) -> Result<()> { ... }
    pub fn unload(&self, key: &WorkspaceKey) -> Result<()> { ... }
    pub fn status(&self) -> DaemonStatus { ... }
    fn check_admission(&self, estimated_bytes: usize) -> Result<()> { ... }
}
```

- [ ] **Step 4: Implement admission control with high/low watermarks**

Before loading or rebuilding, `check_admission(estimated_bytes)` verifies the new graph won't exceed the memory budget:
- High watermark: 100% of `memory_limit_mb` → triggers eviction
- Low watermark: 80% of `memory_limit_mb` → eviction target (evict LRU non-pinned until total_memory < low watermark)
- If after evicting all non-pinned workspaces the budget is still exceeded, reject the load with `memory_budget_exceeded` error
- Pinned workspaces are exempt from eviction but still counted in total_memory

- [ ] **Step 4a: [A2 §G.1] Single-mutex admission state with two-phase reservation**

(from Amendment 2 §G, approved 2026-04-09 — pre-implementation gate item 3: this step must be complete before any `ArcSwap::store` call path lands in `WorkspaceManager`.)

Rev 1 had two race conditions and one accounting bug: `reserve_rebuild` read-decide-write was not atomic; the RAII guard dropped reservation before the old graph was freed; estimates only covered the final graph, not the working set. This step replaces that design.

Add a single `parking_lot::Mutex<AdmissionState>` per `WorkspaceManager`. This is **not** a query-path lock — it is acquired only when a rebuild starts or finishes. Query and read paths remain lock-free.

```rust
pub struct AdmissionState {
    /// Sum of memory_bytes across all loaded workspaces.
    loaded_bytes: u64,
    /// Sum of reserved bytes for in-flight rebuilds (new graph working set).
    reserved_bytes: u64,
    /// Published-but-not-yet-dropped old graphs, keyed by an opaque token.
    /// The map OWNS the strong reference; the reaper (§G.3) is the SOLE
    /// removal owner.
    retained_old: HashMap<OldGraphToken, RetainedEntry>,
}

pub struct WorkspaceManager {
    admission: Mutex<AdmissionState>,
    // ... other fields ...
}
```

**Two-phase reservation protocol** that respects the §J.4 lock order (`workspaces -> rebuild_lane -> admission`):

```rust
impl WorkspaceManager {
    fn memory_limit(&self) -> u64 { self.config.memory_limit_mb * 1024 * 1024 }

    fn reserve_rebuild(
        &self,
        key: &WorkspaceKey,
        working_set_estimate: u64,
    ) -> Result<RebuildReservation> {
        let limit = self.memory_limit();

        // Phase 1: pick victims while holding `workspaces`.
        let plan: EvictionPlan = {
            let ws_guard = self.workspaces.read();
            let state = self.admission.lock();
            let retained_total: u64 = state.retained_old.values().map(|e| e.bytes).sum();
            let projected = state.loaded_bytes + state.reserved_bytes
                + retained_total + working_set_estimate;
            if projected <= limit {
                EvictionPlan::none_needed()
            } else {
                let need = projected - limit;
                select_lru_non_pinned(&ws_guard, &state, need, key)?
            }
            // Both `state` and `ws_guard` drop here; no locks held across phase 2.
        };

        // Phase 2: execute eviction side effects with NO locks held.
        for victim in &plan.victims {
            self.execute_eviction(victim)?;
        }

        // Phase 3: reacquire `admission` alone — authoritative commit point.
        let mut state = self.admission.lock();
        let retained_total: u64 = state.retained_old.values().map(|e| e.bytes).sum();
        let projected = state.loaded_bytes + state.reserved_bytes
            + retained_total + working_set_estimate;
        if projected > limit {
            return Err(Error::MemoryBudgetExceeded { code: -32003 });
        }
        state.reserved_bytes += working_set_estimate;
        Ok(RebuildReservation { manager: self, bytes: working_set_estimate })
    }
}
```

**Lock-order compliance:** Phase 1 acquires `workspaces -> admission` (per §J.4). Phase 2 acquires no locks. Phase 3 acquires `admission` alone. At no point is `admission` held while acquiring `workspaces`. Two concurrent rebuilds still cannot both commit headroom because phase 3's re-check is the authoritative commit point.

**Race window:** Between phases a concurrent rebuild may reserve bytes that phase 3 then finds unavailable, causing a clean `MemoryBudgetExceeded` (`-32003`) error for the losing caller. This is correct behavior; the loser retries or transitions to Failed per §G.7.

- [ ] **Step 4b: [A2 §G.2] `RetainedEntry` + `publish_and_retain` with single-owner cleanup and panic-safe rollback**

(from Amendment 2 §G.2, §G.4, approved 2026-04-09.)

The admission map is the **sole owner** of the retained `Arc<CodeGraph>`. There is no separate `RetainedOldGraph` type, no `Drop` impl that touches accounting, and no way for task cancellation to cause early token removal.

```rust
pub struct RetainedEntry {
    pub bytes: u64,
    /// The admission map holds the strong reference. Slow queries hold
    /// additional strong references; `strong_count` measures whether any
    /// outlive this entry.
    pub graph: Arc<CodeGraph>,
    pub published_at: Instant,
    pub warned_past_timeout: bool,
}
```

**`publish_and_retain` is a sync `fn`, not `async fn`.** The body between the `swap` and the `admission.lock().insert(...)` MUST contain zero `.await` points — tokio task cancellation can only occur at `.await` boundaries, so the sequence executes atomically with respect to cancellation. Reviewers MUST verify the function remains synchronous and contains no calls to other `async fn` or `.await`-bearing helpers.

**Panic-safety contract via `RollbackGuard`** — captures *both* original values before either swap and reverses both on unwind:

```rust
fn publish_and_retain(
    mgr: &Arc<WorkspaceManager>,
    ws: &LoadedWorkspace,
    new_graph: CodeGraph,
    new_bytes: u64,
) -> OldGraphToken {
    let new_arc = Arc::new(new_graph);
    let token = OldGraphToken::new();
    // Capture originals BEFORE any swap so unwind can reverse.
    let prior_arc_for_rollback = ws.graph.load_full();
    let prior_bytes = ws.memory_bytes.load(Ordering::Acquire);

    let mut rollback = RollbackGuard {
        ws,
        prior_arc: Some(prior_arc_for_rollback),
        prior_bytes,
        armed: true,
    };

    // --- Non-recoverable zone (no .await, no fallible ops between swaps) ---
    let old_arc = ws.graph.swap(new_arc);
    let old_bytes = ws.memory_bytes.swap(new_bytes, Ordering::AcqRel);
    // --- End non-recoverable zone ---

    let mut state = mgr.admission.lock();
    state.loaded_bytes = state.loaded_bytes + new_bytes - old_bytes;
    state.retained_old.insert(token, RetainedEntry {
        bytes: old_bytes,
        graph: old_arc,
        published_at: Instant::now(),
        warned_past_timeout: false,
    });
    drop(state);

    rollback.armed = false; // disarm on success
    token
}

struct RollbackGuard<'a> {
    ws: &'a LoadedWorkspace,
    prior_arc: Option<Arc<CodeGraph>>,
    prior_bytes: usize,
    armed: bool,
}

impl<'a> Drop for RollbackGuard<'a> {
    fn drop(&mut self) {
        if !self.armed { return; }
        if let Some(arc) = self.prior_arc.take() {
            self.ws.graph.store(arc);
        }
        self.ws.memory_bytes.store(self.prior_bytes, Ordering::Release);
    }
}
```

**Mutation-free panic zones.** The arithmetic and `HashMap::insert` inside the `admission.lock()` critical section cannot panic under normal operation (u64 deltas; no allocation failure handling). A panic before the lock is fully reversible by the guard. After the lock is released, the only remaining statement is disarming the guard.

The rebuild dispatcher wraps the call to `publish_and_retain` in `std::panic::catch_unwind` at its outermost boundary, so any panic is caught, the workspace is marked Failed per §G.7, and the dispatcher continues serving other workspaces.

This function is the canonical implementation of `WorkspaceManager::publish_graph()` referenced by §F.3 — every `ArcSwap::store` call path on `LoadedWorkspace.graph` MUST go through this helper. The §F.1 bucket bijection check fires here before the swap.

- [ ] **Step 4c: [A2 §G.3] Retention reaper task — sole cleanup owner**

(from Amendment 2 §G.3, §G.4, approved 2026-04-09.)

A single long-lived reaper task per `WorkspaceManager` periodically scans `retained_old` and removes entries whose `Arc::strong_count` shows the admission map is the last holder. The reaper is the **only** code path that removes tokens from `retained_old`.

```rust
async fn retention_reaper(mgr: Arc<WorkspaceManager>) {
    let interval = Duration::from_millis(25);
    loop {
        tokio::time::sleep(interval).await;
        let timeout = Duration::from_millis(mgr.config.rebuild_drain_timeout_ms);
        let now = Instant::now();
        let mut to_log: Vec<OldGraphToken> = Vec::new();
        {
            let mut state = mgr.admission.lock();
            state.retained_old.retain(|token, entry| {
                if Arc::strong_count(&entry.graph) == 1 {
                    false // last holder; drop the Arc with the entry
                } else {
                    if !entry.warned_past_timeout
                        && now.duration_since(entry.published_at) > timeout
                    {
                        entry.warned_past_timeout = true;
                        to_log.push(*token);
                    }
                    true
                }
            });
        }
        for token in to_log {
            warn!(
                "rebuild drain exceeded {:?}ms for retained token {:?}; bytes still accounted",
                mgr.config.rebuild_drain_timeout_ms, token
            );
        }
    }
}
```

**`rebuild_drain_timeout_ms` is a logging threshold, NOT an accounting deadline.** A retained entry is freed iff its `Arc::strong_count` drops to 1, regardless of how long that takes. The timeout only governs when the warning fires.

**Cancellation safety.** If the reaper task is aborted (daemon shutdown, panic, `JoinHandle::abort`), retained entries remain in the admission map. Their `Arc`s are still held by the map, so the old graphs stay alive and correctly accounted until the map itself is dropped. On daemon shutdown, `WorkspaceManager::Drop` drops the admission state, which drops every `RetainedEntry`, which drops every retained `Arc<CodeGraph>` in one pass. No accounting leak, no dangling Arc.

Spawn the reaper from `WorkspaceManager::new` and store its `JoinHandle` so `Drop` can abort it cleanly.

- [ ] **Step 4d: [A2 §G.5–§G.7] Authoritative accounting contract, working-set rule, pinned vs non-pinned failure modes**

(from Amendment 2 §G.5–§G.7, approved 2026-04-09.)

**Authoritative accounting rule** (shared by §G, §H, and `daemon/status`):

> At any instant, `loaded_bytes + reserved_bytes + sum(retained_old bytes)` equals the sum of every `CodeGraph`-worth of memory the daemon is currently responsible for, including graphs published to a workspace, graphs being constructed, and old graphs whose `Arc` has not yet been uniquely held by the admission map.

Tests exercise this invariant after every publish, rebuild failure, eviction, and cancellation. Admission rejects a reservation iff `loaded + reserved + retained + new_working_set > limit`.

**Working-set rule, not final-size rule.** Admission estimates cover the full working set, not just the final graph:

```text
working_set_estimate = new_graph_final_estimate * WORKING_SET_MULTIPLIER
                       + staging_overhead
                       + interner_builder_overhead
```

| Term | Definition | Default |
|---|---|---|
| `new_graph_final_estimate` | Full rebuild: `file_count * avg_bytes_per_file`. Incremental: `current_bytes + closure.len() * avg_bytes_per_file`. | — |
| `WORKING_SET_MULTIPLIER` | Duplicated index/edge structures held during rebuild before finalize. | `1.5` |
| `staging_overhead` | `staging_node_count * sizeof(StagingNode)` estimate. | — |
| `interner_builder_overhead` | `interner_snapshot_bytes * INTERNER_BUILDER_OVERHEAD_RATIO`. | `0.25` |

Constants live in `sqry-daemon/src/config.rs` (Task 5 Step 4b).

**Pinned vs non-pinned failure modes.** If admission cannot satisfy the reservation after evicting all non-pinned workspaces to zero:

- **Pinned workspace, full rebuild**: transition to Failed with error `-32003 memory_budget_exceeded`. Old graph continues serving with `meta.stale = true` (per Amendment 1 §C, subject to the `stale_serve_max_age_hours` cap).
- **Non-pinned workspace**: unload entirely (stop watcher, drop graph, drop tracker), mark Unloaded. Next query re-attempts under current memory pressure; if it still fails, the query receives the same `-32003` error.

The `-32003 memory_budget_exceeded` error code is plumbed through `WorkspaceError`, `IpcError`, and the JSON-RPC response envelope.

- [ ] **Step 4e: [A2 §G] Test requirements**

- **Concurrency**: 16 threads each start rebuilds on distinct workspaces with estimates summing to 1.5× budget. Assert no two concurrent rebuilds both succeed in exceeding the budget; assert eventual consistency of `loaded_bytes + reserved_bytes + retained_total`.
- **Drain accounting**: hold an old-Arc reference via a mock slow query. Trigger rebuild. Assert `retained_old` retains the old bytes until the mock query releases the Arc, *even if* `rebuild_drain_timeout_ms` elapses.
- **Forced logging vs forced accounting**: assert the warning fires at timeout but `retained_old[token]` remains non-zero.
- **Reservation RAII**: inject a rebuild failure mid-build; assert `reserved_bytes` returns to pre-rebuild value and no leak.
- **Working-set headroom**: a rebuild with a tiny final graph but large staging set is still gated by the multiplier; assert admission rejects a rebuild whose working set would exceed budget even though its final graph would not.
- **Pinned failure**: all non-pinned evicted and still insufficient → pinned workspace transitions to Failed with `-32003`, stale serving continues.
- **Panic-safe rollback**: inject a panic between `swap` and `admission.lock()`; assert `RollbackGuard` reverts both `graph` and `memory_bytes` and the workspace serves the prior graph.
- **Reaper cancellation**: abort the reaper task; assert retained entries remain accounted; assert dropping `WorkspaceManager` releases all retained `Arc`s.

- [ ] **Step 5: Implement LRU eviction**

Evict least-recently-accessed non-pinned workspace: stop watcher, drop graph from ArcSwap, decrement total_memory. Continue until below low watermark.

- [ ] **Step 6: Implement Failed state with exponential backoff and stale:true propagation**

Queries return last good graph with `stale: true` metadata flag in the JSON-RPC response. The `stale` flag must propagate through:
- WorkspaceManager → IPC response metadata → CLI client display
- WorkspaceManager → LSP router → LSP response metadata
- WorkspaceManager → MCP router → MCP tool response metadata

If no prior good graph exists, return structured error `workspace_build_failed` (code -32001).
Retry policy: exponential backoff 30s → 60s → 120s → 300s → 600s. Triggers: timer, file change, explicit `daemon/load`.

**[A2026-04-09] High-water mark update hook.** Every time `memory_bytes` is
assigned (initial load, full rebuild completion, incremental rebuild's
`ArcSwap::store`), immediately call
`memory_high_water_bytes.fetch_max(new, Relaxed)` and
`total_memory_high_water.fetch_max(new_total, Relaxed)`. High-water marks are
monotonic over the workspace's loaded lifetime — they reset only on
unload/eviction (fresh `LoadedWorkspace`), not on rebuilds or backoff.

**[A2026-04-09] Staleness age cap.** `LoadedWorkspace` tracks `last_good_at:
RwLock<Option<SystemTime>>`, stamped on every successful build. When serving a
Failed workspace, compute `now - last_good_at`. If
`>= stale_serve_max_age_hours` (and cap > 0), stop serving stale results and
return structured error `workspace_stale_expired` (code `-32002`) with
`{last_good_at, age_hours, last_error}` in `error.data`. The `-32002` code is
distinct from `-32001` (the no-good-graph case).

- [ ] **Step 7: Write unit tests**

Test: load workspace, verify state transitions, evict LRU, verify pinned exemption, memory admission control with watermarks, Failed state with stale:true propagation, **[A2026-04-09] high-water mark monotonicity** (rebuild with smaller graph must not decrease `memory_high_water_bytes`; unload resets it; aggregate `total_memory_high_water` matches sum of peaks across load sequence).

- [ ] **Step 8: Run tests and commit**

Run: `cargo test -p sqry-daemon workspace`
Commit: `feat(daemon): add WorkspaceManager with LRU eviction and state machine`

---

### Task 7: Watcher Orchestration and Rebuild Dispatcher

**Files:**
- Create: `sqry-daemon/src/rebuild.rs`
- Modify: `sqry-daemon/src/workspace/manager.rs`

- [ ] **Step 1: Write rebuild dispatcher**

```rust
pub struct RebuildDispatcher {
    manager: Arc<WorkspaceManager>,
    config: Arc<DaemonConfig>,
}

impl RebuildDispatcher {
    /// Called when SourceTreeWatcher produces a ChangeSet.
    /// Decides incremental vs full rebuild and executes on background thread.
    pub async fn handle_changes(
        &self,
        key: &WorkspaceKey,
        changes: ChangeSet,
    ) -> Result<()> { ... }
}
```

- [ ] **Step 2: Implement hybrid rebuild decision logic**

```rust
if changes.git_state_changed {
    self.full_rebuild(key).await
} else if changes.changed_files.len() > self.config.incremental_threshold {
    self.full_rebuild(key).await
} else {
    let graph = self.manager.get_graph(key)?;
    let closure = compute_reverse_dep_closure(&file_ids, &graph);
    let limit = graph.file_count() * self.config.closure_limit_percent as usize / 100;
    if closure.len() > limit {
        self.full_rebuild(key).await
    } else {
        self.incremental_rebuild(key, &changes.changed_files, &closure).await
    }
}
```

- [ ] **Step 2b: [A2 §J] Per-workspace rebuild lane, coalescing, and eviction cancellation**

(from Amendment 2 §J, approved 2026-04-09 — pre-implementation gate item 6: this step must be complete before Task 7 is marked done.)

Rev 1 did not state whether two debounce windows firing back-to-back on the same workspace queue, coalesce, cancel, or race. Any of the first three is acceptable; racing is not.

**Per-workspace rebuild lane.** Each `LoadedWorkspace` owns a single-slot rebuild channel + an atomic cancel flag:

```rust
pub struct LoadedWorkspace {
    // ... existing fields ...
    /// At most one queued rebuild per workspace.
    pub rebuild_lane: tokio::sync::Mutex<Option<PendingRebuild>>,
    pub rebuild_cancelled: AtomicBool,
}

pub struct PendingRebuild {
    pub changes: ChangeSet,
    pub enqueued_at: Instant,
}
```

The dispatcher's event loop per workspace is a serial consumer: at most one rebuild executes at a time per workspace, and at most one additional rebuild is pending.

**Coalescing rule.** When a new `ChangeSet` arrives while another rebuild is in flight:
- If `rebuild_lane` is empty: store the new `ChangeSet`.
- If `rebuild_lane` is occupied: **merge** the two `ChangeSet`s (union of changed files, `git_state_changed = a || b`), replace the slot with the merged set, update `enqueued_at` to the newer timestamp.

When the in-flight rebuild completes, the lane is drained: if a pending rebuild exists, the dispatcher immediately starts it with the coalesced changes.

**Cancellation on workspace eviction is lock-free.** Eviction MUST NOT acquire `rebuild_lane`:
1. Eviction sets `rebuild_cancelled = true` via `store(Ordering::Release)`.
2. Eviction releases its reference to the workspace. It does not touch `rebuild_lane`.

The in-flight rebuild task polls `rebuild_cancelled` at each pass boundary (after Pass 1, 2, 3, 4, 5, and before `finalize()` — see Task 4 Step 4 item 10). On detection, the rebuild aborts, drops its `RebuildReservation` RAII guard (releasing `reserved_bytes` under the admission mutex), and exits without publishing. On exit, the rebuild task drains `rebuild_lane`: it takes the pending rebuild if any and discards it (the workspace is gone).

`rebuild_lane` is only ever locked by the dispatcher's per-workspace event loop task — not by eviction, not by admission, not by the reaper. This eliminates any possibility of a `rebuild_lane` ↔ `admission` lock inversion.

**Lock order contract (authoritative — referenced by §G.1).** All code paths that acquire more than one lock MUST follow this total order; acquiring out of order is a bug enforced by code review.

Order (outermost → innermost):
1. `WorkspaceManager.workspaces: RwLock<HashMap<...>>`
2. `LoadedWorkspace.rebuild_lane: tokio::sync::Mutex<_>`
3. `WorkspaceManager.admission: parking_lot::Mutex<AdmissionState>`

Rules:
- A holder of `admission` may NOT acquire `rebuild_lane` or `workspaces` — it is the innermost lock.
- A holder of `rebuild_lane` may NOT acquire `workspaces`. `rebuild_lane` is used only for scheduling/coalescing pending rebuilds; it is never held across a call that takes `workspaces` or `admission` nestedly.
- The per-workspace dispatcher task acquires `rebuild_lane` **only** to read/coalesce `PendingRebuild` and **releases** it before calling `WorkspaceManager::reserve_rebuild`. After `reserve_rebuild` returns, the dispatcher may reacquire `rebuild_lane` briefly to update lane state (mark in-flight, etc.).
- Eviction iterates `workspaces` and, for each victim, sets the atomic cancel flag (no lock), then acquires `admission` alone to update accounting. Eviction never takes `rebuild_lane`.
- The retention reaper acquires only `admission`.

**Dispatcher reservation call path (canonical sequence — Step 3 must implement this exactly):**

```text
1. lane = workspace.rebuild_lane.lock().await
2. pending = lane.take()                       // may be coalesced set
3. drop(lane)                                  // release BEFORE reserve_rebuild
4. reservation = manager.reserve_rebuild(key, estimate)?
   //   internally: workspaces.read() -> admission.lock() (phase 1),
   //               then admission.lock() alone (phase 3)
5. (execute rebuild with reservation held)
6. (on completion) lane = workspace.rebuild_lane.lock().await
7. lane.mark_complete(); drain any new pending
```

Step 3 is load-bearing: `rebuild_lane` is released before `reserve_rebuild` so that §G.1's phase 1 can take `workspaces` without violating the lock order. The dispatcher cannot coalesce a new `PendingRebuild` into the lane while `reserve_rebuild` is running, but that is fine — the watcher event loop parks new events on the workspace's event queue, and they are drained in step 7.

**Interaction with §G admission.** The per-workspace serialization is independent of the global admission mutex. A workspace with a pending rebuild does not hold any manager-level lock while waiting for its in-flight rebuild to finish.

- [ ] **Step 3: Implement watcher event loop per workspace**

Each loaded workspace spawns a tokio task that loops on `SourceTreeWatcher::wait_for_changes()` and calls `RebuildDispatcher::handle_changes()`. The handler implements the canonical 7-step reservation call path from Step 2b exactly.

- [ ] **Step 4: Write tests for rebuild decision logic**

- [ ] **Step 4b: [A2 §I + §J] Dispatcher-level rebuild decision tests + serialization stress**

(from Amendment 2 §I and §J, approved 2026-04-09.) Closes the rev 1 gap where `ChangeSet` correctness was tested but rebuild *scheduling* correctness was not.

Create `sqry-daemon/tests/rebuild_dispatch_patterns.rs`:

```rust
fn assert_exactly_one_rebuild<F: FnOnce()>(workspace: &TestWorkspace, f: F);
fn assert_zero_rebuilds<F: FnOnce()>(workspace: &TestWorkspace, f: F);
```

These helpers instrument `RebuildDispatcher` to count dispatched rebuilds (full + incremental combined). Every row of the editor pattern matrix (Task 2 Step 9b) and every bulk git scenario has a matching dispatcher-level test asserting the expected rebuild count:

- DirectWrite / VimAtomicRename / JetBrainsAtomicSave / VscodeSafeSave / EmacsBackup → exactly 1 rebuild scheduled per save.
- `git checkout` (100+ file diff) → exactly 1 full rebuild.
- `git stash` → `git stash pop` → 2 rebuilds (one per debounce window).
- `git gc` → 0 rebuilds.
- `git commit` of a previously-edited file → 0 *additional* rebuilds beyond the original edit.

**§J serialization tests:**
- **Serialization**: fire three `ChangeSet`s in rapid succession; assert exactly two rebuilds execute (first + coalesced second/third).
- **Coalescing correctness**: file set of the second rebuild is the union of the second and third `ChangeSet`s.
- **`git_state_changed` propagation**: if any coalesced entry has `git_state_changed = true`, the scheduled rebuild is a full rebuild.
- **Eviction cancellation**: evict during rebuild; assert the rebuild aborts at the next pass boundary, the reservation is released (`reserved_bytes` decremented under admission), and no graph is published.
- **No races**: 100-iteration stress test with interleaved edits and evictions; assert no panics and no tombstone residue.

- [ ] **Step 5: Run tests and commit**

Run: `cargo test -p sqry-daemon rebuild`
Commit: `feat(daemon): add watcher orchestration and hybrid rebuild dispatcher`

---

### Task 8: IPC Server (UDS/Named Pipe)

**Files:**
- Create: `sqry-daemon/src/ipc.rs`
- Create: `sqry-daemon/src/ipc/server.rs`
- Create: `sqry-daemon/src/ipc/protocol.rs`
- Create: `sqry-daemon/src/ipc/router.rs`

- [ ] **Step 1: Write protocol types**

```rust
/// Shim registration message (4-byte length prefix + JSON)
pub struct ShimRegister {
    pub protocol: ShimProtocol,  // "lsp" | "mcp"
    pub pid: u32,
}

/// Version negotiation handshake
pub struct DaemonHello {
    pub client_version: String,
    pub protocol_version: u32,
}

pub struct DaemonHelloResponse {
    pub compatible: bool,
    pub daemon_version: String,
    pub envelope_version: u32,  // [A2026-04-09] 1 for current envelope
}

/// [A2026-04-09] Standard response envelope wrapping every tool result.
pub struct ResponseEnvelope<T> {
    pub result: T,
    pub meta: ResponseMeta,
}

pub struct ResponseMeta {
    pub stale: bool,
    pub last_good_at: Option<String>,    // RFC3339
    pub last_error: Option<String>,
    pub workspace_state: String,          // "loaded" | "failed" | "rebuilding" | ...
    pub daemon_version: String,
}
```

- [ ] **Step 2: Write IPC server**

Tokio-based `UnixListener` (Linux/macOS) with per-connection task dispatch. On Windows, use `tokio::net::windows::named_pipe`.

```rust
pub struct IpcServer {
    listener: UnixListener,
    manager: Arc<WorkspaceManager>,
    rebuild: Arc<RebuildDispatcher>,
}

impl IpcServer {
    pub async fn bind(path: &Path) -> Result<Self> { ... }
    pub async fn run(&self) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            tokio::spawn(self.handle_connection(stream));
        }
    }
}
```

- [ ] **Step 3: Write connection router**

Read first message to determine client type:
- `shim/register` with `protocol: "lsp"` → delegate to LSP router
- `shim/register` with `protocol: "mcp"` → delegate to MCP router
- `daemon/hello` → CLI client, route to MCP tool methods + daemon management methods

- [ ] **Step 4: Implement daemon management methods**

`daemon/status`, `daemon/load`, `daemon/unload`, `daemon/stop`

**[A2026-04-09] `daemon/status` must return `MemoryStatus`** per §D of the
amendment:

```rust
pub struct DaemonStatus {
    pub uptime_seconds: u64,
    pub daemon_version: String,
    pub memory: MemoryStatus,
    pub workspaces: Vec<WorkspaceStatus>,
}

pub struct MemoryStatus {
    pub limit_bytes: u64,
    pub current_bytes: u64,
    pub high_water_bytes: u64,
}

pub struct WorkspaceStatus {
    pub index_root: PathBuf,
    pub state: WorkspaceState,
    pub pinned: bool,
    pub current_bytes: u64,
    pub high_water_bytes: u64,
    pub last_good_at: Option<String>,   // RFC3339
    pub last_error: Option<String>,
}
```

Values are read via `Relaxed` atomic loads from `WorkspaceManager.total_memory`,
`WorkspaceManager.total_memory_high_water`, and each `LoadedWorkspace`'s
`memory_bytes` / `memory_high_water_bytes`.

**[A2 §G + §K-cross-cutting] JSON-RPC error code mapping.** The IPC error
translation layer must map the following daemon errors to JSON-RPC error
codes; these are the only daemon-specific codes and have no other changes
beyond Amendment 1:

| Daemon error | JSON-RPC code | Source |
|---|---|---|
| `workspace_build_failed` (no prior good graph) | `-32001` | Amendment 1 §C |
| `workspace_stale_expired` (`stale_serve_max_age_hours` exceeded) | `-32002` | Amendment 1 §C |
| `memory_budget_exceeded` (admission rejection after eviction) | `-32003` | Amendment 2 §G.1, §G.7 |

The `-32003` payload includes `{ limit_bytes, current_bytes, reserved_bytes, retained_bytes, requested_bytes }` in `error.data` so clients can display actionable diagnostics.

- [ ] **Step 5: Wire MCP tool methods to WorkspaceManager**

Route `semantic_search`, `relation_query`, etc. to the workspace's CodeGraph via WorkspaceManager::get_or_load.

**[A2026-04-09] Every tool response MUST be wrapped in `ResponseEnvelope`** —
CLI, MCP, and LSP translation layer alike. The envelope is uniform across all
34 MCP tools; there are no exceptions. LSP responses attach the same `meta`
block via a sibling `sqryExperimental` field (LSP lacks a standard metadata
slot).

When the workspace is Failed and serving a stale last-good graph, the router
populates `meta.stale = true`, `last_good_at`, and `last_error`. When the age
cap is exceeded, the router returns a JSON-RPC error with code `-32002`
(`workspace_stale_expired`) instead of wrapping a result.

MCP tool response payloads also include a `_stale_warning` string when
`meta.stale == true`, so downstream AI reasoners see the warning inline
without having to parse out-of-band metadata.

- [ ] **Step 6: Write integration test — start server, connect client, send query**

- [ ] **Step 6b: [A2026-04-09] Envelope, staleness, and memory telemetry integration tests**

Envelope + staleness:
- Connect, send `daemon/hello`, assert `envelope_version == 1`.
- Trigger a build failure on a previously-loaded workspace; assert subsequent
  tool responses carry `meta.stale == true`, correct `last_good_at`,
  populated `last_error`, and `_stale_warning` inline in the payload.
- Advance virtual clock past `stale_serve_max_age_hours`; assert tool
  responses become JSON-RPC errors with code `-32002` and populated
  `error.data.age_hours`.
- CLI client integration: with `stale: true` in `meta`, assert CLI prints a
  `[stale: ...]` banner above results.

Memory telemetry:
- `daemon/status` returns `MemoryStatus` with `limit_bytes`, `current_bytes`,
  `high_water_bytes` and per-workspace `current_bytes`/`high_water_bytes`.
- High-water mark is monotonic across a rebuild sequence: load → rebuild with
  smaller graph → `current_bytes` decreases, `high_water_bytes` does not.
- Unloading a workspace removes its entry from per-workspace status;
  `total_memory_high_water` does not decrease.

- [ ] **Step 7: Run tests and commit**

Run: `cargo test -p sqry-daemon ipc`
Commit: `feat(daemon): add IPC server with protocol routing`

---

### Task 9: Daemon Binary and Lifecycle

**Files:**
- Create: `sqry-daemon/src/main.rs`
- Create: `sqry-daemon/src/lifecycle.rs`

- [ ] **Step 1: Write daemon main with pidfile locking**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let config = DaemonConfig::load()?;
    setup_logging(&config)?;
    let lock = acquire_pidfile_lock(&config)?;  // flock via fs2
    let plugins = create_plugin_manager()?;
    let manager = Arc::new(WorkspaceManager::new(config.clone(), plugins));
    // Load pinned workspaces
    for ws in &config.workspaces {
        if ws.pinned.unwrap_or(false) {
            manager.get_or_load(&ws.key()).await?;
        }
    }
    let server = IpcServer::bind(&config.socket_path()).await?;
    // Signal readiness (for systemd Type=notify and auto-start pipe)
    signal_ready()?;
    server.run().await
}
```

- [ ] **Step 2: Write lifecycle helpers**

`acquire_pidfile_lock`, `signal_ready`, `handle_shutdown` (SIGTERM handler), `remove_socket_on_exit`.

**Cross-platform pidfile locking:**
- Unix: `flock` via `fs2::FileExt::lock_exclusive()` on `$XDG_RUNTIME_DIR/sqry.lock`
- Windows: `fs2::FileExt::lock_exclusive()` also works on Windows (uses `LockFileEx` internally). The lock file path is `%LOCALAPPDATA%\sqry\sqry.lock`. The `fs2` crate provides cross-platform file locking — no platform-specific code needed.

- [ ] **Step 3: Write service unit generators**

```rust
pub fn generate_systemd_unit(config: &DaemonConfig) -> String { ... }
pub fn generate_launchd_plist(config: &DaemonConfig) -> String { ... }
```

- [ ] **Step 4: Build and verify daemon starts**

Run: `cargo build -p sqry-daemon`
Run: `target/debug/sqry-daemon --help`

- [ ] **Step 5: Commit**

Commit: `feat(daemon): add daemon binary with pidfile locking and service generators`

---

## Phase 3: Client Integration

### Task 10: CLI Daemon Subcommands

**Files:**
- Create: `sqry-cli/src/commands/daemon.rs`
- Modify: `sqry-cli/src/main.rs` (add daemon subcommand)
- Create: `sqry-daemon/src/client.rs` (client library for connecting to daemon)

- [ ] **Step 1: Write daemon client library**

```rust
/// Connect to the daemon over UDS. Auto-start if not running.
pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    pub async fn connect(config: &DaemonConfig) -> Result<Self> { ... }
    pub async fn connect_or_start(config: &DaemonConfig) -> Result<Self> { ... }
    pub async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> { ... }
}
```

- [ ] **Step 2: Implement auto-start with flock**

```rust
async fn auto_start_daemon(config: &DaemonConfig) -> Result<()> {
    let lock_file = fs2::OpenOptions::new().write(true).create(true).open(config.lock_path())?;
    lock_file.lock_exclusive()?;  // blocks until acquired
    // Re-check if daemon is now running (another process may have started it)
    if try_connect(&config.socket_path()).await.is_ok() {
        return Ok(());
    }
    // Fork daemon
    fork_daemon(config)?;
    // Wait for socket to appear (poll with timeout)
    wait_for_socket(&config.socket_path(), Duration::from_secs(10)).await?;
    Ok(())
}
```

- [ ] **Step 3: Add daemon subcommands to CLI**

`sqry daemon start`, `sqry daemon stop`, `sqry daemon status`, `sqry daemon install`, `sqry daemon uninstall`, `sqry daemon load <path>`, `sqry daemon unload <path>`

**[A2026-04-09] `sqry daemon status` memory formatting** per §D of the
amendment. Format human-readable with both current and peak values:

```
sqryd v7.3.0 — uptime 2h 14m
Memory: 450 MB / 2048 MB  (peak: 1.2 GB)

Workspaces (3 loaded):
  ~/repos/main-project      320 MB  (peak: 890 MB)  [pinned, loaded]
  ~/repos/auth-service       80 MB  (peak: 310 MB)  [loaded]
  ~/repos/docs-site          50 MB  (peak:  50 MB)  [loaded, stale: build failed 12m ago]
```

Bytes are rendered via a `human_bytes` helper (B/KB/MB/GB/TB). The `peak:`
suffix is always printed — never hide it — because it is the operator's
primary signal for whether `memory_limit_mb` needs tuning.

A `--json` flag on `sqry daemon status` emits the raw `DaemonStatus` struct
as JSON for scripting / monitoring integrations.

- [ ] **Step 4: Modify existing query commands to try daemon first**

In each query command (search, graph, relations, etc.), add at the top:

```rust
if let Ok(client) = DaemonClient::connect(&config).await {
    return client.send_request("semantic_search", params).await;
}
// Fallback: direct mode (existing code)
```

- [ ] **Step 5: Write tests for daemon subcommands**

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p sqry-cli daemon`
Commit: `feat(cli): add daemon subcommands and daemon-first query routing`

---

### Task 11: LSP Shim Mode

**Files:**
- Modify: `sqry-lsp/src/main.rs` (add `--daemon` flag)
- Create: `sqry-lsp/src/shim.rs`
- Modify: `sqry-lsp/Cargo.toml` (add sqry-daemon client dep)

- [ ] **Step 1: Write LSP shim relay**

```rust
/// Bidirectional byte relay between stdio and daemon UDS.
/// Zero protocol parsing — raw bytes forwarded both directions.
pub async fn run_shim(config: &DaemonConfig) -> Result<()> {
    let mut client = DaemonClient::connect_or_start(config).await?;
    // Send shim/register (4-byte length prefix + JSON)
    client.send_shim_register("lsp", std::process::id()).await?;
    // Bidirectional relay: stdin→UDS, UDS→stdout
    let (reader, writer) = client.split();
    tokio::select! {
        r = tokio::io::copy(&mut tokio::io::stdin(), &mut writer) => r?,
        r = tokio::io::copy(&mut reader, &mut tokio::io::stdout()) => r?,
    };
    client.send_shim_disconnect().await.ok();
    Ok(())
}
```

- [ ] **Step 2: Add --daemon flag to LSP binary**

When `sqry lsp --daemon` is passed, run `run_shim()` **instead of** the standalone LSP server. This completely replaces `build_sqry_service()` and `serve_stdio()` from `sqry-lsp/src/lib.rs:71-190`. The shim mode does NOT construct a `SessionManager`, does NOT start the LSP service tower, and does NOT load any graph. It is a pure byte pump between stdio and UDS.

- [ ] **Step 3: Write integration test**

Spawn daemon, spawn LSP shim, send LSP initialize request through shim, verify response.

- [ ] **Step 4: Run tests and commit**

Run: `cargo test -p sqry-lsp shim`
Commit: `feat(lsp): add daemon shim mode (stdio↔UDS relay)`

---

### Task 12: MCP Shim Mode

**Files:**
- Modify: `sqry-mcp/src/main.rs` (add `--daemon` flag)
- Create: `sqry-mcp/src/shim.rs`
- Modify: `sqry-mcp/Cargo.toml` (add sqry-daemon client dep)

- [ ] **Step 1: Write MCP shim relay**

Same pattern as LSP shim but with `protocol: "mcp"` in the shim/register message.

- [ ] **Step 2: Add --daemon flag to MCP binary**

When `sqry mcp --daemon` is passed, run shim mode. This completely replaces `run_rmcp_server()` from `sqry-mcp/src/main.rs:134`. The shim does NOT construct an `Engine`, does NOT call `bootstrap`/cache setup, and does NOT enter `run_rmcp_server`. It is a pure byte pump between stdio and UDS.

- [ ] **Step 3: Write integration test**

Spawn daemon, spawn MCP shim, send MCP tool call through shim, verify response.

- [ ] **Step 4: Run tests and commit**

Run: `cargo test -p sqry-mcp shim`
Commit: `feat(mcp): add daemon shim mode (stdio↔UDS relay)`

---

## Phase 4: Integration and Polish

### Task 13: End-to-End Integration Tests

**Files:**
- Create: `sqry-daemon/tests/e2e.rs`

- [ ] **Step 1: E2E test — daemon auto-start from CLI**

Start with no daemon, run `sqry search` against a fixture, verify daemon starts and query returns results.

- [ ] **Step 2: E2E test — file change triggers rebuild**

Load workspace, modify a source file, wait for debounce, query again, verify updated results.

- [ ] **Step 3: E2E test — LRU eviction under memory pressure**

Load multiple workspaces to exceed budget, verify oldest is evicted, verify it reloads on next query.

- [ ] **Step 4: E2E test — concurrent LSP + MCP + CLI**

Spawn all three client types against the same workspace, verify all get consistent results.

- [ ] **Step 5: Run full test suite and commit**

Run: `cargo test --workspace`
Commit: `test(daemon): add end-to-end integration tests`

---

### Task 14: Documentation and Version Sync

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`
- Modify: `QUICKSTART.md`
- Modify: `docs/cli/daemon.md` (create if missing)
- Modify: `scripts/sync-versions.sh` (add sqry-daemon)
- Modify: `release-manifest.toml` (add `sqry-daemon` crate entry)

- [ ] **Step 1: Update CHANGELOG with daemon feature**

- [ ] **Step 2: Add daemon usage to QUICKSTART.md**

- [ ] **Step 3: [A2026-04-09] Document `sqry daemon status` output in README and CLI docs**

The high-water mark telemetry added in §D of the 2026-04-09 amendment is the
operator's primary signal for tuning `memory_limit_mb`. It must be discoverable
from the first place a user looks — the README — and fully specified in the
CLI reference doc.

**README.md** — add a "Daemon mode" subsection under the existing architecture
or quickstart section with a worked example:

```
### Daemon mode

Run `sqryd` in the background to keep indexes hot across CLI / LSP / MCP
sessions. Check status and memory usage with:

    $ sqry daemon status
    sqryd v7.3.0 — uptime 2h 14m
    Memory: 450 MB / 2048 MB  (peak: 1.2 GB)

    Workspaces (3 loaded):
      ~/repos/main-project      320 MB  (peak: 890 MB)  [pinned, loaded]
      ~/repos/auth-service       80 MB  (peak: 310 MB)  [loaded]
      ~/repos/docs-site          50 MB  (peak:  50 MB)  [loaded, stale: build failed 12m ago]

The **peak** value is the high-water mark over the workspace's loaded
lifetime — use it to decide whether to raise `memory_limit_mb` in
`~/.config/sqry/daemon.toml`. If peak routinely approaches the limit,
bump it; if peak sits far below 2 GB, the default is fine.
```

**docs/cli/daemon.md** — full CLI reference:

- Every subcommand (`start`, `stop`, `status`, `install`, `uninstall`,
  `load`, `unload`) with flags, exit codes, and example output.
- A dedicated "Tuning memory" section explaining:
  - What the high-water mark measures (peak resident graph memory over the
    loaded lifetime, monotonic until unload/eviction).
  - Why it persists across incremental/full rebuilds but resets on unload.
  - How to interpret it:
    - `peak << limit` → headroom available, default is fine.
    - `peak ≈ limit` → at or near eviction thresholds, consider raising
      `memory_limit_mb` (or accept LRU churn).
    - `peak > limit` is impossible (admission control rejects loads that
      would exceed the budget).
  - Baseline reference: sqry's own codebase (384k nodes / 1.3M edges)
    consumes 150–300 MB per workspace. 2 GB default holds 5–8 repos of
    that class. Monolithic enterprise repos may require 4+ GB.
  - How to tune: edit `~/.config/sqry/daemon.toml` `memory_limit_mb`, or
    set `SQRY_DAEMON_MEMORY_MB=4096` for a one-shot override. Restart the
    daemon (`sqry daemon stop && sqry daemon start`) for the change to
    take effect.
- A `--json` flag example showing the raw `DaemonStatus` payload for
  scripting / monitoring integrations.

**CHANGELOG.md** — under the daemon feature entry, explicitly call out the
high-water mark telemetry as a user-facing feature, not just an internal
metric. Line item: "Daemon reports per-workspace and aggregate memory
high-water marks via `sqry daemon status`, enabling informed tuning of
`memory_limit_mb`."

- [ ] **Step 4: Add sqry-daemon to version sync script**

- [ ] **Step 5: Update release-manifest.toml**

Per CLAUDE.md: any new workspace crate requires a `release-manifest.toml`
entry in the same PR. Add `sqry-daemon` to the crate list. CI will fail
the PR otherwise.

- [ ] **Step 6: Run sync-versions.sh --fix and verify**

- [ ] **Step 7: Verify documentation rendering**

- Render README locally (or on GitHub preview) and confirm the daemon
  status output block displays correctly with monospace alignment.
- Verify `docs/cli/daemon.md` cross-links from README and QUICKSTART.
- Grep the repo for stale references to "no daemon" / "cold start" that
  the daemon now obsoletes, update them.

- [ ] **Step 8: Commit**

Commit: `docs: add sqryd daemon documentation including high-water mark tuning guide`

---

## Task Dependency Graph

```
Task 1 (GraphMemorySize) ──┐
                           ├──► Task 3 (Reverse-Dep) ──► Task 4 (Incremental) ──┐
Task 2 (SourceTreeWatcher) ┘                                                    │
                                                                                ▼
                                                    Task 5 (Crate Scaffold) ──► Task 6 (WorkspaceManager)
                                                                                       │
                                                                                Task 7 (Rebuild Dispatcher)
                                                                                       │
                                                                                Task 8 (IPC Server)
                                                                                       │
                                                                                Task 9 (Daemon Binary)
                                                                                       │
                                                                        ┌──────────────┼──────────────┐
                                                                  Task 10 (CLI)  Task 11 (LSP)  Task 12 (MCP)
                                                                        └──────────────┼──────────────┘
                                                                                       │
                                                                                Task 13 (E2E Tests)
                                                                                       │
                                                                                Task 14 (Docs)
```

**Phase 1** (Tasks 1-2) can be parallelized. Task 3 depends on nothing but is a prerequisite for Task 4. Task 4 depends on Task 3.
**Correct Phase 1 order**: `{Task 1, Task 2} parallel → Task 3 → Task 4 → Task 5`
**Phase 2** (Tasks 5-9) is sequential.
**Phase 3** (Tasks 10-12) can be parallelized after Task 9.
**Phase 4** (Tasks 13-14) is sequential after Phase 3.

---

## Reconciliation with Existing Infrastructure

### sqry-core/src/session/manager.rs

The existing `SessionManager` (lines 329-375) has watcher registration, LRU cache, and idle timeout logic. The daemon's `WorkspaceManager` is **intentionally separate** because:

- `SessionManager` is per-LSP-session (one graph cache, tied to editor lifecycle)
- `WorkspaceManager` is system-wide (multiple graphs, cross-client, independent of any single editor)
- When running in daemon mode, `SessionManager.graph_cache` is **bypassed** — the LSP shim relays to the daemon, which owns the graph via `WorkspaceManager`
- `SessionManager` remains unchanged for standalone (non-daemon) LSP mode

### sqry-mcp/src/server.rs and sqry-mcp/src/lib.rs

The existing MCP server (`server.rs`) and tool routing (`lib.rs`) define the 33 JSON-RPC tools. The daemon:

- **Reuses** the tool handler functions from `sqry-mcp/src/lib.rs` (extracted as library functions)
- **Does NOT reuse** the MCP server loop from `server.rs` — the daemon has its own IPC server that routes MCP tool calls to the same handler functions
- The existing `sqry-mcp` binary gains a `--daemon` shim mode that relays stdio to the daemon's UDS
- In standalone mode (no daemon), `sqry-mcp` continues to work as before via `server.rs`

### sqry-core/src/session/watcher.rs

The existing session watcher monitors `.sqry/graph/manifest.json` for staleness (mtime/size/inode checks). The daemon's `SourceTreeWatcher` is different:

- Session watcher: non-recursive, watches **index artifacts** for cache invalidation
- SourceTreeWatcher: recursive, watches **source files** for change detection + rebuild triggering
- Both coexist — session watcher is for non-daemon mode, SourceTreeWatcher is for daemon mode

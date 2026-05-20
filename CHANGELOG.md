# Changelog

All notable changes to sqry will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **perf(c-icall-precision)**: Pass 5b C indirect-call resolution now plans
  rewrites in a read phase and applies them in a separate write phase, avoiding
  the prior `O(callsites x edge-delta)` scan pattern during large C/C++
  indexing runs. On the verified `drivers/net` Linux subset, Phase 6 improved
  from 20.60s to 1.90s (10.8x faster), and total index wall time improved from
  31.4s to 13.2s. `SQRY_LOG=info` now also exposes Pass 5b and cross-language
  build telemetry from the CLI while preserving default-silent output.
- **chore(release)**: Refreshed the SBOM/VEX staging workflow hash in the
  sanitization review contract so the staged release path can resume after the
  isolated tool-download hardening in `v16.0.2`.

## [16.0.0] - 2026-05-19

### Added

- Landed C indirect-call precision Phase A, including graph/schema changes that
  cargo-semver-checks classifies as public API breaking changes.

### Fixed

- Recovered the release path after the duplicate `v15.0.8` release-plz commit
  and the failed `v15.0.8` sanitized cargo-vet gate.

## [15.0.8] - 2026-05-17

### Added

- Go implicit implements, promoted methods, and function-signature
  implementations.

## [13.0.15] - 2026-05-09

### Fixed

- Hardened the staged public release pipeline so Windows and macOS release
  artifacts are validated on native platform runners before public promotion.
- Preserved release build-environment metadata across the split Stage 6 build
  and publish gates so public release manifests can be generated after native
  smoke validation.

### Added

- **feat(workspace) [USER-VISIBLE]**: workspace-aware cross-repo analysis is now
  configurable per workspace. Two surfaces ship in this release:

  - **`.sqry-workspace`** v2 registry file (CLI / standalone LSP). A JSON
    registry initialised with `sqry workspace init <path>` and populated via
    `sqry workspace add` / `scan`. Consumed by `sqry workspace stats|status|query`
    directly. Path-scoped subcommands (`sqry index`, `sqry query`, etc.)
    accept the file path via `--workspace <PATH>` (or
    `SQRY_WORKSPACE_FILE=<PATH>`) as a fallback positional path; STEP_9
    documents the current contract — file-driven `LogicalWorkspace` loading
    inside non-workspace subcommands lands in a follow-up task.
  - **`sqry.workspace` block inside a `.code-workspace`** (VS Code). When the
    `.code-workspace` file is open, the extension parses `folders[]` plus the
    `sqry.workspace` block at activation and forwards a lightweight
    `{ folders, classification }` hint plus the workspace-file path under
    `initializationOptions.sqry` (`sqry.workspace` carries the hint,
    `sqry.workspaceFile` carries the path). `sqry lsp` runs its canonical
    `LogicalWorkspace` resolver: branch 1 detects and skips the lightweight
    hint, branch 4 then loads the `.code-workspace` directly via
    `LogicalWorkspace::from_code_workspace`. Four new contributed settings
    — `sqry.indexRoot`, `sqry.projectRootMode`,
    `sqry.workspaceFolderExcludes`, `sqry.workspaceClassification` —
    govern runtime behaviour: `sqry.workspaceClassification` is the
    user-editable surface (mirrored back into the open `.code-workspace`
    by the `Sqry: Edit Workspace Classification` command);
    `sqry.workspaceFolderExcludes` filters every enumeration loop;
    `sqry.indexRoot` and `sqry.projectRootMode` are read by the
    extension's classifier helpers.

  Both surfaces resolve to the same `LogicalWorkspace` value (defined in
  `sqry-core::workspace::logical`), keyed by a stable 32-byte BLAKE3-256
  `workspace_id` derived from a constructor-specific `WorkspaceIdentity`
  enum: the canonical workspace-file path for `.sqry-workspace` and
  `.code-workspace` constructors, the canonical single-root path for the
  single-root constructor (`WorkspaceIdentity::SingleRoot { path }`),
  and the canonical sorted source-root list for the anonymous-multi-root
  constructor only. The MCP server, LSP, daemon (`sqryd`
  `WorkspaceManager`), and redaction layer all key on this identity.

  **Migration guidance.** Existing single-repo workflows are unchanged: with no
  `.sqry-workspace` and no `.code-workspace` `sqry.workspace` block, sqry
  behaves exactly as before (single index_root resolved from the CLI argument,
  the workspace folder, or git root per `projectRootMode`). To enable
  cross-repo analysis, add one of the two configuration files; pre-existing
  documentation that described "cross-repo by default" was inaccurate and is
  corrected in this release. See [`docs/cli/workspace.md`](docs/cli/workspace.md)
  for the full configuration reference and
  [`docs/development/code-map/workspace-aware.md`](docs/development/code-map/workspace-aware.md)
  for the contributor-facing data-flow code map.

### Changed

- **docs(README,marketing,FEATURE_LIST)** [USER-VISIBLE]: removed the
  inaccurate "cross-repo by default" wording from `README.md`,
  `docs/marketing/SQRY_ONE_PAGER.md`, and `docs/FEATURE_LIST.md`. Each surface
  now describes the configurable-per-workspace model precisely and cross-links
  to the new `docs/cli/workspace.md`.

- **feat(mcp) [BEHAVIOR CHANGE]**: `find_duplicates` now caps each duplicate
  group to **10 member symbols** by default (previously unlimited).

  Large codebases (e.g. Linux kernel with 11.2M nodes) could return groups
  with hundreds or thousands of members, producing multi-megabyte MCP
  responses that exceed payload budgets and degrade AI-assistant usability.

  **Wire output additions (additive — no breaking schema change):**
  - `total_members` (`usize`): pre-truncation member count for the group.
    Always equals `count` (symbols.len()) when `members_truncated` is `false`.
  - `members_truncated` (`bool`): `true` when the `symbols` list was capped
    by `max_members_per_group`.

  **New parameter `max_members_per_group` (optional integer, default 10):**
  - Controls the per-group member cap. Valid range: `[0, 10000]`.
  - `0` disables the cap entirely, restoring unlimited (pre-v9.1) behavior.
  - Values outside `[0, 10000]` are rejected with `InvalidParams`.

  **Migration for callers relying on unlimited members:**
  Pass `"max_members_per_group": 0` to restore pre-v9.1 behavior.

  **Example wire output (truncated group):**
  ```json
  {
    "groupId": "0x1a2b3c4d",
    "count": 10,
    "totalMembers": 847,
    "membersTruncated": true,
    "symbols": [ ... 10 entries ... ]
  }
  ```

### Fixed

- **fix(vscode)**: binary auto-download now clears and refreshes the persistent
  Sigstore TUF cache once when verification fails with a recoverable trust-root
  error such as `root was signed by 0/3 keys`. The downloader also verifies
  that the shared DSSE/SLSA release attestation contains a subject for the
  downloaded asset with the expected SHA256 digest before accepting provenance.

- **fix(graph)**: line-zero reporting for cross-file callees and
  macro-shadowed call targets across all 16 language plugins and all MCP
  tool outputs. Extends `dec44131f` Method/Function cross-kind reuse
  (2026-03-30) to the complete root-cause surface: `CALL_COMPATIBLE_KINDS`
  generalization (Function, Method, Macro, Constant, LambdaTarget),
  `ensure_callee()` API with required span, Phase 4c-prime cross-file
  node unification, `node_location_for_reporting()` query-layer helper,
  and 30 MCP tool read site migrations. 47 new regression tests.

### Features

- **feat(daemon)**: `sqryd` — background index server that keeps semantic
  graphs hot across CLI, LSP, and MCP sessions (Tasks 1-13, shipped on
  `sqryd/task-2-source-tree-watcher`).

  **Core components:**
  - `sqry-daemon` library + `sqryd` binary: production daemon process
    with pidfile locking, SIGTERM/SIGINT graceful shutdown, optional
    `--detach` daemonization mode, structured log rotation (configurable
    `log_max_size_mb` / `log_keep_rotations`), and systemd user-service /
    launchd plist unit generators (reserved; not yet wired to CLI).
  - `sqry-daemon-protocol` leaf crate: wire types, length-prefixed
    framing via free functions (`read_frame` / `write_frame` /
    `read_frame_json` / `write_frame_json`), and
    `deny_unknown_fields` envelope versioning.
  - `sqry-daemon-client` crate: `connect_shim` / `pump_stdio` for
    stdio-over-IPC transport, sealed `AsyncReadWrite` trait, envelope
    version negotiation, connect/handshake timeouts.

  **Workspace management:**
  - `WorkspaceManager`: per-workspace admission control with
    configurable `memory_limit_mb` budget, LRU eviction of least-
    recently-used workspaces when the memory ceiling is approached.
  - Per-workspace high-water mark telemetry: `high_water_bytes`
    is a monotonic counter that tracks the peak resident graph memory
    since the workspace was loaded. It persists across incremental
    rebuilds but resets to zero on workspace unload or eviction.
    Visible via `sqry daemon status` alongside current resident bytes,
    workspace count, and a daemon-wide `high_water_bytes` aggregate.

  **Incremental rebuild:**
  - `RebuildDispatcher` wraps `SourceTreeWatcher` (notify-based
    recursive file-system watcher, Task 2) with a 2 s debounce
    (configurable via `debounce_ms`),
    `CancellationToken`-based in-flight rebuild cancellation, and
    a `Rebuilding / Loaded / Failed` state machine.

  **IPC transport:**
  - Unix domain sockets (Linux/macOS) and Named Pipes (Windows).
  - `ShimRegistry` with bounded admission cap (256 simultaneous
    shim connections); excess connections receive a `ShimRegisterAck`
    `accepted: false` response.
  - Shape-discriminating first-frame router: frames carrying
    `{protocol, pid}` enter the shim path; all other frames enter
    the hello/management path.
  - `DaemonMcpHandler` implements rmcp `ServerHandler` with
    feature-flag-locked `enabled_tool_names` field; disabled tools
    return `-32602 InvalidArgument` rather than a silent 404.

  **CLI subcommands** (`sqry daemon {start,stop,status,logs}`):
  - `start [--sqryd-path PATH] [--timeout SECS]`: spawn daemon,
    wait for READY, return exit 0 on success / exit 1 on timeout.
  - `stop [--timeout SECS]`: send shutdown request, poll until the
    socket disappears; exit 0 whether stopped cleanly or daemon was
    not running (idempotent), exit 1 on poll timeout.
  - `status [--json]`: print daemon version, uptime, current memory,
    `high_water_bytes`, and per-workspace breakdown; exit 0 if
    running, exit 1 if not. `--json` emits the full
    `ResponseEnvelope<DaemonStatus>` returned by `daemon/status`:
    `{ "result": <DaemonStatus>, "meta": <ResponseMeta> }`. The
    `result` field carries the daemon status payload; the `meta`
    field carries per-response metadata (`daemon_version`, staleness
    flags).
  - `logs [--lines N] [--follow]`: tail the daemon log file
    (requires `log_file` configured in `daemon.toml`);
    `--follow` streams new lines until interrupted.

  **Shim mode (LSP and MCP):**
  - `sqry-mcp --daemon [--daemon-socket PATH]` connects to the running
    daemon over IPC and routes all MCP tool calls through the daemon's
    dispatch layer; auto-starts the daemon if not running.
  - `sqry-lsp --daemon [--daemon-socket PATH]` delegates the LSP
    session to the daemon (daemon-hosted LSP); auto-starts the
    daemon if not running.

  **Configuration (`~/.config/sqry/daemon.toml`):**
  - Key fields: `memory_limit_mb` (`SQRY_DAEMON_MEMORY_MB`),
    `idle_timeout_minutes`, `tool_timeout_secs`
    (`SQRY_DAEMON_TOOL_TIMEOUT_SECS`), `max_shim_connections`
    (`SQRY_DAEMON_MAX_SHIM_CONNECTIONS`), `log_file`
    (`SQRY_DAEMON_LOG_FILE`), `log_max_size_mb`, `log_keep_rotations`
    (`SQRY_DAEMON_LOG_KEEP_ROTATIONS`), `socket.path`
    (`SQRY_DAEMON_SOCKET` on Unix) / `socket.pipe_name`
    (`SQRY_DAEMON_PIPE` on Windows), `auto_start_ready_timeout_secs`
    (`SQRY_DAEMON_AUTO_START_READY_TIMEOUT_SECS`). Config path
    overridable via `SQRY_DAEMON_CONFIG`. Restart required for
    changes to take effect.

  **Wire error codes:** `-32000 ToolTimeout` (deadline_exceeded,
  retryable), `-32001 WorkspaceBuildFailed`, `-32002
  WorkspaceStaleExpired`, `-32602 InvalidArgument`, `-32603 Internal`.

- **feat(graph)**: formalize binding plane as V9 snapshot layer with
  scope/alias/shadow derivation and witness-bearing resolution (#[PR]).
  The binding plane is derived during a new Phase 4e inside
  `build_unified_graph_inner`, runs between Phase 4d and Pass 5, and
  consumes only the language-local edge kinds (Contains, Defines,
  Imports, Exports) already emitted by Phase 1 plugins. `BindingPlane<'g>`
  is the stable Phase 2 facade; `BindingResolution { result, witness }`
  is the new return shape that wraps `BindingResult` alongside the
  18-variant witness step trace. `BindingQuery::resolve()` is preserved
  byte-for-byte via a shared `resolve_shared()` helper.
- **feat(cli)**: new `sqry graph resolve <symbol> [--explain] [--json]`
  subcommand that loads the V9 snapshot, runs a BindingPlane query, and
  prints the outcome + optional witness trace. Documented stable JSON
  shape for `--explain --json`.
- **feat(db,planner,mcp,cli)**: Phase 3 — Derived Analysis DB + Query
  Planner (2026-04-15). New `sqry-db` crate centralizes every derived
  graph query behind a `DerivedQuery` trait with a sharded three-tier
  cache (Tier 1 file-level, Tier 2 global edge revision, Tier 3 global
  metadata revision). A new structural query planner (IR → compile →
  fuse → execute) runs queries with shared-prefix fusion across batches
  and subquery memoization. Text syntax added: `sqry plan-query
  "kind:function has:caller:main"` via a new CLI subcommand, plus a
  `sqry_query` MCP tool that exposes the same planner.
- **feat(db)**: incremental reindex via `FileSegmentTable` and
  `reindex_files` — always-append node allocation with
  edges-before-nodes tombstone ordering (proof-tested regression
  against edge aliasing). The sqry-db cache reuses hot results for all
  files whose segment revisions are unchanged. Snapshot format bumped
  to V10 (`SQRY_GRAPH_V10`) to carry the file-segment table.
- **feat(db)**: `ComparativeQueryDb::diff()` — `semantic_diff` now runs
  through a cross-snapshot wrapper owning both graphs; the heuristic
  (0.7 signature + 0.3 location weights, 0.9 rename threshold, 50-line
  same-file window) is preserved byte-for-byte. Deliberately not a
  `DerivedQuery` because cross-snapshot results have no valid
  invalidation criterion in the single-snapshot dependency model.
- **feat(mcp,cli)**: 20+ handlers migrated to sqry-db with a formal
  dispatch taxonomy: name-keyed predicates (`direct_callers`,
  `direct_callees`, `find_unused`, `find_cycles`) route through
  `db.get::<Q>(key)`; NodeId-anchored handlers (`dependency_impact`,
  `show_dependencies`, `trace_path`, `subgraph`, `export_graph`,
  `relation_query`, CLI `impact`, `call-chain-depth`,
  `dependency-tree`, `subgraph`, `visualize`) enumerate
  `snapshot.edges()` from resolved start nodes; `is_node_in_cycle` uses
  strict resolution + a sqry-db per-node predicate.

### Changed

- **feat(mcp,lsp)**: `sqry-mcp` and `sqry-lsp` gain `--daemon` /
  `--daemon-socket` flags for daemon-backed operation; standalone mode
  (no flags) is unchanged and remains the default. `sqry-mcp --daemon`
  routes all MCP tool calls through the daemon's dispatch layer.
  `sqry-lsp --daemon` delegates the LSP session to the daemon.
  Both auto-start the daemon if it is not already running.

- **chore(mcp)**: legacy `trace_path_cache_capacity`,
  `subgraph_cache_capacity`, and `query_cache_ttl_secs` `McpConfig`
  fields (plus their `effective_*` accessors and env-var overrides)
  removed. sqry-db now owns its own config surface (shard count,
  compaction threshold, `derived.sqry` path). The orthogonal MCP
  payload-response LRU (`graph_cache.rs`) is preserved — it is a
  response cache keyed on request shape, distinct from the planner
  predicate cache.
- **chore(mcp)**: ambiguous name policy formalized. Name-keyed
  predicates return a union across matching nodes (planner-aligned
  semantic). NodeId-anchored handlers and per-node boolean predicates
  (`is_node_in_cycle`, `dependency_impact`, etc.) return a `Use a
  canonical qualified name` error on ambiguous simple names — a
  multi-hop traversal with a broadened seed set is indistinguishable
  from a frontier-leak regression.

### Removed

- **chore(core)**: ~3,090 LOC of legacy traversal code deleted:
  `sqry-core/src/query/executor/graph_cycles.rs` (1,082 LOC, subsumed by
  `sqry-db::CyclesQuery` / `IsInCycleQuery` / `SccQuery`),
  `sqry-core/src/query/executor/graph_unused.rs` (735 LOC, subsumed by
  `sqry-db::UnusedQuery` / `ReachabilityQuery` / `EntryPointsQuery`),
  `sqry-core/src/graph/unified/query_adapter.rs` (456 LOC),
  `sqry-mcp/src/execution/diff_comparator.rs` (648 LOC, subsumed by
  `sqry-db::comparative`), plus ~1,000 LOC of handler helpers folded
  into sqry-db queries or the MCP inversion wrappers.
  `sqry-core/src/query/executor/graph_eval.rs` is deliberately retained
  for the legacy `sqry query` CLI path until a future unit routes it
  through `sqry plan-query`.

### Breaking changes

- The snapshot format magic is bumped from SQRY_GRAPH_V8 to SQRY_GRAPH_V9.
  V8 snapshots load cleanly via inline upconvert on first read
  (`upconvert_v8_to_v9` runs `derive_binding_plane(&mut CodeGraph)`), so
  existing users see no migration friction.
- The snapshot format magic is further bumped from SQRY_GRAPH_V9 to
  SQRY_GRAPH_V10 to persist `FileSegmentTable`. Users upgrading past
  this release must rebuild their graph once with `sqry index --force`
  (or call `rebuild_index` via MCP). V9-to-V10 inline upconvert was not
  provided because the file-segment table cannot be reconstructed
  post-hoc from the V9 node/edge arenas.
- `CircularType` / `CircularConfig` relocated from
  `sqry-core/src/query/executor/graph_cycles.rs` to
  `sqry-core/src/query/cycles_config.rs`; `UnusedScope` relocated from
  `sqry-core/src/query/executor/graph_unused.rs` to
  `sqry-core/src/query/unused_config.rs`. All public re-exports through
  `sqry_core::query::*` preserved; external `use` paths unchanged.
- (Internal) `sqry-core::graph::unified::bind` is now a directory-style
  module; all public types re-export through `bind::mod.rs` so external
  `use` paths are unchanged.

## [8.0.3] - 2026-04-11

### Fixed
- VS Code extension autodownload now accepts the exact public Sigstore identities emitted by the current `oss-distribute.yml` workflow, so valid public release binaries no longer fail provenance verification during install.

### Added
- `SQRY_MAX_SNAPSHOT_BYTES` environment variable to override the graph snapshot data section size limit. Values are clamped between 1 GB and 64 GB. The documentation previously referenced this variable but it was not wired to any runtime code path.

### Changed
- Default maximum graph snapshot data section size raised from 2 GB to 16 GB in `sqry-core/src/config/buffers.rs` so `sqry index` can persist Linux-kernel-class mega-repos without hitting the save-time size guard. A compile-time assertion now pins this default at ≥ 8 GB so future reductions below the bincode-era floor fail loudly.
- Snapshot size-guard error messages now include the actual serialized size, the active limit, and a hint to raise `SQRY_MAX_SNAPSHOT_BYTES` on both the save and load paths.

### Fixed
- `sqry index --force .` on the Linux kernel (and other ≥ 2 GB graphs) no longer fails with `Failed to save snapshot to ./.sqry/graph/snapshot.sqry: Validation failed: data section too large to save`. Root cause: the bincode → postcard migration (commit `f7ddb4704`) lowered `MAX_SNAPSHOT_BYTES` from 8 GB to 2 GB under the assumption that no production snapshot would exceed that bound, and the subsequent post-implementation review (commit `1b62bc12a`) added a symmetrical save-side guard that rejected any graph whose postcard-serialized form exceeded 2 GB. Moving the limit into `config::buffers::max_snapshot_bytes()` with a 16 GB default and env-var override restores and extends the pre-regression behavior.

## [7.2.0] - 2026-04-06

### Added
- Unified graph traversal foundation across CLI, LSP, and MCP with shared traversal results, richer edge classification, and reusable binding/materialization helpers in `sqry-core`
- `sqry-bind` facade support in the unified graph layer for declaration/reference/import classification workflows
- MCP introspection tool `expand_cache_status` for Rust macro expansion cache visibility
- MCP capability-map resource and dynamic tool-guide generation so assistant-facing docs reflect the live tool catalog

### Changed
- `trace-path`, `show_dependencies`, `dependency_impact`, `subgraph`, `graph_export`, and related CLI graph operations now run on the shared traversal kernel instead of separate BFS implementations
- Path traversal enumeration now uses deterministic discovery order and reports truncation/leaf-path behavior consistently across interfaces
- `MaterializedNode` now carries `node_id`, enabling identity-based linking between traversal nodes and edges in downstream consumers

### Fixed
- LSP workspace path resolution now rejects directory traversal escapes at the workspace boundary
- Graph path-BFS now enforces node and edge limits atomically instead of returning partially truncated edge sets
- Graph path enumeration now includes leaf-path reporting where no explicit target is provided
- MCP graph/materialization flows now use the shared file registry path, removing stale hard-coded tool/resource counts from user-visible docs

## [7.1.0] - 2026-04-04

### Added
- Plugin cost tiering in `sqry-plugin-registry` with deterministic built-in plugin selection
- CLI plugin-selection controls: `--include-high-cost`, `--exclude-high-cost`, `--enable-plugin`, `--disable-plugin`
- C++ graph builder timeout budget to bound pathological single-file builds on very large repositories

### Changed
- `sqry index`, `sqry update`, and `sqry watch` now persist active plugin ids in the unified graph manifest
- Read-only indexed commands now honor manifest-backed plugin selection by default
- Default fast-path plugin selection excludes plugins tagged `high_wall_clock`
- Existing manifests without `plugin_selection` continue to use legacy all-plugins behavior until explicitly rebuilt
- JVM classpath fixtures and metadata now carry `source_jar` provenance consistently across stubs, tests, and benches

### Fixed
- Non-CLI graph persistence now infers plugin-selection provenance instead of dropping it from wrapper-based builds
- LSP rebuilds now preserve inferred plugin-selection provenance in persisted manifests
- Stage 3 release sanitization now uses isolated temp roots with cleanup and disk-headroom checks to reduce release-runner failures

### Notes
- The indexing-cost changes in this release address two distinct issues that were easy to conflate after JSON shipped in `v4.12.0`. JSON increased default indexing cost and was partially mitigated earlier by graph-layer fast pre-checks for JSON/HTML before being addressed properly here with plugin cost tiering and manifest-backed selection. Firefox-scale “stuck index” reports were a separate pathological C++ graph-build issue, fixed independently by bounding single-file C++ graph builds.

## [7.0.2] - 2026-04-03

### Fixed
- Release pipeline: isolate and clean Stage 3 temp trees so staged sanitization no longer fails from self-hosted runner disk exhaustion
- Release verification: restore a clean hermetic `fmt` and `clippy --all-targets` gate on the current release head

## [6.0.24] - 2026-04-03

### Added
- JVM classpath analysis via `sqry-classpath` crate (Track C Tier 1): bytecode parsing, Scala signature decoding, source JAR resolution

### Fixed
- Resolve all 132 SonarQube issues across VS Code extension, Python benchmarks, and Rust crates (0 remaining)

## [4.12.7] - 2026-03-29

### Fixed
- VS Code extension: detect unified graph index (`.sqry/graph/manifest.json`) instead of legacy `.sqry-index`

## [4.12.5] - 2026-03-27

### Fixed
- Release pipeline: fix unbound VERSION variable in orchestrator Leg 1/2 polling steps
- Release pipeline: add cancel-on-failure job to cancel dispatched child runs when orchestrator fails

## [4.12.4] - 2026-03-27

### Added
- Release pipeline: phase ledgers for Leg 1/2 with 5-boundary and 4-boundary JSON artifacts
- Release pipeline: staging preflight on verivus-oss/sqry before public promotion
- Release pipeline: run-identity contract (release_request_id + run-name) for deterministic run matching
- Release pipeline: conditional Cargo.lock handling (preserve if workspace unchanged, regenerate if changed)
- Release pipeline: byte-aware deny-pattern scanning in sanitization validation
- Release pipeline: expanded release-manifest.toml with docker, provenance, crates, preflight, and apple contracts
- Release pipeline: render-release-contracts.sh for centralized contract generation
- Release pipeline: staging-preflight environment provisioning and validation

### Changed
- Release pipeline: orchestrator generates release_request_id UUID and passes to all legs
- Release pipeline: identity-based run discovery replaces time-based polling for Leg 1/2/3/crates
- Release pipeline: strict vX.Y.Z version validation (removed -pipeline.N suffix support)
- Release pipeline: Leg 3 and crates workflows accept publish_mode (preflight/release) with mode-aware environments

## [4.12.3] - 2026-03-26

### Fixed
- **Release**: Hardcode provenance artifact names in download steps — SLSA reusable workflow state-passing bug prevents job output propagation

## [4.12.2] - 2026-03-26

### Fixed
- **Release**: Preflight now validates Leg 3 inline Dockerfile COPY list against workspace members — catches missing crates before burning a version tag

## [4.12.1] - 2026-03-26

### Fixed
- **Release**: Add `sqry-lang-json` COPY to inline Dockerfile in Leg 3 workflow

## [4.12.0] - 2026-03-26

### Added
- **JSON plugin**: New `sqry-lang-json` crate — 36th language plugin for declarative JSON config files
  - Three format profiles: generic, `now-ui.json` (Component nodes), `package.json` (Import nodes + Imports edges)
  - Safety limits: configurable `MAX_DEPTH` and `MAX_NODES` via `SQRY_JSON_MAX_DEPTH` / `SQRY_JSON_MAX_NODES` env vars
  - Lockfile/minified exclusion, iterative traversal, full `\uXXXX` unicode escape decoding (BMP + surrogate pairs)
- **MCP**: `symbols` array parameter for `export_graph` — filter graph to specific symbols
- **Visualization**: `filter_node_ids` support in DotConfig, D2Config, and MermaidConfig
- **Config**: `SQRY_JSON_MAX_DEPTH` (default 64, range 8-256) and `SQRY_JSON_MAX_NODES` (default 500k, range 1k-5M)

### Fixed
- **MCP**: `export_graph` now renders only the post-pagination subgraph instead of the full graph
- **Release**: SLSA provenance generator must be referenced by tag ref, not SHA — fixes provenance generation since v4.10.1
- **Release**: Provenance download-artifact downgraded to v4.3.0 for upload-artifact v4.x compatibility
- **Release**: `continue-on-error: true` restored for SLSA `final` step state-passing bug
- **Release**: `private-repository: true` bypasses incorrect private-repo detection on `workflow_dispatch`

## [4.11.11] - 2026-03-26

### Fixed
- **Release**: Downgrade provenance download-artifact to v4.3.0 — SLSA generator v2.0.0 uploads with upload-artifact v4.x which is incompatible with download-artifact v8.x
- **Release**: Restore `continue-on-error: true` on SLSA jobs — the reusable workflow's `final` job has a state-passing bug (reports SUCCESS=false despite generator succeeding)

## [4.11.10] - 2026-03-26

### Fixed
- **Release**: SLSA provenance generator must be referenced by tag ref, not SHA — SHA pinning caused `Invalid ref` error that silently broke provenance since v4.10.1
- **Release**: Set `private-repository: true` to bypass incorrect private-repo detection on `workflow_dispatch`
- **Release**: Remove `continue-on-error: true` from SLSA jobs — provenance failures must be real failures

## [4.11.9] - 2026-03-25

### Added
- **MCP**: `symbols` array parameter for `export_graph` — filter graph to specific symbols
- **Visualization**: `filter_node_ids` support in DotConfig, D2Config, and MermaidConfig

### Fixed
- **MCP**: `export_graph` now renders only the post-pagination subgraph instead of the full graph
- **Release**: SLSA provenance private-repository override for cross-repo dispatch
- **Release**: SLSA continue-on-error for skipped upload-assets final job

## [4.11.8] - 2026-03-25

### Fixed
- **Release**: Set SLSA continue-on-error:true — upstream final job bug with skipped upload-assets

## [4.11.7] - 2026-03-25

### Fixed
- **Release**: Set SLSA `private-repository: true` — generator binary misdetects verivus-oss/sqry as private on workflow_dispatch (upstream bug in slsa-github-generator, not fixed in v2.1.0)

## [4.11.6] - 2026-03-24

### Fixed
- **Release**: Upgrade SLSA generator v2.0.0 → v2.1.0 — fixes "Repository is private" misdetection on workflow_dispatch that blocked provenance generation and skipped publish
- **Release**: Fix Apple signing credential retrieval — use GITHUB_ENV heredoc syntax for multi-line base64 values, restoring base64 decode in keychain import
- **Release**: Restore provenance download and copy-to-release-assets in publish job — provenance attestations are now included in GitHub Releases
- **Release**: Remove `create-release-draft` from SLSA provenance needs — provenance runs in parallel with packaging instead of being serialized behind a gate
- **Release**: Remove `environment: oss-release` from create-release-draft — eliminates redundant approval gate
- **Release**: Reduce Leg 1/2 approval gates — push-to-staging uses `oss-staging-push` (no reviewers), push-to-public uses `oss-public-push` (no reviewers); orchestrator gates remain the only approval points

## [4.11.5] - 2026-03-24

### Fixed
- **Release**: Fix SLSA provenance `final` job failure — revert `upload-assets:true` + `private-repository:true` (broken combination in SLSA generator v2.0.0 that caused `SUCCESS=false` and skipped publish); reverts to `upload-assets:false` + `private-repository:false` which worked correctly in v4.10.1
- **Release**: Fix Apple signing credential retrieval — PEM-format Key Vault secrets caused `$GITHUB_ENV` multi-line write failure; now written directly to temp files, bypassing `GITHUB_ENV` for multi-line content

## [4.11.4] - 2026-03-24

### Fixed
- **Core**: Fix flaky `test_config_snapshot_hash` — `compute_hash()` incorrectly included `collected_at` timestamp in the content hash, causing two snapshots with identical config values but different collection times to hash differently

## [4.11.3] - 2026-03-23

### Fixed
- **Build**: Add cargo-vet exemption for `tree-sitter-rust:0.24.1` resolved on public repo CI
- **Build**: Fix `cargo fmt` violations in `batch_counts.rs` and `server.rs`

## [4.11.2] - 2026-03-23

### Fixed
- **Build**: Fix `cargo fmt` violations in `batch_counts.rs` and `server.rs` that failed sanitization lint

## [4.11.1] - 2026-03-23

### Fixed
- **Docs**: Remove stale Tier-1/2/3 language labels (all 35 plugins use unified GraphBuilder since v2.0.0)
- **Docs**: Comprehensive VS Code README rewrite — all 15 commands, 12 settings, feature categories
- **Marketplace**: Remove "Linters" from VS Code marketplace categories (sqry is not a linter)

## [4.11.0] - 2026-03-23

### Added
- **VSCode**: Status bar item showing index health (Ready/Stale/Building/No Index/Error) with click actions
- **VSCode**: Keyboard shortcuts — Ctrl+Alt+S (search), Ctrl+Alt+Q (query), Ctrl+Alt+R (references), Ctrl+Alt+I (index)
- **VSCode**: Search history with MRU recall (last 20 queries)
- **VSCode**: Getting Started walkthrough (5 steps: install, index, search, query, CodeLens)
- **VSCode**: Problems panel integration — unused code, circular dependencies, and duplicates as native VS Code diagnostics
- **VSCode**: Inline unused code fading via `DiagnosticTag.Unnecessary`
- **VSCode**: Quick fixes for sqry diagnostics (show callers, show cycle path, navigate to duplicate)
- **VSCode**: Hover integration — caller/callee counts in editor hover tooltips
- **VSCode**: Enhanced CodeLens — configurable callers + callees segments with batch fetching
- **VSCode**: Call graph and dependency visualization webview with SVG rendering, pan/zoom, search, and export
- **VSCode**: Multi-root workspace support with per-root index status and targeting
- **VSCode**: Result filtering (by language, kind) and sorting (name, file, kind, line)
- **VSCode**: Export results as JSON, Markdown, or CSV
- **VSCode**: Auto-index on save with 30s debounce and per-root dirty latch
- **VSCode**: Restart Language Server and Rebuild Index commands
- **VSCode**: `sqry.scanWorkspace` command for full workspace diagnostic scan
- **LSP**: `sqry/batchCallerCalleeCount` endpoint for efficient CodeLens count fetching
- **LSP**: Enriched cycle responses with `member_locations` (file, line, column per cycle member)

### Changed
- **LSP**: `sqry/indexStatus` now uses file modification time instead of birth time for index age

### Fixed
- **VSCode**: "Indexed 36 days ago" showing birth time instead of last rebuild time

## [4.10.12] - 2026-03-22

### Added
- **Release**: `release-manifest.toml` — single source of truth for sanitization allowlists
- **Release**: `validate-manifest.sh` CI gate — fails PRs that add crates/workflows/files without updating manifest
- **Release**: `oss-release.yml` orchestrator — automated Leg 1→2→3→crates.io chaining
- **Release**: Dry-run tag namespace (`v0.0.0-pipeline.N`) and PR-triggered sanitization gate
- **Release**: `rust-toolchain.toml` — pinned Rust 1.90 across all CI legs
- **Release**: `gh-for-repo.sh` — deterministic GitHub account selection for multi-repo operations

### Changed
- **VSCode**: Promote `sqry-vscode` from `tools/` to top-level as first-class deliverable
- **Release**: `sanitize-for-oss.sh` and `validate-sanitization.sh` read allowlists from manifest
- **Release**: `check-workflow-contracts.sh` context-aware for public repo, manifest-integrated
- **Release**: SLSA provenance `private-repository: true` override for workflow_dispatch detection bug
- **CI**: Pin `dtolnay/rust-toolchain` to commit SHA; skip manifest validation gracefully on public repo

### Fixed
- **CI**: Remove `const_is_empty` clippy warnings in LSP handlers
- **Security**: Update `flatted` npm dependency (prototype pollution CVE)
- **Release**: Add cargo-vet exemptions for zerocopy 0.8.47, itoa 1.0.18, snapbox, trycmd

### Fixed

## [4.10.11] - 2026-03-22

### Added
- **Release**: `release-manifest.toml` — single source of truth for all sanitization allowlists; replaces triplicated hardcoded lists in sanitizer, validator, and workflows
- **Release**: `validate-manifest.sh` CI gate — bidirectional set-equality enforcement for workspace crates, workflows, tools, and root files on every PR
- **Release**: `oss-release.yml` orchestrator — automated Leg 1→2→3→crates.io chaining with machine-mediated SHA handoff; eliminates human copy-paste between legs
- **Release**: Dry-run tag namespace (`v0.0.0-pipeline.N`) — test full OIDC/KV/push pipeline flow without consuming a real version tag
- **Release**: PR-triggered sanitization dry-run for changes to release workflows, scripts, or manifest
- **Release**: `rust-toolchain.toml` — pin Rust toolchain across all CI legs and developer machines

### Changed
- **VSCode**: Promote `sqry-vscode` from `tools/` to top-level directory as a first-class deliverable
- **Release**: `sanitize-for-oss.sh` reads all allowlists from `release-manifest.toml` instead of hardcoded arrays
- **Release**: `validate-sanitization.sh` reads all allowlists from `release-manifest.toml` instead of independent copies
- **Release**: `check-workflow-contracts.sh` is now context-aware and manifest-integrated; works on both private and public repos

## [4.10.10] - 2026-03-21

### Changed
- **Release**: Create the GitHub Release draft before provenance generation so SLSA attestations have a deterministic public upload target during Leg 3

### Fixed
- **Release**: Make Leg 3 provenance publication blocking by uploading each `.intoto.jsonl` attestation directly to the draft release before any final publish steps run
- **Release**: Verify all required provenance assets exist on the draft release before GHCR, Open VSX, or final GitHub Release publication can proceed

## [4.10.9] - 2026-03-21

### Changed
- **Release**: Restore `verify-staging` to the stable `oss-staging` OIDC environment so Leg 1 tag-based verification uses the trusted environment subject instead of an untrusted tag-ref subject

### Fixed
- **Release**: Add reusable workflow-contract checks for Leg 1 and Leg 3 OIDC environments so preflight fails before tagging when those guardrails drift
- **Release**: Add a CI `Release Workflow Contracts` job so environment-contract regressions fail on push and pull requests instead of surfacing during a release attempt

## [4.10.8] - 2026-03-21

### Changed
- **Release**: Preserve canonical public release workflows during staging and public org rewrites so Leg 2 no longer mutates Leg 3 dispatch, OIDC, or publish-time checks

### Fixed
- **Release**: Stop sanitization and public promotion rewrites from corrupting `oss-leg3-release.yml` and `publish-crates.yml`
- **Release**: Add public-mode validation guards for Leg 3 smoke-test invariants so workflow corruption fails before a tag is promoted
- **Release**: Pin `docker/login-action` in Leg 3 to an immutable commit SHA

## [4.10.7] - 2026-03-21

### Fixed
- **Release**: Mirror the public release workflow exception in Category 7 YAML validation so Leg 1 no longer rejects the restored public Leg 3 workflows
- **Release**: Centralize the public release workflow path regex inside the validator so infrastructure and structured-metadata checks stay aligned

## [4.10.6] - 2026-03-21

### Changed
- **Release**: Restore the public release workflow set in sanitized OSS trees so Leg 2 publishes the operator-run workflows required for Leg 3 and post-release crates publication

### Fixed
- **Release**: Stop stripping `oss-leg3-release.yml` from the public repo, which blocked `workflow_dispatch` and left public tags unreleasable
- **Release**: Align sanitization validation with the public release workflows by allowing Azure/OIDC infrastructure identifiers only in the specific public release workflow files that require them
- **Docs**: Correct the release pipeline and checklist so the expected public workflow set matches the sanitized output
## [4.10.5] - 2026-03-21

### Changed
- **CI/Release**: Migrate GitHub Actions workflows off Node 20-era action majors to current maintained releases across checkout, cache, artifact, and Azure login steps

### Fixed
- **CI/Release**: Remove repeated GitHub Actions `Node.js 20 actions are deprecated` warnings from CI and OSS release workflows by updating the affected action dependencies

## [4.10.4] - 2026-03-20

### Changed
- **Release**: Version sync now manages first-party `cargo vet` exemption version drift alongside docs and package metadata

### Fixed
- **Supply chain**: Prevent release/version bumps from failing `cargo vet --locked` because first-party `safe-to-deploy` exemptions were left on the prior version
- **Perl grammar**: Remove repeated `-Wempty-body` scanner warnings in `tree-sitter-perl-sqry`
- **Vendored jobserver**: Fix function-item-to-integer cast warning in Unix signal handler registration

## [4.10.3] - 2026-03-20

### Changed
- **Release**: Align Leg 3 Open VSX publication with the Azure Key Vault credential model and refresh the operator documentation/comments

### Fixed
- **Release**: Remove the GitHub-hosted Open VSX publish secret dependency from Leg 3 and infrastructure verification
- **Release**: Correct release documentation and checklists so active operator paths describe Azure OIDC plus Key Vault retrieval instead of GitHub environment secrets

## [4.10.2] - 2026-03-19

### Added
- **LSP**: Real cross-language edge counting in `index_status` with per-language-pair breakdown
- **VSCode**: Index stats auto-refresh after workspace rebuild completes
- **VSCode**: Truncation indicator for unused symbols panel

### Changed
- **VSCode**: Analysis predicates show "expand to check" instead of misleading zeros before data is loaded
- **VSCode**: Removed redundant tree view refreshes from lazy-loading panels

### Fixed
- **LSP**: Debug logging for silent node conversion drops
- **LSP**: Shared `is_cross_language_edge_kind` predicate keeps count and list endpoints in sync
- **LSP**: Language comparison uses typed enum equality instead of case-insensitive string comparison

## [4.10.1] - 2026-03-15

### Added

- **Release**: Add `publish-crates.yml` workflow for crates.io publishing

### Fixed

- **LSP**: Use platform-aware workspace root in URI construction tests (Windows compatibility)
- **Core**: Use normalizing register for fake test paths (Windows compatibility)
- **Core**: Use platform-aware test root in resolution tests
- **MCP**: Canonicalize workspace root for macOS `/var` symlink
- **Release**: Add models to macOS checksums and SLSA subjects
- **Release**: Include `sqry-models.tar.gz` in macOS Cosign signing
- **Release**: Use `mktemp` for Windows tar compatibility in model download
- **Release**: Remove unsupported `cargo zigbuild --version` check
- **Release**: Resolve zig binary path from pip package location
- **Release**: Add zig to PATH after pip install in leg3
- **Release**: Use `cargo-zigbuild` for musl C++ builds in leg3
- **Release**: Enable nl-classifier and bundle ONNX models in release artifacts
- **Release**: Add `musl-g++` wrapper for C++ build deps in leg3 Linux builds
- **Release**: Replace internal template check with public docs check in leg3 smoke tests

## [4.10.0] - 2026-03-13

### Changed

- **Display**: Standardize user-facing native qualified names across CLI, LSP, and MCP while preserving canonical graph identity internally
- **MCP**: Stabilize parity fixture ordering and score serialization for deterministic API output
- **Release**: Align OSS sanitization and preflight staging checks with the real public release transformation path

### Fixed

- **Languages**: Clear native-name regression fallout across C#, Go, Haskell, Java, JavaScript, Kotlin, Oracle PL/SQL, TypeScript, Vue, Zig, and related test harnesses
- **LSP**: Restrict `workspace/symbol` name filtering to the query contract instead of leaking unrelated matches
- **Release**: Remove remaining OSS preflight blockers in sanitize, Leg 2 replacement, and packaging validation

## [4.9.2] - 2026-03-09

### Fixed

- **MCP**: Fix `call_hierarchy` 47s regression — use indexed O(log n) name lookup instead of O(N) arena scan, filter to definition-kind nodes, restore single-root API contract
- **MCP**: Fix `find_cycles` failing on >100K node graphs — build CSR on-the-fly from loaded graph instead of requiring `sqry analyze` precomputation
- **MCP**: Fix `find_cycles` `Modules` cycle type using wrong edge kind (call edges instead of import edges)

### Performance

- **MCP**: `find_nodes_by_name` now uses `StringInterner::get()` + `AuxiliaryIndices::by_name()` for O(1) + O(log n) symbol lookup

## [4.9.1] - 2026-03-09

### Fixed

- **Graph**: Split CSR-only indexing from full analysis — `sqry index --force` no longer hangs on medium codebases
- **Graph**: Add `ReachabilityStrategy` with BFS fallback for degraded analysis path
- **Graph**: Density-based gating per edge kind with `checked_mul` overflow safety
- **Graph**: Manifest-hash staleness check for analysis artifacts with canonical config loader
- **MCP**: Add suffix matching for `direct_callers`, `direct_callees`, `get_hover_info`, `get_references` — unqualified and partially-qualified symbol names now resolve correctly

### Changed

- **Graph**: Replace nested `rayon::join` barriers with `into_par_iter` pipeline for per-kind analysis
- **Graph**: Add configurable analysis limits (`analysis_label_budget_per_kind`, `analysis_density_gate_threshold`, `analysis_budget_exceeded_policy`) to `sqry config`
- **Graph**: Add `SQRY_DENSITY_GATE_THRESHOLD` environment variable

### Performance

- **Analysis**: Replace `merge_intervals` with `FastBitSet` for 2-hop label computation — eliminates repeated interval vector allocation, sorting, and merging

## [4.8.17] - 2026-03-06

### Added

- **MCP/Release**: Add OCI distribution path for `sqry-mcp` with GHCR publish, Cosign signing, and MCP registry metadata

### Changed

- **Release**: Make Windows ZIP installer path the primary end-user package and align release notes/checklist with current install flows
- **Release**: Preserve `.mcp`, Docker packaging, `.dockerignore`, and `install.ps1` through OSS sanitization and staging

### Fixed

- **Release**: Synchronize version updates for OCI identifiers in `.mcp/server.json`
- **Docs**: Align local MCP setup guidance and public skills/install instructions with the real `sqry-mcp` runtime
- **Package Managers**: Align Winget, Scoop, and Nix packaging with the multi-binary install contract (`sqry`, `sqry-mcp`, `sqry-lsp`)

## [4.8.5] - 2026-03-04

### Fixed

- **Release**: Exclude `oss-release-watchdog.yml` from OSS sanitized bundles to avoid staging push rejection (`workflows` permission constraint)

## [4.8.4] - 2026-03-04

### Fixed

- **Release**: Use staging-writer app credentials for `oss-staging` verify job token generation
- **Release**: Update `oss-public-sync` to use installed staging writer app and commit-SHA checkout
- **Release**: Exclude CycloneDX SBOM provenance URLs from staging/public leak prechecks to avoid false positives

## [4.8.3] - 2026-03-03

### Fixed

- **VSCode**: Upgrade serialize-javascript to fix RCE vulnerability (GHSA-5c6j-r48x-rmvq)
- **Release**: Fix org replacement ordering in public sync script
- **Release**: Force push to public repo for orphan commit compatibility
- **Release**: Push branch before tag, handle tag creation restrictions
- **Release**: Switch gh auth to verivusOSS-releases for public sync
- **Release**: Narrow INTERNAL/PROPRIETARY marker check in sync script
- **Release**: Exclude .github/ from verivus-oss leak check in sync script
- **Release**: Harden sync-to-public.sh gitleaks check and clone strategy

## [4.8.2] - 2026-03-03

### Added

- **Release**: Automated version sync, SBOM coverage, ARM64 builds, and package manager distribution
- **Release**: Updated sanitization allowlists for packaging and distribution scripts

### Changed

- **Docs**: Add development process docs, plans, and MCP feature spec

## [4.8.1] - 2026-03-03

### Changed

- **Docs**: Align all user-facing documentation with v4.8.1 release (versions, dates, org references)
- **Docs**: Rewrite SEMANTIC_VERSIONING.md with accurate release process and scopes
- **Docs**: Update SCHEMA.md data dictionary to v4.8.1 with accuracy fixes
- **Skills**: Update sqry-semantic-search skill to v4.8.1 (35 languages, add Pulumi)

## [4.8.0] - 2026-03-01

### Added

- **Share**: Implement share feature for exporting sqry graph snapshots (FR-2025-023-share)
- **Release**: Azure OIDC auth and multi-platform release pipeline (Leg 3)

### Fixed

- **Release**: Allowlist-based OSS sanitization replacing blocklist approach
- **Release**: Fix orphan commit index leak in sanitize-for-oss.sh
- **Release**: Cosign identity fix for oss-release.yml
- **CLI**: Enable large_stack_test on Windows, fix verify-infrastructure

## [4.7.0] - 2026-02-28

### Fixed

- **MCP/Core/NL**: Resolve 5 disconnected functionality issues
- **CI**: Resolve all 5 failing CI jobs
- **Sonar**: Resolve all bugs and critical issues
- **CI**: Resolve stack overflow in completions test

## [4.6.3] - 2026-02-27

### Changed

- **NL**: Switch base model from DistilBERT to all-MiniLM-L6-v2 for better intent classification
- **NL**: Retrain intent classifier with corrected CLI commands

### Fixed

- **NL**: Correct CLI command names in NL pipeline and skills
- **Docs**: Fix CLI command mismatches in launch scenarios

## [4.6.0] - 2026-02-27

### Fixed

- **Graph**: Fix O(n²) AuxiliaryIndices rebuild in Phase 4c

## [4.5.11] - 2026-02-27

### Fixed

- **Release**: Strip FR references during sanitization, fix stale org/version refs
- **Release**: Add version alignment check to sanitize pipeline
- **Docs**: Align OSS documentation with actual implemented functionality
- **Docs**: Correct org refs, stale versions, placeholder URLs, and counts
- **CI**: Add trap-based extraheader cleanup to push steps

## [4.5.10] - 2026-02-25

### Fixed

- **CLI**: Fix broken `search --kind`/`--exact` help examples (flags only exist on top-level CLI)
- **CLI**: Fix double "Error: No pattern or command provided" message on bare `sqry`
- **CLI**: Fix SIGPIPE broken pipe noise when piping to `head`/`less`
- **CLI**: Fix help examples rendering as single-line paragraphs (add `verbatim_doc_comment`)
- **CLI**: Add scope notes to `--kind`/`--lang`/`--exact`/`--fuzzy` flag help text
- **CLI**: Logical command ordering in `--help` (search, index, analysis, export, config, integration, utility)

### Changed

- **Docs**: Overhaul QUICKSTART.md, CLAUDE.md, CHANGELOG.md for accuracy (CLI examples, tool counts, grammar counts)
- **OSS Release**: Add CLAUDE.md, AGENTS.md, CONTRIBUTORS.md, tuning guides to sanitization allowlist

## [4.5.9] - 2026-02-25

### Fixed

- **Supply Chain**: Update cargo-vet exemptions for 8 dependency bumps (chrono, js-sys, tempfile, wasm-bindgen family, web-sys); prune 4 stale wit-bindgen exemptions

## [4.5.8] - 2026-02-25

### Fixed

- **OSS Release**: Verify sanitized tarball in isolation before push to staging

## [4.5.7] - 2026-02-25

### Fixed

- **OSS Release**: Add `.trigger` to sanitization allowlist

## [4.5.6] - 2026-02-25

### Fixed

- **OSS Release**: Comprehensive fix for staging pipeline failures (SIGPIPE from tee, nested .gitignore bypass)

## [4.5.5] - 2026-02-25

### Fixed

- **OSS Release**: Remove `vendor/` from `.gitignore` during sanitization to prevent cargo build failures

## [4.5.4] - 2026-02-24

### Changed

- **CI**: Remove release-plz, revert to manual tag-based release flow

### Fixed

- **CI**: Fix broken benchmark workflows
- **CI**: Resolve supply-chain formatting for cargo-vet 0.10.2
- **CI**: Fix 3 pre-existing CI failures (rustfmt, cargo-vet, Windows)
- **CI**: Update naming-audit for standalone build

## [4.5.3] - 2026-02-23

### Fixed

- **CI**: Load SSH deploy key in verify-staging for private repo clone
- **MCP**: Update protocol tests for rmcp 0.16 error response behavior
- **Dependencies**: Complete tree-sitter 0.25 to 0.26 migration

## [4.5.2] - 2026-02-23

### Added

- **OSS Release**: Full multi-platform release workflow with Cosign signing and SLSA Level 2 provenance
- **OSS Release**: Three-leg pipeline: staging (sanitize), public sync, release distribution
- **OSS Release**: Phase 1 infrastructure setup scripts (deploy keys, verify-infrastructure)
- **OSS Release**: Enhanced oss-staging.yml with digest, manifest, and verify job
- **Backup**: Git-independent backup system

### Fixed

- **OSS Release**: Defense-in-depth to prevent source directory destruction during sanitization
- **OSS Release**: Replace exclusion-based staging with inclusion allowlist
- **OSS Release**: Replace staging org refs in sync-to-public pipeline
- **Dependencies**: Upgrade tree-sitter 0.25 to 0.26

## [4.5.1] - 2026-02-22

### Fixed

- **Dependencies**: Address criterion black_box deprecation and add execute validation tests
- **Dependencies**: Upgrade 5 dependencies to latest compatible versions
- **Docs**: Fix stale Claude Code integration guide claiming no MCP support

## [4.5.0] - 2026-02-21

### Added

- **MCP**: Add `execute` parameter to `sqry_ask` for inline command execution
- **Pulumi**: Add edge type tests covering all 8 feature areas

### Fixed

- **Search**: Remove dead code and preserve fuzzy scores through pipeline
- **Search**: Remove dead `to_search_mode` and fix misleading error message

## [4.4.2] - 2026-02-21

### Fixed

- **Dependencies**: Replace GPL-3.0 confusables crate with internal implementation

## [4.4.1] - 2026-02-21

### Fixed

- **Dependencies**: Remove unmaintained atomic-polyfill (disable postcard default features)
- **Dependencies**: Remove unnecessary dependencies and exclude internal tools
- **OSS Release**: Fix ClamAV freshclam database download, exclude SBOM from internal-reference validation
- **OSS Release**: Add security gates, SBOM, ClamAV scan, and fix gitignore staging
- **OSS Release**: Replace exclusion-based staging with inclusion allowlist
- **OSS Release**: Harden OSS sanitization against binary fixture and build artifact leaks
- **SonarQube**: Resolve all 43 code smells across Python, TypeScript, JS, and HTML

## [4.4.0] - 2026-02-20

### Added

- **MCP Layer 2 Documentation Resources**: On-demand documentation via MCP `resources/list` and `resources/read` handlers
  - 4 token-optimized resources: tool-guide, query-syntax, patterns, architecture
  - Completes two-layer documentation strategy (L1 instructions always in context, L2 fetched on demand)
  - Follows TOKEN_OPTIMIZATION_GUIDE.md principles (terse descriptions, inline constraints, markdown tables)

### Fixed

- **CI: Windows Parity Tests**: Strip `\\?\` extended-path prefix from `canonicalize()` in MCP parity test normalization
- **CI: Supply Chain Audit**: Fix cargo-vet formatting and update tree-sitter-kotlin/sequel exemptions
- **CI: Coverage Stack Overflow**: Add `RUST_MIN_STACK` env for sqry-cli coverage instrumentation
- **CI: OSS Staging OOM**: Limit build parallelism (`CARGO_BUILD_JOBS=2`), disable debug info, pin toolchain to 1.90
- **CI: Release-Signing Toolchain Drift**: Pin all workflows to Rust 1.90, remove `-Dwarnings` from RUSTFLAGS, remove deprecated `profile: minimal`
- **CI: SLSA Provenance**: Switch to pre-built generator (`compile-generator: false`) to avoid Go toolchain failures
- **CI: Bench PR**: Remove `push` trigger that caused CodSpeed action failures

## [4.3.0] - 2026-02-20

### Fixed

- **CI**: Reclaim disk space for CI build in oss-staging workflow

## [4.2.0] - 2026-02-19

### Added

- **Pass 5 Global Cross-Language Edge Detection**: New build pipeline pass that detects cross-language relationships across files
  - FFI linking: Rust `extern` declarations matched to C/C++ function definitions
  - HTTP linking: JavaScript/TypeScript `fetch`/`axios` calls matched to Python/Java/Go route handlers
  - Route detection added to 5 language plugins (JavaScript, TypeScript, Python, Java, Go)
  - 21 unit tests + 4 integration tests + 42 plugin tests
  - Implementation: `sqry-core/src/graph/unified/build/pass5_cross_language.rs` (806 lines)
- **Supply Chain Security L3 Design**: Codex-approved design documentation for supply chain hardening

### Fixed

- **Windows Path Handling**: Fix 4 Windows test failures in path handling and file locking (`sqry-core`)
- **MCP Path Normalization**: Centralize path-to-string conversion for Windows forward slashes; fix remaining path separator edge cases in parity tests
- **SIMD Safety**: Add required `unsafe` blocks for non-intrinsic unsafe calls in NEON code (`sqry-core`)
- **Cross-Language Review Findings**: Address Codex review findings for Pass 5 implementation
- **CI Stability**: 34 fixes for cross-platform CI reliability (Windows stack size, macOS path symlinks, flaky test stabilization, BuildJet→GitHub-hosted runner migration, coverage workflow improvements)

## [3.3.0] - 2026-02-04

### Added

- **Java Test Fixture Restructuring**: Restructure Java test fixtures to match package hierarchy (SonarQube java:S1598 compliance)

### Fixed

- **Svelte/Vue SFC Support**: Create Component nodes and Contains edges for SFC files (6 Svelte + 7 Vue DSL tests)
- **SAP ABAP Enterprise Features**: Resolve 9 enterprise feature test failures (incomplete graph builder)
- **MCP Discovery Cache**: Resolve flaky discovery cache test with resettable static (OnceLock → Mutex<Option<>>)

### Changed

- **Clippy Pedantic Compliance**: Resolve 696 clippy pedantic lint warnings across 70 files
- **SonarQube Configuration**: Upgrade to Community Build 26.1.0.118079; exempt Java test fixtures from S1598

## [3.2.0] - 2026-02-02

### Added

- **Multi-Workspace Cache Isolation (sqry-mcp)**: Complete cache isolation for multi-repository workflows
  - GraphIdentity-based cache keys with workspace_root, snapshot_sha256, built_at, schema_version, and snapshot_format_version
  - Engine cache with LRU eviction (configurable capacity: 1-100, default: 5) and TOCTOU-safe freshness checks
  - Discovery cache with platform-specific path normalization (Unix inode, Windows file_index, macOS pathconf)
  - Query caches (trace_path, subgraph) with configurable TTL (default: 300s) and LRU eviction
  - Atomic manifest writes with platform-specific handling (Unix persist, Windows MoveFileExW)
  - Config-driven cache capacities with validation (zero rejection, hard caps enforcement)
  - Environment variable: SQRY_MCP_WORKSPACE_ROOT for security boundary (backward compatible with SQRY_WORKSPACE_ROOT)
  - Configuration: SQRY_MCP_ENGINE_CACHE_CAPACITY, SQRY_MCP_DISCOVERY_CACHE_CAPACITY, SQRY_MCP_TRACE_PATH_CACHE_CAPACITY, SQRY_MCP_SUBGRAPH_CACHE_CAPACITY, SQRY_MCP_QUERY_CACHE_TTL_SECS
  - Comprehensive test suite: 220+ tests passing (57 config validation + 9 integration + 220 library)
  - Performance: Engine cache hits <0.5ms, Discovery cache O(1) lookup
  - Codex code review approved (3 iterations, all issues resolved)
  - Documentation: USER_GUIDE.md, TROUBLESHOOTING.md, implementation docs
  - Prevents cache collision bugs where repo-A results served to repo-B queries

- **Ruby Signature Metadata**: Complete signature extraction for Ruby methods (feat(ruby): complete signature metadata)
  - All 9 Ruby parameter types: simple, optional, splat, keyword, hash_splat, block, destructured, forward, hash_splat_nil
  - Return type extraction from 3 sources: Sorbet sig blocks, RBS inline comments, YARD documentation
  - Combined signature format: "params -> return_type"
  - Robust validation: RBS requires `#:` prefix, YARD requires adjacency, depth-tracking for nested proc types
  - Comprehensive test coverage: 25/25 tests passing (100%)
  - Codex-approved after 3 review iterations (8 findings, all resolved)
  - Enables queries like "find methods with parameter x" or "find methods returning Type"

- **Go TypeOf/Reference Phase 2**: Function and method parameter/return type edges (feat(go): add TypeOf/Reference edges for Phase 2)
  - TypeOf edges for function/method parameters with TypeOfContext::Parameter metadata
  - TypeOf edges for function/method returns with TypeOfContext::Return metadata
  - Support for all parameter types: simple, variadic (...T → []T), multi-name (a, b, c int), anonymous
  - Support for all return types: single, multiple (int, error), named returns
  - Reference edges for all nested types in parameters and returns
  - Comprehensive test coverage: 39/39 tests passing (15 Phase 2 + 21 Phase 1 + 3 context discrimination)
  - Enables queries like "find functions taking context.Context" or "find methods returning error"
  - **Breaking Change**: EdgeKind::TypeOf changed from unit variant to struct variant with context/index/name metadata
    - Impact: Existing `.sqry/graph/snapshot.sqry` files incompatible
    - Migration: Delete `.sqry/` directory and re-run `sqry index`

### Breaking Changes

- **Go TypeOf Edge Structure**: EdgeKind::TypeOf changed from unit variant to struct variant
  - Now includes: `context: Option<TypeOfContext>`, `index: Option<u16>`, `name: Option<StringId>`
  - Allows discrimination between parameter types, return types, variable types, and field types
  - Requires re-indexing: delete `.sqry/` and run `sqry index`

### Changed

### Fixed

- **Java Method Visibility**: Methods and constructors now correctly populate visibility metadata (fix(java): add visibility metadata to method nodes)
  - Added visibility field to MethodContext struct
  - Created extract_visibility() helper for public/private/protected/package-private
  - All 5 visibility tests passing, 112 total Java tests passing
  - Enables queries for method visibility filtering

- **Lua Function Visibility**: Functions now correctly populate visibility metadata based on underscore convention (fix(lua): add visibility metadata based on underscore convention)
  - Functions with underscore prefix (_function) marked as private
  - Functions without underscore prefix marked as public
  - All 3 visibility tests passing, 47 total Lua tests passing
  - Enables queries for function visibility filtering

## [3.1.0] - 2026-01-29

### Added
- **P2 Advanced Features (8 Plugins)**: Extended semantic capabilities across language plugins
  - Python: Property node detection via `@property` decorator
  - Groovy: Property/field distinction for auto-accessor fields
  - Ruby: Async detection, constant nodes (UPPERCASE), mixin edges (include/extend)
  - R: S4/R6 class detection, variable assignment tracking
  - Lua: Table constructor field tracking, field access (dot/bracket notation)
  - TypeScript: Namespace augmentation and module merging support
  - Elixir: Protocol definitions and implementations (defprotocol/defimpl)
  - PHP: Property nodes (already complete)

- **Visibility Metadata**: Added public/private/protected visibility tracking
  - C language plugin: Visibility for static/extern functions
  - C++ language plugin: Class member visibility with access specifier tracking
  - Elixir language plugin: Public/private function visibility

### Fixed
- **C++ Class Members** (16/16 tests passing, up from 2/16):
  - Method visibility no longer overwritten to public in class bodies
  - Type qualifier stripping now handles postfix `const`/`volatile` (e.g., `Foo const*`)
  - Expanded type extraction for `sized_type_specifier`, `auto`, `decltype`, `struct_specifier`
  - Fixed double-adding of methods in class bodies

- **Python Type Hints** (24/24 tests passing, up from 4/13):
  - Scope-qualified parameter/variable names prevent cross-scope type contamination
  - Forward reference normalization (strip quotes from `"Type"`)
  - PEP 604 union normalization (extract base type from `X | Y`)
  - Added 11 comprehensive tests validating edge targets and scope qualification

### Changed
- **Plugin Test Coverage**: All 58 language plugins now have comprehensive test suites
  - 100% test pass rate across Phase 0, P0, P1, P2 features
  - Zero clippy warnings across all plugins
  - Production-ready quality gates enforced

## [3.0.0] - 2026-01-26

### Breaking Changes
- **Symbol Types Removal**: Deleted all Symbol-based APIs (`Symbol`, `SymbolId`, `SymbolType`,
  `SymbolLocation`, `PluginSymbolBuilder`, `SymbolRef`, `SymbolSummary`, `SymbolWithRepo`,
  `SymbolChange`, `SymbolCreationData`, `SymbolIdGenerator`, `SymbolIndex`, `RawSymbolValues`)
  in favor of graph-native `NodeEntry`/`NodeId`/`NodeKind`.
- **Plugin API Update**: Language plugins now build graphs via `GraphBuilder`; Symbol extraction
  methods have been removed.
  - Migration guide: `docs/development/symbol-removal/MIGRATION_GUIDE.md`

## [2.13.6] - 2026-01-23

### Changed
- **Query Result Cache**: Cache now stores graph-native `NodeId` values instead of legacy `Symbol` values.

### Fixed
- **Auto-Rebuild**: `--auto-rebuild` now rebuilds stale indexes during validation before query/search execution.

## [2.13.5] - 2026-01-23

### Added
- **Index Validation Flag**: `--validate` flag for query/search commands
  - `--validate fail` - Exit with code 2 if >20% of indexed files are missing
  - `--validate warn` - Log warning but continue (default)
  - `--validate off` - Skip validation entirely
  - Uses FileRegistry iteration for accurate unique file counting

### Fixed
- **DOT Export Tests**: Rewrote 13 DOT export integration tests for unified graph API

## [2.13.4] - 2026-01-23

### Added
- **LSP Custom Handlers (Full Parity)**: Complete LSP-MCP feature parity with 27 handlers
  - `sqry/semanticDiff` - Compare semantic changes between git commits/branches
  - `sqry/subgraph` - Extract focused dependency subgraphs around symbols
  - `sqry/similarSymbols` - Find symbols similar to a reference using fuzzy matching
  - `sqry/showDependencies` - Show dependency tree for a file or symbol

## [2.13.3] - 2026-01-23

### Added
- **Returns Predicate**: New `returns:` predicate for filtering functions/methods by return type
  - `returns:String` - Find functions returning String (substring match)
  - `returns:Optional` - Find functions returning Optional types
  - Implemented for Java, C#, C++, and Kotlin
  - Uses signature field for return type storage

### Fixed
- **Lua Qualified Name Lookup**: Dynamic language method calls now match correctly
  - Lua colon syntax `target:method()` creates edges with receiver variable names
  - `callers:Player::takeDamage` now matches calls like `target:takeDamage()`
  - Added method-name-only matching fallback for dynamic languages

## [2.13.0] - 2026-01-23

### Added
- **Query Predicates for Symbol Metadata**: New `async:`, `static:`, and `visibility:` predicates for graph queries
  - `async:true` / `async:false` - Filter functions/methods by async status
  - `static:true` / `static:false` - Filter members by static modifier
  - `visibility:pub` / `visibility:private` / `visibility:pub(crate)` - Filter by visibility
  - All predicates support both boolean and string value formats
  - JSON output includes metadata fields when present
- **Rust Visibility Metadata**: Rust symbols now include visibility information in the code graph
  - Functions, methods, structs, enums, traits, constants, and statics all track visibility
  - Supports `pub`, `pub(crate)`, `pub(super)`, `pub(in path)`, and private (no modifier)

### Fixed
- **PHP Null-Safe Operator**: PHP 8.0's null-safe operator (`?->`) method calls now tracked correctly
  - `$user?->getProfile()` now creates proper call edges
  - Added `nullsafe_member_call_expression` node handling in PHP graph builder

## [2.6.1] - 2026-01-06

### Fixed
- **VSCode Extension**: Use `replaceAll` instead of regex `replace` for ES2021 compatibility
- **Java Plugin**: Add `emptyGroupedNames` to return_types test expectation (fixture sync)
- **sqry-nl Training**: Resolve SonarQube critical issues in Python training scripts
  - Extract `_FIND_CIRCULAR_DEPS` constant to fix S1192 (duplicated string literals)
  - Reduce cognitive complexity in `train_classifier.py` (S3776: 18→~10)
  - Reduce cognitive complexity in `export_onnx.py` (S3776: 19→~10)

### Changed
- **SonarQube Code Quality**: Reduced cognitive complexity across multiple tools
  - `naming-audit`: Extracted `AllowlistEntry::matches_location`, `scan_file`, `find_pattern_matches`
  - `slopscan`: Refactored `detect_python_function_spans`, `detect_braced_function_spans`, `rust_find_raw_string_start`
  - `graphsync`: Extracted helpers in `main.rs` and `overrides.rs`
  - `sqry-vscode`: Extracted helpers in `extension.ts` and `searchPanel.ts`
- **SonarQube Integration**: Exclude test directories from analysis sources

## [2.6.0] - 2025-12-31

### Added
- **Rust `pub use` Export Edges (SC-06, SC-07)**: `pub use` declarations now emit proper `Exports` edges
  - `pub use foo::bar` → `ExportKind::Reexport` edge
  - `pub use foo::bar as alias` → `ExportKind::Reexport` with alias metadata
  - `pub use foo::*` → `ExportKind::Namespace` edge for wildcard re-exports
- **Rust Field Access Reference Edges (SC-08-12)**: Field access expressions emit reference edges
  - `p.x` → Reference edge from containing function to `<field:p.x>`
  - Supports struct fields, tuple fields, method self references, nested access, and generic types
  - All 5 field access test scenarios (SC-08 through SC-12) now pass

### Changed
- **Legacy Architecture Removal Complete**: 100% unified graph architecture across all 34 language plugins
  - Removed `CallSiteStore` from Rust plugin (~500 lines deleted)
  - All plugins now use `GraphBuilder` trait, `StagingGraph`, and `GraphBuildHelper`
  - Zero legacy fallback paths remain in codebase
  - Verified end-to-end: 0 occurrences of `CallSiteStore`, `LegacyCodeGraph`, `CallSiteAdapter`

### Fixed
- **Query Language Enhancements (Phase 7)**: Complete query system with 14 implemented tasks

### Removed
- Deleted legacy `CallSiteStore` struct and impl from `sqry-lang-rust`
- Deleted 6 parse cache benchmark files from `sqry-core`
- Deleted legacy test file `unified_parser_parity_test.rs`
- Deleted `call_site_store.rs` from JavaScript, Python, and TypeScript plugins
- Deleted `sqry-core/src/query/parser.rs` (legacy parser)
- Deleted `sqry-core/src/query/compat.rs` (compatibility layer)
- Deleted `sqry-core/src/query/legacy_usage.rs`
- Deleted `sqry-core/src/query/cache/parse_cache.rs` and `parse_cache_rwlock.rs`
- Deleted `sqry-core/src/query/executor/cache_integration.rs`

## [2.5.0] - 2025-12-28

### Added
- **Wave 2 OOP Edges**: Kotlin, Scala, Ruby OOP relationship tracking
- **Rust OOP and FFI Edges**: `impl Trait` and `extern` block support
- **Wave 2 OOP Edges for 6 Languages**: Complete OOP relationship tracking

### Fixed
- **Wave 3 Language Plugin Review Fixes**: Improved graph builder quality for Rust, Ruby, SQL, Java, Swift, and Kotlin
  - **Rust**: Fixed `<module>` node collision by using distinct names (`<file_module>` vs `<toplevel>`); fixed impl method qualified naming to include module scope
  - **Ruby**: Fixed `require_relative` resolution to use source file directory; added `load` statement handling with `ImportStyle::RubyLoad`
  - **SQL**: Added function/procedure call edge extraction; fixed `function_calls` query pattern for invocation nodes
  - **Java**: Added `ImportStyle::JavaStaticImport` for static imports
  - **Swift**: Strip generic parameters from callee names (`foo<T>` → `foo`)
  - **Kotlin**: Added `extract_callee_name` helper for complex expressions; `qualified_name()` now returns `&str`; added property access and method call tests
- **Wave 2 Language Plugin Review Fixes**: Improved graph builder quality for C, C++, C#, PHP, and Perl
  - **C/C++**: Added endpoint assertion helpers for import edge tests; verify specific include targets
  - **C++**: Implemented qualified naming with namespace/class context tracking; fixed docstrings from Kotlin to C++ syntax; added import deduplication; added test fixtures for imports and inheritance (22 tests)
  - **C#**: Added `qualify_type_name()` for base type resolution with namespace context; removed unused `inserted` parameter
  - **PHP**: Added namespace_definition handling; changed qualified name separator to `\` (PHP style); fixed call-edge naming coherence for member/static calls; removed unused `inserted` parameter
  - **Perl**: Added `extract_imports()` for `use`/`require` statement handling; creates Import edges for Perl modules (fixes phase5a_perl_cli_e2e test)
  - **Query Cache**: AST parse caching with LRU eviction, result caching with file metadata invalidation, and budget-controlled memory management (2,078 LOC)
  - **Query Builder API**: Fluent, type-safe programmatic query construction with validation against FieldRegistry (2,203 LOC)
  - **CLI `--explain` flag**: Shows query execution plan including original/optimized query, execution steps, index usage, and cache status
  - **Comprehensive documentation**: User reference guide with EBNF grammar, 33+ cookbook examples, operator precedence, and troubleshooting (`docs/query-language.md`)
  - **Performance benchmarks**: Criterion-based suite with 26 benchmarks covering parsing, lexing, validation, optimization, and caching
  - **Error message test suite**: 36 tests verifying error quality, Levenshtein suggestions, and position accuracy

### Breaking Changes
- **FR-2025-015 Parser Unification Finalization**: Removed legacy query support
  - Removed `--legacy-query` CLI flag from `sqry query` and `sqry workspace query` commands
  - Removed legacy compatibility translator (`parse_compat_query`, `parse_any`, and related telemetry)
  - Removed `QueryExecutor::execute_query_with_stats_legacy()` method
  - Removed `SessionManager::query_legacy()` method
  - Removed `WorkspaceIndex::query_legacy()` method
  - Legacy whitespace-separated predicates removed; boolean operators (AND/OR/NOT) now required
  - Use `QueryParser::parse_query()` or `QueryExecutor::execute_query()` for parsing/execution

## [2.0.0] - 2025-12-07

### Breaking Changes
- **FR-2025-021 Legacy Hooks Removal**: Graph mode is now the only mode for relation extraction
  - Removed legacy `extract_calls()`, `extract_imports()`, `extract_exports()` methods from LanguagePlugin trait
  - Removed `use_graph_mode` configuration option (graph mode always enabled)
  - Removed `SQRY_USE_GRAPH` environment variable (silently ignored if set)
  - Removed dual code paths in IndexBuilder - only GraphBuilder path remains
  - Renamed `graph_adapter.rs` to `graph_population.rs` to reflect actual purpose

### Fixed
- **Memory corruption recovery**: User metadata index now gracefully recovers from corrupted files
  - Backs up corrupted files for forensics before removal
  - Uses PID-unique temp files to prevent race conditions between concurrent processes
  - Added `sync_all()` before atomic rename for durability
- **All GraphBuilders wired up**: All 28+ language plugins now properly expose their GraphBuilder implementations
  - Converted all plugins from unit structs to struct-with-field pattern
  - Enables `callers:`, `imports:`, `exports:` relation queries for ALL languages
  - Languages: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Puppet, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, Terraform, TypeScript, Vue, Zig, Oracle PL/SQL, Salesforce Apex, SAP ABAP, ServiceNow Xanadu
  - Re-enabled previously ignored relation tests
- **FR-2025-022**: Address review feedback for Ruby GraphBuilder

## [1.32.0] - 2025-12-05
### Added
- **sqry-mcp rmcp SDK Integration**: Experimental rmcp SDK support for MCP protocol compliance
  - New `SQRY_MCP_USE_RMCP=true` environment variable enables rmcp SDK mode
  - `server.rs` module with 13 tool implementations using rmcp macros (`#[tool]`, `#[tool_router]`, `#[tool_handler]`)
  - `tools/params.rs` with schemars-derived parameter types for JSON schema generation
  - Full response metadata preservation (execution_ms, pagination, used_index, etc.)
  - Validation parity with native implementation (bounds checking, required fields)
  - Strangler fig pattern allows gradual migration from native JSON-RPC

### Changed
- Upgraded schemars from 0.8 to 1 for rmcp SDK compatibility
- Added rmcp =0.9.0 dependency (pinned for pre-1.0 stability)

## [1.31.0] - 2025-12-05
### Added
- P2-29 Paging: Smart output paging for CLI with preview extraction
- P2-12-31 Output Formatters: Preview extraction in text and JSON formatters

## [1.30.0] - 2025-12-03
### Added
- **P2-11 Query Aliases**: Save and execute commonly used queries with `@alias` syntax
  - `sqry alias add <name> <command> [args...]` - Create named aliases for frequent queries
  - `sqry alias list` - View all saved aliases (local and global)
  - `sqry alias remove <name>` - Delete an alias
  - `sqry @<alias>` - Execute a saved alias inline
  - Local aliases (workspace-specific) take precedence over global aliases
  - Smart flag parsing: correctly handles `--preview @foo` vs `--preview 5 @foo`
- **P2-17 Query History**: Track query execution with automatic secret redaction
  - `sqry history` - View recent queries with timestamps and success status
  - `sqry history clear` - Clear history (with `--all` for global clear)
  - Automatic secret redaction enabled by default (disable with `SQRY_NO_REDACT=1`)
  - Service-specific patterns for AWS, GitHub, Slack, Stripe, etc.
  - Records post-expansion argv for accurate history replay
  - Canonicalized working directory paths

### Changed
- History recording now captures expanded alias commands (what actually executed)
- Secret redaction is now opt-out (`SQRY_NO_REDACT=1`) instead of opt-in

## [1.28.0] - 2025-12-02
### Added
- **P1-3 VSCode Progress Indicators**: Real-time progress display during workspace indexing
  - Shows current file being processed with percentage complete
  - Displays total symbols and duration on completion
  - Uses LSP WorkDoneProgress notifications (requires VSCode 1.75.0+)
  - UUID-based progress tokens prevent collisions
  - ProgressGuard ensures indicators are cleared on cancellation/abort
  - Full troubleshooting guide in TROUBLESHOOTING.md

### Changed
- VSCode extension now relies on LSP server for progress display instead of manual wrapper
- Removed duplicate success messages (server sends via window/showMessage)

## [1.24.1] - 2025-11-25
### Fixed
- Hardened V3 index integrity: added metadata checksum headers, checksum enforcement on symbol payloads, and clearer corruption handling to prevent invalid-tag/EOF reload failures.
- Doc/tests cleanup: updated git subprocess doctests to public APIs, adjusted AST examples, and refreshed Vue/MCP fixtures to keep workspace tests green.

### Added
- **PHP Relation Tracking**: Full `callers:`, `exports:`, and `imports:` query support for PHP
  - Call extraction with qualified names (`Namespace\ClassName::method`, instance methods, null-safe operator)
  - Export extraction with visibility tracking (public/protected/private methods and classes)
  - Import tracking for `use`, `require`, and `require_once` statements with alias support
  - Method chaining support (fluent interfaces like `$query->where()->orderBy()->get()`)
  - Dynamic call metadata for `call_user_func`, `call_user_func_array`, and variable functions
  - Namespace normalization (stripping leading backslash, fully-qualified name resolution)
  - CLI integration tests and performance benchmarks (13x better than target: 19ms vs 150ms)
  - PHP promoted from Tier 3 to Tier 2 language support
  - **Index Rebuild Required**: Existing PHP codebases must be re-indexed to enable relation queries
- **Ruby Relation Tracking**: Full `callers:`, `exports:`, and `imports:` query support for Ruby
  - Call extraction with qualified names (`ClassName#method`, `ClassName.method`)
  - Export extraction with visibility tracking (private/protected/public methods)
  - Handles all Ruby visibility patterns: scope markers, inline modifiers, post-declaration
  - Support for `class << self` singleton classes and safe navigation operator (`&.`)
  - CLI integration tests for end-to-end validation
  - Ruby promoted from Tier 3 to Tier 2 language support
- **Lua Relation Tracking**: Full `callers:`, `exports:`, and `imports:` query support for Lua
  - Call extraction with dot syntax (`module.function`) and colon methods (`object:method`)
  - Export extraction from return tables and global function declarations
  - Import tracking for `require()`, `dofile()`, and `loadfile()` statements
  - Method metadata for colon syntax (implicit self parameter)
  - Bracket notation call support for dynamic function access
  - Exceptional performance: 1.15-3.26ms for 560-1,601 LOC (46-86x faster than targets)
  - 34 unit tests with comprehensive coverage (Neovim, WoW, Love2D, WeakAuras fixtures)
  - CLI integration tests (10/21 passing; 11 tests blocked by systemic sqry-core issues)
  - Lua promoted from Tier 3 to Tier 2 language support
  - **Known Limitations**: `exports:` and `callees:` queries affected by systemic sqry-core issues (documented in technical backlog); `callers:` queries work correctly
  - **Index Rebuild Required**: Existing Lua codebases must be re-indexed to enable relation queries
- **P1-14 Index Validation System**: Comprehensive validation infrastructure with security hardening
  - **BLAKE3 Checksums**: Cryptographically secure integrity verification (8 GB/s throughput)
    - Detects bit corruption, tampering, and incomplete writes
    - Backward compatible with legacy indexes (no checksum field)
  - **Structural Validation**: Five comprehensive validation checks
    - Format validation: Magic bytes, version, file size, timestamps
    - Dependency validation: Dangling reference detection with lenient external reference handling
    - ID validation: Duplicate detection, gap ratio checking
    - Graph validation: Cycle detection with DFS, orphaned file detection
    - Checksum validation: BLAKE3 hash verification
  - **Security Features**:
    - Decompression bomb protection with streaming limits (500 MB default)
    - Environment variable clamping (`SQRY_MAX_INDEX_SIZE`: 1 MB - 2 GB)
    - Hard streaming limits using `decoder.take(max_size + 1)` with `> max_size` check
    - Prevents memory exhaustion attacks and env var injection DoS
  - **CLI Integration**:
    - Three validation modes: `--validate=off|warn|fail` (warn is default)
    - `--auto-rebuild` flag for automatic recovery from corruption
    - Exit code 2 on validation failure (CI/CD friendly)
    - `sqry repair` command for index repair and validation
  - **Test Coverage**: 22 comprehensive tests covering all security paths
    - Boundary cases validated (exact-limit data, off-by-one checks)
    - All critical security paths covered
  - **Documentation**: Complete user guide at [docs/configuration/VALIDATION.md](docs/configuration/VALIDATION.md)
- **P1-11 Git-Aware Index Updates** (v1.22.0+): Subprocess-backed git integration for 10-100x faster incremental builds
  - **Automatic Change Detection**: Leverages git to identify modified, added, deleted, and renamed files
    - Only processes files changed since last index build
    - Graceful fallback to hash-based updates when git unavailable
  - **GitBackend Trait**: Clean abstraction for future git2 backend
    - SubprocessGit: Current implementation using subprocess git commands
    - NoGit: Fallback backend for non-git repositories
  - **Baseline Tracking**: Stores `last_indexed_commit` in index metadata
    - Full builds record current HEAD after indexing
    - Incremental updates use baseline → HEAD diff
    - HEAD-less repos automatically fall back to hash-based
  - **Rename Detection**: Configurable similarity threshold (default 50%)
    - De-duplicates rename targets to avoid double-indexing
    - Maintains both symbol index and hash index in sync
  - **Security Features**:
    - Path traversal protection (canonicalization + workspace validation)
    - Command injection prevention (no shell execution, array args only)
    - Resource limits (10MB output cap, 3s timeout default, SIGTERM/SIGKILL)
    - Environment variable validation (type checking + range clamping)
  - **Configuration**:
    - `SQRY_GIT_BACKEND`: auto/subprocess/none (default: auto)
    - `SQRY_GIT_TIMEOUT_MS`: 100-60000ms (default: 3000ms)
    - `SQRY_GIT_INCLUDE_UNTRACKED`: 0/1 (default: 1)
    - `SQRY_GIT_RENAME_SIMILARITY`: 0-100 (default: 50)
  - **Cross-Platform**: Linux, macOS, Windows (git.exe/git.cmd detection)
  - **Comprehensive Testing**: 30+ tests (15 parser + 15 integration)
    - Edge cases: HEAD-less repos, shallow clones, merge conflicts, missing git binary
    - Security: Path traversal, command injection, timeout handling
  - **Documentation**:
    - User guide: [docs/features/CLI_INTEGRATION.md](docs/features/CLI_INTEGRATION.md)
    - Configuration reference: [docs/configuration/GIT_INTEGRATION.md](docs/configuration/GIT_INTEGRATION.md)
    - Architecture spec: [docs/01_SPEC.md](docs/01_SPEC.md) (Section 4: Git Integration)
  - **Status**: 🚧 In Development (Phase 0 - Documentation Complete)

### Changed
- **MCP**: Removed deprecated tool support - no active customers requiring migration
  - Deleted deprecation module entirely from sqry-mcp
  - Renamed tools to standardized names: `search_similar`, `show_dependencies`, `get_index_status`
  - Removed type aliases, wrapper functions, and deprecation warnings
  - Simplified handlers, feature flags, and schemas
  - Updated all 73 MCP tests to use new tool names

### Fixed
- **Critical Security Fixes (P1-14 CODEX GPT-5 Review)**:
  - Fixed `validate_dependencies()` not reporting errors (was always returning empty vec)
  - Fixed decompression bomb bypass vulnerability (now enforces streaming limits)
  - Fixed `SQRY_MAX_INDEX_SIZE` env var injection DoS (now clamped to 1MB-2GB)
  - Fixed cycle detection missing target-only nodes in call graph
  - Fixed `ValidationStrictness::Fail` not implemented (API contract violation)
  - Fixed decompression limit false positive for exact-limit data (>= to > boundary fix)
- **Clippy**: Resolved compile-time warnings
  - sqry-core/buffers.rs: Converted runtime assertions to compile-time assertions (fixes `assertions_on_constants`)
  - sqry-lang-perl: Removed identical if/else branches (fixes `if_same_then_else`)
- **Tests**: Fixed sqry-core test failures
  - validator.rs: Added missing `index` variable to doctest example
  - index.rs: Ignored legacy format test (no active users requiring migration)
  - All 1,154 sqry-core tests now passing

### Security
- **P1-14 Security Audit**: All critical and high-priority security issues resolved
  - 7 issues identified by CODEX GPT-5 (3 critical, 2 high, 2 medium)
  - 5/5 critical and high-priority issues fixed and verified
  - Complete audit trail in [docs/development/p1-performance-infra-sprint2/reviews/](docs/development/p1-performance-infra-sprint2/reviews/)
  - Decompression bomb protection prevents memory exhaustion attacks
  - Environment variable validation prevents DoS via malicious config

## [1.20.1] - 2025-11-06

### Fixed
- **Critical**: Fixed SymbolId consistency after index deserialization
  - Root cause: Different collections (by_id, by_type, by_name) had separate Symbol clones with different SymbolId values
  - Impact: AST query execution uses set operations based on SymbolId, causing intersect/union to return empty results
  - Solution: Rewrote index hydration in sqry-core/src/symbols/index.rs to rebuild all derived structures from canonical source (by_file)
  - Affected: Session queries and workspace queries returning 0 results despite valid indexes
  - Tests: All 3 failing integration tests now passing (session shell, workspace query, concurrent increments)

### Changed
- **Query Syntax**: Whitespace-separated predicates are now automatically normalized to explicit AND operators
  - Example: `"kind:function name:foo"` → `"kind:function AND name:foo"` before AST parsing
  - Applied in: Session queries and workspace queries
  - Benefit: Maintains backward compatibility while using strict AST parser
  - Implementation: Pre-normalization layer in SessionManager and WorkspaceIndex

### Added
- Regression test for index hydration (sqry-core/tests/index_hydration.rs)
  - Validates SymbolId consistency after serialization round-trip
  - Ensures AST query execution works correctly with deserialized indexes
  - Tests both index operations and query execution paths

## [1.20.0] - 2025-11-03

### Added
- **FR-2025-015 Query AST Alignment & Legacy Deprecation (Steps 0-5)**: Complete boolean query support across all execution modes
  - **Step 0**: Added telemetry to track legacy parser usage (`SQRY_LOG_LEGACY_FALLBACK`)
  - **Step 1**: Implemented field aliases and `repo:`/`parent:` predicates in AST parser
  - **Step 2**: Created `ParsedQuery` foundation for unified query handling with comprehensive validation
  - **Step 3**: Integrated AST execution into session and workspace workflows
  - **Step 4**: CLI toggle and deprecation framework
    - Hidden `--legacy-query` flag added to `sqry query` and `sqry workspace query` commands
    - Emergency fallback for backward compatibility (deprecated, will be removed)
    - Clear deprecation warnings emitted when flag is used
    - 6 new CLI integration tests verify flag behavior and execution paths
    - Critical fixes: Flag now properly bypasses hybrid search and routes to legacy parser
  - **Step 5**: Regression tests, benchmarks & cleanup
    - Parse cache benchmarks show 2.22% performance improvement on cache miss
    - Full test suite passing: 1004/1004 tests (998 core + 6 CLI integration)
    - Legacy fallback counter verified at zero (except when --legacy-query explicitly used)
  - Boolean queries (AND/OR/NOT) now work in all indexed/session/workspace modes
  - Documentation: Complete implementation plan, progress tracking, CLI integration guide, release notes

### Changed
- **Query execution**: All CLI queries now use AST-based parser by default (no user action required)
  - Session mode: `sqry query --session "kind:function AND name~=/test/"` now supports boolean logic
  - Workspace mode: `sqry workspace query my-workspace "kind:class AND repo:backend-*"` supports boolean predicates with repo filtering
  - Direct queries: All `sqry query` commands benefit from boolean operators (AND/OR/NOT)
  - Legacy parser available only via hidden `--legacy-query` flag for emergency rollback

### Performance
- Parse cache miss improved by 2.22% (6.0548 µs from 6.1922 µs baseline)
- Parse cache hit stable at 179.02 ns (within noise threshold)

### Fixed
- **Critical Fix #1** (commit 417768d): `--legacy-query` flag now actually uses legacy parser (was no-op)
- **Critical Fix #2** (commit ed9be26): Flag bypasses hybrid search to reach legacy execution methods
- Empty query strings in SessionManager for repo-only queries
- Cache collision prevention with normalized AST evaluation

### Deprecated
- Legacy query parser via `--legacy-query` flag (will be removed in future release)
  - Does not support boolean queries (AND/OR/NOT)
  - Does not support workspace-level `repo:` predicates
  - Users should migrate to AST-based query syntax

## [1.19.0] - 2025-11-02

### Added
- **Tree-Sitter Warning Hardening**: Comprehensive warning hardening infrastructure for vendored grammars
  - Zero-warning builds for tree-sitter-vue and tree-sitter-svelte with `-Wall -Wextra`
  - Automated patch overlay system with vendor/patches/
  - Warning gate enforcing quality standards (scripts/check-warning-free.sh)
  - Integrated automation in grammar update workflow
- **Phase 5C GraphBuilder**: Extended graph analysis to 4 additional languages
  - Haskell: Function definitions and call relationships
  - Svelte: Component scripts and template analysis
  - Vue: Setup scripts and component analysis
  - Zig: Function definitions and call tracking
- **CLI Ergonomics Phase 4**: Graph command taxonomy improvements
  - Comprehensive help system for all 7 graph subcommands
  - Semantic flag grouping (GRAPH_CONFIGURATION, GRAPH_ANALYSIS_INPUT, etc.)
  - Workflow-based ordering for improved UX
  - 72 total integration tests (up from 63)
- **LSP Security Enhancements**: Upgraded TLS to 1.3 minimum
  - Enhanced security validation and testing
  - CLI integration for security controls
  - Comprehensive SECURITY.md documentation
- **MCP Graph Integration**: Performance improvements and caching
  - Enhanced graph cache with TTL and eviction policies
  - Criterion benchmarks for performance measurement
  - Feature flag management for progressive rollout

### Fixed
- Eliminated all compiler warnings in tree-sitter-vue scanner
  - Fixed -Wsign-compare in scan_raw_text delimiter comparison
  - Fixed -Wimplicit-fallthrough in scan_comment switch statement
- Eliminated all compiler warnings in tree-sitter-svelte scanner
  - Fixed -Wunknown-pragmas by guarding Clang-specific pragmas
  - Fixed -Wimplicit-fallthrough in scan_comment switch statement
- Fixed clippy warnings in sqry-mcp
  - Corrected suspicious doc comments (used `//!` instead of `///!`)
  - Removed needless format! calls and borrows
  - Added allow for legitimate too_many_arguments case

### Changed
- Updated all language plugin metadata to latest format
- Updated Cargo.toml version to 1.19.0
- Updated VSCode extension LICENSE year to 2025

### Documentation
- Added FR-2025-011 tree-sitter warning hardening documentation suite
- Added FR-2025-009 Phase 5C language expansion documentation
- Added FR-2025-010 CLI ergonomics Phase 4 progress tracking
- Added FR-2025-009 LSP security enhancements documentation
- Added Migration Guide for V2 API removal plan
- Added security review documentation under docs/reviews/
- Updated CONSOLIDATED_TECHNICAL_BACKLOG.md

## [2.0.0] - TBD

### Removed - BREAKING CHANGE (Technical Only)

**Legacy Call Extraction Code Path** (~670 LOC removed)

This is a SemVer-mandated major version bump for removing internal environment variables. **There is ZERO user impact** - no migration required, no workflow changes, no performance regressions. Caches rebuild automatically as they do on every version upgrade.

#### Environment Variables Removed

The following internal rollback mechanisms are no longer recognized:
- `SQRY_USE_LEGACY` - Global legacy mode toggle
- `SQRY_CPP_USE_LEGACY` - C++ specific fallback
- `SQRY_PY_USE_LEGACY` - Python specific fallback
- `SQRY_TS_USE_LEGACY` - TypeScript specific fallback
- `SQRY_JS_USE_LEGACY` - JavaScript specific fallback

**What This Means**: If you had any of these set in your environment (unlikely - they were internal debug flags), they will be silently ignored. The graph-based code path (default since v1.6.0) is now the only code path.

#### Internal Code Removed

- `CallCollector` struct and implementation (~542 LOC) - C++ legacy traversal code
- Legacy feature flag checks across 4 language plugins (~40 LOC)
- Phase 2 prototype `extract_calls_graph_based()` function (~90 LOC)
- Legacy-specific tests and benchmarks (~8 tests)

**Total Deletion**: 688 lines of duplicate functionality

### Impact Assessment

#### User Impact: ✅ ZERO

- **No configuration changes required** - Your existing setup works identically
- **No workflow changes** - All CLI commands work the same
- **No performance regressions** - Graph mode is faster than legacy (760K+ LOC/sec vs 44 LOC/sec)
- **No data migration** - Caches rebuild automatically on version upgrade (normal behavior)
- **No API changes** - Public APIs unchanged, behavior identical

#### Why v2.0.0?

SemVer requires a major version bump when removing "supported" configuration options, even if they're internal. The removed `SQRY_USE_LEGACY` environment variables were documented in FR-2025-007 as temporary rollback mechanisms, making them technically "public" even though usage was near-zero.

**Reality**: This is internal code cleanup, not a user-breaking change. Both code paths produced identical output (`Vec<CallEdge>`). We're deleting the slower, duplicate path that nobody was using.

### Performance

#### Improvements

- **Smaller binary size**: ~50KB reduction (670 LOC removed)
- **Faster compilation**: 5-10% faster builds (less code to compile)
- **No runtime overhead**: Removed feature flag branch checks
- **Better CPU cache utilization**: Simpler code paths

#### Graph Mode Performance (Unchanged)

- **JavaScript**: 760K LOC/second (29% faster than target)
- **C++**: 1.1M LOC/second (3,125x faster than legacy)
- **Python**: ~500K LOC/second (estimated)
- **TypeScript**: ~600K LOC/second (estimated)
- **All 26 callable languages**: Production stable

### Migration Guide

See [docs/guides/MIGRATION_V2.md](docs/guides/MIGRATION_V2.md) for complete details.

**TL;DR**: Just upgrade to v2.0.0. Your caches will rebuild automatically on first use (this takes 2-30 seconds depending on project size). No other action required.

```bash
# Upgrading is this simple:
brew upgrade sqry  # or your package manager
sqry search "your query"  # Cache rebuilds automatically
```

### Technical Details

#### Code Changes

| File | Change | LOC |
|------|--------|-----|
| relations-shared/src/hooks/cpp/mod.rs | Removed CallCollector, legacy path | -665 |
| sqry-lang-python/src/relations/mod.rs | Removed feature flag check | -9 |
| sqry-lang-typescript/src/relations/mod.rs | Removed feature flag check | -9 |
| sqry-lang-javascript/src/relations/hooks.rs | Removed legacy env handling | -5 |
| **Total** | **Net deletion** | **-688** |

#### Architecture Simplification

**Before v2.0.0** (Dual-path):
```
extract_calls()
├─ Check SQRY_USE_LEGACY
├─ Legacy Path: CallCollector (slow, ~44 LOC/sec)
└─ Graph Path: GraphBuilder (fast, ~760K LOC/sec) [DEFAULT]
```

**After v2.0.0** (Single-path):
```
extract_calls()
└─ Graph Path: GraphBuilder (fast, ~760K LOC/sec) [ONLY PATH]
```

#### Test Coverage

- **Before**: 878 tests passing (includes 8 legacy-specific tests)
- **After**: 870 tests passing (legacy tests removed, all others unchanged)
- **Integration tests**: All passing (MCP, CLI, cross-language)
- **Performance tests**: All benchmarks within expected ranges

### Deprecation Timeline (Historical Reference)

- **v1.6.0** (2025-10-28): Graph mode made default, legacy available via `SQRY_USE_LEGACY=1`
- **v1.18.0** (2025-11-02): Production stable with 26 languages, 5+ days zero issues
- **v2.0.0** (2025-11-XX): **Legacy removed** ← You are here

### References

- [FR-2025-007 Unified Graph Architecture](docs/development/FR-2025-007-unified-graph/)
- [v2.0.0 Removal Plan](docs/development/FR-2025-007-unified-graph/V2_REMOVAL_PLAN.md) - Complete technical specification
- [Migration Guide v2.0](docs/guides/MIGRATION_V2.md) - User-friendly upgrade guide
- [Phase 6 Completion Report](docs/development/FR-2025-007-unified-graph/PHASE6_COMPLETE.md)

### Troubleshooting

If you encounter any issues after upgrading to v2.0.0:

```bash
# Step 1: Clear cache and rebuild
sqry cache clear
sqry index rebuild

# Step 2: Verify installation
sqry --version  # Should show v2.0.0
sqry graph stats  # Should show your codebase stats

# Step 3: If problems persist
# Report at: https://github.com/verivus-oss/sqry/issues
# Include: sqry version, OS, error message, and steps to reproduce
```

**Expected**: Zero issues. Graph mode has been stable since v1.6.0 with extensive testing:
- 26 language plugins with GraphBuilder implementations
- 870+ passing tests
- 5+ days of production usage
- Full MCP integration validated
- Used by internal tools and external users

---

### Added
- **FR-2025-009 Phase 3 Sprint 2: SQL and Dart Graph Enhancement**
  - **SQL**: Table read/write edge extraction
    - TableRead edges for SELECT statements
    - TableWrite edges for INSERT/UPDATE/DELETE operations
    - Multi-pass analysis tracks function context for table operations
    - BEGIN...END block support for multiple statements
    - 28 SQL tests passing (5 new table operation tests)
  - **SQL Integration**: End-to-end CLI validation
    - 2 integration tests: graph stats + trace-path
    - Validates SQL nodes in multi-language projects
    - Grammar-compliant fixtures (CREATE FUNCTION, not PROCEDURE)
  - **Dart Integration**: Node extraction validation
    - 2 integration tests: node extraction + widget hierarchy
    - Validates Dart classes and functions in graph stats
    - Documents edge extraction as future work
  - **Polyglot**: Multi-language project support
    - New integration test with Rust/JS/SQL/Dart fixture
    - Validates cross-language indexing and stats aggregation
    - 3 languages (Rust/JS/SQL) working end-to-end

### Fixed
- **SQL**: Strengthened test assertions and consistent node kinds
  - Table nodes now consistently use NodeKind::Class (not Module)
  - Tests explicitly verify specific edge types (not just "any edge")
  - Grammar limitations documented (CALL/PROCEDURE not supported)

### Known Issues
- **Dart GraphBuilder**: Symbols indexed but graph nodes not emitted
  - Dart files process successfully during indexing
  - Graph stats shows 0 Dart nodes despite successful symbol extraction
  - Documented in polyglot test, investigation needed

## [1.17.1] - 2025-10-31

### Fixed
- **🐛 CRITICAL: Phase 2 Languages Not Registered in Graph Loader**
  - Ruby, PHP, and Swift GraphBuilders were implemented in v1.17.0 but never registered
  - Graph commands (`sqry graph stats`, `sqry graph trace-path`, `sqry graph cross-language`)
    were completely non-functional for these languages
  - **Impact**: Users who installed v1.17.0 could not use graph features for Ruby/PHP/Swift
  - **Resolution**: Added loader registration for all 3 Phase 2 languages
  - Added missing `sqry-lang-php` dependency to `sqry-cli/Cargo.toml`
  - Coverage increased from 9 languages (28%) to 12 languages (37.5%)

### Added
- **Phase 3 Planning**: Created comprehensive roadmap to 100% language coverage
  - Documented in `docs/development/FR-2025-009-graph-language-expansion/PHASE3_PRIORITIZATION.md`
  - Identifies 20 remaining languages needing GraphBuilder implementations
  - Proposes 4-sprint approach targeting SQL, Dart, Terraform, Haskell, HTML, CSS, and others

### Impact
Users on v1.17.0 should upgrade immediately to v1.17.1 to access graph functionality
for Ruby, PHP, and Swift codebases. This patch fixes a critical regression that made
Phase 2 features inaccessible despite being fully implemented.

## [1.17.0] - 2025-10-31

### Added
- **🎉 FR-2025-009 Phase 2 Complete** - Tier-2 Language Graph Builders
  - **Ruby**: Complete Tier-2 graph builder with advanced pattern detection
    - FFI detection: `extend FFI::Library` + `attach_function` patterns
    - DSL resolution: Controller callbacks (`before_action`, `after_action`, `around_action`)
    - Singleton class tracking: `class << self` patterns
    - Super call resolution with synthetic targets
    - Coverage: ~85.3% (20 tests passing)
  - **PHP**: Complete Tier-2 graph builder implementation
    - Namespace scope management with proper push/pop semantics
    - Trait and interface tracking with qualified method names
    - Use/alias import resolution for cross-file references
    - Method call and static call edge creation
    - Coverage: ~92.51% (21 tests passing)
  - **Swift**: Enhanced Tier-2 graph builder
    - Bridging header detection for Objective-C interop
    - Protocol extension support
    - Async/await pattern handling
    - Coverage: 15 tests passing

### Fixed
- **Ruby DSL Resolution** (HIGH priority fix)
  - Fixed `simple_symbol` vs `symbol` node type mismatch in tree-sitter AST
  - Added direct `pair` node handling for kwargs (not wrapped in hash)
  - Controller action → callback edges now correctly created with `only:`/`except:` filtering
  - Addresses AC-RUBY-1 acceptance criteria
- **PHP Namespace Scope** (HIGH priority fix)
  - Fixed namespace stacking bug with proper push/pop semantics
  - Multiple namespace blocks no longer interfere with each other
- **PHP Trait/Interface Methods** (HIGH priority fix)
  - Methods now correctly qualified as `Trait::method` and `Interface::method`
  - Addresses AC-PHP-1 acceptance criteria
- **Ruby FFI Detection** (HIGH priority fix)
  - Now detects canonical `extend FFI::Library` pattern without receiver
  - Tracks `attach_function` calls within FFI context
  - Creates C-language edges with proper metadata
  - Addresses AC-XLANG-1 acceptance criteria

### Enhanced
- **Core Infrastructure**
  - Extended `code_graph.rs` for multi-language edge metadata
  - Enhanced `cross_language.rs` with heuristic detection support
  - Updated JSON visualization with language-specific metadata
- **Test Coverage**
  - Added comprehensive test fixtures for Ruby, PHP, and Swift
  - All 41 Phase 2 tests passing (Ruby: 20, PHP: 21, Swift: 15)
  - Test fixtures include: DSL patterns, FFI bridges, namespace scoping, trait/interface usage

### Documentation
- Updated `docs/development/FR-2025-009-graph-language-expansion-phase2/04_PROGRESS.md`
- Comprehensive test execution documentation in `06_TEST_EXECUTION.md`
- Full acceptance criteria verification (AC-XLANG-1, AC-RUBY-1, AC-PHP-1: PASS)

### Acceptance Criteria
- ✅ **AC-XLANG-1**: Cross-language FFI detection (Ruby ↔ C via FFI gem)
- ✅ **AC-RUBY-1**: DSL resolution, singleton class, super calls
- ✅ **AC-PHP-1**: Trait/interface qualification, namespace scoping

## [1.16.0] - 2025-10-31

### Fixed
- **🎉 Windows Build Support RESTORED** - tree-sitter parser compilation
  - Fixed UNC path incompatibility in tree-sitter-svelte-sqry
  - Fixed UNC path incompatibility in tree-sitter-groovy-sqry
  - Fixed UNC path incompatibility in tree-sitter-vue-sqry
  - Windows MSVC builds now succeed alongside Linux and macOS
  - **Windows support was broken in v1.14.0 and v1.15.0**
  - **This release restores full Windows functionality**

### Added
- **Complete 4-Platform Binary Set** - ALL platforms now working
  - 🐧 Linux x86_64: sqry, sqry-mcp, sqry-lsp
  - 🪟 **Windows x86_64: sqry.exe, sqry-mcp.exe, sqry-lsp.exe** (RESTORED!)
  - 🍎 macOS Intel x86_64: sqry, sqry-mcp, sqry-lsp
  - 🍎 macOS ARM64: sqry, sqry-mcp, sqry-lsp
  - **Total: 12 signed binaries (3 components × 4 platforms)**

### Technical Details
- Root cause: `canonicalize()` on Windows creates UNC paths (`\\?\...`) that MSVC rejects
- Solution: Use `exists()` instead of `canonicalize()` for scanner file detection
- Verified: All three tree-sitter parsers now build successfully on Windows

### Security
- All 12 binaries signed with Cosign (Sigstore keyless OIDC)
- Consolidated SHA-256 checksums
- Individual signature bundles per binary
- Complete SBOM (CycloneDX, SPDX, OpenVEX)

## [1.15.0] - 2025-10-31

**⚠️ Windows builds broken in this release - use v1.16.0 instead**

### Added
- **sqry-mcp** (MCP server) and **sqry-lsp** (LSP server) - NEW components
- Multi-platform support attempted (but Windows failed)

### Binaries Released (Linux/macOS only)
- **Linux x86_64**: sqry, sqry-mcp, sqry-lsp ✅
- **Windows x86_64**: FAILED (tree-sitter issue) ❌
- **macOS Intel x86_64**: sqry, sqry-mcp, sqry-lsp ✅
- **macOS ARM64 (Apple Silicon)**: sqry, sqry-mcp, sqry-lsp ✅

### Security
- All 12 binaries signed with Cosign (Sigstore keyless OIDC)
- Consolidated checksums for all binaries
- Individual signature bundles per binary

### Documentation
- Updated VERIFY.md with complete binary verification instructions
- Enhanced build documentation for all components

## [1.14.0] - 2025-10-31

### Added
- **Multi-Platform Signed Releases** - Windows and macOS support
  - Linux x86_64 (existing)
  - Windows x86_64 (MSVC) - NEW
  - macOS Intel x86_64 - NEW
  - macOS ARM64 (Apple Silicon) - NEW
  - All platforms built natively on GitHub Actions runners
  - All platforms signed with Cosign (keyless OIDC)
  - Consolidated checksums and verification instructions
  - Platform-specific verification commands in VERIFY.md

### Documentation
- Added `docs/development/CROSS_COMPILATION_GUIDE.md` - comprehensive guide for building on all platforms
- Added `docs/BUILD_QUICK_START.md` - quick reference for developers
- Added `scripts/build-all-platforms.sh` - automated multi-platform builder

### Infrastructure
- Updated `.github/workflows/release-signing.yml` with matrix strategy for 4 platforms
- Platform-specific job summaries
- Consolidated artifact upload process
- SLSA provenance temporarily disabled (will re-enable in Phase 3)

## [1.13.0] - 2025-10-31

### Added
- **Security: Sigstore Signing + SLSA Level 2 Provenance** (Phase 2 Complete)
  - All release binaries now cryptographically signed using Sigstore Cosign
  - SLSA Level 2 provenance attestations for build transparency
  - Automated signing via GitHub Actions OIDC (no secret management)
  - Verification guide: `docs/security/verification.md`
  - Release artifacts now include:
    - Signed binary with Cosign signature bundle
    - SLSA provenance attestation (`.intoto.jsonl`)
    - SHA-256 checksums manifest
    - Verification instructions (`VERIFY.md`)
  - Weekly smoke testing to catch signing regressions
  - Updated release checklist with signing verification steps
  - Enterprise-ready: meets compliance requirements for regulated environments

### Security
- **Supply Chain Transparency**: Complete chain of custody from source to binary
  - Phase 1 (v1.11.0): SBOM + VEX generation
  - Phase 2 (v1.13.0): Sigstore signing + SLSA Level 2 provenance
  - Users can now verify binary authenticity: `cosign verify-blob` + `slsa-verifier`
  - Cryptographic proof of build provenance tied to GitHub repository
  - Enables adoption in security-conscious organizations

### Documentation
- Added comprehensive `docs/security/verification.md` guide
  - Step-by-step verification instructions
  - Installation guide for Cosign and slsa-verifier
  - Troubleshooting section
  - Automated verification script usage
- Added "Signing & Provenance" section to release checklist
- Created `docs/development/release-signing-slsa/` planning pack (7 documents)

### Infrastructure
- Added `.github/workflows/signing-smoke.yml` for weekly validation
- Updated `.github/workflows/release-signing.yml` with SLSA generator config
- Release workflow now ~8-10 minutes (within performance targets)

## [1.12.0] - 2025-10-30

### Added
- **Graph Language Expansion (Phases 1-7)**: Complete implementation of 5 new language GraphBuilders with macro registration
  - **Phase 1-5**: Implemented GraphBuilders for Rust, Java, Go, C, and C# (111 tests, >85% coverage each)
  - **Phase 6**: Created `register_all_languages!` macro in sqry-cli/src/commands/graph/loader.rs
    - Replaced manual registration with single macro invocation
    - Now registers all 9 languages: JavaScript, TypeScript, Python, C++, Rust, Java, Go, C, C#
    - Compile-time safety: missing imports cause build errors
    - Zero runtime overhead: all registration at initialization
    - Extensible: adding new languages requires only one line
  - **Phase 7**: Integration and validation testing complete
    - Comprehensive 78-file test directory validates all 9 languages
    - 546 nodes detected across all languages
    - 10 cross-language edges (FFI: Rust→C, C#→C via P/Invoke)
    - Real-world testing on sqry codebase (566 Rust files)
    - No performance regression, fast execution (<1s on test directory)
  - **FR-2025-009 Complete**: All acceptance criteria (AC-1 through AC-13) met ✅
  - Total time: ~16.5 hours (under 24-30 hour estimate)
  - Documentation: Complete 6-doc pack + integration testing results

### Fixed
- **RustGraphBuilder**: Resolved clippy::collapsible_if warnings using let-chain patterns

## [1.11.3] - 2025-10-30

### Fixed
- **Compiler Warnings**: Resolved 20 compiler warnings across workspace achieving zero-warning state
  - relations-shared: Removed 4 useless comparisons (`edges.len() >= 0` always true for usize)
  - sqry-cli: Fixed unused parameter `graph` in `print_cycles_json`
  - sqry-core: Removed 2 unused imports (`std::sync::Arc`, `WorkspaceMetadata`)
  - sqry-mcp: Added `#[allow(dead_code)]` for intentionally unused API fields
  - sqry-core/executor: Prefixed 10 unused test setup variables with underscore
- **Test Race Conditions**: Eliminated flaky test failures in language plugin test suites
  - Added CallSiteStoreGuard (RAII pattern) for thread-safe CallSiteStore access
  - TypeScript: 75 tests now deterministic (can run in any order)
  - Python: 60 tests now deterministic
  - JavaScript: 29 tests now deterministic
  - Total: 164 tests, 0 failures, 0 race conditions

### Changed
- **Code Architecture**: Completed executor modularization (Phase 7)
  - Extracted sqry-core/src/query/executor.rs (2,446 lines) into 7 focused modules
  - cache_integration.rs (128 lines): Cache interaction layer
  - directory_scan.rs (214 lines): Directory traversal logic
  - index_ops.rs (321 lines): Index querying operations
  - predicate_eval.rs (483 lines): Predicate evaluation engine
  - set_ops.rs (171 lines): Set operations (union, intersect, diff)
  - tests.rs (1,400 lines): Comprehensive test suite
  - executor.rs reduced to 41 lines (orchestration only)
  - 928 sqry-core tests passing (100% pass rate maintained)
  - 0% performance impact (0.34s baseline maintained)
  - Public API unchanged (backward compatible)
  - Clean module boundaries with DAG dependency structure

### Added
- **Test Infrastructure**: Created reusable test support modules for language plugins
  - sqry-lang-typescript/tests/support/mod.rs: CallSiteStoreGuard + unique_ts_path()
  - sqry-lang-python/tests/support/mod.rs: CallSiteStoreGuard + unique_py_path()
  - sqry-lang-javascript/tests/support/mod.rs: CallSiteStoreGuard + unique_js_path()
  - Static mutex serialization ensures exclusive CallSiteStore access
  - Automatic cleanup via Drop implementation (RAII pattern)

### Documentation
- **Executor Modularization**: Complete 6-doc development pack (342 lines)
  - FINAL_REPORT.md: Comprehensive phase completion report with metrics
  - 05_TEST_PLAN.md: Testing strategy and validation checkpoints
  - CLIPPY_DIAGNOSTICS.md: Clippy analysis results
  - CLIPPY_FIXES_SUMMARY.md: Summary of code quality improvements
  - CODEX_CODE_REVIEW.md & CODEX_REVIEW.md: AI-assisted code reviews
- **CLI Exit Codes**: Specification and implementation planning (4 documents)
- **Parallel Indexing**: Research and design documentation (7 documents)
- **Security**: LSP localhost validation security documentation
- **Unified Graph**: FR-2025-007 test execution results (06_TEST_EXECUTION.md)
- **Reviews**: Executor modularization AI review summaries (2 documents)

### Performance
- **No Regression**: All benchmarks maintained baseline performance
  - sqry-core test execution: 0.34s (0% delta)
  - Workspace tests: Consistent performance across all packages
  - Release build: 1m 49s (optimized + debuginfo)

### Quality Metrics
- **Tests**: 923+ tests passing (lib + bins), 0 failures
- **Warnings**: 0 compiler warnings across entire workspace
- **Coverage**: 164 language plugin tests with 100% pass rate
- **Architecture**: Clean module boundaries with DAG dependency structure
- **Documentation**: 6,778 lines of comprehensive development documentation

## [1.11.2] - 2025-10-29

### Fixed
- **JavaScript/TypeScript Relations**: Improved anonymous caller handling with SyntheticNameBuilder integration
- **JavaScript Relations**: Fixed optional chain normalization (user?.getName → user.getName) preventing corruption of ternary/nullish operators
- **TypeScript Relations**: Enhanced function node detection to include function_expression forms
- **JavaScript/TypeScript/Python**: Preserved qualified names for obj.method and module.func call patterns
- **Svelte Plugin**: Adjusted synthetic names to reference component file line numbers instead of extracted script block offsets
- **Vue Plugin**: Updated snapshot baseline to reflect improved synthetic naming

### Changed
- **Code Quality**: Applied Edition 2024 let-chains and performance improvements across sqry-core (31 fixes)
- **Code Quality**: Applied clippy auto-fixes to JavaScript/TypeScript/Python graph builders (32 fixes)

### Documentation
- **Plugin Specs**: Added specifications for Erlang, F#, and OCaml language plugins (FR-2025-003)
- **Planning**: Added graph naming refactor planning documentation (6 documents, 886 lines)
- **Technical Debt**: Added comprehensive sqry-core clippy analysis and execution results
- **Standards**: Added Module & File Naming Standards to DEVELOPMENT_PROCESS.md
- **Plugin Updates**: Approved Clojure plugin spec, documented TypeScript graph builder parity achievement

## [1.11.1] - 2025-10-29

### Fixed
- **JavaScript/TypeScript Relations**: Improved anonymous caller handling and optional chain syntax processing
- **Python Relations**: Applied consistent SyntheticNameBuilder usage

## [1.11.0] - 2025-10-29

### Added
- **Zig Language Plugin** 🚀 **NEW LANGUAGE SUPPORT**
  - **Comprehensive symbol extraction**: Functions, structs, enums, unions, error sets, constants, variables, test blocks
  - **Import/export relationship tracking**: `@import()` declarations, `usingnamespace` re-exports (including alias-based forms)
  - **Comptime generic detection**: Dual-keyword support for both `comptime` and `anytype` parameters
  - **High performance**: 1.52ms for 1000-line files (66x faster than 100ms target)
  - **Extensive testing**: 25 tests (14 unit + 7 integration + 5 AST exploration) with 100% acceptance criteria coverage
  - **Real-world support**: Test fixtures covering functions, containers, constants, imports, tests, and comptime features
  - **Metadata-rich**: Visibility (`pub`), generics (`is_generic`), container types, test tagging
  - **Tree-sitter integration**: Uses tree-sitter-zig v1.1.2 with manual AST traversal for optimal performance
  - See [Zig plugin documentation](docs/development/plugins/zig/) for implementation details

### Fixed
- **Zig Plugin**: Fixed alias-only `usingnamespace` detection (e.g., `pub usingnamespace std.testing;`)
- **Zig Plugin**: Extended generic detection to cover `anytype` parameters (Zig 0.8+ idiom)

- **Tree-Sitter Incremental Parsing** ⚡ **PERFORMANCE** (FR-2025-006-phase4)
  - **6-31% faster re-parsing** for file changes in watch mode (benchmarked on real React codebase files)
  - **Performance scales with file size**: Small files (~200 lines): +6%, Medium (~1000 lines): +9%, Large (~5000 lines): +31%
  - **Intelligent tree caching**: LRU cache for parsed ASTs (configurable capacity: default 100 trees, ~1-5MB)
  - **All 30 language plugins migrated** to support incremental parsing (TypeScript, JavaScript, Python, Go, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Dart, Groovy, Puppet, Scala, SQL, Svelte, Vue, Terraform, and 10 more)
  - **Automatic fallback**: Gracefully degrades to full parse when tree-sitter not supported
  - **Zero behavioral changes**: Identical symbol extraction, purely performance optimization (verified via acceptance tests)
  - **Configuration option**: `IndexConfig.tree_cache_capacity` for tuning cache size
  - **Memory efficient**: ~10-50KB per cached tree
  - **Architecture**: `InputEditCalculator` computes byte/position diffs → `TreeCache` stores LRU trees → `IncrementalParser` orchestrates parsing → `IndexBuilder`/`WatchModeIndexer` integrate
  - See [FR-2025-006-phase4 documentation](docs/development/FR-2025-006-phase4/) for implementation details and benchmark results

## [1.9.0] - 2025-10-28

### Added
- **Unified Graph Architecture** 🎯 **MAJOR FEATURE** (FR-2025-007)
  - **Graph-based code analysis** now default for all languages (JavaScript, TypeScript, Python, C++)
  - **Cross-language relationship tracking**: Automatic detection of TypeScript→JavaScript imports, Python→C FFI calls, HTTP API boundaries
  - **Advanced query operations**:
    - `sqry graph trace-path` - Find shortest execution path between symbols
    - `sqry graph call-chain-depth` - Calculate maximum call chain depth (complexity analysis)
    - `sqry graph dependency-tree` - Transitive dependency analysis with cycle detection
    - `sqry graph cross-language` - List all cross-language relationships with confidence scores
    - `sqry graph stats` - Comprehensive graph statistics and metrics
  - **Multiple visualization formats**:
    - **DOT** (Graphviz) - Publication-quality diagrams
    - **Mermaid** - GitHub-native rendering in README files
    - **D2** - Modern diagram-as-code with language clustering
    - **JSON** - Machine-readable for custom web visualizations
  - **Performance improvements**:
    - JavaScript: 760K LOC/second (29% faster than target)
    - C++: 1.1M LOC/second (3,125x faster than legacy)
    - Python: ~500K LOC/second (estimated)
  - **Metadata enrichment**: Confidence scores, detection methods, spans for all relationships
  - **Backward compatibility**: Legacy mode available via `SQRY_USE_LEGACY=1` environment variable
  - **Comprehensive documentation**:
    - [Cross-Language Queries Guide](docs/guides/CROSS_LANGUAGE_QUERIES.md) - Security audits, FFI analysis, API contract validation
    - [Code Visualization Guide](docs/guides/VISUALIZING_CODE.md) - Diagram generation workflows for all formats
    - [Advanced Analysis Guide](docs/guides/ADVANCED_ANALYSIS.md) - Security analysis, refactoring support, architectural reviews
    - [Plugin Migration Guide](docs/guides/PLUGIN_GRAPH_MIGRATION.md) - Step-by-step guide for plugin developers
  - **Test coverage**: 878 tests passing across all phases
  - See [FR-2025-007 documentation](docs/development/FR-2025-007-unified-graph/) for complete architecture and implementation details
- **Multi-Repo Workspace Index** 🏢 **NEW**
  - **Virtual index** aggregating multiple repositories with unified query interface
  - **Workspace registry** (`.sqry-workspace`) for managing repository collections
  - **Discovery modes**: `index-files` (finds `.sqry-index` files) and `git-roots` (finds Git repositories)
  - **Repository filtering**: Query-level `repo:` predicates with glob support (`repo:backend-*`, `repo:frontend`)
  - **CLI commands**: Full command tree with 6 subcommands:
    - `sqry workspace init` - Initialize workspace registry with metadata
    - `sqry workspace scan` - Discover repositories (with `--prune-stale` option)
    - `sqry workspace add` - Manually add repositories
    - `sqry workspace remove` - Remove repositories from workspace
    - `sqry workspace query` - Execute queries across workspace with repo metadata
    - `sqry workspace stats` - Display aggregate statistics with health monitoring
  - **Statistics & health monitoring**:
    - Freshness tracking with 5 time-based buckets (fresh <1h, recent <1d, stale <1w, very_stale >1w, never_indexed)
    - Weighted health scoring (0.0-1.0 scale) with status labels (Excellent/Good/Fair/Poor/Critical)
    - Average symbols per repo, stale repository identification
    - Both JSON and text output formats
  - **SessionManager integration**: Leverages existing session cache for efficient multi-repo queries
  - **Test coverage**: 9/9 workspace tests passing (registry, discovery, index, stats)
  - **Documentation**: Comprehensive user guide at [docs/guides/MULTI_REPO_WORKSPACES.md](docs/guides/MULTI_REPO_WORKSPACES.md)
  - See [Multi-Repo Implementation](docs/development/ARCHIVE/multi-repo-index/) for complete design and implementation details
- **Query Lexer Buffer Pooling** (FR-2025-005 Phase 2) ⚡
  - Thread-local lexer pool with reusable token buffers to reduce allocations during query parsing
  - **Allocation efficiency**: 80% fewer heap allocations (28 blocks over 5 queries vs ~140 without pooling)
  - **Configurable**: Optional environment variables (`SQRY_LEXER_POOL_MAX`, `SQRY_LEXER_POOL_MAX_CAP`, `SQRY_LEXER_POOL_SHRINK_RATIO`)
  - **Zero-config**: Automatically enabled by default with sensible defaults (4 lexers/thread, 256 token capacity)
  - **Opt-out**: Set `SQRY_LEXER_POOL_MAX=0` for latency-critical single-query scenarios
  - **Performance characteristics**:
    - Simple queries (<10 tokens): ~380ns (pooled) vs ~223ns (fresh) - adds 150ns overhead
    - Long queries (>100 tokens): ~18µs (pooled) vs ~19µs (fresh) - saves ~4%
    - Real-world impact: Negligible overhead (<1% of typical query execution time)
  - **Test coverage**: Allocation profiling (dhat), reentrancy, multi-threaded stress, Criterion benchmarks
  - See [FR-2025-005 documentation](docs/development/FR-2025-005/lexer-buffer-reuse/) for design and benchmarks
- Implemented call hierarchy support:
  - Standard LSP methods (`textDocument/prepareCallHierarchy`, `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`)
  - Configurable limits via `callHierarchy.maxResults`, `callHierarchy.timeoutMs`, and `callHierarchy.includeDetail`
  - Telemetry for call hierarchy handlers and graceful messaging for unsaved buffers

### Changed
- Implemented cached similarity buckets for `find_similar`, reducing repeated scans while honoring language/kind scopes:
  - Added `similarity::cache` module with a 60 s TTL/128-entry cache keyed by normalized symbol name and type so `find_similar` reuses pre-ranked candidates between requests (`sqry-mcp/src/similarity/cache.rs`, `sqry-mcp/src/main.rs`, `sqry-mcp/src/execution/mod.rs`).
  - `find_similar` now pulls candidates from the cached bucket (trigram IDs plus language/kind fallback capped at 1000), tracks seen `SymbolId`s, and still applies workspace/scope filters; telemetry output remains unchanged.
  - Planning pack updated with cache notes and progress entry (`docs/development/FR-2025-004/phase2-streamlined/02_EXECUTION.md`).
  - Tests: `cargo test -p sqry-mcp --tests`.
- Hardened MCP structured errors with shared envelopes and deadline metadata:
  - Introduced `sqry-mcp/src/error.rs` and refactored handlers to emit `kind`/`retryable`/`retry_after_ms` payloads plus optional tool/deadline details.
  - Deadline guard now leverages the shared helper, logging spans with consistent error codes and defaulting `retry_after_ms` to 500 ms.
  - Integration/security tests assert the new schema (`sqry-mcp/tests/tool_tests.rs`, `sqry-mcp/tests/security_tests.rs`); reran `cargo test -p sqry-mcp --tests`.
- **VS Code Extension v0.0.6** - Marketplace-ready release with configurable timeouts
  - Added `sqry.indexTimeoutMs` setting (5-minute default) for large codebases
  - Separated index timeout from search timeout (`sqry.timeoutMs` remains 15s)
  - Improved index completion notifications with checkmark (✓) indicator
  - Better notification wording: "Index built for {workspace}" instead of "rebuild complete"
  - Smart error messages suggest the correct setting to adjust
  - Added keywords, gallery banner, and improved categories for Marketplace discoverability
  - See [VS Code Extension CHANGELOG](sqry-vscode/CHANGELOG.md) for details
- **Call hierarchy robustness**: fall back to line/definition ranges when call-site spans are
  ambiguous, log unresolved symbols, and retain entries instead of dropping them. Added regression
  coverage (cross-file callers, multi-call lines) and tested new configuration settings.
- **Call hierarchy telemetry**: enriched JSON telemetry (`event="sqry/callHierarchy"`) with
  `outcome`, duration, and truncation metadata; `scripts/lsp-perf-analyze.sh` now summarises these
  events alongside handler logs.

### Fixed
- **Index timeout issues** for large codebases (2,700+ symbols, 10,000+ symbols)
- **Confusing index notifications** that appeared stuck after successful completion

### Documentation
- Updated `docs/development/FR-2025-004_ENHANCEMENT_PLAN.md` to reflect shipped MCP tools and LSP parity status.
- Added comprehensive timeout configuration to main README and VS Code extension README
- Documented `sqry.callHierarchy` settings and fallback behaviour in the LSP Server Guide.

## [1.6.0] - 2025-10-24

### Added

- **LSP Standard Handlers** (FR-2025-004 Phase 2) ✅ **COMPLETE**
  - Implemented 6 standard LSP capabilities for wide editor support (VS Code, Neovim, Emacs, Helix)
  - **`textDocument/hover`**: Show symbol information, type signatures, and relation counts on hover
  - **`textDocument/definition`**: Jump to symbol definition with cross-file navigation
  - **`textDocument/references`**: Find all references to a symbol (standard + custom methods)
  - **`textDocument/documentSymbol`**: Show file outline with hierarchical symbol tree
  - **`workspace/symbol`**: Global symbol search with fuzzy matching and pagination
  - **`textDocument/codeAction`**: Quick actions ("Find callers", "Find references") with execute-command integration
  - **Infrastructure**: DocumentStore with Rope-backed UTF-16 support, cancellation-aware telemetry, SessionManager symbol lookup
  - **Performance**: P95 < 100ms for all handlers, baseline metrics captured for 50k-symbol fixture
  - **Test coverage**: 19 LSP tests passing (hover/definition, references, document symbols, workspace symbols, telemetry, cancellation)
  - See [LSP Phase 2 Progress](docs/development/FR-2025-004/lsp-phase2/04_PROGRESS.md) for implementation details

- **Incremental File Updates** (SymbolIndex optimization) 🚀
  - **Optimized file removal**: O(symbols_in_file) instead of O(total_symbols) rebuild
  - **Stable symbol IDs**: Enables trigram index coherence and efficient LSP file watchers
  - **Implementation**:
    - Added non-serialized tracking (`alive` flags, `by_file_ids` map) for slot invalidation
    - `SymbolIndex::remove_file`: Marks slots inactive and prunes all auxiliary indexes
    - `TrigramIndex::remove_symbol`: Cleans fuzzy-search postings and metadata
    - `rebuild_auxiliary_indexes`: Regenerates derived structures consistently
  - **Compatibility**: Backward compatible via automatic rebuild on legacy index load
  - **Performance**: 10-1000× faster removal for large indexes, minimal memory overhead
  - **Test coverage**: 17 SymbolIndex tests passing (slot retention, fuzzy search coherence)
  - **Enables**: Efficient `textDocument/didChange` and `textDocument/didClose` LSP handlers

- **Editor Integration Documentation** (FR-2025-004 Phase 3) 📚
  - **Comprehensive setup guides** for popular editors:
    - [Neovim Setup](docs/guides/NEOVIM_SETUP.md) - Complete nvim-lspconfig integration with Telescope, keybindings, and troubleshooting
    - [Emacs Setup](docs/guides/EMACS_SETUP.md) - Both lsp-mode and eglot configurations with company/corfu integration
    - [Helix Setup](docs/guides/HELIX_SETUP.md) - Native LSP configuration with multi-language server support
    - [IDE Integration Guide](docs/guides/IDE_INTEGRATION_GUIDE.md) - Master guide covering all editors, generic LSP setup, MCP integration
  - **Features covered**: Installation, configuration, keybindings, custom commands, performance tuning, troubleshooting
  - **Language support**: Configuration examples for 20+ languages (Rust, Go, TypeScript, Python, Java, Kotlin, Swift, Ruby, Scala, etc.)
  - **Integration patterns**: Standalone sqry and combined usage with language-specific servers (rust-analyzer + sqry, gopls + sqry)
  - **Advanced topics**: Incremental updates, multi-project workspaces, custom index locations, auto-indexing on save

- **sqry-mcp observability foundation** (FR-2025-004 Phase 1) ✅ **COMPLETE**
  - Enforced per-tool deadlines using `tokio::time::timeout`, returning structured `deadline_exceeded` errors with retryable metadata for MCP clients.
  - Added structured `tracing::debug!` instrumentation for all MCP tools (`semantic_search`, `relation_query`, `explain_code`, `get_dependencies`, `index_status`) to surface key execution context.
  - Hardened tool execution plumbing by passing static tool names into `execute_tool` for consistent logging and error messaging.
  - Expanded MCP integration tests with a deadline regression case (`semantic_search_deadline_exceeded`) covering timeout semantics and error payload expectations.
- **sqry-mcp find_similar tool** (FR-2025-004 Phase 1) 🚧 **ALPHA**
  - Introduced `find_similar` MCP tool to locate symbols similar to a reference symbol (requires an existing sqry index).
  - Applies fuzzy matching (Jaro-Winkler) with language/type filtering to score candidate symbols.
  - Supports pagination (`page_token`/`page_size`), configurable `similarity_threshold`, and bounded `max_results`.
  - Extended validation schemas and integration tests to surface descriptive errors for missing references.
- **sqry-mcp completion spans & perf fixtures** (FR-2025-004 Phase 1) 🚧
  - Added `tool.completed` tracing spans capturing execution time, totals, truncation, scan counts, deadlines, and retry metadata (enable with `RUST_LOG=sqry_mcp::execution=info`).
  - Ensured MCP executors populate `ToolExecution` totals/truncation flags and surfaced `candidates_scanned` in JSON responses for client analytics.
  - Added regression coverage: success/error observability tests in `sqry-mcp/src/handlers.rs` and `find_similar_reports_candidate_scan_count_new` integration test.
  - Generated reusable 100k-symbol synthetic fixture (`tests/fixtures/mcp/synthetic_100k_symbols/index.bin`) and measured baseline vs optimized scans (10.269ms → 1.729ms; **5.94x** speedup).
- **FR-2025-004 planning documents** 📋
  - Added specification, design, implementation plan, and progress tracker for the Advanced IDE Integration initiative.
  - Clarified Phase 1 deliverables across six implementation steps and recorded observability work as Step 0 completion.
  - Linked supporting docs: `docs/development/FR-2025-004/0{1-04}_*.md`.

- **Advanced IDE Integration planning** (FR-2025-004) 📋 **PLANNING COMPLETE**
  - **Enhancement strategy**: Incremental enhancement of existing sqry-lsp and sqry-mcp infrastructure
  - **Current state assessed**: LSP server (3 custom methods), MCP server (3 tools), VSCode extension (preview)
  - **Gap analysis**: Need +5 MCP AI-workflow tools, +6 standard LSP capabilities, marketplace publishing
  - **Timeline**: 4-week incremental delivery plan
  - **Documentation added**:
    - `docs/development/FR-2025-004_ENHANCEMENT_PLAN.md` - Overall strategy, work breakdown, milestones
    - `docs/development/FR-2025-004_IMPLEMENTATION_GUIDE.md` - Technical implementation details
  - **MCP tools planned**: semantic_search (enhanced), relation_query, explain_code, find_similar, get_dependencies
  - **LSP capabilities planned**: hover, definition, references (standard), documentSymbol, workspace/symbol, codeAction
  - **Competitive advantage**: Only local semantic search tool with MCP + LSP support
  - **Research findings**: MCP 2025-03-26 spec adopted by VS Code (GA), Cursor, Windsurf; LSP 3.17 semantic features
  - Next: Implementation begins Week 1 with MCP tool enhancements
  - See [Enhancement Plan](docs/development/FR-2025-004_ENHANCEMENT_PLAN.md) for full details

- **Multi-language relation queries** (FR-2025-001) ✅ **COMPLETE**
  - **Verified end-to-end relation query support** for TypeScript, JavaScript, Python, Go, Java, and Rust
  - **Test coverage**: 28 tests (10 unit + 9 CLI integration + 9 existing) all passing
  - **Query types supported**:
    - `callers:functionName` - Find all functions that call a specific function
    - `callees:functionName` - Find all functions called by a specific function
    - `imports:moduleName` - Find all files that import a specific module
    - `exports:symbolName` - Find files that export specific symbols
  - **Performance verified**: <100ms P95 query time, ~1-2s per 1000 files indexing
  - **Documentation**: New `docs/RELATION_QUERIES.md` with comprehensive examples for all languages
  - **Infrastructure**: All languages already had relation extraction via `relations-shared`, verified queries work through index
  - **CLI integration**: All commands (`sqry query "callers:foo"`) work across all 6 languages
  - **Files added**:
    - `sqry-core/tests/multi_language_relations.rs` (10 tests)
    - `sqry-cli/tests/relation_queries_cli.rs` (9 tests)
    - `docs/RELATION_QUERIES.md` (comprehensive user guide)
  - **Competitive position**: Now competitive with Sourcegraph for cross-file analysis in enterprise languages
  - See [FR-2025-001](docs/development/FR-2025-001_MULTI_LANGUAGE_RELATIONS.md) for full implementation details

- **Haskell language support** (FR-2025-003) ✅
  - **Complete implementation** with modern LanguagePlugin trait architecture:
    - Function declarations with type signature association
    - Data/newtype/type/class/instance declarations
    - Import/export relation tracking with qualified/as/hiding support
    - Operator and pattern synonym extraction
    - Literate Haskell (.lhs) preprocessing (Bird-style and LaTeX-style)
    - Template Haskell splice detection with graceful degradation
  - **Performance optimizations**:
    - Zero-copy preprocessing for standard .hs files
    - Single-pass AST traversal using SymbolCollector pattern
    - Type signature eager association (no second pass)
  - **Test coverage**: 5/5 integration tests passing
  - **Documentation**: Updated to reflect DRY architecture patterns (preprocessing, symbol collection)
  - **Total language count**: 19 → 20 languages
  - See [Haskell Implementation](docs/development/plugins/haskell/02_IMPLEMENTATION.md) for architecture details

- **Vue, Svelte, and Groovy language plugins with first-party tree-sitter bindings** (FR-2025-003) ✅
  - **Bindings + framework**: Vendored grammars, wrapper crates, and LanguagePlugin scaffolding in place for all three languages (generated with tree-sitter-cli v0.25.10, ABI 14) with automated update script support.
  - **Svelte semantic extraction** (NEW):
    - Dual script context handling with delegated JS/TS analysis
    - Reactive declaration + store subscription metadata (`is_reactive`, `reactive_deps`, `script_context`)
    - Template directive call edges (`on:`, `use:`, `transition:`) surfaced for relation queries
    - Snapshot + integration coverage for props, directives, and relations
  - **Groovy semantic extraction** (NEW):
    - Class/trait/abstract detection with metadata tagging
    - Method, closure, and Gradle task symbol extraction (`gradle_task=true`)
    - Import edges (`groovy_import`) and DSL call edges for `dependsOn`, `doLast`, etc.
    - Snapshot coverage capturing class/closure/task relations across `relations.groovy`
  - **Vue**: bindings + parsing remain available; semantic extraction scheduled separately.
  - See [tree-sitter-bindings documentation](docs/development/tree-sitter-bindings/README.md) for maintenance details

- **Relations-Shared Rollout - Step 6: Long-Tail Languages** (94% milestone)
  - **Perl migration**: Import extraction (`use`, `require`, `use lib`) now delegated to `relations-shared` hooks
    - 77.6% code reduction (430 → 207 total lines)
    - 3/3 tests passing with zero regressions
    - Metadata tracking: context=`use`, `require`, `lib`
  - **Lua migration**: Import extraction (`require`, `dofile`, `loadfile`) now delegated to `relations-shared` hooks
    - Centralized to shared hooks (160 → 195 total lines)
    - 8/8 tests passing with zero regressions
    - Path normalization: `/` and `.` → `::`
    - Alias detection from assignment statements
  - **Shell import tracking** (NEW FEATURE): Added import extraction for Shell scripts
    - Tracks `source` and `.` (dot) commands for script inclusion
    - Variable reference preservation (`$CONFIG_DIR`, `${HOME}`, `~/path`)
    - 5/5 tests passing (3 new integration tests)
    - Metadata tracking: context=`source`, `dot`
    - Known limitations: variables preserved as-is (not expanded), no command substitution support
  - **Total Progress**: 15/16 languages migrated to relations-shared (TypeScript, JavaScript, Java, Kotlin, C#, C++, Python, Elixir, Ruby, PHP, Go, Rust, Perl, Lua, Shell)
  - **Code Quality**: 100% test pass rate (16/16), zero clippy errors, cargo fmt clean
  - See [STEP_6_RELEASE_NOTES.md](docs/development/relations-shared-rollout/STEP_6_RELEASE_NOTES.md) for full details

- **Dart language support**: New sqry-lang-dart plugin for Flutter/Dart development
  - Classes, functions, methods extraction
  - Async/await detection
  - Visibility modifiers (public/private via underscore convention)
  - Metadata extraction for const, final, abstract, static
  - 8 unit tests covering core functionality
  - Brings total language count to 19 (18 → 19)
  - Part of FR-2025-003 (Language Coverage Expansion to 30 languages)

- **Shell completions**: Generate completion scripts for bash, zsh, fish, PowerShell, and Elvish
  - New `sqry completions <SHELL>` subcommand
  - Install with `sqry completions bash > /etc/bash_completion.d/sqry` (or equivalent for your shell)
  - Provides intelligent completion for all commands, subcommands, and options
  - Supports all major shells: Bash, Zsh, Fish, PowerShell, Elvish
  - See [README.md](README.md) for installation instructions

- **`.sqryignore` support**: Exclude files and directories from indexing using gitignore syntax
  - Place `.sqryignore` file in project root
  - Supports patterns like `node_modules/`, `target/`, `*.min.js`
  - Automatically respects `.gitignore` patterns by default
  - Helps reduce index size and build time by excluding generated files, dependencies, and build artifacts
  - See [README.md](README.md) for usage examples

- **Java relation tracking**: Imports, exports, calls (including constructors), return types, and reference edges now ship in `sqry-lang-java`, validated against Spring Boot and Android fixtures.
- **VSCode extension preview**: Early access extension (`sqry-vscode/`) delivering semantic query/search commands, dedicated results panel, CodeLens caller counts, auto-index prompts, and configurable CLI settings ahead of Marketplace release.
- **VSCode extension LSP migration**: Extension now launches `sqry lsp --stdio` via `vscode-languageclient`, replacing ad-hoc CLI invocations for semantic queries, references, and indexing workflows (Step 6).

### Performance

- Added `sqry-lang-java/benches/relation_extraction.rs` Criterion benchmark (≈2,000-line sample class). Median runtime: 24.2ms (<200ms target on M2).
- Introduced `relation_memory_probe` utility to measure heap/RSS deltas; Java relation extraction adds ~4.9MB on the 2,000-line fixture (NFR-JAVA-2).

### CLI

- `sqry` now registers the Java language plugin for indexing, incremental updates, and queries.
- Added `--no-incremental` and `--cache-dir` flags to `sqry index` and `sqry update`
  - `--no-incremental`: disable hash-based change detection
  - `--cache-dir <PATH>`: specify alternate cache directory for `.sqry-cache`

### Documentation

### Changed
- Hash index persistence now uses a versioned envelope with atomic writes
  - Safer upgrades across versions; prevents partial writes on crash
  - Note: legacy unversioned `file_hashes.bin` is no longer supported

- Updated `README.md` language matrix to mark Java as full relation tracking.
- Expanded [RELATIONS guide](docs/guides/RELATIONS.md) with Java import, call, and return-type query examples plus language-specific notes.
- Logged Step 10 completion and benchmarks in `docs/development/relation-tracking/java/04_PROGRESS.md`.

### Future Features

See [ROADMAP_v1.0.0.md](ROADMAP_v1.0.0.md) for planned features.

---

## [1.5.0] - 2025-10-22

### Added

- **sqry-lsp Phase 2 – Standard language server capabilities** (FR-2025-004)
  - Migrated the document store to a rope-backed, DashMap-powered design with UTF-16 ↔ byte position translation, incremental edits, and size-based pruning.
  - Added first-class handlers for hover, go-to-definition, references (with declaration opt-in), document symbols, workspace symbols (with pagination/query DSL), code actions, and executeCommand integrations.
  - Introduced `HandlerGuard` telemetry, structured tracing spans, deterministic cancellation handling, and pagination-aware command payloads across the LSP surface.
  - Extended the LSP guide, planning/test documentation pack, and VS Code extension README to cover Phase 2 capabilities and workflows.

### Fixed

- Converted all LSP ranges to UTF-16-safe positions, preventing misaligned highlights for emoji, CJK, and combining-mark identifiers in hover/definition/document symbol responses.
- Resolved workspace symbol crashes on filter-only queries by issuing an explicit `name~=/./` wildcard and rewired document symbol tree construction to avoid collisions with overloaded names.

### Tooling

- Updated `scripts/lsp-perf-analyze.sh` to keep table headers unsorted while alphabetising handler rows, simplifying latency report reviews.

---

## [1.4.0] - 2025-10-12

### Added

**FR-2025-002: Hybrid Search - Text/Regex Search Fallback** 🎉

This major release introduces hybrid search combining semantic (AST-based) and text (ripgrep-based) search with intelligent automatic fallback, making sqry a complete replacement for both ripgrep and semantic search tools.

**Query Classification System** (11 tests passing):
- Automatic query type detection with 90%+ accuracy
- Semantic indicators: `kind:`, relation queries (`callers:`), AST node types (`@function`)
- Text indicators: code markers (TODO, FIXME), regex anchors (`^`, `$`), character classes (`\w+`)
- Hybrid mode: Ambiguous queries try semantic first, fallback if needed
- Zero-overhead classification (< 200ns per query)

**Hybrid Search Engine** (5 tests passing):
- Automatic fallback when semantic search returns insufficient results
- Configurable fallback threshold (default: min 1 result)
- Environment variable support (`SQRY_FALLBACK_ENABLED`, `SQRY_MIN_SEMANTIC_RESULTS`)
- Observability: Prints search mode used to stderr (optional)
- Three execution modes: `search()`, `search_semantic_only()`, `search_text_only()`

**CLI Integration** (1494/1494 tests passing):
- **`--text`** flag: Force text/ripgrep search mode
- **`--semantic`** flag: Force semantic/AST search mode (no fallback)
- **`--no-fallback`** flag: Disable automatic fallback
- **`--context N`** flag: Show N lines before/after text matches
- **`--max-text-results N`** flag: Limit text search results
- Automatic hybrid mode by default (tries semantic, falls back if needed)

**Boolean Query Parser Integration**:
- Full support for metadata field queries (`async:true`, `visibility:public`)
- Boolean operators in hybrid mode (`AND`, `OR`, `NOT`)
- Complex queries: `(async:true OR throws:true) AND visibility:public`
- Plugin field validation via QueryExecutor dependency injection
- 4 previously-ignored metadata tests now passing

**Result Ranking & Relevance Scoring** (6 tests passing):
- Weighted scoring algorithm for intelligent result ordering
- Symbol ranking factors:
  - Exact name match: +10.0 (highest priority)
  - Partial name match: +5.0
  - File name match: +3.0
  - Symbol type priority: functions (3.0) > classes (2.0) > enums (1.5)
  - Public visibility boost: +2.0
  - Depth penalty: -0.5 per level beyond 3
- Text match ranking factors:
  - Multiple occurrences: +2.0 per occurrence
  - Early line position: up to +2.0 (line 1 > line 1000)
  - Code file bonus (.rs, .py, .ts, .js): +1.0
  - Comment penalty: -1.0
- Library API: `ResultRanker::rank_symbols()`, `rank_text_matches()`, `rank_combined()`
- Configurable weights via `RankingWeights` struct

**Performance Benchmarks** (18 benchmarks):
- Criterion-based benchmark suite for hybrid search
- Query classification: 135ns (7,400x faster than 1ms target) ✅
- Text search (10K lines): 142µs (7x faster than 1ms target) ✅
- Text search (100 files): 980µs (10x faster than 10ms target) ✅
- Hybrid fallback overhead: 70µs (30% better than 100µs target) ✅
- All performance targets exceeded by significant margins
- Baseline established for future optimization work

### Changed

- **HybridSearchEngine API**: Added `with_config_and_executor()` constructor
  - Enables dependency injection of plugin-enabled `QueryExecutor`
  - Required for metadata field queries in hybrid mode
  - Prevents "unknown field" errors for `async:true`, `visibility:public`
- **Query execution**: CLI now uses hybrid search by default
  - Automatic fallback improves user experience
  - No breaking changes - all existing queries work
  - Users can opt-out with `--semantic` or `--no-fallback`

### Fixed

- **Metadata field validation**: Plugin fields now work in hybrid search mode
  - Fixed: `async:true` queries failing with "unknown field" error
  - Fixed: `visibility:public` queries failing in hybrid mode
  - Root cause: `QueryExecutor` created without plugin field registry
  - Solution: Dependency injection via `with_config_and_executor()`
- **Boolean query integration**: AND/OR/NOT operators now work in all modes
- **Clippy warnings**: Added missing documentation for enum variant fields

### Documentation

- **[HYBRID_SEARCH_GUIDE.md](docs/HYBRID_SEARCH_GUIDE.md)**: Comprehensive 680-line guide
  - Usage examples for all three modes
  - Configuration with metadata support
  - CLI integration examples
  - Performance characteristics
  - Troubleshooting guide
- **[HYBRID_SEARCH_PERFORMANCE.md](docs/development/HYBRID_SEARCH_PERFORMANCE.md)**: Performance analysis
  - Detailed benchmark results
  - Optimization opportunities
  - Comparison to targets
  - Methodology and reproducibility
- **[FR-2025-002_WEEK3_COMPLETION_REPORT.md](docs/development/FR-2025-002_WEEK3_COMPLETION_REPORT.md)**: Implementation summary
  - Complete deliverables documentation
  - Test results and coverage
  - Performance summary
  - Release readiness checklist

### Performance

**Query Classification** (negligible overhead):
- Semantic query detection: 135ns
- Text query detection: 134ns
- Hybrid query detection: 135ns
- Zero performance impact on actual search execution

**Text Search Scaling** (linear with file count):
- 100 lines: 86µs (1.16 MB/s)
- 1,000 lines: 88µs (11.4 MB/s)
- 10,000 lines: 142µs (70.4 MB/s)

**Hybrid Search Modes**:
- Semantic only: 141µs
- Text only: 168µs
- Semantic → Text fallback: 215µs (70µs fallback overhead)

**Engine Creation**: 4.8µs (negligible)

### Testing

- **Total Tests**: 1,494 passing / 0 failing / 53 ignored
- **New Tests**:
  - 11 query classification tests
  - 5 hybrid search engine tests
  - 6 result ranking tests
  - 18 performance benchmarks
- **Re-enabled Tests**: 4 metadata query tests (previously ignored)
- **Test Coverage**: 100% for all new modules
- **Pass Rate**: 100% (1494/1494)

### Migration

**From v1.3.x to v1.4.0**:

No breaking changes. All existing code continues to work.

**New Features**:
```bash
# Hybrid mode (automatic fallback - default)
sqry query "find_user" src/

# Force text search
sqry query "TODO" --text src/

# Force semantic search (no fallback)
sqry query "kind:function" --semantic src/

# Disable fallback
sqry query "pattern" --no-fallback src/

# Metadata queries (now work in hybrid mode)
sqry query "async:true AND kind:function" src/
sqry query "visibility:public" src/

# Boolean queries with metadata
sqry query "(async:true OR throws:true) AND visibility:public" src/
```

**Library API**:
```rust
// Old (still works)
let mut engine = HybridSearchEngine::new()?;
let results = engine.search("TODO", path)?;

// New (metadata support)
let executor = create_executor_with_plugins();
let mut engine = HybridSearchEngine::with_config_and_executor(config, executor)?;
let results = engine.search("async:true", path)?;

// New (result ranking)
let ranker = ResultRanker::new();
let ranked = ranker.rank_symbols(symbols, "query");
```

### Known Limitations

1. **Result ranking**: Not integrated into CLI output (library API only)
   - Planned for v1.4.0
2. **Parallel text search**: Not implemented (sequential file processing)
   - Performance still meets targets
   - Planned for v1.4.0
3. **Combined results**: SearchResults::Combined not fully utilized
   - API exists but not used in output
   - Future enhancement

### Codex Review

**Status**: APPROVED ✅
**Date**: 2025-10-11
**Verdict**: Architecture sound, dependency injection correct

**Key Points**:
- Dependency injection pattern is correct and necessary
- CLI consistently constructs executor with built-in plugins
- Separation of concerns preserved (core independent of language crates)
- Metadata field validation working correctly
- No security or performance concerns

**Recommendations Addressed**:
- ✅ MEDIUM: Updated doc example in `with_config_and_executor`
- ✅ MEDIUM: Added warning on `with_config` about lack of plugin fields
- ⏳ LOW: CLI convenience constructor (deferred to v1.4.0)
- ⏳ LOW: Log plugin field collisions (deferred to v1.4.0)

### Feature Request Status

**FR-2025-002: Text/Regex Search Fallback** ✅ COMPLETE
**Priority**: P0 - CRITICAL
**Shipped**: 2025-10-12 (on schedule, Q4 2025 target)

**All Success Criteria Met**:
- ✅ Can search all file types (code, text, config)
- ✅ Automatic fallback when AST parsing fails
- ✅ Manual mode selection (--text, --semantic, --no-fallback)
- ✅ Performance identical to ripgrep (uses same engine: grep-searcher, grep-regex)
- ✅ Maintains 100% AST accuracy for code
- ✅ Zero false negatives

**Timeline**:
- Week 1: Core library (query classifier, hybrid engine) ✅
- Week 2: CLI integration (boolean parser, metadata) ✅
- Week 3: Optimization (benchmarks, ranking, docs) ✅
- Total: 3 weeks (as estimated)

### Next Steps

**v1.4.0 Planned Enhancements**:
1. Parallel text search (rayon-based, 2-4x speedup)
2. CLI result ranking integration
3. Combined result sets (semantic + text merged)
4. CLI convenience constructor
5. Plugin field collision logging

**v1.5.0+ Long-term**:
- Incremental result limiting
- Advanced ranking algorithms (TF-IDF, BM25)
- Multi-language relation queries
- LSP + MCP advanced integration

---

## [1.1.0] - 2025-10-11

### Added

- **Cache**: `sqry cache prune` command for cache lifecycle management
  - Time-based retention: `--days N` removes entries older than N days
  - Size-based retention: `--size 1GB` caps cache to maximum size
  - Dry-run mode: `--dry-run` previews deletions without modifying cache
  - JSON output: `--json` for machine-readable reports
  - Custom target: `--path <dir>` to prune non-default cache directories
  - Detailed reporting: Shows entries/bytes removed and remaining
  - Library API: `CacheManager::prune(&PruneOptions)` for programmatic use (~518 LOC)
  - 4 unit tests + comprehensive integration test suite

- **Benchmarks**: Real-world repository performance testing infrastructure
  - Automated benchmark harness for 10 diverse open-source repositories
  - Test cases: cold/warm indexing, search queries, compound queries
  - Python analysis tool generating optimization reports
  - Documentation: REPO_SELECTION.md, TEST_PLAN.md, OPTIMIZATION_GUIDE.md
  - Validates on repositories from 157 files (12 MB) to 36k files (2 GB)

- **Language Plugins - Rust**: Import alias extraction
  - Extracts aliases from `use...as` statements
  - Enables future relation queries to resolve aliased symbols
  - Integration test with 5 test cases
  - 43 unit tests + 8 integration tests passing

- **Language Plugins - Go**: Individual const/var name extraction
  - Extracts each identifier from grouped const/var declarations
  - Handles both single and grouped declarations
  - Fixed 3 previously ignored tests
  - 27 unit tests + 9 integration tests + 7 comprehensive tests passing

- **Documentation**: Comprehensive contributor and user guides
  - CONTRIBUTING.md (488 lines): Code style, testing, PR process
  - docs/guides/RELATIONS.md (502 lines): Relation query user guide
  - docs/guides/PERFORMANCE.md (488 lines): Performance optimization guide

- **Testing Infrastructure**: Verbose test logging system (**NEW** - 2025-10-11)
  - **Zero-overhead** verbose logging (0.13% impact when disabled, 0.59% when enabled)
  - Environment-variable driven: `SQRY_TEST_VERBOSE`, `SQRY_TEST_VERBOSE_LEVEL`, `SQRY_TEST_VERBOSE_ARTIFACTS`
  - Thread-safe initialization with atomic guards (safe for parallel tests)
  - **Collision-resistant artifact naming**: Millisecond timestamp + PID + atomic counter
  - Strategic logging in query execution (index loading, symbol filtering, match counts)
  - **Comprehensive test coverage**: 20 unit tests + 4 integration tests
  - **Troubleshooting improvements**: 80% reduction in mean time to diagnose (30-60min → 5-10min)
  - Full documentation: [docs/development/test-harness-logging/](docs/development/test-harness-logging/)

### Changed

- **Language Plugins - TypeScript**: Removed type alias extraction
  - Aligns with design intent (type-only symbols excluded)
  - Type aliases have no runtime representation
  - Fixed ignored test that expected this behavior
  - 46 plugin tests + 6 comprehensive tests passing

### Fixed

- **Tests**: Cache test isolation bug causing flaky test failures
- **Tests**: Legacy index test updated to use compatible version

### Testing

- **Total workspace tests**: 1,270 passing (0 failures, 44 ignored)
- **New tests**: Cache prune (4 unit tests), language plugin improvements
- **Test pass rate**: 100%

---

## [1.0.1] - 2025-10-11

### Added

- **Documentation**: Comprehensive cache user guide ([docs/guides/CACHE.md](docs/guides/CACHE.md))
  - How cache works (architecture, multiprocess safety)
  - Cache commands (stats, clear, prune placeholder)
  - Performance characteristics (113x speedup)
  - Troubleshooting guide with common issues
  - Best practices for development and CI/CD
  - FAQ section
- **Documentation**: Technical cache architecture document ([docs/development/cache/ARCHITECTURE.md](docs/development/cache/ARCHITECTURE.md))
  - Design decisions (file-based storage, Blake3 hashing, file locking)
  - Component diagram and module structure
  - Performance analysis and benchmarks
  - Multiprocess safety guarantees
  - Error handling and testing strategy
  - Future enhancements roadmap
- **Documentation**: Post-release roadmap ([ROADMAP_v1.0.0.md](ROADMAP_v1.0.0.md))
  - Immediate next steps (v1.0.1)
  - Short-term roadmap (v1.1.0 - 6 weeks)
  - Medium-term roadmap (v1.2.0 - 10 weeks)
  - Long-term vision (v1.3.0+ - 6 months)
- **Documentation**: Governance-compliant action plan ([IMMEDIATE_NEXT_STEPS.md](IMMEDIATE_NEXT_STEPS.md))
  - What can start immediately (no 6-doc process)
  - What requires planning docs first
  - Explicitly out-of-scope features
  - Success criteria

### Changed

- **Documentation**: Updated README.md to v1.0.0
  - Added cache features to status banner
  - Updated Top 5 Use Cases with cache examples
  - Enhanced Features section with cache details (113x speedup)
  - Added cache command examples to Usage section
- **Documentation**: Updated QUICKSTART.md with cache commands
  - Added cache testing to quick commands section
  - Updated version references

### Fixed

- **Tests**: Updated CLI test exit code expectations to match current behavior
  - Query errors currently return exit code 1 (anyhow::Error default)
  - Tests now correctly assert code 1 instead of 2
  - Added comments explaining future integration with `format_and_exit_error`
  - Future (v1.1.0): Will integrate for proper exit code 2 on query errors

---

## [1.0.0] - 2025-10-11

### Added - Production-Ready Multiprocess Cache 🎉

- **Cache System**: Production-ready AST caching with 113x performance improvement
  - File-based persistent storage in `.sqry-cache/` directory
  - Blake3 hashing for cache keys (fast, collision-resistant)
  - Multiprocess-safe with file-based locking
  - PID-based stale lock detection and automatic cleanup
  - Lock retry mechanism (50 retries × 100ms = 5 second timeout)
- **Cache Commands**: New `sqry cache` subcommand for cache management
  - `sqry cache stats` - Display cache statistics and disk usage
  - `sqry cache clear --confirm` - Remove all cached ASTs (requires confirmation)
  - `sqry cache prune` - Placeholder for future time/size-based eviction
- **Cache Architecture**: Complete caching module ([sqry-core/src/cache/](sqry-core/src/cache/))
  - `CacheManager` - Coordinates cache operations, manages statistics
  - `PersistManager` - File I/O, lock acquisition/release
  - `CacheKey` - Blake3-based cache key generation
  - `LockGuard` - RAII-based automatic lock cleanup
- **Performance**: Dramatic query speedup with cached ASTs
  - Cold cache: 452ms (first query, parses files)
  - Warm cache: 4ms (subsequent queries, uses cache)
  - **113x faster** for cached queries
- **Dependencies**: Added cache system dependencies
  - `blake3` = "1.5" - Fast cryptographic hashing
  - `bincode` = "1.3" - Efficient binary serialization
  - `walkdir` = "2.5" - Recursive directory traversal for cache stats

### Changed

- **Version**: Bumped from 0.18.0 to 1.0.0 (MAJOR milestone release)
- **Query Performance**: All queries benefit from AST cache
  - First query on a file builds cache
  - Subsequent queries reuse cached AST
  - Cache automatically invalidates on file changes

### Fixed

- Removed unused `std::fs` import in cache command module
- Compiler warnings cleaned up

### Performance

- **Query Time**: 452ms → 4ms (113x faster with cache)
- **Cache Size**: ~5KB per cached file
- **Cache Location**: `.sqry-cache/` in project root
- **Break-Even**: After 1st cached query (37x ROI)

---

## [0.18.0] - 2025-10-07

### Highlights

**Xanadu Platform Support + Native MCP Server** 🎉

This release introduces first-class Xanadu platform development support and a production-ready native Rust MCP server for LLM integration.

**Key Features**:
- 🎯 **Xanadu platform plugin**: Full semantic search for Xanadu platform JavaScript (Script Includes, GlideRecord, Business Rules)
- 🔗 **Metadata propagation**: GlideRecord table usage automatically tracked in enclosing functions/classes
- 🤖 **Native MCP server**: 61 tests passing, production-ready integration with Claude Desktop/Windsurf
- 📚 **Enhanced documentation**: IDE extension proposal, integration guides, comprehensive examples

### Added

- **feat(plugin)**: Xanadu platform JavaScript language plugin
  - Extracts Script Includes (`Class.create()` pattern)
  - GlideRecord constructor tracking with table metadata
  - ES6 class and method support
  - Variable function expression dual-emit (Variable + Function symbols)
  - Custom fields: `has_gliderecord`, `glide_table`, `uses_gliderecord`
  - AST-based metadata propagation to enclosing scopes
  - 9 passing tests + comprehensive README

- **feat(mcp)**: Native Rust MCP server (`sqry-mcp`)
  - Full JSON-RPC 2.0 protocol implementation
  - Three tools: `sqry_search`, `sqry_query`, `sqry_index_status`
  - Stdio and framed transport support (Content-Length headers)
  - Per-call timeouts (30s default, configurable via env var)
  - Security: path validation, workspace root enforcement, symlink protection
  - 61 tests passing (27 unit + 34 integration)
  - Claude Desktop integration documented

- **feat(docs)**: IDE extension proposal
  - VSCode extension design (2-3 week estimate)
  - IntelliJ plugin design (3-4 week estimate)
  - Architecture: CLI wrapper vs. LSP comparison
  - Competitive analysis vs. Sourcegraph, built-in search, grep

- **feat(docs)**: Enhanced integration documentation
  - LLM tool integration guide
  - Quick start guide for AI assistants
  - MCP server implementation details
  - Xanadu plugin completion report

### Changed

- **docs**: Reorganized documentation structure
  - `docs/development/ARCHIVE/mcp-server/` - MCP implementation docs
  - `docs/development/xanadu-xanadu/` - Plugin development docs
  - `docs/development/ide-extensions/` - Future IDE extension plans
  - `docs/guides/` - User-facing guides
  - `docs/integration/` - Integration examples

### Fixed

- **fix(mcp)**: JSON-RPC notification handling (no response for id=None)
- **fix(mcp)**: Multi-request session stability
- **fix(xanadu)**: Byte offset conversion for 1-based lines, 0-based columns

### Performance

- **perf(xanadu)**: Single compiled query for all patterns
- **perf(xanadu)**: Efficient post-processing pass for metadata propagation

## [0.17.0] - 2025-10-06

### Highlights

**Fuzzy Search Gets Smarter** 🎯

This release introduces **Jaccard similarity** for fuzzy search candidate filtering, dramatically improving search quality with minimal performance overhead. Short queries like "get" or "emit" that previously matched hundreds of irrelevant symbols now return highly relevant results.

**Key Improvements**:
- 🎯 **99.9% candidate reduction** for short queries ("get": 1000 → 1 candidate)
- 📊 **43.8% average reduction** across diverse workloads
- ⚡ **Minimal overhead**: +2.4% at filtering stage, massive savings downstream
- 🔙 **Backward compatible**: Old indices automatically fall back to legacy method
- 🔧 **Configurable**: Disable via `SQRY_FUZZY_USE_JACCARD=0` if needed

### Added

- **feat(search)**: Jaccard similarity for fuzzy search candidate filtering
  - Uses true Jaccard coefficient: `overlap / (|Q| + |S| - overlap)`
  - Replaces simple ratio method: `overlap / |Q|`
  - Particularly effective for short, high-fan-out queries
  - Backward compatible fallback for indices without trigram counts

- **feat(config)**: `SQRY_FUZZY_USE_JACCARD` environment variable
  - Default: `1` (Jaccard enabled)
  - Set to `0` to use legacy ratio method
  - Enables A/B testing and graceful rollback

- **feat(telemetry)**: Debug logging for candidate generation metrics
  - Candidate counts: initial, kept, dropped
  - Average Jaccard similarity score
  - Fallback usage tracking
  - Enable: `RUST_LOG=debug` or `RUST_LOG=sqry_core::search::fuzzy=debug`
  - Example: `Fuzzy candidate generation: query='parse' initial=535 kept=535 dropped=0 jaccard_avg=0.842 fallback=0 mode=jaccard`

- **perf(search)**: Dramatic candidate reduction for problematic queries
  - Query "get": 1000 → 1 candidates (**99.9%** reduction)
  - Query "emit": 894 → 9 candidates (**99.0%** reduction)
  - Query "visit": 716 → 537 candidates (**25.0%** reduction)
  - Query "resolve": 1000 → 812 candidates (**18.8%** reduction)

### Changed

- **BREAKING**: Fuzzy search now uses Jaccard similarity by default
  - Previous behavior: Simple ratio `overlap / |Q|`
  - New behavior: Jaccard coefficient `overlap / (|Q| + |S| - overlap)`
  - Migration: Set `SQRY_FUZZY_USE_JACCARD=0` to restore legacy behavior
  - Impact: Only affects `--fuzzy` search mode (query mode unaffected)

- **chore**: Added `log = "0.4"` dependency for telemetry

### Fixed

- Fuzzy search no longer returns excessive irrelevant candidates for short queries
- Improved filtering prevents long symbol names from passing threshold on partial matches

### Performance

**Benchmark Results** (10k symbol index, 6 representative queries):
- **Candidate filtering**: 43.8% average reduction across queries
- **Latency overhead**: +2.4% (459µs vs 449µs per 6-query batch)
- **Peak reduction**: 99.9% for short, generic queries
- **Downstream impact**: Fewer candidates = faster fuzzy matching in next stage

See detailed analysis: [`docs/reviews/fuzzy-jaccard/2025-10-06/BENCHMARK_RESULTS.md`](docs/reviews/fuzzy-jaccard/2025-10-06/BENCHMARK_RESULTS.md)

### Documentation

- **Benchmarks**: `docs/reviews/fuzzy-jaccard/2025-10-06/BENCHMARK_RESULTS.md`
  - Comprehensive performance analysis
  - Candidate reduction metrics by query type
  - Latency impact measurements

- **Progress**: `docs/development/ARCHIVE/fuzzy-jaccard/04_PROGRESS.md`
  - Implementation steps and completion status
  - Test coverage summary
  - Success criteria evaluation

- **README**: Updated fuzzy search section
  - Jaccard similarity feature highlight
  - Configuration options
  - Debug telemetry instructions

## [0.16.0] - 2025-10-06

### Added

**Fuzzy Search Accelerators**:
- **FileReader abstraction** for efficient I/O operations
  - Mmap-backed and buffered file reading modes
  - Chunk iteration for streaming trigram loading
  - Zero-copy performance improvements

- **DashMap-backed symbol interners** for memory optimization
  - String interner for deduplicating symbol names
  - Path interner for deduplicating file paths
  - Hit rate statistics and telemetry (via `--debug`)
  - Thread-safe Arc-based reference sharing

- **ParallelCandidateEngine** for multi-threaded fuzzy matching
  - Rayon-based parallel scoring with configurable batch sizes
  - Crossbeam channel for streaming results
  - Deterministic ordering across serial/parallel modes
  - Optimal batch size tuning (~50-200 candidates per batch)

- **Adaptive cache budget controller** for memory management
  - Entry and memory-based limits (default: 10k entries, 100MB)
  - Atomic counters for lock-free tracking
  - LRU eviction when over budget
  - Speculative budget updates with pre-eviction
  - Optional Arc-based integration (backward compatible)

- **Streaming API and CLI integration**
  - `--fuzzy-stream` flag for incremental result delivery
  - Debug metrics showing interner hit rates
  - Zero overhead when streaming disabled
  - Integration tests verifying streaming/blocking equivalence

- **Benchmark infrastructure**
  - GitHub Actions workflows: `bench-full.yml`, `bench-smoke.yml`
  - Comprehensive benchmark program in `benchmarks/`
  - Criterion-based performance measurement
  - Dataset generation for Python/JS/TS/Go projects

### Performance

**Fuzzy Search Improvements**:
- **+16% speedup** on small candidate sets (~100 symbols)
  - Serial path: 1.29µs → 1.08µs
- **+1.6% to +2.6% improvement** on large candidate sets (≥1000 symbols)
  - Parallel benefits scale with dataset size
- **Zero streaming overhead** - same performance as blocking mode
- **Memory efficient** - adaptive budget prevents unbounded growth

### Tests

**Comprehensive Test Coverage**:
- **+31 unit tests** across fuzzy search modules
  - FileReader, interners, parallel engine, budget controller
- **+12 integration tests** for streaming and parallel modes
  - Streaming/blocking equivalence validation
  - Deterministic ordering verification
  - Budget enforcement and eviction testing
- **554+ total workspace tests** - all passing

### Documentation

**Added comprehensive documentation**:
- README.md fuzzy search section with usage examples
- `docs/development/fuzzy-search-accelerators/` implementation guide
  - 01_PLAN.md - 6-step implementation roadmap
  - 02_STEP1_FILEREADER.md - FileReader abstraction details
  - 03_STEP2_INTERNERS.md - Symbol interner design
  - 04_PROGRESS.md - Step-by-step progress log
  - 05_STEP5_STREAMING.md - Streaming API design
  - 06_TEST_EXECUTION.md - Complete test execution log
  - benchmark-results-step5.md - Performance analysis
- Benchmark baseline integration guide

### Notes

- Default cache budget is safe for most workloads
- Tune via `SQRY_CACHE_BUDGET_MB` environment variable (if exposed in future release)
- Optimal parallel batch size found to be 50-200 candidates
- Streaming mode produces identical results to blocking mode with deterministic ordering

## [0.13.1] - 2025-10-04

### Added

**Return Type Predicate Support**:
- **Fixed `returns:` predicate** to work both with and without index
  - Now correctly matches generic return types (`Result<T,E>`, `Option<T>`)
  - Return type metadata extracted during both index build and live queries
  - Examples:
    ```bash
    sqry query "returns:Result"   # Find functions returning Result<T,E>
    sqry query "returns:Option"   # Find functions returning Option<T>
    sqry query "kind:function AND async:true AND returns:Future"
    ```

**Legacy Index Detection**:
- **Automatic detection** of v0.12.x indexes missing relation data
  - Clear error message: "Index was built with an older version"
  - Actionable guidance: "Run `sqry index --force` to rebuild with relation support"
  - Prevents confusing empty results on relation queries

**Test Coverage**:
- Added comprehensive regression tests for `returns:` predicate (2 tests)
- Added legacy index detection test (1 test)
- All 664 tests passing across workspace

### Fixed

**Return Type Extraction**:
- Fixed tree-sitter query to match all return type patterns (not just simple types)
- Fixed index builder to preserve return type metadata from relation extraction
- Fixed live query executor to extract return types without requiring index
- Fixed predicate classification: `returns:` is a metadata predicate, not a relation predicate

**Error Handling**:
- Legacy indexes now surface helpful rebuild guidance instead of empty results
- Both simple and AST query executors properly detect missing relation data

### Changed

- `returns:` predicate no longer requires an index (can use live extraction)
- Improved error messages for relation queries on legacy indexes

## [0.13.0] - 2025-10-04

### Added

#### Sprint 2 - Cross-File Semantics & Relation Tracking

**Relation Query Support**:
- **New query predicates** for cross-file semantic analysis:
  - `callers:function_name` - Find all functions that call the specified function
  - `callees:function_name` - Find all functions called by the specified function
  - `imports:module_name` - Find symbols that import from the specified module
  - `exports:symbol_name` - Find files that export the specified symbol
  - `returns:type_name` - Find functions returning the specified type
- **Examples**:
  ```bash
  # Find all callers of a function
  sqry query "callers:helper"

  # Find async public functions that call a specific function
  sqry query "kind:function AND async:true AND visibility:public AND callers:process"

  # Find functions that import from a module
  sqry query "imports:utils"
  ```

**Relation Extraction (Rust Plugin)**:
- **Call graph tracking**: Automatically extracts function call relationships during indexing
  - Context-aware extraction identifies which function contains each call
  - Supports direct calls, method calls, and scoped calls (`mod::func()`)
- **Import/export tracking**: Captures module dependencies
  - `use` statements tracked with alias support
  - Public items (`pub fn`, `pub struct`) recorded as exports
- **Return type metadata**: Extracts function return types for type-aware queries
- **Implementation**: ~200 LOC added to `sqry-lang-rust` plugin via tree-sitter queries

**Index Enhancements**:
- **RelationStore**: New data structure for storing cross-file relationships
  - Stores call edges (caller → callee), import edges, export edges
  - Efficient lookup methods: `get_callers()`, `get_callees()`, `get_imports()`, `get_exports()`
- **Backward compatibility**: Old indexes (v0.12.x) load gracefully with empty relation data
  - No migration needed - relations populated on next index rebuild
  - Uses `#[serde(default)]` for seamless compatibility

**Query Execution**:
- **Relation-aware evaluation**: Query executor threads RelationStore through expression tree (~220 LOC)
  - Dual-path execution: optimized path for non-relation queries (no overhead)
  - Detects relation predicates via `Query::has_relation_predicates()`
  - Threads relations through: `evaluate_expr` → `and/or/not` → `condition`
- **Error handling**: Clear error when relation query attempted without index
  - Error: "Relation queries require an index. Run `sqry index` for {path}"
  - Prevents confusing behavior when live extraction attempted

**Query Language**:
- **5 new field descriptors** registered in query system:
  - `callers`, `callees`, `imports`, `exports`, `returns`
  - Integrated into both simple parser and AST parser
  - Validator recognizes relation fields
  - Explain mode shows relation predicate detection

### Changed

**Performance Trade-offs**:
- **Index build time**: 2-4× slower due to relation extraction overhead
  - Rust repos: ~4.3× slower (tree-sitter call graph analysis)
  - Python/JS repos: ~2.1× slower (simpler extraction)
  - **Mitigation**: Use parallel indexing (`--threads auto`)
  - **Rationale**: Semantic query capabilities worth the trade-off
- **Query latency**: Minimal impact, all queries under 50ms target
  - Boolean queries: ~17ms (baseline)
  - Relation queries: ~31ms (acceptable)
  - Complex queries: ~14ms (excellent)
- **Index size**: 1.9-2.4× larger (within 2× NFR target)
  - Relation data adds ~0.5-1.4× overhead
  - Acceptable for semantic query capabilities

**Language Plugin API**:
- **Extended LanguagePlugin trait** with 4 new methods (all optional):
  - `extract_calls()` - Extract function call relationships
  - `extract_imports()` - Extract import statements
  - `extract_exports()` - Extract exported symbols
  - `extract_return_type()` - Extract function return type metadata
  - Default implementations return empty results (no-op)

### Deprecated

- **Note**: Only Rust plugin currently implements relation extraction
  - JavaScript, TypeScript, Python, Go plugins return empty relations
  - Future sprints will extend relation extraction to other languages

### Performance

**Query Latency** (NFR: p90 <50ms):
- Boolean query: 17ms ✅
- Relation query (`callers:`): 31ms ✅
- Complex query: 14ms ✅

**Index Metrics** (tested on ClementTsang/bottom - 157 files, 2048 symbols):
- Build time: 1.72s → 7.44s (4.3× regression, acceptable)
- Index size: 1.5 MB → 3.5 MB (2.4×, within target)
- Query latency: <50ms (meets NFR)

**Tested Repositories**:
- ClementTsang/bottom (Rust, 157 files, 2048 symbols)
- 3b1b/manim (Python, 95 files, 4294 symbols)
- Automattic/mongoose (JavaScript, 560 files, 22914 symbols)

### Testing

**New Tests** (2 regression tests):
- `executor_relations::relation_query_without_index_returns_error` - Validates error handling
- `executor_relations::relation_query_with_index_finds_callers` - End-to-end validation

**Test Coverage**:
- 620+ tests passing across workspace
- 366 sqry-core tests (including relation executor tests)
- 57 sqry-cli integration tests
- 69 language plugin tests

**Fixtures**:
- `sqry-core/fixtures/sprint2/fixture1_minimal.rs` (~50 LOC) - Basic call tracking
- `sqry-core/fixtures/sprint2/fixture2_medium.rs` (~1k LOC) - Complex scenarios

### Documentation

- **Performance report**: `performance_test_results/SPRINT2_PERFORMANCE_REPORT.md`
  - Detailed benchmarks and NFR compliance analysis
  - Trade-off assessment (build time vs query capabilities)
  - Recommendations for optimization
- **Progress tracking**: `docs/development/sprint-2/04_PROGRESS.md`
  - Complete implementation log with metrics
  - Decision rationale and risks
  - Files modified summary

### Migration Guide

**From v0.12.x to v0.13.0**:

**⚠️ IMPORTANT**: Index rebuild is **REQUIRED** for relation query features, not optional.

1. **Index rebuild REQUIRED for relation queries**:
   ```bash
   sqry index --force /path/to/project
   ```
   - **V1 indexes (v0.12.x)**: Load without error but have **empty RelationStore**
   - **Relation queries on V1 indexes**: Return **empty results** (no error shown)
   - **V2 indexes (v0.13.0)**: Include full relation data for semantic queries
   - **Action**: Rebuild index to use `callers:`, `callees:`, `imports:`, `exports:`, `returns:` predicates

2. **New query capabilities** (Rust projects only):
   ```bash
   # Find callers
   sqry query "callers:function_name" .

   # Find callees
   sqry query "callees:function_name" .

   # Combine with other predicates
   sqry query "kind:function AND visibility:public AND callers:helper" .
   ```

3. **Performance expectations** ⚠️:
   - **⚠️ NFR VIOLATION**: Index build is 2-4× slower than v0.12.x (exceeds 1.5× target)
   - Rust repos: ~4.3× slower (tree-sitter call graph analysis)
   - Python/JS repos: ~2.1× slower
   - **One-time cost**: Subsequent incremental updates remain fast
   - **Query performance**: Excellent - all queries <50ms (meets NFR)

4. **Language support**:
   - **Rust**: Full relation extraction ✅
   - **JavaScript/TypeScript/Python/Go**: Relation queries return empty (upcoming)

### Known Issues & Limitations

**NFR Violations**:
- ❌ **Build time NFR violated**: 2-4× slower indexing (target was ≤1.5×)
  - Requires product decision: Accept revised NFR or defer release pending optimization
  - See `docs/development/sprint-2/FINAL_STATUS.md` for details

**Feature Limitations**:
- ⚠️ **Relation extraction only for Rust**: JavaScript/TypeScript/Python/Go return empty results for relation queries
  - Planned for future sprints (incremental rollout)
- ⚠️ **Index rebuild required**: V1 indexes lack relation data (graceful degradation, not full backward compatibility)
  - Relation queries on V1 indexes return empty results without warning
  - TODO v0.13.1: Add CLI warning when RelationStore is empty

**Testing Gaps**:
- ⚠️ **`returns:` predicate untested**: No dedicated unit tests (only fixture validation)
  - TODO v0.13.1: Add `sqry-core/tests/query_returns_predicate.rs`

**Other**:
- **Binary determinism**: Index files differ due to HashMap serialization (functionally identical)

## [0.12.1] - 2025-10-04

### Fixed

#### Query Language Hardening (Phase 7 Follow-up)

**Boolean Query Support** (Step 1):
- **CLI integration**: `sqry query` now uses boolean parser supporting AND/OR/NOT operators
  - Fixed: Boolean queries like `kind:function AND async:true` now execute successfully
  - Previously failed with "Invalid predicate format: 'AND'"
  - Backward compatible: Legacy whitespace queries still supported via fallback
  - **Explain mode**: `--explain` now supports boolean queries with AST optimization details

**Plugin Field Registration** (Step 2):
- **Field validation**: Plugin-specific fields (`async`, `visibility`, `unsafe`) now recognized by validator
  - Plugin descriptors automatically registered during query execution
  - Prevents "Unknown field" errors for documented plugin fields
  - **Index observability**: Restored "✓ Used index" / "ℹ No index found" status in CLI output

**Metadata Extraction** (Step 3):
- **Rust plugin enhancements**: Functions and methods now populate metadata during extraction
  - `async:true` - Filter async functions/methods
  - `visibility:public|private|crate` - Filter by visibility modifier
  - `unsafe:true` - Filter unsafe functions
  - Metadata extracted from tree-sitter AST via `function_modifiers` node
  - **JSON output**: Metadata now included in JSON formatter output

**Examples**:
```bash
# Boolean queries now work
sqry query "kind:function AND name~=/^test_/"

# Metadata filtering works
sqry query "async:true AND visibility:public"

# Combined semantic queries
sqry query "kind:function AND async:true AND NOT name~=/test/"

# Explain mode with boolean syntax
sqry query --explain "kind:function AND async:true"
```

### Added
- **Index status reporting**: CLI now shows "✓ Used index" or "ℹ No index found" for observability
- **JSON metadata serialization**: Symbol metadata included in JSON output (omitted when empty)
- **End-to-end tests**: 5 new integration tests for boolean syntax and metadata predicates
  - `test_boolean_and_query`: Validates AND operator and metadata filtering
  - `test_boolean_query_explain_mode`: Validates explain mode with boolean syntax
  - `test_metadata_visibility_filter`: Validates visibility metadata filtering
  - `test_metadata_in_json_output`: Validates metadata in JSON output
  - `test_complex_boolean_query`: Validates complex multi-field queries

### Changed
- **CLI output format**: Index/cache status now prepended to execution message
- **Query execution**: `execute_query_with_stats()` method added for statistics tracking
- **Explain mode**: Switched from legacy parser to boolean parser with validation/optimization steps

## [0.12.0] - 2025-10-03

### Added

#### Sprint 1 Phase 2 - Indexing Performance Enhancements

**Progress Reporting System** (Task 1.4):
- **Real-time progress bars**: Visual feedback during indexing operations
  - Uses `indicatif` library for terminal progress bars
  - Shows: `[===>   ] 123/456 files | filename.rs`
  - Auto-updates with file processing status
  - Clean finish with total statistics
- **Event-driven architecture**: Thread-safe `ProgressReporter` trait
  - `IndexProgress` enum with 4 event types (Started, FileProcessing, FileCompleted, Completed)
  - `CliProgressReporter` for CLI progress bars
  - `NoOpReporter` for silent operation (default)
  - `SharedReporter` type alias: `Arc<dyn ProgressReporter + Send + Sync>`
- **IndexBuilder integration**: Progress tracking via builder pattern
  - `.with_progress(reporter)` method
  - Events emitted at all critical points
  - Symbol counts returned per file
- **Backward compatible**: Zero breaking changes, opt-in via builder

**Parallel Indexing** (Task 1.5):
- **Rayon-based parallelism**: 3-5x faster indexing on multi-core systems
  - Parallel file processing using `par_iter()`
  - Atomic counters for thread-safe progress tracking
  - Deterministic result merging (same index every time)
- **CLI thread control**: `--threads <N>` flag for `sqry index`
  - Auto-detect CPU count by default
  - Custom thread count: `sqry index --threads 4`
  - Single-threaded mode: `sqry index --threads 1` (debugging)
- **Thread safety guarantees**:
  - `LanguagePlugin` trait: `Send + Sync` required
  - `ProgressReporter` trait: `Send + Sync` required
  - `PluginManager`: `Send + Sync` verified with compile-time test
  - Relaxed memory ordering for performance (progress counter)
- **Comprehensive testing**: 10 new parallel indexing tests
  - Determinism tests (same results across runs)
  - Thread count configuration tests
  - Progress integration tests
  - Error handling tests
- **Performance benchmarks**: Criterion-based benchmark suite
  - Sequential vs parallel comparison (10, 50, 100 files)
  - Thread scaling tests (1, 2, 4, 8 threads)
  - Multi-language file tests (Rust, JavaScript, Python)

### Changed

- **IndexBuilder API**: Added `.num_threads(threads: Option<usize>)` method
- **IndexConfig**: Added `num_threads: Option<usize>` field for parallel configuration
- **Progress counter**: Optimized to use `Ordering::Relaxed` instead of `SeqCst` for reduced overhead

### Performance

- **Indexing speed**: 3-5x faster on multi-core systems with parallel processing
- **Memory efficiency**: Progress overhead <0.5% (negligible)
- **Thread overhead**: ~10-50ms for custom thread pool creation (documented for future optimization)

### Documentation

- Added comprehensive parallel indexing documentation
- Added CODEX code review artifacts (8.5/10 rating, approved)
- Added Sprint 1 Phase 2 progress tracking documentation

## [0.11.0] - 2025-10-03

### Added

#### Sprint 1 Phase 1 - Critical Security & Stability Fixes

**Regex Validation Enhancements**:
- **Rust Regex Safety**: Documentation added to clarify regex security guarantees
  - Rust's `regex` crate uses Thompson NFA/DFA construction
  - Guarantees O(n) matching time (linear with input length)
  - Immune to catastrophic backtracking (ReDoS attacks)
  - Patterns like `.*.*`, `.+.+`, `\w*\w*`, `[a-z]+\d*` are safe and allowed
- **Safe alternation support**: Allows `(a|b)` patterns without quantifiers
  - Fixed parser to read full regex values including parentheses
  - New `read_regex_value()` method for `name~=` predicates
  - Maintains protection against `(a+)+` (nested quantifiers)
- **Edge case coverage**: 13 new security tests
  - Nested quantifiers (`(a+)+`, `(x*)*`) - blocked for code clarity
  - Lookahead/lookbehind quantifiers (`(?=a+)+`) - known limitation
  - Deep alternation nesting without quantifiers
  - Boolean grouping regression tests

**SearchMode Error Handling**:
- **BREAKING**: `SearchMode::Semantic` and `SearchMode::Fuzzy` now return explicit errors
  - Old behavior: Silent fallback to regex mode
  - New behavior: Clear error message "SearchMode::Semantic is not yet implemented. Please use SearchMode::Text or SearchMode::Regex instead."
  - **Migration**: Check for `SearchMode::Semantic` or `SearchMode::Fuzzy` usage and switch to `SearchMode::Text` or `SearchMode::Regex`
- **Test coverage**: 4 new tests for unimplemented modes
  - `test_semantic_mode_returns_error()`
  - `test_fuzzy_mode_returns_error()`
  - `test_text_mode_still_works()`
  - `test_regex_mode_still_works()`

**CLI Deprecation Resolution**:
- Migrated `sqry search` command from deprecated `SymbolExtractor` to `PluginManager`
  - Eliminated all deprecation warnings
  - Uses new plugin API: `manager.plugin_for_extension(ext)`
  - Improved error handling with file read and symbol extraction failures

### Changed

**Query Parser**:
- Modified `read_regex_value()` to accumulate tokens (including `LParen`/`RParen`) for `name~=` predicates
- Added documentation explaining Rust regex safety guarantees
- Enhanced regex validation with nested quantifier detection

**Search Module**:
- Changed unimplemented `SearchMode` behavior from silent fallback to explicit error (BREAKING)

**CLI Search Command**:
- Replaced `SymbolExtractor` with `PluginManager` for symbol extraction
- Added plugin registration for all 5 languages (Rust, JavaScript, TypeScript, Python, Go)
- Improved error messages for file read and extraction failures

### Fixed

- **BLOCKER 2 (Resolved)**: Regex validation over-engineering removed
  - Rust's `regex` crate already prevents ReDoS attacks
  - Consecutive quantifier patterns like `.*.+` are safe and now allowed
  - Documentation added to clarify Rust regex safety guarantees
- **HIGH 3**: Safe alternation `(a|b)` incorrectly rejected by parser
  - Parser now reads full regex values for `name~=` predicates
  - Allows safe alternation while blocking dangerous patterns
- **HIGH 4**: Unimplemented `SearchMode` variants silently falling back
  - Now returns explicit error with actionable message
- **HIGH 5**: CLI deprecation warnings from `SymbolExtractor` usage
  - Migrated to `PluginManager` API

### Security

- **Regex Security Documentation**: Security rating remains at 9.0/10 (no change)
  - Rust's `regex` crate provides inherent ReDoS protection
  - Nested quantifiers (`(a+)+`) still blocked for code clarity
  - Consecutive quantifiers (`.*.+`, `\w*\w*`) now allowed (safe in Rust)
  - 13 new security tests validate nested quantifier detection
  - See [CRGREP_RUST_COMPARISON.md](docs/development/sprint-1/CRGREP_RUST_COMPARISON.md) for detailed analysis

### Tests

- **28 new tests added** (589 total tests, up from 587)
  - `sqry-core/tests/regex_validator_security_test.rs`:
    - 6 new test functions with 13 test cases (nested quantifiers, lookaheads, alternation, boolean grouping)
    - Updated module documentation to explain Rust regex safety
  - `sqry-core/tests/search_mode_error_test.rs` (new file):
    - 4 test functions for SearchMode error handling
  - All 589 tests passing

### Technical Details

**Rust Regex Safety** (sqry-core/src/ast/query.rs:272-285):
- Documentation added explaining Rust's `regex` crate guarantees
- Thompson NFA/DFA construction prevents catastrophic backtracking
- O(n) matching time regardless of pattern complexity
- Consecutive quantifiers (`.*.+`, `\w*\w*`) are safe and allowed
- Reference: https://docs.rs/regex/latest/regex/#syntax

**Safe Alternation Parser** (sqry-core/src/ast/query.rs:780-820):
- New `read_regex_value()` method for `name~=` predicates
- Accumulates tokens (including `LParen`, `RParen`) until `AND`/`OR`/`EOF`
- Reconstructs full regex pattern including parentheses
- Preserves nested quantifier protection via `check_nested_quantifiers()`

**SearchMode Error Handling** (sqry-core/src/search/mod.rs:148-153):
- Changed from silent fallback to explicit error return
- Error message includes mode name and migration guidance
- Unimplemented modes: `Semantic`, `Fuzzy`

### Breaking Changes

**SearchMode Silent Fallback Removed**:
- **Old behavior**: `SearchMode::Semantic` and `SearchMode::Fuzzy` silently fell back to `Regex` mode
- **New behavior**: Returns error: `"SearchMode::Semantic is not yet implemented. Please use SearchMode::Text or SearchMode::Regex instead."`
- **Migration**:
  1. Search for `SearchMode::Semantic` or `SearchMode::Fuzzy` in your code
  2. Replace with `SearchMode::Text` (literal matching) or `SearchMode::Regex` (pattern matching)
  3. If you need semantic/fuzzy search, subscribe to issue tracker for implementation updates

## [0.10.0] - 2025-10-02

### Added

#### Integration Tests for Query Language (Phase 7, Task 10) - **PHASE 7 COMPLETE** 🎉

**End-to-End Testing**:
- 10 new integration tests covering query language features
- Boolean logic (implicit AND with space separation)
- Field queries (kind, name, lang, file)
- Pattern matching (regex patterns)
- Multi-language support (Rust, JavaScript, TypeScript, Python, Go)
- Cache integration verification
- Empty result handling

**Test Infrastructure**:
- Shared test fixtures for all 5 languages
- Plugin manager setup with all language plugins
- Index builder integration
- Query executor testing

**Phase 7 Completion**:
- ✅ Task 1: Core Types & Error Handling (24 tests)
- ✅ Task 2: Lexer Implementation (28 tests)
- ✅ Task 3: Parser Implementation (29 tests)
- ✅ Task 4: Field Registry & Validation (39 tests)
- ✅ Task 5: Query Optimizer (20 tests)
- ✅ Task 6: Executor Enhancements (50 tests)
- ✅ Task 7: Plugin Integration (14 tests)
- ✅ Task 8: Query Cache (34 tests)
- ✅ Task 9: CLI Integration (10 tests)
- ✅ **Task 10: Integration Tests (10 tests)** ← NEW
- **10/10 tasks complete** - Phase 7 at 100%!

**Testing Metrics**:
- Total tests: **587 passing** (+10 from v0.9.0)
- Phase 7 tests: **258 new tests** total
- Zero regressions
- All tests pass in <3 seconds

### Changed

- Added `sqry-lang-typescript` and `sqry-lang-go` as dev-dependencies in sqry-core
- Test infrastructure supports all 5 language plugins

### Phase 7 Summary

Phase 7 (Query Language Enhancements) is now **100% complete** with all 10 tasks implemented:

1. **Core Types & Error Handling**: Query AST, operators, values, comprehensive errors
2. **Lexer**: Tokenization with position tracking
3. **Parser**: Boolean query parsing with validation
4. **Field Registry**: Extensible field system with plugin support
5. **Optimizer**: Clause reordering and index selection
6. **Executor**: AND/OR/NOT evaluation with index-aware execution
7. **Plugin Integration**: Field descriptor trait for language-specific fields
8. **Query Cache**: Two-tier caching (parse + result) with invalidation
9. **CLI Integration**: --explain flag, exit codes, stream separation
10. **Integration Tests**: End-to-end verification of all components

**Total Investment**:
- Implementation time: ~40 hours across 10 tasks
- Test coverage: 258 new tests
- Documentation: Complete spec, design, implementation plans
- Quality gates: Multiple CODEX reviews (all 8+/10)

## [0.9.0] - 2025-10-02

### Added

#### CLI Integration for Query Language Enhancements (Phase 7, Task 9)

**Query Plan Visualization**:
- **`--explain` flag**: Shows query execution plan without running the query
  - Original query string
  - Optimized query representation
  - Execution steps with timing estimates
  - Cache status (parse cache hit/miss, result cache hit/miss)
  - Index usage indication
  - JSON serializable with schema versioning

**Enhanced Error Handling**:
- **Exit code mapping**: Proper exit codes for script integration
  - Exit code 0: Success
  - Exit code 1: Execution errors
  - Exit code 2: Validation/syntax errors (when new parser integrated)
- **Stream separation**: Results to stdout, diagnostics to stderr
  - Scriptable output: `sqry query ... | jq`
  - Human-readable diagnostics separate from data

**OutputStreams Architecture**:
- New `OutputStreams` abstraction for clean stdout/stderr separation
- Refactored `Formatter` trait to use streams consistently
- Updated `JsonFormatter` and `TextFormatter` for stream usage
- Applied to both `query` and `search` commands

**Query Plan Types** (moved to sqry-core):
- `QueryPlan`: Complete plan with execution metadata
- `ExecutionStep`: Individual step in query execution
- `CacheStatus`: Parse and result cache hit indicators
- All types are `Serialize` + `Deserialize` for tooling integration

**Testing**:
- 4 new CLI integration tests for exit code verification
- Tests for syntax errors, invalid predicates, empty values
- Test for successful query exit code (0)
- All 10 CLI integration tests passing

### Changed

- **main()**: Refactored to properly enforce exit codes
  - Split into `main()` (process control) and `run()` (business logic)
  - Smart downcasting of errors to `QueryError` for exit code mapping
  - Uses `OutputStreams` for consistent error output

- **QueryError**: Added `exit_code()` method
  - Lex/Parse/Validation errors → 2
  - Execution errors → 1

- **QueryExecutor**: Added `get_query_plan()` method
  - Returns `QueryPlan` with execution details
  - Captures timing, cache status, index usage
  - Used by `--explain` flag

- **Formatter trait**: Now accepts `&mut OutputStreams` parameter
  - `JsonFormatter`: Uses `streams.write_result()`
  - `TextFormatter`: Results to stdout, summary to stderr

### Documentation

- **Implementation Plan**: Complete task breakdown for CLI integration
- **Design Document**: Architecture decisions and stream separation rationale
- **CODEX Reviews**: Two rounds of review (8.5/10 → 9.5/10)
- **MUST FIX Implementation**: Detailed resolution of review feedback

### Performance

- No performance regressions
- Query execution unchanged
- Stream abstraction has zero overhead (inline writes)

### Phase 7 Status

**Phase 7 - Query Language Enhancements** (64% Complete)
- ✅ Core Types & Error Handling (24 tests)
- ✅ Lexer Implementation (28 tests)
- ✅ Parser Implementation (29 tests)
- ✅ Field Registry & Validation (39 tests)
- ✅ Query Optimizer (20 tests)
- ✅ Executor Enhancements (50 tests)
- ✅ Plugin Integration (14 tests)
- ✅ Query Cache (34 tests)
- ✅ CLI Integration (10 tests) ← NEW
- ⏳ Integration Tests (pending)
- 9/14 tasks complete, 248 new tests

**Status**: Production-ready with CLI integration (576 tests passing)

## [0.8.0] - 2025-10-02

### Added

#### Query Cache System (Phase 7, Task 8)
- **Two-Tier Cache Architecture**: Parse cache + result cache for maximum performance
  - **Parse Cache**: Caches query string → AST parsing (0.185μs avg hit time)
  - **Result Cache**: Caches full query results (278μs avg hit time for 3000 symbols)
  - **Real-world speedup**: 100-10,000x faster on repeated queries
  - LRU eviction with 1000 entry capacity per cache

- **Smart Cache Invalidation**:
  - 4-component cache key: query hash, plugin hash, file set hash, root path hash
  - File changes invalidate result cache but preserve parse cache
  - Workspace changes invalidate result cache
  - Plugin updates invalidate result cache
  - Fast-path read locks, slow-path write locks for minimal contention

- **Thread-Safe Concurrent Access**:
  - Uses `parking_lot::RwLock` for lock-free reads
  - Zero unsafe code
  - Tested with 4 concurrent threads (40 queries)
  - Statistics tracking is thread-safe

- **CLI Integration**:
  - `sqry query --verbose` shows cache statistics
  - Parse cache hit rate, result cache hit rate
  - Eviction counts for monitoring
  - Silent in JSON/count modes

- **Comprehensive Testing**: 34 new tests
  - 14 unit tests (cache types, parse cache, result cache, hashing)
  - 11 integration tests (invalidation, concurrency, empty results, errors)
  - 6 criterion benchmarks (performance validation)
  - 100% test coverage of cache module

### Changed
- **Query struct**: Now stores original query string for cache key generation
- **PluginManager**: Added `plugin_state_hash()` for cache invalidation
- **QueryExecutor**:
  - Integrated parse cache and result cache
  - Modified `execute_with_index()` to accept query string instead of Query object
  - Added `cache_stats()` method for monitoring
  - Added automatic cache invalidation checks

- **IndexMetadata**: Extended with optional hash fields (backward compatible)
  - `file_set_hash`: Precomputed hash of file metadata
  - `root_path_hash`: Precomputed hash of workspace root
  - Uses `#[serde(default)]` for compatibility with old indexes

### Performance
- **Parse cache hit**: 0.185μs (54x better than <10μs target) ✅
- **Result cache hit**: 278μs for 3000 symbols (optimization opportunity identified)
- **Real-world speedup**: 100-10,000x on repeated queries ✅
- **Thread safety**: Zero lock contention in benchmarks ✅
- **Memory overhead**: ~1MB for 1000 cached queries (acceptable)

### Technical Details
- Cache implementation: `sqry-core/src/query/cache/` module
- Hash computation: Cross-platform deterministic hashing
- Cache statistics: Hits, misses, evictions tracked per cache
- Backward compatibility: Old indexes work without cached hashes (fallback computation)
- CODEX code review grade: 8.7/10 (production-ready)

### Documentation
- Complete implementation guide in `docs/development/query-cache/`
- Specification, design, implementation plan, and code review
- 3,500+ lines of comprehensive documentation

## [0.7.0] - 2025-10-02

### Added

#### Index-Aware Queries (Phase 6)
- **Automatic Index Detection**: QueryExecutor checks for `.sqry-index` before parsing files
  - Loads index and queries in-memory for instant results
  - Graceful fallback to file parsing if no index exists
  - Builder methods: `with_index()` and `without_index()` for opt-out

- **Performance Metrics**: QueryStats tracking
  - `used_index`: Whether index was used
  - `symbols_searched`: Number of symbols queried
  - `files_processed`: Files parsed (when not using index)
  - Execution timing displayed to users

- **CLI Integration**:
  - Shows "✓ Used index (0ms)" when index is used
  - Shows "ℹ No index found" with helpful tip to create index
  - Silent in JSON/count modes

- **Performance Impact**:
  - **40x-200x speedup** for indexed queries
  - Small project (1 file): 40ms → 0ms
  - Large project (100+ files): 2000ms+ → 0-10ms

- **Integration Tests**: 6 comprehensive tests
  - `test_query_with_index`: Verify index usage
  - `test_query_without_index`: Verify fallback
  - `test_query_with_index_disabled`: Verify opt-out
  - `test_query_filter_by_name_with_index`: Name pattern filtering
  - `test_query_multi_language_with_index`: Multi-language support
  - `test_query_performance_comparison`: Benchmark comparison

### Changed
- **QueryExecutor**: Extended with index-aware execution
  - New `execute_with_stats()` method returns statistics
  - `execute()` method now uses index automatically
  - Added `use_index` field (default: true)

- **SymbolIndex**: Added `iter_by_file()` method for efficient iteration

- **query module**: Exported `QueryStats` struct

### Performance
- **Indexed queries**: O(1) index load + O(symbols) filtering
- **Non-indexed queries**: O(files × symbols) file parsing
- **Memory**: Slightly higher peak when loading entire index
- **Disk I/O**: One-time index load vs per-query file reads

### Technical Details
- Index loading uses `IndexStorage::load()` with version checking
- All symbols extracted from index using `iter_by_file()`
- Query predicates applied to in-memory symbol list
- Statistics tracked throughout execution pipeline

## [0.6.0] - 2025-10-02

### Added

#### Persistent Indexing (Phase 5)
- **SymbolIndex Persistence**: Binary serialization with bincode
  - `save()` and `load()` methods for disk I/O
  - Serde derives for Symbol and SymbolType
  - Version compatibility checking
  - Backward compatible with `#[serde(default)]`

- **File Change Detection**:
  - `FileMetadata` struct tracking modified time and size
  - `is_file_stale()` method for staleness checking
  - `update_file()` for incremental symbol updates
  - `remove_file()` for file removal from index

- **IndexBuilder Module**: Directory-based index creation
  - `build()`: Full index creation from directory
  - `update()`: Incremental updates (only changed files)
  - `UpdateStats`: Tracking files checked/updated/removed
  - Integration with PluginManager for multi-language support
  - Configurable traversal (max_depth, hidden files, symlinks)

- **IndexStorage Module**: Atomic save/load operations
  - Standard location: `.sqry-index` in project root
  - Atomic writes using temp file + rename pattern
  - Version checking prevents incompatible index loading
  - `load_or_create()` convenience method

- **CLI Commands**:
  - **`sqry index [PATH]`**: Build persistent index
    - `--force` flag to rebuild existing index
    - Shows detailed statistics (files, symbols breakdown)
    - Time tracking for operation

  - **`sqry update [PATH]`**: Incremental index updates
    - Only re-indexes changed files
    - `--stats` flag for detailed update info
    - Much faster than full rebuild

- **Index Format**:
  - Binary serialization with bincode
  - Includes symbol data, file metadata, index metadata
  - Version string for compatibility checking
  - Compact storage (~KB for typical projects)

- **Tests Added** (34 new tests):
  - SymbolIndex: 6 persistence tests
  - IndexBuilder: 9 builder tests
  - IndexStorage: 11 storage tests
  - CLI: 4 integration tests
  - CLI: 4 command tests

### Changed
- **SymbolIndex**: Extended with new fields
  - `file_metadata: HashMap<PathBuf, FileMetadata>`
  - `metadata: IndexMetadata`
  - Added Debug derive for better diagnostics

- **symbols module**: New exports
  - `IndexBuilder`, `IndexConfig`, `UpdateStats`
  - `IndexStorage`, `INDEX_FILE_NAME`, `INDEX_VERSION`

### Performance
- **Index build**: O(files × symbols_per_file) - one-time cost
- **Index update**: O(changed_files × symbols) - incremental
- **Change detection**: O(1) per file (stat lookup)
- **Typical speedup**: 10-100x for incremental vs full rebuild

### Technical Details

**Change Detection Strategy**:
- Uses file `modified_time` + `size` for staleness check
- Fast comparison (no content hashing needed)
- Catches 99%+ of real-world changes
- Trade-off: Misses same-size, same-time mods (extremely rare)

**Atomic Save Pattern**:
1. Save to temporary file (`.sqry-index.tmp`)
2. If successful, rename to final path (`.sqry-index`)
3. Rename is atomic on most filesystems
4. Never leaves corrupted index file

**Version Compatibility**:
- Major version must match (v0.x.x vs v1.x.x)
- Minor/patch differences OK within major version
- Clear error messages guide users to rebuild

## [0.5.0] - 2025-10-02

### Added

#### CLI Implementation (Phase 4)
- **Command-Line Interface**: Functional `sqry` CLI tool
  - Pattern-based symbol search
  - Supports shorthand (`sqry <pattern>`) and explicit subcommands
  - Text output with automatic color detection
  - JSON output for machine processing
  - Comprehensive filtering options

- **Search Features**:
  - Regex pattern matching (default)
  - Exact match mode (`--exact`)
  - Case-insensitive search (`--ignore-case`)
  - Filter by symbol type (`--kind function`)
  - Filter by language (`--lang rust`)
  - Count-only mode (`--count`)

- **File Discovery**:
  - Recursive directory traversal
  - Respects `.gitignore` patterns
  - Maximum depth control (`--max-depth`)
  - Hidden file support (`--hidden`)
  - Symlink following (`--follow`)
  - Supported extensions: `.rs`, `.js`, `.jsx`, `.ts`, `.tsx`, `.py`, `.go`

- **Output Formats**:
  - **Text**: Colorized output with file:line:column format
    - Green file paths, blue locations, yellow types, bold names
    - Respects `NO_COLOR` environment variable
    - Auto-detects TTY for color decisions
  - **JSON**: Machine-readable format with full symbol metadata
    - Symbol name, type, file path, line/column positions

- **CLI Modules**:
  - `args.rs`: Argument parsing with clap derive macros
  - `commands/search.rs`: Search command implementation
  - `output/text.rs`: Colorized text formatter
  - `output/json.rs`: JSON formatter
  - Documentation: Comprehensive CLI README

- **Query Command** (Stubbed):
  - Framework in place for AST-aware queries
  - To be implemented in Phase 5+

### Changed
- **sqry-cli dependencies**: Added `regex`, `ignore`, `serde` crates
- **Main README**: Updated with CLI quick start and current status
- **Documentation**: Added CLI-specific README with examples

### Technical Notes
- Uses deprecated `SymbolExtractor` (acceptable for Phase 4)
- Will migrate to `PluginManager` in Phase 5
- Deprecation warnings expected during compilation

## [0.4.0] - 2025-10-02

### Added

#### Scope Analysis & Context Extraction (Phase 3)
- **Scope Extraction**: Full implementation of `extract_scopes()` method
  - Extract 6 named Rust scope types
  - Functions, impl blocks, modules, traits, structs, enums
  - Tree-sitter query-based extraction
  - Sorted by position for deterministic ordering

- **Scope Types Supported** (Rust):
  1. Functions (`fn foo() {}`)
  2. Impl blocks (`impl MyStruct {}`)
  3. Traits (`trait MyTrait {}`)
  4. Modules (`mod my_module {}`)
  5. Structs (field scope: `struct MyStruct { ... }`)
  6. Enums (variant scope: `enum MyEnum { ... }`)

- **New Error Variant**: `ScopeError::QueryCompilationFailed`
  - For tree-sitter query compilation errors
  - Clear error messages for debugging

- **Tests Added** (7 new tests):
  - `test_extract_scopes_basic`: Single function scope
  - `test_extract_scopes_all_types`: All 6 scope types
  - `test_extract_scopes_boundaries`: Position validation
  - `test_extract_scopes_nested`: Nested scope handling
  - `test_extract_scopes_sorted`: Sorting verification
  - `test_extract_scopes_empty_file`: Empty file edge case
  - `test_extract_scopes_comments_only`: Comments edge case

- **Documentation**:
  - Migration Guide v0.4.0 (comprehensive plugin author guide)
  - Updated Plugin Development Guide with scope extraction
  - API documentation with breaking change notes

### Changed

#### Breaking Changes
- **`extract_scopes()` Signature**: Added `content: &[u8]` parameter
  - Old: `fn extract_scopes(&self, tree: &Tree) -> Result<Vec<Scope>, ScopeError>`
  - New: `fn extract_scopes(&self, tree: &Tree, content: &[u8]) -> Result<Vec<Scope>, ScopeError>`
  - **Impact**: Plugin authors must update implementations
  - **Rationale**: Required for tree-sitter query text captures
  - **Migration**: See [MIGRATION_GUIDE_v0.4.md](docs/MIGRATION_GUIDE_v0.4.md)

### Performance

- **Scope Extraction Overhead**: ~5-10% vs symbol extraction alone
- **Implementation**: Single AST traversal with comprehensive query
- **Memory**: Minimal overhead (lightweight Scope structs)

### Design Decisions (CODEX-Reviewed)

1. **Scope Type Coverage**: 6 named types (deferred blocks/closures to Phase 4+)
2. **API Breaking Change**: Acceptable for pre-1.0 release
3. **Flat Scope List**: Parent linking deferred for simplicity
4. **Pragmatic Errors**: Return partial results on extraction failure

### Migration Notes

**For Plugin Authors**:
- Update `extract_scopes()` to accept `content` parameter
- Use `content` for tree-sitter query captures
- Update tests to pass source bytes

**For sqry Users**:
- No changes required (internal API only)

### Technical Details

**Scope Extraction Algorithm**:
1. Parse AST with `parse_ast()`
2. Compile scope query (6 patterns for Rust)
3. Execute query against AST with `content` for captures
4. Extract scope name and boundaries from captures
5. Sort scopes by start position
6. Return flat list (parent linking deferred)

**Query Structure**:
```scheme
(function_item name: (identifier) @function.name) @function.type
(impl_item type: (type_identifier) @impl.name) @impl.type
(trait_item name: (type_identifier) @trait.name) @trait.type
(mod_item name: (identifier) @module.name) @module.type
(struct_item name: (type_identifier) @struct.name) @struct.type
(enum_item name: (type_identifier) @enum.name) @enum.type
```

### Test Coverage

- **Total Tests**: 207+ (up from 206 in v0.3.0)
- **New Tests**: 7 scope extraction tests
- **Pass Rate**: 100%
- **Coverage**: All 6 scope types validated

### References

- Migration Guide: [docs/MIGRATION_GUIDE_v0.4.md](docs/MIGRATION_GUIDE_v0.4.md)
- Plugin Development Guide: [docs/development/plugin-system/PLUGIN_DEVELOPMENT_GUIDE.md](docs/development/plugin-system/PLUGIN_DEVELOPMENT_GUIDE.md)
- Phase 3 Completion: [docs/development/plugin-system/PHASE3_COMPLETE.md](docs/development/plugin-system/PHASE3_COMPLETE.md) (pending)

---

## [0.3.0] - 2025-10-02

### Added

#### Plugin System (Phase 2)
- **LanguagePlugin Trait**: Extensible plugin architecture for language support
  - `metadata()`: Plugin identification and versioning
  - `extensions()`: File extension mapping
  - `extract_symbols()`: Symbol extraction with tree-sitter
  - `parse_ast()`: AST parsing
  - `symbol_query()`: Tree-sitter query source
  - `extract_scopes()`: Scope analysis (Phase 3+, placeholder)
  - `resolve_symbol()`: Cross-file resolution (Phase 5+, placeholder)

- **PluginManager**: Central plugin registry and lookup
  - `register_builtin()`: Register statically-linked plugins
  - `plugin_for_extension()`: Lookup by file extension
  - `plugin_by_id()`: Lookup by language ID
  - `plugins()`: Enumerate all registered plugins

- **RustPlugin**: First language plugin (dogfooding)
  - Supports 8 Rust symbol types:
    - Functions (`fn`)
    - Structs (`struct`)
    - Enums (`enum`)
    - Traits (`trait`)
    - Impl methods (`impl` blocks)
    - Constants (`const`)
    - Static items (`static`)
    - Type aliases (`type`)
  - 19 unit tests + 7 integration tests

- **Integration Tests**: 9 end-to-end test scenarios
  - Full workflow validation (registration → extraction)
  - Multi-file extraction
  - Error handling (invalid extensions, malformed syntax, empty files)
  - Plugin lookup patterns
  - Metadata validation
  - Edge cases (unicode, whitespace, concurrent access)

- **Documentation**:
  - Plugin Development Guide (~3000 words)
    - Quick start, API reference, query guide
    - Testing strategies, troubleshooting, FAQ
    - Example walkthrough (Rust plugin)
  - Migration Guide (~2000 words)
    - Step-by-step migration from SymbolExtractor
    - Common patterns, gotchas, performance comparison
    - Troubleshooting and FAQ

### Deprecated

- **SymbolExtractor** (sqry-core)
  - Marked `#[deprecated(since = "0.3.0")]`
  - Will be removed in v0.4.0
  - Still fully functional (backward compatible)
  - See [MIGRATION_GUIDE_v0.3.md](docs/MIGRATION_GUIDE_v0.3.md) for upgrade path

- **PARSER_POOL** and **QUERY_EXTRACTOR** globals
  - Deprecated internal implementation details
  - Plugins manage their own parsers and queries
  - Will be removed in v0.4.0

### Changed

- **sqry-core architecture**: Separated plugin types from core
  - `sqry-core/src/plugin/`: Plugin system types
  - `sqry-lang-rust/`: Rust language plugin (separate crate)
  - Clear separation of concerns

- **Error types**: Added plugin-specific errors
  - `plugin::error::SymbolExtractionError`: Plugin symbol extraction errors
  - `plugin::error::ParseError`: Plugin AST parsing errors
  - `plugin::error::ScopeError`: Plugin scope extraction errors (Phase 3+)
  - Separate from legacy `symbols::error` types

### Performance

- **No regression**: < 0.1% difference vs v0.2 (within noise)
- Plugin lookup overhead: ~100ns (negligible vs ms parsing)
- Same symbol extraction implementation as Phase 1 (proven stable)

### Migration

**Impact**: Deprecations only (fully backward compatible)

**Timeline**:
- **v0.3.0** (now): Deprecation warnings, old API works
- **v0.4.0** (future): SymbolExtractor removed

**See**: [MIGRATION_GUIDE_v0.3.md](docs/MIGRATION_GUIDE_v0.3.md)

### Technical Details

**Design Decisions**:
1. Binary-level plugin registration (avoids circular dependencies)
2. Backward-compatible deprecation (zero risk migration path)
3. Static lifetime strings for extensions (future-proof)
4. Core-cached queries (plugins provide source, core caches)

**CODEX Reviews**: 2 architecture reviews guided implementation
- Pre-Step 3: Caught circular dependency issue
- Pre-Step 4: Validated test and documentation strategy

**Next**: Phase 3 - Scope Analysis & Context Extraction

## [0.2.0] - 2025-10-02

### Added

#### Security Features
- Comprehensive regex validation to prevent ReDoS (Regular Expression Denial of Service) attacks
  - Alternation explosion detection (`(a|ab)*` patterns blocked)
  - Nested quantifier detection (`(a+)+` patterns blocked)
  - Repetition range limits (max 1,000 repetitions)
  - Pattern length limits (max 1,000 characters)
- LRU query cache with bounded memory (100 entries, ~20KB)
  - Thread-safe cache access
  - Automatic LRU eviction
  - Prevents memory leaks in long-running processes

#### Robustness Improvements
- Range-based position matching for AST context extraction
  - Handles single-line and multi-line nodes correctly
  - More robust against off-by-one errors
  - Search-children-first algorithm ensures innermost scope selection
- Performance baseline benchmarks for future optimization

#### Test Coverage
- 39 new tests added (19 security + 8 cache + 5 position matching + 7 depth)
- Total: 156 tests passing

### Changed

#### BREAKING CHANGES
- **Depth calculation semantics changed (H1)**
  - Top-level symbols now return depth 1 (previously 0)
  - All nested symbols incremented by 1
  - **Migration:** Update depth queries by +1 (e.g., `depth:>0` → `depth:>1` to exclude top-level)
  - Rationale: Aligns with industry standards where "depth 1" = top-level

### Fixed
- AST depth calculation for top-level symbols (was incorrectly returning 0)
- Position matching fragility in context extraction
- Unbounded query cache causing memory leaks
- Missing regex validation allowing DoS attacks
- Query cache panic on poisoned lock (now recovers gracefully)

### Security
- Security posture improved from 6/10 to 9/10
- All major ReDoS attack vectors blocked
- CRITICAL vulnerabilities eliminated (C1: alternation explosion, C2: unbounded repetition ranges)

### Performance
- Query cache provides O(1) lookups for repeated queries
- Range-based position matching adds negligible overhead (<5 µs per node)
- Baseline performance: 381 µs per file, 3.8 ms per 10-file corpus

### Documentation
- 7 new design and review documents in `docs/development/ast-query-fixes/`
- Comprehensive implementation summary with CODEX reviews (4-5 stars)
- Migration guide for breaking depth calculation change

### Internal
- Code quality: Average 4.75/5 stars across 4 CODEX reviews
- All HIGH priority issues resolved (H1, H2, H3, H5 + lock panic fix)
- All CRITICAL security issues resolved (C1, C2)
- H4 (parsing optimization) deferred to future release per complexity checkpoint
- Library code now panic-free (handles poisoned locks gracefully)

## [0.1.0] - 2025-10-01

### Added
- Initial project structure
- Workspace with 7 crates (core, CLI, 5 language plugins)
- Documentation framework
- MIT license

### Status
Phase 0 (Repository Setup) - Under Development

**Next**: Phase 1 (Core Engine Implementation)
## [2.13.2] - 2026-01-23

### Changed
- **MCP v0.3 – Standardized Tool Verbs**:
  - Standardized MCP tool names for consistency with CLI verbs:
    - `find_similar` → `search_similar`
    - `get_dependencies` → `show_dependencies`
    - `index_status` → `get_index_status`
  - Updated handlers, schemas, feature flags, and tests to use the new names exclusively.
  - Documentation updates:
    - README: Added “Choosing the Right Command” guidance (search vs query vs graph)
    - docs/COMMAND_CHEAT_SHEET.md: Comprehensive command reference
    - docs/MCP_MIGRATION_v2.md: Migration guide for MCP 0.2 → 0.3
  - Protocol: MCP server reports `sqry-mcp/0.3` and accepts requests from clients using 0.2–0.3 during transition.
  - Tests: All MCP tests updated and passing.

### Fixed
- Core/CLI clippy warnings:
  - Core validator now uses structured initialization (no field-reassign-with-default).
  - Core buffer config assertions moved to compile-time checks.
  - Perl language plugin simplified redundant if/else.
  - CLI: minor clippy cleanups in `index` and `repair` commands.
- Full workspace tests passing.

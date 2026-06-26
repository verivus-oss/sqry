# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [20.0.10](https://github.com/verivus-oss/sqry/compare/v20.0.5...v20.0.10) - 2026-06-14

### Changed
- *(graph)* remove dead ExportMap / pass4_cross plumbing ([#418](https://github.com/verivus-oss/sqry/pull/418))## [20.0.2](https://github.com/verivus-oss/sqry/compare/v20.0.1...v20.0.2) - 2026-06-12

### Fixed
- *(release)* repair public docs and macos daemon build## [20.0.0](https://github.com/verivus-oss/sqry/compare/v19.0.9...v20.0.0) - 2026-06-12

### Fixed
- land actual product work before marketplace packaging
## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))
### Fixed
- *(daemon)* preserve durable graph artifacts- *(release)* allowlist v16.0.7 baseline drift## [19.0.0](https://github.com/verivus-oss/sqry/compare/v18.0.11...v19.0.0) - 2026-06-03

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking## [18.0.0](https://github.com/verivus-oss/sqry/compare/v17.0.1...v18.0.0) - 2026-06-01

### Added
- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))## [17.0.0](https://github.com/verivus-oss/sqry/compare/v16.0.8...v17.0.0) - 2026-05-31

### Other
- update Cargo.toml dependencies
## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Added
- *(db)* V12 schema + framework/resolved_via filter predicates ([#323](https://github.com/verivus-oss/sqry/pull/323))
### Fixed
- *(daemon)* preserve durable graph artifacts
### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [16.0.7](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.7) - 2026-05-27

### Fixed
- *(graph)* preserve cross-language HttpRequest edges across incremental rebuild ([#313](https://github.com/verivus-oss/sqry/pull/313))
### Performance
- *(c-icall-precision)* speed up Pass 5b kernel indexing ([#289](https://github.com/verivus-oss/sqry/pull/289))## [16.0.3](https://github.com/verivus-oss/sqry/compare/v16.0.2...v16.0.3) - 2026-05-20

### Performance
- *(c-icall-precision)* speed up Pass 5b kernel indexing ([#289](https://github.com/verivus-oss/sqry/pull/289))## [16.0.0](https://github.com/verivus-oss/sqry/compare/v15.0.8...v16.0.0) - 2026-05-19

### Added
- *(c-icall-precision)* land Phase A## [15.0.8](https://github.com/verivus-oss/sqry/compare/v15.0.6...v15.0.8) - 2026-05-17

### Added
- *(go)* Go implicit implements + promoted methods + function-signature implementations (T1) ([#277](https://github.com/verivus-oss/sqry/pull/277))## [15.0.3](https://github.com/verivus-oss/sqry/compare/v15.0.2...v15.0.3) - 2026-05-13

### Other
- update Cargo.toml dependencies
## [15.0.2](https://github.com/verivus-oss/sqry/compare/v15.0.1...v15.0.2) - 2026-05-13

### Other
- *(deps)* batch update 2026-05-12-2311## [15.0.1](https://github.com/verivus-oss/sqry/compare/v15.0.0...v15.0.1) - 2026-05-11

### Other
- update Cargo.toml dependencies
## [15.0.0](https://github.com/verivus-oss/sqry/compare/v14.0.4...v15.0.0) - 2026-05-11
## [14.0.4](https://github.com/verivus-oss/sqry/compare/v14.0.3...v14.0.4) - 2026-05-10

### Fixed
- *(core)* stabilize cancellation latency regression## [14.0.3](https://github.com/verivus-oss/sqry/compare/v14.0.2...v14.0.3) - 2026-05-10

### Other
- update Cargo.toml dependencies
## [14.0.2](https://github.com/verivus-oss/sqry/compare/v13.0.17...v14.0.2) - 2026-05-10

### Fixed
- *(mcp)* sqry-mcp large-repo stability## [14.0.0](https://github.com/verivus-oss/sqry/compare/v13.0.17...v14.0.0) - 2026-05-09

### Fixed
- bugs from no-skip kernel test pass (#216, #213, #214, #215) ([#228](https://github.com/verivus-oss/sqry/pull/228))
## [13.0.17](https://github.com/verivus-oss/sqry/compare/v13.0.16...v13.0.17) - 2026-05-09

### Other
- update Cargo.toml dependencies
## [13.0.16](https://github.com/verivus-oss/sqry/compare/v13.0.14...v13.0.16) - 2026-05-09

### Other
- *(deps)* apply approved dependency upgrade batch## [13.0.11](https://github.com/verivus-oss/sqry/compare/v13.0.10...v13.0.11) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.10](https://github.com/verivus-oss/sqry/compare/v13.0.9...v13.0.10) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.9](https://github.com/verivus-oss/sqry/compare/v13.0.7...v13.0.9) - 2026-05-08

### Added
- *(graph)* share graph acquisition across clients## [13.0.6](https://github.com/verivus-oss/sqry/compare/v13.0.5...v13.0.6) - 2026-05-07

### Fixed
- *(daemon)* persist validated derived cache entries## [13.0.2](https://github.com/verivus-oss/sqry/compare/v13.0.1...v13.0.2) - 2026-05-06

### Fixed
- *(unused)* apply binding-plane boundary filter## [12.1.6](https://github.com/verivus-oss/sqry/compare/v12.1.2...v12.1.6) - 2026-05-04

### Fixed
- *(index)* stabilize large high-cost graph rebuilds## [12.1.0](https://github.com/verivus-oss/sqry/compare/v12.0.3...v12.1.0) - 2026-05-03

### Added
- cross-language field and generic type-parameter emission ([#169](https://github.com/verivus-oss/sqry/pull/169))
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [11.0.0](https://github.com/verivus-oss/sqry/compare/v10.0.4...v11.0.0) - 2026-04-30

### Documentation
- *(public-issue-triage)* add layer3 b1 codex iter3 review## [10.0.0](https://github.com/verivus-oss/sqry/compare/v9.0.23...v10.0.0) - 2026-04-27

### Added
- workspace-aware / cross-repo indexing (DAG 2026-04-26) ([#146](https://github.com/verivus-oss/sqry/pull/146))
## [9.0.20](https://github.com/verivus-oss/sqry/compare/v9.0.19...v9.0.20) - 2026-04-26

### Fixed
- *(vscode)* stabilize lsp indexing## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

### Added
- *(graph)* line-zero holistic fix — Chunk 1 (HU01-HU07, Phase 4c-prime)## [8.0.1](https://github.com/verivus-oss/sqry/compare/v8.0.0...v8.0.1) - 2026-04-11

### Fixed
- *(release)* stabilize snapshot limits and sanitization gates## [8.0.0](https://github.com/verivus-oss/sqry/compare/v7.2.0...v8.0.0) - 2026-04-10

### Added
- *(graph)* land curated provenance and staged-release updates- *(core)* add GraphMemorySize trait for heap memory tracking
### Fixed
- *(core)* count inner heap for DeltaEdge spans, node metadata, confidence, Arc headers- *(classpath)* integrate pre-pass graph enrichment## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.2.0) - 2026-04-06

### Added
- *(graph)* add sqry-bind facade with SymbolClassification and BindingQuery- *(graph)* add path enumeration mode with SCC pruning strategy to BFS kernel
### Fixed
- *(graph)* enforce node/edge limits atomically and add leaf enumeration in path BFS- *(graph)* use discovery-order vector in path enumeration BFS per spec invariant
### Other
- bump version to 7.2.0
- *(clippy)* resolve pedantic lints after BFS kernel migration- *(core)* fix collapsible-if clippy lints in kernel.rs## [7.1.5](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.1.5) - 2026-04-06

### Added
- *(core)* add shared materialize module for node materialization and seed lookup- *(graph)* add shared graph helpers and witness-bearing resolution
### Documentation
- *(graph)* formalize witness seam as baseline API and update progress
### Plan
- add DAG TOML implementation plan for MCP resource-backed skills
## [7.1.0](https://github.com/verivus-oss/sqry/compare/v6.0.23...v7.1.0) - 2026-04-04

### Added
- *(cli)* add plugin cost tiering and manifest-backed selection- *(classpath)* add JVM classpath analysis (Track C Tier 1)
### Documentation
- *(rust)* add macro and proc-macro boundaries design spec
### Fixed
- *(cpp)* bound pathological graph builds- *(release)* add VERSION stamps to all public directories for consistent release metadata- *(release)* harden public sanitization outputs- *(release)* resolve hermetic clippy bench warning- *(release)* unblock hermetic sanitized-tree validation- *(security)* harden MCP server against red team findings- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers
### Other
- sync versions and VERSION stamps to 6.0.18
- *(rustfmt)* normalize code for Rust 1.90 toolchain- apply rustfmt to fix formatting in sanitized tree build
## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03

### Added
- *(classpath)* add JVM classpath analysis (Track C Tier 1)## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [6.0.15](https://github.com/verivus-oss/sqry/compare/v6.0.12...v6.0.15) - 2026-04-02

### Fixed
- *(release)* harden public sanitization outputs## [6.0.2](https://github.com/verivus-oss/sqry/compare/v5.0.1...v6.0.2) - 2026-03-31

### Fixed
- *(release)* resolve hermetic clippy bench warning- *(release)* unblock hermetic sanitized-tree validation
### Other
- *(rustfmt)* normalize code for Rust 1.90 toolchain## [6.0.0](https://github.com/verivus-oss/sqry/compare/v5.0.1...v6.0.0) - 2026-03-31

### Fixed
- *(security)* harden MCP server against red team findings## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(json)* make MAX_DEPTH and MAX_NODES configurable via SQRY_JSON_MAX_DEPTH/SQRY_JSON_MAX_NODES env vars- *(json)* add sqry-lang-json plugin for declarative JSON config files- *(visualization)* add filter_node_ids to DotConfig- *(visualization)* add filter_node_ids to D2Config- *(visualization)* add filter_node_ids to MermaidConfig- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(resolution)* add file-aware symbol resolution- *(uses)* implement share feature (FR-2025-023-share)- *(mcp,core,nl)* resolve 5 disconnected functionality issues, bump to 4.7.0- *(graph)* implement parallel commit pipeline (phases 2-4) and wire into entrypoint- *(graph)* configurable label budget with graceful degradation- *(graph)* add exhaustive StringId remap for parallel commit- *(graph)* add CommitPlan and prefix-sum range assignment- *(graph)* add BidirectionalEdgeStore::add_edges_bulk_ordered- *(graph)* add StagingGraph u32 count accessors for range assignment- *(graph)* deterministic StringInterner serialization via sorted serde- *(graph)* add FileRegistry::register_batch for parallel commit- *(graph)* add StringInterner bulk APIs for parallel commit- *(graph)* switch AuxiliaryIndices to BTreeMap + add build_from_arena- *(graph)* add NodeArena bulk APIs for parallel commit- *(graph)* parallelize file processing with rayon thread pool- *(graph)* add thread_count field to BuildResult for diagnostics- *(graph)* add StagingGraph::estimated_byte_size() for memory instrumentation- *(graph)* add Pass 5 global cross-language edge detection- *(servicenow)* add Import edge detection for ES6 imports and require() calls- *(persistence)* [**breaking**] migrate bincode → postcard for all serialization- *(core)* add macOS and Windows network filesystem detection- *(core,cli)* add advanced query features — variables, subqueries, joins, aggregations- *(core)* add SIMD ASCII fast-path for trigram extraction- *(core)* consolidate build pipeline into single entry point- *(core)* extract shared ScopeTree infrastructure for local variable tracking- *(core,mcp,lsp)* add auto-indexing when graph snapshot is missing- *(core)* add is_unsafe metadata field to NodeEntry- *(persistence)* add plugin version tracking to graph indexes- *(fixtures)* restructure Java fixtures to match package hierarchy- *(analysis)* add 2-hop labels and persistence (Pass 5 Tasks 5-6)- *(analysis)* implement Pass 5 analysis module foundation (CSR + SCC + Condensation)- *(cli)* implement error collection and workflow trace generation- *(go)* [**breaking**] add TypeOf/Reference edges for function/method parameters and returns (Phase 2)- *(security)* harden recursion limits and align with configuration strategy- *(mcp)* improve workspace path resolution in MCP tools- *(core)* implement RecursionGuard and ExprFuelCounter- *(core)* add RecursionLimits configuration module- *(mcp)* implement config-driven workspace discovery- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(query)* add returns predicate and fix Lua qualified name lookup- *(query)* implement async:, static:, and visibility: predicates- *(visibility)* implement visibility metadata for Rust symbols- *(update)* enable git-aware update tests with commit tracking- *(exports)* enable Export edge support across all languages- parallel agent execution - multi-stream improvements
- *(lsp)* enable cross-file call hierarchy for Rust plugin- *(lsp)* add sqry/semanticDiff custom method for git diff comparison- *(diagram)* migrate to graph-native types (FR-2026-001)- *(core)* add QueryResults type and graph build enhancements- *(project)* add graph() method and graph_cache to Project and SessionManager- *(schema)* add canonical schema module with unified data dictionary- *(query)* enable path: predicate with workspace root in execute_with_index- *(query)* add workspace_root for relative path glob matching- *(query)* implement scope and references predicates using unified graph- *(mcp)* add 4 new graph-based MCP tools with validation- *(cli,mcp)* add analysis commands and tools- *(index)* add ingest progress counts and ETA- *(cli)* add per-step progress output for indexing- *(lang-rust)* complete P3 feature wiring with codex sign-off- *(lang-rust)* implement P3 features for Rust plugin- *(cli)* add progress feedback during unified graph build phase- *(cli)* add automatic index ancestor discovery- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang-python)* enhance parameter_types with JSON storage and review fixes- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(test)* add Wave 0.3 test harness for GraphBuilder assertions- *(graph)* Phase 5C Svelte/Vue script-level call edges- *(unified-graph)* add staging-local StringId remap and helper metadata- *(query)* complete CD predicate integration (circular, duplicates, unused)- *(lsp)* add span-based call site tracking for precise call hierarchy- *(relations)* implement staging graph relation extraction- *(mcp)* migrate MCP tools to unified graph architecture- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(unified-graph)* add query APIs to GraphSnapshot and language tracking to FileRegistry- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(config)* implement unified graph config partition- *(unified-graph)* implement Workstream D CLI migration to save graph snapshots- *(unified-graph)* implement FR-2025-007 unified graph migration- *(rust)* implement complete FR-RUST relation tracking- *(uses)* implement local uses and insights system (FR-2025-023)- *(vscode)* add CD predicate tree views for code discovery- *(cd)* add circular, duplicates, and unused predicates- *(cd)* implement impl: predicate using graph-based edges- *(plugin)* add SafeParser error types and update plugins- *(fuzz)* add parallel fuzzing infrastructure and OOM protection- *(parser)* complete unified parser migration (FR-2025-015)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(indexing)* enable GraphBuilder as production default- *(persistence)* implement project state persistence- *(output)* integrate preview extraction into text and JSON formatters- *(paging)* complete P2-29 paging implementation with command integration- *(query)* add Query Builder API with lookaround regex support (P2-10)- *(prewarm)* complete P2-8 Step 6 edge cases & hardening- *(prewarm)* complete P2-8 Steps 3-5 (CLI, Metrics, Benchmarks)- *(session)* implement warm image persistence (P2-8 Step 2)- *(session)* add prewarm module scaffolding (P2-8 Step 1)- *(graph)* implement 4-pass build pipeline (M8)- *(graph)* implement unified graph architecture M1-M7- *(project)* implement PROJECT_ROOT_SPEC with multi-project workspace support- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(graphbuilders)* implement Phase 7 GraphBuilder for 7 domain-specific plugins- add slopscan tooling and audit docs
- *(vscode)* hierarchical tree grouping for semantic search results- *(lsp)* expose cross_language_relation_count in IndexStatus- *(core)* enhance error handling and type safety based on Rust best practices review- *(rr-15)* integrate indexed evaluation with QueryExecutor (Week 1 Day 4)- *(perf)* add index-based query evaluation scaffolding (RR-15 Week 1 Day 3)- *(perf)* thread-safe index caching with path tracking (RR-15 Week 1 Day 2)- *(perf)* implement Arc-based index loading (RR-15 Week 1 Day 1)- *(rr-09)* add tree-sitter validation and test support infrastructure- *(rr-13)* add panic documentation and must_use attributes to public API- *(rr-10)* implement DoS prevention limits with configurable thresholds- *(fuzz)* complete RR-11 defense-in-depth fuzzing for all 34 language plugins- *(fuzz)* add nightly CI workflow and complete P2-37 Phase 1 Fix #4- *(fuzz)* add seed corpus generation script- *(core)* enhance plugin manager and import resolver- *(hybrid)* wire --force-semantic CLI flag with Approach B (default hybrid when embeddings enabled)- *(hybrid)* extend HybridQueryExecutor to implement full Stage 3 design (7-component scoring + RRF + graph confidence)- *(query)* wire HybridQueryExecutor into QueryExecutor- *(hybrid)* implement real symbol resolution against AST/graph- *(query)* add HybridQueryExecutor foundation for Stage 3 AST/graph fusion- *(query)* implement SemanticQueryExecutor with hierarchical two-stage retrieval- *(vector)* add ANN search with IVF-PQ indexing- *(vector)* complete Lance 0.39 API migration- *(vector)* implement Lance-based vector storage for embeddings- *(embeddings)* implement hierarchical dual-model inference engine- *(embeddings)* add Phase 0 scaffolding for FR-2026-001 hybrid embeddings- *(cache)* integrate TinyLFU into auxiliary caches- *(cache)* implement TinyLFU eviction policy with telemetry- *(P2-7)* parallel query execution with validated 2× speedup- *(P2-5)* SIMD-accelerated text search (1.3-1.8x faster)- *(P2-3)* complete Phase 3 Zero-Warnings Initiative- *(P2-3)* add ResolvedSymbolView for zero-copy symbol access (Step 2a-2b)- *(P2-3)* Rust plugin reference implementation with PluginSymbolBuilder- *(P2-3)* add PluginSymbolBuilder infrastructure for warning-free plugin API- *(P2-3b)* complete full lazy index loading with V3 format- *(P2-33b)* add graph_mode flag to skip redundant ImportResolver- *(P2-33)* complete Phase 1 - cross-file symbol resolution- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(index)* add partial lazy loading optimization (P2-3)- *(query)* add regex compilation caching (P2-1)- *(index)* use mmap for large symbol indexes (P2-4)- *(config)* add configurable git output and mmap limits (P1-17)- *(P1-12)* add compression performance gates with CI enforcement- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(graph)* add parallel graph mode with opt-in configuration- *(lang-c)* Implement full relation tracking with CLI tests (Tier 1)- *(swift)* Implement full relation tracking support- *(cli,core,lua)* Complete Step 2 - 17-language CLI test matrix- *(cli,core)* Enhance CLI commands and core search functionality- *(php)* Complete PHP Tier 2 relation tracking with comprehensive tests- *(ruby)* Add full relation tracking (calls, exports, imports)- *(watch)* Implement platform-aware defaults and improved event handling- *(P1-11)* Add Phase 3 integration tests for git-aware updates- *(P1-14)* Add validation to JSON output for index status- *(P1-14)* Integrate validator into IndexStorage load path- *(P1-14)* Implement checksum computation and validation- *(P1-14)* Add index validation infrastructure- *(cli)* implement rich query diagnostics with miette integration (P1-8)- merge FR-2025-016 and FR-2025-017 feature branches to master
- *(core)* complete Step 3 - session and workspace AST alignment (FR-2025-015)- *(core)* complete Step 2 - AST execution and comprehensive tests (FR-2025-015)- *(core)* add ParsedQuery foundation and fix critical security bugs (FR-2025-015 Step 2 partial)- *(core)* add field aliases and repo/parent predicates for AST (FR-2025-015 Step 1)- *(core)* add legacy parser usage tracking for FR-2025-015 Step 0- *(core,cli)* implement lock coordination for index builds (Phase 1)- consolidate multiple feature implementations and documentation updates
- *(cli)* extend graph filters (languages + edge kinds); register Lua in CLI loader\n\n- Adds new edge filters: table_read, table_write, triggered_by, channel_invoke, widget_child\n- Extends language filters to include sql, dart, lua, perl, shell, groovy, etc.\n- Registers LuaGraphBuilder in CLI loader\n\nDocs:\n- Update README examples for cross-language filters\n- Sync Phase 3/5A test execution docs with CLI alignment\n- Add Phase 4 04_PROGRESS.md and CODEX review placeholders for phases 2/4/5A\n\nSemver: minor bump to 1.18.0- *(sql,dart)* implement Phase 3 GraphBuilder with critical bug fixes- *(lang)* complete FR-2025-009 Phase 2 critical fixes for Ruby/PHP/Swift- *(viz)* add Tier-2 language support to graph visualizations- *(csharp)* complete Phase 5 C# GraphBuilder with full P/Invoke support- *(c)* add CGraphBuilder for relation tracking- *(go)* add GoGraphBuilder with method receiver support- *(graph)* implement Java GraphBuilder for unified CodeGraph architecture (Phase 2 - partial)- *(graph)* implement Rust GraphBuilder for unified CodeGraph architecture- *(mcp)* implement semantic_diff tool with git worktree support- *(FR-2025-006-phase4)* complete Step 9 - documentation & polish ✅ PHASE 4 COMPLETE- *(FR-2025-006-phase4)* implement Step 8 - add tree_cache_capacity config option- *(FR-2025-006-phase4)* implement Step 6 - WatchModeIndexer integration- *(FR-2025-006-phase4)* implement Step 4 - export public API- *(FR-2025-006-phase4)* implement Step 3 - IncrementalParser wrapper- *(FR-2025-006-phase4)* implement Step 2 - TreeCache (LRU)- *(FR-2025-006-phase4)* implement Step 1 - InputEdit calculator- *(indexing)* implement Phase 3 Step 5 - Watch mode content diff integration + bugfix- *(indexing)* implement Phase 3 Step 4 - IndexBuilder content diff integration- *(indexing)* implement Phase 3 Step 3 - HashIndex content caching- *(indexing)* implement Phase 3 content diffing - Step 1 & 2- *(graph)* complete Phase 6 - unified graph architecture rollout- *(visualization)* implement Phase 5.1-5.2 graph export formats- *(graph)* implement Phase 5.3 advanced graph queries- *(graph)* implement Phase 4 cross-language edge detection- *(graph)* implement Phase 1-3 unified graph core architecture- *(graph)* implement Phase 1.3 graph indices for fast queries- *(graph)* implement Phase 1.2 CodeGraph structure with DashMap- *(core)* Add Phase 1.1 core graph types (NodeId, CodeNode, CodeEdge)- *(cpp)* add CallContext cache infrastructure- *(workspace)* add detailed stats with staleness tracking and health monitoring- *(workspace)* implement WorkspaceIndex with SessionManager integration- *(workspace)* add multi-repo workspace registry foundation- *(cli)* implement sqry shell and sqry batch commands (Week 2)- *(query)* add thread-local lexer pooling for allocation efficiency- *(query)* integrate reusable lexer pool with parser- *(lexer)* add token buffer reuse optimization (Day 4)- *(phase2)* finalize Day 3 - Moka production ready- *(benchmark)* add Moka vs RwLock comparative analysis - MOKA WINS- *(benchmark)* add proper load benchmark with valid results- *(phase2)* add throughput-focused benchmark for Moka validation- *(phase2)* migrate ParseCache to Moka for lock-free concurrency- *(phase2)* complete Day 2 Arc cache benchmarking with exceptional results- *(phase2)* implement Arc-based cache for zero-copy hits- *(profiling)* add Phase 2 query pipeline profiling and analysis- *(phase1)* complete classifier optimization with validation- *(core)* optimize SymbolIndex file removal for incremental updates- *(fr-2025-004)* ship MCP observability + perf harness- *(mcp)* complete FR-2025-004 Phase 1 - Production-Ready MCP Server- *(cli)* add watch mode command for real-time index updates- *(indexing)* implement watch mode Phase 2 (real-time file monitoring)- *(indexing)* implement incremental indexing Phase 1 (hash-based change detection)- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata- *(core)* add lookahead/lookbehind support to regex validator (FT-C.1)- *(core)* add cross-language metadata consistency tests (FT-B.6)- *(core)* implement FT-B.5 query field name normalization- *(core)* add metadata normalizer for backward compatibility (FT-B.3)- *(core)* add shared metadata constants module (FT-B.2)- *(core)* add .sqryignore support for excluding files from indexing- *(tests)* add shared test infrastructure and improve benchmarks- *(cli)* add comprehensive boolean query regression tests and relation query improvements- *(core)* add relation matching utilities and FQN support for RelationStore- *(search)* implement result ranking and relevance scoring- *(benches)* add hybrid search benchmark foundation (WIP)- *(hybrid)* add JSON output support for text and combined search results- *(registry)* add debug logging for plugin field collisions- *(hybrid)* enable metadata field queries in hybrid search mode- *(query)* migrate hybrid search to new boolean query parser (Day 1/7)- *(search)* implement hybrid search engine with automatic fallback- *(search)* implement query classification for hybrid search mode- *(test)* complete verbose logging instrumentation for symbols/ comprehensive tests- *(test)* complete boilerplate-ready files instrumentation (Sprint 5)- *(test)* achieve 60% verbose logging adoption milestone (30/49 files)- *(test)* expand verbose logging adoption to 24.5% with CODEX A+ grade- *(test)* adopt verbose logging in core tests and add query execution logging- *(test)* implement test harness verbose logging system- *(python)* add decorator extraction with cache metadata preservation- *(cache)* implement cache prune command- *(go)* extract individual const/var names from declarations- *(query)* integrate AST cache into QueryExecutor- *(cache)* implement disk-persisted AST cache system- *(search)* [**breaking**] release v0.17.0 - Jaccard similarity for fuzzy search- *(search)* fuzzy-search accelerators (v0.16.0)- *(core)* RT-S0 shared foundation for multi-language relation tracking- *(sprint2)* add cross-file semantic analysis with relation tracking- *(sprint2)* partial Step 4 - add relation fields to query registry- *(sprint2)* complete Step 3 - relation query language support- *(sprint2)* complete Step 2 - relation extraction in IndexBuilder- *(sprint2)* implement relation extraction infrastructure (Steps 0-2 partial)- *(v0.12.1)* complete Query Language Hardening with review fixes- *(index)* add parallel indexing with rayon- *(index)* add progress reporting for indexing operations- complete Phase 7 - Query Language Enhancements (Task 10: Integration Tests)
- *(query)* add index-aware query execution for instant results- *(index)* add IndexStorage with atomic save/load- *(index)* add IndexBuilder for creating and updating indexes- *(index)* add persistence and change detection to SymbolIndex- *(query)* implement AST-aware query command- *(scope)* implement scope extraction for Phase 3- *(plugin)* define LanguagePlugin trait and PluginManager- *(ast)* [**breaking**] implement AST query fixes and security improvements for v0.2.0- *(ast)* implement Step 8 - documentation and final polish- *(ast)* implement Step 7 - comprehensive integration tests- *(ast)* implement Step 5 - query parser with comprehensive tests- *(ast)* implement Steps 3 & 4 - context extraction with tests- *(ast)* implement Step 2 - context types- *(ast)* implement Step 1 - module structure and error types- *(symbols)* add symbol extraction implementation- *(symbols)* add error types and port tree-sitter queries- *(symbols)* add core symbol types and simple index- *(search)* port core search engine from crgrep- initial sqry repository setup (Phase 0)

### Changed
- *(resolution)* remove legacy snapshot lookup entry points- *(java,kotlin)* migrate local_scopes to shared ScopeTree from sqry-core- *(core)* centralize analysis path logic in GraphStorage- *(core,cli,lsp,mcp)* reduce cognitive complexity for S3776 compliance- *(core)* complete SymbolIndex removal (~10,000 LOC)- *(core)* remove IndexBuilder infrastructure (~4,850 LOC)- *(core)* remove legacy SymbolIndex components (~6,500 LOC)- *(diagram)* remove legacy Symbol-based APIs- *(bench)* migrate benchmarks to unified CodeGraph API- *(core)* migrate QueryExecutor to CodeGraph-native implementation- migrate CLI commands from SymbolIndex to unified graph
- remove Kroki integration for diagram rendering
- *(sonarqube)* critical cleanup batch- *(plugins)* share metadata application helper- *(plugins)* centralize query extraction- *(simd)* deduplicate search helpers- *(sonarqube)* complete critical cleanup and lint passes- *(indices)* extract removal helpers- *(edge-store)* extract LWW merge helpers- *(search)* extract ranking scoring helpers- *(admission)* extract CAS reservation helpers- *(core)* add QueryExecutor index override- reduce cognitive complexity in analysis.rs and fix clippy warnings
- reduce cognitive complexity in Sprint 7 (slopscan, ast, cycle_detector)
- reduce cognitive complexity in high-priority functions
- *(graph)* migrate GraphBuilder trait to use GraphSnapshot- *(FR-2025-021)* complete WP6 - adapter cleanup and dual path removal- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(plugin)* deprecate legacy relation extraction methods (FR-2025-021 WP3)- *(symbols)* modularize index.rs into focused submodules (Phase 3)- *(core)* remove hybrid embeddings path and align docs- *(core)* split core.rs into processing.rs and relations.rs- *(core)* modularize builder.rs into directory module- *(mcp)* extract tools/ directory from execution/mod.rs- *(mcp)* begin execution module modularization (Phase 1 partial)- *(rr-15)* implement Day 4 fixes for indexed evaluation- *(perf)* add cache invalidation logging and concurrency tests- *(embeddings)* update config for all-MiniLM-L6-v2- fix 73 clippy warnings across language modules
- Add reasons to all #[ignore] test attributes
- Apply clippy pedantic auto-fixes
- *(graph)* Rename cross_lang to cross_language for consistency- *(cache)* Rename user_hash to user_namespace_id for semantic clarity- *(symbols)* Rename temp path variables to semantic names in atomic writes- *(cache)* Rename temp_path to tmp_cache_file_path in atomic write- *(sqry-core)* rename truncated → is_truncated in struct fields- *(naming-p1)* Rename temp_dir → tmp_*_dir in sqry-core tests- *(phase2)* fix float comparisons with approx crate - 16 warnings fixed- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(executor)* finalize modular structure- *(executor)* extract tests to dedicated module- *(executor)* extract predicate evaluation to dedicated module- *(executor)* extract directory scanning to dedicated module- *(executor)* extract index operations to dedicated module- *(executor)* extract cache integration to dedicated module- *(executor)* extract set operations to dedicated module- *(executor)* extract tests to dedicated module- *(executor)* extract predicate evaluation to dedicated module- *(executor)* extract directory scanning to dedicated module- *(executor)* extract index operations to dedicated module- *(executor)* extract cache integration to dedicated module- *(executor)* extract set operations to dedicated module- *(graph)* rename modules to follow naming guidelines- *(core)* apply clippy fixes for Edition 2024 and performance- *(symbols)* [**breaking**] remove content diffing layer - simplify to hash-based detection only- *(cli)* Replace atty with IsTerminal to fix audit warnings- *(query)* [**breaking**] remove unnecessary consecutive quantifier validation- *(tests)* migrate go_comprehensive.rs to PluginManager- *(tests)* migrate python_comprehensive.rs to PluginManager- *(tests)* migrate typescript_comprehensive.rs to PluginManager- *(tests)* migrate javascript_comprehensive.rs to PluginManager- *(tests)* migrate rust_comprehensive.rs to PluginManager- *(tests)* migrate edge_cases.rs to PluginManager- *(core)* mark SymbolExtractor as deprecated (Step 3)
### Documentation
- align OSS documentation with actual implemented functionality
- update capabilities matrix and fix stale FfiCall doc comments
- *(core,cli)* add structured STUB markers to would_orphan_edges and get_symbols_at_ref- *(analysis)* remove unused persistence header structs- *(P2-26)* fix all rustdoc warnings across workspace- *(review)* add LOW priority post-implementation review responses- *(builder)* address Phase 2.5 review recommendations- *(p2-6)* mark implementation complete and add RKG annotations- *(P2-2)* add comprehensive Symbol interning documentation- *(P2-33)* add debug tests and review documentation- *(P2-34)* Phase 2 complete - CLI tests + scope.parent bug fix- *(rustdoc)* Fix all 14 intra-doc link warnings- add testing documentation, issue tracking, and plugin macro infrastructure
- *(release)* finalize FR-2025-008 v1.18.0 production readiness- *(FR-2025-006-phase4)* add acceptance tests, benchmarks, and complete documentation- *(core)* Fix review findings - doc example, progress tracking, missing docs- *(workspace)* add missing field documentation for SymbolWithRepo and WorkspaceStats- *(phase2)* add comprehensive Arc cache API documentation- *(hybrid)* implement Codex MEDIUM priority recommendations- comprehensive benchmark suite and competitive analysis
- *(search)* add exclude pattern gitignore syntax clarification
### Fixed
- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers- *(release)* remove 408 files tracked in git but matched by .gitignore- *(graph)* per-block body hashes for Vue/Svelte, fast pre-checks for JSON/HTML- *(test)* relax xxhash64 performance threshold for CI runners- *(core)* exclude collected_at from config snapshot content hash- *(graph)* compact edge stores to CSR before persistence- *(core)* use normalizing register for fake test paths (Windows compat)- *(core)* use platform-aware test root in resolution tests- *(cli,core)* strengthen weak test assertions identified in review- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(release)* resolve preflight native-name regressions- *(native-display)* preserve native names and synthetic ffi ids- *(resolution)* enforce canonical graph names and complete migration- *(release)* align sanitization and linux fs checks- *(analysis)* use iterator instead of needless_range_loop in bitset test- *(graph)* restore per-phase progress reporting and fix formatting- *(analysis)* update stale test comment after removing serde default- *(graph)* remove false backward-compat claim and add SQRY_DENSITY_GATE_THRESHOLD env var- *(graph)* use checked_mul for density gate and preserve precedence on invalid config- *(graph)* wire density gate into build_with_budget and handle budget=0 as unlimited- *(graph)* split CSR-only indexing from full analysis and harden analysis pipeline- *(ci)* resolve all 5 failing CI jobs- *(graph)* add lookup_stale invariant guard to StringInterner- *(graph)* upgrade staging validation to runtime asserts (iter5 review)- *(graph)* address iter4 review - staging validation + edge count checks- *(graph)* make Phase 3 count mismatch a hard error (iter3 review)- *(graph)* address iter2 review findings for parallel commit pipeline- *(graph)* address Codex review findings for parallel commit pipeline- *(graph)* fix commit failure accounting and clean internal references- *(graph)* complete parallel indexing quality gaps per AGENTS.md- *(deps)* complete tree-sitter 0.25→0.26 migration- *(deps)* address criterion black_box deprecation and add execute validation tests- *(search)* remove dead to_search_mode and fix misleading error message- *(graph)* address Codex review findings for Pass 5 cross-language detection- *(ci)* stabilize flaky tests across macOS and Windows CI- *(ci)* handle Windows backslash separators in path splitting- *(ci)* canonicalize workspace folders and lower coverage threshold- *(core)* fix 4 Windows test failures in path handling and file locking- *(ci)* relax plugin loading benchmark thresholds and skip auto_rebuild on Windows- *(ci)* fix Windows dead_code and macOS repo_id_fidelity failures- *(ci)* skip flaky file-watcher tests on macOS and Windows stack overflow- *(ci)* gate Instant import behind cfg(linux) in multiprocess tests- *(ci)* resolve remaining platform-specific CI failures- *(simd)* add unsafe blocks for non-intrinsic unsafe calls in NEON code- *(ci)* resolve all CI failures across platforms- *(r,core)* emit FfiCall edges for R FFI calls and include FfiCall in reference queries- *(persistence)* address Codex post-implementation review findings- *(core)* surface subquery cache misses as errors and add missing tests- *(core,cli)* address iter2 codex review findings for D3 query features- *(core,cli)* address codex review findings for D3 advanced query features- *(core,mcp)* increase label budget to 5M and MCP timeout to 60s- *(mcp)* resolve find_unused and find_cycles timeouts on large graphs- *(lsp,core)* migrate remaining analysis path references to .sqry/analysis/- *(cli)* address all Codex findings for semantic diff command- *(haskell)* resolve snapshot format compatibility and add end-to-end persistence test- *(lang-go)* prevent duplicate edges in go/defer statement handling- *(test-helpers,go)* address Codex review findings (medium+low)- *(core,cli)* remove stub functions that silently produced wrong results- *(core)* merge duplicate add_node internal methods in GraphBuildHelper- *(analysis)* use discriminant-based lookup in AnalysisCache- *(analysis)* add self-loop accounting and improve scc_of error handling- *(analysis)* prevent OOM by checking 2-hop budget incrementally- *(analysis)* add graph identity validation to prevent stale analyses- *(analysis)* apply LWW merges and tombstone removals in CSR construction- *(analysis)* increase 2-hop label budget for large codebases- *(mcp)* address filesIndexed=0 bug and Codex review findings- *(core)* fix doctest compilation failures- *(java)* fix declaration context detection and local variable shadowing- *(core)* remove redundant error conversion in parallel evaluation- *(clippy)* collapse nested if-let statements in query_adapter- *(query)* enable scope predicates and workspace queries with CodeGraph- *(core)* align is_node_in_cycle semantics with find_all_cycles_graph- *(index)* report stage progress during post-processing- *(core)* stabilize plugin helper test- *(simd)* wrap unsafe helper calls- *(sonarqube)* harden coverage ingest for scans- *(core,cli,lang)* fix flaky tests and minor cleanups- *(core,lang)* address Codex/Gemini review issues for Wave 2/3 Import Edges- *(graph)* Wave 3 review fixes for language plugins- *(graph)* resolve bincode/serde alignment crash in snapshot persistence- address Gemini and Codex review feedback for FR-2025-021
- *(core)* resolve Groovy parser regression and bincode serialization bug- *(benchmarks)* align pilot queries to sqry predicate syntax, fix clippy- *(core)* resolve compiler warnings and improve type safety- *(FR-2025-022)* address review feedback for Ruby GraphBuilder- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(FR-2025-021)* wire up RustGraphBuilder and fix memory corruption bug (WP7)- *(persistence)* address all review findings from Codex and Gemini- *(core)* address clippy warnings in graph/session modules- *(prewarm)* address Step 2 code review findings from Codex- *(prewarm)* address code review findings for P2-8 Step 1- *(graph)* improve unified graph module stability- *(project)* address LOW priority post-implementation review items- *(project)* implement per-project index caching for MEDIUM review items- *(builder)* upgrade graph mode error logging from debug to warn- *(core)* repair broken test fixtures and method calls- *(core)* pedantic cleanup across git, search, symbols- *(visualization)* enforce detail levels and arrow labels- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(symbols)* harden v3 index integrity- *(rr-15)* fix trigram panic and document agent submission procedure- *(rr-15)* add missing evaluation_path field and export EvaluationPath- *(rr-15)* address Day 3 post-implementation review feedback- *(session)* replace .expect() with poison recovery in session cache (RR-03)- *(parser)* replace invariant .expect() calls with proper error handling- *(test)* replace double-unwrap with .expect() in session watcher test- *(thread-pool)* replace parking_lot::Mutex with std::sync::Mutex for poison recovery- *(test)* mark environmental lexer tests as ignored for CI stability- *(test)* update graph confidence test expectations for rebalanced weights- *(test)* add plugin factory support and mark plugin-dependent tests as ignored- *(compilation)* resolve all workspace compilation errors and warnings- *(warnings)* correct inner attribute placement- *(warnings)* suppress compilation warnings in sqry-core- *(ci)* add missing plugin_factory_helpers.rs test module- *(test)* address QA feedback on E2E golden fixtures- *(ruby)* add missing ruby-qualified-callers implementation files- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- *(java)* preserve qualified names in extract_symbols_from_tree- *(P2-3)* resolve all rust-analyzer warnings - unused imports and deprecations- *(P2-3)* clean up test code warnings - Symbol::new_unintern and unused imports- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(P2-34)* add missing scope_id field to Symbol literals in tests/benchmarks- *(query)* Check import alias when matching imports: predicate- *(core)* Allow empty RelationStore in relation queries- *(core)* Complete Step 3 - Stabilize watcher and add test helper script- *(auto-rebuild)* add temp CI workflows and instrumentation- Serialize buffer config tests to prevent env var interference
- Address clippy pedantic warnings across codebase
- Resolve rust-analyzer warnings in tests and benchmarks
- *(clippy)* apply safe type casts batch 2 - reduce warnings by 17%- *(clippy)* apply safe type cast conversions - reduce cast warnings by 55%- *(tests)* Fix CI test failures and deprecated warnings- *(fmt)* Apply rustfmt formatting to session/manager.rs- *(ci)* stabilize macOS watcher tests- *(ci)* Cross-platform fixes for CI failures- *(clippy)* Collapse nested if statement in watch mode debounce- *(ci)* Resolve clippy, rustdoc, and Windows compilation failures- Apply rustfmt formatting to sqry-cli/src/commands/index.rs
- *(legacy_usage)* Fix flaky concurrent test with SeqCst ordering- *(P1-14)* Address critical CODEX final verification blockers- *(P1-14)* Fix bincode serialization and checksum validation- *(P1-14)* Fix failing comprehensive validation tests- *(P1-14)* Fix decompression limit false positive for exact-limit data- *(P1-14)* Address all critical CODEX review findings for index validation- *(tests)* Resolve sqry-core test failures- *(clippy)* Resolve clippy warnings- *(core)* preserve SymbolId consistency after index load; normalize whitespace to AND for AST queries- *(d2)* remove erroneous closing brace in D2 exporter node output- *(merge)* add graph_builder trait method and graph_adapter module exports- *(vscode)* improve error diagnostics, progress cleanup, and stale lock handling- *(cli)* wire up --legacy-query flag to actually use legacy parser (FR-2025-015 Step 4 critical fix)- *(core)* handle empty query strings in SessionManager for repo-only queries (FR-2025-015)- *(core)* prevent cache collisions and evaluate normalized AST (FR-2025-015 critical fixes)- *(core)* prevent lock guard from removing replaced locks (CRITICAL)- *(core,cli,lsp)* fix critical race conditions in lock coordination- *(executor)* prefix unused executor variables with underscore- *(warnings)* resolve 10 compiler warnings across workspace- *(FR-2025-006-phase4)* fix Step 5 - add fallback for plugins without tree-sitter- *(watch-mode)* remove [TEST] eprintln! instrumentation from production code- *(content-diff)* CRITICAL - fix line number tracking bug causing false negatives- *(relations)* correct callers query false positive bug- *(graph)* correctly identify HTTP and FFI calls as cross-language edges- *(workspace)* prefix unused test variables with underscore- *(benchmark)* fix race condition - set running=true before barrier- *(benchmark)* force compiler to evaluate cache.get() results- *(phase2)* add Moka eviction listener to track eviction stats- *(phase2)* resolve cache hit counting bug in Arc integration- *(bench)* black_box benchmark results per codex review- *(query)* add fallback for legacy plugin fields + complete FT-D.1- *(core)* update normalizer unit tests to match reversed mappings- *(core)* reverse normalizer mappings to match metadata::keys (CRITICAL)- *(hybrid)* correctly report index usage in hybrid search results- *(tests)* add missing bincode dev-dependency for CLI tests- *(clippy)* add missing documentation for enum variant fields- *(benches)* correct hybrid search benchmark API calls- *(tests)* resolve all remaining integration test failures- *(core)* update legacy index test to use compatible version- *(cache)* test isolation bug causing flaky test failures- *(typescript)* remove type alias extraction per design intent- *(core)* add Debug derives to StringInterner and PathInterner- *(index)* address CODEX review HIGH priority items- *(search)* [**breaking**] return explicit error for unimplemented SearchMode variants- *(query)* add consecutive quantifier detection for ReDoS prevention- *(symbols)* address Codex CODE review critical and high priority issues- *(search)* address Codex HIGH priority review issues- *(search)* address Codex review HIGH priority issues
### Other
- apply rustfmt to fix formatting in sanitized tree build
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- run cargo fmt after clippy derivable_impls fixes
- fix clippy warnings for Rust 1.94
- *(fmt)* apply cargo fmt + complete share process docs- cleanup trailing whitespace, remove website, harden parallel commit
- *(graph)* fix clippy pedantic warnings in parallel commit pipeline- strip internal requirement IDs and LLM references from comments
- *(deps)* upgrade 5 dependencies to latest compatible versions- *(deps)* remove unnecessary dependencies and exclude internal tools- fix cargo fmt formatting in core and R plugin
- add clippy.toml to reduce too_many_arguments annotations
- apply cargo fmt formatting
- *(core)* address clippy pedantic warnings in build pipeline- *(workspace)* apply clippy pedantic Phase 3 formatting improvements- *(core)* remove orphaned vector module stubs- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- *(sonarqube)* implement cleanup plan and enable multi-language scanning- checkpoint symbol removal work
- remove dead code (~400 LOC)
- *(core)* remove obsolete SymbolIndex integration tests- *(cli)* remove dead code and unused functions- *(tests)* remove deprecated legacy format migration test- *(tests)* remove deprecated SymbolIndex scope tests- *(visualization)* drop unused mut binding- *(fmt)* normalize test formatting- *(relations)* document query sets- apply cargo fmt formatting fixes
- *(query)* rustfmt CD predicate analyzers and tests- *(docs)* complete feature archival and documentation cleanup- fix dead_code warnings and complete unified graph migration cleanup
- consolidate WIP for paging, MCP redaction, coverage tooling, and reviews
- *(clippy)* finalize cleanup and regenerate rkg- apply cargo fmt and import ordering
- *(clippy)* resolve lib warnings for sqry-core and sqry-lang-ruby- *(deps)* upgrade Lance 0.18→0.39.0 and Arrow 54→56.1 (WIP)- prepare codebase for FR-2026-001 hybrid embeddings implementation
- Clean up validation issues after relation tracking fix
- CI improvements, test infrastructure, and documentation updates
- *(pedantic)* clean up missing doc warnings- *(quality)* fix all clippy warnings for quality and security- *(format)* apply rustfmt to executor modules- update supporting files and legacy visualization command
- *(clippy)* resolve workspace lint warnings- sync outstanding modifications
- *(deps)* add cache system dependencies and update metadata- complete ServiceNow rebranding - update all plugin authors
- fix compiler warnings and update documentation
- release v0.9.0 - CLI Integration for Query Language Enhancements

### Performance
- *(graph)* wavefront parallelism, budget guards, and density gate for analysis- *(graph)* fix O(N*E) hang in Pass 5 collect_http_requests- *(analysis)* replace merge_intervals with FastBitSet for label computation- *(graph)* replace nested rayon::join barriers with into_par_iter pipeline- *(graph)* fix O(n²) AuxiliaryIndices rebuild in Phase 4c- *(graph)* add memory-bounded chunked parse-commit batching- *(analysis)* parallelize condensation DAG builds with rayon::join- *(core)* migrate remove_file to remove_file_with_info for O(N*B) performance- *(analysis)* eliminate O(degree²) allocations in SCC computation- *(progress)* add discovery phase events to prevent apparent index stall- *(rr-15)* add comprehensive query execution benchmark validating indexed evaluation- add comprehensive baseline benchmark suite and tooling
- *(FR-2025-006-phase4)* add realistic benchmarks, revise performance claims to match reality- *(graph)* add comprehensive benchmarks for graph operations- *(classifier)* optimize with single-pass regex matching (FR-2025-005 Phase 1.1)- *(symbols)* reuse IndexBuilder thread pools to reduce warm-build overhead- *(test)* optimize verbose logging performance and fix CODEX review issues
### WIP
- *(FR-2025-006-phase4)* implement Step 5 - IndexBuilder integration (tests failing)
### Bench
- *(core)* Add session performance benchmark suite- *(classifier)* add Criterion micro-benchmarks (FR-2025-005 Phase 1.3)
### Debug
- *(ci)* add diagnostic logging to hybrid e2e tests
### Release
- v0.18.0 - ServiceNow plugin + Native MCP server
- v0.15.0 - Go relations support
- v0.13.1 - Return type predicates and legacy index detection

### Style
- rustfmt formatting fixes
- fix if-let chain formatting and import ordering
- apply clippy auto-fixes
- apply cargo fmt formatting
- Fix rustfmt formatting issues
- *(clippy)* Fix float_cmp warnings batch 3 - COMPLETE (31 warnings) ✅- *(clippy)* Apply auto-fix for test code style improvements- apply rustfmt formatting

### Wip
- *(phase2)* add Arc cache benchmark (incomplete)## [5.0.0](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.0) - 2026-03-31

### Fixed
- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers
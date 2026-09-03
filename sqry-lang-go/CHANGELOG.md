# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [31.0.0](https://github.com/verivus-oss/sqry/compare/v30.0.1...v31.0.0) - 2026-09-02

### Fixed

- *(indexing)* [**breaking**] report real lines for local binding declarations in 7 languages ([#746](https://github.com/verivus-oss/sqry/pull/746))
  - **Upgrade note:** run `sqry index --force` once after upgrading past this release. It corrects reported declaration lines and columns in every language plugin it converts, changes every R declaration line and with it every R body hash and shape descriptor, and drops whole-file pseudo-bodies from python module-level FFI callers. Python indexes grow substantially: a binding and its usages were one node and are now separate, which measured +88% nodes, +47% edges and +32% snapshot size on the 33-file python3.13 asyncio tree, and took `sqry unused` there from 3320 rows to 7707. Daemon-backed setups must RESTART the daemon as part of the upgrade (`sqry daemon stop`, `sqry index --force .`, `sqry daemon start`): `sqry daemon rebuild --force` executes inside the still-running pre-upgrade daemon and republishes a pre-upgrade graph.
- *(graph)* record the declaration extent the body plane hashes ([#751](https://github.com/verivus-oss/sqry/pull/751))
## [28.0.0](https://github.com/verivus-oss/sqry/compare/v27.0.8...v28.0.0) - 2026-07-07

### Documentation

- *(release)* batched-release model + vet-drift/concurrency/runner recovery
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [20.0.0](https://github.com/verivus-oss/sqry/compare/v19.0.9...v20.0.0) - 2026-06-12

### Fixed
- land actual product work before marketplace packaging
## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))
### Fixed
- *(daemon)* preserve durable graph artifacts- *(release)* allowlist v16.0.7 baseline drift## [19.0.4](https://github.com/verivus-oss/sqry/compare/v19.0.3...v19.0.4) - 2026-06-04

### Other
- update Cargo.toml dependencies
## [19.0.3](https://github.com/verivus-oss/sqry/compare/v19.0.2...v19.0.3) - 2026-06-03

### Other
- update Cargo.toml dependencies
## [19.0.2](https://github.com/verivus-oss/sqry/compare/v19.0.1...v19.0.2) - 2026-06-03

### Other
- update Cargo.toml dependencies
## [19.0.1](https://github.com/verivus-oss/sqry/compare/v19.0.0...v19.0.1) - 2026-06-03

### Other
- update Cargo.toml dependencies
## [19.0.0](https://github.com/verivus-oss/sqry/compare/v18.0.11...v19.0.0) - 2026-06-03

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking## [18.0.2](https://github.com/verivus-oss/sqry/compare/v18.0.1...v18.0.2) - 2026-06-01

### Other
- update Cargo.toml dependencies
## [18.0.1](https://github.com/verivus-oss/sqry/compare/v18.0.0...v18.0.1) - 2026-06-01

### Other
- update Cargo.toml dependencies
## [18.0.0](https://github.com/verivus-oss/sqry/compare/v17.0.1...v18.0.0) - 2026-06-01

### Added
- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Fixed
- *(daemon)* preserve durable graph artifacts
### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [16.0.7](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.7) - 2026-05-31

### Fixed
- *(daemon)* preserve durable graph artifacts## [16.0.0](https://github.com/verivus-oss/sqry/compare/v15.0.8...v16.0.0) - 2026-05-19

### Added
- *(c-icall-precision)* land Phase A## [15.0.8](https://github.com/verivus-oss/sqry/compare/v15.0.6...v15.0.8) - 2026-05-17

### Added
- *(go)* Go implicit implements + promoted methods + function-signature implementations (T1) ([#277](https://github.com/verivus-oss/sqry/pull/277))## [15.0.3](https://github.com/verivus-oss/sqry/compare/v15.0.2...v15.0.3) - 2026-05-13

### Other
- update Cargo.toml dependencies
## [15.0.2](https://github.com/verivus-oss/sqry/compare/v15.0.1...v15.0.2) - 2026-05-13

### Other
- update Cargo.toml dependencies
## [12.1.0](https://github.com/verivus-oss/sqry/compare/v12.0.3...v12.1.0) - 2026-05-03

### Added
- cross-language field and generic type-parameter emission ([#169](https://github.com/verivus-oss/sqry/pull/169))
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [11.0.0](https://github.com/verivus-oss/sqry/compare/v10.0.4...v11.0.0) - 2026-04-30

### Documentation
- *(public-issue-triage)* add layer3 b1 codex iter3 review## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

### Added
- *(graph)* line-zero holistic fix — Chunk 1 (HU01-HU07, Phase 4c-prime)## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.2.0) - 2026-04-06

### Added
- *(graph)* add path enumeration mode with SCC pruning strategy to BFS kernel
### Other
- bump version to 7.2.0
## [7.1.5](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.1.5) - 2026-04-06

### Plan
- add DAG TOML implementation plan for MCP resource-backed skills
## [7.1.0](https://github.com/verivus-oss/sqry/compare/v6.0.23...v7.1.0) - 2026-04-04

### Documentation
- *(rust)* add macro and proc-macro boundaries design spec
### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata
### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03
## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(graph)* add Pass 5 global cross-language edge detection- *(go)* add local variable reference tracking with scope resolution- *(go)* complete Phase 4 - TypeOf/Reference edges for type aliases and generics- *(go)* implement Phase 4 - type aliases and generics- *(go)* add TypeOf/Reference edges for struct fields and interface methods (Phase 3)- *(go)* [**breaking**] add TypeOf/Reference edges for function/method parameters and returns (Phase 2)- *(go)* implement TypeOf and Reference edges for Phase 1 (variables/constants)- *(plugins)* apply RecursionGuard to 4 depth-based language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(cli)* add per-step progress output for indexing- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(relations)* implement staging graph relation extraction- *(unified-graph)* add query APIs to GraphSnapshot and language tracking to FileRegistry- *(plugins)* enable passing graph builder tests after unified graph migration- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(go)* implement complete FR-GO relation tracking- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(graphbuilders)* implement Phase 7 GraphBuilder for 7 domain-specific plugins- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-go to PluginSymbolBuilder- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(go)* add GoGraphBuilder with method receiver support- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(mcp)* enforce tool deadlines and tracing- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata- *(plugins)* complete FT-B.4 metadata migration for TypeScript, JavaScript, and Go (FINAL)- *(go)* add return type extraction for functions and methods- *(hybrid)* add JSON output support for text and combined search results- *(test)* expand verbose logging to language plugin tests (Sprint 3)- *(go)* extract individual const/var names from declarations- *(search)* fuzzy-search accelerators (v0.16.0)- *(v0.12.1)* complete Query Language Hardening with review fixes- *(plugins)* implement symbol extraction for JS, TS, Python, and Go- *(query)* implement AST-aware query command- initial sqry repository setup (Phase 0)

### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* critical cleanup batch- *(relations)* rename helper extractors- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(review)* add LOW priority post-implementation review responses- comprehensive benchmark suite and competitive analysis

### Fixed
- *(release)* remove 408 files tracked in git but matched by .gitignore- *(lang-go)* replace weak assertions with behavioral checks- *(lang-go)* strengthen test assertions- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(native-display)* preserve native names and synthetic ffi ids- *(deps)* complete tree-sitter 0.25→0.26 migration- *(graph)* address Codex review findings for Pass 5 cross-language detection- *(ci)* handle Windows backslash separators in path splitting- *(lang-go)* prevent duplicate edges in go/defer statement handling- *(test-helpers,go)* address Codex review findings (medium+low)- *(go)* address Codex Phase 3 review findings- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(go)* satisfy clippy lints and update doc status- *(go)* correct package qualification and add CallSiteStore
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- *(deps)* upgrade 5 dependencies to latest compatible versions- *(workspace)* apply clippy pedantic Phase 3 formatting improvements- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(swift)* resolve clippy pedantic lints- apply cargo fmt formatting across workspace
- apply cargo fmt formatting fixes
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- *(quality)* fix all clippy warnings for quality and security- complete ServiceNow rebranding - update all plugin authors

### Release
- v0.15.0 - Go relations support

### Style
- Fix rustfmt formatting issues

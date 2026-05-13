# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [15.0.3](https://github.com/verivus-oss/sqry/compare/v15.0.2...v15.0.3) - 2026-05-13

### Other
- update Cargo.toml dependencies
## [15.0.2](https://github.com/verivus-oss/sqry/compare/v15.0.1...v15.0.2) - 2026-05-13

### Other
- update Cargo.toml dependencies
## [13.0.11](https://github.com/verivus-oss/sqry/compare/v13.0.10...v13.0.11) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.10](https://github.com/verivus-oss/sqry/compare/v13.0.9...v13.0.10) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.9](https://github.com/verivus-oss/sqry/compare/v13.0.8...v13.0.9) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [12.1.0](https://github.com/verivus-oss/sqry/compare/v12.0.3...v12.1.0) - 2026-05-03

### Added
- cross-language field and generic type-parameter emission ([#169](https://github.com/verivus-oss/sqry/pull/169))
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [11.0.0](https://github.com/verivus-oss/sqry/compare/v10.0.4...v11.0.0) - 2026-04-30

### Documentation
- *(public-issue-triage)* add layer3 b1 codex iter3 review## [10.0.0](https://github.com/verivus-oss/sqry/compare/v9.0.23...v10.0.0) - 2026-04-27

### Added
- workspace-aware / cross-repo indexing (DAG 2026-04-26) ([#146](https://github.com/verivus-oss/sqry/pull/146))
## [9.0.11](https://github.com/verivus-oss/sqry/compare/v9.0.6...v9.0.11) - 2026-04-24

### Fixed
- *(rust)* preload t10 analyzer test state## [9.0.5](https://github.com/verivus-oss/sqry/compare/v9.0.1...v9.0.5) - 2026-04-24

### Fixed
- *(rust)* format shim isolation release fix## [9.0.4](https://github.com/verivus-oss/sqry/compare/v9.0.1...v9.0.4) - 2026-04-24

### Fixed
- *(rust)* isolate rust-analyzer shim tests## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

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

### Added
- *(rust)* implement macro boundary sub-analyzers and pipeline integration
### Documentation
- *(rust)* add macro and proc-macro boundaries design spec
### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata- *(rust)* resolve 12 clippy lints in macro_boundaries for sanitized tree- *(rust)* address Codex review - remove derive macros from attr registry, fix cfg eval format, add cache size guard
### Other
- sync versions and VERSION stamps to 6.0.18
- *(rustfmt)* normalize code for Rust 1.90 toolchain## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03
## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [6.0.2](https://github.com/verivus-oss/sqry/compare/v5.0.1...v6.0.2) - 2026-03-31

### Other
- *(rustfmt)* normalize code for Rust 1.90 toolchain## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(rust)* implement macro boundary sub-analyzers and pipeline integration- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(rust)* add local variable reference tracking with scope resolution- *(go)* [**breaking**] add TypeOf/Reference edges for function/method parameters and returns (Phase 2)- *(rust)* implement TypeOf/Reference edges with complete type extraction- *(plugins)* apply RecursionGuard to remaining language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(query)* add returns predicate and fix Lua qualified name lookup- *(query)* implement async:, static:, and visibility: predicates- *(visibility)* implement visibility metadata for Rust symbols- *(rust)* add file module path computation for qualified symbol names- *(cli)* add per-step progress output for indexing- *(lang-rust)* enable all P3 features by default for developer workflows- *(lang-rust)* complete P3 implementation with RA bridge and module resolver- *(lang-rust)* complete P3 feature wiring with codex sign-off- *(lang-rust)* implement P3 features for Rust plugin- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(lang)* add Rust attribute and TypeScript decorator extraction- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(graph)* add Rust OOP (impl Trait) and FFI (extern) edges- *(lsp)* add span-based call site tracking for precise call hierarchy- *(relations)* implement staging graph relation extraction- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(rust)* implement complete FR-RUST relation tracking- *(cd)* implement impl: predicate using graph-based edges- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* complete Phase 3 Zero-Warnings Initiative- *(P2-3)* Rust plugin reference implementation with PluginSymbolBuilder- *(P2-33)* complete Phase 1 - cross-file symbol resolution- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(graph)* implement macro-based registration for all 9 language GraphBuilders- *(graph)* implement Rust GraphBuilder for unified CodeGraph architecture- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(rust-relations)* delegate extraction to shared hooks- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata- *(core)* add lookahead/lookbehind support to regex validator (FT-C.1)- *(plugins)* migrate Python and Rust plugins to shared metadata constants (FT-B.4)- *(hybrid)* add JSON output support for text and combined search results- *(test)* expand verbose logging to language plugin tests (Sprint 3)- *(typescript,rust)* add TypeScript visibility modifiers and fix field collision- *(rust)* extract import aliases from use...as statements- *(search)* [**breaking**] release v0.17.0 - Jaccard similarity for fuzzy search- *(search)* fuzzy-search accelerators (v0.16.0)- *(core)* RT-S0 shared foundation for multi-language relation tracking- *(sprint2)* add cross-file semantic analysis with relation tracking- *(sprint2)* implement relation extraction infrastructure (Steps 0-2 partial)- *(v0.12.1)* complete Query Language Hardening with review fixes- *(scope)* implement scope extraction for Phase 3- *(plugin)* implement Rust language plugin (Step 2)- initial sqry repository setup (Phase 0)

### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(lang-rust)* clean up legacy code and implement missing metadata- *(sonarqube)* critical cleanup batch- *(sonarqube)* complete critical cleanup and lint passes- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- comprehensive benchmark suite and competitive analysis

### Fixed
- *(rust)* address Codex review - remove derive macros from attr registry, fix cfg eval format, add cache size guard- *(release)* remove 408 files tracked in git but matched by .gitignore- *(lang-rust)* replace weak assertions with behavioral checks- *(lang-rust)* strengthen test assertions- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(deps)* complete tree-sitter 0.25→0.26 migration- *(deps)* address criterion black_box deprecation and add execute validation tests- *(ci)* resolve all CI failures across platforms- *(rust)* address Codex review findings - type alias bounds and function TypeOf- *(query)* enable scope predicates and workspace queries with CodeGraph- *(graph)* Wave 3 review fixes for language plugins- *(FR-2025-021)* wire up RustGraphBuilder and fix memory corruption bug (WP7)- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- strip internal requirement IDs and LLM references from comments
- *(deps)* upgrade 5 dependencies to latest compatible versions- bump version to v3.4.2
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting fixes
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- complete ServiceNow rebranding - update all plugin authors

### Performance
- *(lang-rust)* fix O(2N) subprocess spawns in RA bridge
### Release
- v0.13.1 - Return type predicates and legacy index detection

### Style
- Fix rustfmt formatting issues
- apply rustfmt formatting
## [5.0.0](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.0) - 2026-03-31

### Added
- *(rust)* implement macro boundary sub-analyzers and pipeline integration
### Fixed
- *(rust)* address Codex review - remove derive macros from attr registry, fix cfg eval format, add cache size guard
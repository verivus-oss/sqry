# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
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
- *(cpp)* bound pathological graph builds- *(release)* add VERSION stamps to all public directories for consistent release metadata- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers
### Other
- sync versions and VERSION stamps to 6.0.18
- apply rustfmt to fix formatting in sanitized tree build
## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03
## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(graph)* add Pass 5 global cross-language edge detection- *(typescript)* add HTTP request edge detection for fetch/axios patterns- *(typescript)* add local variable reference tracking with scope resolution- *(typescript)* upgrade TypeOf/Reference edges to new API with context metadata- *(ruby)* implement signature metadata extraction for methods- *(go)* [**breaking**] add TypeOf/Reference edges for function/method parameters and returns (Phase 2)- *(typescript)* implement complete type extraction with zero limitations- *(plugins)* apply RecursionGuard to remaining language plugins- *(plugins)* complete P2 advanced features for all 8 plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(sql,ts)* add SQL trigger call edges and TypeScript import specifiers- *(cli)* add per-step progress output for indexing- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(lang)* add Rust attribute and TypeScript decorator extraction- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(plugins)* enhance TypeScript/Dart/Groovy/SQL with metadata and strict tests- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-typescript to PluginSymbolBuilder- *(P2-33)* complete Phase 1 - cross-file symbol resolution- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(graph)* complete Phase 6 - unified graph architecture rollout- *(typescript)* implement TypeScript GraphBuilder for unified graph- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata- *(plugins)* complete FT-B.4 metadata migration for TypeScript, JavaScript, and Go (FINAL)- *(typescript)* add return type extraction for functions and methods- *(hybrid)* add JSON output support for text and combined search results- *(test)* add logging to TypeScript critical_fixes and patch tests- *(test)* expand verbose logging to language plugin tests (Sprint 3)- *(typescript,rust)* add TypeScript visibility modifiers and fix field collision- *(search)* fuzzy-search accelerators (v0.16.0)- *(plugins)* implement symbol extraction for JS, TS, Python, and Go- *(query)* implement AST-aware query command- initial sqry repository setup (Phase 0)

### Changed
- migrate CLI commands from SymbolIndex to unified graph
- reduce cognitive complexity in high-priority functions
- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- Add reasons to all #[ignore] test attributes
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(relations)* apply clippy auto-fixes to graph builders- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- *(review)* add LOW priority post-implementation review responses- comprehensive benchmark suite and competitive analysis

### Fixed
- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers- *(release)* remove 408 files tracked in git but matched by .gitignore- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(release)* resolve preflight native-name regressions- *(graph)* address Codex review findings for Pass 5 cross-language detection- *(ci)* resolve all CI failures across platforms- *(cpp,python)* address all Codex review findings (100% test pass)- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(FR-2025-021)* wire up RustGraphBuilder and fix memory corruption bug (WP7)- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(relations)* improve JavaScript/TypeScript anonymous caller handling and optional chains- *(relations)* correct callers query false positive bug- *(typescript)* correct named function expression return type extraction- *(typescript)* remove type alias extraction per design intent
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- apply rustfmt to fix formatting in sanitized tree build
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- *(deps)* remove unnecessary dependencies and exclude internal tools- add clippy.toml to reduce too_many_arguments annotations
- bump version to v3.4.2
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- *(quality)* fix all clippy warnings for quality and security- complete ServiceNow rebranding - update all plugin authors

### Release
- v0.14.0 - TypeScript relations with type-only metadata

### Style
- Fix rustfmt formatting issues
## [5.0.0](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.0) - 2026-03-31

### Fixed
- *(graph)* resolve Method/Function NodeKind mismatch dropping get_references callers
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [17.0.0](https://github.com/verivus-oss/sqry/compare/v16.0.8...v17.0.0) - 2026-05-31

### Other
- update Cargo.toml dependencies
## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [13.0.11](https://github.com/verivus-oss/sqry/compare/v13.0.10...v13.0.11) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.10](https://github.com/verivus-oss/sqry/compare/v13.0.9...v13.0.10) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.9](https://github.com/verivus-oss/sqry/compare/v13.0.8...v13.0.9) - 2026-05-08

### Other
- update Cargo.toml dependencies
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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(graph)* add Pass 5 global cross-language edge detection- *(python)* add local variable reference tracking with scope resolution- *(plugins)* apply RecursionGuard to remaining language plugins- *(plugins)* complete P2 advanced features for all 8 plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- parallel agent execution - multi-stream improvements
- *(cli)* add per-step progress output for indexing- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang-python)* enhance parameter_types with JSON storage and review fixes- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(graphbuilders)* implement Phase 7 GraphBuilder for 7 domain-specific plugins- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-python to PluginSymbolBuilder- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(graph)* complete Phase 6 - unified graph architecture rollout- *(python)* implement Python GraphBuilder for unified graph- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(plugins)* add tier-2 plugins and metadata- *(plugins)* migrate Python and Rust plugins to shared metadata constants (FT-B.4)- *(python)* add return type extraction for functions with type hints- *(python)* add decorator extraction with cache metadata preservation- *(python)* add async detection and visibility metadata- *(search)* fuzzy-search accelerators (v0.16.0)- *(js)* implement call edge extraction for JavaScript (JS-2)- *(python)* implement relation tracking (PY-1 through PY-4)- *(plugins)* implement symbol extraction for JS, TS, Python, and Go- *(query)* implement AST-aware query command- initial sqry repository setup (Phase 0)

### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* critical cleanup batch- *(plugins)* share metadata application helper- *(plugins)* centralize query extraction- *(sonarqube)* complete critical cleanup and lint passes- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(relations)* apply clippy auto-fixes to graph builders
### Documentation
- *(review)* add LOW priority post-implementation review responses- comprehensive benchmark suite and competitive analysis

### Fixed
- *(lang-python)* strengthen test assertions- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(release)* resolve preflight native-name regressions- *(python)* separate FFI library naming from general call targets- *(python)* preserve library base names in simple_name()- *(cpp,python)* address all Codex review findings (100% test pass)- *(python)* index standalone functions at module level- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(relations)* improve JavaScript/TypeScript anonymous caller handling and optional chains- *(relations)* correct callers query false positive bug- *(python)* address code review feedback
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- *(workspace)* apply clippy pedantic Phase 3 formatting improvements- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply cargo fmt formatting across workspace
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- *(quality)* fix all clippy warnings for quality and security- complete ServiceNow rebranding - update all plugin authors

### Style
- Fix rustfmt formatting issues

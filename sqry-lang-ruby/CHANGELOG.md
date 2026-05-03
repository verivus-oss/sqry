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
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(ruby)* complete signature metadata with return type extraction and validation- *(ruby)* implement signature metadata extraction for methods- *(plugins)* apply RecursionGuard to remaining language plugins- *(plugins)* complete P2 advanced features for all 8 plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(graph)* add Wave 2 OOP edges for Kotlin, Scala, Ruby- *(relations)* implement staging graph relation extraction- *(unified-graph)* add query APIs to GraphSnapshot and language tracking to FileRegistry- *(plugins)* enable passing graph builder tests after unified graph migration- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- [**breaking**] remove legacy hooks from relations-shared and plugins (FR-2025-021 WP5)
- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(ruby)* Promote to Tier 1, achieve 15-language milestone ✨- *(swift)* Implement full relation tracking support- *(ruby)* Add full relation tracking (calls, exports, imports)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(lang)* complete FR-2025-009 Phase 2 critical fixes for Ruby/PHP/Swift- *(ruby)* implement Tier-2 graph builder with FFI detection- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata- *(ruby)* add Ruby language plugin with full symbol extraction
### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- *(ruby)* migrate to PluginSymbolBuilder pattern- *(ruby)* Enhance Ruby relation tracking with improved tests and memory profiling- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- *(deprecated-api-removal)* remove extract_imports mentions- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(release)* resolve preflight native-name regressions- *(cpp,python)* address all Codex review findings (100% test pass)- *(graph)* Wave 3 review fixes for language plugins- *(FR-2025-022)* address review feedback for Ruby GraphBuilder- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(ruby)* add missing ruby-qualified-callers implementation files- *(ruby)* populate Symbol.qualified_name for workspace queries- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- Resolve rust-analyzer warnings in tests and benchmarks
- *(ruby)* collapse nested if statements in tests for clippy
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- *(deps)* upgrade 5 dependencies to latest compatible versions- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- *(swift)* resolve clippy pedantic lints- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(clippy)* resolve lib warnings for sqry-core and sqry-lang-ruby- prepare codebase for FR-2026-001 hybrid embeddings implementation
- sync outstanding modifications

### Performance
- *(ruby)* Add memory profiling and verify NFR-RB-2- *(ruby)* Add relation extraction benchmark and verify NFR-RB-1
### Style
- Fix rustfmt formatting issues

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(persistence)* [**breaking**] migrate bincode → postcard for all serialization- *(php)* implement FFI edge detection in unified graph- *(php)* implement signature extraction with return types- *(plugins)* apply RecursionGuard to remaining language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- parallel agent execution - multi-stream improvements
- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- [**breaking**] remove legacy hooks from relations-shared and plugins (FR-2025-021 WP5)
- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(scopes)* convert Elixir/Haskell/PHP/R to real Scope API implementations- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(swift)* Implement full relation tracking support- *(php)* Add comprehensive PHP relation tracking implementation- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(lang)* complete FR-2025-009 Phase 2 critical fixes for Ruby/PHP/Swift- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test- *(php)* add PHP language plugin with comprehensive web framework support
### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* critical cleanup batch- *(graph)* migrate GraphBuilder trait to use GraphSnapshot- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- *(php)* migrate to PluginSymbolBuilder pattern- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- enforce GROUND RULES and archive completed CLI/VSCode Sync

### Fixed
- *(release)* remove 408 files tracked in git but matched by .gitignore- *(release)* resolve preflight native-name regressions- *(deps)* complete tree-sitter 0.25→0.26 migration- *(graph)* address Codex review findings for Pass 5 cross-language detection- *(php)* use field-based identity comparison in argument unwrapping- *(php)* improve argument unwrapping and interpolation detection robustness- *(php)* use field-based access and expand interpolation detection- *(php)* add name-aware argument extraction and comprehensive interpolation detection- *(php)* add PHP 8 named arguments and interpolation detection for FFI- *(php)* address Codex iteration 1 findings for FFI implementation- *(php)* address Codex review findings for FFI implementation- *(cpp,python)* address all Codex review findings (100% test pass)- *(php)* add null-safe operator support and enable PHP relation tests- *(test)* update PHP tests to use :: separator for qualified names- *(graph-builders)* Wave 2 review fixes for C/C++/C#/PHP/Perl- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(php)* Align method names in symbol and export extraction- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- Resolve rust-analyzer warnings in tests and benchmarks

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- strip internal requirement IDs and LLM references from comments
- *(deps)* upgrade 5 dependencies to latest compatible versions- bump version to v3.4.2
- *(kotlin,php)* apply cargo fmt formatting- *(workspace)* apply clippy pedantic Phase 3 formatting improvements- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- *(swift)* resolve clippy pedantic lints- apply cargo fmt formatting across workspace
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- sync outstanding modifications

### Style
- Fix rustfmt formatting issues

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [28.0.0](https://github.com/verivus-oss/sqry/compare/v27.0.8...v28.0.0) - 2026-07-07

### Documentation

- *(release)* batched-release model + vet-drift/concurrency/runner recovery
## [25.0.0](https://github.com/verivus-oss/sqry/compare/v24.0.1...v25.0.0) - 2026-06-30

### Added
- *(rules)* Phase 5 declarative rule layer## [24.0.0](https://github.com/verivus-oss/sqry/compare/v23.2.0...v24.0.0) - 2026-06-29

### Documentation
- complete MCP filter and public docs refresh
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [15.0.3](https://github.com/verivus-oss/sqry/compare/v15.0.2...v15.0.3) - 2026-05-13

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
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.2.0) - 2026-04-06

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(vue,svelte)* add TypeOf and References edge extraction for TypeScript annotations- *(langs)* add visibility metadata to 6 language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- *(imports)* add Import edge support for Swift, Dart, Zig, Vue, Svelte- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* implement Wave 4 Call + Import edges for Vue, Svelte, HTML, CSS- *(graph)* Phase 5C Svelte/Vue script-level call edges- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* add SafeParser error types and update plugins- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- add slopscan tooling and audit docs
- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(plugins)* tier audit - downgrade Vue/ServiceNow to Tier 2, remove Svelte dead code (Stream 3)- *(P2-3-Step1)* migrate Svelte plugin to PluginSymbolBuilder (29/34)- *(P2-34)* implement scope nesting & file path support (Phase 1)- consolidate multiple feature implementations and documentation updates
- *(FR-2025-006-phase4)* complete Step 9 - documentation & polish ✅ PHASE 4 COMPLETE- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* complete critical cleanup and lint passes- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(release)* finalize FR-2025-008 v1.18.0 production readiness
### Fixed
- *(graph)* per-block body hashes for Vue/Svelte, fast pre-checks for JSON/HTML- *(clippy)* resolve all -D warnings across workspace- *(release)* resolve preflight native-name regressions- *(mcp)* resolve flaky discovery cache test with resettable static- *(svelte,vue)* create Component nodes and Contains edges for SFC files- *(cpp,python)* address all Codex review findings (100% test pass)- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- *(P2-3)* resolve all rust-analyzer warnings - unused imports and deprecations- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(svelte,vue)* adjust synthetic names to reference component file line numbers
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- *(deps)* remove unnecessary dependencies and exclude internal tools- *(vue,svelte)* fix clippy pedantic warnings in TypeOf/References extraction- add clippy.toml to reduce too_many_arguments annotations
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- fix dead_code warnings and complete unified graph migration cleanup
- *(pedantic)* clean up missing doc warnings- *(clippy)* resolve workspace lint warnings- sync outstanding modifications

### Style
- Fix rustfmt formatting issues

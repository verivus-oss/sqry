# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [30.0.1](https://github.com/verivus-oss/sqry/compare/v30.0.0...v30.0.1) - 2026-08-28

### Fixed

- *(indexing)* report real line and column for declarations in 9 languages ([#742](https://github.com/verivus-oss/sqry/pull/742))
## [28.0.0](https://github.com/verivus-oss/sqry/compare/v27.0.8...v28.0.0) - 2026-07-07

### Documentation

- *(release)* batched-release model + vet-drift/concurrency/runner recovery
## [24.0.0](https://github.com/verivus-oss/sqry/compare/v23.2.0...v24.0.0) - 2026-06-29

### Documentation
- complete MCP filter and public docs refresh
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(groovy)* complete FFI edge detection with all bug fixes- *(groovy)* implement complete FFI edge detection- *(fixtures)* restructure Java fixtures to match package hierarchy- *(langs)* add TypeOf and Reference edges for Groovy- *(plugins)* complete P2 advanced features for all 8 plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(relations)* implement staging graph relation extraction- *(unified-graph)* add query APIs to GraphSnapshot and language tracking to FileRegistry- *(plugins)* enable passing graph builder tests after unified graph migration- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* add SafeParser error types and update plugins- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(plugins)* enhance TypeScript/Dart/Groovy/SQL with metadata and strict tests- add slopscan tooling and audit docs
- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* complete Phase 3 Zero-Warnings Initiative- *(P2-3b)* complete full lazy index loading with V3 format- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang-groovy)* Implement export extraction and promote to Tier 1- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(cli)* extend graph filters (languages + edge kinds); register Lua in CLI loader\n\n- Adds new edge filters: table_read, table_write, triggered_by, channel_invoke, widget_child\n- Extends language filters to include sql, dart, lua, perl, shell, groovy, etc.\n- Registers LuaGraphBuilder in CLI loader\n\nDocs:\n- Update README examples for cross-language filters\n- Sync Phase 3/5A test execution docs with CLI alignment\n- Add Phase 4 04_PROGRESS.md and CODEX review placeholders for phases 2/4/5A\n\nSemver: minor bump to 1.18.0- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(plugins)* complete Svelte & Groovy semantic extraction + fix legacy test
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* complete critical cleanup and lint passes- *(relations)* rename helper extractors- *(relations)* deprecate legacy hook surfaces and extractors- *(groovy)* migrate to PluginSymbolBuilder pattern- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(native-display)* preserve native names and synthetic ffi ids- *(deps)* complete tree-sitter 0.25→0.26 migration- *(ci)* resolve all CI failures across platforms- *(groovy)* re-enable 3 disabled test assertions using StagingGraph API- *(core,lang)* address Codex/Gemini review issues for Wave 2/3 Import Edges- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- *(P2-3)* resolve all rust-analyzer warnings - unused imports and deprecations- complete P2-2 Symbol interning migration compatibility (215 errors → 0)

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- strip internal requirement IDs and LLM references from comments
- *(deps)* remove unnecessary dependencies and exclude internal tools- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting fixes
- fix dead_code warnings and complete unified graph migration cleanup
- *(pedantic)* clean up missing doc warnings- *(clippy)* resolve workspace lint warnings- sync outstanding modifications

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(java)* implement local variable reference tracking with scope resolution- *(dart)* implement static modifier detection for class fields- *(dart)* implement comprehensive FFI edge detection- *(fixtures)* restructure Java fixtures to match package hierarchy- *(langs)* add TypeOf and Reference edges for Dart- *(dart)* add enum and mixin node extraction- *(plugins)* apply RecursionGuard to 4 depth-based language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- *(imports)* add Import edge support for Swift, Dart, Zig, Vue, Svelte- *(graph)* add Export edge emission for 18 language plugins- *(cli)* add per-step progress output for indexing- *(graph)* complete legacy architecture removal and add Rust relation features- *(lang)* implement v2.6.0 Call and Table edges for Swift, Dart, SQL, ABAP, Apex, ServiceNow- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(plugins)* enhance TypeScript/Dart/Groovy/SQL with metadata and strict tests- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(lang-dart)* Implement full relation tracking with CLI tests (Tier 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- *(cli)* extend graph filters (languages + edge kinds); register Lua in CLI loader\n\n- Adds new edge filters: table_read, table_write, triggered_by, channel_invoke, widget_child\n- Extends language filters to include sql, dart, lua, perl, shell, groovy, etc.\n- Registers LuaGraphBuilder in CLI loader\n\nDocs:\n- Update README examples for cross-language filters\n- Sync Phase 3/5A test execution docs with CLI alignment\n- Add Phase 4 04_PROGRESS.md and CODEX review placeholders for phases 2/4/5A\n\nSemver: minor bump to 1.18.0- *(dart)* implement Phase 3 widget hierarchy edge extraction- *(dart)* support qualified receiver MethodChannel invocations- *(dart)* implement MethodChannel edge extraction (Phase 2)- *(dart)* broaden GraphBuilder to extract all classes and functions- *(sql)* implement table read/write edge extraction- *(sql,dart)* implement Phase 3 GraphBuilder with critical bug fixes- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(core)* add shared metadata constants module (FT-B.2)- *(dart)* replace async string-contains with AST-based detection (FT-B.0 PoC)- *(dart)* add Dart language plugin with Flutter support
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* critical cleanup batch- *(plugins)* share metadata application helper- *(plugins)* centralize query extraction- *(sonarqube)* complete critical cleanup and lint passes- *(relations)* rename helper extractors- *(relations)* deprecate legacy hook surfaces and extractors- *(dart)* migrate to PluginSymbolBuilder pattern- fix 73 clippy warnings across language modules
- Add reasons to all #[ignore] test attributes
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(native-display)* preserve native names and synthetic ffi ids- *(ci)* resolve all CI failures across platforms- *(haskell)* resolve snapshot format compatibility and add end-to-end persistence test- *(dart)* remove outdated TODO comments for class field TypeOf edges- *(dart)* implement TypeOf edges for class fields- *(cpp,python)* address all Codex review findings (100% test pass)- *(core,cli,lang)* fix flaky tests and minor cleanups- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(lang-dart)* Fix method receiver and cascade call extraction bugs- *(dart)* filter widget extraction false positives- *(dart)* support static const MethodChannel field declarations- *(dart)* use original channel names in edge metadata and support const fields- *(dart)* handle comments/whitespace in async detection (CRITICAL)
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- fix clippy warnings for Rust 1.94
- *(packaging)* prepare all crates for crates.io publishing- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- *(clippy)* finalize cleanup and regenerate rkg
### Style
- Fix rustfmt formatting issues

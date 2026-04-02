# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(scala)* complete Codex suggestions - comprehensive array coverage + javap validation- *(scala)* implement FFI edge detection for @native JNI methods- *(langs)* add TypeOf and Reference edges for Dart- *(scala)* add TypeOf and Reference edge support with visibility metadata- *(plugins)* apply RecursionGuard to remaining language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(graph)* add Wave 2 OOP edges for Kotlin, Scala, Ruby- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(lang-scala)* Implement full relation tracking (Tier 1 promotion)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(cli)* extend graph filters (languages + edge kinds); register Lua in CLI loader\n\n- Adds new edge filters: table_read, table_write, triggered_by, channel_invoke, widget_child\n- Extends language filters to include sql, dart, lua, perl, shell, groovy, etc.\n- Registers LuaGraphBuilder in CLI loader\n\nDocs:\n- Update README examples for cross-language filters\n- Sync Phase 3/5A test execution docs with CLI alignment\n- Add Phase 4 04_PROGRESS.md and CODEX review placeholders for phases 2/4/5A\n\nSemver: minor bump to 1.18.0- *(graph)* add GraphBuilder implementations for Kotlin, Scala, and C++- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(scala)* add Scala language plugin with comprehensive symbol extraction- *(ruby)* add Ruby language plugin with full symbol extraction
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(relations)* rename helper extractors- *(relations)* deprecate legacy hook surfaces and extractors- *(scala)* migrate to PluginSymbolBuilder pattern- fix 73 clippy warnings across language modules
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Fixed
- *(release)* resolve preflight native-name regressions- *(graph)* complete parallel indexing quality gaps per AGENTS.md- *(ci)* resolve all CI failures across platforms- *(scala)* implement Codex-suggested FFI enhancements- *(scala)* address Codex review findings for FFI implementation- *(cpp,python)* address all Codex review findings (100% test pass)- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- bump version to v3.4.2
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- fix dead_code warnings and complete unified graph migration cleanup

### Style
- Fix rustfmt formatting issues

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
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
- *(cpp)* bound pathological graph builds- *(release)* add VERSION stamps to all public directories for consistent release metadata
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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(persistence)* [**breaking**] migrate bincode → postcard for all serialization- *(ruby)* implement signature metadata extraction for methods- *(go)* [**breaking**] add TypeOf/Reference edges for function/method parameters and returns (Phase 2)- *(plugins)* apply RecursionGuard to 4 depth-based language plugins- *(cpp,elixir)* add visibility metadata support- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(query)* add returns predicate and fix Lua qualified name lookup- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-cpp to PluginSymbolBuilder- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(lang-c)* Implement full relation tracking with CLI tests (Tier 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- *(cpp)* integrate ASTGraph for O(1) caller lookup and FQN resolution (FR-2025-016)- consolidate multiple feature implementations and documentation updates
- *(cli)* extend graph filters (languages + edge kinds); register Lua in CLI loader\n\n- Adds new edge filters: table_read, table_write, triggered_by, channel_invoke, widget_child\n- Extends language filters to include sql, dart, lua, perl, shell, groovy, etc.\n- Registers LuaGraphBuilder in CLI loader\n\nDocs:\n- Update README examples for cross-language filters\n- Sync Phase 3/5A test execution docs with CLI alignment\n- Add Phase 4 04_PROGRESS.md and CODEX review placeholders for phases 2/4/5A\n\nSemver: minor bump to 1.18.0- *(graph)* add GraphBuilder implementations for Kotlin, Scala, and C++- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(lsp)* migrate clients to language server- *(relations)* add C++/C#/Kotlin relation extraction- *(cpp)* add C++ language plugin with comprehensive template support
### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* complete critical cleanup and lint passes- reduce cognitive complexity in analysis.rs and fix clippy warnings
- *(relations)* rename helper extractors- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- fix 73 clippy warnings across language modules
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- *(symbol-removal)* record C++ review and doc cleanup- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(release)* remove 408 files tracked in git but matched by .gitignore- *(sonar)* resolve quality gate failures and scan infrastructure issues- *(native-display)* preserve native names and synthetic ffi ids- *(haskell,cpp,apex,redaction)* resolve TODO audit findings across 4 crates- *(cpp)* correct copy-paste documentation errors from Kotlin plugin- *(cpp,python)* address all Codex review findings (100% test pass)- *(core,lang)* address Codex/Gemini review issues for Wave 2/3 Import Edges- *(graph-builders)* Wave 2 review fixes for C/C++/C#/PHP/Perl- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(plugins)* resolve all rust-analyzer warnings and CppPlugin instantiation issues
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- strip internal requirement IDs and LLM references from comments
- add clippy.toml to reduce too_many_arguments annotations
- bump version to v3.4.2
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply cargo fmt formatting across workspace
- *(docs)* complete feature archival and documentation cleanup- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- sync outstanding modifications

### Style
- Fix rustfmt formatting issues

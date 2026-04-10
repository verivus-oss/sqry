# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [8.0.0](https://github.com/verivus-oss/sqry/compare/v7.2.0...v8.0.0) - 2026-04-10

### Added
- *(graph)* land curated provenance and staged-release updates- *(lang-java)* statement-flow local variable tracking with labeled-break awareness## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.2.0) - 2026-04-06

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(graph)* add Pass 5 global cross-language edge detection- *(servicenow)* add Import edge detection for ES6 imports and require() calls- *(java)* implement local variable reference tracking with scope resolution- *(fixtures)* restructure Java fixtures to match package hierarchy- *(java)* add complete pattern variable support (Java 16-17)- *(plugins)* apply RecursionGuard to remaining language plugins- *(plugins)* apply RecursionGuard to Java language plugin- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(query)* add returns predicate and fix Lua qualified name lookup- *(cli)* add per-step progress output for indexing- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(graph)* add OOP and FFI edges for 6 languages (Wave 2)- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-java to PluginSymbolBuilder- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- *(java)* implement field type resolution for graph mode (FR-2025-016)- consolidate multiple feature implementations and documentation updates
- *(graph)* implement Java GraphBuilder for unified CodeGraph architecture (Phase 2 - partial)- *(FR-2025-006-phase4)* complete Step 9 - documentation & polish ✅ PHASE 4 COMPLETE- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(relations)* achieve 90% milestone - TypeScript/JavaScript migrations verified- *(lsp)* migrate clients to language server- *(java)* add full relation tracking with imports, exports, calls, references, return types- *(lang)* implement Java language plugin with full verbose logging support
### Changed
- *(java)* extract helper functions from build_scopes_recursive- *(java,kotlin)* migrate local_scopes to shared ScopeTree from sqry-core- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* critical cleanup batch- *(sonarqube)* complete critical cleanup and lint passes- *(relations)* rename helper extractors- *(FR-2025-021)* complete WP6 - adapter cleanup and dual path removal- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(java)* remove dead code fields from MethodContext and ASTGraph
### Documentation
- comprehensive benchmark suite and competitive analysis

### Fixed
- *(release)* resolve preflight native-name regressions- *(native-display)* preserve native names and synthetic ffi ids- *(deps)* complete tree-sitter 0.25→0.26 migration- *(graph)* address Codex review findings for Pass 5 cross-language detection- *(java)* early-return scope lookup and expand type-shadow test coverage- *(java)* address codex review findings for local variable tracking- *(java)* add visibility metadata to method nodes- *(java)* complete shadowing detection for all local scopes- *(java)* fix declaration context detection and local variable shadowing- *(cpp,python)* address all Codex review findings (100% test pass)- *(java)* add emptyGroupedNames to return_types test expectation- *(graph)* Wave 3 review fixes for language plugins- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- *(java)* preserve qualified names in extract_symbols_from_tree- *(P2-3)* resolve all rust-analyzer warnings - unused imports and deprecations- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(P2-34)* add missing scope_id field to Symbol literals in tests/benchmarks- *(clippy)* apply safe type cast conversions - reduce cast warnings by 55%- *(plugins)* resolve all rust-analyzer warnings and CppPlugin instantiation issues- *(java)* correct package qualification and API consistency in GraphBuilder
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- strip internal requirement IDs and LLM references from comments
- *(deps)* upgrade 5 dependencies to latest compatible versions- add clippy.toml to reduce too_many_arguments annotations
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply cargo fmt formatting fixes
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- *(java)* apply rustfmt to graph_builder_tests.rs- *(java)* apply cargo fmt formatting to graph_builder.rs
### Style
- Fix rustfmt formatting issues
- fix line wrapping in Java plugin

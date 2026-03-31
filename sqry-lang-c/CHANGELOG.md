# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(lang-c)* detect function pointer targets in designated initializers- *(c)* implement TypeOf and Reference edge support- *(plugins)* apply RecursionGuard to remaining language plugins- *(plugins)* apply RecursionGuard to 4 depth-based language plugins- *(c)* add visibility metadata support- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* harden Rust attributes + Wave 2/3 GraphBuilder edges- *(relations)* expand plugin graph builders for exports and inheritance- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(unified-graph)* add query APIs to GraphSnapshot and language tracking to FileRegistry- *(plugins)* enable passing graph builder tests after unified graph migration- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- [**breaking**] remove legacy hooks from relations-shared and plugins (FR-2025-021 WP5)
- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(graphbuilders)* implement Phase 7 GraphBuilder for 7 domain-specific plugins- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3)* migrate sqry-lang-c to PluginSymbolBuilder- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(lang-c)* Implement full relation tracking with CLI tests (Tier 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(c)* add CGraphBuilder for relation tracking- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(lsp)* migrate clients to language server- *(c)* add C language plugin with full verbose logging support
### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* complete critical cleanup and lint passes- *(graph)* migrate GraphBuilder trait to use GraphSnapshot- *(FR-2025-021)* stub deprecated extract_* methods and remove legacy tests (WP5)- *(relations)* deprecate legacy hook surfaces and extractors- fix 73 clippy warnings across language modules
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(deps)* complete tree-sitter 0.25→0.26 migration- *(ci)* handle Windows backslash separators in path splitting- *(c)* resolve iteration 4 HIGH finding - void return regression- *(c)* address all Codex iteration 3 review findings- *(c)* address Codex review findings for TypeOf/Reference edges- *(core,cli,lang)* fix flaky tests and minor cleanups- *(graph-builders)* Wave 2 review fixes for C/C++/C#/PHP/Perl- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- add clippy.toml to reduce too_many_arguments annotations
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(c)* resolve clippy pedantic lints (partial)- *(zig)* resolve clippy pedantic lints- apply cargo fmt formatting across workspace
- repository cleanup and review tooling
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg- *(pedantic)* clean up missing doc warnings- *(quality)* fix all clippy warnings for quality and security
### Style
- Fix rustfmt formatting issues

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(abap)* add Import edge detection for INCLUDE and TYPE-POOLS statements- *(abap)* add TypeOf and References edge extraction for type declarations- *(exports)* complete export edge implementation for all applicable languages- *(langs)* add visibility metadata to 6 language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(graph)* complete legacy architecture removal and add Rust relation features- *(lang)* implement v2.6.0 Call and Table edges for Swift, Dart, SQL, ABAP, Apex, ServiceNow- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(scopes)* implement real scope extraction for 6 domain-specific plugins- *(graphbuilders)* implement Phase 7 GraphBuilder for 7 domain-specific plugins- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3-Step1)* migrate SAP ABAP plugin to PluginSymbolBuilder (33/34)- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- *(abap)* implement SAP ABAP language plugin foundation with metadata extraction
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(relations)* deprecate legacy hook surfaces and extractors- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(review)* add LOW priority post-implementation review responses- *(FR-2025-011)* update enterprise plugin docs and fix ABAP bugs
### Fixed
- *(clippy)* resolve all -D warnings across workspace- *(plsql,abap,apex)* address Codex review findings for TypeOf/References edges- *(sap-abap)* resolve 9 enterprise feature test failures- *(tests)* update DSL plugin tests for unified NodeKind enum- *(cpp,python)* address all Codex review findings (100% test pass)- *(lang,tools)* address Codex review recommendations for production quality- *(clippy)* refactor or_insert_with to or_insert per pedantic lint (P2-23)- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(P2-34)* add missing scope_id field to Symbol literals in tests/benchmarks- *(FR-2025-011)* address second round of HIGH/MEDIUM/LOW priority bugs- *(FR-2025-011)* address HIGH/MEDIUM priority bugs in Phase 1 plugins
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- apply code formatting and fix clippy warning
- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg
### Style
- Fix rustfmt formatting issues

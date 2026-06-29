# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [24.0.0](https://github.com/verivus-oss/sqry/compare/v23.2.0...v24.0.0) - 2026-06-29

### Documentation
- complete MCP filter and public docs refresh
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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(servicenow)* add Import edge detection for ES6 imports and require() calls- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(symbol-removal)* [**breaking**] migrate core and plugins to graph-only- *(sql,ts)* add SQL trigger call edges and TypeScript import specifiers- *(exports)* enable Export edge support across all languages- *(graph)* add Export edge emission for 18 language plugins- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(lang)* implement v2.6.0 Call and Table edges for Swift, Dart, SQL, ABAP, Apex, ServiceNow- *(relations)* implement staging graph relation extraction- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(graph)* add caller/callee identity fields to EdgeMetadata (FR-2025-022)- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(plugins)* enhance TypeScript/Dart/Groovy/SQL with metadata and strict tests- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-34)* implement scope nesting & file path support (Phase 1)- *(lang)* promote Elixir, Shell, SQL, Zig to Tier 1- *(sql)* Integrate tree-sitter-sql fork with PL/pgSQL assignment support- *(lang-sql)* Implement Tier 1 relation extraction (extract_calls, extract_exports, extract_imports)- *(cli)* implement rich query diagnostics with miette integration (P1-8)- consolidate multiple feature implementations and documentation updates
- *(dart)* broaden GraphBuilder to extract all classes and functions- *(sql)* implement table read/write edge extraction- *(sql,dart)* implement Phase 3 GraphBuilder with critical bug fixes- *(FR-2025-006-phase4)* complete Step 7 - migrate all 21 plugins to extract_symbols_from_tree()- *(sql)* add SQL language plugin with table and view extraction
### Changed
- *(plugins)* migrate type extractors to shared utilities- migrate CLI commands from SymbolIndex to unified graph
- *(sonarqube)* complete critical cleanup and lint passes- *(relations)* rename helper extractors- *(relations)* deprecate legacy hook surfaces and extractors- *(sql)* migrate to PluginSymbolBuilder pattern- fix 73 clippy warnings across language modules
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep- *(plugins)* standardize language plugin implementations and test improvements
### Documentation
- *(review)* add LOW priority post-implementation review responses
### Fixed
- *(clippy)* resolve all -D warnings across workspace- *(release)* prepare v4.8.17 for leg1- *(sql)* mark visibility tests as ignored - feature not yet implemented- *(tests)* update NodeKind variants in SQL and Oracle PL/SQL tests- *(cpp,python)* address all Codex review findings (100% test pass)- *(graph)* Wave 3 review fixes for language plugins- *(lang)* resolve unused warnings across language plugins- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)
- *(P2-34)* add missing scope_id field to Symbol literals in tests/benchmarks- *(sql)* strengthen tests and ensure consistent table node kinds
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(packaging)* prepare all crates for crates.io publishing- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- apply cargo fmt formatting across workspace
- apply cargo fmt formatting fixes
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg
### Style
- Fix rustfmt formatting issues

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [28.0.0](https://github.com/verivus-oss/sqry/compare/v27.0.8...v28.0.0) - 2026-07-07

### Documentation

- *(release)* batched-release model + vet-drift/concurrency/runner recovery
## [24.0.0](https://github.com/verivus-oss/sqry/compare/v23.2.0...v24.0.0) - 2026-06-29

### Documentation
- complete MCP filter and public docs refresh
## [22.0.0](https://github.com/verivus-oss/sqry/compare/v21.0.1...v22.0.0) - 2026-06-25

### Added
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [20.0.10](https://github.com/verivus-oss/sqry/compare/v20.0.5...v20.0.10) - 2026-06-14

### Changed
- *(graph)* remove dead ExportMap / pass4_cross plumbing ([#418](https://github.com/verivus-oss/sqry/pull/418))## [20.0.0](https://github.com/verivus-oss/sqry/compare/v19.0.9...v20.0.0) - 2026-06-12

### Fixed
- land actual product work before marketplace packaging
## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

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
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [11.0.0](https://github.com/verivus-oss/sqry/compare/v10.0.4...v11.0.0) - 2026-04-30

### Documentation
- *(public-issue-triage)* add layer3 b1 codex iter3 review## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

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
- *(index)* surface structural indexing phases and highlights- *(index)* reunify analysis generation during indexing- *(haskell)* add References edge extraction for type constructor names- *(haskell)* add parens unwrapping for constrained and return type signatures- *(haskell)* add forall unwrapping, exact-target matcher, and forall+constraint tests- *(haskell)* add TypeOf edge extraction for type signatures, data types, and typeclasses- *(core)* add is_unsafe metadata field to NodeEntry- *(haskell)* implement comprehensive FFI edge detection- *(exports)* complete export edge implementation for all applicable languages- *(haskell)* implement visibility extraction based on module exports- *(plugins)* apply RecursionGuard to remaining language plugins- *(core)* [**breaking**] remove symbol types and migrate to CodeGraph- *(graph)* complete legacy architecture removal and add Rust relation features- *(core)* consolidate relations-shared into sqry-core (FR-2025-022)- *(relations)* implement staging graph relation extraction- *(unified-graph)* add metadata fields to EdgeKind (Calls, Imports, Exports)- *(graph)* migrate all language plugins to GraphBuildHelper (FR-2025-007 Phase 2)- *(graph)* make unified CodeGraph primary export (FR-2025-007 Phase 1)- *(plugin)* [**breaking**] remove deprecated extract_calls/imports/exports methods- *(plugins)* enhance Go/Python/Zig with rich metadata + fix evaluate_field semantics- *(scopes)* convert Elixir/Haskell/PHP/R to real Scope API implementations- *(rr-09)* update tree-sitter wrappers and language plugins for validation layer- *(P2-3-Step1)* migrate Haskell plugin to PluginSymbolBuilder (30/34)- *(P2-34)* implement scope nesting & file path support (Phase 1)- consolidate multiple feature implementations and documentation updates
- *(lsp)* migrate clients to language server- *(plugins)* add tier-2 plugins and metadata
### Changed
- migrate CLI commands from SymbolIndex to unified graph
- *(relations)* deprecate legacy hook surfaces and extractors- Add reasons to all #[ignore] test attributes
- apply clippy pedantic auto-fixes - reduce warnings by 54%
- *(clippy)* apply automated pedantic quick-fix sweep
### Documentation
- *(deprecated-api-removal)* remove extract_imports mentions
### Fixed
- *(native-display)* preserve native names and synthetic ffi ids- *(ci)* resolve all CI failures across platforms- *(haskell)* thread References dedup state across multi-constructor ADTs- *(haskell)* add cross-call References edge deduplication- *(haskell)* handle parens→forall unwrapping order and add negative assertions- *(haskell)* prevent rank-2 forall decomposition and add regression test- *(haskell,cpp,apex,redaction)* resolve TODO audit findings across 4 crates- *(cli)* address all Codex findings for semantic diff command- *(haskell)* resolve snapshot format compatibility and add end-to-end persistence test- *(haskell)* address Codex iteration 1 findings for FFI implementation- *(cpp,python)* address all Codex review findings (100% test pass)- *(FR-2025-021)* convert all language plugins from unit structs to struct-with-field- *(serde)* replace skip_serializing_if with serde(default) for bincode compat- *(FR-JS-PATCH-2)* update test for hash-based naming + RKG edge + fmt- complete P2-2 Symbol interning migration compatibility (215 errors → 0)

### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
- *(clippy)* reduce pedantic lint backlog- *(packaging)* prepare all crates for crates.io publishing- bump version to v3.4.2
- *(plugins)* standardize metadata version to env!("CARGO_PKG_VERSION")- *(clippy)* resolve pedantic lints- *(mcp)* clippy phase 2 - resolve warnings for multi-workspace cache isolation- apply cargo fmt formatting across workspace
- fix dead_code warnings and complete unified graph migration cleanup
- *(clippy)* finalize cleanup and regenerate rkg
### Style
- Fix rustfmt formatting issues

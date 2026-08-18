# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [30.0.0](https://github.com/verivus-oss/sqry/compare/v29.0.6...v30.0.0) - 2026-08-18

### Added

- *(planner)* add is_unsafe predicate for security rule filters ([#660](https://github.com/verivus-oss/sqry/pull/660))

### Documentation

- bring the README up to the v29.0.6 surface, and fix stale MCP counts ([#693](https://github.com/verivus-oss/sqry/pull/693))
## [29.0.5](https://github.com/verivus-oss/sqry/compare/v29.0.3...v29.0.5) - 2026-07-18

### Fixed

- *(db)* make relation-source-set scan O(|csr|+|delta|) not O(N x |delta|) ([#627](https://github.com/verivus-oss/sqry/pull/627))
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
- *(shape)* per-function body-shape descriptor + structural-similar surfaces (V15) ([#426](https://github.com/verivus-oss/sqry/pull/426))## [20.0.0](https://github.com/verivus-oss/sqry/compare/v19.0.9...v20.0.0) - 2026-06-12

### Fixed
- land actual product work before marketplace packaging
## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))
### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [19.0.0](https://github.com/verivus-oss/sqry/compare/v18.0.11...v19.0.0) - 2026-06-03

### Added
- *(go)* T2.4 channel pairing + T2.5 generic instantiation tracking## [18.0.0](https://github.com/verivus-oss/sqry/compare/v17.0.1...v18.0.0) - 2026-06-01

### Added
- *(go)* T3 — error chains + context propagation + build tags ([#279](https://github.com/verivus-oss/sqry/pull/279))## [17.0.0](https://github.com/verivus-oss/sqry/compare/v16.0.8...v17.0.0) - 2026-05-31

### Other
- update Cargo.toml dependencies
## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Added
- *(db)* V12 schema + framework/resolved_via filter predicates ([#323](https://github.com/verivus-oss/sqry/pull/323))
### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [16.0.7](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.7) - 2026-05-27

### Fixed
- *(db)* make iterative Tarjan SCC deterministic across HashMap iteration ([#316](https://github.com/verivus-oss/sqry/pull/316))## [16.0.0](https://github.com/verivus-oss/sqry/compare/v15.0.8...v16.0.0) - 2026-05-19

### Added
- *(c-icall-precision)* land Phase A## [15.0.8](https://github.com/verivus-oss/sqry/compare/v15.0.6...v15.0.8) - 2026-05-19

### Added
- *(c-icall-precision)* land Phase A## [14.0.3](https://github.com/verivus-oss/sqry/compare/v14.0.2...v14.0.3) - 2026-05-10

### Other
- update Cargo.toml dependencies
## [14.0.2](https://github.com/verivus-oss/sqry/compare/v13.0.17...v14.0.2) - 2026-05-10

### Fixed
- *(mcp)* sqry-mcp large-repo stability## [14.0.0](https://github.com/verivus-oss/sqry/compare/v13.0.17...v14.0.0) - 2026-05-09

### Fixed
- bugs from no-skip kernel test pass (#216, #213, #214, #215) ([#228](https://github.com/verivus-oss/sqry/pull/228))
## [13.0.17](https://github.com/verivus-oss/sqry/compare/v13.0.16...v13.0.17) - 2026-05-09

### Other
- update Cargo.toml dependencies
## [13.0.16](https://github.com/verivus-oss/sqry/compare/v13.0.14...v13.0.16) - 2026-05-09

### Other
- *(deps)* apply approved dependency upgrade batch## [13.0.11](https://github.com/verivus-oss/sqry/compare/v13.0.10...v13.0.11) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.10](https://github.com/verivus-oss/sqry/compare/v13.0.9...v13.0.10) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.9](https://github.com/verivus-oss/sqry/compare/v13.0.8...v13.0.9) - 2026-05-08

### Other
- update Cargo.toml dependencies
## [13.0.6](https://github.com/verivus-oss/sqry/compare/v13.0.5...v13.0.6) - 2026-05-07

### Fixed
- *(daemon)* persist validated derived cache entries## [13.0.2](https://github.com/verivus-oss/sqry/compare/v13.0.1...v13.0.2) - 2026-05-06

### Fixed
- *(unused)* apply binding-plane boundary filter## [12.1.0](https://github.com/verivus-oss/sqry/compare/v12.0.3...v12.1.0) - 2026-05-03

### Added
- cross-language field and generic type-parameter emission ([#169](https://github.com/verivus-oss/sqry/pull/169))
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [11.0.0](https://github.com/verivus-oss/sqry/compare/v10.0.4...v11.0.0) - 2026-04-30

### Documentation
- *(public-issue-triage)* add layer3 b1 codex iter3 review## [9.0.20](https://github.com/verivus-oss/sqry/compare/v9.0.19...v9.0.20) - 2026-04-26

### Fixed
- *(ci)* eliminate release test skips## [9.0.1](https://github.com/verivus-oss/sqry/compare/v9.0.0...v9.0.1) - 2026-04-23

### Added
- *(sqry-db)* implement PN4 structural subset fusion
### Fixed
- *(release)* require versions for publishable path deps- *(docs)* address Codex iter-0 BLOCK — version sync, FrameCodec, status --json## [9.0.0](https://github.com/verivus-oss/sqry/releases/tag/v9.0.0) - 2026-04-23

### Added
- *(sqry-db)* implement PN4 structural subset fusion
### Documentation
- *(reviews)* Task 14 end-of-phase summary
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [23.0.0](https://github.com/verivus-oss/sqry/compare/v22.0.4...v23.0.0) - 2026-06-27

### Added
- *(daemon)* add revision-aware workspaces## [20.0.0](https://github.com/verivus-oss/sqry/compare/v19.0.9...v20.0.0) - 2026-06-12

### Fixed
- land actual product work before marketplace packaging
## [19.0.5](https://github.com/verivus-oss/sqry/compare/v19.0.4...v19.0.5) - 2026-06-04

### Fixed
- *(release)* allowlist v16.0.7 baseline drift## [16.0.8](https://github.com/verivus-oss/sqry/compare/v16.0.6...v16.0.8) - 2026-05-31

### Other
- *(security)* tighten deny.toml bans + sources policy ([#321](https://github.com/verivus-oss/sqry/pull/321))## [14.0.3](https://github.com/verivus-oss/sqry/compare/v14.0.2...v14.0.3) - 2026-05-10

### Other
- update Cargo.toml dependencies
## [14.0.2](https://github.com/verivus-oss/sqry/compare/v13.0.17...v14.0.2) - 2026-05-10

### Fixed
- *(mcp)* sqry-mcp large-repo stability## [13.0.11](https://github.com/verivus-oss/sqry/compare/v13.0.10...v13.0.11) - 2026-05-08

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
- *(nl)* add 5-level model_dir resolver and wire --model-dir override## [10.0.0](https://github.com/verivus-oss/sqry/compare/v9.0.23...v10.0.0) - 2026-04-27

### Added
- workspace-aware / cross-repo indexing (DAG 2026-04-26) ([#146](https://github.com/verivus-oss/sqry/pull/146))
## [9.0.1](https://github.com/verivus-oss/sqry/compare/v9.0.0...v9.0.1) - 2026-04-23

### Added
- *(cli)* implement sqry daemon rebuild with status polling and timeout (CLI_REBUILD_3)- *(mcp)* add find_duplicates per-group member cap wire output and schema (DUP_2)## [9.0.0](https://github.com/verivus-oss/sqry/releases/tag/v9.0.0) - 2026-04-23

### Added
- *(cli)* implement sqry daemon rebuild with status polling and timeout (CLI_REBUILD_3)- *(mcp)* add daemon workspace mutual exclusion check in ensure_graph (MUTEX_1)
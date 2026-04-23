# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [9.0.1](https://github.com/verivus-oss/sqry/compare/v9.0.0...v9.0.1) - 2026-04-23

### Other
- update Cargo.toml dependencies
## [9.0.0](https://github.com/verivus-oss/sqry/compare/v8.0.7...v9.0.0) - 2026-04-20

### Other
- update Cargo.lock dependencies
## [8.0.7](https://github.com/verivus-oss/sqry/compare/v8.0.6...v8.0.7) - 2026-04-12

### Other
- update Cargo.lock dependencies
## [8.0.6](https://github.com/verivus-oss/sqry/compare/v8.0.5...v8.0.6) - 2026-04-12

### Other
- update Cargo.lock dependencies
## [8.0.5](https://github.com/verivus-oss/sqry/compare/v8.0.4...v8.0.5) - 2026-04-11

### Other
- update Cargo.lock dependencies
## [8.0.4](https://github.com/verivus-oss/sqry/compare/v8.0.3...v8.0.4) - 2026-04-11

### Other
- update Cargo.lock dependencies
## [8.0.2](https://github.com/verivus-oss/sqry/compare/v8.0.1...v8.0.2) - 2026-04-11

### Other
- update Cargo.lock dependencies
## [8.0.1](https://github.com/verivus-oss/sqry/compare/v8.0.0...v8.0.1) - 2026-04-11

### Other
- update Cargo.lock dependencies
## [8.0.0](https://github.com/verivus-oss/sqry/compare/v7.2.0...v8.0.0) - 2026-04-10

### Other
- update Cargo.lock dependencies
## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.5...v7.2.0) - 2026-04-06

### Changed
- graph-backed handlers now share the traversal kernel used by CLI and MCP, improving consistency for `trace_path`, `show_dependencies`, `dependency_impact`, `subgraph`, and `graph_export`

### Fixed
- workspace path resolution now rejects directory-traversal escapes outside the configured workspace root
- traversal-backed graph handlers now use deterministic path ordering and atomic truncation semantics

## [7.1.5](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.1.5) - 2026-04-05

### Other
- update Cargo.lock dependencies
## [7.1.4](https://github.com/verivus-oss/sqry/compare/v7.1.3...v7.1.4) - 2026-04-04

### Other
- update Cargo.lock dependencies
## [7.1.3](https://github.com/verivus-oss/sqry/compare/v7.1.2...v7.1.3) - 2026-04-04

### Other
- update Cargo.lock dependencies
## [7.1.2](https://github.com/verivus-oss/sqry/compare/v7.1.1...v7.1.2) - 2026-04-04

### Other
- update Cargo.lock dependencies
## [7.1.1](https://github.com/verivus-oss/sqry/compare/v7.1.0...v7.1.1) - 2026-04-04

### Other
- update Cargo.lock dependencies
## [7.1.0](https://github.com/verivus-oss/sqry/compare/v6.0.23...v7.1.0) - 2026-04-04

### Added
- *(classpath)* add JVM classpath analysis (Track C Tier 1)
### Documentation
- *(rust)* add macro and proc-macro boundaries design spec
### Fixed
- *(lsp)* preserve plugin selection provenance on rebuild- *(release)* add VERSION stamps to all public directories for consistent release metadata- *(release)* harden public sanitization outputs
### Other
- sync versions and VERSION stamps to 6.0.18
## [7.0.1](https://github.com/verivus-oss/sqry/compare/v7.0.0...v7.0.1) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [7.0.0](https://github.com/verivus-oss/sqry/compare/v6.0.24...v7.0.0) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.23](https://github.com/verivus-oss/sqry/compare/v6.0.22...v6.0.23) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.22](https://github.com/verivus-oss/sqry/compare/v6.0.21...v6.0.22) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.21](https://github.com/verivus-oss/sqry/compare/v6.0.20...v6.0.21) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.20](https://github.com/verivus-oss/sqry/compare/v6.0.19...v6.0.20) - 2026-04-03

### Other
- update Cargo.lock dependencies
## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [6.0.17](https://github.com/verivus-oss/sqry/compare/v6.0.16...v6.0.17) - 2026-04-02

### Other
- update Cargo.lock dependencies
## [6.0.16](https://github.com/verivus-oss/sqry/compare/v6.0.15...v6.0.16) - 2026-04-02

### Other
- update Cargo.lock dependencies
## [6.0.15](https://github.com/verivus-oss/sqry/compare/v6.0.12...v6.0.15) - 2026-04-02

### Fixed
- *(release)* harden public sanitization outputs## [6.0.14](https://github.com/verivus-oss/sqry/compare/v6.0.13...v6.0.14) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.13](https://github.com/verivus-oss/sqry/compare/v6.0.12...v6.0.13) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.12](https://github.com/verivus-oss/sqry/compare/v6.0.11...v6.0.12) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.11](https://github.com/verivus-oss/sqry/compare/v6.0.10...v6.0.11) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.10](https://github.com/verivus-oss/sqry/compare/v6.0.9...v6.0.10) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.9](https://github.com/verivus-oss/sqry/compare/v6.0.8...v6.0.9) - 2026-04-01

### Other
- update Cargo.lock dependencies
## [6.0.8](https://github.com/verivus-oss/sqry/compare/v6.0.7...v6.0.8) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.7](https://github.com/verivus-oss/sqry/compare/v6.0.6...v6.0.7) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.6](https://github.com/verivus-oss/sqry/compare/v6.0.5...v6.0.6) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.5](https://github.com/verivus-oss/sqry/compare/v6.0.4...v6.0.5) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.4](https://github.com/verivus-oss/sqry/compare/v6.0.3...v6.0.4) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.3](https://github.com/verivus-oss/sqry/compare/v6.0.2...v6.0.3) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.2](https://github.com/verivus-oss/sqry/compare/v6.0.1...v6.0.2) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.1](https://github.com/verivus-oss/sqry/compare/v6.0.0...v6.0.1) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [6.0.0](https://github.com/verivus-oss/sqry/compare/v5.0.1...v6.0.0) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [5.0.1](https://github.com/verivus-oss/sqry/compare/v5.0.0...v5.0.1) - 2026-03-31

### Other
- update Cargo.lock dependencies
## [5.0.0](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.0) - 2026-03-31

### Other
- update Cargo.lock dependencies

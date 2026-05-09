# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
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
## [13.0.9](https://github.com/verivus-oss/sqry/compare/v13.0.7...v13.0.9) - 2026-05-08
## [12.0.3](https://github.com/verivus-oss/sqry/compare/v12.0.2...v12.0.3) - 2026-05-03

### Other
- update Cargo.toml dependencies
## [12.0.2](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.2) - 2026-05-03

### Fixed
- *(nl)* make integrity tests clean-checkout safe## [12.0.1](https://github.com/verivus-oss/sqry/compare/v12.0.0...v12.0.1) - 2026-05-03

### Other
- update Cargo.toml dependencies
## [12.0.0](https://github.com/verivus-oss/sqry/compare/v11.0.4...v12.0.0) - 2026-05-02

### Added
- *(nl)* surface ONNX Runtime missing as actionable platform hint across CLI/MCP/LSP/daemon- *(nl)* classifier pool + daemon sqry_ask tool + LSP wiring + perf bounds- *(nl)* add SharedClassifier wrapper for concurrent IntentClassifier access- [**breaking**] strict integrity by default for NL classifier loader
- *(nl)* gated model auto-download with manifest sha256 verify- *(nl)* add 5-level model_dir resolver and wire --model-dir override
### Fixed
- *(nl)* harden classifier trust and daemon config- *(nl)* tighten ort dylib detection and gate LSP map_error test seam behind feature- *(nl)* replace placeholder NL07 tests with real harnesses; use scopeguard for pool panic-safety- *(nl)* regenerate checksums.json — version.txt hash drift fix- *(nl)* warn when custom-mode anchor skipped; add Display for TrustMode/ResolverLevel- *(nl)* add ureq connect/read timeouts to model downloader## [7.2.0](https://github.com/verivus-oss/sqry/compare/v7.1.4...v7.2.0) - 2026-04-06

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
- *(release)* add VERSION stamps to all public directories for consistent release metadata- *(ci)* un-track 2276 ignored files and add git clean to release workflows
### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.24](https://github.com/verivus-oss/sqry/compare/v6.0.23...v6.0.24) - 2026-04-03
## [6.0.19](https://github.com/verivus-oss/sqry/compare/v6.0.18...v6.0.19) - 2026-04-03

### Other
- sync versions and VERSION stamps to 6.0.18
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Fixed
- *(ci)* un-track 2276 ignored files and add git clean to release workflows
### Other
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))
## [5.0.0](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.0) - 2026-03-31

### Fixed
- *(ci)* un-track 2276 ignored files and add git clean to release workflows
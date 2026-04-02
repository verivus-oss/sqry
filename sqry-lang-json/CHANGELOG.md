# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [6.0.18](https://github.com/verivus-oss/sqry/compare/v6.0.17...v6.0.18) - 2026-04-02

### Fixed
- *(release)* add VERSION stamps to all public directories for consistent release metadata## [5.0.1](https://github.com/verivus-oss/sqry/compare/v4.12.7...v5.0.1) - 2026-03-31

### Added
- *(json)* make MAX_DEPTH and MAX_NODES configurable via SQRY_JSON_MAX_DEPTH/SQRY_JSON_MAX_NODES env vars- *(json)* add sqry-lang-json plugin for declarative JSON config files
### Fixed
- *(graph)* per-block body hashes for Vue/Svelte, fast pre-checks for JSON/HTML- *(json)* add #[serial] to env-mutating tests, boundary-specific depth assertions- *(json)* address Codex review — malformed unicode returns FFFD, MAX_NODES test hits limit- *(json)* address review findings — unicode escapes, inc() guard, parse depth limit, Value::Scalar
### Other
- release v5.0.1 ([#60](https://github.com/verivus-oss/sqry/pull/60))
- release v5.0.0 ([#58](https://github.com/verivus-oss/sqry/pull/58))

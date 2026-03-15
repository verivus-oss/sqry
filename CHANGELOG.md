# Changelog

Release notes for sqry. See [GitHub Releases](https://github.com/verivus-oss/sqry/releases) for downloads and signatures.

## [4.10.1] - 2026-03-15

### Fixed
- **LSP**: Index age now uses manifest `built_at` timestamp instead of filesystem `created()`, which on Linux returns stale inode birth time causing incorrect "index is stale" reports after rebuild.
- **VS Code Extension**: `validateIndexViaLSP` error handling no longer short-circuits the filesystem fallback, fixing false "No index found" prompts at startup.
- **VS Code Extension**: `folderHasIndex` now checks `.sqry/graph/manifest.json` instead of the removed `.sqry-index` file path.

# Change Log

All notable changes to the "sqry-vscode" extension will be documented in this file.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [4.8.1] - 2026-03-03

### Changed
- **Version aligned with sqry v4.8.1** - all documentation and references updated to match release

---

## [4.5.11] - 2026-02-27

### Changed
- **Version aligned with sqry CLI** - extension version now tracks the sqry CLI version for consistency
- **Binary auto-download** - offers to download the sqry binary from GitHub when not found on PATH (`sqry.autoDownload` setting)
- **35 language support** - aligned with sqry CLI 4.5.11 (all 35 tree-sitter plugins)
- **Cross-language edge detection** - Pass 5 global linking for FFI and HTTP route matching
- **OOP edge detection** - 16 languages support Inherits/Implements edges
- **FFI edge detection** - 11 languages support cross-language call detection

### Fixed
- Organization references corrected across documentation
- Language count consistency (35 languages, not 36)

---

## [0.0.8] - 2025-11-10

### Fixed
- **ESLint configuration** added to enable linting and fix `npm test` failures
- **TypeScript ESLint integration** with @typescript-eslint/parser and @typescript-eslint/eslint-plugin

### Changed
- **Version consistency** - reconciled version numbers across package.json and documentation
- **Developer dependencies** updated for better tooling support

## [0.0.7] - 2025-10-23

### Changed
- **Webpack bundling** for optimized package size and faster activation
  - Reduced package size from 345 KB → 295 KB (14% smaller)
  - Reduced file count from 245 → 151 files (38% fewer files)
  - Single bundled extension.js instead of 9 separate compiled files
  - Tree-shaking removed unused code from dependencies
  - Production minification for smaller footprint
- **Improved extension packaging** with better .vscodeignore configuration
  - Source files (.ts, tsconfig.json, webpack.config.js) excluded from package
  - Documentation files (PUBLISHING_GUIDE.md, test summaries) excluded
  - Only essential runtime files included

### Added
- **webpack** and **ts-loader** for production bundling
- **vscode:prepublish** script for automatic bundling before publishing

## [0.0.6] - 2025-10-23

### Added
- **Configurable index timeout** (`sqry.indexTimeoutMs`) with 5-minute default for large codebases
- **Separate timeout configuration** for search operations (`sqry.timeoutMs` - 15 seconds)
- **Smart error messages** that suggest the correct setting to adjust on timeout
- **Comprehensive configuration documentation** in README with examples for large projects

### Changed
- **Improved index completion notifications** with checkmark (✓) indicator for clarity
- **Better notification wording**: "Index built for {workspace}" instead of "rebuild complete"
- **Updated LSP server** with auto-dismissing completion messages
- **Enhanced timeout descriptions** in settings to clarify usage

### Fixed
- **Index timeout issues** for large codebases (2,700+ symbols, 10,000+ symbols)
- **Confusing "Rebuilding..." notifications** that appeared stuck after successful completion
- **Timeout error handling** now opens the appropriate setting based on operation type

## [0.0.5] - 2025-10-22

### Added
- LSP-based semantic search integration with `sqry lsp --stdio`
- Standard LSP handlers: `textDocument/definition`, `textDocument/references`, `textDocument/hover`
- Document synchronization with UTF-16 position conversion
- Code actions for "Find Callers" and "Explain Symbol"
- Semantic search results panel with tree view
- CodeLens annotations showing caller counts

### Changed
- Migrated from CLI-based search to LSP server architecture
- Improved error handling for LSP communication
- Enhanced telemetry and observability

### Fixed
- UTF-16/UTF-8 position conversion issues for multi-byte characters
- Document synchronization race conditions

## [0.0.4] - 2025-10-16

### Added
- Code Actions support for contextual commands
- Improved CodeLens performance
- Better error diagnostics

### Changed
- Refactored client-server communication
- Enhanced search result display

## [0.0.3] - 2025-10-15

### Added
- CodeLens provider for caller counts
- Auto-indexing prompt on workspace open
- Configuration settings for timeout and limits

### Fixed
- Search panel state management
- Extension activation timing

## [0.0.2] - 2025-10-15

### Added
- Semantic search panel with results tree
- Find References command
- Search Workspace command

### Changed
- Improved search result formatting
- Better error messages

## [0.0.1] - 2025-10-15

### Added
- Initial VSIX release
- Basic query command integration
- Index workspace command
- sqry CLI integration
- Basic configuration settings

[0.0.6]: https://github.com/verivus-oss/sqry/compare/vscode-v0.0.5...vscode-v0.0.6
[0.0.5]: https://github.com/verivus-oss/sqry/compare/vscode-v0.0.4...vscode-v0.0.5
[0.0.4]: https://github.com/verivus-oss/sqry/compare/vscode-v0.0.3...vscode-v0.0.4
[0.0.3]: https://github.com/verivus-oss/sqry/compare/vscode-v0.0.2...vscode-v0.0.3
[0.0.2]: https://github.com/verivus-oss/sqry/compare/vscode-v0.0.1...vscode-v0.0.2
[0.0.1]: https://github.com/verivus-oss/sqry/releases/tag/vscode-v0.0.1

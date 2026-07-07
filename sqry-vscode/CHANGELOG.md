# Change Log

All notable changes to the "sqry-vscode" extension will be documented in this file.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

## [28.0.1] - 2026-07-07

### Fixed
- Stop rejecting valid binary downloads when the bundled Sigstore trust root
  expires. The trust root shipped inside sigstore-js expires roughly every few
  months; once it lapsed, provenance verification failed with `root was signed
  by 0/3 keys` and the extension blocked the install even though the binary and
  its SHA-256 were valid. sigstore is updated to v5 (current trust root), and
  when cosign fails specifically because the Sigstore trust root is expired or
  unreachable and the binary's SHA-256 already matched the release checksum
  manifest, the download is now accepted with a loud warning instead of being
  rejected. A genuine signature or certificate-identity mismatch still fails.

## [25.0.2] - 2026-07-01

### Changed
- Updated extension release metadata and bundled sqry compatibility to
  `25.0.2`.

## [21.0.0] - 2026-06-16

### Changed
- Updated extension release metadata and bundled sqry compatibility to
  `21.0.0`. The bundled sqry removes the natural-language surface (`sqry ask`,
  the `sqry_ask` MCP tool, `sqry/ask` LSP request); use the structured query and
  graph commands instead.

## [16.0.0] - 2026-05-19

### Changed
- Updated extension release metadata and bundled sqry compatibility to
  `16.0.0`.

## [15.0.8] - 2026-05-17

### Changed
- Updated extension release metadata and bundled sqry compatibility to
  `15.0.8`.

## [13.0.15] - 2026-05-09

### Changed
- Updated extension release metadata and bundled sqry compatibility to
  `13.0.15`.

## [10.0.1] - 2026-04-27

### Fixed
- Fixed VS Code binary auto-download provenance verification for current public releases signed by `release-distribute.yml`, while keeping compatibility with older releases signed by `oss-distribute.yml`.
- Fixed multi-root workspace status rendering so indexed source roots are shown from the authoritative `sqry/workspaceStatus` aggregate instead of falling back to a false "not indexed" state.
- Improved single-folder status hydration so rich index statistics remain visible while aggregate workspace status is still used for workspace health.

### Changed
- Updated binary compatibility to the sqry `10.0.1` toolchain.
- Added regression coverage for release provenance, workspace-status routing, and multi-root panel rendering.

## [10.0.0] - 2026-04-27

### Added
- Added workspace-aware multi-root support for saved `.code-workspace` files with `sqry.workspace` classification.
- Added source-root, member-folder, exclusion, and project-root-mode handling for cross-repo workspace analysis.
- Added aggregate workspace status rendering in the sqry pane and status bar.

### Changed
- Updated extension activation to pass workspace classification hints and the workspace-file path to the sqry LSP server.
- Updated index prompts and auto-index behavior to operate over source roots instead of treating every opened folder as an independent workspace.

## [9.0.0] - 2026-04-23

### Added
- Added daemon-backed workflows for sqry `9.x`, including shared graph use across CLI, LSP, and MCP sessions.
- Added support for daemon auto-start when the LSP or MCP server is launched in daemon mode.
- Added support for semantic-diff, dependency, duplicate, cycle, and unused-code analysis backed by the unified query engine.

### Changed
- Improved graph and relation analysis consistency across CLI, LSP, MCP, and the VS Code extension.
- Improved cold-start behavior by reusing persisted derived query data when available.

## [8.0.3] - 2026-04-11

### Fixed
- Accepted the public Sigstore workflow identities emitted by `verivus-oss/sqry` so binary auto-download no longer rejects valid public releases with a provenance verification error.

## [7.1.0] - 2026-04-04

### Changed
- Bump bundled sqry toolchain compatibility to `7.1.0`

### Fixed
- Align extension release metadata with plugin cost tiering and manifest-backed plugin selection support in the core sqry toolchain

## [7.0.2] - 2026-04-03

### Fixed
- Version bump for workspace release verification and pipeline stabilization fixes

## [6.0.24] - 2026-04-03

### Fixed
- Resolve SonarQube issues: modernize graph.js (let/const, for-of, globalThis), add sort comparators, simplify optional chaining, reduce cognitive complexity, mark readonly members

## [4.12.7] - 2026-03-29

### Fixed
- Detect unified graph index (`.sqry/graph/manifest.json`) instead of legacy `.sqry-index`

## [4.12.5] - 2026-03-27

### Fixed
- Release pipeline orchestrator fix

## [4.12.4] - 2026-03-27

### Changed
- Version bump for release pipeline stability improvements

## [4.12.3] - 2026-03-26

### Fixed
- SLSA provenance artifact download fix

## [4.12.2] - 2026-03-26

### Fixed
- Release preflight Dockerfile COPY validation

## [4.12.1] - 2026-03-26

### Fixed
- Docker image build fix for JSON plugin crate

## [4.12.0] - 2026-03-26

### Added
- JSON language plugin support (36th language)
- Export graph symbol filtering and visualization filter_node_ids

### Fixed
- Export graph post-pagination rendering
- SLSA provenance generation and artifact compatibility

## [4.11.11] - 2026-03-26

### Fixed
- SLSA provenance artifact download compatibility fix

## [4.11.10] - 2026-03-26

### Fixed
- SLSA provenance generation fix (tag ref + private-repository override)

## [4.11.9] - 2026-03-25

### Added
- Export graph symbol filtering and visualization filter_node_ids support

### Fixed
- Export graph post-pagination rendering

## [4.11.8] - 2026-03-25

### Fixed
- Version bump for SLSA continue-on-error fix

## [4.11.7] - 2026-03-25

### Fixed
- Version bump for SLSA private-repository override

## [4.11.6] - 2026-03-24

### Fixed
- Version bump for SLSA v2.1.0 upgrade, Apple signing fix, and gate reduction

## [4.11.5] - 2026-03-24

### Fixed
- Version bump for SLSA pipeline fix and Apple signing fix

## [4.11.4] - 2026-03-24

### Fixed
- Version bump for sqry-core config snapshot hash fix

## [4.11.3] - 2026-03-23

### Fixed
- Version bump for cargo-vet and formatting fixes

## [4.11.2] - 2026-03-23

### Fixed
- Version bump for sanitization lint fix

## [4.11.1] - 2026-03-23

### Fixed
- Remove stale Tier-1/2/3 language labels — all plugins use unified GraphBuilder
- Comprehensive README rewrite with all 15 commands, 12 settings, organized by feature category
- Remove "Linters" from marketplace categories

## [4.11.0] - 2026-03-23

### Added
- **Status bar item** — persistent index health indicator (Ready/Stale/Building/No Index/Error)
- **Keyboard shortcuts** — Ctrl+Alt+S (search), Ctrl+Alt+Q (query), Ctrl+Alt+R (references), Ctrl+Alt+I (index)
- **Search history** — recall and re-run last 20 queries via Sqry: Search History command
- **Getting Started walkthrough** — 5-step onboarding for new users
- **Problems panel integration** — unused code, cycles, and duplicates as native VS Code diagnostics
- **Inline unused code fading** — unused symbols rendered as dimmed text via DiagnosticTag.Unnecessary
- **Quick fixes** — code actions for show callers, show cycle path, navigate to duplicate
- **Hover integration** — caller/callee counts in editor hover tooltips
- **Enhanced CodeLens** — configurable callers + callees segments with batch endpoint
- **Call graph webview** — interactive SVG visualization of callers/callees with pan/zoom and export
- **Dependency graph webview** — cross-language relationship visualization
- **Multi-root workspace support** — per-root index status, targeting, and aggregate status bar
- **Result filtering** — filter by language and symbol kind via QuickPick
- **Result sorting** — sort by name, file, kind, or line number
- **Export results** — JSON, Markdown, or CSV export to untitled editor
- **Auto-index on save** — optional debounced rebuild with dirty latch (sqry.autoIndexOnSave setting)
- **Restart Language Server** command
- **Rebuild Index** context menu on index status items
- **Scan Workspace** command for full diagnostic scan

### Fixed
- Index age display now shows modification time instead of file birth time

## [4.10.12] - 2026-03-22

### Changed
- **Version aligned with sqry v4.10.12**

## [4.10.11] - 2026-03-22

### Changed
- **Promoted to top-level directory** — `sqry-vscode/` is now a first-class deliverable alongside other `sqry-*` crates (previously under `tools/`)

## [4.10.10] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.10** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.9] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.9** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.8] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.8** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.7] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.7** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.6] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.6** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.5] - 2026-03-21

### Changed
- **Version aligned with sqry v4.10.5** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.4] - 2026-03-20

### Changed
- **Version aligned with sqry v4.10.4** - extension packaging and documentation references updated to match the current CLI/LSP/MCP release train

## [4.10.2] - 2026-03-19

### Added
- **LSP**: Real cross-language edge counting in `index_status` — previously returned 0, now iterates graph edges to report accurate per-language-pair counts
- **VSCode**: Index stats auto-refresh after workspace rebuild completes
- **VSCode**: Truncation indicator for unused symbols panel — shows "N+ symbols" and info item when results are limited

### Changed
- **VSCode**: Analysis predicates (Duplicates, Circular Dependencies, Unused Code) now show "expand to check" instead of "0" or "none" before data is loaded
- **VSCode**: Removed redundant tree view refreshes from lazy-loading panels (symbols, files, cross-language relations) for better performance

### Fixed
- **LSP**: Debug logging for silent node conversion drops — logs when symbol name, file path, or URI resolution fails instead of silently returning `None`
- **LSP**: Cross-language relation listing now uses shared `is_cross_language_edge_kind` predicate, ensuring count and list endpoints stay in sync
- **LSP**: Language comparison uses typed enum equality instead of case-insensitive string comparison

## [4.10.1] - 2026-03-15

### Fixed
- **LSP**: Platform-aware workspace root in URI construction tests (Windows compatibility)
- **MCP**: Canonicalize workspace root for macOS `/var` symlink
- **Release**: Bundled ONNX models for nl-classifier, improved cross-platform build reliability (zigbuild, musl C++, macOS signing)

## [4.10.0] - 2026-03-13

### Changed
- **Version aligned with sqry v4.10.0** - documentation, install guidance, and packaging references updated to match the current CLI/LSP/MCP release train

## [4.9.2] - 2026-03-09

### Fixed
- **MCP Tools**: Fix `call_hierarchy` regression — indexed name lookup, definition-kind filtering, single-root correctness
- **MCP Tools**: Fix `find_cycles` on large codebases — in-memory CSR build removes `sqry analyze` dependency
- **MCP Tools**: Fix `find_cycles` Modules cycle type using wrong edge kind

## [4.9.1] - 2026-03-09

### Fixed
- **MCP Tools**: Suffix matching for `direct_callers`, `direct_callees`, `get_hover_info`, `get_references` — unqualified symbol names now resolve correctly

### Performance
- **Analysis**: FastBitSet optimization for 2-hop label computation
- **Graph**: `sqry index --force` no longer hangs on medium codebases

## [4.8.17] - 2026-03-06

### Changed
- **Version aligned with sqry v4.8.17** - release packaging, installer guidance, and OCI distribution docs updated to match the current public release flow

## [4.8.5] - 2026-03-04

### Fixed
- **Version aligned with sqry v4.8.5**

## [4.8.4] - 2026-03-04

### Fixed
- **Version aligned with sqry v4.8.4**

## [4.8.3] - 2026-03-03

### Fixed
- **Security**: Upgrade serialize-javascript to fix RCE vulnerability (GHSA-5c6j-r48x-rmvq)
- **Version aligned with sqry v4.8.3**

## [4.8.2] - 2026-03-03

### Added
- **Version aligned with sqry v4.8.2** - ARM64 builds and package manager distribution support

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

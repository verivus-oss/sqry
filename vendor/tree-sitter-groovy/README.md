# Tree-Sitter Groovy Grammar (Vendored)

**Upstream**: https://github.com/murtaza64/tree-sitter-groovy
**Commit**: 86911590a8e46d71301c66468e5620d9faa5b6af
**Date**: 2025-01-22
**License**: Apache-2.0

## Update Procedure

1. Update grammar.js from upstream
2. Run `tree-sitter generate --abi=14`
3. Copy/verify `src/parser.c`, `src/scanner.c`, `src/node-types.json` are generated
4. Test: `cargo test -p tree-sitter-groovy-sqry`
5. Update this README with new commit hash
6. Commit all changes

See `../../docs/development/tree-sitter-bindings/02_DESIGN.md` Section 3.2 for details.

# Tree-Sitter Svelte Grammar (Vendored)

**Upstream**: https://github.com/Himujjal/tree-sitter-svelte
**Commit**: 60ea1d673a1a3eeeb597e098d9ada9ed0c79ef4b
**Date**: 2024-09-06
**License**: MIT

## Update Procedure

1. Update grammar.js from upstream
2. Run `tree-sitter generate --abi=14`
3. Copy/verify `src/parser.c`, `src/scanner.c`, `src/node-types.json` are generated
4. Test: `cargo test -p tree-sitter-svelte-sqry`
5. Update this README with new commit hash
6. Commit all changes

See `../../docs/development/tree-sitter-bindings/02_DESIGN.md` Section 3.2 for details.

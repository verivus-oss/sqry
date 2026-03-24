# Tree-Sitter Vue Grammar (Vendored)

**Upstream**: https://github.com/ikatyang/tree-sitter-vue
**Commit**: 91fe2754796cd8fba5f229505a23fa08f3546c06
**Date**: 2021-04-04
**License**: MIT

## Update Procedure

1. Update grammar.js from upstream
2. Run `tree-sitter generate`
3. Copy/verify `src/parser.c`, `src/scanner.cc`, `src/node-types.json` are generated
4. Test: `cargo test -p tree-sitter-vue-sqry`
5. Update this README with new commit hash
6. Commit all changes

See `../../docs/development/tree-sitter-bindings/02_DESIGN.md` Section 3.2 for details.

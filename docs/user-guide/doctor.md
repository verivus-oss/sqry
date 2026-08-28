# Doctor

`sqry doctor` is a read-only installation diagnostic. It never mutates config, index, or daemon state.

## Channels

```bash
sqry doctor channels
sqry doctor channels --json
```

`sqry doctor channels` resolves the stable toolchain (`sqry`, `sqry-mcp`, `sqry-lsp`, `sqryd`) and the dev (`-d` wrapper) toolchain, then cross-checks MCP config keys, daemon socket paths, and plugin rosters. It reports mixed-channel conditions and exits non-zero when a mismatch is detected.

Use this after installing both a release build and a source/dev build on the same machine, or when MCP clients appear to talk to a different binary than `sqry --version`.

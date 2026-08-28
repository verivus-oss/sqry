# Context Propagation

`sqry context-propagation` finds Go call sites where `context.Context` is available but not threaded into a callee that accepts it. MCP clients use `context_propagation`. This tool is standalone-only; it is not in the daemon-hosted 17-tool subset.

## Modes

| Mode | Meaning |
| --- | --- |
| `break-site` | Sync caller has a `context.Context` parameter and the callee accepts ctx, but the call passes none |
| `unthreaded-goroutine` | `go callee(...)` where the callee accepts ctx and none is passed |
| `http-handler-leak` | `http.HandlerFunc`-shaped caller that does not thread `r.Context()` |

Default `--mode all` reports every class.

## Usage

```bash
sqry context-propagation .
sqry context-propagation . --mode break-site --json
sqry context-propagation . --scope file:src/handler.go --limit 50
```

Exit code 0 includes the empty-finding case (`no context-propagation leaks`). Invalid `--scope` or `--mode` is exit 2. A missing index is exit 3.

Planner users who want wrap-chain edges rather than ctx leaks should use `sqry plan-query "kind:function wraps"` instead. That is a different analysis (`EdgeKind::Wraps`).

Full flag and JSON field notes: [`docs/cli/context-propagation.md`](../cli/context-propagation.md).

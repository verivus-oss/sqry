# Explore Caller Counts with CodeLens

sqry adds **CodeLens annotations** above every function and method in your source files.
These show how many other symbols call each function — at a glance, without searching.

## What You See

Above each function definition you will see something like:

```
3 callers
fn handle_request(req: Request) -> Response {
```

The number reflects how many callers sqry has found across your entire workspace,
spanning all languages and files.

## Click to Explore

Clicking the caller count opens the callers list in the Sqry sidebar panel. You can
then click any caller to jump to its definition.

This is especially useful for:

- **Understanding impact**: before changing a function, see who depends on it
- **Finding entry points**: functions with 0 callers are likely entry points or dead code
- **Tracing call chains**: follow callers up the call stack interactively

## Enable or Disable

CodeLens is enabled by default. Toggle it with the `sqry.codeLens.enabled` setting:

- Open VS Code settings (`Ctrl+,`)
- Search for `sqry.codeLens`
- Uncheck `Sqry: Code Lens Enabled` to turn it off

You can also toggle it per workspace or per language via workspace/folder settings.

## Note on Index Freshness

Caller counts reflect the last indexed state of your workspace. If you add or remove
callers, re-index (`Ctrl+Alt+I`) to update the counts.

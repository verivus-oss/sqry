# Elixir NIF Test Fixtures

This directory contains test fixtures for Elixir NIF (Native Implemented Function) detection.

## Files

- `example.ex` - Example module with NIF loading
- `example_nif.so` - (Not included) Native library would go here

## Pattern Detection

The sqry Elixir plugin detects these FFI patterns:

### Primary Pattern

`:erlang.load_nif/2` calls - the core NIF loading function.

### Optional Patterns (Enhance Detection)

- `@on_load` - Module attribute for auto-loading
- `:erlang.nif_error/1` - Stub function pattern

**Important**: Detection does NOT require `@on_load` or stubs. The presence of `:erlang.load_nif/2` alone is sufficient.

## Manual Validation

### Compile and Load

```bash
# Option 1: Using IEx (Interactive Elixir)
iex example.ex

# Option 2: Compile first, then load
elixirc example.ex
iex
iex(1)> c("example.ex")

# Option 3: In a Mix project
iex -S mix
iex(1)> c("test-fixtures/ffi/elixir/example.ex")
```

### Expected Behavior

When loading `example.ex`, the `@on_load` hook will execute and attempt to load the NIF:
```
** (File.Error) could not read file "priv/example_nif.so": no such file or directory
```

This error is expected - it confirms the NIF loading code executes correctly. The `.so` file doesn't exist in test fixtures, but the Elixir code is syntactically and semantically valid.

### Verify Module Structure

```elixir
iex(1)> h ExampleNIF
# Shows module documentation

iex(2)> ExampleNIF.module_info
# Shows compiled module information
```

## References

- [Erlang NIF Guide](https://www.erlang.org/doc/system/nif.html)
- [Elixir Interoperability](https://hexdocs.pm/elixir/library-guidelines.html#avoid-using-macros)

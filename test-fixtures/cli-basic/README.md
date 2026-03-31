# CLI Basic Test Fixture

This is a minimal test fixture used by `sqry-cli/tests/cli_basic_test.rs` for basic CLI integration tests.

## Structure

- `main.rs` - Contains simple functions, structs, and method implementations
- `lib.rs` - Contains a module with utility functions, traits, and implementations
- `.sqry/graph/snapshot.sqry` - Prebuilt graph index for queries

## Contents

### Functions (9 total)

From `main.rs`:
- `calculate_sum(a: i32, b: i32) -> i32` - Public function
- `multiply(x: i32, y: i32) -> i32` - Public function
- `main()` - Entry point
- `Calculator::new(initial: i32) -> Self` - Constructor
- `Calculator::add(&mut self, n: i32)` - Method
- `Calculator::get_value(&self) -> i32` - Method

From `lib.rs`:
- `utils::subtract(a: i32, b: i32) -> i32` - Module function
- `utils::divide(a: i32, b: i32) -> i32` - Module function
- `DefaultProcessor::process(&self)` - Trait method implementation

## Used By

- `test_simple_query()` - Tests basic `kind:function` query
- `test_query_with_verbose()` - Tests verbose flag with queries
- `test_successful_query_exit_code_zero()` - Tests exit code 0 on success

## Rebuilding the Index

If the `.sqry` directory is deleted or corrupted, rebuild it with:

```bash
sqry index /srv/repos/internal/verivus-oss/sqry/test-fixtures/cli-basic
```

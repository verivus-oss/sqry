# Repository overview

## Hubs (load-bearing symbols)

| # | Symbol | Kind | fan-in | fan-out | Location |
|---|--------|------|--------|---------|----------|
| 1 | normalize | Function | 2 | 2 | core/src/lib.rs:10 |
| 2 | handle_one | Function | 1 | 6 | api/src/lib.rs:4 |
| 3 | helper_a | Function | 1 | 3 | core/src/lib.rs:26 |
| 4 | helper_b | Function | 1 | 1 | core/src/lib.rs:30 |
| 5 | transform | Function | 1 | 6 | core/src/lib.rs:18 |
| 6 | validate | Function | 1 | 1 | core/src/lib.rs:14 |
| 7 | dead_public_api | Function | 0 | 2 | core/src/lib.rs:35 |
| 8 | dispatch | Function | 0 | 3 | api/src/lib.rs:20 |
| 9 | engine_run | Function | 0 | 3 | core/src/lib.rs:4 |
| 10 | handle_two | Function | 0 | 6 | api/src/lib.rs:12 |

Next: `sqry impact "normalize"`

## Subsystems (by path/package)

| # | Subsystem | Symbols | Internal edges | Representative |
|---|-----------|---------|----------------|----------------|
| 1 | core/src | 7 | 35 | normalize |
| 2 | api/src | 3 | 31 | handle_one |

**Couplings** (sparse-but-high-fan first)

_No cross-subsystem couplings found._

Next: `sqry graph subsystems`


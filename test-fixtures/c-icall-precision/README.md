# `test-fixtures/c-icall-precision/`

Committed test corpus for the **C indirect-call precision** work
(Phase A) — the feature whose deliverables, design, and acceptance
criteria live under
`docs/development/c-semantic-phase-a-icall-precision/`. The fixtures
here are the single source of truth for every Phase A success
criterion that mentions a `.c` file path: U12 integration tests, U17
cap calibration measurements, U19 performance benches, and the
`measure_indirect_fanout` example all consume this tree.

## Layout

```
test-fixtures/c-icall-precision/
├── README.md                              this file
├── EXPECTED.md                            master expected-resolution matrix
├── address-taken-patterns/                one .c file per SPEC §3.1.1 row
│   ├── unary_amp.c                        &my_func
│   ├── argument_pass.c                    register_callback(my_func)
│   ├── designated_init.c                  { .read = my_read }
│   ├── positional_init.c                  { my_read, NULL }
│   ├── field_assign.c                     fops->read = my_read;
│   ├── subscript_assign.c                 table[0] = my_func;
│   ├── return_function.c                  return my_func;
│   ├── init_declarator.c                  int (*fp)(int) = my_func;
│   └── nonfunction_taken.c                NEGATIVE — &g_int, must NOT flag
├── linux-driver-subset/                   vendored Linux 6.10.5 ext4 subset
│   ├── README.md                          provenance, SHA pin, license posture
│   ├── EXPECTED-bindings.md               expected binding-plane resolutions
│   ├── file.c                             SPDX: GPL-2.0   (vendored)
│   ├── dir.c                              SPDX: GPL-2.0   (vendored)
│   ├── symlink.c                          SPDX: GPL-2.0   (vendored)
│   ├── verity.c                           SPDX: GPL-2.0   (vendored — has indirect callsites)
│   ├── fsmap.c                            SPDX: GPL-2.0+  (vendored — has indirect callsites)
│   └── ext4_dispatch_harness.c            SPDX: MIT       (synthetic, NOT vendored)
└── ebpf-struct-ops/                       hand-written eBPF cross-check
    ├── tcp_cubic_ops.c                    synthetic — NOT vendored
    └── expected.json                      machine-readable expectations
```

## License posture (READ THIS)

The `address-taken-patterns/` files and the `ebpf-struct-ops/`
fixtures are hand-written by Verivus contributors and carry an MIT
SPDX header consistent with the rest of the sqry workspace.

The `linux-driver-subset/` files (`file.c`, `dir.c`, `symlink.c`,
`verity.c`, `fsmap.c`) are **vendored verbatim from the Linux kernel**
(release tag `v6.10.5` on `linux-stable` at kernel.org). They retain
their original SPDX headers: `GPL-2.0` for `file.c` / `dir.c` /
`symlink.c` / `verity.c`, and `GPL-2.0+` for `fsmap.c`. The sqry source tree is
permissive-licensed (MIT) at the crate level; the vendored GPL-2.0
fixture is isolated under `test-fixtures/` and:

- is **not compiled into any sqry crate or library output**;
- exists **solely as test input** for the indirect-call precision
  suite and the per-callsite fan-out measurement;
- is **excluded from published `crates.io` packages** via the
  workspace `release-manifest.toml` selection rules (see the
  `verivus-oss/sqry` distribution pipeline).

In short: the GPL header is intentional, must be preserved verbatim,
and triggers no licence contamination of sqry itself because the file
is never linked into a shipped binary.

## How the fixtures are exercised

- **U12 integration tests** (`sqry-core/tests/`): every fixture under
  `address-taken-patterns/` is consumed by a `pass5b_resolves_*`
  integration test that asserts the captured `BindingEntry`s and
  address-taken marks match `EXPECTED.md`. The negative case
  (`nonfunction_taken.c`) asserts the address-taken set is empty.
- **U16 measurement tooling**: `cargo run --example
  measure_indirect_fanout -- test-fixtures/c-icall-precision/linux-driver-subset/`
  builds a sqry CodeGraph, walks every captured indirect callsite,
  computes the raw type-match candidate count without binding-plane
  refinement (DESIGN §5.1 step 2), and emits one integer per
  callsite to stdout. Piping into
  `scripts/measure/icall_fanout_histogram.py` produces the p50 /
  p75 / p90 / p95 / p99 / max histogram that calibrates the
  cardinality cap in `sqry-core/src/graph/unified/build/pass5b_c_indirect.rs`.
- **U17 cap calibration**: commits the histogram output under
  `docs/development/c-semantic-phase-a-icall-precision/measurements/`.
- **U19 performance benches**: `sqry-lang-c/benches/c_indirect.rs`
  uses `linux-driver-subset/` as the `bench_full_build_linux_fs_subset`
  corpus.

## Why this tree exists

Without committed fixtures every Phase A success criterion is
unfalsifiable. The Linux 6.10.5 ext4 subset is bounded to ≤2 KLOC so
measurement is fast (<5s) and reproducible across CI runs, and the
eBPF struct-ops cross-check catches over-fitting to ext4-specific
idioms (DESIGN §13.3). The hand-written `address-taken-patterns/`
files provide one minimal, unambiguous AST per classification rule in
SPEC §3.1.1 — they are the unit-level evidence base behind the
binding-classifier walker in `sqry-lang-c::relations::graph_builder`.

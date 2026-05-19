# `c-icall-precision` — master expected-resolution matrix

This is the **single source of truth** that downstream Phase A units
(U12 integration tests, U17 cap calibration, U19 performance gates)
cross-reference. Every row maps a fixture site to the exact sqry
post-`pass5b` outcome a correct implementation produces.

Schema:

- **Address-taken set** rows: one row per (`fixture file`, `expected
  address-taken function`).
- **Binding-plane** rows: one row per (`fixture file`, `(struct_qn,
  field_name)`, `target_fn`, `site_kind`).
- **Resolution outcome** rows: one row per indirect callsite in
  `linux-driver-subset/` describing the expected `ResolvedVia` and
  candidate count after pass5b.

The terminology mirrors `sqry-core/src/graph/unified/storage/c_indirect.rs`
(`BindingEntry`, `BindingSiteKind`) and
`sqry-core/src/graph/unified/build/pass5b_c_indirect.rs` (`ResolvedVia`,
`ResolutionOutcome`).

---

## 1. `address-taken-patterns/` (SPEC §3.1.1 row coverage)

| Fixture | SPEC §3.1.1 row | Expected address-taken function | Binding entry produced? | Notes |
|---------|-----------------|---------------------------------|-------------------------|-------|
| `unary_amp.c` | Unary `&` on function identifier | `my_func` | No | `&my_func` assignment to file-scope handler pointer; no struct slot, no `BindingEntry`. |
| `argument_pass.c` | Function identifier in arg position | `my_func` | No | `register_callback(my_func)` is an argument-pass site, not a struct binding. |
| `designated_init.c` | Designated initializer `.field = fn` | `my_read` | Yes — `(struct ops, read) → my_read [DesignatedInitializer]` | Canonical struct-ops binding shape. |
| `positional_init.c` | Positional initializer slot of fn-ptr type | `my_read` | Yes — `(struct ops, read) → my_read [PositionalInitializer]` | The trailing `NULL` slot for `.write` is NOT a binding (only function-identifier RHS produces a `BindingEntry`). |
| `field_assign.c` | RHS of `field_expression =` assignment | `my_read` | No (DESIGN §7 / SPEC §3.3.2 explicitly exclude runtime assignment from the binding plane) | Address-taken-only — no `BindingEntry`. |
| `subscript_assign.c` | RHS of `subscript_expression =` assignment | `my_func` | No (same exclusion as field_assign) | Address-taken-only. |
| `return_function.c` | Function identifier in value position (`return fn`) | `my_func` | No | Argument-/return-style address-take. |
| `init_declarator.c` | Function identifier as init-declarator initializer | `my_func` | No | Local `int (*fp)(int) = my_func` triggers address-taken but stores no binding plane entry (the variable is not a struct slot). |
| `nonfunction_taken.c` | NEGATIVE control | *(none — address-taken set is empty)* | No | Taking `&some_var` MUST NOT flag any function. |

## 2. `linux-driver-subset/` — binding plane (designated initializer instances)

Every row below is the precise output the C plugin must stage and
sqry-core's Phase 4 must merge into `bindings_by_field`. Cross-file
resolution is REQUIRED — the U12 integration tests build all three
files as one workspace and assert the merged `bindings_by_field`
contains every row.

The detailed per-line table lives in
`linux-driver-subset/EXPECTED-bindings.md`. The roll-up below
summarises by struct-type and instance:

| Instance | Struct type | Source file | Field count | Notes |
|----------|-------------|-------------|-------------|-------|
| `ext4_dax_vm_ops` | `vm_operations_struct` | `file.c` | 4 | DAX fault path; `.fault` and `.page_mkwrite` both bind `ext4_dax_fault` (duplicate target across slots — DESIGN §3.3.2 explicitly admits this). |
| `ext4_file_vm_ops` | `vm_operations_struct` | `file.c` | 3 | Non-DAX fault path. Same struct type as `ext4_dax_vm_ops`; binding plane must disambiguate by instance. |
| `ext4_file_operations` | `file_operations` | `file.c` | 14 (2 CONFIG-gated) | Primary file-ops table. |
| `ext4_file_inode_operations` | `inode_operations` | `file.c` | 8 | Inode ops for the regular-file `inode_operations` slot. |
| `ext4_dir_operations` | `file_operations` | `dir.c` | 7 (1 CONFIG-gated) | Multi-instance: `file_operations` is bound twice (here + `ext4_file_operations`). Per SPEC §3.3 / DESIGN §13.2, the binding plane MUST narrow `instance->read(...)` to the correct per-instance target. |
| `ext4_encrypted_symlink_inode_operations` | `inode_operations` | `symlink.c` | 4 | Sibling inode-ops; different `.get_link` than the next two. |
| `ext4_symlink_inode_operations` | `inode_operations` | `symlink.c` | 4 | |
| `ext4_fast_symlink_inode_operations` | `inode_operations` | `symlink.c` | 4 | The three symlink tables share field shape but bind different `.get_link` targets — the binding plane is the sole disambiguator between them. |
| `ext4_verityops` | `fsverity_operations` | `verity.c` | 5 | Standalone struct-ops table; no co-instances on this corpus. Designated initializer only. |

Plus `verity.c` and `fsmap.c` contribute real indirect callsites
(`aops->write_begin(...)` at verity.c:83 / 89; `info->gfi_formatter(...)`
at fsmap.c:149 / 171) that the measurement binary captures as
`IndirectCallsite { shape: FieldExpr }`. They have no associated
binding-plane entries because neither struct is defined within the
bounded corpus.

The MIT-licensed `ext4_dispatch_harness.c` is the file that drives
the measurement signal: it declares minimal stand-ins for
`struct file_operations`, `struct inode_operations`, and
`struct vm_operations_struct`, and contains 13 dispatcher functions
whose `obj->slot(...)` callsites are the population the measurement
histogram counts. See `linux-driver-subset/EXPECTED-bindings.md`
for the per-dispatcher slot/candidate-count breakdown.

CONFIG-gated rows (`#ifdef CONFIG_COMPAT`) produce a `BindingEntry`
for every visible branch — pass5b does NOT evaluate `cfg` conditions
in Phase A. Both alternatives are recorded.

## 3. `linux-driver-subset/` — expected indirect-callsite resolution shape

The exact per-callsite outcomes are derived empirically by U17's
calibration run. This table records the *kinds* of outcomes pass5b
must demonstrate on this corpus, not their exact counts (which are
fixture-dependent and version-pinned in
`docs/development/c-semantic-phase-a-icall-precision/measurements/`):

| Outcome | Expected non-zero on this corpus? | Notes |
|---------|------------------------------------|-------|
| `BindingPlane` | Yes | Anywhere a receiver's struct-type is locally recoverable and matches an in-tree binding. |
| `TypeMatch` | Yes | Fallback when receiver type is locally unknown (`struct file *f` passed in, etc.). |
| `CapExceeded` | No (within ext4 subset alone; the cap is calibrated above this corpus's p99 — DESIGN §5.2) | `CALLSITE_PROMISCUOUS` flag should not fire on this corpus. |
| `FallbackToStub` | Yes | Callees declared in unincluded headers (`thp_get_unmapped_area`, etc.) leave the original synthetic stub in place. |

## 4. `ebpf-struct-ops/`

Expectations are encoded in machine-readable form at
`ebpf-struct-ops/expected.json`. Summary:

| Instance | Struct type | Designated bindings | Positional bindings | Total `BindingEntry`s |
|----------|-------------|---------------------|---------------------|------------------------|
| `cubictcp` | `tcp_congestion_ops` | 7 | 0 | 7 |

This fixture is the cross-check that the binding plane works against a
NON-ext4 idiom (DESIGN §13.3) — pure designated-initializer shape, no
CONFIG gates, no positional-NULL fields participating as bindings.

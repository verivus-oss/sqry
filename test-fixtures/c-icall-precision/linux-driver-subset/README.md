# Linux ext4 driver subset — Phase A measurement corpus

This directory holds a small, version-pinned subset of the Linux kernel's
`fs/ext4/` source tree. It is the canonical measurement corpus for the
sqry C indirect-call precision work (Phase A) per DESIGN §5.1 and §13.1.

## Upstream provenance

- **Source repository:** `git://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git`
  (the `linux-stable` tree on kernel.org).
- **Release tag:** `v6.10.5` (Linux 6.10.5 stable, released 2024-08-11).
- **Pinned commit SHA:** `b9a04d77a09a47547d70878d80147eb2b40e3afc`
  (the commit pointed at by `refs/tags/v6.10.5` in `linux-stable`).
- **Local cross-check:** the `VERSION = 6`, `PATCHLEVEL = 10`, `SUBLEVEL = 5`
  fields at the top of the upstream `Makefile` identify the release; the
  vendored files were taken from that exact tree without modification.

If the upstream stable tree is ever rebased (which it is not, by policy),
re-pin to whichever commit `refs/tags/v6.10.5` resolves to at that point —
the tag itself is the contract, the SHA above is a convenience cross-check.

## Files vendored

| Path | Upstream path | LOC | Reason |
|------|---------------|-----|--------|
| `file.c` | `fs/ext4/file.c` | 960 | Defines `ext4_file_operations`, `ext4_file_inode_operations`, `ext4_dax_vm_ops`, `ext4_file_vm_ops` (4 struct-ops tables, designated initializers, every Phase A motivator from DESIGN §13.2). |
| `dir.c` | `fs/ext4/dir.c` | 677 | Defines `ext4_dir_operations` (multi-instance binding sibling to `ext4_file_operations`). |
| `symlink.c` | `fs/ext4/symlink.c` | 136 | Three `inode_operations` instances sharing field shape with different callbacks (`get_link` slot differentiates `ext4_encrypted_get_link` / `ext4_get_link` / `simple_get_link`) — exercises the binding plane's per-instance disambiguation requirement (SPEC §3.3). |
| `verity.c` | `fs/ext4/verity.c` | 397 | Carries real `field_expression` indirect callsites (`aops->write_begin(...)` / `aops->write_end(...)` at lines 83 / 89) that exercise the type-match resolver at measurement time. |
| `fsmap.c` | `fs/ext4/fsmap.c` | 719 | Carries real `field_expression` indirect callsites (`info->gfi_formatter(...)` at lines 149 / 171), a function-pointer-typed struct field declared in the same TU — pure intra-file path for the resolver. SPDX `GPL-2.0+`. |
| **Vendored subtotal** | | **2889** | Vendored verbatim from upstream `linux-stable` at the pinned tag. |

Plus one MIT-licensed synthetic file that lives alongside the vendored
sources but is NOT part of the upstream kernel:

| Path | License | LOC | Reason |
|------|---------|-----|--------|
| `ext4_dispatch_harness.c` | MIT (Verivus-written) | ~160 | DESIGN §13.1's `ext4_fops_table.c` companion. Declares minimal stand-ins for the kernel structs whose definitions live in `<linux/fs.h>` (`struct file_operations`, `struct inode_operations`, `struct vm_operations_struct`) — without these declarations the C plugin's struct walker has no `struct_field_fnptr` entries to compare against, and the measurement binary would emit zero usable signal. Carries synthetic dispatcher functions that invoke each slot through a receiver of known declared type so `IndirectCallsite` capture fires. The harness's MIT SPDX header is intentional — it is fixture-only Verivus-authored code and never `#include`s a vendored GPL-2.0 file. |

**Grand total**: ~3,050 LOC, comfortably under DESIGN §13.2's ≤5 KLOC budget.

## License posture

The vendored files retain their original SPDX headers verbatim: four of
the five carry `SPDX-License-Identifier: GPL-2.0` (`file.c`, `dir.c`,
`symlink.c`, `verity.c`), and one carries `SPDX-License-Identifier:
GPL-2.0+` (`fsmap.c` — the `+` denoting "or any later version" per
upstream's choice for that file). This is fixture-only material: it is
**not** compiled into any sqry crate or published binary, lives strictly
under `test-fixtures/`, and exists solely as test input for the
indirect-call precision suite.

The sqry workspace itself is licensed MIT — see the top-level
`test-fixtures/c-icall-precision/README.md` for the full license-posture
note and the release-manifest exclusion that guarantees this subtree is
not redistributed in any sqry crate package on crates.io.

## Tooling that consumes this directory

- `cargo run --example measure_indirect_fanout -- test-fixtures/c-icall-precision/linux-driver-subset/`
  builds a sqry CodeGraph over this subset and emits per-callsite
  type-match candidate counts on stdout (DESIGN §5.1 step 2).
- `scripts/measure/icall_fanout_histogram.py` consumes that output and
  emits p50 / p75 / p90 / p95 / p99 / max percentiles. The histogram is
  committed under
  `docs/development/c-semantic-phase-a-icall-precision/measurements/`
  by U17_CAP_CALIBRATION.

See `EXPECTED-bindings.md` for the hand-labelled expected resolution
matrix that U12 integration tests assert against.

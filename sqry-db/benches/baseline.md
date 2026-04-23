# PN4 Benchmark Baseline

**Date:** 2026-04-16 00:28:13 UTC
**Worktree:** `<WORKSPACE_ROOT>/.worktrees/pn4-fusion`
**Branch:** `feat/pn4-structural-subset-fusion`
**Command:** `/usr/bin/time -v cargo bench -p sqry-db --bench fusion_bench`

## Scope

This is the pre-PN4 baseline captured before any production changes to:

- `sqry-db/src/planner/fuse.rs`
- `sqry-db/src/planner/execute.rs`

The harness records:

- a required realistic 100-plan mixed batch scaled deterministically from
  `realistic_mixed_batch_with_chain_setop_and_standalone_scan`
- an execution-level 100-plan overlapping-subtree batch that PN4 is expected
  to improve materially

## Wall-Clock Baseline

### `planner_fuse/realistic_mixed_batch/100`

- Criterion time: `10.275 µs` to `19.016 µs`
- Criterion median: `13.464 µs`
- Throughput median: `7.4272 Melem/s`

### `planner_execute_batch/overlapping_subtree_batch/100`

- Criterion time: `3.2363 ms` to `3.6411 ms`
- Criterion median: `3.3769 ms`
- Throughput median: `29.613 Kelem/s`

## Memory / Structural Baseline

- Maximum resident set size (real process metric): `437416 kbytes`
- Realistic mixed batch fused postcard bytes: `2212`
- Realistic mixed batch structural operator count: `300`
- Overlapping-subtree batch fused postcard bytes: `3688`
- Overlapping-subtree batch structural operator count: `400`

## Notes

- `structural_operator_count` and fused postcard bytes are structural proxies,
  not heap-allocation measurements.
- The `Maximum resident set size` value comes from `/usr/bin/time -v` for the
  full benchmark process and is the real memory number used for baseline
  comparison.

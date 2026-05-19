#!/usr/bin/env python3
"""Per-callsite indirect-call fan-out histogram (Phase A, DESIGN §5.1).

Consumes the output of
`cargo run --example measure_indirect_fanout` — one integer per line
on stdin, each integer being the raw type-match candidate count for
one captured indirect callsite. Emits the p50 / p75 / p90 / p95 / p99 /
max percentiles plus a count of samples on stdout.

Lines that fail to parse as integers are skipped silently (so blank
trailing lines from cargo do not blow up the run). The script
deliberately uses only the Python 3 standard library so it works in
hermetic CI environments without third-party deps.

The output format is contractual — U17_CAP_CALIBRATION commits this
verbatim under
`docs/development/c-semantic-phase-a-icall-precision/measurements/`
and downstream gates may parse it. Each line:

    <label>: <integer>

with labels exactly `p50`, `p75`, `p90`, `p95`, `p99`, `max`, `count`,
in that fixed order. A separator and a TOTALS section can be added by
future units (U17 may amend) but the existing seven lines must remain
unchanged.
"""

from __future__ import annotations

import math
import sys
from collections.abc import Iterable


def parse_samples(lines: Iterable[str]) -> list[int]:
    """Parse non-negative integers, one per line. Skip blanks and
    non-integer lines silently (defensive against cargo's stderr
    leaking into a redirected stream)."""
    out: list[int] = []
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        try:
            value = int(stripped)
        except ValueError:
            continue
        if value < 0:
            continue
        out.append(value)
    return out


def percentile(sorted_samples: list[int], p: float) -> int:
    """Nearest-rank percentile (per the NIST-recommended C=1 method).

    For a sorted vector `s` of length `n` and percentile `p` in [0, 100]:

        rank = ceil(p/100 * n)
        result = s[rank - 1]   (1-indexed -> 0-indexed)

    Returns 0 when `sorted_samples` is empty. This matches the
    convention used by every other histogram tool in `scripts/` and
    keeps the output stable across hosts.
    """
    n = len(sorted_samples)
    if n == 0:
        return 0
    if p <= 0:
        return sorted_samples[0]
    if p >= 100:
        return sorted_samples[-1]
    rank = math.ceil((p / 100.0) * n)
    # rank is in [1, n] by construction above (p in (0, 100), n >= 1).
    return sorted_samples[rank - 1]


def main(argv: list[str]) -> int:
    if any(arg in {"-h", "--help"} for arg in argv[1:]):
        print(__doc__)
        return 0

    samples = parse_samples(sys.stdin)
    samples.sort()
    n = len(samples)

    p50 = percentile(samples, 50)
    p75 = percentile(samples, 75)
    p90 = percentile(samples, 90)
    p95 = percentile(samples, 95)
    p99 = percentile(samples, 99)
    pmax = samples[-1] if samples else 0

    # Stable, line-oriented output. Order is contractual — see the
    # module docstring above.
    print(f"p50: {p50}")
    print(f"p75: {p75}")
    print(f"p90: {p90}")
    print(f"p95: {p95}")
    print(f"p99: {p99}")
    print(f"max: {pmax}")
    print(f"count: {n}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

//! Same-commit marginal build-time measurement for Phase A C indirect-call
//! precision (verivus-oss/sqry#280).
//!
//! This is the data source for `scripts/measure/check_phase_a_perf_gate.sh`.
//! It measures the marginal cost of Phase A by building the same fixture with
//! Phase A on and off at a single commit, many times, and reporting the
//! median of the PER-PAIR ratio `(with_i - without_i) / without_i` (see
//! "Estimator" below for why per-pair, not per-arm).
//!
//! ## Why a paired, alternating loop instead of two criterion benches
//!
//! Phase A's marginal is roughly 1 ms on a roughly 12 ms full build (order
//! 8 to 10 percent). Two separate criterion benchmarks of the whole build,
//! measured tens of seconds apart, each carry 5 to 15 percent run-to-run
//! variance from CPU frequency scaling, thermal drift, page-cache state, and
//! host contention. Differencing two such means buries the Phase A signal in
//! that common-mode noise (observed marginals swung from -1 to +19 percent
//! across repeated runs). This binary instead measures the two builds
//! back-to-back, microseconds apart, for many iterations, alternating which
//! build runs first each pair. Slow drift then affects both halves of a pair
//! almost equally and cancels in the paired difference, so the small marginal
//! is resolvable and the result is reproducible on any reasonably quiet host.
//!
//! ## What is measured (floor arm)
//!
//! "With" is the full build; "without" (the floor) skips only the three Phase A
//! C-plugin instrumentation walks (address-taken classification, the local
//! scope index, and the known-function-names leg) via
//! `sqry_lang_c::CPlugin::without_phase_a`. The callsite capture and the core
//! `pass5b_c_indirect` pass run in both arms, so the marginal isolates the
//! SPEC §5.2 "C plugin build time" cost, matching PR #351's floor method.
//!
//! ## Estimator: median of the per-pair ratio (ENFORCED), with per-arm
//! medians and min as cross-checks
//!
//! The statistic the gate ENFORCES is the median, over all paired
//! iterations, of the PER-PAIR ratio `(with_i - without_i) / without_i`.
//! Computing the ratio pair by pair, before either arm's samples are
//! independently sorted, is what makes common-mode drift cancel: a pair's
//! two builds run microseconds to milliseconds apart, so a slow host at
//! iteration `i` inflates `with_i` and `without_i` together, and the ratio
//! taken from that SAME pair cancels the inflation directly.
//!
//! This is deliberately NOT `(median(with_ns) - median(without_ns)) /
//! median(without_ns)` (the "separate-median" statistic, still reported below
//! as `MARGINAL_SEPARATE_MEDIAN` for observability only). That formula
//! independently sorts each arm before differencing, which discards the
//! pairing: `median(A) - median(B)` only equals `median(A - B)` when every
//! pair's rank position is preserved across both independent sorts, which is
//! not guaranteed once each measurement carries its own timing noise. A
//! 2026-07-06 review of this gate (verivus-oss/sqry#280) found exactly
//! this: a contended run where the separate-median statistic collapsed to a
//! small number while the true, paired marginal (and the min cross-check)
//! stayed high, i.e. a false PASS on a real regression. Gating on the
//! per-pair ratio's median removes that false-negative path.
//!
//! The per-arm medians, the min-based marginal, and the paired-difference
//! band (in nanoseconds) are still reported as lower-noise cross-checks, but
//! none of them feed the PASS/FAIL decision; only the per-pair ratio median
//! does.
//!
//! ## Output (machine-readable lines, consumed by the gate)
//!
//! ```text
//! PAIRED_MARGINAL=<median over pairs of (with_i - without_i) / without_i -- ENFORCED>
//! WITH_NS=<median of the Phase-A-on build, nanoseconds -- cross-check>
//! WITHOUT_NS=<median of the floor build, nanoseconds -- cross-check>
//! MARGINAL_SEPARATE_MEDIAN=<(WITH_NS - WITHOUT_NS) / WITHOUT_NS -- cross-check only, NOT enforced>
//! WITH_MIN_NS=<min of the Phase-A-on build, nanoseconds -- cross-check>
//! WITHOUT_MIN_NS=<min of the floor build, nanoseconds -- cross-check>
//! SAMPLES=<iteration count>
//! ```
//!
//! A human-readable summary (the enforced paired marginal, the separate-median
//! and min-based cross-checks, and the interquartile band of the paired
//! difference) is printed after the machine lines.
//!
//! ## Configuration
//!
//! * `argv[1]` (optional): fixture path. Defaults to
//!   `test-fixtures/c-icall-precision/linux-driver-subset/`.
//! * `SQRY_PHASE_A_GATE_ITERS` (default 50): paired iterations.
//! * `SQRY_PHASE_A_GATE_WARMUP` (default 5): discarded warmup pairs.
//!
//! Requires the `phase-a-toggle` feature so the non-production
//! `CPlugin::without_phase_a` constructor is available. Built without it, the
//! binary exits non-zero with an explanatory message so the gate fails loudly
//! rather than measuring nothing.

use std::process::ExitCode;

#[cfg(not(feature = "phase-a-toggle"))]
fn main() -> ExitCode {
    eprintln!(
        "phase_a_marginal requires the `phase-a-toggle` feature. Re-run with \
         `cargo run --release --example phase_a_marginal --features phase-a-toggle`."
    );
    ExitCode::FAILURE
}

#[cfg(feature = "phase-a-toggle")]
fn main() -> ExitCode {
    imp::run()
}

#[cfg(feature = "phase-a-toggle")]
mod imp {
    use std::hint::black_box;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::Instant;

    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::plugin::PluginManager;
    use sqry_lang_c::CPlugin;

    fn env_usize(key: &str, default: usize) -> usize {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(default)
    }

    fn default_fixture() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent()
            .expect("sqry-lang-c has a workspace parent")
            .join("test-fixtures/c-icall-precision/linux-driver-subset")
    }

    fn build(root: &Path, phase_a: bool) -> CodeGraph {
        let mut plugins = PluginManager::new();
        let plugin = if phase_a {
            CPlugin::new()
        } else {
            CPlugin::without_phase_a()
        };
        plugins.register_builtin(Box::new(plugin));
        build_unified_graph(root, &plugins, &BuildConfig::default())
            .expect("build_unified_graph must succeed")
    }

    fn time_build(root: &Path, phase_a: bool) -> u128 {
        let start = Instant::now();
        let graph = build(root, phase_a);
        let elapsed = start.elapsed().as_nanos();
        black_box(&graph);
        elapsed
    }

    fn median(sorted: &[u128]) -> u128 {
        let n = sorted.len();
        if n == 0 {
            return 0;
        }
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2
        }
    }

    /// Median of an ALREADY SORTED `f64` slice (ascending, no `NaN`). Empty
    /// returns 0.0.
    fn median_f64(sorted: &[f64]) -> f64 {
        let n = sorted.len();
        if n == 0 {
            return 0.0;
        }
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        }
    }

    /// The per-pair marginal ratio for ONE paired measurement:
    /// `(with_ns - without_ns) / without_ns`. Computed from the two
    /// measurements of the SAME pair, before either arm is independently
    /// sorted, so common-mode drift affecting both halves of the pair
    /// cancels directly in this ratio. This is the value the ENFORCED
    /// `PAIRED_MARGINAL` statistic is the median of; see the module doc for
    /// why that differs from `(median(with) - median(without)) /
    /// median(without)`.
    fn paired_ratio(with_ns: u128, without_ns: u128) -> f64 {
        if without_ns == 0 {
            0.0
        } else {
            (with_ns as f64 - without_ns as f64) / without_ns as f64
        }
    }

    fn percentile(sorted: &[u128], p: f64) -> u128 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
        sorted[idx]
    }

    pub fn run() -> ExitCode {
        let fixture = std::env::args()
            .nth(1)
            .map_or_else(default_fixture, PathBuf::from);
        if !fixture.exists() {
            eprintln!("fixture path does not exist: {}", fixture.display());
            return ExitCode::FAILURE;
        }

        let iters = env_usize("SQRY_PHASE_A_GATE_ITERS", 50);
        let warmup = env_usize("SQRY_PHASE_A_GATE_WARMUP", 5);

        // Warm the process: page cache for the fixture files, allocator arenas,
        // and CPU frequency, so the first measured pair is not penalised.
        for _ in 0..warmup {
            black_box(build(&fixture, true));
            black_box(build(&fixture, false));
        }

        let mut with_ns: Vec<u128> = Vec::with_capacity(iters);
        let mut without_ns: Vec<u128> = Vec::with_capacity(iters);
        let mut diff_ns: Vec<i128> = Vec::with_capacity(iters);
        let mut pair_ratio: Vec<f64> = Vec::with_capacity(iters);

        for i in 0..iters {
            // Alternate which build runs first so neither arm is systematically
            // the "cold" or "warm" half of the pair.
            let (w, wo) = if i % 2 == 0 {
                let w = time_build(&fixture, true);
                let wo = time_build(&fixture, false);
                (w, wo)
            } else {
                let wo = time_build(&fixture, false);
                let w = time_build(&fixture, true);
                (w, wo)
            };
            with_ns.push(w);
            without_ns.push(wo);
            diff_ns.push(w as i128 - wo as i128);
            // Captured from THIS pair, before with_ns/without_ns are
            // independently sorted below -- this is what keeps the ratio
            // paired instead of collapsing into two separate distributions.
            pair_ratio.push(paired_ratio(w, wo));
        }

        // ENFORCED statistic: median of the per-pair ratios. Sorted on its
        // own axis; unrelated to the with_ns/without_ns sorts below, which
        // exist only to report per-arm cross-check numbers.
        pair_ratio.sort_by(f64::total_cmp);
        let paired_marginal = median_f64(&pair_ratio);

        with_ns.sort_unstable();
        without_ns.sort_unstable();
        diff_ns.sort_unstable();

        let with_med = median(&with_ns);
        let without_med = median(&without_ns);

        // Minimum (best-case) build times, reported as a lower-noise cross-check.
        let with_min = *with_ns.first().unwrap_or(&0);
        let without_min = *without_ns.first().unwrap_or(&0);

        // Separate-median marginal: NOT enforced (see module doc for why this
        // is unsound as a gate statistic). Reported only as a cross-check so
        // a reader can see how far it diverges from the paired statistic on
        // a given host. Clamp the numerator at zero for display purposes (a
        // negative difference here just means the two arms' independent
        // medians landed within the noise floor of each other).
        let marginal_separate_median = if without_med == 0 {
            0.0
        } else {
            with_med.saturating_sub(without_med) as f64 / without_med as f64
        };
        let marginal_min = if without_min == 0 {
            0.0
        } else {
            with_min.saturating_sub(without_min) as f64 / without_min as f64
        };

        // Paired-difference median + interquartile band, a noise indicator
        // (absolute ns, not normalized to a ratio like PAIRED_MARGINAL).
        let diff_med_signed = {
            let n = diff_ns.len();
            if n == 0 {
                0
            } else if n % 2 == 1 {
                diff_ns[n / 2]
            } else {
                (diff_ns[n / 2 - 1] + diff_ns[n / 2]) / 2
            }
        };
        let diff_med = diff_med_signed.max(0) as u128;
        let diff_u: Vec<u128> = diff_ns.iter().map(|&d| d.max(0) as u128).collect();
        let p25 = percentile(&diff_u, 0.25);
        let p75 = percentile(&diff_u, 0.75);

        // Machine-readable lines for the gate. PAIRED_MARGINAL is the ONLY
        // field the gate script (check_phase_a_perf_gate.sh) keys its
        // PASS/FAIL decision on; every other field is observability.
        println!("PAIRED_MARGINAL={paired_marginal:.6}");
        println!("WITH_NS={with_med}");
        println!("WITHOUT_NS={without_med}");
        println!("MARGINAL_SEPARATE_MEDIAN={marginal_separate_median:.6}");
        println!("WITH_MIN_NS={with_min}");
        println!("WITHOUT_MIN_NS={without_min}");
        println!("SAMPLES={iters}");

        // Human summary.
        eprintln!("Phase A instrumentation marginal (paired, alternating; per-pair ratio median):");
        eprintln!(
            "  fixture                                : {}",
            fixture.display()
        );
        eprintln!("  iterations                             : {iters} (warmup {warmup})");
        eprintln!(
            "  with    (median / min)                : {:.3} / {:.3} ms",
            with_med as f64 / 1.0e6,
            with_min as f64 / 1.0e6,
        );
        eprintln!(
            "  without (median / min)                : {:.3} / {:.3} ms",
            without_med as f64 / 1.0e6,
            without_min as f64 / 1.0e6,
        );
        eprintln!(
            "  paired diff (median)                  : {:.3} ms  [p25 {:.3} ms, p75 {:.3} ms]",
            diff_med as f64 / 1.0e6,
            p25 as f64 / 1.0e6,
            p75 as f64 / 1.0e6,
        );
        eprintln!(
            "  marginal, paired per-pair ratio (ENFORCED): {:+.2}%",
            paired_marginal * 100.0,
        );
        eprintln!(
            "  marginal, separate-median (cross-check only): {:+.2}%",
            marginal_separate_median * 100.0,
        );
        eprintln!(
            "  marginal, separate-min (cross-check only)  : {:+.2}%",
            marginal_min * 100.0,
        );

        ExitCode::SUCCESS
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn median_handles_odd_and_even_lengths() {
            assert_eq!(median(&[1, 2, 3]), 2);
            assert_eq!(median(&[1, 2, 3, 4]), 2); // (2 + 3) / 2, integer division
            assert_eq!(median(&[]), 0);
        }

        #[test]
        fn median_f64_handles_odd_and_even_lengths() {
            assert!((median_f64(&[0.1, 0.2, 0.3]) - 0.2).abs() < 1e-12);
            assert!((median_f64(&[0.1, 0.2, 0.3, 0.4]) - 0.25).abs() < 1e-12);
            assert_eq!(median_f64(&[]), 0.0);
        }

        #[test]
        fn paired_ratio_matches_manual_computation() {
            assert!((paired_ratio(11_000_000, 10_000_000) - 0.10).abs() < 1e-9);
            assert_eq!(paired_ratio(10_000_000, 0), 0.0);
        }

        /// When every pair shares the SAME true marginal (no independent
        /// per-measurement noise), the paired-ratio median and the
        /// separate-median ratio must agree: this is the sanity baseline the
        /// next test's divergence is measured against.
        #[test]
        fn paired_and_separate_median_agree_without_independent_noise() {
            let without_ns: Vec<u128> =
                vec![10_000_000, 12_000_000, 9_000_000, 11_000_000, 14_000_000];
            let with_ns: Vec<u128> = without_ns.iter().map(|&wo| wo + wo / 10).collect(); // +10% every pair

            let mut ratios: Vec<f64> = with_ns
                .iter()
                .zip(without_ns.iter())
                .map(|(&w, &wo)| paired_ratio(w, wo))
                .collect();
            ratios.sort_by(f64::total_cmp);
            let paired = median_f64(&ratios);

            let mut w_sorted = with_ns.clone();
            let mut wo_sorted = without_ns.clone();
            w_sorted.sort_unstable();
            wo_sorted.sort_unstable();
            let separate =
                (median(&w_sorted) as f64 - median(&wo_sorted) as f64) / median(&wo_sorted) as f64;

            assert!((paired - 0.10).abs() < 1e-6);
            assert!((separate - 0.10).abs() < 1e-6);
        }

        /// The blocker-1 regression test for verivus-oss/sqry#280: a
        /// synthetic drift scenario (independent per-measurement noise layered
        /// on top of widely varying, host-contention-like per-pair baselines)
        /// where the OLD separate-median statistic reports a marginal WITHIN
        /// the +15% budget (a false PASS on a real regression) while the
        /// per-pair ratio median correctly reports the true marginal WELL
        /// ABOVE budget. This is the exact failure mode Codex's PR #531 r1
        /// review found in the field (run 3 of
        /// `measurements/2026-07-06-perf-gate-remethodology.md`: separate
        /// median +2.35%, min cross-check +11.10%).
        ///
        /// Data below is a fixed, deterministic 9-pair dataset (not a live
        /// build measurement) constructed so that: every pair's TRUE marginal
        /// is ~20% (a real regression, above the +15% budget), but
        /// independent per-arm noise decorrelates the two arms enough that
        /// `median(with) - median(without)) / median(without)` collapses to
        /// under 10%.
        #[test]
        fn paired_statistic_catches_regression_separate_median_would_miss() {
            const BUDGET: f64 = 0.15;

            let with_ns: [u128; 9] = [
                14_272_511, 16_925_444, 13_161_386, 14_138_143, 18_469_153, 16_994_100, 14_371_734,
                14_028_218, 15_624_458,
            ];
            let without_ns: [u128; 9] = [
                10_993_438, 13_142_542, 10_411_798, 11_229_224, 15_176_095, 14_624_701, 13_196_778,
                11_124_646, 13_328_169,
            ];

            let mut ratios: Vec<f64> = with_ns
                .iter()
                .zip(without_ns.iter())
                .map(|(&w, &wo)| paired_ratio(w, wo))
                .collect();
            ratios.sort_by(f64::total_cmp);
            let paired = median_f64(&ratios);

            let mut w_sorted = with_ns.to_vec();
            let mut wo_sorted = without_ns.to_vec();
            w_sorted.sort_unstable();
            wo_sorted.sort_unstable();
            let with_med = median(&w_sorted);
            let without_med = median(&wo_sorted);
            let separate = with_med.saturating_sub(without_med) as f64 / without_med as f64;

            // The old (buggy) statistic would have PASSED a real regression.
            assert!(
                separate <= BUDGET,
                "expected the separate-median statistic to read within budget \
                 (demonstrating the false-negative path), got {separate}"
            );
            // The paired statistic correctly FAILS the same data.
            assert!(
                paired > BUDGET,
                "expected the paired per-pair-ratio statistic to exceed budget \
                 on this regression, got {paired}"
            );
            // And the gap between them is not marginal noise -- it is large
            // enough to flip the PASS/FAIL verdict outright.
            assert!(paired - separate > 0.10);
        }
    }
}

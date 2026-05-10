//! `A_cancellation.md` §6 row 7 — `tokio-util` feature contract audit.
//!
//! Pins the `sqry-daemon` ↔ `tokio-util` dependency contract for the
//! `tokio_util::sync::CancellationToken` callers in
//! `lifecycle/signals.rs`, `mcp_host/mod.rs`, and
//! `ipc/methods/daemon_cancel_rebuild.rs`.
//!
//! Implementation note (resolves `A_cancellation.md` §1 audit OQ-1):
//! the design's iter-3 §1 audit note speculated that the build worked
//! through transitive feature unification because the `Cargo.toml`
//! line declared only `["rt"]` while the code uses
//! `tokio_util::sync::CancellationToken`. Investigation during the
//! Layer-2 implementation pass against `tokio-util 0.7.18` shows the
//! actual situation:
//!
//! - `tokio-util 0.7.18`'s manifest declares **no** `"sync"` feature
//!   (only `__docs_rs`, `codec`, `compat`, `default`, `full`, `io`,
//!   `io-util`, `join-map`, `net`, `rt`, `slab`, `time`, `tracing`).
//!   See `~/.cargo/registry/src/.../tokio-util-0.7.18/Cargo.toml`
//!   `[features]`.
//! - `tokio-util/src/lib.rs:56` declares `pub mod sync;`
//!   **unconditionally** (no `cfg_*!` macro). The `sync` module
//!   compiles regardless of which features the caller selects.
//! - There is therefore no transitive feature-unification dependency
//!   to lock — `["rt"]` is the canonical, self-sufficient feature
//!   set for sqry-daemon.
//!
//! This test pins the `["rt"]` contract verbatim so future tokio-util
//! version bumps that DO move `sync` behind a feature gate (or that
//! drop `pub mod sync` from the unconditional surface) trigger this
//! test before the build silently breaks at a non-obvious site.

use std::path::PathBuf;

#[test]
fn sqry_daemon_cargo_toml_declares_tokio_util_rt_feature() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));

    let tokio_util_line = text
        .lines()
        .find(|line| line.trim_start().starts_with("tokio-util"))
        .unwrap_or_else(|| {
            panic!(
                "sqry-daemon/Cargo.toml does not declare tokio-util at all; \
                 A_cancellation.md §1 contract requires it"
            )
        });

    assert!(
        tokio_util_line.contains("\"rt\""),
        "sqry-daemon/Cargo.toml tokio-util line must enable the \"rt\" feature; \
         line was: {tokio_util_line}"
    );
}

#[test]
fn cancellation_token_resolves_through_unconditional_sync_module() {
    // Compile-time + runtime check that `tokio_util::sync::CancellationToken`
    // is reachable without a `"sync"` feature flag, validating the
    // unconditional `pub mod sync;` contract from `tokio-util 0.7.18`'s
    // `lib.rs:56`. If a future bump adds a `"sync"` feature gate around
    // the module, this assertion either fails to compile (preferred —
    // forces the maintainer to update `Cargo.toml`) or panics at runtime
    // (fallback — still surfaces the regression).
    let token: tokio_util::sync::CancellationToken = tokio_util::sync::CancellationToken::new();
    assert!(
        !token.is_cancelled(),
        "freshly-constructed CancellationToken must not report cancelled"
    );
}

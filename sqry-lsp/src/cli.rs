use clap::{Args, Parser};
use std::path::PathBuf;

/// Common options for launching the sqry LSP server.
#[derive(Debug, Clone, Args)]
pub struct LspOptions {
    /// Run the server over stdin/stdout (default transport)
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with = "socket")]
    pub stdio: bool,

    /// Listen on a TCP socket instead of stdio.
    ///
    /// Security: Use 127.0.0.1:PORT for localhost-only access (recommended).
    /// Binding to 0.0.0.0 or LAN IPs exposes the server to your network.
    ///
    /// Example: --socket 127.0.0.1:9257
    #[arg(long, value_name = "ADDR")]
    pub socket: Option<String>,

    /// Explicit index root (defaults to first workspace folder containing .sqry-index).
    ///
    /// **Deprecation note (Step 10).** When the LSP `initialize` request
    /// carries an in-band workspace signal — either an
    /// `initializationOptions.sqry.workspace` classification hint
    /// (a `{ folders, classification }` payload that the `sqry-vscode`
    /// extension constructs from the active `.code-workspace` after
    /// Step 5) **or** an `initializationOptions.sqry.indexRoot` value
    /// (forwarded from the extension's `sqry.indexRoot` setting in
    /// Step 10 iter3) — `--index-root` is redundant: the `initialize`
    /// payload already communicates the canonical workspace identity
    /// in-band. The server emits a single `tracing::warn!` event at
    /// session start when the flag is combined with either in-band
    /// signal, with a pointer to `docs/cli/workspace-wrapper-migration.md`.
    /// The flag continues to work — this is informational, not a refusal.
    #[arg(long, value_name = "PATH")]
    pub index_root: Option<PathBuf>,

    /// Logging level (error|warn|info|debug|trace)
    #[arg(long, value_name = "LEVEL", default_value = "warn")]
    pub log_level: String,

    /// Optional path to JSON config with advanced settings
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Suppress security warnings for public network bindings.
    ///
    /// Use this flag when deploying sqry-lsp in environments where binding to
    /// non-localhost addresses is intended (e.g., containerized deployments,
    /// CI/CD pipelines). Setting this flag acknowledges the security implications
    /// of exposing the LSP server to network interfaces beyond localhost.
    ///
    /// Can also be set via `SQRY_LSP_ALLOW_PUBLIC_BIND` environment variable.
    ///
    /// WARNING: The LSP protocol transmits source code without encryption or
    /// authentication by default. Only use this flag if you understand the risks.
    #[arg(long, env = "SQRY_LSP_ALLOW_PUBLIC_BIND", action = clap::ArgAction::SetTrue)]
    pub allow_public_bind: bool,

    /// Connect to a running sqryd daemon and pump LSP bytes over its
    /// shim byte-pump transport (Phase 8c U11).
    ///
    /// **Client-side semantics.** When set, this process acts as a
    /// shim CLIENT: it opens [`sqry_daemon_client::connect_shim`]
    /// against the daemon's UDS / named-pipe endpoint, sends a
    /// `ShimRegister { protocol: Lsp, pid: std::process::id() }` frame
    /// as the very first wire frame, awaits `ShimRegisterAck`, and
    /// then pumps bytes bidirectionally between this process's
    /// stdin/stdout and the shim connection. The actual tower_lsp
    /// server lives inside the daemon and is hosted via
    /// `daemon_host::host_on_streams`.
    ///
    /// **Not the same as `default_daemon()`.** [`LspOptions::default_daemon`]
    /// constructs options for a SERVER-SIDE, daemon-hosted LSP
    /// session (used by the daemon router when it spawns a
    /// `SessionManager` per accepted shim connection). Those are
    /// disjoint concerns that happen to share the word "daemon":
    /// the flag means "connect as client", the constructor means
    /// "configure as daemon-hosted server".
    ///
    /// Mutually exclusive with [`LspOptions::stdio`] and
    /// [`LspOptions::socket`] (TCP) — the byte-pump transport owns
    /// stdio end-to-end, so a simultaneous `--stdio` or TCP bind
    /// would race for the same file descriptors / ports.
    #[arg(long, conflicts_with_all = ["stdio", "socket"])]
    pub daemon: bool,

    /// Daemon UDS / named-pipe socket path. Usable only with
    /// [`Self::daemon`].
    ///
    /// Resolution precedence (see `resolve_daemon_socket` in
    /// `lib.rs`):
    ///
    /// 1. This explicit `--daemon-socket <PATH>` override.
    /// 2. `$SQRYD_SOCKET` environment variable — **client-side only**.
    ///    The daemon reads `$SQRY_DAEMON_SOCKET` for its bind-path
    ///    override (see `sqry-daemon/src/config.rs`). These are
    ///    intentionally distinct: `SQRYD_SOCKET` is the shim-client
    ///    env var; `SQRY_DAEMON_SOCKET` is the server-side env var.
    ///    Setting both to the same path is one way to co-configure
    ///    client + daemon without `--daemon-socket`.
    /// 3. Platform default: on Unix, `$XDG_RUNTIME_DIR/sqry/sqryd.sock`
    ///    when `XDG_RUNTIME_DIR` is set, else `$TMPDIR/sqry-<uid>/sqryd.sock`,
    ///    else `/tmp/sqry-<uid>/sqryd.sock`. On Windows,
    ///    `\\.\pipe\sqry`. These exactly mirror the paths a
    ///    default-configured `sqry-daemon` binds to via
    ///    `DaemonConfig::socket_path()` (see `sqry-daemon/src/config.rs`),
    ///    so a default-configured daemon + default-configured shim
    ///    client meet without user intervention.
    #[arg(long, requires = "daemon", value_name = "PATH")]
    pub daemon_socket: Option<PathBuf>,
}

impl LspOptions {
    /// Determine if stdio transport should be used (default when no socket is provided).
    #[must_use]
    pub fn use_stdio(&self) -> bool {
        self.stdio || self.socket.is_none()
    }

    /// Default [`LspOptions`] for a daemon-hosted session.
    ///
    /// Used by sqry-daemon's shim dispatch (Phase 8c U10) when
    /// constructing a fresh [`crate::session::SessionManager`] per shim
    /// connection. Per Codex iter-1 / iter-2 §E, each daemon-hosted
    /// LSP shim gets its own `SessionManager`; sharing session state
    /// across connections is a deferred performance optimisation, not
    /// a correctness requirement for Phase 8c.
    ///
    /// **Naming distinction vs [`Self::daemon`] flag (Phase 8c U11).**
    /// `default_daemon()` configures this `LspOptions` for a SERVER-SIDE,
    /// daemon-hosted session. The `daemon: bool` flag on [`LspCli`] /
    /// `LspOptions` means the opposite: "run as a CLIENT — connect to
    /// an external daemon via `sqry_daemon_client::connect_shim` and
    /// pump stdio". The server-side default deliberately sets `daemon
    /// = false` so that `run(options)` in `lib.rs` does NOT re-enter
    /// the client path when a daemon-hosted session is spun up by
    /// the router (which invokes `daemon_host::host_on_streams`
    /// directly, bypassing `run`).
    ///
    /// The returned options signal a pumped-stdio-equivalent transport:
    /// `stdio = true` (so [`use_stdio`] returns `true`) and `socket =
    /// None` (no network bind is performed by the daemon host path).
    /// `log_level` defaults to `"warn"` to match the CLI default.
    ///
    /// [`use_stdio`]: Self::use_stdio
    #[must_use]
    pub fn default_daemon() -> Self {
        Self {
            stdio: true,
            socket: None,
            index_root: None,
            log_level: "warn".to_string(),
            config: None,
            allow_public_bind: false,
            // Daemon-hosted server-side session: the `daemon` flag is
            // a CLIENT-side concern. Leave it false so that if this
            // options value ever flows through `run(options)` (which
            // it does not today — the router calls `host_on_streams`
            // directly) it does not try to connect to itself.
            daemon: false,
            daemon_socket: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_stdio_when_no_socket() {
        let opts = LspOptions {
            stdio: false,
            socket: None,
            index_root: None,
            log_level: "warn".into(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
        };

        assert!(opts.use_stdio());
    }

    #[test]
    fn socket_disables_stdio_by_default() {
        let opts = LspOptions {
            stdio: false,
            socket: Some("127.0.0.1:9257".into()),
            index_root: None,
            log_level: "warn".into(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
        };

        assert!(!opts.use_stdio());
    }

    #[test]
    fn allow_public_bind_defaults_to_false() {
        let opts = LspOptions {
            stdio: false,
            socket: Some("0.0.0.0:9257".into()),
            index_root: None,
            log_level: "warn".into(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
        };

        assert!(!opts.allow_public_bind);
    }

    #[test]
    fn allow_public_bind_can_be_set() {
        let opts = LspOptions {
            stdio: false,
            socket: Some("0.0.0.0:9257".into()),
            index_root: None,
            log_level: "warn".into(),
            config: None,
            allow_public_bind: true,
            daemon: false,
            daemon_socket: None,
        };

        assert!(opts.allow_public_bind);
    }

    #[test]
    fn default_daemon_constructor_leaves_client_flag_false() {
        // Regression guard for the naming-distinction comment on
        // `default_daemon`: server-side daemon-hosted options MUST NOT
        // set the CLIENT-side `daemon` flag.
        let opts = LspOptions::default_daemon();
        assert!(!opts.daemon, "default_daemon is the server-side shape");
        assert!(opts.daemon_socket.is_none());
        assert!(opts.use_stdio(), "daemon-hosted uses stdio-equivalent");
    }
}

/// Command-line interface for the standalone `sqry-lsp` binary.
#[derive(Debug, Parser)]
#[command(
    name = "sqry-lsp",
    about = "Start the sqry Language Server Protocol endpoint",
    version
)]
pub struct LspCli {
    #[command(flatten)]
    pub options: LspOptions,
}

impl LspCli {
    #[must_use]
    pub fn into_options(self) -> LspOptions {
        self.options
    }
}

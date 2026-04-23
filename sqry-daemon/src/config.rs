//! Daemon configuration.
//!
//! Loads `~/.config/sqry/daemon.toml` (path overridable via `SQRY_DAEMON_CONFIG`)
//! into a [`DaemonConfig`] and layers environment-variable overrides on top.
//! Every field from the Amendment-2 design is represented — memory budgeting,
//! admission working-set multipliers, staleness window, debounce / incremental
//! thresholds, interner compaction trigger, socket path, log rotation.
//!
//! Design notes per plan Task 5 Step 4b (Amendment 2 §G.6):
//!
//! - [`WORKING_SET_MULTIPLIER`] and [`INTERNER_BUILDER_OVERHEAD_RATIO`] are
//!   `const` and *not* user-tuneable. They are derived from benchmarking on
//!   the reference 384 k-node / 1.3 M-edge graph and must stay in sync with
//!   the `WorkspaceManager::reserve_rebuild` accounting in Task 6.
//! - Runtime-tuneable knobs live on [`DaemonConfig`] and flow through admission
//!   accounting, the retention reaper, the rebuild dispatcher, and the stale-
//!   serving router.
//! - Every knob that users can legitimately want to override without editing a
//!   config file has an `SQRY_DAEMON_*` env-var override. Env-var overrides
//!   take precedence over the TOML file so operators can run one-off daemons
//!   with bumped memory limits without munging user configs.

use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use serde::Deserialize;

use crate::error::{DaemonError, DaemonResult};

// ---------------------------------------------------------------------------
// Public constants (Amendment 2 §G.6 — admission working-set rule).
// ---------------------------------------------------------------------------

/// Covers duplicated index/edge structures held during rebuild before finalize.
///
/// Source: Amendment 2 §G.6. `working_set_estimate = new_graph_final_estimate *
/// WORKING_SET_MULTIPLIER + staging_overhead + interner_builder_overhead`.
/// Conservative by design — err high.
pub const WORKING_SET_MULTIPLIER: f64 = 1.5;

/// Bounded growth headroom for the rebuild-local interner builder, expressed
/// as a fraction of the seed snapshot's bytes.
///
/// Source: Amendment 2 §G.6. Used by
/// `WorkspaceManager::reserve_rebuild` in Task 6.
pub const INTERNER_BUILDER_OVERHEAD_RATIO: f64 = 0.25;

/// Heuristic per-file byte estimate for rebuild staging overhead.
///
/// Consumed by the Task 7 [`crate::rebuild::RebuildDispatcher`] when
/// populating [`crate::workspace::WorkingSetInputs::staging_overhead`]
/// before calling `reserve_rebuild`. 4 KiB ≈ one memory page of
/// per-file staging state (`StagingGraph` + per-plugin buffers).
///
/// This value is **heuristic**, not empirically measured. Large
/// symbol-dense files may exceed it; admission is permitted to
/// over-reserve because the reservation is refunded on failure and
/// the excess bytes return to the pool after publish's
/// `saturating_sub` in `publish_and_retain`. Per-fixture calibration
/// is deferred to Task 14 tuning, not a 7a correctness concern.
pub const ESTIMATE_STAGING_PER_FILE_BYTES: u64 = 4_096;

/// Heuristic per-file byte estimate for the committed graph's final
/// heap cost.
///
/// Consumed by the Task 7 [`crate::rebuild::RebuildDispatcher`] when
/// populating [`crate::workspace::WorkingSetInputs::new_graph_final_estimate`]
/// for incremental rebuilds — the final-size estimate is
/// `prior.heap_bytes() + closure.len() * ESTIMATE_FINAL_PER_FILE_BYTES`.
///
/// Like [`ESTIMATE_STAGING_PER_FILE_BYTES`], this is a heuristic
/// starting value rather than a fixture-tuned constant. Calibration
/// is a Task 14 concern.
pub const ESTIMATE_FINAL_PER_FILE_BYTES: u64 = 2_048;

/// Environment variable that overrides the daemon config file path.
pub const ENV_CONFIG_PATH: &str = "SQRY_DAEMON_CONFIG";

/// Environment variable that overrides `memory_limit_mb`.
pub const ENV_MEMORY_LIMIT_MB: &str = "SQRY_DAEMON_MEMORY_MB";

/// Environment variable that overrides the IPC socket path.
pub const ENV_SOCKET_PATH: &str = "SQRY_DAEMON_SOCKET";

/// Environment variable that overrides the Windows named pipe name.
pub const ENV_PIPE_NAME: &str = "SQRY_DAEMON_PIPE";

/// Environment variable that overrides `log_level`.
pub const ENV_LOG_LEVEL: &str = "SQRY_DAEMON_LOG_LEVEL";

/// Environment variable that overrides `log_file`.
pub const ENV_LOG_FILE: &str = "SQRY_DAEMON_LOG_FILE";

/// Environment variable that overrides `stale_serve_max_age_hours`.
pub const ENV_STALE_MAX_AGE_HOURS: &str = "SQRY_DAEMON_STALE_MAX_AGE_HOURS";

/// Environment variable that overrides `tool_timeout_secs`. Task 8
/// Phase 8c U6.
pub const ENV_TOOL_TIMEOUT_SECS: &str = "SQRY_DAEMON_TOOL_TIMEOUT_SECS";

/// Environment variable that overrides `max_shim_connections`. Task 8
/// Phase 8c U10.
pub const ENV_MAX_SHIM_CONNECTIONS: &str = "SQRY_DAEMON_MAX_SHIM_CONNECTIONS";

/// Environment variable that overrides `auto_start_ready_timeout_secs`. Task 9
/// U2.
pub const ENV_AUTO_START_READY_TIMEOUT_SECS: &str = "SQRY_DAEMON_AUTO_START_READY_TIMEOUT_SECS";

/// Environment variable that overrides `log_keep_rotations`. Task 9 U2.
pub const ENV_LOG_KEEP_ROTATIONS: &str = "SQRY_DAEMON_LOG_KEEP_ROTATIONS";

// ---------------------------------------------------------------------------
// Built-in defaults (match plan §5 Step 3 table).
// ---------------------------------------------------------------------------

/// Default: 2 GiB memory budget for the whole daemon.
pub const DEFAULT_MEMORY_LIMIT_MB: u64 = 2_048;
/// Default: idle workspaces eligible for eviction after 30 minutes.
pub const DEFAULT_IDLE_TIMEOUT_MINUTES: u64 = 30;
/// Default: 2 s coalescing window for file-system notifications.
pub const DEFAULT_DEBOUNCE_MS: u64 = 2_000;
/// Default: > 20 changed files → full rebuild instead of incremental.
pub const DEFAULT_INCREMENTAL_THRESHOLD: usize = 20;
/// Default: reverse-dep closure > 30% of file count → full rebuild.
pub const DEFAULT_CLOSURE_LIMIT_PERCENT: u32 = 30;
/// Default: 24 h stale-serve cap (`0` disables the cap).
pub const DEFAULT_STALE_SERVE_MAX_AGE_HOURS: u32 = 24;
/// Default: retention reaper logs a WARN after 5 s of held-retained state.
pub const DEFAULT_REBUILD_DRAIN_TIMEOUT_MS: u64 = 5_000;
/// Default: `live_ratio < 0.5` triggers a mandatory full rebuild at the next
/// debounce tick (interner compaction housekeeping).
pub const DEFAULT_INTERNER_COMPACTION_THRESHOLD: f32 = 0.5;
/// Default: 5 s grace window for the IPC accept loop to drain active
/// connections during shutdown before the server returns.
///
/// Task 8 Phase 8a. Valid range (enforced by [`DaemonConfig::validate`]):
/// `1..=3600`.
pub const DEFAULT_IPC_SHUTDOWN_DRAIN_SECS: u64 = 5;
/// Default: 60 s per-tool invocation timeout — response-latency bound
/// consumed by
/// [`crate::ipc::tool_core::classify_and_execute`]. Task 8 Phase 8c U6.
///
/// Valid range (enforced by [`DaemonConfig::validate`]): `1..=3600`. A
/// zero timeout would cause every `spawn_blocking` call to race
/// `tokio::time::timeout` at 0ms and is therefore rejected.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 60;
/// Default: cap on the number of concurrently-registered shim
/// byte-pump connections (`sqry lsp --daemon` / `sqry mcp --daemon`).
/// Consumed by
/// [`crate::ipc::shim_registry::ShimRegistry::try_register_bounded`]
/// from the Phase 8c router (U10). Task 8 Phase 8c U10.
///
/// Valid range (enforced by [`DaemonConfig::validate`]): `1..=65_536`.
/// `256` is comfortably above the realistic fan-out of any single
/// developer workstation (one IDE + one MCP client per project,
/// typically ≤ 8 workspaces × 2 protocols = 16) while still bounding
/// the worst-case memory footprint of the registry
/// (`HashMap<ShimConnId, ShimConnEntry>`) should a buggy or malicious
/// client spam `ShimRegister` frames.
pub const DEFAULT_MAX_SHIM_CONNECTIONS: usize = 256;
/// Default: `info`.
pub const DEFAULT_LOG_LEVEL: &str = "info";
/// Default: rotate daemon log at 50 MiB.
pub const DEFAULT_LOG_MAX_SIZE_MB: u64 = 50;
/// Default: poll timeout waiting for the daemon socket to become reachable
/// after auto-spawn. Used by both the `--detach` parent wait loop and the
/// `lifecycle::start_detached` helper. Validated range: `1..=60`.
pub const DEFAULT_AUTO_START_READY_TIMEOUT_SECS: u64 = 10;
/// Default: number of rotated log files to keep alongside the active log.
/// A value of 5 means up to 5 `.N` suffixed archive files are retained;
/// the oldest is deleted when a new rotation creates `.6`. Validated range:
/// `1..=100`.
pub const DEFAULT_LOG_KEEP_ROTATIONS: u32 = 5;

// ---------------------------------------------------------------------------
// Config structs.
// ---------------------------------------------------------------------------

/// Top-level daemon configuration.
///
/// Loaded from `~/.config/sqry/daemon.toml` by default. Env-var overrides
/// (see the `ENV_*` constants) are layered on top by [`DaemonConfig::load`].
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Hard cap on total resident graph memory across every loaded workspace.
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,

    /// Workspace idle-timeout before it becomes eligible for LRU eviction.
    #[serde(default = "default_idle_timeout_minutes")]
    pub idle_timeout_minutes: u64,

    /// Filesystem-watcher debounce window (ms) for coalescing bursts of changes.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,

    /// If > `incremental_threshold` files changed in one window, full-rebuild
    /// instead of incremental-rebuild.
    #[serde(default = "default_incremental_threshold")]
    pub incremental_threshold: usize,

    /// If the reverse-dep closure covers > `closure_limit_percent`% of the
    /// graph's files, full-rebuild instead of incremental-rebuild.
    #[serde(default = "default_closure_limit_percent")]
    pub closure_limit_percent: u32,

    /// Cap on how long a Failed workspace may keep serving its last-good
    /// snapshot as `stale: true`. `0` disables the cap (serve indefinitely).
    #[serde(default = "default_stale_serve_max_age_hours")]
    pub stale_serve_max_age_hours: u32,

    /// Retention-reaper WARN threshold, **not** an accounting deadline.
    /// Retained bytes are released when `Arc::strong_count` drops to 1 —
    /// regardless of wall-clock time.
    #[serde(default = "default_rebuild_drain_timeout_ms")]
    pub rebuild_drain_timeout_ms: u64,

    /// Grace window (seconds) for the IPC accept loop to drain active
    /// connections during shutdown. Task 8 Phase 8a.
    #[serde(default = "default_ipc_shutdown_drain_secs")]
    pub ipc_shutdown_drain_secs: u64,

    /// Per-tool invocation timeout. Bounds the response latency of
    /// any single tool call; exceeding this returns
    /// [`DaemonError::ToolTimeout`] (JSON-RPC `-32000` / MCP
    /// `internal_error` with `kind = "deadline_exceeded"`).
    ///
    /// **Important contract**: this bounds RESPONSE LATENCY, not the
    /// detached OS-thread lifetime. When the timeout fires, the
    /// [`tokio::task::spawn_blocking`] [`tokio::task::JoinHandle`] is
    /// dropped; the OS thread running the tool closure continues
    /// until the closure itself returns. A buggy/runaway tool closure
    /// can keep its thread alive past `daemon/stop`. Default 60
    /// seconds. Task 8 Phase 8c U6.
    ///
    /// [`DaemonError::ToolTimeout`]: crate::error::DaemonError::ToolTimeout
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,

    /// Cap on the number of concurrently-registered shim byte-pump
    /// connections. Every accepted `ShimRegister` frame must pass
    /// [`crate::ipc::shim_registry::ShimRegistry::try_register_bounded`]
    /// against this cap under a single mutex-guard — over-cap
    /// admissions reply `ShimRegisterAck { accepted: false, reason:
    /// "shim registry full (N / cap)" }` and the connection closes.
    /// Default `256`. Task 8 Phase 8c U10.
    #[serde(default = "default_max_shim_connections")]
    pub max_shim_connections: usize,

    /// Interner housekeeping: if the live-ratio drops below this, the next
    /// debounce tick schedules a mandatory full rebuild.
    #[serde(default = "default_interner_compaction_threshold")]
    pub interner_compaction_threshold: f32,

    /// Optional structured-log file path.
    #[serde(default)]
    pub log_file: Option<PathBuf>,

    /// Log verbosity (matches `tracing_subscriber::EnvFilter` syntax).
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Log-rotation trigger.
    #[serde(default = "default_log_max_size_mb")]
    pub log_max_size_mb: u64,

    /// IPC listener binding (UDS on Unix, named pipe on Windows).
    #[serde(default)]
    pub socket: SocketConfig,

    /// Pre-declared workspaces — pinned workspaces load at daemon startup.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceConfig>,

    /// Timeout (seconds) used in two places:
    ///
    /// 1. **`--detach` parent wait loop** (`run_start_detach`): how long the
    ///    launching parent process waits for the grandchild to signal ready via
    ///    the self-pipe before giving up and killing the grandchild.
    /// 2. **`lifecycle::start_detached`** (Task 10 auto-spawn): how long the
    ///    client helper polls the daemon socket before returning
    ///    [`DaemonError::AutoStartTimeout`].
    ///
    /// Valid range (enforced by [`DaemonConfig::validate`]): `1..=60`.
    #[serde(default = "default_auto_start_ready_timeout_secs")]
    pub auto_start_ready_timeout_secs: u64,

    /// Number of rotated log archives to keep alongside the live log file.
    ///
    /// When [`RollingSizeAppender`] rotates, it shifts existing `.1`–`.N` files
    /// one position and deletes any file beyond this limit. A value of `5` means
    /// `.1`–`.5` are retained; `.6` and beyond are removed.
    ///
    /// Valid range (enforced by [`DaemonConfig::validate`]): `1..=100`.
    ///
    /// [`RollingSizeAppender`]: crate::lifecycle::log_rotate::RollingSizeAppender
    #[serde(default = "default_log_keep_rotations")]
    pub log_keep_rotations: u32,

    /// Reserved for future use — will drive automated systemd user-service
    /// installation on first `sqryd start` when set to `true`. Currently a
    /// no-op; stored in config to avoid breaking changes when the feature
    /// lands. Defaults to `false`.
    #[serde(default)]
    pub install_user_service: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            memory_limit_mb: DEFAULT_MEMORY_LIMIT_MB,
            idle_timeout_minutes: DEFAULT_IDLE_TIMEOUT_MINUTES,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            incremental_threshold: DEFAULT_INCREMENTAL_THRESHOLD,
            closure_limit_percent: DEFAULT_CLOSURE_LIMIT_PERCENT,
            stale_serve_max_age_hours: DEFAULT_STALE_SERVE_MAX_AGE_HOURS,
            rebuild_drain_timeout_ms: DEFAULT_REBUILD_DRAIN_TIMEOUT_MS,
            ipc_shutdown_drain_secs: DEFAULT_IPC_SHUTDOWN_DRAIN_SECS,
            tool_timeout_secs: DEFAULT_TOOL_TIMEOUT_SECS,
            max_shim_connections: DEFAULT_MAX_SHIM_CONNECTIONS,
            interner_compaction_threshold: DEFAULT_INTERNER_COMPACTION_THRESHOLD,
            log_file: None,
            log_level: DEFAULT_LOG_LEVEL.to_owned(),
            log_max_size_mb: DEFAULT_LOG_MAX_SIZE_MB,
            socket: SocketConfig::default(),
            workspaces: Vec::new(),
            auto_start_ready_timeout_secs: DEFAULT_AUTO_START_READY_TIMEOUT_SECS,
            log_keep_rotations: DEFAULT_LOG_KEEP_ROTATIONS,
            install_user_service: false,
        }
    }
}

/// IPC binding configuration.
///
/// On Unix, [`SocketConfig::path`] takes precedence. On Windows,
/// [`SocketConfig::pipe_name`] takes precedence. If neither is set the
/// platform default is used (see [`DaemonConfig::socket_path`]).
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SocketConfig {
    /// Unix-domain socket path.
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Windows named-pipe name (e.g. `sqryd`).
    #[serde(default)]
    pub pipe_name: Option<String>,
}

/// Pre-declared workspace entry.
///
/// `pinned = true` keeps the workspace in memory indefinitely (LRU exempt).
/// `exclude = true` skips the workspace during auto-discovery.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Absolute path to the workspace root.
    pub path: PathBuf,

    /// Whether the workspace is LRU-exempt. Defaults to `false`.
    #[serde(default)]
    pub pinned: bool,

    /// Whether the workspace should be skipped entirely. Defaults to `false`.
    #[serde(default)]
    pub exclude: bool,
}

// ---------------------------------------------------------------------------
// Loader / path helpers.
// ---------------------------------------------------------------------------

impl DaemonConfig {
    /// Load the effective config: start from defaults, apply the TOML file at
    /// the canonical path (or the one named by [`ENV_CONFIG_PATH`]), then
    /// layer environment-variable overrides.
    ///
    /// A missing config file is **not** an error — the defaults plus env-var
    /// overrides are returned. A malformed file is always an error.
    pub fn load() -> DaemonResult<Self> {
        let path = Self::resolve_config_path()?;
        let mut config = if path.exists() {
            Self::load_from_path(&path)?
        } else {
            Self::default()
        };
        config.apply_env_overrides()?;
        config.validate()?;
        Ok(config)
    }

    /// Load a config file from an explicit path, ignoring env overrides.
    /// Useful for tests and documentation examples.
    pub fn load_from_path(path: &Path) -> DaemonResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| DaemonError::Config {
            path: path.to_path_buf(),
            source: anyhow::Error::from(source).context("reading daemon config"),
        })?;
        Self::from_toml_str(&text).map_err(|source| DaemonError::Config {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Parse a TOML string into a [`DaemonConfig`]. Defaults fill any missing
    /// fields.
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(text).context("parsing daemon config TOML")?;
        Ok(cfg)
    }

    /// Apply `SQRY_DAEMON_*` environment-variable overrides. See the
    /// `ENV_*` constants for the full list.
    pub fn apply_env_overrides(&mut self) -> DaemonResult<()> {
        if let Some(v) = env::var_os(ENV_MEMORY_LIMIT_MB) {
            let v = v.to_string_lossy().into_owned();
            self.memory_limit_mb = v.parse::<u64>().map_err(|e| DaemonError::Config {
                path: PathBuf::from(ENV_MEMORY_LIMIT_MB),
                source: anyhow!("{ENV_MEMORY_LIMIT_MB}={v:?} must be an unsigned int: {e}"),
            })?;
        }
        if let Some(v) = env::var_os(ENV_SOCKET_PATH) {
            self.socket.path = Some(PathBuf::from(v));
        }
        if let Some(v) = env::var_os(ENV_PIPE_NAME) {
            self.socket.pipe_name = Some(v.to_string_lossy().into_owned());
        }
        if let Some(v) = env::var_os(ENV_LOG_LEVEL) {
            self.log_level = v.to_string_lossy().into_owned();
        }
        if let Some(v) = env::var_os(ENV_LOG_FILE) {
            self.log_file = Some(PathBuf::from(v));
        }
        if let Some(v) = env::var_os(ENV_STALE_MAX_AGE_HOURS) {
            let v = v.to_string_lossy().into_owned();
            self.stale_serve_max_age_hours = v.parse::<u32>().map_err(|e| DaemonError::Config {
                path: PathBuf::from(ENV_STALE_MAX_AGE_HOURS),
                source: anyhow!("{ENV_STALE_MAX_AGE_HOURS}={v:?}: {e}"),
            })?;
        }
        if let Some(v) = env::var_os(ENV_TOOL_TIMEOUT_SECS) {
            let v = v.to_string_lossy().into_owned();
            self.tool_timeout_secs = v.parse::<u64>().map_err(|e| DaemonError::Config {
                path: PathBuf::from(ENV_TOOL_TIMEOUT_SECS),
                source: anyhow!("{ENV_TOOL_TIMEOUT_SECS}={v:?} must be an unsigned int: {e}"),
            })?;
        }
        if let Some(v) = env::var_os(ENV_MAX_SHIM_CONNECTIONS) {
            let v = v.to_string_lossy().into_owned();
            self.max_shim_connections = v.parse::<usize>().map_err(|e| DaemonError::Config {
                path: PathBuf::from(ENV_MAX_SHIM_CONNECTIONS),
                source: anyhow!("{ENV_MAX_SHIM_CONNECTIONS}={v:?} must be an unsigned int: {e}"),
            })?;
        }
        if let Some(v) = env::var_os(ENV_AUTO_START_READY_TIMEOUT_SECS) {
            let v = v.to_string_lossy().into_owned();
            self.auto_start_ready_timeout_secs =
                v.parse::<u64>().map_err(|e| DaemonError::Config {
                    path: PathBuf::from(ENV_AUTO_START_READY_TIMEOUT_SECS),
                    source: anyhow!(
                        "{ENV_AUTO_START_READY_TIMEOUT_SECS}={v:?} must be an unsigned int: {e}"
                    ),
                })?;
        }
        if let Some(v) = env::var_os(ENV_LOG_KEEP_ROTATIONS) {
            let v = v.to_string_lossy().into_owned();
            self.log_keep_rotations = v.parse::<u32>().map_err(|e| DaemonError::Config {
                path: PathBuf::from(ENV_LOG_KEEP_ROTATIONS),
                source: anyhow!("{ENV_LOG_KEEP_ROTATIONS}={v:?} must be an unsigned int: {e}"),
            })?;
        }
        Ok(())
    }

    /// Sanity-check invariants that admission accounting and the rebuild
    /// dispatcher depend on.
    pub fn validate(&self) -> DaemonResult<()> {
        let reject = |msg: &str| DaemonError::Config {
            path: PathBuf::from("<in-memory>"),
            source: anyhow!("{msg}"),
        };
        if self.memory_limit_mb == 0 {
            return Err(reject("memory_limit_mb must be > 0"));
        }
        if self.closure_limit_percent == 0 || self.closure_limit_percent > 100 {
            return Err(reject("closure_limit_percent must be in 1..=100"));
        }
        if !self.interner_compaction_threshold.is_finite()
            || self.interner_compaction_threshold <= 0.0
            || self.interner_compaction_threshold > 1.0
        {
            return Err(reject(
                "interner_compaction_threshold must be in (0.0, 1.0]",
            ));
        }
        if self.debounce_ms == 0 {
            return Err(reject("debounce_ms must be > 0"));
        }
        if self.log_max_size_mb == 0 {
            return Err(reject("log_max_size_mb must be > 0"));
        }
        if self.ipc_shutdown_drain_secs == 0 || self.ipc_shutdown_drain_secs > 3_600 {
            return Err(reject("ipc_shutdown_drain_secs must be in 1..=3600"));
        }
        if self.tool_timeout_secs == 0 || self.tool_timeout_secs > 3_600 {
            return Err(reject("tool_timeout_secs must be in 1..=3600"));
        }
        if self.max_shim_connections == 0 || self.max_shim_connections > 65_536 {
            return Err(reject("max_shim_connections must be in 1..=65536"));
        }
        if self.auto_start_ready_timeout_secs == 0 || self.auto_start_ready_timeout_secs > 60 {
            return Err(reject("auto_start_ready_timeout_secs must be in 1..=60"));
        }
        if self.log_keep_rotations == 0 || self.log_keep_rotations > 100 {
            return Err(reject("log_keep_rotations must be in 1..=100"));
        }
        Ok(())
    }

    /// Resolve the config-file path, respecting [`ENV_CONFIG_PATH`].
    ///
    /// Falls back to `$XDG_CONFIG_HOME/sqry/daemon.toml`, then
    /// `$HOME/.config/sqry/daemon.toml`.
    pub fn resolve_config_path() -> DaemonResult<PathBuf> {
        if let Some(v) = env::var_os(ENV_CONFIG_PATH) {
            return Ok(PathBuf::from(v));
        }
        let base = dirs::config_dir().ok_or_else(|| DaemonError::Config {
            path: PathBuf::from("~/.config"),
            source: anyhow!("could not determine user config directory; set {ENV_CONFIG_PATH}"),
        })?;
        Ok(base.join("sqry").join("daemon.toml"))
    }

    /// Path the IPC server binds to.
    ///
    /// - Unix: explicit `socket.path`, else `$XDG_RUNTIME_DIR/sqry/sqryd.sock`,
    ///   else `$TMPDIR/sqry-<uid>/sqryd.sock`.
    /// - Windows: `\\\\.\\pipe\\<socket.pipe_name>` (default `sqry`).
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        if cfg!(windows) {
            let name = self
                .socket
                .pipe_name
                .clone()
                .unwrap_or_else(|| "sqry".to_string());
            return PathBuf::from(format!(r"\\.\pipe\{name}"));
        }
        if let Some(path) = &self.socket.path {
            return path.clone();
        }
        runtime_dir().join("sqryd.sock")
    }

    /// Where to write the daemon pidfile. One per user.
    #[must_use]
    pub fn pid_path(&self) -> PathBuf {
        runtime_dir().join("sqryd.pid")
    }

    /// Flock target — held exclusively by the running daemon, and briefly
    /// by clients during auto-start to avoid racing two `sqry` processes.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        runtime_dir().join("sqryd.lock")
    }

    /// Platform-specific per-user runtime directory where the socket, pidfile,
    /// and lockfile live.
    ///
    /// This is the public accessor for the private [`runtime_dir`] free
    /// function.  The return value is the same as `socket_path().parent()`
    /// when the socket path uses the default (not the explicit `socket.path`
    /// override).
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        runtime_dir()
    }

    /// Memory budget in bytes, derived from [`Self::memory_limit_mb`].
    #[must_use]
    pub const fn memory_limit_bytes(&self) -> u64 {
        self.memory_limit_mb.saturating_mul(1024 * 1024)
    }
}

/// Platform-specific per-user runtime directory for socket / pid / lock files.
///
/// On Unix, the `/tmp` fallback is *always* suffixed with the real POSIX
/// UID (via `libc::getuid`) rather than the `USER` env var, so that two
/// processes running as different users on the same host cannot collide
/// on `/tmp/sqry-default/sqryd.{sock,pid,lock}` when `USER`/`USERNAME`
/// are unset (realistic in systemd units without `User=`, Docker
/// containers, and CI runners). See Codex Task 5 iter-1 review MAJOR
/// finding (`docs/reviews/sqryd-daemon/2026-04-18/task-5-scaffold_iter1_request_review.md`).
fn runtime_dir() -> PathBuf {
    if cfg!(windows)
        && let Some(local) = env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(local).join("sqry");
    }
    if let Some(xdg) = env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("sqry");
    }
    if let Some(tmp) = env::var_os("TMPDIR") {
        return PathBuf::from(tmp).join(user_scoped_dir_name());
    }
    PathBuf::from("/tmp").join(user_scoped_dir_name())
}

/// Per-user directory name used in the `/tmp`-style fallback.
///
/// - On Unix, always `sqry-<uid>` where `<uid>` is the real POSIX UID
///   via [`libc::getuid`]. Never falls back to a string env-var proxy —
///   `getuid` cannot fail.
/// - On Windows the only reachable callers of this function already
///   bypassed the LOCALAPPDATA branch, so we use `USERNAME` as a
///   best-effort user scope with a constant-suffix fallback. Windows
///   UIDs (SIDs) would require a separate dependency just for this
///   edge case; in practice LOCALAPPDATA is always set in any
///   Windows configuration sqry supports.
fn user_scoped_dir_name() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `libc::getuid` is a POSIX call with no preconditions,
        // no mutable state, and no way to fail. Calling it from a
        // multi-threaded program is safe per POSIX.
        let uid = unsafe { libc::getuid() };
        format!("sqry-{uid}")
    }
    #[cfg(not(unix))]
    {
        let user = env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
        format!("sqry-{user}")
    }
}

// ---------------------------------------------------------------------------
// serde default-function helpers.
// ---------------------------------------------------------------------------

const fn default_memory_limit_mb() -> u64 {
    DEFAULT_MEMORY_LIMIT_MB
}
const fn default_idle_timeout_minutes() -> u64 {
    DEFAULT_IDLE_TIMEOUT_MINUTES
}
const fn default_debounce_ms() -> u64 {
    DEFAULT_DEBOUNCE_MS
}
const fn default_incremental_threshold() -> usize {
    DEFAULT_INCREMENTAL_THRESHOLD
}
const fn default_closure_limit_percent() -> u32 {
    DEFAULT_CLOSURE_LIMIT_PERCENT
}
const fn default_stale_serve_max_age_hours() -> u32 {
    DEFAULT_STALE_SERVE_MAX_AGE_HOURS
}
const fn default_rebuild_drain_timeout_ms() -> u64 {
    DEFAULT_REBUILD_DRAIN_TIMEOUT_MS
}
const fn default_ipc_shutdown_drain_secs() -> u64 {
    DEFAULT_IPC_SHUTDOWN_DRAIN_SECS
}
const fn default_tool_timeout_secs() -> u64 {
    DEFAULT_TOOL_TIMEOUT_SECS
}
const fn default_max_shim_connections() -> usize {
    DEFAULT_MAX_SHIM_CONNECTIONS
}
const fn default_interner_compaction_threshold() -> f32 {
    DEFAULT_INTERNER_COMPACTION_THRESHOLD
}
fn default_log_level() -> String {
    DEFAULT_LOG_LEVEL.to_owned()
}
const fn default_log_max_size_mb() -> u64 {
    DEFAULT_LOG_MAX_SIZE_MB
}
const fn default_auto_start_ready_timeout_secs() -> u64 {
    DEFAULT_AUTO_START_READY_TIMEOUT_SECS
}
const fn default_log_keep_rotations() -> u32 {
    DEFAULT_LOG_KEEP_ROTATIONS
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Use the crate-wide TEST_ENV_LOCK to serialise environment-variable
    // mutations across ALL test modules in the same binary.
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    #[test]
    fn defaults_match_plan_table() {
        let cfg = DaemonConfig::default();
        assert_eq!(cfg.memory_limit_mb, 2_048);
        assert_eq!(cfg.idle_timeout_minutes, 30);
        assert_eq!(cfg.debounce_ms, 2_000);
        assert_eq!(cfg.incremental_threshold, 20);
        assert_eq!(cfg.closure_limit_percent, 30);
        assert_eq!(cfg.stale_serve_max_age_hours, 24);
        assert_eq!(cfg.rebuild_drain_timeout_ms, 5_000);
        assert_eq!(cfg.tool_timeout_secs, 60);
        assert_eq!(cfg.max_shim_connections, 256);
        assert!((cfg.interner_compaction_threshold - 0.5).abs() < f32::EPSILON);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.log_max_size_mb, 50);
        assert!(cfg.log_file.is_none());
        assert!(cfg.socket.path.is_none());
        assert!(cfg.socket.pipe_name.is_none());
        assert!(cfg.workspaces.is_empty());
    }

    #[test]
    fn memory_limit_bytes_is_mb_times_megabyte() {
        let cfg = DaemonConfig::default();
        assert_eq!(cfg.memory_limit_bytes(), 2_048 * 1024 * 1024);
    }

    #[test]
    fn parses_minimal_toml() {
        let text = r"
            memory_limit_mb = 4096
            idle_timeout_minutes = 60

            [socket]
            path = '/tmp/custom-sqryd.sock'

            [[workspaces]]
            path = '/repos/main'
            pinned = true

            [[workspaces]]
            path = '/repos/secondary'
        ";
        let cfg = DaemonConfig::from_toml_str(text).expect("parse");
        assert_eq!(cfg.memory_limit_mb, 4_096);
        assert_eq!(cfg.idle_timeout_minutes, 60);
        assert_eq!(
            cfg.socket.path.as_deref(),
            Some(Path::new("/tmp/custom-sqryd.sock"))
        );
        assert_eq!(cfg.workspaces.len(), 2);
        assert!(cfg.workspaces[0].pinned);
        assert!(!cfg.workspaces[0].exclude);
        assert!(!cfg.workspaces[1].pinned);
    }

    #[test]
    fn parses_all_knobs_with_defaults_filled_in() {
        // Empty TOML body — every field defaulted.
        let cfg = DaemonConfig::from_toml_str("").expect("parse");
        assert_eq!(cfg.memory_limit_mb, DEFAULT_MEMORY_LIMIT_MB);
        assert_eq!(
            cfg.stale_serve_max_age_hours,
            DEFAULT_STALE_SERVE_MAX_AGE_HOURS
        );
        assert_eq!(
            cfg.rebuild_drain_timeout_ms,
            DEFAULT_REBUILD_DRAIN_TIMEOUT_MS
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = "totally_bogus_knob = 42";
        let err = DaemonConfig::from_toml_str(text).expect_err("unknown field must fail");
        // `anyhow::Error::context` buries the offending field name in the
        // source chain; format with the alternate specifier to include it.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("totally_bogus_knob") && chain.contains("unknown field"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn validation_rejects_zero_memory_limit() {
        let cfg = DaemonConfig {
            memory_limit_mb: 0,
            ..DaemonConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validation_rejects_closure_limit_out_of_range() {
        let low = DaemonConfig {
            closure_limit_percent: 0,
            ..DaemonConfig::default()
        };
        assert!(low.validate().is_err());
        let high = DaemonConfig {
            closure_limit_percent: 101,
            ..DaemonConfig::default()
        };
        assert!(high.validate().is_err());
    }

    #[test]
    fn validation_rejects_compaction_threshold_out_of_range() {
        let zero = DaemonConfig {
            interner_compaction_threshold: 0.0,
            ..DaemonConfig::default()
        };
        assert!(zero.validate().is_err());
        let over = DaemonConfig {
            interner_compaction_threshold: 1.5,
            ..DaemonConfig::default()
        };
        assert!(over.validate().is_err());
        let nan = DaemonConfig {
            interner_compaction_threshold: f32::NAN,
            ..DaemonConfig::default()
        };
        assert!(nan.validate().is_err());
    }

    #[test]
    fn validation_rejects_zero_debounce_and_zero_log_size() {
        let debounce = DaemonConfig {
            debounce_ms: 0,
            ..DaemonConfig::default()
        };
        assert!(debounce.validate().is_err());
        let log = DaemonConfig {
            log_max_size_mb: 0,
            ..DaemonConfig::default()
        };
        assert!(log.validate().is_err());
    }

    #[test]
    fn validation_rejects_max_shim_connections_out_of_range() {
        let zero = DaemonConfig {
            max_shim_connections: 0,
            ..DaemonConfig::default()
        };
        assert!(zero.validate().is_err());
        let too_large = DaemonConfig {
            max_shim_connections: 65_537,
            ..DaemonConfig::default()
        };
        assert!(too_large.validate().is_err());
        let ok = DaemonConfig {
            max_shim_connections: 1_024,
            ..DaemonConfig::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn validation_rejects_tool_timeout_out_of_range() {
        let zero = DaemonConfig {
            tool_timeout_secs: 0,
            ..DaemonConfig::default()
        };
        assert!(zero.validate().is_err());
        let too_long = DaemonConfig {
            tool_timeout_secs: 3_601,
            ..DaemonConfig::default()
        };
        assert!(too_long.validate().is_err());
        let ok = DaemonConfig {
            tool_timeout_secs: 120,
            ..DaemonConfig::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn load_from_missing_path_is_an_error() {
        let err = DaemonConfig::load_from_path(Path::new("/nonexistent/sqryd.toml"))
            .expect_err("missing file is an error for explicit path");
        match err {
            DaemonError::Config { path, .. } => {
                assert_eq!(path, Path::new("/nonexistent/sqryd.toml"));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn socket_path_uses_runtime_dir_when_unspecified() {
        let cfg = DaemonConfig::default();
        let p = cfg.socket_path();
        if cfg!(unix) {
            assert!(p.ends_with("sqryd.sock"), "{p:?}");
        } else if cfg!(windows) {
            let s = p.to_string_lossy();
            assert!(s.starts_with(r"\\.\pipe\"), "{s}");
        }
    }

    #[test]
    fn apply_env_overrides_applies_memory_limit_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK so no concurrent env-var reader
        // in this module observes the in-flux value.
        unsafe {
            env::set_var(ENV_MEMORY_LIMIT_MB, "8192");
        }
        let mut cfg = DaemonConfig::default();
        let outcome = cfg.apply_env_overrides();
        // Always clean up the env var even if the assertion below would
        // fail, so sibling tests do not start in a poisoned state.
        // SAFETY: still guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_MEMORY_LIMIT_MB);
        }
        outcome.expect("override ok");
        assert_eq!(cfg.memory_limit_mb, 8_192);
    }

    #[test]
    fn apply_env_overrides_rejects_malformed_memory_limit() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::set_var(ENV_MEMORY_LIMIT_MB, "not-a-number");
        }
        let mut cfg = DaemonConfig::default();
        let err = cfg.apply_env_overrides();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_MEMORY_LIMIT_MB);
        }
        let err = err.expect_err("malformed override must fail");
        match err {
            DaemonError::Config { path, .. } => {
                assert_eq!(path, Path::new(ENV_MEMORY_LIMIT_MB));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn working_set_multiplier_matches_spec() {
        // If either of these two constants changes, the Task 6
        // reserve_rebuild tests will need to be regenerated — pin them
        // here so changes are reviewed together.
        assert!((WORKING_SET_MULTIPLIER - 1.5_f64).abs() < f64::EPSILON);
        assert!((INTERNER_BUILDER_OVERHEAD_RATIO - 0.25_f64).abs() < f64::EPSILON);
    }

    #[test]
    #[cfg(unix)]
    fn runtime_dir_is_real_uid_scoped_when_user_env_is_unset() {
        // Regression for Codex Task 5 iter-1 MAJOR finding:
        // `/tmp/sqry-default/...` collisions across users when
        // `USER`/`USERNAME`/`XDG_RUNTIME_DIR` are all unset. The fix
        // switched the fallback to a `libc::getuid()`-derived suffix
        // so every user gets their own socket/pid/lock namespace.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Stash and clear every env var that the runtime_dir() chain
        // would otherwise read ahead of the UID-based fallback.
        let prior_user = env::var_os("USER");
        let prior_username = env::var_os("USERNAME");
        let prior_xdg = env::var_os("XDG_RUNTIME_DIR");
        let prior_tmpdir = env::var_os("TMPDIR");
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe {
            env::remove_var("USER");
            env::remove_var("USERNAME");
            env::remove_var("XDG_RUNTIME_DIR");
            env::remove_var("TMPDIR");
        }

        let cfg = DaemonConfig::default();
        let socket = cfg.socket_path();
        let pid = cfg.pid_path();
        let lock = cfg.lock_path();

        // Restore the prior environment before any assertion so a
        // failing assertion does not poison sibling tests.
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            if let Some(v) = prior_user {
                env::set_var("USER", v);
            }
            if let Some(v) = prior_username {
                env::set_var("USERNAME", v);
            }
            if let Some(v) = prior_xdg {
                env::set_var("XDG_RUNTIME_DIR", v);
            }
            if let Some(v) = prior_tmpdir {
                env::set_var("TMPDIR", v);
            }
        }

        // SAFETY: `libc::getuid` is infallible; see the inline comment
        // on `user_scoped_dir_name` above.
        let uid = unsafe { libc::getuid() };
        let expected = format!("/tmp/sqry-{uid}");
        assert_eq!(
            socket.parent().and_then(Path::to_str),
            Some(expected.as_str()),
            "socket_path must be UID-scoped: socket = {socket:?}",
        );
        assert_eq!(
            pid.parent().and_then(Path::to_str),
            Some(expected.as_str()),
            "pid_path must be UID-scoped: pid = {pid:?}",
        );
        assert_eq!(
            lock.parent().and_then(Path::to_str),
            Some(expected.as_str()),
            "lock_path must be UID-scoped: lock = {lock:?}",
        );
        // And the directory name is never the literal "default".
        assert!(
            !expected.ends_with("sqry-default"),
            "runtime dir must never fall back to the shared /tmp/sqry-default path",
        );
    }

    #[test]
    fn round_trip_via_toml_preserves_workspace_entries() {
        // Author a TOML string → parse → re-emit → re-parse — the two
        // parses must produce the same workspace list.
        let text = r#"
            memory_limit_mb = 1024

            [[workspaces]]
            path = "/foo"
            pinned = true
            [[workspaces]]
            path = "/bar"
            exclude = true
        "#;
        let cfg = DaemonConfig::from_toml_str(text).unwrap();
        assert_eq!(cfg.workspaces.len(), 2);
        assert!(cfg.workspaces[0].pinned);
        assert!(cfg.workspaces[1].exclude);
    }

    // -----------------------------------------------------------------------
    // Task 9 U2 tests.
    // -----------------------------------------------------------------------

    #[test]
    fn u2_defaults_match_spec() {
        let cfg = DaemonConfig::default();
        assert_eq!(
            cfg.auto_start_ready_timeout_secs, 10,
            "auto_start_ready_timeout_secs default must be 10"
        );
        assert_eq!(
            cfg.log_keep_rotations, 5,
            "log_keep_rotations default must be 5"
        );
        assert!(
            !cfg.install_user_service,
            "install_user_service default must be false"
        );
    }

    #[test]
    fn u2_auto_start_ready_timeout_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::set_var(ENV_AUTO_START_READY_TIMEOUT_SECS, "30");
        }
        let mut cfg = DaemonConfig::default();
        let result = cfg.apply_env_overrides();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_AUTO_START_READY_TIMEOUT_SECS);
        }
        result.expect("override ok");
        assert_eq!(cfg.auto_start_ready_timeout_secs, 30);
    }

    #[test]
    fn u2_auto_start_ready_timeout_env_override_rejects_malformed() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::set_var(ENV_AUTO_START_READY_TIMEOUT_SECS, "not-a-number");
        }
        let mut cfg = DaemonConfig::default();
        let err = cfg.apply_env_overrides();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_AUTO_START_READY_TIMEOUT_SECS);
        }
        let err = err.expect_err("malformed value must fail");
        match err {
            DaemonError::Config { path, .. } => {
                assert_eq!(path, Path::new(ENV_AUTO_START_READY_TIMEOUT_SECS));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn u2_log_keep_rotations_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::set_var(ENV_LOG_KEEP_ROTATIONS, "20");
        }
        let mut cfg = DaemonConfig::default();
        let result = cfg.apply_env_overrides();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_LOG_KEEP_ROTATIONS);
        }
        result.expect("override ok");
        assert_eq!(cfg.log_keep_rotations, 20);
    }

    #[test]
    fn u2_log_keep_rotations_env_override_rejects_malformed() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::set_var(ENV_LOG_KEEP_ROTATIONS, "bad");
        }
        let mut cfg = DaemonConfig::default();
        let err = cfg.apply_env_overrides();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            env::remove_var(ENV_LOG_KEEP_ROTATIONS);
        }
        let err = err.expect_err("malformed value must fail");
        match err {
            DaemonError::Config { path, .. } => {
                assert_eq!(path, Path::new(ENV_LOG_KEEP_ROTATIONS));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn u2_validate_auto_start_ready_timeout_range() {
        // Zero is rejected.
        let zero = DaemonConfig {
            auto_start_ready_timeout_secs: 0,
            ..DaemonConfig::default()
        };
        assert!(zero.validate().is_err(), "0 must be rejected");

        // 61 exceeds the max of 60.
        let over = DaemonConfig {
            auto_start_ready_timeout_secs: 61,
            ..DaemonConfig::default()
        };
        assert!(over.validate().is_err(), "61 must be rejected");

        // Boundary values must pass.
        let min = DaemonConfig {
            auto_start_ready_timeout_secs: 1,
            ..DaemonConfig::default()
        };
        assert!(min.validate().is_ok(), "1 must be valid");

        let max = DaemonConfig {
            auto_start_ready_timeout_secs: 60,
            ..DaemonConfig::default()
        };
        assert!(max.validate().is_ok(), "60 must be valid");
    }

    #[test]
    fn u2_validate_log_keep_rotations_range() {
        // Zero is rejected.
        let zero = DaemonConfig {
            log_keep_rotations: 0,
            ..DaemonConfig::default()
        };
        assert!(zero.validate().is_err(), "0 must be rejected");

        // 101 exceeds the max of 100.
        let over = DaemonConfig {
            log_keep_rotations: 101,
            ..DaemonConfig::default()
        };
        assert!(over.validate().is_err(), "101 must be rejected");

        // Boundary values must pass.
        let min = DaemonConfig {
            log_keep_rotations: 1,
            ..DaemonConfig::default()
        };
        assert!(min.validate().is_ok(), "1 must be valid");

        let max = DaemonConfig {
            log_keep_rotations: 100,
            ..DaemonConfig::default()
        };
        assert!(max.validate().is_ok(), "100 must be valid");
    }

    #[test]
    fn u2_from_toml_str_round_trip_new_fields() {
        let text = r#"
            auto_start_ready_timeout_secs = 45
            log_keep_rotations = 10
            install_user_service = true
        "#;
        let cfg = DaemonConfig::from_toml_str(text).expect("parse");
        assert_eq!(cfg.auto_start_ready_timeout_secs, 45);
        assert_eq!(cfg.log_keep_rotations, 10);
        assert!(cfg.install_user_service);
    }

    #[test]
    fn u2_from_toml_str_new_fields_default_when_absent() {
        // None of the new fields are present — they must fall back to defaults.
        let text = r"memory_limit_mb = 1024";
        let cfg = DaemonConfig::from_toml_str(text).expect("parse");
        assert_eq!(
            cfg.auto_start_ready_timeout_secs,
            DEFAULT_AUTO_START_READY_TIMEOUT_SECS
        );
        assert_eq!(cfg.log_keep_rotations, DEFAULT_LOG_KEEP_ROTATIONS);
        assert!(!cfg.install_user_service);
    }

    #[test]
    fn u2_install_user_service_defaults_false_and_is_tolerated_by_validate() {
        // install_user_service is a no-op bool; validate must not reject any
        // value for it (both true and false are permanently valid).
        let with_true = DaemonConfig {
            install_user_service: true,
            ..DaemonConfig::default()
        };
        assert!(
            with_true.validate().is_ok(),
            "install_user_service=true must pass validate"
        );
        let with_false = DaemonConfig {
            install_user_service: false,
            ..DaemonConfig::default()
        };
        assert!(
            with_false.validate().is_ok(),
            "install_user_service=false must pass validate"
        );
    }
}

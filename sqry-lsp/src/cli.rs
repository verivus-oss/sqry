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

    /// Explicit index root (defaults to first workspace folder containing .sqry-index)
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
}

impl LspOptions {
    /// Determine if stdio transport should be used (default when no socket is provided).
    #[must_use]
    pub fn use_stdio(&self) -> bool {
        self.stdio || self.socket.is_none()
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
        };

        assert!(opts.allow_public_bind);
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

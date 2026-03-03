//! Security validation for LSP socket bindings.
//!
//! This module provides security checks for network socket bindings to prevent
//! accidental exposure of the LSP server to untrusted networks.

use log::{debug, warn};
use std::net::{IpAddr, SocketAddr};

/// Security classification for socket bind addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindSecurity {
    /// Localhost binding (127.0.0.1 or `::1`) - secure for all environments
    Localhost,
    /// Private network binding (10.x.x.x, 172.16-31.x.x, 192.168.x.x) - caution on shared networks
    PrivateNetwork,
    /// Public/wildcard binding (0.0.0.0, ::, or public IP) - exposed to all network interfaces
    Public,
}

impl BindSecurity {
    /// Classify a socket address based on its IP address.
    pub fn classify(addr: SocketAddr) -> Self {
        match addr.ip() {
            IpAddr::V4(ipv4) => {
                if ipv4.is_loopback() {
                    BindSecurity::Localhost
                } else if ipv4.is_unspecified() || ipv4.is_private() {
                    // 0.0.0.0 binds to all interfaces (public)
                    // Private IPs: 10.x.x.x, 172.16-31.x.x, 192.168.x.x
                    if ipv4.is_unspecified() {
                        BindSecurity::Public
                    } else {
                        BindSecurity::PrivateNetwork
                    }
                } else {
                    BindSecurity::Public
                }
            }
            IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() {
                    BindSecurity::Localhost
                } else if ipv6.is_unspecified() {
                    // :: binds to all interfaces
                    BindSecurity::Public
                } else {
                    // IPv6 private/public distinction is more complex
                    // For safety, classify non-localhost as public
                    BindSecurity::Public
                }
            }
        }
    }

    /// Returns true if this binding type is considered safe by default.
    #[allow(dead_code)] // Public API for future use
    pub fn is_safe(self) -> bool {
        matches!(self, BindSecurity::Localhost)
    }

    /// Returns a human-readable description of the security implications.
    pub fn description(self) -> &'static str {
        match self {
            BindSecurity::Localhost => "localhost only (secure)",
            BindSecurity::PrivateNetwork => "private network (caution on shared networks)",
            BindSecurity::Public => "all network interfaces (exposed to LAN/WAN)",
        }
    }
}

/// Validate and warn about potentially unsafe socket bindings.
///
/// This function logs security warnings when the LSP server is bound to
/// non-localhost addresses, which may expose sensitive development data
/// to other hosts on the network.
///
/// # Arguments
///
/// * `addr` - The socket address the LSP server is binding to
/// * `allow_suppress` - If true, warnings are suppressed and logged at DEBUG level instead
///
/// # Security Considerations
///
/// The LSP protocol transmits:
/// - Source code and file contents
/// - Symbol information and call graphs
/// - Search queries and results
/// - Workspace configurations
///
/// Binding to non-localhost addresses without authentication can expose
/// this data to other hosts on the network.
///
/// When `allow_suppress` is true, the caller acknowledges the security implications
/// and warnings are logged at DEBUG level for audit purposes.
pub fn validate_bind_address(addr: SocketAddr, allow_suppress: bool) {
    let security = BindSecurity::classify(addr);

    match security {
        BindSecurity::Localhost => {
            // Safe - no warning needed
        }
        BindSecurity::PrivateNetwork => {
            if allow_suppress {
                debug!(
                    "Security validation suppressed (--allow-public-bind): LSP server binding to private network address {} ({})",
                    addr,
                    security.description()
                );
            } else {
                warn!("Security Notice: LSP server is binding to private network address {addr}");
                warn!(
                    "  This exposes the server to other hosts on your local network ({})",
                    security.description()
                );
                warn!(
                    "  Recommendation: Use '127.0.0.1:{}' for localhost-only access",
                    addr.port()
                );
                warn!("  Or ensure your network is trusted (e.g., home network with firewall)");
                warn!(
                    "  To suppress this warning, use --allow-public-bind flag or SQRY_LSP_ALLOW_PUBLIC_BIND=1"
                );
            }
        }
        BindSecurity::Public => {
            if allow_suppress {
                debug!(
                    "Security validation suppressed (--allow-public-bind): LSP server binding to {} ({})",
                    addr,
                    security.description()
                );
            } else {
                warn!(
                    "SECURITY WARNING: LSP server is binding to {} ({})",
                    addr,
                    security.description()
                );
                warn!("  This exposes the LSP server to ALL network interfaces, including:");
                warn!("  - Local Area Network (LAN) hosts");
                warn!("  - Wide Area Network (WAN) if publicly routable");
                warn!("  - Untrusted devices on shared networks (coffee shops, airports, etc.)");
                warn!("");
                warn!(
                    "  STRONG RECOMMENDATION: Use '127.0.0.1:{}' instead",
                    addr.port()
                );
                warn!("  The LSP protocol transmits source code and has no authentication.");
                warn!(
                    "  To suppress this warning, use --allow-public-bind flag or SQRY_LSP_ALLOW_PUBLIC_BIND=1"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn classify_ipv4_localhost() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Localhost);
        assert!(BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv4_loopback_range() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3)), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Localhost);
        assert!(BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv4_unspecified() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Public);
        assert!(!BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv4_private_network() {
        // 10.0.0.0/8
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 9257);
        assert_eq!(BindSecurity::classify(addr1), BindSecurity::PrivateNetwork);
        assert!(!BindSecurity::classify(addr1).is_safe());

        // 172.16.0.0/12
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), 9257);
        assert_eq!(BindSecurity::classify(addr2), BindSecurity::PrivateNetwork);

        // 192.168.0.0/16
        let addr3 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 9257);
        assert_eq!(BindSecurity::classify(addr3), BindSecurity::PrivateNetwork);
    }

    #[test]
    fn classify_ipv4_public() {
        // Public IP (Google DNS)
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Public);
        assert!(!BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv6_localhost() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Localhost);
        assert!(BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv6_unspecified() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 9257);
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Public);
        assert!(!BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn classify_ipv6_public() {
        // Google Public DNS IPv6
        let addr = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
            9257,
        );
        assert_eq!(BindSecurity::classify(addr), BindSecurity::Public);
        assert!(!BindSecurity::classify(addr).is_safe());
    }

    #[test]
    fn security_descriptions() {
        assert_eq!(
            BindSecurity::Localhost.description(),
            "localhost only (secure)"
        );
        assert_eq!(
            BindSecurity::PrivateNetwork.description(),
            "private network (caution on shared networks)"
        );
        assert_eq!(
            BindSecurity::Public.description(),
            "all network interfaces (exposed to LAN/WAN)"
        );
    }

    #[test]
    fn validate_localhost_no_panic() {
        // Should not panic or log warnings (but we can't easily test log output)
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9257);
        validate_bind_address(addr, false);
        validate_bind_address(addr, true); // Should also work with suppression
    }

    #[test]
    fn validate_private_network_no_panic() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 9257);
        validate_bind_address(addr, false); // With warnings
        validate_bind_address(addr, true); // Suppressed
    }

    #[test]
    fn validate_public_no_panic() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9257);
        validate_bind_address(addr, false); // With warnings
        validate_bind_address(addr, true); // Suppressed
    }

    #[test]
    fn validate_private_network_suppression() {
        // Test that suppression doesn't panic and changes behavior
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 9257);
        // Both should complete without panic
        validate_bind_address(addr, false);
        validate_bind_address(addr, true);
    }

    #[test]
    fn validate_public_suppression() {
        // Test that suppression doesn't panic for most severe case
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9257);
        // Both should complete without panic
        validate_bind_address(addr, false);
        validate_bind_address(addr, true);
    }
}

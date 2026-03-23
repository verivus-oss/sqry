# Security Guide for sqry-lsp

## Overview

The sqry Language Server Protocol (LSP) implementation provides semantic code analysis capabilities to editors. This document outlines security considerations for deploying and using sqry-lsp.

## Transport Security

### Default: stdio (Recommended)

By default, sqry-lsp uses stdio (standard input/output) for communication:

```bash
sqry-lsp
# Or explicitly:
sqry-lsp --stdio
```

**Security:** ✅ **Secure** - stdio is the recommended transport for LSP servers. Communication is local to the process and cannot be intercepted over the network.

### Optional: TCP Socket

For advanced use cases (remote development, debugging), sqry-lsp supports TCP sockets:

```bash
# Localhost only (RECOMMENDED)
sqry-lsp --socket 127.0.0.1:9257

# IPv6 localhost
sqry-lsp --socket [::1]:9257
```

## Security Warnings

### ⚠️ Network Binding Security

When using `--socket`, sqry-lsp will validate your bind address and issue warnings:

### Suppressing Security Warnings

For CI/CD pipelines, containerized deployments, or environments where non-localhost binding is intentional and understood, you can suppress security warnings:

```bash
# Using CLI flag
sqry-lsp --socket 0.0.0.0:9257 --allow-public-bind

# Using environment variable
export SQRY_LSP_ALLOW_PUBLIC_BIND=1
sqry-lsp --socket 0.0.0.0:9257
```

**⚠️ WARNING**: This flag suppresses warnings but does NOT add security. It only acknowledges that you understand the risks of exposing the LSP server to non-localhost addresses.

**When to use `--allow-public-bind`**:
- ✅ CI/CD pipelines where logs need to be clean
- ✅ Docker containers with explicit port mapping to localhost
- ✅ Kubernetes pods with network policies enforcing security
- ✅ Internal networks with firewall protection
- ❌ **NEVER** on public/untrusted networks
- ❌ **NEVER** on personal laptops in coffee shops, airports, etc.

**Audit Trail**: When this flag is set, suppression is still logged at DEBUG level for security audits:
```bash
sqry-lsp --socket 0.0.0.0:9257 --allow-public-bind --log-level debug
# Logs: "Security validation suppressed (--allow-public-bind): LSP server binding to 0.0.0.0:9257"
```

#### ✅ Localhost Binding (Secure)
```bash
sqry-lsp --socket 127.0.0.1:9257
```
- **Security:** Secure for all environments
- **Accessible by:** Only processes on the same machine
- **Warning:** None

#### ⚠️ Private Network Binding (Caution)
```bash
sqry-lsp --socket 192.168.1.100:9257
```
- **Security:** Exposed to local network
- **Accessible by:** Any device on your LAN (home/office network)
- **Warning:** Logged at startup
- **Safe when:** On trusted home networks with firewall protection
- **Unsafe when:** On shared networks (coworking spaces, coffee shops, airports)

#### 🚨 Public/Wildcard Binding (Dangerous)
```bash
sqry-lsp --socket 0.0.0.0:9257
sqry-lsp --socket [::]:9257
```
- **Security:** Exposed to ALL network interfaces
- **Accessible by:** Any device that can reach your machine (LAN, WAN, internet)
- **Warning:** Strong security warning logged at startup
- **Risk:** Source code and workspace data exposed without authentication

## Data Exposure

The LSP protocol transmits the following data:

### Sensitive Information
- **Source code contents** (full files)
- **Symbol definitions** (functions, classes, variables)
- **Code structure** (call graphs, dependencies)
- **Search queries** (what you're looking for)
- **Workspace paths** (directory structure)

### No Authentication

The LSP protocol **does not include authentication**. Any client that can connect to the socket can:
- Read your source code
- Execute queries
- Access symbol information

## Threat Model

### ✅ Protected Against (with localhost binding)
- Network eavesdropping
- Remote code execution via network
- Unauthorized access from other machines

### ❌ NOT Protected Against
- Local privilege escalation (any process on your machine can connect)
- Physical access (if attacker has local shell access)
- Editor/IDE vulnerabilities (LSP server trusts the client)

## Best Practices

### 1. Default to stdio
Unless you have a specific need for network sockets, use the default stdio transport:
```bash
sqry-lsp  # Uses stdio by default
```

### 2. Bind to localhost only
If you must use sockets, always bind to localhost:
```bash
sqry-lsp --socket 127.0.0.1:9257
```

### 3. Never use 0.0.0.0 on untrusted networks
The `0.0.0.0` wildcard binds to **all** network interfaces:
```bash
# ❌ DANGEROUS - Exposes to entire network
sqry-lsp --socket 0.0.0.0:9257
```

### 4. Use firewall rules
If you need non-localhost binding, add firewall rules:
```bash
# Example: Allow only specific IP
sudo ufw allow from 192.168.1.50 to any port 9257

# Or: Deny all incoming on the port
sudo ufw deny 9257
```

### 5. Monitor logs
sqry-lsp logs all connections. Monitor for unexpected clients:
```bash
sqry-lsp --socket 127.0.0.1:9257 --log-level info
```

## Security Checklist

Before deploying sqry-lsp:

- [ ] Using stdio transport (default)?
- [ ] If using socket: Bound to 127.0.0.1 or ::1?
- [ ] If using LAN binding: On a trusted network?
- [ ] If using LAN binding: Firewall rules configured?
- [ ] Logs monitored for unexpected connections?
- [ ] Not binding to 0.0.0.0 or :: on public networks?
- [ ] If using `--allow-public-bind`: Deployment is in controlled environment (CI/CD, containers)?
- [ ] If suppressing warnings: Audit logs enabled (`--log-level debug`)?

## Remote Development Scenarios

### Scenario 1: SSH Port Forwarding (Recommended)

Instead of binding to a network interface, use SSH tunneling:

```bash
# On remote machine: Use stdio or localhost
ssh remote-host
sqry-lsp --socket 127.0.0.1:9257

# On local machine: Forward port
ssh -L 9257:127.0.0.1:9257 remote-host

# Configure editor to connect to localhost:9257
```

**Security:** ✅ Encrypted, authenticated via SSH

### Scenario 2: Docker/Containers

When running in containers, bind to localhost and expose via port mapping:

```dockerfile
# Dockerfile
CMD ["sqry-lsp", "--socket", "127.0.0.1:9257"]
```

```bash
# Docker run (maps container localhost to host localhost)
docker run -p 127.0.0.1:9257:9257 sqry-lsp-image
```

For CI/CD containers where warnings clutter logs:
```dockerfile
# Dockerfile for CI/CD
ENV SQRY_LSP_ALLOW_PUBLIC_BIND=1
CMD ["sqry-lsp", "--socket", "0.0.0.0:9257"]
```

```bash
# Docker Compose (isolated network)
version: '3'
services:
  sqry-lsp:
    image: sqry-lsp-image
    environment:
      - SQRY_LSP_ALLOW_PUBLIC_BIND=1
    command: sqry-lsp --socket 0.0.0.0:9257
    networks:
      - build-network
networks:
  build-network:
    internal: true  # No external access
```

### Scenario 3: VPN/Trusted Network

If you must bind to a LAN address, use a VPN or trusted network:

```bash
# Only on VPN or isolated network
sqry-lsp --socket 10.0.1.50:9257
```

## Reporting Security Issues

If you discover a security vulnerability in sqry-lsp:

1. **Do NOT** open a public GitHub issue
2. Contact the maintainers privately (see main repository SECURITY.md)
3. Provide details:
   - Vulnerability description
   - Steps to reproduce
   - Impact assessment
   - Suggested fix (if any)

## Version History

- **v1.10.0** ( Phase A): Added `--allow-public-bind` flag for warning suppression in CI/CD
- **v1.10.0**: Added `SQRY_LSP_ALLOW_PUBLIC_BIND` environment variable support
- **v1.10.0**: Security warnings now include suppression instructions
- **v1.9.0**: Added socket binding security validation warnings
- **Earlier versions**: No security warnings for non-localhost bindings

## Additional Resources

- [LSP Specification](https://microsoft.github.io/language-server-protocol/)
- [OWASP Secure Coding Practices](https://owasp.org/www-project-secure-coding-practices-quick-reference-guide/)
- [Rust Security Guidelines](https://anssi-fr.github.io/rust-guide/)

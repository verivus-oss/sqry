# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest  | ✅        |
| < Latest | ❌       |

Only the latest release of sqry receives security updates. We recommend always running the most recent version.

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

Please report security vulnerabilities via [GitHub Private Vulnerability Reporting](https://github.com/verivus-oss/sqry/security/advisories/new).

You will receive an acknowledgement within 48 hours. We aim to provide a fix or mitigation within 7 days for critical issues.

## Security Practices

- Every release binary is signed with [Sigstore](https://www.sigstore.dev/) keyless signing
- SLSA Level 2 provenance attestations ship with every release
- CycloneDX and SPDX SBOMs are generated for every release
- All dependencies are audited via `cargo-vet` with imports from Mozilla, Google, and Bytecode Alliance
- The query parser is continuously fuzzed with libFuzzer and AddressSanitizer
- Weekly `cargo-geiger` audits track all `unsafe` code
- Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org)

## Verification

```bash
# Verify binary signature
cosign verify-blob --bundle sqry-<platform>.bundle sqry-<platform>

# Verify SLSA provenance
slsa-verifier verify-artifact sqry-<platform> \
  --provenance-path sqry-<platform>-provenance.intoto.jsonl \
  --source-uri github.com/verivus-oss/sqry
```

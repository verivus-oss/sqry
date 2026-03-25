#!/usr/bin/env bash
set -euo pipefail

REPO="verivus-oss/sqry"
VERSION_TAG="latest"
COMPONENT="sqry"
INSTALL_DIR="${HOME}/.local/bin"
VERIFY_CHECKSUMS=true
VERIFY_SIGNATURES=false

usage() {
  cat <<USAGE
sqry installer (Linux/macOS)

Usage: $(basename "$0") [options]

Options:
  --version TAG         Release tag to install (default: latest)
  --component NAME      One of: sqry, sqry-mcp, sqry-lsp, all (default: sqry)
  --install-dir DIR     Install destination (default: ~/.local/bin)
  --repo OWNER/REPO     GitHub repository (default: verivus-oss/sqry)
  --no-checksum         Skip checksum verification (not recommended)
  --verify-signatures   Verify Cosign bundles (requires cosign)
  -h, --help            Show this help message
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION_TAG="$2"
      shift 2
      ;;
    --component)
      COMPONENT="$2"
      shift 2
      ;;
    --install-dir)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --repo)
      REPO="$2"
      shift 2
      ;;
    --no-checksum)
      VERIFY_CHECKSUMS=false
      shift
      ;;
    --verify-signatures)
      VERIFY_SIGNATURES=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

if [[ "$VERIFY_CHECKSUMS" == true ]]; then
  if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    echo "error: sha256sum or shasum is required for checksum verification" >&2
    exit 1
  fi
fi

if [[ "$VERIFY_SIGNATURES" == true ]] && ! command -v cosign >/dev/null 2>&1; then
  echo "error: cosign is required for --verify-signatures" >&2
  exit 1
fi

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)

case "$arch" in
  x86_64|amd64)
    arch="x86_64"
    ;;
  aarch64|arm64)
    arch="arm64"
    ;;
  *)
    echo "error: unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

platform_suffix=""
checksum_file="CHECKSUMS.sha256"

case "$os" in
  linux)
    if [[ "$arch" == "x86_64" ]]; then
      platform_suffix="linux-x86_64"
    else
      platform_suffix="linux-arm64"
    fi
    ;;
  darwin)
    if [[ "$arch" != "arm64" ]]; then
      echo "error: macOS builds are currently published for ARM64 only" >&2
      exit 1
    fi
    platform_suffix="macos-arm64"
    ;;
  *)
    echo "error: unsupported operating system: $os" >&2
    echo "For Windows, use scripts/install.ps1 or the sqry-windows-x86_64.zip release asset." >&2
    exit 1
    ;;
esac

case "$COMPONENT" in
  sqry|sqry-mcp|sqry-lsp|all)
    ;;
  *)
    echo "error: invalid component '$COMPONENT' (expected sqry, sqry-mcp, sqry-lsp, all)" >&2
    exit 1
    ;;
esac

if [[ "$VERSION_TAG" == "latest" ]]; then
  VERSION_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | sed -n 's/.*"tag_name": "\([^"]*\)".*/\1/p' | head -n1)
  if [[ -z "$VERSION_TAG" ]]; then
    echo "error: failed to resolve latest release tag from GitHub API" >&2
    exit 1
  fi
fi

if [[ ! "$VERSION_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: version tag must be v<MAJOR>.<MINOR>.<PATCH>, got '$VERSION_TAG'" >&2
  exit 1
fi

if [[ ! "$REPO" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
  echo "error: --repo must match OWNER/REPO, got '$REPO'" >&2
  exit 1
fi

release_base="https://github.com/${REPO}/releases/download/${VERSION_TAG}"
oidc_issuer="https://token.actions.githubusercontent.com"
repo_regex="${REPO//./\\.}"
version_escaped="${VERSION_TAG//./\\.}"
cert_identity="^https://github\\.com/${repo_regex}/\\.github/workflows/oss-leg3-release\\.yml@refs/tags/${version_escaped}$"

version_num="${VERSION_TAG#v}"
tarball_name="sqry-${version_num}-${platform_suffix}.tar.gz"

asset_name_for_component() {
  # v2 pipeline: components are inside a platform tarball, not separate downloads
  local component_name="$1"
  case "$component_name" in
    sqry|sqry-mcp|sqry-lsp) printf '%s\n' "$component_name" ;;
    *)
      echo "error: unsupported component name '$component_name'" >&2
      exit 1
      ;;
  esac
}

sha256_of_file() {
  local file_path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file_path" | awk '{print $1}'
  else
    shasum -a 256 "$file_path" | awk '{print $1}'
  fi
}

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

# Download and verify the platform tarball
checksum_path="$tmp_dir/$checksum_file"
if [[ "$VERIFY_CHECKSUMS" == true ]]; then
  echo "Downloading checksums: $checksum_file"
  curl -fsSL "${release_base}/${checksum_file}" -o "$checksum_path"
fi

echo "Downloading ${tarball_name}..."
curl -fsSL "${release_base}/${tarball_name}" -o "$tmp_dir/$tarball_name"

if [[ "$VERIFY_CHECKSUMS" == true ]]; then
  expected_sha=$(awk -v name="$tarball_name" '$2 ~ name {print $1}' "$checksum_path")
  if [[ -z "$expected_sha" ]]; then
    echo "error: missing checksum entry for '$tarball_name' in '$checksum_file'" >&2
    exit 1
  fi
  actual_sha=$(sha256_of_file "$tmp_dir/$tarball_name")
  if [[ "$expected_sha" != "$actual_sha" ]]; then
    echo "error: checksum mismatch for $tarball_name" >&2
    echo "expected: $expected_sha" >&2
    echo "actual:   $actual_sha" >&2
    exit 1
  fi
  echo "Checksum verified: $tarball_name"
fi

if [[ "$VERIFY_SIGNATURES" == true ]]; then
  echo "Verifying Cosign bundle: ${tarball_name}.bundle"
  curl -fsSL "${release_base}/${tarball_name}.bundle" -o "$tmp_dir/${tarball_name}.bundle"
  cosign verify-blob \
    --bundle "$tmp_dir/${tarball_name}.bundle" \
    --certificate-identity-regexp "$cert_identity" \
    --certificate-oidc-issuer "$oidc_issuer" \
    "$tmp_dir/$tarball_name" >/dev/null
  echo "Signature verified: $tarball_name"
fi

# Extract tarball
tar xzf "$tmp_dir/$tarball_name" -C "$tmp_dir"

install_component() {
  local component_name="$1"
  if [[ ! -f "$tmp_dir/$component_name" ]]; then
    echo "error: component '$component_name' not found in tarball" >&2
    exit 1
  fi
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$tmp_dir/$component_name" "$INSTALL_DIR/$component_name"
  echo "Installed: $INSTALL_DIR/$component_name"
}

if [[ "$COMPONENT" == "all" ]]; then
  install_component "sqry"
  install_component "sqry-mcp"
  install_component "sqry-lsp"
else
  install_component "$COMPONENT"
fi

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo ""
  echo "note: '$INSTALL_DIR' is not in PATH"
  echo "Add this line to your shell profile:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

echo ""
echo "Installation complete: ${VERSION_TAG} (${COMPONENT})"

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

checksum_file="SHA256SUMS.txt"

case "$os" in
  linux)
    platform_suffix="linux-${arch}"
    ;;
  darwin)
    platform_suffix="macos-${arch}"
    ;;
  *)
    echo "error: unsupported operating system: $os" >&2
    echo "For Windows, use scripts/install.ps1 or download from GitHub Releases." >&2
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
cert_identity="^https://github\\.com/${repo_regex}/\\.github/workflows/oss-distribute\\.yml@refs/tags/${version_escaped}$"

# Map component name to release asset name
asset_name_for_component() {
  local component_name="$1"
  printf '%s-%s\n' "$component_name" "$platform_suffix"
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

# Download checksums once
checksum_path="$tmp_dir/$checksum_file"
if [[ "$VERIFY_CHECKSUMS" == true ]]; then
  echo "Downloading checksums: $checksum_file"
  curl -fsSL "${release_base}/${checksum_file}" -o "$checksum_path"
fi

# Download, verify, and install a single component binary
download_and_install() {
  local component_name="$1"
  local asset_name
  asset_name=$(asset_name_for_component "$component_name")
  local asset_path="$tmp_dir/$asset_name"

  echo "Downloading ${asset_name}..."
  curl -fsSL "${release_base}/${asset_name}" -o "$asset_path"

  if [[ "$VERIFY_CHECKSUMS" == true ]]; then
    # SHA256SUMS.txt uses "hash  filename" format (two spaces)
    expected_sha=$(awk -v name="$asset_name" '$2 == name || $2 == "./"name { print $1 }' "$checksum_path")
    if [[ -z "$expected_sha" ]]; then
      echo "error: missing checksum entry for '$asset_name' in '$checksum_file'" >&2
      exit 1
    fi
    actual_sha=$(sha256_of_file "$asset_path")
    if [[ "$expected_sha" != "$actual_sha" ]]; then
      echo "error: checksum mismatch for $asset_name" >&2
      echo "expected: $expected_sha" >&2
      echo "actual:   $actual_sha" >&2
      exit 1
    fi
    echo "Checksum verified: $asset_name"
  fi

  if [[ "$VERIFY_SIGNATURES" == true ]]; then
    local bundle_name="${asset_name}.bundle"
    echo "Verifying Cosign bundle: $bundle_name"
    if curl -fsSL "${release_base}/${bundle_name}" -o "$tmp_dir/${bundle_name}" 2>/dev/null; then
      cosign verify-blob \
        --bundle "$tmp_dir/${bundle_name}" \
        --certificate-identity-regexp "$cert_identity" \
        --certificate-oidc-issuer "$oidc_issuer" \
        "$asset_path" >/dev/null
      echo "Signature verified: $asset_name"
    else
      echo "warning: no Cosign bundle found for $asset_name, skipping signature verification" >&2
    fi
  fi

  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$asset_path" "$INSTALL_DIR/$component_name"
  echo "Installed: $INSTALL_DIR/$component_name"
}

if [[ "$COMPONENT" == "all" ]]; then
  download_and_install "sqry"
  download_and_install "sqry-mcp"
  download_and_install "sqry-lsp"
else
  download_and_install "$COMPONENT"
fi

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo ""
  echo "note: '$INSTALL_DIR' is not in PATH"
  echo "Add this line to your shell profile:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

echo ""
echo "Installation complete: ${VERSION_TAG} (${COMPONENT})"

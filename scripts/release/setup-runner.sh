#!/usr/bin/env bash
set -euo pipefail

# setup-runner.sh - One-time setup for self-hosted GitHub Actions release runners.
# Installs release toolchain dependencies for the public Stage 6 cross-build.
# Idempotent: safe to re-run. Must be run as root.

ZIG_VERSION="0.15.2"
ZIG_INSTALL_ROOT="/opt/zig"
ZIG_INSTALL_DIR="${ZIG_INSTALL_ROOT}/zig-${ZIG_VERSION}"
ZIG_SHA256_X86_64="02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239"
ZIG_SHA256_AARCH64="958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f"
NODE_MAJOR=24
OPENSUSE_LEAP_AARCH64_GLIBC_RPM_URL="https://download.opensuse.org/distribution/leap/16.0/repo/oss/aarch64/glibc-2.40-160000.4.1.aarch64.rpm"
OPENSUSE_LEAP_AARCH64_GLIBC_RPM_SHA256="bcca70b763355f8251b6917db5fe425c0700d905828fb17faf33092fc48bca33"

if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
    export PATH="/home/${SUDO_USER}/.cargo/bin:/usr/local/bin:/opt/zig/current:${PATH}"
else
    export PATH="${HOME}/.cargo/bin:/usr/local/bin:/opt/zig/current:${PATH}"
fi

MACOS_SDK_VERSION="11.3"
MACOS_SDK_SHA256="cd4f08a75577145b8f05245a2975f7c81401d75e9535dcffbb879ee1deefcbf4"
MACOS_SDK_ROOT="/opt/macos-sdk"
MACOS_SDK_DIR="${MACOS_SDK_ROOT}/MacOSX${MACOS_SDK_VERSION}.sdk"
MACOS_SDK_COMPAT_DIR="${MACOS_SDK_ROOT}/MacOSX14.0.sdk"

RELEASE_TOOLS_ROOT="/opt/sqry-release-tools"
MINGW_PACKAGES=(
    cross-aarch64-glibc-devel
    cross-aarch64-linux-glibc-devel
    mingw64-binutils
    mingw64-cross-binutils
    mingw64-gcc
    mingw64-gcc-c++
    mingw64-cross-gcc
    mingw64-cross-gcc-c++
    mingw64-headers
    mingw64-runtime
    mingw64-libgcc_s_seh1
    mingw64-libstdc++6
    mingw64-libwinpthread1
    mingw64-winpthreads-devel
    libfl2
    libunwind8
    qemu-linux-user
    wine
)

info() { printf '\033[1;34m[INFO]\033[0m  %s\n' "$*"; }
ok() { printf '\033[1;32m[OK]\033[0m    %s\n' "$*"; }
warn() { printf '\033[1;33m[WARN]\033[0m  %s\n' "$*"; }
err() { printf '\033[1;31m[ERR]\033[0m   %s\n' "$*"; exit 1; }

check_root() {
    [[ "${EUID}" -eq 0 ]] || err "This script must be run as root."
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

print_version() {
    local name="$1"
    shift
    local output
    if ! output="$("$@" 2>&1)"; then
        err "${name} version check failed: ${output}"
    fi
    ok "${name} installed: $(head -1 <<<"$output")"
}

download_with_sha256() {
    local url="$1"
    local output="$2"
    local expected_sha256="$3"

    curl -fSL --retry 3 -o "$output" "$url"
    local actual_sha256
    actual_sha256="$(sha256sum "$output" | awk '{print $1}')"
    [[ "$actual_sha256" == "$expected_sha256" ]] || {
        rm -f "$output"
        err "Checksum mismatch for ${url}. Expected ${expected_sha256}, got ${actual_sha256}"
    }
}

install_apt_packages() {
    info "Installing release packages with apt."
    local packages=()

    command_exists curl || packages+=(curl)
    command_exists jq || packages+=(jq)
    command_exists python3 || packages+=(python3)
    command_exists objdump || packages+=(binutils)
    command_exists wine64 || packages+=(wine64)
    command_exists qemu-aarch64-static || packages+=(qemu-user-static)
    command_exists x86_64-w64-mingw32-gcc || packages+=(gcc-mingw-w64-x86-64)
    command_exists x86_64-w64-mingw32-g++ || packages+=(g++-mingw-w64-x86-64)
    dpkg -s ca-certificates >/dev/null 2>&1 || packages+=(ca-certificates)
    dpkg -s libc6-arm64-cross >/dev/null 2>&1 || packages+=(libc6-arm64-cross)

    if [[ "${#packages[@]}" -gt 0 ]]; then
        apt-get update -qq
        apt-get install -y -qq "${packages[@]}"
    fi

    ok "apt package check complete."
}

extract_rpm() {
    local rpm="$1"
    rpm2cpio "$rpm" | cpio -idmu -D "$RELEASE_TOOLS_ROOT" >/dev/null
}

install_zypper_release_tools() {
    info "Installing release packages with zypper RPM extraction."
    command_exists zypper || err "zypper was not found."
    command_exists curl || err "curl was not found."
    command_exists rpm2cpio || err "rpm2cpio was not found."
    command_exists cpio || err "cpio was not found."

    mkdir -p "$RELEASE_TOOLS_ROOT"
    local cache
    cache="$(mktemp -d)"
    trap 'rm -rf "${cache:-}"' RETURN

    XDG_CACHE_HOME="$cache" zypper --non-interactive download "${MINGW_PACKAGES[@]}"

    while IFS= read -r rpm; do
        info "Extracting ${rpm##*/}"
        extract_rpm "$rpm"
    done < <(find "$cache" -name '*.rpm' -print | sort)

    local aarch64_glibc_rpm="${cache}/glibc-aarch64.rpm"
    info "Downloading openSUSE aarch64 glibc runtime RPM."
    download_with_sha256 \
        "$OPENSUSE_LEAP_AARCH64_GLIBC_RPM_URL" \
        "$aarch64_glibc_rpm" \
        "$OPENSUSE_LEAP_AARCH64_GLIBC_RPM_SHA256"
    extract_rpm "$aarch64_glibc_rpm"

    install_mingw_wrappers
    rm -rf "$cache"
    trap - RETURN
    ok "zypper package extraction complete."
}

install_system_packages() {
    if command_exists apt-get; then
        install_apt_packages
    elif command_exists zypper; then
        install_zypper_release_tools
    else
        err "Unsupported host package manager. Expected apt-get or zypper."
    fi

    print_version "jq" jq --version
    print_version "python3" python3 --version
}

install_zig() {
    info "Checking Zig ${ZIG_VERSION}."
    if command_exists zig && [[ "$(zig version 2>/dev/null)" == "$ZIG_VERSION" ]]; then
        ok "Zig ${ZIG_VERSION} already installed."
        return
    fi

    local arch
    arch="$(uname -m)"
    local expected_sha256
    case "$arch" in
        x86_64) expected_sha256="$ZIG_SHA256_X86_64" ;;
        aarch64) expected_sha256="$ZIG_SHA256_AARCH64" ;;
        *) err "Unsupported architecture for Zig: ${arch}" ;;
    esac

    local tarball="zig-${arch}-linux-${ZIG_VERSION}.tar.xz"
    local url="https://ziglang.org/download/${ZIG_VERSION}/${tarball}"
    local tmpdir
    tmpdir="$(mktemp -d)"

    info "Downloading ${url}"
    download_with_sha256 "$url" "${tmpdir}/${tarball}" "$expected_sha256"

    rm -rf "$ZIG_INSTALL_DIR"
    mkdir -p "$ZIG_INSTALL_ROOT"
    tar -xf "${tmpdir}/${tarball}" -C "$ZIG_INSTALL_ROOT"
    mv "${ZIG_INSTALL_ROOT}/zig-${arch}-linux-${ZIG_VERSION}" "$ZIG_INSTALL_DIR"
    ln -sfn "$ZIG_INSTALL_DIR" "${ZIG_INSTALL_ROOT}/current"
    ln -sf "${ZIG_INSTALL_ROOT}/current/zig" /usr/local/bin/zig
    rm -rf "$tmpdir"

    print_version "Zig" zig version
}

install_macos_sdk() {
    info "Checking macOS SDK ${MACOS_SDK_VERSION}."
    if [[ -d "${MACOS_SDK_DIR}/System/Library/Frameworks/CoreFoundation.framework" ]]; then
        ok "macOS SDK ${MACOS_SDK_VERSION} already installed."
    else
        local tmpdir
        tmpdir="$(mktemp -d)"
        local tarball="${tmpdir}/MacOSX${MACOS_SDK_VERSION}.sdk.tar.xz"
        local url="https://github.com/phracker/MacOSX-SDKs/releases/download/${MACOS_SDK_VERSION}/MacOSX${MACOS_SDK_VERSION}.sdk.tar.xz"

        info "Downloading ${url}"
        download_with_sha256 "$url" "$tarball" "$MACOS_SDK_SHA256"
        mkdir -p "$MACOS_SDK_ROOT"
        tar -xf "$tarball" -C "$MACOS_SDK_ROOT"
        rm -rf "$tmpdir"
    fi

    ln -sfn "$MACOS_SDK_DIR" "$MACOS_SDK_COMPAT_DIR"
    [[ -d "${MACOS_SDK_COMPAT_DIR}/System/Library/Frameworks/CoreServices.framework" ]] ||
        err "macOS SDK compatibility symlink is missing CoreServices."
    ok "macOS SDK available at ${MACOS_SDK_COMPAT_DIR}."
}

install_rust_targets() {
    info "Checking Rust cross-compilation targets."
    local targets=(
        aarch64-unknown-linux-gnu
        x86_64-apple-darwin
        aarch64-apple-darwin
        x86_64-pc-windows-gnu
    )

    local installed
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for target in "${targets[@]}"; do
        if grep -q "^${target}$" <<<"$installed"; then
            ok "Rust target ${target} already installed."
        else
            rustup target add "$target"
        fi
    done
}

install_cargo_tools() {
    for tool in cargo-zigbuild release-plz; do
        if command_exists "$tool"; then
            ok "${tool} already installed."
        elif [[ "$tool" == "release-plz" ]]; then
            cargo install release-plz --locked
        else
            cargo install "$tool"
        fi
    done
    print_version "cargo-zigbuild" cargo-zigbuild --version
    print_version "release-plz" release-plz --version
}

install_node() {
    info "Checking Node.js ${NODE_MAJOR}.x."
    if command_exists node && node --version 2>/dev/null | grep -q "^v${NODE_MAJOR}\\."; then
        ok "Node.js ${NODE_MAJOR}.x already installed."
    elif command_exists apt-get; then
        curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -
        apt-get install -y -qq nodejs
    else
        warn "Node.js ${NODE_MAJOR}.x is not installed and this host is not apt-based. Install Node ${NODE_MAJOR} manually."
    fi

    if command_exists npm; then
        npm list -g @vscode/vsce >/dev/null 2>&1 || npm install -g @vscode/vsce
        print_version "Node.js" node --version
        print_version "npm" npm --version
        local npm_prefix
        npm_prefix="$(npm prefix -g 2>/dev/null || npm config get prefix 2>/dev/null || true)"
        local npm_global_bin=""
        if [[ -n "$npm_prefix" && "$npm_prefix" != "undefined" ]]; then
            npm_global_bin="${npm_prefix%/}/bin"
        fi
        if [[ -n "$npm_global_bin" ]]; then
            export PATH="${npm_global_bin}:${PATH}"
        fi
        if ! command_exists vsce; then
            err "vsce executable not found after installing @vscode/vsce"
        fi
        print_version "@vscode/vsce" vsce --version
    else
        warn "npm is unavailable; VS Code extension publishing/building may fail."
    fi
}

write_file() {
    local path="$1"
    install -m 0755 /dev/stdin "$path"
}

install_mingw_wrappers() {
    [[ -d "$RELEASE_TOOLS_ROOT" ]] || return 0

    ln -sf "${RELEASE_TOOLS_ROOT}/usr/bin/qemu-aarch64" /usr/local/bin/qemu-aarch64-static

    if [[ -x "${RELEASE_TOOLS_ROOT}/usr/bin/wine" ]]; then
        write_file /usr/local/bin/wine64 <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export LD_LIBRARY_PATH="/opt/sqry-release-tools/usr/lib64:${LD_LIBRARY_PATH:-}"
export WINEDLLPATH="/opt/sqry-release-tools/usr/lib64/wine/x86_64-unix:/opt/sqry-release-tools/usr/lib64/wine/x86_64-windows:${WINEDLLPATH:-}"
exec /opt/sqry-release-tools/usr/bin/wine "$@"
EOF
    fi

    write_file /usr/local/bin/x86_64-w64-mingw32-dlltool <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export LD_LIBRARY_PATH="/opt/sqry-release-tools/usr/lib64:${LD_LIBRARY_PATH:-}"
exec /opt/sqry-release-tools/usr/bin/x86_64-w64-mingw32-dlltool "$@"
EOF

    write_file /usr/local/bin/x86_64-w64-mingw32-gcc <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
P=/opt/sqry-release-tools
case "${1:-}" in
  -print-prog-name=objdump) echo "/usr/bin/objdump"; exit 0 ;;
  -print-prog-name=dlltool) echo "/usr/local/bin/x86_64-w64-mingw32-dlltool"; exit 0 ;;
  -print-file-name=*) exec "$P/usr/bin/x86_64-w64-mingw32-gcc" --sysroot="$P/usr/x86_64-w64-mingw32/sys-root" -B"$P/usr/lib64/gcc/x86_64-w64-mingw32/13.2.0" -B"$P/usr/x86_64-w64-mingw32/sys-root/mingw/bin" "$@" ;;
esac
has_compile_arg=false
filtered=()
skip_next_target=false
for arg in "$@"; do
  if [[ "$skip_next_target" == true ]]; then skip_next_target=false; continue; fi
  case "$arg" in
    -E|-c) has_compile_arg=true; filtered+=("$arg") ;;
    --target=x86_64-pc-windows-gnu|--target=x86_64-w64-mingw32) ;;
    --target) skip_next_target=true ;;
    *) filtered+=("$arg") ;;
  esac
done
if [[ "$has_compile_arg" == true ]]; then
  exec /opt/zig/current/zig cc -target x86_64-windows-gnu "${filtered[@]}"
fi
exec "$P/usr/bin/x86_64-w64-mingw32-gcc" --sysroot="$P/usr/x86_64-w64-mingw32/sys-root" -B"$P/usr/lib64/gcc/x86_64-w64-mingw32/13.2.0" -B"$P/usr/x86_64-w64-mingw32/sys-root/mingw/bin" "$@"
EOF

    write_file /usr/local/bin/x86_64-w64-mingw32-g++ <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
P=/opt/sqry-release-tools
case "${1:-}" in
  -print-prog-name=objdump) echo "/usr/bin/objdump"; exit 0 ;;
  -print-prog-name=dlltool) echo "/usr/local/bin/x86_64-w64-mingw32-dlltool"; exit 0 ;;
  -print-file-name=*) exec "$P/usr/bin/x86_64-w64-mingw32-g++" --sysroot="$P/usr/x86_64-w64-mingw32/sys-root" -B"$P/usr/lib64/gcc/x86_64-w64-mingw32/13.2.0" -B"$P/usr/x86_64-w64-mingw32/sys-root/mingw/bin" "$@" ;;
esac
filtered=()
skip_next_target=false
for arg in "$@"; do
  if [[ "$skip_next_target" == true ]]; then skip_next_target=false; continue; fi
  case "$arg" in
    --target=x86_64-pc-windows-gnu|--target=x86_64-w64-mingw32) ;;
    --target) skip_next_target=true ;;
    *) filtered+=("$arg") ;;
  esac
done
exec "$P/usr/bin/x86_64-w64-mingw32-g++" --sysroot="$P/usr/x86_64-w64-mingw32/sys-root" -B"$P/usr/lib64/gcc/x86_64-w64-mingw32/13.2.0" -B"$P/usr/x86_64-w64-mingw32/sys-root/mingw/bin" "${filtered[@]}"
EOF

    ok "MinGW wrappers installed."
}

validate_release_tools() {
    zig version | grep -q "^${ZIG_VERSION}$" || err "Unexpected Zig version."
    [[ -d "${MACOS_SDK_COMPAT_DIR}/System/Library/Frameworks/CoreFoundation.framework" ]] ||
        err "macOS SDK compatibility path is incomplete."

    if command_exists x86_64-w64-mingw32-gcc; then
        local tmpdir
        tmpdir="$(mktemp -d)"
        printf 'int main(void){return 0;}\n' > "${tmpdir}/t.c"
        x86_64-w64-mingw32-gcc -c "${tmpdir}/t.c" -o "${tmpdir}/t.o"
        x86_64-w64-mingw32-gcc "${tmpdir}/t.o" -o "${tmpdir}/t.exe"
        "$(x86_64-w64-mingw32-gcc -print-prog-name=objdump)" -p "${tmpdir}/t.exe" >/dev/null
        rm -rf "$tmpdir"
        ok "Windows C compile/link and objdump validated."
    fi

    if command_exists x86_64-w64-mingw32-g++; then
        local tmpdir
        tmpdir="$(mktemp -d)"
        printf '#include <string>\nint main(){std::string s; s.push_back(65); return (int)s.size();}\n' > "${tmpdir}/t.cc"
        x86_64-w64-mingw32-g++ --target=x86_64-pc-windows-gnu -c "${tmpdir}/t.cc" -o "${tmpdir}/t.o"
        x86_64-w64-mingw32-g++ "${tmpdir}/t.o" -o "${tmpdir}/t.exe"
        "$(x86_64-w64-mingw32-g++ -print-prog-name=objdump)" -p "${tmpdir}/t.exe" >/dev/null
        rm -rf "$tmpdir"
        ok "Windows C++ compile/link and objdump validated."
    fi

    local aarch64_loader=""
    aarch64_loader="$(find "$RELEASE_TOOLS_ROOT" /usr -path '*aarch64*' -name ld-linux-aarch64.so.1 -print -quit 2>/dev/null || true)"
    [[ -n "$aarch64_loader" ]] || err "aarch64 glibc loader not found; qemu smoke tests cannot run."
    ok "aarch64 qemu loader available: ${aarch64_loader}"

    wine64 --version >/dev/null
    ok "Wine runtime validated."
}

main() {
    info "=== sqry self-hosted release runner setup ==="
    check_root
    install_system_packages
    install_zig
    install_macos_sdk
    install_rust_targets
    install_cargo_tools
    install_node
    validate_release_tools
    ok "All release runner dependencies are installed."
}

main "$@"

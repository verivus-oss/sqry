# Install the sqry Binary

sqry requires its CLI binary to be available on your system. There are three ways to get it:

## Option 1: Auto-Download (Default)

When sqry is not found on your PATH, the extension offers to download it automatically from
GitHub Releases. This is enabled by the `sqry.autoDownload` setting (on by default).

The binary is downloaded for your platform (Linux, macOS, Windows) and stored locally.
No manual steps required.

## Option 2: Manual Download

Download a pre-built binary directly from GitHub Releases:

1. Visit [github.com/verivus-oss/sqry/releases](https://github.com/verivus-oss/sqry/releases)
2. Download the binary for your platform
3. Place it somewhere on your PATH (e.g., `/usr/local/bin/sqry` on Linux/macOS)

## Option 3: Build from Source

If you have Rust installed (1.94+):

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo build --release -p sqry-cli
# Binary is at: target/release/sqry
```

## Custom Binary Location

If your binary is not on PATH, set the full path in settings:

- **Setting**: `sqry.path`
- **Example**: `/home/user/tools/sqry` or `C:\tools\sqry.exe`

Open VS Code settings (`Ctrl+,`) and search for `sqry.path` to configure it.

## Verify Installation

Run `sqry --version` in your terminal to confirm the binary is working.

class Sqry < Formula
  desc "Semantic code search tool"
  homepage "https://sqry.dev"
  version "@VERSION@"
  license "MIT"

  head "https://github.com/@REPO@.git", branch: "master"

  on_macos do
    on_arm do
      resource "sqry" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-macos-arm64"
        sha256 "@SHA_SQRY_MACOS_ARM64@"
      end
      resource "sqry-mcp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-mcp-macos-arm64"
        sha256 "@SHA_SQRY_MCP_MACOS_ARM64@"
      end
      resource "sqry-lsp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-lsp-macos-arm64"
        sha256 "@SHA_SQRY_LSP_MACOS_ARM64@"
      end
      resource "sqryd" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqryd-macos-arm64"
        sha256 "@SHA_SQRYD_MACOS_ARM64@"
      end
    end

    on_intel do
      resource "sqry" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-macos-x86_64"
        sha256 "@SHA_SQRY_MACOS_X86_64@"
      end
      resource "sqry-mcp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-mcp-macos-x86_64"
        sha256 "@SHA_SQRY_MCP_MACOS_X86_64@"
      end
      resource "sqry-lsp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-lsp-macos-x86_64"
        sha256 "@SHA_SQRY_LSP_MACOS_X86_64@"
      end
      resource "sqryd" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqryd-macos-x86_64"
        sha256 "@SHA_SQRYD_MACOS_X86_64@"
      end
    end
  end

  on_linux do
    on_intel do
      resource "sqry" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-linux-x86_64"
        sha256 "@SHA_SQRY_LINUX_X86_64@"
      end
      resource "sqry-mcp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-mcp-linux-x86_64"
        sha256 "@SHA_SQRY_MCP_LINUX_X86_64@"
      end
      resource "sqry-lsp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-lsp-linux-x86_64"
        sha256 "@SHA_SQRY_LSP_LINUX_X86_64@"
      end
      resource "sqryd" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqryd-linux-x86_64"
        sha256 "@SHA_SQRYD_LINUX_X86_64@"
      end
    end

    on_arm do
      resource "sqry" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-linux-arm64"
        sha256 "@SHA_SQRY_LINUX_ARM64@"
      end
      resource "sqry-mcp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-mcp-linux-arm64"
        sha256 "@SHA_SQRY_MCP_LINUX_ARM64@"
      end
      resource "sqry-lsp" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-lsp-linux-arm64"
        sha256 "@SHA_SQRY_LSP_LINUX_ARM64@"
      end
      resource "sqryd" do
        url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqryd-linux-arm64"
        sha256 "@SHA_SQRYD_LINUX_ARM64@"
      end
    end
  end

  def install
    if build.head?
      # HEAD build: compile from source via cargo workspace.
      system "cargo", "install", "--locked", "--path", "sqry-cli", "--root", prefix
      system "cargo", "install", "--locked", "--path", "sqry-mcp", "--root", prefix
      system "cargo", "install", "--locked", "--path", "sqry-lsp", "--root", prefix
      system "cargo", "install", "--locked", "--path", "sqry-daemon", "--root", prefix
    else
      ["sqry", "sqry-mcp", "sqry-lsp", "sqryd"].each do |name|
        resource(name).stage do
          bin_file = Dir["*"].first
          chmod 0o755, bin_file
          bin.install bin_file => name
        end
      end
    end
  end

  def caveats
    <<~EOS
      Installed binaries: sqry, sqry-mcp, sqry-lsp, sqryd.

      Quick start:
        sqry index .            # index the current workspace
        sqry search "query"     # semantic search
        sqryd start             # start the workspace-aware daemon

      Documentation: https://sqry.dev
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/sqry --version")
  end
end

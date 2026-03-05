class Sqry < Formula
  desc "Semantic code search tool"
  homepage "https://sqry.dev"
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-macos-arm64"
      sha256 "@SHA_MACOS_ARM64@"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-linux-x86_64"
      sha256 "@SHA_LINUX_X86_64@"
    end

    on_arm do
      url "https://github.com/@REPO@/releases/download/@VERSION_TAG@/sqry-linux-arm64"
      sha256 "@SHA_LINUX_ARM64@"
    end
  end

  def install
    binary_name = if OS.mac?
      "sqry-macos-arm64"
    elsif Hardware::CPU.arm?
      "sqry-linux-arm64"
    else
      "sqry-linux-x86_64"
    end

    chmod 0o755, binary_name
    bin.install binary_name => "sqry"
  end

  test do
    output = shell_output("#{bin}/sqry --version")
    assert_match version.to_s, output
  end
end

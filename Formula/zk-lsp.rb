class ZkLsp < Formula
  desc "Zettelkasten LSP server and CLI tools for Typst-based wikis"
  homepage "https://github.com/pxwg/zk-lsp.typst"
  license "AGPL-3.0"
  version "0.3.0"

  on_macos do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_ARM_DARWIN"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_X86_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_ARM_LINUX"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_X86_LINUX"
    end
  end

  def install
    bin.install "zk-lsp"
  end

  test do
    assert_match "zk-lsp", shell_output("#{bin}/zk-lsp --help")
  end
end

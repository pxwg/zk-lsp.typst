class ZkLsp < Formula
  desc "Zettelkasten LSP server and CLI tools for Typst-based wikis"
  homepage "https://github.com/pxwg/zk-lsp.typst"
  license "AGPL-3.0"
  version "0.3.1"

  on_macos do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.1/zk-lsp-aarch64-apple-darwin.tar.gz"
      sha256 "4fc39ee2a947badd22c88419f2eb4143093798e18390c505734bfe7eea0f6bc5"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.1/zk-lsp-x86_64-apple-darwin.tar.gz"
      sha256 "4819370d4e200acf5cedc68af0f2d3484e956953a444629487a22b8dbf9f1d97"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.1/zk-lsp-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d7a4fc3eb6bb5ef6075702428cd88e5e8e6ecd33708fb9eba5886c495b883dd2"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.1/zk-lsp-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1783633d31a29a014c4f77d175fff7bf63555fb2a8f3ae192d7f8468ae2f0cfb"
    end
  end

  def install
    bin.install "zk-lsp"
  end

  test do
    assert_match "zk-lsp", shell_output("\#<built-in function bin>/zk-lsp --help")
  end
end

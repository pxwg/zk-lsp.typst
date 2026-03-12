class ZkLsp < Formula
  desc "Zettelkasten LSP server and CLI tools for Typst-based wikis"
  homepage "https://github.com/pxwg/zk-lsp.typst"
  license "AGPL-3.0"
  version "0.3.0"

  on_macos do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-aarch64-apple-darwin.tar.gz"
      sha256 "40c494cc235edcb3d46415445ec2f66e007f6523207386301e2377b530429438"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-x86_64-apple-darwin.tar.gz"
      sha256 "9375f942c13e6a59eeff9ba1c5a24bccc6dd37f9a7ee0014a35c53466779d75a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "65a59b764856d8e811438d3b1f520ebdef58b948445d4688f0f36ed82b5bb839"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typst/releases/download/v0.3.0/zk-lsp-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "44df7ff0fd5bd53b4891def9a413a2d6bd6ca622b00c325d4942f61fe2830871"
    end
  end

  def install
    bin.install "zk-lsp"
  end

  test do
    assert_match "zk-lsp", shell_output("\#<built-in function bin>/zk-lsp --help")
  end
end

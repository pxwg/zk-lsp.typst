class ZkLsp < Formula
  desc "Zettelkasten LSP server and CLI tools for Typst-based wikis"
  homepage "https://github.com/pxwg/zk-lsp.typ"
  license "AGPL-3.0"
  version "0.5.5"

  on_macos do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typ/releases/download/v0.5.5/zk-lsp-aarch64-apple-darwin.tar.gz"
      sha256 "158d8ce18deecdf8ff71afb0ffb7b9316605abf32b061fb29b9803ba6450bdbc"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typ/releases/download/v0.5.5/zk-lsp-x86_64-apple-darwin.tar.gz"
      sha256 "9d4be34835094d4ffd2d766ad0390c167567a9c0f5a8a5dd0d3497407cd946d8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/pxwg/zk-lsp.typ/releases/download/v0.5.5/zk-lsp-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "197365e846bfb95014a66037edf4758b4ad133dc0b4233ac2187b5ea24367446"
    end
    on_intel do
      url "https://github.com/pxwg/zk-lsp.typ/releases/download/v0.5.5/zk-lsp-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5b55a2ddb2246b8a1abe4097c564ce0bfa45f75423d219e46cc0bac9ee3dbd96"
    end
  end

  def install
    bin.install "zk-lsp"
  end

  test do
    assert_match "zk-lsp", shell_output("#{bin}/zk-lsp --help")
  end
end

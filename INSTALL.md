# Installing zk-lsp

## macOS — Homebrew (pre-built binary, recommended)

This repository doubles as a Homebrew tap. The formula installs a pre-built
binary — no Rust toolchain required.

```bash
brew tap pxwg/zk-lsp https://github.com/pxwg/zk-lsp.typst
brew install zk-lsp
```

Supported: macOS arm64 (Apple Silicon) and x86_64 (Intel).

To upgrade after a new release:

```bash
brew upgrade zk-lsp
```

---

## cargo-binstall (pre-built binary)

[`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall) downloads the
pre-built binary from GitHub Releases — no compilation needed.

```bash
cargo binstall zk-lsp --git https://github.com/pxwg/zk-lsp.typst
```

Supported targets: `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

---

## Download binary manually

Pre-built tarballs are attached to every [GitHub Release](https://github.com/pxwg/zk-lsp.typst/releases):

| Target | File |
|--------|------|
| macOS Apple Silicon | `zk-lsp-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `zk-lsp-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `zk-lsp-x86_64-unknown-linux-gnu.tar.gz` |
| Linux arm64 | `zk-lsp-aarch64-unknown-linux-gnu.tar.gz` |

```bash
# Example: macOS Apple Silicon
curl -L https://github.com/pxwg/zk-lsp.typst/releases/latest/download/zk-lsp-aarch64-apple-darwin.tar.gz \
  | tar xz -C ~/.local/bin
```

---

## Build from source

Requires **Rust 1.75+** (`rustup` recommended).

```bash
git clone https://github.com/pxwg/zk-lsp.typst
cd zk-lsp.typst
cargo build --release
ln -sf "$(pwd)/target/release/zk-lsp" ~/.local/bin/zk-lsp
```

Or install directly with Cargo (no clone needed):

```bash
cargo install --git https://github.com/pxwg/zk-lsp.typst
```

---

## Verifying the install

```bash
zk-lsp --help
```

---

## Maintainer: releasing a new version

1. Bump `version` in `Cargo.toml` and commit to `main`.

2. Push a matching tag:

   ```bash
   git tag v0.x.0
   git push origin v0.x.0
   ```

CI will then:
- Build pre-compiled binaries for all four targets (macOS arm64/x86_64,
  Linux arm64/x86_64).
- Run the test suite on native targets (macOS and Linux x86_64).
- Create a GitHub release and attach all four binary tarballs.
- Compute sha256 checksums and commit an updated `Formula/zk-lsp.rb` to
  `main` automatically.

Homebrew users and `cargo-binstall` users pick up the update on the next
`brew upgrade` / `cargo binstall`.

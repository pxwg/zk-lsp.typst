# CLAUDE.md — zk-lsp

Rust LSP binary for the `~/wiki` Typst-based Zettelkasten.

## Build & Test

```bash
cargo build          # dev build
cargo build --release
cargo test           # 5 unit tests in src/parser.rs
```

Zero warnings are expected. Fix all warnings before committing.

## CLI Commands

```bash
zk-lsp [lsp]                        # start LSP on stdin/stdout (default)
zk-lsp generate [--wiki-root PATH]  # regenerate ~/wiki/link.typ
zk-lsp new [--metadata] [--wiki-root PATH]   # create note, print path
zk-lsp remove <ID> [--wiki-root PATH]        # delete note + remove from link.typ
```

`WIKI_ROOT` env overrides the `~/wiki` default. `--wiki-root` overrides `WIKI_ROOT`.

## Wiki Note Structure

```
/* Metadata:           <- optional 6-line block (notes created with --metadata)
Aliases: ...
Abstract: ...
Keyword: ...
Generated: true
*/
#import "../include.typ": *    <- import_idx  (0-based)
#show: zettel
                               <- import_idx + 2  (blank)
= Title <YYMMDDHHMM>           <- title_line_idx = import_idx + 3
#tag.xxx                       <- tag_line_idx   = import_idx + 4
#evolution_link(<ID>)          <- import_idx + 5  (optional)
```

Notes without a metadata block start directly with the `#import` line.

## Key Design Rules

- **Parser is stateless** — `src/parser.rs` takes `&str`, returns owned structs. No I/O.
- **Index is async** — `NoteIndex` uses `DashMap`; all file I/O via `tokio::fs`.
- **Atomic writes** — `link.typ` is always written via `tmp → rename`.
- **Tracing to stderr** — stdout is reserved for JSON-RPC. Use `tracing::{info, error, …}`.
- **ID format** — exactly 10 ASCII digits (`YYMMDDHHMM`). Regex: `@(\d{10})`.

## Reference Lua Sources

These files are authoritative for behaviour parity:

| File | Defines |
|------|---------|
| `~/.config/nvim/lua/zk_lsp.lua` | Diagnostics, code actions, references, LSP capabilities |
| `~/.config/nvim/lua/zk_scripts.lua` | Note CRUD, tag formatter, cross-file todo propagation |
| `~/.config/nvim/lua/zk_extmark.lua` | Inlay hint regex and title extraction logic |

## Source Map

```
src/
├── main.rs          CLI dispatch + LSP server startup
├── cli.rs           clap CLI definitions
├── config.rs        WikiConfig resolution
├── parser.rs        Stateless note parsing (unit-tested)
├── index.rs         NoteIndex (DashMap notes + backlinks)
├── link_gen.rs      link.typ generation and entry management
├── note_ops.rs      create_note / delete_note
├── server.rs        tower-lsp LanguageServer impl
├── watcher.rs       notify-debouncer-mini (300 ms) on note_dir
└── handlers/
    ├── references.rs    find_references (uses backlink index)
    ├── diagnostics.rs   archived → Warning, legacy → Info (with suppression)
    ├── code_actions.rs  replace + append quick-fixes
    ├── inlay_hints.rs   @ID → title after cursor
    └── formatting.rs    willSaveWaitUntil tag edit + cross-file propagation
```

## Neovim Integration

```lua
vim.lsp.config("zk-lsp", {
  cmd = { "zk-lsp", "lsp" },
  filetypes = { "typst" },
  root_dir = vim.fn.expand("~/wiki"),
})
```

## Install

```bash
cargo build --release
cp target/release/zk-lsp ~/.local/bin/zk-lsp
```

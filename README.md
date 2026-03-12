# zk-lsp

A standalone Rust LSP server for a Typst-based Zettelkasten wiki.

Replaces the Lua/Python automation in `~/wiki` with a single compiled binary that provides:

- **Inlay hints** — `@2602082037` displays the note title inline
- **Diagnostics** — warnings for archived references, info for legacy references, errors for cyclic dependencies
- **Code actions** — quick-fix to replace or append the successor note ID
- **References** — find every file that links to the note under the cursor
- **Tag formatter** — `zk-lsp format` normalizes checkbox states and `#tag.todo/wip/done` for a single note
- **Reconcile** — `zk-lsp reconcile` propagates done-states across the whole wiki in a single topological pass
- **Cycle detection** — cyclic `@ID` task dependencies are a hard error (CLI + LSP diagnostics)
- **Migration** — `zk-lsp migrate` converts legacy comment-format notes to TOML schema v1
- **CLI tools** — `generate`, `new`, `remove` for note management without opening Neovim
- **File watcher** — `link.typ` stays in sync when notes are created or deleted from the terminal

## Requirements

- Rust 1.75+
- A wiki directory following the structure below (default: `~/wiki`)

## Install

```bash
git clone https://github.com/you/zk-lsp
cd zk-lsp
cargo build --release
ln -sf $(pwd)/target/release/zk-lsp ~/.local/bin/zk-lsp
```

## Wiki Structure

```
~/wiki/
├── include.typ          # template definitions
├── link.typ             # auto-generated index (do not edit manually)
└── note/
    ├── 2602072319.typ
    ├── 2602082037.typ
    └── ...
```

Each note filename is a 10-digit timestamp (`YYMMDDHHMM`). Notes use the TOML format (created by `zk-lsp new`) to store metadata:

`````typst
#import "../include.typ": *
#let zk-metadata = toml(bytes(```
schema-version = 1
title = "Note Title"
tags = []
checklist-status = "none"   # or "active", "done", "archived"
generated = false
```))
#show: zettel

= Note Title <2602082037>
// Content goes here
`````

~Legacy comment-format notes are read-only. Run `zk-lsp migrate` to convert them to TOML schema v1.~

## CLI

```
zk-lsp [OPTIONS] [COMMAND]

Commands:
  lsp        Start the LSP server on stdin/stdout [default]
  generate   Regenerate link.typ from the note directory
  new        Create a new note and print its path to stdout
  remove     Delete a note and remove it from link.typ
  format     Read a note from stdin, write formatted content to stdout
  migrate    Migrate legacy comment-format notes to TOML schema v1
  reconcile  Reconcile cross-file checkbox states across the whole wiki
  export     Export a BFS context document for AI consumption
  check      Check graph integrity: dead links and orphan notes

Options:
  --wiki-root <PATH>   Override the wiki root directory
```

The wiki root is resolved in this order:
1. `--wiki-root` CLI flag
2. `WIKI_ROOT` environment variable
3. `~/wiki` (fallback)

### Examples

```bash
# Regenerate link.typ after bulk changes
zk-lsp generate

# Create a new note and open it in Neovim
nvim $(zk-lsp new)

# Delete a note (removes file + link.typ entry)
zk-lsp remove 2602082037

# Format a note in-place
zk-lsp format < note/2602082037.typ > /tmp/out.typ

# Migrate all legacy notes to TOML
zk-lsp migrate

# Propagate done-states across the wiki (dry run first)
zk-lsp reconcile --dry-run
zk-lsp reconcile

# Export context for AI: forward BFS (entry note first, deps follow)
zk-lsp export 2602082037 --depth 3

# Export context for AI: inverse BFS (ancestors first, entry note last)
zk-lsp export 2602082037 --depth 3 --inverse

# Check graph integrity (exits 1 on dead links)
zk-lsp check
zk-lsp check --no-orphans

# Use a non-default wiki directory
zk-lsp --wiki-root ~/notes generate
```

## Configuration

`zk-lsp` loads configuration from two locations in this order (project overrides user):

| Path | Scope |
|---|---|
| `$XDG_CONFIG_HOME/zk-lsp/config.toml` (default: `~/.config/zk-lsp/config.toml`) | User-level |
| `<wiki-root>/zk-lsp.toml` | Project-level |

### Note template (`zk-lsp new`)

Customize the file created by `zk-lsp new` with a `[new_note]` section. Two placeholders are available:

| Placeholder | Expands to |
|---|---|
| `{{id}}` | The 10-digit timestamp ID (`YYMMDDHHMM`) |
| `{{metadata}}` | The standard TOML metadata block (`#let zk-metadata = toml(bytes(...))`) |

**`~/.config/zk-lsp/config.toml`** (user default):

```toml
[new_note]
template = """
#import "../include.typ": *
{{metadata}}
#show: zettel.with(metadata: zk-metadata)

= <{{id}}>
"""
```

**`<wiki-root>/zk-lsp.toml`** (project override — e.g. a research wiki with a richer skeleton):

```toml
[new_note]
template = """
#import "../include.typ": *
{{metadata}}
#show: zettel.with(metadata: zk-metadata)

= <{{id}}>

== References
"""
```

If neither file exists, `zk-lsp new` falls back to the built-in default template.

## Neovim Integration

Add to your Neovim config (requires Neovim 0.11+):

```lua
vim.lsp.config("zk-lsp", {
  cmd = { "zk-lsp", "lsp" },
  filetypes = { "typst" },
  root_dir = vim.fn.expand("~/wiki"),
})
vim.lsp.enable("zk-lsp")
```

The server advertises these capabilities:

| Capability | Trigger |
|---|---|
| Inlay hints | Automatically on every `@ID` reference |
| Diagnostics | `didOpen`, `didSave`, `didChangeWatchedFiles` |
| Code actions | On diagnostic ranges (archived / legacy) |
| References | `gr` / `textDocument/references` |
| Workspace symbols | `:lua vim.lsp.buf.workspace_symbol(query-string)` |

### Commands exposed via `executeCommand`

| Command | Arguments | Effect |
|---|---|---|
| `zk.newNote` | — | Create a note and notify with its URI |
| `zk.removeNote` | `id: string` | Delete a note |
| `zk.generateLinkTyp` | — | Regenerate `link.typ` |
| `zk.exportContext` | `id: string, depth?: number, inverse?: bool` | Return a Markdown context document; `inverse=true` lists ancestors first |

## Diagnostics

| Condition | Severity | Message |
|---|---|---|
| `@ID` references an archived note | Warning | `Note @ID is archived. New version: @ALT` |
| `@ID` references a legacy note | Info | `Note @ID is legacy. Newer insights: @EVO` |
| `@ID` participates in a cyclic dependency | Error | `cyclic task dependency detected` |

**Legacy suppression**: if a legacy reference is immediately followed by its evolution ID on the same line (`@old @new`), the diagnostic is suppressed.

## Tag Formatter

`zk-lsp format` reads a note from stdin and writes the normalized content to stdout. It:

1. Ticks or unticks `- [ ] @<id>` checkboxes based on whether all referenced notes are done (reads on-disk `checklist-status` set by `reconcile`)
2. Propagates parent checkbox state from children (nested lists)
3. Updates the `#tag.todo/wip/done` status line

| Checkbox state | Tag |
|---|---|
| All incomplete | `#tag.todo` |
| Mixed | `#tag.wip` |
| All complete | `#tag.done` |

## Reconcile

`zk-lsp reconcile` evaluates done-states for the entire wiki in dependency order and rewrites changed files:

1. Builds a dependency graph from all `- [ ] @<id>` checklist entries
2. Fails fast if any cyclic dependencies are detected (prints Typst-style errors with file locations)
3. Evaluates note done-states in a single Kahn topological pass
4. Writes back only changed notes (updates `checklist-status` in TOML metadata)

Use `--dry-run` to preview changes without writing files.

## Environment

| Variable | Purpose |
|---|---|
| `WIKI_ROOT` | Default wiki root path |
| `ZK_LSP_LOG` | Log level filter (`info`, `debug`, `trace`) |

## To-Do

- [ ] `zk-lsp init` command to scaffold a basic `include.typ` and `link.typ` if missing

## License

GNU Affero General Public License v3 (AGPL-3.0)

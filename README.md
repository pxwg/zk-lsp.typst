# zk-lsp

A standalone Rust LSP server for a Typst-based Zettelkasten wiki.

Replaces the Lua/Python automation in `~/wiki` with a single compiled binary that provides:

- **Inlay hints** — `@2602082037` displays the note title inline
- **Diagnostics** — warnings for archived references, info for legacy references
- **Code actions** — quick-fix to replace or append the successor note ID
- **References** — find every file that links to the note under the cursor
- **Tag formatter** — `willSaveWaitUntil` auto-updates `#tag.todo/wip/done` based on checkbox state
- **Cross-file propagation** — when a note becomes done, all `- [ ] @<id>` checkboxes in other notes are ticked
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
ln target/release/zk-lsp ~/.local/bin/
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

Each note filename is a 10-digit timestamp (`YYMMDDHHMM`). Notes have two layouts:

**Simple** (created with `zk-lsp new`):
```typst
#import "../include.typ": *
#show: zettel

= Note Title <2602082037>
#tag.todo
```

**With metadata** (created with `zk-lsp new --metadata`):
```typst
/* Metadata:
Aliases: Short Name, Another Name
Abstract: One-sentence summary of the note.
Keyword: topic, subtopic
Generated: true
*/
#import "../include.typ": *
#show: zettel

= Note Title <2602082037>
#tag.todo
```

## CLI

```
zk-lsp [OPTIONS] [COMMAND]

Commands:
  lsp       Start the LSP server on stdin/stdout [default]
  generate  Regenerate link.typ from the note directory
  new       Create a new note and print its path to stdout
  remove    Delete a note and remove it from link.typ

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

# Create a note with metadata scaffolding
nvim $(zk-lsp new --metadata)

# Delete a note (removes file + link.typ entry)
zk-lsp remove 2602082037

# Use a non-default wiki directory
zk-lsp --wiki-root ~/notes generate
```

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
| Tag formatter | `willSaveWaitUntil` for `*/note/*.typ` |
| Workspace symbols | `:lua vim.lsp.buf.workspace_symbol(query-string)` |

### Commands exposed via `executeCommand`

| Command | Effect |
|---|---|
| `zk.newNote` | Create a note (arg: `true` for metadata template) |
| `zk.removeNote` | Delete a note (arg: note ID string) |
| `zk.generateLinkTyp` | Regenerate `link.typ` |
| `zk.exportContext` | (reserved) |

## Diagnostics

| Condition | Severity | Message |
|---|---|---|
| `#tag.archived` on referenced note | Warning | `Note @ID is archived. New version: @ALT` |
| `#tag.legacy` on referenced note | Info | `Note @ID is legacy. Newer insights: @EVO` |

**Legacy suppression**: if a legacy reference is immediately followed by its evolution ID on the same line (`@old @new`), the diagnostic is suppressed.

## Tag Formatter

On every save of a `*/note/*.typ` file, `willSaveWaitUntil` inspects the todo checkboxes (skipping fenced code blocks) and returns a `TextEdit` if the status tag needs to change:

| Checkbox state | Tag |
|---|---|
| All incomplete | `#tag.todo` |
| Mixed | `#tag.wip` |
| All complete | `#tag.done` |
| Any state + `#tag.archived` | `#tag.done` |

When the tag becomes `done` or `wip`, the server applies a `WorkspaceEdit` to tick (or untick) every `- [ ] @<id>` checkbox in other notes that reference this note.

## Environment

| Variable | Purpose |
|---|---|
| `WIKI_ROOT` | Default wiki root path |
| `ZK_LSP_LOG` | Log level filter (`info`, `debug`, `trace`) |

## To-Do

- [ ] Workspace symbols for note titles and aliases
- [ ] `zk.exportContext` command to export a note and its references as Markdown
- [ ] `zk-lsp init` command to scaffold a basic `include.typ` and `link.typ` if missing

## License

MIT

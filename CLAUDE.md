# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# CLAUDE.md — zk-lsp

Rust LSP binary for the `~/wiki` Typst-based Zettelkasten.

## Build & Test

```bash
cargo build          # dev build
cargo build --release
cargo test           # 51 tests across parser, formatting, migrate, reconcile, cycle, diagnostics, code_actions, completion
cargo test <name>    # run a single test by name (substring match)
```

Zero warnings are expected. Fix all warnings before committing.

## CLI Commands

```bash
zk-lsp [lsp]                        # start LSP on stdin/stdout (default)
zk-lsp generate [--wiki-root PATH]  # regenerate ~/wiki/link.typ
zk-lsp new [--wiki-root PATH]       # create note, print path
zk-lsp remove <ID> [--wiki-root PATH]  # delete note + remove from link.typ
zk-lsp format                       # read note from stdin, write formatted to stdout
zk-lsp migrate [--wiki-root PATH]   # migrate legacy comment-format notes to TOML schema v1
zk-lsp reconcile [--wiki-root PATH] [--dry-run]  # reconcile cross-file checkbox states
```

`WIKI_ROOT` env overrides the `~/wiki` default. `--wiki-root` overrides `WIKI_ROOT`.

## Wiki Note Structure

Notes use the TOML format (primary, created by `zk-lsp new`):

```
#import "../include.typ": *
#let zk-metadata = toml(bytes("""
schema-version = 1
title = "..."
tags = [...]
checklist-status = "none"   # or "todo", "wip", "done"
relation = "active"         # or "archived", "legacy"
relation-target = []        # required when relation != "active"
generated = false
"""))
#show: zettel

= Title <YYMMDDHHMM>
```

Legacy comment format (read-only; run `zk-lsp migrate` to convert):

```
/* Metadata:
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

Parser tries TOML path first; falls back to legacy. `parse_header()` no longer creates legacy-format notes.

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
├── main.rs               CLI dispatch + LSP server startup
├── cli.rs                clap CLI definitions
├── config.rs             WikiConfig resolution
├── parser.rs             Stateless note parsing (unit-tested)
├── dependency_graph.rs   build_dependency_graph: RefItem → positioned edge list
├── cycle.rs              detect_cycles (Tarjan SCC) + render_cycle_errors (CLI)
├── reconcile.rs          single-pass DAG eval + batch write-back; fails on cycles
├── index.rs              NoteIndex (DashMap notes + backlinks)
├── link_gen.rs           link.typ generation and entry management
├── migrate.rs            migrate_wiki / migrate_note (legacy → TOML v1)
├── note_ops.rs           create_note / delete_note
├── server.rs             tower-lsp LanguageServer impl
├── watcher.rs            notify-debouncer-mini (300 ms) on note_dir
└── handlers/
    ├── references.rs    find_references (uses backlink index)
    ├── diagnostics.rs   archived/legacy warnings; cycle, schema diagnostics
    ├── code_actions.rs  quick-fixes + metadata toggle actions (checklist-status, relation)
    ├── completion.rs    TOML metadata completions (enum values, note IDs, field names)
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

## Checklist Semantics

Two item types exist in checklists:

1. **`LocalItem`** — truth = `item.checked` (source fact; user-authored)
2. **`RefItem`** (`- [ ] @A @B …`) — truth = `∀ t ∈ targets: done_lookup(t.target_id)` (all referenced notes must be done; **never** the rendered checkbox)

**`RefTarget`** carries `target_id`, `byte_start`, `byte_end` (byte offsets of `@ID` within the full line). Used by `dependency_graph` for positioned error reporting and by LSP diagnostics via `byte_to_utf16`.

**Leaf items rule:** Only leaf items participate in note status aggregation. A leaf has no subsequent item with strictly greater indent. Non-leaf LocalItems are derived display views of their children and must not be used as source facts.

**Rendered `[x]` on a RefItem is NEVER a source of truth in the solver** — `dep_states["B"]` is authoritative.

**Note done formula:**
```
note.done = ∀ leaf_item ∈ items: eval_item_truth(leaf_item) == true
          (falls back to metadata.checklist_status when items list is empty)
```

**Responsibility split:**
- **Formatter** (`formatting.rs`): current-file normalization + read-only dep_states lookup (trusts reconciled metadata via `is_note_done`); no graph solving. `is_note_done_with_deps` is the canonical semantic evaluator.
- **Reconcile** (`reconcile.rs`): build dependency graph → detect cycles (fail fast) → Kahn topo-sort → single-pass DAG evaluation → batch write-back. No convergence loop.
- **Cycles** — hard error. `detect_cycles` (Tarjan SCC) returns `Vec<DependencyCycle>`; CLI renders Typst-style errors with ANSI colour and CJK-aware `^` alignment; LSP emits per-file `ERROR` diagnostics via `get_cycle_diagnostics`.

**Key functions:**
- `parser::parse_checklist_items(content)` → `Vec<ChecklistItem>` (skips fenced blocks)
- `parser::eval_item_truth(item, done_lookup)` → bool
- `parser::compute_note_done_from_items(items, done_lookup)` → bool (leaf-only)
- `parser::find_all_refs_filtered(content)` → `Vec<RefOccurrence>` (skips TOML block, `/* */` comments, fenced blocks)
- `dependency_graph::build_dependency_graph(notes)` → `DependencyGraph`
- `cycle::detect_cycles(graph)` → `Vec<DependencyCycle>`
- `cycle::render_cycle_errors(cycles)` → `String` (CLI; byte columns, ANSI colour, CJK width)
- `diagnostics::get_cycle_diagnostics(content, path, cycles)` → `Vec<Diagnostic>` (LSP; UTF-16)
- `diagnostics::get_schema_diagnostics(content, index)` → `Vec<Diagnostic>` (validates TOML metadata fields)
- `code_actions::get_metadata_actions(uri, content, range)` → `Vec<CodeActionOrCommand>` (checklist-status toggle, relation switch)
- `completion::get_completions(content, position, index)` → `Vec<CompletionItem>` (TOML enum values, note IDs, field names)

## Install

```bash
cargo build --release
ln -sf $(pwd)/target/release/zk-lsp ~/.local/bin/zk-lsp
```

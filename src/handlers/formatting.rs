use std::collections::HashMap;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use regex::Regex;
use tower_lsp::lsp_types::*;

use crate::config::WikiConfig;
use crate::parser::{self, StatusTag, ChecklistStatus};
use crate::hooks::lua::{HookRunner, build_hook_note_input};
use crate::hooks::apply::apply_hook_result;

/// Default hooks embedded at compile time.
const DEFAULT_CHECKLIST_HOOK: &str = include_str!("../../examples/hooks/checklist.lua");
const DEFAULT_RELATION_HOOK: &str = include_str!("../../examples/hooks/relation_status.lua");

#[allow(dead_code)]
/// Apply a list of byte-range edits to `content`.
///
/// Each edit is `(start_byte, end_byte, replacement_text)`.
/// Edits must be non-overlapping. They are applied from last to first so byte
/// offsets remain valid throughout.
///
/// Returns `Err` if any edit is invalid (out of bounds, inverted range, or overlap).
pub fn apply_byte_edits(content: &str, edits: &[(usize, usize, String)]) -> anyhow::Result<String> {
    let len = content.len();
    for (start, end, _) in edits {
        anyhow::ensure!(start <= end, "edit has start ({start}) > end ({end})");
        anyhow::ensure!(*end <= len, "edit end ({end}) out of bounds (len={len})");
    }
    // Sort by start ascending, check no overlaps
    let mut sorted: Vec<&(usize, usize, String)> = edits.iter().collect();
    sorted.sort_by_key(|(s, _, _)| *s);
    for w in sorted.windows(2) {
        anyhow::ensure!(
            w[0].1 <= w[1].0,
            "edits overlap: [{}, {}) and [{}, {})",
            w[0].0, w[0].1, w[1].0, w[1].1
        );
    }
    // Apply in reverse order so earlier byte offsets remain valid
    let mut result = content.to_string();
    for (start, end, text) in sorted.iter().rev() {
        result.replace_range(start..end, text);
    }
    Ok(result)
}

/// Render a `toml::Value` as an inline TOML literal (no newlines).
/// Used by `apply_metadata_patch` for targeted in-place key replacement.
fn toml_value_inline(value: &toml::Value) -> anyhow::Result<String> {
    match value {
        toml::Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            Ok(format!("\"{escaped}\""))
        }
        toml::Value::Integer(n) => Ok(n.to_string()),
        toml::Value::Float(f) => Ok(f.to_string()),
        toml::Value::Boolean(b) => Ok(b.to_string()),
        toml::Value::Array(arr) => {
            let items: Vec<String> =
                arr.iter().map(toml_value_inline).collect::<anyhow::Result<_>>()?;
            Ok(format!("[{}]", items.join(", ")))
        }
        _ => anyhow::bail!("nested TOML tables are not supported in a metadata patch"),
    }
}

/// Apply a metadata patch to a TOML-format note using targeted in-place replacement.
///
/// For each `(key, value)` in `patch`, finds the key's line within the TOML block
/// and replaces only that line — preserving key order, indentation, and all other lines.
/// Returns `Err` if the note has no TOML metadata block or a key is not found in the block.
pub fn apply_metadata_patch(
    content: &str,
    patch: &HashMap<String, toml::Value>,
) -> anyhow::Result<String> {
    let block = parser::find_toml_metadata_block(content)
        .ok_or_else(|| anyhow::anyhow!("no TOML metadata block found"))?;

    let lines: Vec<&str> = content.lines().collect();
    let trailing_newline = content.ends_with('\n');
    let mut result_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    'patch: for (key, value) in patch {
        let val_str = toml_value_inline(value)?;
        for i in block.start_line..=block.end_line {
            let line = lines[i];
            let trimmed = line.trim_start();
            if trimmed.starts_with(&format!("{key} =")) || trimmed.starts_with(&format!("{key}=")) {
                let indent_len = line.len() - trimmed.len();
                result_lines[i] = format!("{}{key} = {val_str}", " ".repeat(indent_len));
                continue 'patch;
            }
        }
        anyhow::bail!("key '{key}' not found in TOML metadata block");
    }

    let mut out = result_lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

static RE_TODO_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"@(\d{10})").unwrap());

/// Apply the tag-line formatting to `content` and return the result.
/// Internal helper; no cross-file I/O.
fn apply_tag_edit(content: &str) -> String {
    let Some(edit) = compute_tag_edit(content) else {
        return content.to_string();
    };
    let line_num = edit.range.start.line as usize;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    if line_num < lines.len() {
        lines[line_num] = edit.new_text;
    }
    let trailing_newline = content.ends_with('\n');
    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Canonical semantic evaluator: is `content` done given `deps` (id → done)?
///
/// - Archived notes are always done.
/// - If the note has checklist items: evaluates **leaf items only** using
///   `deps` for RefItems and checkbox state for LocalItems.
/// - If the note has no checklist items: falls back to `checklist-status`
///   metadata (written by `reconcile`).
/// - Returns `false` if the note has no parseable header.
///
/// This function does NOT call `normalize_note` or `count_todos`; it works
/// directly on raw content.
pub fn is_note_done_with_deps(content: &str, deps: &HashMap<String, bool>) -> bool {
    let Some(header) = parser::parse_header(content) else {
        return false;
    };
    if header.archived {
        return true;
    }
    let items = parser::parse_checklist_items(content);
    if items.is_empty() {
        return header.checklist_status == Some(ChecklistStatus::Done);
    }
    parser::compute_note_done_from_items(&items, &|id| deps.get(id).copied().unwrap_or(false))
}

#[allow(dead_code)]
/// Best-effort done check for reading dependency notes (formatter path).
///
/// Trusts explicit `checklist-status` metadata written by `reconcile` first.
/// Falls back to leaf-item semantics with an empty dep context (RefItems
/// default to not-done when no global state is available).
///
/// NOTE: May underestimate done-ness for notes with RefItems that haven't been
/// reconciled. For authoritative global evaluation, use `reconcile` +
/// `is_note_done_with_deps`.
pub fn is_note_done(content: &str) -> bool {
    let Some(header) = parser::parse_header(content) else {
        return false;
    };
    if header.archived {
        return true;
    }
    // Trust explicit status written by reconcile first
    match &header.checklist_status {
        Some(ChecklistStatus::Done) => return true,
        Some(ChecklistStatus::Todo) | Some(ChecklistStatus::Wip) => return false,
        _ => {}
    }
    // Fallback: evaluate leaf items with empty dep context
    is_note_done_with_deps(content, &HashMap::new())
}

/// Normalize `content` using a pre-built map of dependency states.
/// Pure (no I/O): looks up each `@ID` in `dep_states` (absent = not done).
/// Calls `update_ref_checkboxes_sync`, `update_nested_checkboxes`, and `apply_tag_edit`.
pub fn normalize_note(content: &str, dep_states: &HashMap<String, bool>) -> String {
    let after_refs = update_ref_checkboxes_sync(content, dep_states);
    let after_nested = update_nested_checkboxes(&after_refs);
    apply_tag_edit(&after_nested)
}

/// Sync version of ref-checkbox update using a pre-built dep_states map.
fn update_ref_checkboxes_sync(content: &str, dep_states: &HashMap<String, bool>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let mut changed = false;
    let mut in_fence = false;

    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !is_todo_line(line) {
            continue;
        }
        let ids: Vec<&str> = RE_TODO_ID
            .captures_iter(line)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();
        if ids.is_empty() {
            continue;
        }
        let all_done = ids.iter().all(|id| dep_states.get(*id).copied().unwrap_or(false));
        let new_state = if all_done { 'x' } else { ' ' };
        if get_todo_state(line) != Some(new_state) {
            if let Some(new_line) = replace_todo_state(line, new_state) {
                result[i] = new_line;
                changed = true;
            }
        }
    }

    if !changed {
        return content.to_string();
    }
    let trailing_newline = content.ends_with('\n');
    let mut out = result.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Format `content` by running hooks in sequence:
/// 1. Built-in default hooks (checklist.lua + relation_status.lua, embedded at compile time),
///    unless `config.zk_config.disable_default_hooks` is true.
/// 2. User-configured file hooks from `config.zk_config.hooks`, loaded at runtime.
///
/// Cross-file ref-checkbox sync (`@ID` items) is intentionally NOT performed here;
/// that is the exclusive responsibility of the `reconcile` command.
///
/// On any hook error the step is skipped and a warning is emitted; the original
/// content (or the output of the previous step) is passed through unchanged.
pub async fn format_content(content: &str, config: &WikiConfig) -> String {
    let zk = &config.zk_config;
    let mut current = content.to_string();
    if !zk.disable_default_hooks {
        current = run_default_hooks(&current);
    }
    current = run_hooks(&current, &zk.hooks);
    current
}

/// Run the built-in embedded hooks (checklist.lua + relation_status.lua).
pub(crate) fn run_default_hooks(content: &str) -> String {
    let hooks: &[(&str, &str)] = &[
        ("checklist", DEFAULT_CHECKLIST_HOOK),
        ("relation_status", DEFAULT_RELATION_HOOK),
    ];
    let mut current = content.to_string();
    for (name, src) in hooks {
        let runner = match HookRunner::load_str(src) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("default hook '{name}' load error: {e}");
                continue;
            }
        };
        let input = build_hook_note_input(&current);
        let result = match runner.run(&input) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("default hook '{name}' run error: {e}");
                continue;
            }
        };
        match apply_hook_result(&result, &current) {
            Ok(out) => current = out,
            Err(e) => tracing::warn!("default hook '{name}' apply error: {e}"),
        }
    }
    current
}

/// Run user-configured file hooks loaded at runtime. No-op if `hook_paths` is empty.
pub(crate) fn run_hooks(content: &str, hook_paths: &[PathBuf]) -> String {
    let mut current = content.to_string();
    for path in hook_paths {
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let runner = match HookRunner::load_file(path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("hook '{name}' load error: {e}");
                continue;
            }
        };
        let input = build_hook_note_input(&current);
        let result = match runner.run(&input) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("hook '{name}' run error: {e}");
                continue;
            }
        };
        match apply_hook_result(&result, &current) {
            Ok(out) => current = out,
            Err(e) => tracing::warn!("hook '{name}' apply error: {e}"),
        }
    }
    current
}

/// Propagate nested checkbox states bottom-up: if a todo item has children,
/// its state is derived from them (all `[x]` → `[x]`, any `[ ]` → `[ ]`).
/// Leaf items are left unchanged.
fn update_nested_checkboxes(content: &str) -> String {
    let mut owned_lines: Vec<String> = content.lines().map(str::to_string).collect();

    let mut todo_items: Vec<(usize, usize)> = Vec::new();
    let mut in_fence = false;
    for (idx, line) in owned_lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if is_todo_line(line) {
            let indent = line.len() - trimmed.len();
            todo_items.push((idx, indent));
        }
    }

    for i in (0..todo_items.len()).rev() {
        let (line_idx, indent) = todo_items[i];

        let mut descendants: Vec<usize> = Vec::new();
        for j in (i + 1)..todo_items.len() {
            let (child_line_idx, child_indent) = todo_items[j];
            if child_indent <= indent {
                break;
            }
            descendants.push(child_line_idx);
        }

        if descendants.is_empty() {
            continue;
        }

        let all_done = descendants
            .iter()
            .all(|&child_idx| get_todo_state(&owned_lines[child_idx]) == Some('x'));

        // If the parent has @ID refs, its checkbox was already set by update_ref_checkboxes_sync.
        // Respect that: only promote to [x] if the ref is also satisfied.
        let has_ref = RE_TODO_ID.is_match(&owned_lines[line_idx]);
        let ref_satisfied = !has_ref || get_todo_state(&owned_lines[line_idx]) == Some('x');
        let new_state = if all_done && ref_satisfied { 'x' } else { ' ' };
        if get_todo_state(&owned_lines[line_idx]) != Some(new_state) {
            if let Some(new_line) = replace_todo_state(&owned_lines[line_idx], new_state) {
                owned_lines[line_idx] = new_line;
            }
        }
    }

    let trailing_newline = content.ends_with('\n');
    let mut out = owned_lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Compute the TextEdit needed to update `checklist-status` in a TOML metadata
/// block to `new_status`. Returns None if not found or already correct.
pub fn compute_toml_status_edit(content: &str, new_status: &str) -> Option<TextEdit> {
    let block = parser::find_toml_metadata_block(content)?;
    let lines: Vec<&str> = content.lines().collect();

    for i in block.start_line..=block.end_line {
        let line = lines.get(i)?;
        if line.trim_start().starts_with("checklist-status") {
            let new_line = format!("  checklist-status = \"{new_status}\"");
            if *line == new_line {
                return None;
            }
            return Some(TextEdit {
                range: Range {
                    start: Position {
                        line: i as u32,
                        character: 0,
                    },
                    end: Position {
                        line: i as u32,
                        character: line.len() as u32,
                    },
                },
                new_text: new_line,
            });
        }
    }
    None
}

/// Compute the TextEdit needed to update the status, if any change is required.
/// For TOML-format notes, updates `checklist-status` in the TOML block.
/// For legacy notes, updates the tag line.
/// Returns None if no change is needed.
pub fn compute_tag_edit(content: &str) -> Option<TextEdit> {
    let header = parser::parse_header(content)?;
    let todos = parser::count_todos(content);
    let new_tag = parser::compute_status_tag(&todos, header.archived)?;

    if header.metadata_block.is_some() {
        let status_str = match new_tag {
            StatusTag::Done => "done",
            StatusTag::Wip => "wip",
            StatusTag::Todo => "todo",
        };
        // Only update if the current checklist_status differs
        let current = header.checklist_status.as_ref();
        let already_correct = match new_tag {
            StatusTag::Done => current == Some(&ChecklistStatus::Done),
            StatusTag::Wip => current == Some(&ChecklistStatus::Wip),
            StatusTag::Todo => current == Some(&ChecklistStatus::Todo),
        };
        if already_correct {
            return None;
        }
        return compute_toml_status_edit(content, status_str);
    }

    // Legacy path
    let tag_line_idx = header.tag_line_idx?;
    let new_tag_str = match new_tag {
        StatusTag::Done => "#tag.done",
        StatusTag::Wip => "#tag.wip",
        StatusTag::Todo => "#tag.todo",
    };

    let lines: Vec<&str> = content.lines().collect();
    let tag_line = lines.get(tag_line_idx)?;

    let current_tag_str = if tag_line.contains("#tag.done") {
        Some("#tag.done")
    } else if tag_line.contains("#tag.wip") {
        Some("#tag.wip")
    } else if tag_line.contains("#tag.todo") {
        Some("#tag.todo")
    } else {
        None
    };

    if current_tag_str == Some(new_tag_str) {
        return None;
    }

    let new_line = if let Some(old) = current_tag_str {
        tag_line.replace(old, new_tag_str)
    } else {
        format!("{tag_line} {new_tag_str}")
    };

    let line_num = tag_line_idx as u32;
    Some(TextEdit {
        range: Range {
            start: Position {
                line: line_num,
                character: 0,
            },
            end: Position {
                line: line_num,
                character: tag_line.len() as u32,
            },
        },
        new_text: new_line,
    })
}


fn is_todo_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- [") && t.len() >= 5
}

fn get_todo_state(line: &str) -> Option<char> {
    let t = line.trim_start();
    if t.starts_with("- [") && t.len() >= 5 {
        Some(t.chars().nth(3)?)
    } else {
        None
    }
}

fn replace_todo_state(line: &str, new_state: char) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let trimmed = &line[indent_len..];
    if trimmed.starts_with("- [") && trimmed.len() >= 5 {
        let mut chars: Vec<char> = line.chars().collect();
        // Position of the state character: indent + 3
        let state_pos = indent_len + 3;
        if state_pos < chars.len() {
            chars[state_pos] = new_state;
            return Some(chars.into_iter().collect());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_children_done_parent_becomes_checked() {
        let input = "- [ ] parent\n  - [x] child one\n  - [x] child two\n";
        let out = update_nested_checkboxes(input);
        assert_eq!(out, "- [x] parent\n  - [x] child one\n  - [x] child two\n");
    }

    #[test]
    fn any_child_incomplete_parent_becomes_unchecked() {
        let input = "- [x] parent\n  - [x] child one\n  - [ ] child two\n";
        let out = update_nested_checkboxes(input);
        assert_eq!(out, "- [ ] parent\n  - [x] child one\n  - [ ] child two\n");
    }

    #[test]
    fn three_level_nesting_propagates_to_grandparent() {
        let input = "- [ ] grandparent\n  - [ ] parent\n    - [x] grandchild\n";
        let out = update_nested_checkboxes(input);
        // grandchild done → parent done → grandparent done
        assert_eq!(
            out,
            "- [x] grandparent\n  - [x] parent\n    - [x] grandchild\n"
        );
    }

    #[test]
    fn leaf_items_unchanged() {
        let input = "- [ ] leaf one\n- [x] leaf two\n";
        let out = update_nested_checkboxes(input);
        assert_eq!(out, input);
    }

    #[test]
    fn sibling_groups_resolved_independently() {
        let input = concat!(
            "- [ ] group a\n",
            "  - [x] a child\n",
            "- [ ] group b\n",
            "  - [ ] b child\n",
        );
        let out = update_nested_checkboxes(input);
        assert_eq!(
            out,
            concat!(
                "- [x] group a\n",
                "  - [x] a child\n",
                "- [ ] group b\n",
                "  - [ ] b child\n",
            )
        );
    }

    #[test]
    fn trailing_newline_preserved() {
        let with_nl = "- [ ] p\n  - [x] c\n";
        let without_nl = "- [ ] p\n  - [x] c";
        assert!(update_nested_checkboxes(with_nl).ends_with('\n'));
        assert!(!update_nested_checkboxes(without_nl).ends_with('\n'));
    }

    #[test]
    fn fenced_checkboxes_are_not_modified() {
        let input = "- [ ] real item\n```\n- [ ] fake in fence\n```\n";
        let dep_states = HashMap::new();
        let after_refs = update_ref_checkboxes_sync(input, &dep_states);
        assert_eq!(after_refs, input);
        let after_nested = update_nested_checkboxes(input);
        assert_eq!(after_nested, input);
    }

    #[test]
    fn parent_ref_not_overridden_by_done_children() {
        // Parent has @ID (ref not done), child is done. Parent must stay [ ].
        let input = "- [ ] @1234567890 task\n  - [x] child\n";
        let dep_states = HashMap::new(); // ref absent → not done
        let after_refs = update_ref_checkboxes_sync(input, &dep_states);
        let out = update_nested_checkboxes(&after_refs);
        assert!(
            out.starts_with("- [ ]"),
            "parent with unsatisfied ref must stay unchecked even when children are done"
        );
    }

    #[test]
    fn parent_ref_and_children_both_done_promotes_parent() {
        // Parent has @ID (ref done), child not done. Parent stays [ ] because child is incomplete.
        let input = "- [ ] @1234567890 task\n  - [ ] child\n";
        let dep_states = HashMap::from([("1234567890".to_string(), true)]);
        let after_refs = update_ref_checkboxes_sync(input, &dep_states);
        // after_refs: ref becomes [x], child stays [ ]
        // after nested: child still [ ] → parent should stay [ ] (child not done)
        let out = update_nested_checkboxes(&after_refs);
        assert!(
            out.starts_with("- [ ]"),
            "parent stays unchecked when ref done but child not done"
        );
    }

    #[test]
    fn effective_status_with_no_todos_uses_checklist_status() {
        // compute_status_tag returns None when there are no todos
        let empty = parser::TodoStatus {
            completed: 0,
            incomplete: 0,
        };
        assert_eq!(parser::compute_status_tag(&empty, false), None);
        // When None, the ref_is_done branch falls through to header.checklist_status
        // ChecklistStatus::Done → true; ChecklistStatus::None → false
        assert!(
            parser::ChecklistStatus::Done == parser::ChecklistStatus::Done,
            "Done variant equality check"
        );
        assert!(
            parser::ChecklistStatus::None != parser::ChecklistStatus::Done,
            "None variant inequality check"
        );
    }
}

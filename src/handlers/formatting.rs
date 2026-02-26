use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser::{self, StatusTag};

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

/// Format `content`:
/// 1. Update `- [ ] @<id>` / `- [x] @<id>` checkboxes by reading referenced
///    notes from `note_dir` — all IDs on a line must be Done for the box to be
///    checked, otherwise the box is cleared.
/// 2. Recompute and apply the note's own status tag based on the updated
///    checkbox state.
pub async fn format_content(content: &str, note_dir: &Path) -> String {
    let after_refs = update_ref_checkboxes(content, note_dir).await;
    let after_nested = update_nested_checkboxes(&after_refs);
    apply_tag_edit(&after_nested)
}

/// Returns true iff the note at `path` has an effective tag of `done`.
///
/// "Effective" means: simulate what `apply_tag_edit` would produce, then read
/// the resulting tag line.  This way the judgment is always based on the tag
/// (not on raw todo counts), while still handling the case where the on-disk
/// tag is stale.
///
/// Concretely:
/// - If `compute_tag_edit` would change the tag line → use the new text.
/// - If the tag line is already correct (no edit needed) → use the existing one.
/// Either way we check for the literal string `#tag.done`.
async fn ref_is_done(path: &Path) -> bool {
    let Ok(content) = tokio::fs::read_to_string(path).await else {
        return false;
    };
    let Some(header) = parser::parse_header(&content) else {
        return false;
    };
    let lines: Vec<&str> = content.lines().collect();
    let existing = lines
        .get(header.tag_line_idx)
        .copied()
        .unwrap_or("")
        .to_string();
    let effective = match compute_tag_edit(&content) {
        Some(edit) => edit.new_text,
        None => existing,
    };
    effective.contains("#tag.done")
}

/// Update `- [ ] @id` / `- [x] @id` checkboxes in `content`.
/// All `@id` references on a todo line must resolve to Done for the box to be
/// checked; if any is not Done (or the file cannot be read) the box is cleared.
async fn update_ref_checkboxes(content: &str, note_dir: &Path) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let mut changed = false;

    for (i, line) in lines.iter().enumerate() {
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
        let mut all_done = true;
        for id in &ids {
            if !ref_is_done(&note_dir.join(format!("{id}.typ"))).await {
                all_done = false;
                break;
            }
        }
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

/// Propagate nested checkbox states bottom-up: if a todo item has children,
/// its state is derived from them (all `[x]` → `[x]`, any `[ ]` → `[ ]`).
/// Leaf items are left unchanged.
fn update_nested_checkboxes(content: &str) -> String {
    let mut owned_lines: Vec<String> = content.lines().map(str::to_string).collect();

    let todo_items: Vec<(usize, usize)> = owned_lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            if is_todo_line(line) {
                let indent = line.len() - line.trim_start().len();
                Some((idx, indent))
            } else {
                None
            }
        })
        .collect();

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

        let new_state = if all_done { 'x' } else { ' ' };
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

/// Compute the TextEdit needed to update the tag line, if any change is required.
/// Returns None if no change is needed.
pub fn compute_tag_edit(content: &str) -> Option<TextEdit> {
    let header = parser::parse_header(content)?;
    let todos = parser::count_todos(content);
    let new_tag = parser::compute_status_tag(&todos, header.archived)?;

    let new_tag_str = match new_tag {
        StatusTag::Done => "#tag.done",
        StatusTag::Wip => "#tag.wip",
        StatusTag::Todo => "#tag.todo",
    };

    let lines: Vec<&str> = content.lines().collect();
    let tag_line = lines.get(header.tag_line_idx)?;

    // Check if the tag line already has the correct status tag
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

    let line_num = header.tag_line_idx as u32;
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

/// Apply cross-file checkbox propagation: for all notes containing
/// `- [ ] @<note_id>` or `- [x] @<note_id>`, update the checkbox state.
pub async fn propagate_tag_change(
    note_id: &str,
    new_tag: &StatusTag,
    index: &Arc<NoteIndex>,
) -> Result<WorkspaceEdit> {
    let new_state = if *new_tag == StatusTag::Done {
        'x'
    } else {
        ' '
    };
    let pattern = format!("@{note_id}");

    let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();

    // Use backlinks to find candidate files
    let backlinks = index.get_backlinks(note_id);
    let mut seen_files = std::collections::HashSet::new();
    for loc in &backlinks {
        seen_files.insert(loc.file.clone());
    }

    for file_path in &seen_files {
        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut edits = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            if !line.contains(&pattern) {
                continue;
            }
            // Only update todo lines
            if !is_todo_line(line) {
                continue;
            }
            let current_state = get_todo_state(line);
            if current_state == Some(new_state) {
                continue;
            }
            if let Some(new_line) = replace_todo_state(line, new_state) {
                edits.push(TextEdit {
                    range: Range {
                        start: Position {
                            line: line_num as u32,
                            character: 0,
                        },
                        end: Position {
                            line: line_num as u32,
                            character: line.len() as u32,
                        },
                    },
                    new_text: new_line,
                });
            }
        }
        if !edits.is_empty() {
            if let Ok(uri) = Url::from_file_path(file_path) {
                changes.insert(uri, edits);
            }
        }
    }

    Ok(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
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
}

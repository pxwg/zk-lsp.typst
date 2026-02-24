use std::sync::Arc;

use anyhow::Result;
use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser::{self, StatusTag};

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
            start: Position { line: line_num, character: 0 },
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
    let new_state = if *new_tag == StatusTag::Done { 'x' } else { ' ' };
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
                        start: Position { line: line_num as u32, character: 0 },
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

    Ok(WorkspaceEdit { changes: Some(changes), ..Default::default() })
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

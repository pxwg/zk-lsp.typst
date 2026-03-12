use tower_lsp::lsp_types::*;

use super::diagnostics::DiagnosticData;
use crate::parser;

/// Build code actions from diagnostics with source "zk-lsp".
pub fn get_code_actions(uri: &Url, diagnostics: &[Diagnostic]) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();

    for diag in diagnostics {
        if diag.source.as_deref() != Some("zk-lsp") {
            continue;
        }
        let data: DiagnosticData = match diag
            .data
            .as_ref()
            .and_then(|d| serde_json::from_value(d.clone()).ok())
        {
            Some(d) => d,
            None => continue,
        };
        let Some(ref new_id) = data.new_id else {
            continue;
        };

        let old_text = format!("@{}", data.old_id);
        let new_text = format!("@{new_id}");

        // Action 1: Replace @old with @new
        actions.push(make_replace_action(
            uri,
            diag,
            format!("Fix: Replace {old_text} with {new_text}"),
            new_text.clone(),
        ));

        // Action 2 (legacy only): Append @old @new
        if data.kind == "legacy" {
            let append_text = format!("{old_text} {new_text}");
            actions.push(make_replace_action(
                uri,
                diag,
                format!("Fix: Append new insight ({old_text} {new_text})"),
                append_text,
            ));
        }
    }

    actions
}

/// Generate metadata quick-actions when the cursor is within the TOML block.
///
/// Action A — Toggle `checklist-status` to any other valid value.
/// Action B — Mark/unmark `relation` as archived or legacy (with `relation-target` placeholder).
pub fn get_metadata_actions(
    uri: &Url,
    content: &str,
    range: Range,
) -> Vec<CodeActionOrCommand> {
    let Some(block) = parser::find_toml_metadata_block(content) else {
        return Vec::new();
    };

    // Only generate actions when the cursor range overlaps the TOML block
    let block_start = block.start_line as u32;
    let block_end = block.end_line as u32;
    if range.end.line < block_start || range.start.line > block_end {
        return Vec::new();
    }

    let Some(parsed) = parser::parse_toml_metadata(&block.toml_content) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    let toml_line_count = block.toml_content.lines().count();
    let toml_start = block.end_line.saturating_sub(toml_line_count);

    let mut actions = Vec::new();

    // --- Action A: Toggle checklist-status ---
    let current_status = match parsed.checklist_status {
        parser::ChecklistStatus::None => "none",
        parser::ChecklistStatus::Todo => "todo",
        parser::ChecklistStatus::Wip => "wip",
        parser::ChecklistStatus::Done => "done",
    };
    for new_status in ["none", "todo", "wip", "done"] {
        if new_status == current_status {
            continue;
        }
        if let Some(edit) =
            crate::handlers::formatting::compute_toml_status_edit(content, new_status)
        {
            let workspace_edit = WorkspaceEdit {
                changes: Some([(uri.clone(), vec![edit])].into_iter().collect()),
                ..Default::default()
            };
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("ZK: Set checklist-status to {new_status}"),
                kind: Some(CodeActionKind::REFACTOR),
                edit: Some(workspace_edit),
                ..Default::default()
            }));
        }
    }

    // --- Action B: Toggle relation ---
    let mut relation_line_idx: Option<usize> = None;
    let mut relation_target_line_idx: Option<usize> = None;
    for (i, toml_line) in block.toml_content.lines().enumerate() {
        let t = toml_line.trim_start();
        if t.starts_with("relation") && !t.starts_with("relation-target") {
            relation_line_idx = Some(toml_start + i);
        } else if t.starts_with("relation-target") {
            relation_target_line_idx = Some(toml_start + i);
        }
    }

    if let Some(rel_file_line) = relation_line_idx {
        let rel_line_text = lines.get(rel_file_line).copied().unwrap_or("");

        let current_relation = match parsed.relation {
            parser::Relation::Active => "active",
            parser::Relation::Archived => "archived",
            parser::Relation::Legacy => "legacy",
        };

        if current_relation == "active" {
            for new_rel in ["archived", "legacy"] {
                let mut edits = Vec::new();
                edits.push(TextEdit {
                    range: Range {
                        start: Position { line: rel_file_line as u32, character: 0 },
                        end: Position {
                            line: rel_file_line as u32,
                            character: rel_line_text.len() as u32,
                        },
                    },
                    new_text: format!("  relation = \"{new_rel}\""),
                });
                if let Some(rt_line) = relation_target_line_idx {
                    let rt_text = lines.get(rt_line).copied().unwrap_or("");
                    edits.push(TextEdit {
                        range: Range {
                            start: Position { line: rt_line as u32, character: 0 },
                            end: Position {
                                line: rt_line as u32,
                                character: rt_text.len() as u32,
                            },
                        },
                        new_text: "  relation-target = [\"\"]".to_string(),
                    });
                } else {
                    // Insert new line after relation line
                    edits.push(TextEdit {
                        range: Range {
                            start: Position { line: rel_file_line as u32 + 1, character: 0 },
                            end: Position { line: rel_file_line as u32 + 1, character: 0 },
                        },
                        new_text: "  relation-target = [\"\"]\n".to_string(),
                    });
                }
                let title = if new_rel == "archived" {
                    "ZK: Mark as archived".to_string()
                } else {
                    "ZK: Mark as legacy".to_string()
                };
                let workspace_edit = WorkspaceEdit {
                    changes: Some([(uri.clone(), edits)].into_iter().collect()),
                    ..Default::default()
                };
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title,
                    kind: Some(CodeActionKind::REFACTOR),
                    edit: Some(workspace_edit),
                    ..Default::default()
                }));
            }
        } else {
            // Mark as active: clear relation-target
            let mut edits = Vec::new();
            edits.push(TextEdit {
                range: Range {
                    start: Position { line: rel_file_line as u32, character: 0 },
                    end: Position {
                        line: rel_file_line as u32,
                        character: rel_line_text.len() as u32,
                    },
                },
                new_text: "  relation = \"active\"".to_string(),
            });
            if let Some(rt_line) = relation_target_line_idx {
                let rt_text = lines.get(rt_line).copied().unwrap_or("");
                edits.push(TextEdit {
                    range: Range {
                        start: Position { line: rt_line as u32, character: 0 },
                        end: Position { line: rt_line as u32, character: rt_text.len() as u32 },
                    },
                    new_text: "  relation-target = []".to_string(),
                });
            }
            let workspace_edit = WorkspaceEdit {
                changes: Some([(uri.clone(), edits)].into_iter().collect()),
                ..Default::default()
            };
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "ZK: Mark as active".to_string(),
                kind: Some(CodeActionKind::REFACTOR),
                edit: Some(workspace_edit),
                ..Default::default()
            }));

            // Also allow switching to the other non-active relation
            let other_rel = if current_relation == "archived" { "legacy" } else { "archived" };
            let mut edits = Vec::new();
            edits.push(TextEdit {
                range: Range {
                    start: Position { line: rel_file_line as u32, character: 0 },
                    end: Position {
                        line: rel_file_line as u32,
                        character: rel_line_text.len() as u32,
                    },
                },
                new_text: format!("  relation = \"{other_rel}\""),
            });
            if let Some(rt_line) = relation_target_line_idx {
                let rt_text = lines.get(rt_line).copied().unwrap_or("");
                edits.push(TextEdit {
                    range: Range {
                        start: Position { line: rt_line as u32, character: 0 },
                        end: Position { line: rt_line as u32, character: rt_text.len() as u32 },
                    },
                    new_text: "  relation-target = [\"\"]".to_string(),
                });
            } else {
                edits.push(TextEdit {
                    range: Range {
                        start: Position { line: rel_file_line as u32 + 1, character: 0 },
                        end: Position { line: rel_file_line as u32 + 1, character: 0 },
                    },
                    new_text: "  relation-target = [\"\"]\n".to_string(),
                });
            }
            let workspace_edit = WorkspaceEdit {
                changes: Some([(uri.clone(), edits)].into_iter().collect()),
                ..Default::default()
            };
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("ZK: Mark as {other_rel}"),
                kind: Some(CodeActionKind::REFACTOR),
                edit: Some(workspace_edit),
                ..Default::default()
            }));
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE_TOML_ACTIVE: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  checklist-status = \"none\"\n",
        "  relation = \"active\"\n",
        "  relation-target = []\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Test Note <2603110000>\n",
    );

    const NOTE_TOML_ARCHIVED: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  checklist-status = \"done\"\n",
        "  relation = \"archived\"\n",
        "  relation-target = [\"2603110001\"]\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Test Note <2603110002>\n",
    );

    fn make_uri() -> Url {
        Url::parse("file:///wiki/note/2603110000.typ").unwrap()
    }

    fn inside_block_range() -> Range {
        Range {
            start: Position { line: 4, character: 0 },
            end: Position { line: 4, character: 0 },
        }
    }

    fn outside_block_range() -> Range {
        Range {
            start: Position { line: 11, character: 0 },
            end: Position { line: 11, character: 0 },
        }
    }

    #[test]
    fn test_metadata_actions_checklist_status_cycle() {
        let uri = make_uri();
        let actions = get_metadata_actions(&uri, NOTE_TOML_ACTIVE, inside_block_range());
        let titles: Vec<&str> = actions
            .iter()
            .filter_map(|a| {
                if let CodeActionOrCommand::CodeAction(ca) = a {
                    Some(ca.title.as_str())
                } else {
                    None
                }
            })
            .collect();
        // Should have status actions for todo, wip, done (not none, which is current)
        assert!(titles.iter().any(|t| t.contains("todo")));
        assert!(titles.iter().any(|t| t.contains("wip")));
        assert!(titles.iter().any(|t| t.contains("done")));
        assert!(!titles.iter().any(|t| t.contains("\"none\"")));
    }

    #[test]
    fn test_metadata_actions_mark_archived_inserts_relation_target() {
        let uri = make_uri();
        let actions = get_metadata_actions(&uri, NOTE_TOML_ACTIVE, inside_block_range());
        let archived_action = actions.iter().find_map(|a| {
            if let CodeActionOrCommand::CodeAction(ca) = a {
                if ca.title.contains("archived") {
                    return Some(ca);
                }
            }
            None
        });
        assert!(archived_action.is_some(), "Mark as archived action must exist");
        let ca = archived_action.unwrap();
        let edits = ca
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.values().next())
            .unwrap();
        // One edit for relation, one for relation-target
        assert_eq!(edits.len(), 2);
        assert!(edits[0].new_text.contains("archived"));
        assert!(edits[1].new_text.contains("relation-target"));
    }

    #[test]
    fn test_metadata_actions_outside_block_returns_empty() {
        let uri = make_uri();
        let actions = get_metadata_actions(&uri, NOTE_TOML_ACTIVE, outside_block_range());
        assert!(actions.is_empty());
    }

    #[test]
    fn test_metadata_actions_unmark_archived() {
        let uri = Url::parse("file:///wiki/note/2603110002.typ").unwrap();
        let range = Range {
            start: Position { line: 5, character: 0 },
            end: Position { line: 5, character: 0 },
        };
        let actions = get_metadata_actions(&uri, NOTE_TOML_ARCHIVED, range);
        let mark_active = actions.iter().find_map(|a| {
            if let CodeActionOrCommand::CodeAction(ca) = a {
                if ca.title == "ZK: Mark as active" {
                    return Some(ca);
                }
            }
            None
        });
        assert!(mark_active.is_some(), "Mark as active action must exist for archived note");
        // Should also offer switching to the other non-active relation
        let mark_legacy = actions.iter().find_map(|a| {
            if let CodeActionOrCommand::CodeAction(ca) = a {
                if ca.title == "ZK: Mark as legacy" {
                    return Some(ca);
                }
            }
            None
        });
        assert!(mark_legacy.is_some(), "Mark as legacy action must exist when currently archived");
    }
}

fn make_replace_action(
    uri: &Url,
    diag: &Diagnostic,
    title: String,
    new_text: String,
) -> CodeActionOrCommand {
    let edit = WorkspaceEdit {
        changes: Some(
            [(
                uri.clone(),
                vec![TextEdit {
                    range: diag.range,
                    new_text,
                }],
            )]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(edit),
        ..Default::default()
    })
}

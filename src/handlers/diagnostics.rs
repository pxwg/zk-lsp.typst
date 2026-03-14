use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;
use crate::reconcile::types::{DiagnosticSeverity as ReconcileSeverity, ReconcileDiagnostic};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticData {
    pub kind: String, // "archived" | "legacy"
    pub old_id: String,
    pub new_ids: Option<Vec<String>>,
    pub replacement: Option<String>,
}

/// Generate diagnostics for all @ID references in the document content.
pub fn get_diagnostics(content: &str, index: &Arc<NoteIndex>, uri_path: &str) -> Vec<Diagnostic> {
    let note_id = uri_path
        .rsplit('/')
        .next()
        .and_then(|s| s.strip_suffix(".typ"))
        .unwrap_or("");
    let mut diagnostics = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let refs = parser::find_all_refs(line);
        for r in refs {
            let range = Range {
                start: Position {
                    line: line_num as u32,
                    character: parser::byte_to_utf16(line, r.start_char as usize),
                },
                end: Position {
                    line: line_num as u32,
                    character: parser::byte_to_utf16(line, r.end_char as usize),
                },
            };

            let Some(info) = index.get(&r.id) else {
                diagnostics.push(Diagnostic {
                    range,
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("zk-lsp".into()),
                    message: format!("Note @{} does not exist", r.id),
                    ..Default::default()
                });
                continue;
            };

            if info.archived {
                // Suppress if this note is a relation-target of the archived note
                if info.relation_target.iter().any(|t| t == note_id) {
                    continue;
                }
                let mut msg = format!("Note @{} is archived.", r.id);
                if !info.relation_target.is_empty() {
                    let targets = info
                        .relation_target
                        .iter()
                        .map(|id| format!("@{id}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    msg.push_str(&format!(" New ids: {targets}"));
                }
                let data = DiagnosticData {
                    kind: "archived".into(),
                    old_id: r.id.clone(),
                    new_ids: Some(info.relation_target.clone()),
                    replacement: None,
                };
                diagnostics.push(Diagnostic {
                    range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("zk-lsp".into()),
                    message: msg,
                    data: Some(serde_json::to_value(data).unwrap()),
                    ..Default::default()
                });
            } else if info.legacy {
                // Suppress if this note is a relation-target of the legacy note
                if info.relation_target.iter().any(|t| t == note_id) {
                    continue;
                }
                // Suppress if the same line already mentions any successor.
                let after = &line[r.end_char as usize..];
                let has_successor_on_same_line = parser::find_all_refs(after)
                    .into_iter()
                    .any(|next| info.relation_target.iter().any(|id| id == &next.id));
                let should_warn = !has_successor_on_same_line;

                if should_warn {
                    let mut msg = format!("Note @{} is legacy.", r.id);
                    if !info.relation_target.is_empty() {
                        let targets = info
                            .relation_target
                            .iter()
                            .map(|id| format!("@{id}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        msg.push_str(&format!(" New ids: {targets}"));
                    }
                    let data = DiagnosticData {
                        kind: "legacy".into(),
                        old_id: r.id.clone(),
                        new_ids: Some(info.relation_target.clone()),
                        replacement: None,
                    };
                    diagnostics.push(Diagnostic {
                        range,
                        severity: Some(DiagnosticSeverity::INFORMATION),
                        source: Some("zk-lsp".into()),
                        message: msg,
                        data: Some(serde_json::to_value(data).unwrap()),
                        ..Default::default()
                    });
                }
            }
        }
    }

    diagnostics
}

fn extract_toml_string_value(trimmed_line: &str) -> Option<&str> {
    let eq_pos = trimmed_line.find('=')?;
    let after_eq = trimmed_line[eq_pos + 1..].trim();
    after_eq.strip_prefix('"')?.strip_suffix('"')
}

fn extract_toml_field_name(trimmed_line: &str) -> Option<&str> {
    trimmed_line.split_once('=').map(|(field, _)| field.trim())
}

/// Validate TOML metadata block fields and produce diagnostics.
pub fn get_schema_diagnostics(content: &str, index: &Arc<NoteIndex>) -> Vec<Diagnostic> {
    let lines: Vec<&str> = content.lines().collect();
    let Some(block) = parser::find_toml_metadata_block(content) else {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: lines.first().map(|l| l.len()).unwrap_or(0) as u32,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Missing `zk-metadata` TOML block".to_string(),
            ..Default::default()
        }];
    };

    let title_line_idx = lines
        .iter()
        .enumerate()
        .skip(block.end_line + 1)
        .find_map(|(idx, line)| parser::RE_TITLE.is_match(line).then_some(idx));

    if title_line_idx.is_none() {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: block.end_line as u32,
                    character: 0,
                },
                end: Position {
                    line: block.end_line as u32,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Missing note title heading (`= Title <ID>`)".to_string(),
            ..Default::default()
        }];
    }

    // Try to parse as TOML; if invalid, return a single parse-error diagnostic
    if let Err(e) = block.toml_content.parse::<toml::Value>() {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: block.start_line as u32,
                    character: 0,
                },
                end: Position {
                    line: block.end_line as u32,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: format!("TOML parse error: {e}"),
            ..Default::default()
        }];
    }

    let mut diagnostics = Vec::new();
    let toml_line_count = block.toml_content.lines().count();
    let toml_start = block.end_line.saturating_sub(toml_line_count);
    let expected_fields = [
        ("schema-version", "  schema-version = 1\n"),
        ("aliases", "  aliases = []\n"),
        ("abstract", "  abstract = \"\"\n"),
        ("keywords", "  keywords = []\n"),
        ("generated", "  generated = true\n"),
        ("checklist-status", "  checklist-status = \"none\"\n"),
        ("relation", "  relation = \"active\"\n"),
        ("relation-target", "  relation-target = []\n"),
    ];
    let mut present_fields = std::collections::HashMap::new();

    // Per-line field validation
    for (i, toml_line) in block.toml_content.lines().enumerate() {
        let file_line = toml_start + i;
        let file_line_text = lines.get(file_line).copied().unwrap_or("");
        let trimmed = toml_line.trim_start();
        if let Some(field_name) = extract_toml_field_name(trimmed) {
            if let Some((field, _)) = expected_fields
                .iter()
                .find(|(field, _)| *field == field_name)
            {
                present_fields.insert(*field, file_line);
            }
        }

        let line_range = Range {
            start: Position {
                line: file_line as u32,
                character: 0,
            },
            end: Position {
                line: file_line as u32,
                character: file_line_text.len() as u32,
            },
        };

        if trimmed.starts_with("checklist-status") {
            if let Some(val) = extract_toml_string_value(trimmed) {
                if !["none", "todo", "wip", "done"].contains(&val) {
                    diagnostics.push(Diagnostic {
                        range: line_range,
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("zk-lsp".into()),
                        message: format!(
                            "Invalid checklist-status \"{val}\". Expected: none, todo, wip, done"
                        ),
                        ..Default::default()
                    });
                }
            }
        } else if trimmed.starts_with("relation") && !trimmed.starts_with("relation-target") {
            if let Some(val) = extract_toml_string_value(trimmed) {
                if !["active", "archived", "legacy"].contains(&val) {
                    diagnostics.push(Diagnostic {
                        range: line_range,
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("zk-lsp".into()),
                        message: format!(
                            "Invalid relation \"{val}\". Expected: active, archived, legacy"
                        ),
                        ..Default::default()
                    });
                }
            }
        }
    }

    for (idx, (field, replacement)) in expected_fields.iter().enumerate() {
        if present_fields.contains_key(field) {
            continue;
        }
        let insert_line = expected_fields[idx + 1..]
            .iter()
            .find_map(|(next_field, _)| present_fields.get(next_field).copied())
            .unwrap_or(block.end_line);
        diagnostics.push(Diagnostic {
            range: Range {
                start: Position {
                    line: insert_line as u32,
                    character: 0,
                },
                end: Position {
                    line: insert_line as u32,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::INFORMATION),
            source: Some("zk-lsp".into()),
            message: format!("Missing TOML field `{field}`"),
            data: Some(
                serde_json::to_value(DiagnosticData {
                    kind: "missing-toml-field".into(),
                    old_id: (*field).to_string(),
                    new_ids: None,
                    replacement: Some((*replacement).to_string()),
                })
                .unwrap(),
            ),
            ..Default::default()
        });
    }

    // Semantic checks using parsed values
    if let Some(parsed) = parser::parse_toml_metadata(&block.toml_content) {
        use crate::parser::Relation;

        // relation != "active" but relation-target is empty → WARNING on relation line
        if parsed.relation != Relation::Active && parsed.relation_target.is_empty() {
            if let Some((i, _)) = block.toml_content.lines().enumerate().find(|(_, l)| {
                let t = l.trim_start();
                t.starts_with("relation") && !t.starts_with("relation-target")
            }) {
                let file_line = toml_start + i;
                let file_line_text = lines.get(file_line).copied().unwrap_or("");
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line: file_line as u32,
                            character: 0,
                        },
                        end: Position {
                            line: file_line as u32,
                            character: file_line_text.len() as u32,
                        },
                    },
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("zk-lsp".into()),
                    message: "relation is not 'active' but relation-target is empty".to_string(),
                    ..Default::default()
                });
            }
        }

        // relation-target IDs not found in index → WARNING on relation-target line
        let unknown_ids: Vec<&str> = parsed
            .relation_target
            .iter()
            .filter(|id| !id.is_empty() && index.get(id).is_none())
            .map(String::as_str)
            .collect();

        if !unknown_ids.is_empty() {
            if let Some((i, _)) = block
                .toml_content
                .lines()
                .enumerate()
                .find(|(_, l)| l.trim_start().starts_with("relation-target"))
            {
                let file_line = toml_start + i;
                let file_line_text = lines.get(file_line).copied().unwrap_or("");
                for id in unknown_ids {
                    diagnostics.push(Diagnostic {
                        range: Range {
                            start: Position {
                                line: file_line as u32,
                                character: 0,
                            },
                            end: Position {
                                line: file_line as u32,
                                character: file_line_text.len() as u32,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("zk-lsp".into()),
                        message: format!("Note @{id} does not exist in the index"),
                        ..Default::default()
                    });
                }
            }
        }
    }

    diagnostics
}

/// Generate a HINT diagnostic for an orphan note.
///
/// A note is orphan only when BOTH conditions hold:
/// 1. No other note references it (no backlinks in the index)
/// 2. It has no outgoing `@ID` references itself
///
/// Returns `None` if the note is not in the index or is not fully isolated.
pub fn get_orphan_diagnostic(
    content: &str,
    uri_path: &str,
    index: &Arc<NoteIndex>,
) -> Option<Diagnostic> {
    let note_id = uri_path
        .rsplit('/')
        .next()
        .and_then(|s| s.strip_suffix(".typ"))?;

    // Only flag notes that are in the index
    if index.get(note_id).is_none() {
        return None;
    }

    // Not an orphan if it has inbound links
    if !index.get_backlinks(note_id).is_empty() {
        return None;
    }

    // Not an orphan if it has outgoing links
    if !parser::find_all_refs_filtered(content).is_empty() {
        return None;
    }

    // Find the title line (contains `<{note_id}>`)
    let needle = format!("<{note_id}>");
    let (line_num, _line_text) = content
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(&needle))?;

    Some(Diagnostic {
        range: Range {
            start: Position {
                line: line_num as u32,
                character: 0,
            },
            end: Position {
                line: line_num as u32,
                character: 0,
            },
        },
        severity: Some(DiagnosticSeverity::HINT),
        source: Some("zk-lsp".into()),
        message: format!("Orphan note: no inbound or outbound @ID references"),
        ..Default::default()
    })
}

/// Generate WARNING diagnostics for Ref checklist items that are non-leaf nodes.
///
/// A RefItem (`- [ ] @ID`) must always be a leaf. If it has child items (next item
/// with strictly greater indent), the @ID targets will be semantically ignored by
/// the leaf rule, silently breaking the dependency.
pub fn get_reconcile_diagnostics(
    content: &str,
    file_path: &std::path::Path,
    reconcile_diagnostics: &[ReconcileDiagnostic],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for diag in reconcile_diagnostics {
        let Some(location) = &diag.location else {
            continue;
        };
        if location.file_path != file_path {
            continue;
        }

        let line_text = content.lines().nth(location.line).unwrap_or("");
        diagnostics.push(Diagnostic {
            range: Range {
                start: Position {
                    line: location.line as u32,
                    character: parser::byte_to_utf16(line_text, location.byte_start as usize),
                },
                end: Position {
                    line: location.line as u32,
                    character: parser::byte_to_utf16(line_text, location.byte_end as usize),
                },
            },
            severity: Some(match diag.severity {
                ReconcileSeverity::Error => DiagnosticSeverity::ERROR,
            }),
            source: Some("zk-lsp".into()),
            message: diag.message.clone(),
            ..Default::default()
        });
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::index::{BacklinkLocation, NoteIndex, NoteInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn make_index() -> Arc<NoteIndex> {
        let config = Arc::new(WikiConfig::from_root(PathBuf::from("/tmp/wiki")));
        Arc::new(NoteIndex::new(config))
    }

    fn insert_note(index: &Arc<NoteIndex>, id: &str) {
        index.notes.insert(
            id.to_string(),
            NoteInfo {
                id: id.to_string(),
                title: format!("Note {id}"),
                archived: false,
                legacy: false,
                alt_id: None,
                evo_id: None,
                relation_target: vec![],
                aliases: vec![],
                keywords: vec![],
                abstract_text: None,
                checklist_status: None,
                path: PathBuf::from(format!("/tmp/wiki/note/{id}.typ")),
            },
        );
    }

    fn insert_legacy_note(index: &Arc<NoteIndex>, id: &str, targets: &[&str]) {
        index.notes.insert(
            id.to_string(),
            NoteInfo {
                id: id.to_string(),
                title: format!("Note {id}"),
                archived: false,
                legacy: true,
                alt_id: targets.first().map(|s| s.to_string()),
                evo_id: targets.first().map(|s| s.to_string()),
                relation_target: targets.iter().map(|s| s.to_string()).collect(),
                aliases: vec![],
                keywords: vec![],
                abstract_text: None,
                checklist_status: None,
                path: PathBuf::from(format!("/tmp/wiki/note/{id}.typ")),
            },
        );
    }

    fn add_backlink(index: &Arc<NoteIndex>, target_id: &str, from_id: &str) {
        index
            .backlinks
            .entry(target_id.to_string())
            .or_default()
            .push(BacklinkLocation {
                file: PathBuf::from(format!("/tmp/wiki/note/{from_id}.typ")),
                line: 0,
                start_char: 0,
                end_char: 11,
            });
    }

    #[test]
    fn test_dead_link_produces_error() {
        let index = make_index();
        // Note 1111111111 is NOT in the index → dead link
        let content = "- [ ] @1111111111\n";
        let diags = get_diagnostics(content, &index, "/wiki/note/9999999999.typ");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(diags[0].message.contains("does not exist"));
    }

    #[test]
    fn test_missing_metadata_block_produces_error() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics(content, &index);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].message, "Missing `zk-metadata` TOML block");
    }

    #[test]
    fn test_missing_title_heading_produces_error() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  aliases = []\n",
            "  abstract = \"\"\n",
            "  keywords = []\n",
            "  generated = true\n",
            "  checklist-status = \"none\"\n",
            "  relation = \"active\"\n",
            "  relation-target = []\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
        );
        let diags = get_schema_diagnostics(content, &index);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diags[0].message,
            "Missing note title heading (`= Title <ID>`)"
        );
    }

    #[test]
    fn test_orphan_note_produces_hint() {
        let index = make_index();
        insert_note(&index, "1111111111");
        // No backlinks, no outgoing refs → orphan
        let content = "= My Note <1111111111>\n";
        let diag = get_orphan_diagnostic(content, "/wiki/note/1111111111.typ", &index);
        assert!(diag.is_some());
        let d = diag.unwrap();
        assert_eq!(d.severity, Some(DiagnosticSeverity::HINT));
        assert!(d.message.contains("Orphan note"));
    }

    #[test]
    fn test_non_orphan_no_hint_inbound() {
        let index = make_index();
        insert_note(&index, "1111111111");
        add_backlink(&index, "1111111111", "2222222222");
        // Has inbound backlink → not orphan
        let content = "= My Note <1111111111>\n";
        let diag = get_orphan_diagnostic(content, "/wiki/note/1111111111.typ", &index);
        assert!(diag.is_none());
    }

    #[test]
    fn test_non_orphan_no_hint_outgoing() {
        let index = make_index();
        insert_note(&index, "1111111111");
        // No backlinks, but note has outgoing ref → not orphan
        let content = "= My Note <1111111111>\n- [ ] @2222222222\n";
        let diag = get_orphan_diagnostic(content, "/wiki/note/1111111111.typ", &index);
        assert!(diag.is_none());
    }

    #[test]
    fn test_schema_missing_fields_produce_info_diagnostics() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  relation = \"active\"\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics(content, &index);
        assert!(diags
            .iter()
            .any(|d| d.message == "Missing TOML field `aliases`"));
        assert!(diags
            .iter()
            .any(|d| d.message == "Missing TOML field `checklist-status`"));
        assert!(diags
            .iter()
            .filter(|d| d.message.starts_with("Missing TOML field"))
            .all(|d| d.severity == Some(DiagnosticSeverity::INFORMATION)));
    }

    #[test]
    fn test_schema_does_not_flag_existing_relation_target() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  aliases = []\n",
            "  abstract = \"\"\n",
            "  keywords = []\n",
            "  generated = true\n",
            "  checklist-status = \"done\"\n",
            "  relation = \"archived\"\n",
            "  relation-target = [ \"2602082037\" ]\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics(content, &index);
        assert!(!diags
            .iter()
            .any(|d| d.message == "Missing TOML field `relation-target`"));
    }

    #[test]
    fn test_legacy_diagnostic_lists_all_relation_targets() {
        let index = make_index();
        insert_legacy_note(&index, "1111111111", &["2222222222", "3333333333"]);
        let diags = get_diagnostics("- [ ] @1111111111\n", &index, "/wiki/note/9999999999.typ");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("@2222222222"));
        assert!(diags[0].message.contains("@3333333333"));
        let data: DiagnosticData = serde_json::from_value(diags[0].data.clone().unwrap()).unwrap();
        assert_eq!(
            data.new_ids.unwrap(),
            vec!["2222222222".to_string(), "3333333333".to_string()]
        );
    }

    #[test]
    fn test_legacy_diagnostic_suppressed_if_any_relation_target_already_on_line() {
        let index = make_index();
        insert_legacy_note(&index, "1111111111", &["2222222222", "3333333333"]);
        insert_note(&index, "2222222222");
        insert_note(&index, "3333333333");
        let diags = get_diagnostics(
            "- [ ] @1111111111 @3333333333\n",
            &index,
            "/wiki/note/9999999999.typ",
        );
        assert!(diags.is_empty());
    }
}

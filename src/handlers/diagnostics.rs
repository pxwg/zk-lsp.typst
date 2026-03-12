use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::*;

use crate::cycle::DependencyCycle;
use crate::index::NoteIndex;
use crate::parser;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticData {
    pub kind: String, // "archived" | "legacy"
    pub old_id: String,
    pub new_id: Option<String>,
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
                if let Some(ref alt) = info.alt_id {
                    msg.push_str(&format!(" New version: @{alt}"));
                }
                let data = DiagnosticData {
                    kind: "archived".into(),
                    old_id: r.id.clone(),
                    new_id: info.alt_id.clone(),
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
                // Suppression: if next @token on same line matches evo_id, skip
                let should_warn = if let Some(ref evo) = info.evo_id {
                    let after = &line[r.end_char as usize..];
                    let next_ref = after.trim_start().strip_prefix('@').and_then(|s| {
                        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                        Some(&s[..end])
                    });
                    next_ref != Some(evo.as_str())
                } else {
                    true
                };

                if should_warn {
                    let mut msg = format!("Note @{} is legacy.", r.id);
                    if let Some(ref evo) = info.evo_id {
                        msg.push_str(&format!(" Newer insights: @{evo}"));
                    }
                    let data = DiagnosticData {
                        kind: "legacy".into(),
                        old_id: r.id.clone(),
                        new_id: info.evo_id.clone(),
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

/// Validate TOML metadata block fields and produce diagnostics.
pub fn get_schema_diagnostics(content: &str, index: &Arc<NoteIndex>) -> Vec<Diagnostic> {
    let Some(block) = parser::find_toml_metadata_block(content) else {
        return Vec::new();
    };

    // Try to parse as TOML; if invalid, return a single parse-error diagnostic
    if let Err(e) = block.toml_content.parse::<toml::Value>() {
        return vec![Diagnostic {
            range: Range {
                start: Position { line: block.start_line as u32, character: 0 },
                end: Position { line: block.end_line as u32, character: 0 },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: format!("TOML parse error: {e}"),
            ..Default::default()
        }];
    }

    let mut diagnostics = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let toml_line_count = block.toml_content.lines().count();
    let toml_start = block.end_line.saturating_sub(toml_line_count);

    // Per-line field validation
    for (i, toml_line) in block.toml_content.lines().enumerate() {
        let file_line = toml_start + i;
        let file_line_text = lines.get(file_line).copied().unwrap_or("");
        let trimmed = toml_line.trim_start();

        let line_range = Range {
            start: Position { line: file_line as u32, character: 0 },
            end: Position { line: file_line as u32, character: file_line_text.len() as u32 },
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
        } else if trimmed.starts_with("relation")
            && !trimmed.starts_with("relation-target")
        {
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

    // Semantic checks using parsed values
    if let Some(parsed) = parser::parse_toml_metadata(&block.toml_content) {
        use crate::parser::Relation;

        // relation != "active" but relation-target is empty → WARNING on relation line
        if parsed.relation != Relation::Active && parsed.relation_target.is_empty() {
            if let Some((i, _)) = block
                .toml_content
                .lines()
                .enumerate()
                .find(|(_, l)| {
                    let t = l.trim_start();
                    t.starts_with("relation") && !t.starts_with("relation-target")
                })
            {
                let file_line = toml_start + i;
                let file_line_text = lines.get(file_line).copied().unwrap_or("");
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position { line: file_line as u32, character: 0 },
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
                            start: Position { line: file_line as u32, character: 0 },
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

/// Generate a HINT diagnostic for an orphan note (no inbound references).
///
/// Returns `None` if:
/// - The note is not in the index (not a known note file)
/// - The note has at least one backlink
pub fn get_orphan_diagnostic(
    content: &str,
    uri_path: &str,
    index: &Arc<NoteIndex>,
) -> Option<Diagnostic> {
    let note_id = uri_path.rsplit('/').next().and_then(|s| s.strip_suffix(".typ"))?;

    // Only flag notes that are in the index
    if index.get(note_id).is_none() {
        return None;
    }

    // Not an orphan if it has backlinks
    if !index.get_backlinks(note_id).is_empty() {
        return None;
    }

    // Find the title line (contains `<{note_id}>`)
    let needle = format!("<{note_id}>");
    let (line_num, _line_text) = content.lines().enumerate().find(|(_, l)| l.contains(&needle))?;

    Some(Diagnostic {
        range: Range {
            start: Position { line: line_num as u32, character: 0 },
            end: Position { line: line_num as u32, character: 0 },
        },
        severity: Some(DiagnosticSeverity::HINT),
        source: Some("zk-lsp".into()),
        message: format!("Orphan note: no other notes reference @{note_id}"),
        ..Default::default()
    })
}

/// Generate LSP diagnostics for `@ID` occurrences that participate in cycles.
///
/// Filters `cycles` to only occurrences whose `file_path` matches `file_path`.
/// Uses `byte_to_utf16` for correct LSP `character` positions (not raw byte offsets).
pub fn get_cycle_diagnostics(
    content: &str,
    file_path: &std::path::Path,
    cycles: &[DependencyCycle],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for cycle in cycles {
        for occ in &cycle.edges {
            if occ.file_path != file_path {
                continue;
            }
            let line_text = content.lines().nth(occ.line).unwrap_or("");
            diagnostics.push(Diagnostic {
                range: Range {
                    start: Position {
                        line: occ.line as u32,
                        character: parser::byte_to_utf16(line_text, occ.byte_start as usize),
                    },
                    end: Position {
                        line: occ.line as u32,
                        character: parser::byte_to_utf16(line_text, occ.byte_end as usize),
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: format!(
                    "Cyclic task dependency: {} → … → {}",
                    occ.from_note_id, occ.from_note_id
                ),
                ..Default::default()
            });
        }
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::cycle::DependencyCycle;
    use crate::dependency_graph::CycleEdgeOccurrence;
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

    fn add_backlink(index: &Arc<NoteIndex>, target_id: &str, from_id: &str) {
        index.backlinks.entry(target_id.to_string()).or_default().push(BacklinkLocation {
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
    fn test_orphan_note_produces_hint() {
        let index = make_index();
        insert_note(&index, "1111111111");
        // No backlinks → orphan
        let content = "= My Note <1111111111>\n";
        let diag = get_orphan_diagnostic(content, "/wiki/note/1111111111.typ", &index);
        assert!(diag.is_some());
        let d = diag.unwrap();
        assert_eq!(d.severity, Some(DiagnosticSeverity::HINT));
        assert!(d.message.contains("Orphan note"));
    }

    #[test]
    fn test_non_orphan_no_hint() {
        let index = make_index();
        insert_note(&index, "1111111111");
        add_backlink(&index, "1111111111", "2222222222");
        let content = "= My Note <1111111111>\n";
        let diag = get_orphan_diagnostic(content, "/wiki/note/1111111111.typ", &index);
        assert!(diag.is_none());
    }

    #[test]
    fn test_cycle_diagnostic_range() {
        // Line with CJK to verify byte_to_utf16 is used, not raw bytes
        // "- [ ] 你好 @1111111111"
        // indent=0, prefix_len=6
        // "你好 " = 3+3+1 = 7 bytes, but 3 UTF-16 units
        // "@1111111111" starts at byte 6+7=13, ends at 6+7+11=24
        // UTF-16 start = 6 + 2 (你=1, 好=1) + 1 (space) = 9... let me compute:
        // "- [ ] " = 6 bytes/chars (all ASCII)
        // "你" = 3 bytes, 1 UTF-16 unit
        // "好" = 3 bytes, 1 UTF-16 unit
        // " " = 1 byte, 1 UTF-16 unit
        // "@1111111111" starts at byte 13, UTF-16 char 9
        let line_text = "- [ ] 你好 @1111111111";
        let content = format!("{line_text}\n");
        // byte offsets: '@' at byte 13, end at byte 24
        let byte_start = line_text.find('@').unwrap() as u32;
        let byte_end = byte_start + 11;

        let occ = CycleEdgeOccurrence {
            from_note_id: "1111111111".to_string(),
            to_note_id: "2222222222".to_string(),
            file_path: PathBuf::from("/wiki/note/1111111111.typ"),
            line: 0,
            byte_start,
            byte_end,
            line_text: line_text.to_string(),
        };
        let cycle = DependencyCycle { nodes: vec!["1111111111".into()], edges: vec![occ] };

        let diags = get_cycle_diagnostics(
            &content,
            std::path::Path::new("/wiki/note/1111111111.typ"),
            &[cycle],
        );
        assert_eq!(diags.len(), 1);
        let range = diags[0].range;
        // UTF-16 start: "- [ ] " (6) + "你好 " (3 units) = 9
        assert_eq!(range.start.character, 9, "start must be UTF-16 offset, not byte offset");
        // UTF-16 end: 9 + 11 = 20
        assert_eq!(range.end.character, 20);
    }
}

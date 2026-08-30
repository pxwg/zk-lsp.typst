use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::*;

use crate::config::{MetadataFieldConfig, MetadataFieldKind};
use crate::index::NoteIndex;
use crate::metadata;
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

/// Validate central metadata binding and title structure in a note file.
#[allow(dead_code)]
pub fn get_schema_diagnostics(content: &str, _index: &Arc<NoteIndex>) -> Vec<Diagnostic> {
    get_schema_diagnostics_for_note(content, None)
}

pub fn get_schema_diagnostics_for_note(
    content: &str,
    expected_id: Option<&str>,
) -> Vec<Diagnostic> {
    let lines: Vec<&str> = content.lines().collect();
    let title_line_idx = lines
        .iter()
        .enumerate()
        .find_map(|(idx, line)| parser::RE_TITLE.is_match(line).then_some(idx));
    let Some(title_line_idx) = title_line_idx else {
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
            message: "Missing note title heading (`= Title <ID>`)".to_string(),
            ..Default::default()
        }];
    };

    let binding_assignment_count = content
        .lines()
        .filter(|line| line.trim_start().starts_with("#let zk-metadata"))
        .count();
    let binding = parser::find_metadata_binding(content);
    let Some(binding) = binding else {
        if binding_assignment_count > 0 {
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
                message: "Invalid zk-metadata binding".to_string(),
                ..Default::default()
            }];
        }
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
            message: "Missing zk-metadata binding".to_string(),
            ..Default::default()
        }];
    };
    if binding_assignment_count > 1 {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: binding.line_idx as u32,
                    character: 0,
                },
                end: Position {
                    line: binding.line_idx as u32,
                    character: lines.get(binding.line_idx).map(|l| l.len()).unwrap_or(0) as u32,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Invalid zk-metadata binding".to_string(),
            ..Default::default()
        }];
    }

    let title_id = parser::RE_TITLE
        .captures(lines[title_line_idx])
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str())
        .unwrap_or("");
    if let Some(expected_id) = expected_id {
        if title_id != expected_id {
            return vec![Diagnostic {
                range: Range {
                    start: Position {
                        line: title_line_idx as u32,
                        character: 0,
                    },
                    end: Position {
                        line: title_line_idx as u32,
                        character: lines.get(title_line_idx).map(|l| l.len()).unwrap_or(0) as u32,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: "Title ID does not match current note".to_string(),
                ..Default::default()
            }];
        }
    }

    if binding.line_idx >= title_line_idx {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: binding.line_idx as u32,
                    character: 0,
                },
                end: Position {
                    line: binding.line_idx as u32,
                    character: lines.get(binding.line_idx).map(|l| l.len()).unwrap_or(0) as u32,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Invalid zk-metadata binding".to_string(),
            ..Default::default()
        }];
    }
    if binding.id != title_id {
        return vec![Diagnostic {
            range: Range {
                start: Position {
                    line: binding.line_idx as u32,
                    character: 0,
                },
                end: Position {
                    line: binding.line_idx as u32,
                    character: lines.get(binding.line_idx).map(|l| l.len()).unwrap_or(0) as u32,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "zk-metadata binding ID does not match current note".to_string(),
            ..Default::default()
        }];
    }

    let diagnostics = Vec::new();
    diagnostics
}

#[allow(dead_code)]
pub fn get_metadata_index_diagnostics(content: &str) -> Vec<Diagnostic> {
    get_metadata_index_diagnostics_with_context(content, &[], None)
}

pub fn get_metadata_index_diagnostics_with_context(
    content: &str,
    metadata_fields: &[MetadataFieldConfig],
    note_dir: Option<&Path>,
) -> Vec<Diagnostic> {
    let parsed = match content.parse::<toml::Table>() {
        Ok(table) => table,
        Err(err) => {
            let range = err
                .span()
                .map(|span| byte_span_to_lsp_range(content, span.start, span.end))
                .unwrap_or_else(|| Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: content.lines().next().map(|l| l.len()).unwrap_or(0) as u32,
                    },
                });
            return vec![Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: format!("TOML parse error: {err}"),
                ..Default::default()
            }];
        }
    };

    let mut diagnostics = Vec::new();
    match parsed.get("format-version").and_then(|v| v.as_integer()) {
        None => diagnostics.push(Diagnostic {
            range: field_or_default_range(content, None, "format-version"),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Missing format-version".into(),
            ..Default::default()
        }),
        Some(version) if version != metadata::FORMAT_VERSION => diagnostics.push(Diagnostic {
            range: field_or_default_range(content, None, "format-version"),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Unsupported metadata format-version".into(),
            ..Default::default()
        }),
        Some(_) => {}
    }
    let Some(notes) = parsed.get("notes").and_then(|v| v.as_table()) else {
        diagnostics.push(Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Missing notes table".into(),
            ..Default::default()
        });
        return diagnostics;
    };

    for (id, value) in notes {
        if !metadata::is_note_id(id) {
            diagnostics.push(Diagnostic {
                range: loose_record_key_range(content, id).unwrap_or_default(),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: "Invalid note ID".into(),
                ..Default::default()
            });
            continue;
        }
        if note_dir
            .map(|dir| !dir.join(format!("{id}.typ")).exists())
            .unwrap_or(false)
        {
            diagnostics.push(Diagnostic {
                range: record_or_default_range(content, id),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: "Extra metadata record".into(),
                ..Default::default()
            });
        }
        let Some(record) = value.as_table() else {
            diagnostics.push(Diagnostic {
                range: record_or_default_range(content, id),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: "Invalid metadata for current note".into(),
                ..Default::default()
            });
            continue;
        };
        validate_metadata_record(content, id, record, metadata_fields, &mut diagnostics);
    }

    diagnostics
}

fn validate_metadata_record(
    content: &str,
    id: &str,
    record: &toml::Table,
    metadata_fields: &[MetadataFieldConfig],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in metadata::CORE_FIELDS {
        if !record.contains_key(*field) {
            diagnostics.push(Diagnostic {
                range: record_or_default_range(content, id),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: format!("Missing metadata field: {field}"),
                ..Default::default()
            });
        }
    }

    if record
        .get("schema-version")
        .and_then(|v| v.as_integer())
        .is_some_and(|version| version != metadata::SCHEMA_VERSION)
    {
        diagnostics.push(Diagnostic {
            range: field_or_default_range(content, Some(id), "schema-version"),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("zk-lsp".into()),
            message: "Unsupported metadata schema-version".into(),
            ..Default::default()
        });
    }

    validate_array_string(content, id, record, "aliases", diagnostics);
    validate_array_string(content, id, record, "keywords", diagnostics);
    validate_array_string(content, id, record, "relation-target", diagnostics);
    validate_string(content, id, record, "abstract", diagnostics);
    validate_string_enum(
        content,
        id,
        record,
        "checklist-status",
        &["none", "todo", "wip", "done"],
        diagnostics,
    );
    validate_string_enum(
        content,
        id,
        record,
        "relation",
        &["active", "archived", "legacy"],
        diagnostics,
    );
    if record
        .get("generated")
        .is_some_and(|value| value.as_bool().is_none())
    {
        invalid_field(content, id, "generated", diagnostics);
    }
    validate_custom_fields(content, id, record, metadata_fields, diagnostics);
}

fn validate_array_string(
    content: &str,
    id: &str,
    record: &toml::Table,
    field: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if record.get(field).is_some_and(|value| {
        !value
            .as_array()
            .map(|items| items.iter().all(|item| item.as_str().is_some()))
            .unwrap_or(false)
    }) {
        invalid_field(content, id, field, diagnostics);
    }
}

fn validate_string(
    content: &str,
    id: &str,
    record: &toml::Table,
    field: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if record
        .get(field)
        .is_some_and(|value| value.as_str().is_none())
    {
        invalid_field(content, id, field, diagnostics);
    }
}

fn validate_string_enum(
    content: &str,
    id: &str,
    record: &toml::Table,
    field: &str,
    values: &[&str],
    diagnostics: &mut Vec<Diagnostic>,
) {
    if record
        .get(field)
        .is_some_and(|value| value.as_str().map(|s| !values.contains(&s)).unwrap_or(true))
    {
        invalid_field(content, id, field, diagnostics);
    }
}

fn invalid_field(content: &str, id: &str, field: &str, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic {
        range: field_or_default_range(content, Some(id), field),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("zk-lsp".into()),
        message: "Invalid metadata for current note".into(),
        ..Default::default()
    });
}

fn validate_custom_fields(
    content: &str,
    id: &str,
    record: &toml::Table,
    metadata_fields: &[MetadataFieldConfig],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let user = record.get("user");
    let user_table = user.and_then(|value| value.as_table());
    if user.is_some() && user_table.is_none() {
        invalid_field(content, id, "user", diagnostics);
        return;
    }

    for field in metadata_fields {
        let Some(user_key) = field.path.strip_prefix("user.") else {
            continue;
        };
        let Some(value) = user_table.and_then(|table| table.get(user_key)) else {
            diagnostics.push(Diagnostic {
                range: record_or_default_range(content, id),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("zk-lsp".into()),
                message: format!("Missing metadata field: {}", field.path),
                ..Default::default()
            });
            continue;
        };
        if !value_matches_kind(value, &field.kind) {
            invalid_field(content, id, &field.path, diagnostics);
        }
    }
}

pub fn note_metadata_record_message(
    record: &toml::Table,
    metadata_fields: &[MetadataFieldConfig],
) -> Option<&'static str> {
    if metadata_record_is_incomplete(record, metadata_fields) {
        return Some("Metadata record is incomplete");
    }
    if metadata_record_is_invalid(record, metadata_fields) {
        return Some("Invalid metadata for current note");
    }
    None
}

fn metadata_record_is_incomplete(
    record: &toml::Table,
    metadata_fields: &[MetadataFieldConfig],
) -> bool {
    if metadata::CORE_FIELDS
        .iter()
        .any(|field| !record.contains_key(*field))
    {
        return true;
    }
    for field in metadata_fields {
        let Some(user_key) = field.path.strip_prefix("user.") else {
            continue;
        };
        let Some(user_table) = record.get("user").and_then(|value| value.as_table()) else {
            return true;
        };
        if !user_table.contains_key(user_key) {
            return true;
        }
    }
    false
}

fn metadata_record_is_invalid(
    record: &toml::Table,
    metadata_fields: &[MetadataFieldConfig],
) -> bool {
    if record
        .get("schema-version")
        .and_then(|v| v.as_integer())
        .is_some_and(|version| version != metadata::SCHEMA_VERSION)
    {
        return true;
    }
    if field_is_invalid_array_string(record, "aliases")
        || field_is_invalid_array_string(record, "keywords")
        || field_is_invalid_array_string(record, "relation-target")
        || field_is_invalid_string(record, "abstract")
        || field_is_invalid_enum(record, "checklist-status", &["none", "todo", "wip", "done"])
        || field_is_invalid_enum(record, "relation", &["active", "archived", "legacy"])
        || record
            .get("generated")
            .is_some_and(|value| value.as_bool().is_none())
    {
        return true;
    }
    let Some(user_table) = record.get("user").and_then(|value| value.as_table()) else {
        return record.get("user").is_some();
    };
    for field in metadata_fields {
        let Some(user_key) = field.path.strip_prefix("user.") else {
            continue;
        };
        if let Some(value) = user_table.get(user_key) {
            if !value_matches_kind(value, &field.kind) {
                return true;
            }
        }
    }
    false
}

fn field_is_invalid_array_string(record: &toml::Table, field: &str) -> bool {
    record.get(field).is_some_and(|value| {
        !value
            .as_array()
            .map(|items| items.iter().all(|item| item.as_str().is_some()))
            .unwrap_or(false)
    })
}

fn field_is_invalid_string(record: &toml::Table, field: &str) -> bool {
    record
        .get(field)
        .is_some_and(|value| value.as_str().is_none())
}

fn field_is_invalid_enum(record: &toml::Table, field: &str, values: &[&str]) -> bool {
    record
        .get(field)
        .is_some_and(|value| value.as_str().map(|s| !values.contains(&s)).unwrap_or(true))
}

fn value_matches_kind(value: &toml::Value, kind: &MetadataFieldKind) -> bool {
    match kind {
        MetadataFieldKind::String => value.as_str().is_some(),
        MetadataFieldKind::Boolean => value.as_bool().is_some(),
        MetadataFieldKind::ArrayString => value
            .as_array()
            .map(|items| items.iter().all(|item| item.as_str().is_some()))
            .unwrap_or(false),
    }
}

fn field_or_default_range(content: &str, id: Option<&str>, field: &str) -> Range {
    let line_range = id
        .and_then(|id| metadata::find_field_line_range(content, id, field))
        .or_else(|| find_top_level_field_range(content, field));
    line_range.map(lsp_range).unwrap_or_default()
}

fn record_or_default_range(content: &str, id: &str) -> Range {
    metadata::find_record_key_range(content, id)
        .map(lsp_range)
        .unwrap_or_default()
}

fn find_top_level_field_range(content: &str, field: &str) -> Option<metadata::MetadataTextRange> {
    for (line_idx, line) in content.lines().enumerate() {
        if line.trim_start().starts_with('[') {
            return None;
        }
        let trimmed = line.trim_start();
        let Some((candidate, _)) = trimmed.split_once('=') else {
            continue;
        };
        if candidate.trim() == field {
            return Some(metadata::MetadataTextRange {
                line: line_idx,
                start_col: 0,
                end_col: line.len(),
            });
        }
    }
    None
}

fn loose_record_key_range(content: &str, id: &str) -> Option<Range> {
    for (line_idx, line) in content.lines().enumerate() {
        for prefix in ["[notes.\"", "[notes.'", "[notes."] {
            let needle = format!("{prefix}{id}");
            let Some(start) = line.find(&needle) else {
                continue;
            };
            let id_start = start + prefix.len();
            return Some(Range {
                start: Position {
                    line: line_idx as u32,
                    character: id_start as u32,
                },
                end: Position {
                    line: line_idx as u32,
                    character: (id_start + id.len()) as u32,
                },
            });
        }
    }
    None
}

fn lsp_range(range: metadata::MetadataTextRange) -> Range {
    Range {
        start: Position {
            line: range.line as u32,
            character: range.start_col as u32,
        },
        end: Position {
            line: range.line as u32,
            character: range.end_col as u32,
        },
    }
}

fn byte_span_to_lsp_range(content: &str, start: usize, end: usize) -> Range {
    Range {
        start: byte_offset_to_position(content, start),
        end: byte_offset_to_position(content, end),
    }
}

fn byte_offset_to_position(content: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (idx, ch) in content.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let line_text = &content[line_start..offset.min(content.len())];
    Position {
        line,
        character: line_text.chars().map(|ch| ch.len_utf16() as u32).sum(),
    }
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
            related_information: if diag.related_locations.is_empty() {
                None
            } else {
                Some(
                    diag.related_locations
                        .iter()
                        .map(|related| {
                            let related_line = if related.file_path == file_path {
                                content.lines().nth(related.line).unwrap_or("").to_string()
                            } else {
                                std::fs::read_to_string(&related.file_path)
                                    .ok()
                                    .and_then(|content| {
                                        content.lines().nth(related.line).map(str::to_string)
                                    })
                                    .unwrap_or_default()
                            };
                            DiagnosticRelatedInformation {
                                location: Location {
                                    uri: Url::from_file_path(&related.file_path)
                                        .expect("valid file path"),
                                    range: Range {
                                        start: Position {
                                            line: related.line as u32,
                                            character: parser::byte_to_utf16(
                                                &related_line,
                                                related.byte_start as usize,
                                            ),
                                        },
                                        end: Position {
                                            line: related.line as u32,
                                            character: parser::byte_to_utf16(
                                                &related_line,
                                                related.byte_end as usize,
                                            ),
                                        },
                                    },
                                },
                                message: "other dependency edge in the same cycle".to_string(),
                            }
                        })
                        .collect(),
                )
            },
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
    use std::sync::Arc;
    use std::{fs, path::PathBuf};

    fn make_index() -> Arc<NoteIndex> {
        let config = Arc::new(tokio::sync::RwLock::new(WikiConfig::from_root(
            PathBuf::from("/tmp/wiki"),
        )));
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
    fn archived_diagnostic_keeps_utf16_reference_range() {
        let index = make_index();
        insert_note(&index, "1111111111");
        index.notes.get_mut("1111111111").unwrap().archived = true;

        let diags = get_diagnostics("中文😀 @1111111111\n", &index, "/wiki/note/9999999999.typ");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diags[0].range,
            Range::new(Position::new(0, 5), Position::new(0, 16))
        );
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
        assert_eq!(diags[0].message, "Missing zk-metadata binding");
    }

    #[test]
    fn test_missing_title_heading_produces_error() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = zk_metadata(\"2603110000\")\n",
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
    fn test_metadata_index_missing_fields_produce_diagnostics() {
        let content = concat!(
            "format-version = 1\n\n",
            "[notes.\"2603110000\"]\n",
            "schema-version = 1\n",
            "relation = \"active\"\n",
        );
        let diags = get_metadata_index_diagnostics(content);
        assert!(diags
            .iter()
            .any(|d| d.message == "Missing metadata field: aliases"));
        assert!(diags
            .iter()
            .any(|d| d.message == "Missing metadata field: checklist-status"));
        assert!(diags
            .iter()
            .filter(|d| d.message.starts_with("Missing metadata field"))
            .all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
    }

    #[test]
    fn test_schema_does_not_flag_existing_relation_target() {
        let index = make_index();
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = zk_metadata(\"2603110000\")\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics(content, &index);
        assert!(diags.is_empty());
    }

    #[test]
    fn schema_diagnostics_report_path_title_and_binding_mismatch_separately() {
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = zk_metadata(\"2603110000\")\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics_for_note(content, Some("2603119999"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "Title ID does not match current note");

        let binding_mismatch = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = zk_metadata(\"2603119999\")\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics_for_note(binding_mismatch, Some("2603110000"));
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "zk-metadata binding ID does not match current note"
        );
    }

    #[test]
    fn schema_diagnostics_report_wrong_binding_form_as_invalid() {
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = zk_metadata(2603110000)\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Note <2603110000>\n",
        );
        let diags = get_schema_diagnostics_for_note(content, Some("2603110000"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "Invalid zk-metadata binding");
    }

    #[test]
    fn metadata_index_reports_extra_records_and_custom_field_errors() {
        let root = std::env::temp_dir().join(format!("zk-lsp-diagnostics-{}", std::process::id()));
        let note_dir = root.join("note");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&note_dir).unwrap();
        fs::write(note_dir.join("2603110000.typ"), "").unwrap();

        let fields = vec![
            MetadataFieldConfig {
                path: "user.project".into(),
                kind: MetadataFieldKind::String,
                default: toml::Value::String(String::new()),
            },
            MetadataFieldConfig {
                path: "user.reviewed".into(),
                kind: MetadataFieldKind::Boolean,
                default: toml::Value::Boolean(false),
            },
        ];
        let content = concat!(
            "format-version = 1\n\n",
            "[notes.\"2603110000\"]\n",
            "schema-version = 1\n",
            "aliases = []\n",
            "abstract = \"\"\n",
            "keywords = []\n",
            "generated = true\n",
            "checklist-status = \"none\"\n",
            "relation = \"active\"\n",
            "relation-target = []\n\n",
            "[notes.\"2603110000\".user]\n",
            "project = \"zk\"\n",
            "reviewed = \"no\"\n\n",
            "[notes.\"2603110001\"]\n",
            "schema-version = 1\n",
            "aliases = []\n",
            "abstract = \"\"\n",
            "keywords = []\n",
            "generated = true\n",
            "checklist-status = \"none\"\n",
            "relation = \"active\"\n",
            "relation-target = []\n",
        );

        let diags = get_metadata_index_diagnostics_with_context(content, &fields, Some(&note_dir));
        assert!(diags.iter().any(|d| d.message == "Extra metadata record"));
        assert!(diags
            .iter()
            .any(|d| d.message == "Invalid metadata for current note"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn note_metadata_record_message_is_tiered() {
        let fields = vec![MetadataFieldConfig {
            path: "user.project".into(),
            kind: MetadataFieldKind::String,
            default: toml::Value::String(String::new()),
        }];
        let incomplete = toml::toml! {
            schema-version = 1
            relation = "active"
        };
        assert_eq!(
            note_metadata_record_message(&incomplete, &fields),
            Some("Metadata record is incomplete")
        );

        let invalid = toml::toml! {
            schema-version = 1
            aliases = []
            abstract = ""
            keywords = []
            generated = true
            checklist-status = "blocked"
            relation = "active"
            relation-target = []

            [user]
            project = ""
        };
        assert_eq!(
            note_metadata_record_message(&invalid, &fields),
            Some("Invalid metadata for current note")
        );
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

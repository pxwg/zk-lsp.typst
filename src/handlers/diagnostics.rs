use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticData {
    pub kind: String, // "archived" | "legacy"
    pub old_id: String,
    pub new_id: Option<String>,
}

/// Generate diagnostics for all @ID references in the document content.
pub fn get_diagnostics(content: &str, index: &Arc<NoteIndex>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let refs = parser::find_all_refs(line);
        for r in refs {
            let Some(info) = index.get(&r.id) else { continue };

            let range = Range {
                start: Position {
                    line: line_num as u32,
                    character: r.start_char,
                },
                end: Position {
                    line: line_num as u32,
                    character: r.end_char,
                },
            };

            if info.archived {
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
                // Suppression: if next @token on same line matches evo_id, skip
                let should_warn = if let Some(ref evo) = info.evo_id {
                    let after = &line[r.end_char as usize..];
                    let next_ref = after
                        .trim_start()
                        .strip_prefix('@')
                        .and_then(|s| {
                            let end = s
                                .find(|c: char| !c.is_ascii_digit())
                                .unwrap_or(s.len());
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

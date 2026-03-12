use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

/// Produce inlay hints for all @ID references in the given line range.
pub fn get_inlay_hints(content: &str, range: Range, index: &Arc<NoteIndex>) -> Vec<InlayHint> {
    let start_line = range.start.line as usize;
    let end_line = range.end.line as usize;
    let lines: Vec<&str> = content.lines().collect();

    let mut hints = Vec::new();
    for r in parser::find_all_refs_filtered(content) {
        let ln = r.line as usize;
        if ln < start_line || ln > end_line {
            continue;
        }
        if let Some(info) = index.get(&r.id) {
            let line = lines[ln];
            hints.push(InlayHint {
                position: Position {
                    line: r.line,
                    character: parser::byte_to_utf16(line, r.end_char as usize),
                },
                label: InlayHintLabel::String(info.title.clone()),
                kind: Some(InlayHintKind::TYPE),
                padding_left: Some(true),
                padding_right: None,
                text_edits: None,
                tooltip: None,
                data: None,
            });
        }
    }
    hints
}

use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

/// Produce inlay hints for all @ID references in the given line range.
pub fn get_inlay_hints(
    content: &str,
    range: Range,
    index: &Arc<NoteIndex>,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();

    let start_line = range.start.line as usize;
    let end_line = range.end.line as usize;

    for (line_num, line) in content.lines().enumerate() {
        if line_num < start_line || line_num > end_line {
            continue;
        }
        let refs = parser::find_all_refs(line);
        for r in refs {
            if let Some(info) = index.get(&r.id) {
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
    }
    hints
}

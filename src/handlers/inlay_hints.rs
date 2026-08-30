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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::index::NoteInfo;
    use std::path::PathBuf;

    #[test]
    fn inlay_hint_keeps_title_and_utf16_position() {
        let index = Arc::new(NoteIndex::new(Arc::new(tokio::sync::RwLock::new(
            WikiConfig::from_root(PathBuf::from("/tmp/wiki")),
        ))));
        index.notes.insert(
            "2603110001".to_string(),
            NoteInfo {
                id: "2603110001".to_string(),
                title: "Target title".to_string(),
                archived: true,
                legacy: false,
                alt_id: None,
                evo_id: None,
                relation_target: vec![],
                aliases: vec![],
                keywords: vec![],
                abstract_text: None,
                checklist_status: None,
                path: PathBuf::from("/tmp/wiki/note/2603110001.typ"),
            },
        );

        let hints = get_inlay_hints(
            "中文😀 @2603110001",
            Range::new(Position::new(0, 0), Position::new(0, u32::MAX)),
            &index,
        );
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].position, Position::new(0, 16));
        let InlayHintLabel::String(label) = &hints[0].label else {
            panic!("expected string inlay label")
        };
        assert_eq!(label, "Target title");
    }
}

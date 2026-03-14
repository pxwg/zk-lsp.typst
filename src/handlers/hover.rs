use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

/// Return hover content when the cursor is over a quoted note ID inside a
/// `relation-target = [...]` value within the TOML metadata block.
///
/// The hover body is the full file content of the referenced note, rendered as
/// a fenced Typst code block so editors can apply syntax highlighting.
pub fn get_hover(content: &str, position: Position, index: &Arc<NoteIndex>) -> Option<Hover> {
    get_hover_with_loader(content, position, index, |path| {
        std::fs::read_to_string(path).ok()
    })
}

fn get_hover_with_loader<F>(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
    load_note: F,
) -> Option<Hover>
where
    F: Fn(&std::path::Path) -> Option<String>,
{
    let block = parser::find_toml_metadata_block(content)?;

    let line_num = position.line as usize;
    if line_num < block.start_line || line_num > block.end_line {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let current_line = lines.get(line_num).copied()?;
    let trimmed = current_line.trim_start();

    if !trimmed.starts_with("relation-target") || !trimmed.contains('[') {
        return None;
    }

    // Find all quoted 10-digit IDs on this line and check whether the cursor
    // column falls within one of them (quotes inclusive for a generous range).
    let col = position.character as usize;
    let id = find_id_at_col(current_line, col)?;

    let info = index.notes.get(&id)?;
    let note_content = load_note(&info.path)?;
    let preview_content = extract_preview_body(&note_content);

    let markdown = format!(
        "**{}** `{}`\n\n```typst\n{}\n```",
        info.title,
        info.id,
        preview_content.trim_end()
    );

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    })
}

/// Scan `line` for `"XXXXXXXXXX"` patterns (quoted 10-digit ASCII IDs) and
/// return the ID whose quoted span contains byte column `col`.
fn find_id_at_col(line: &str, col: usize) -> Option<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' {
            // Look for a closing quote up to 10 digits away.
            let start = i + 1;
            let end = (start + 10).min(len);
            if end < len && bytes[end] == b'"' {
                let candidate = &line[start..end];
                if candidate.len() == 10 && candidate.bytes().all(|b| b.is_ascii_digit()) {
                    // Span is [i, end] inclusive (the two quote chars).
                    if col >= i && col <= end {
                        return Some(candidate.to_string());
                    }
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    None
}

fn extract_preview_body(content: &str) -> String {
    let Some(header) = parser::parse_header(content) else {
        return content.to_string();
    };
    let lines: Vec<&str> = content.lines().collect();
    lines[header.title_line_idx..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::index::NoteInfo;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn make_index(id: &str, title: &str, path: PathBuf) -> Arc<NoteIndex> {
        let idx = NoteIndex::new(Arc::new(WikiConfig::from_root(PathBuf::from("/tmp"))));
        idx.notes.insert(
            id.to_string(),
            NoteInfo {
                id: id.to_string(),
                title: title.to_string(),
                archived: false,
                legacy: false,
                alt_id: None,
                evo_id: None,
                relation_target: vec![],
                aliases: vec![],
                keywords: vec![],
                abstract_text: None,
                checklist_status: None,
                path,
            },
        );
        Arc::new(idx)
    }

    const NOTE_CONTENT: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  relation = \"archived\"\n",
        "  relation-target = [\"2603110001\"]\n", // line 5, col 20 → inside "2603110001"
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Host <2603110000>\n",
    );

    const TARGET_NOTE_CONTENT: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  relation = \"active\"\n",
        "  relation-target = []\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Target <2603110001>\n",
        "正文第一行\n",
    );

    #[test]
    fn test_hover_on_id_returns_content() {
        let path = PathBuf::from("/virtual/2603110001.typ");
        let index = make_index("2603110001", "Target Note", path);
        // Line 5: `  relation-target = ["2603110001"]`
        //                               ^col 22 (inside the ID)
        let pos = Position {
            line: 5,
            character: 22,
        };
        let hover = get_hover_with_loader(NOTE_CONTENT, pos, &index, |path| {
            if path == PathBuf::from("/virtual/2603110001.typ").as_path() {
                Some(TARGET_NOTE_CONTENT.to_string())
            } else {
                None
            }
        });
        assert!(hover.is_some());
        let HoverContents::Markup(mc) = hover.unwrap().contents else {
            panic!()
        };
        assert!(mc.value.contains("2603110001"));
        assert!(mc.value.contains("Target Note"));
        assert!(mc.value.contains("= Target <2603110001>"));
        assert!(mc.value.contains("正文第一行"));
        assert!(!mc.value.contains("#let zk-metadata"));
        assert!(!mc
            .value
            .contains("#show: zettel.with(metadata: zk-metadata)"));
    }

    #[test]
    fn test_hover_outside_id_returns_none() {
        let index = make_index("2603110001", "Target Note", PathBuf::from("/tmp/x.typ"));
        // col 5 is on `relation-target` text, not on the ID
        let pos = Position {
            line: 5,
            character: 5,
        };
        assert!(get_hover(NOTE_CONTENT, pos, &index).is_none());
    }

    #[test]
    fn test_find_id_at_col() {
        let line = "  relation-target = [\"2603110001\"]";
        // col 22 is inside "2603110001" (quotes at 21 and 32)
        assert_eq!(find_id_at_col(line, 22), Some("2603110001".into()));
        // col 21 is on the opening quote
        assert_eq!(find_id_at_col(line, 21), Some("2603110001".into()));
        // col 10 is before the bracket
        assert_eq!(find_id_at_col(line, 10), None);
    }

    #[test]
    fn test_extract_preview_body_falls_back_for_non_toml_note() {
        let content = "= Legacy <2603110002>\nBody\n";
        assert_eq!(extract_preview_body(content), content);
    }
}

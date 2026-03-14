use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

/// Jump from a quoted note ID inside `relation-target = [...]` to the target
/// note's title line.
pub fn get_definition(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
) -> Option<Location> {
    get_definition_with_loader(content, position, index, |path| {
        std::fs::read_to_string(path).ok()
    })
}

fn get_definition_with_loader<F>(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
    load_note: F,
) -> Option<Location>
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

    let id = find_id_at_col(current_line, position.character as usize)?;
    let info = index.notes.get(&id)?;
    let note_content = load_note(&info.path)?;
    let title_line = parser::parse_header(&note_content)?.title_line_idx as u32;

    Some(Location {
        uri: Url::from_file_path(&info.path).ok()?,
        range: Range {
            start: Position {
                line: title_line,
                character: 0,
            },
            end: Position {
                line: title_line,
                character: 0,
            },
        },
    })
}

fn find_id_at_col(line: &str, col: usize) -> Option<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' {
            let start = i + 1;
            let end = (start + 10).min(len);
            if end < len && bytes[end] == b'"' {
                let candidate = &line[start..end];
                if candidate.len() == 10 && candidate.bytes().all(|b| b.is_ascii_digit()) {
                    if col >= i && col <= end {
                        return Some(candidate.to_string());
                    }
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::index::NoteInfo;
    use std::path::PathBuf;

    fn make_index(id: &str, title: &str, path: PathBuf) -> Arc<NoteIndex> {
        let idx = NoteIndex::new(Arc::new(tokio::sync::RwLock::new(WikiConfig::from_root(
            PathBuf::from("/tmp"),
        ))));
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

    const HOST_NOTE_CONTENT: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  relation = \"archived\"\n",
        "  relation-target = [\"2603110001\"]\n",
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
    fn test_definition_on_relation_target_jumps_to_target_title() {
        let path = PathBuf::from("/virtual/2603110001.typ");
        let index = make_index("2603110001", "Target Note", path.clone());
        let pos = Position {
            line: 5,
            character: 22,
        };
        let loc = get_definition_with_loader(HOST_NOTE_CONTENT, pos, &index, |load_path| {
            if load_path == path.as_path() {
                Some(TARGET_NOTE_CONTENT.to_string())
            } else {
                None
            }
        })
        .expect("expected definition");

        assert_eq!(loc.uri, Url::from_file_path(path).unwrap());
        assert_eq!(loc.range.start.line, 10);
        assert_eq!(loc.range.start.character, 0);
        assert_eq!(loc.range.end, loc.range.start);
    }

    #[test]
    fn test_definition_outside_relation_target_returns_none() {
        let index = make_index("2603110001", "Target Note", PathBuf::from("/virtual/x.typ"));
        let pos = Position {
            line: 5,
            character: 5,
        };
        assert!(get_definition(HOST_NOTE_CONTENT, pos, &index).is_none());
    }
}

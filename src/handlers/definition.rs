use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::{metadata, parser};

pub fn get_note_definition(
    content: &str,
    position: Position,
    metadata_uri: &Url,
    metadata_content: &str,
) -> Option<Location> {
    let header = parser::parse_header(content)?;
    let id = parser::note_identity_at_position(content, position)?;
    if id != header.id {
        return None;
    }
    let range = metadata::find_record_key_range(metadata_content, &id)?;
    Some(Location {
        uri: metadata_uri.clone(),
        range: lsp_range(range),
    })
}

pub fn get_note_ref_definition(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
) -> Option<LocationLink> {
    get_note_ref_definition_with_loader(content, position, index, |path| {
        std::fs::read_to_string(path).ok()
    })
}

fn get_note_ref_definition_with_loader<F>(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
    load_note: F,
) -> Option<LocationLink>
where
    F: Fn(&std::path::Path) -> Option<String>,
{
    let note_ref = parser::note_ref_at_position(content, position)?;
    let info = index.get(&note_ref.id)?;
    let target_content = load_note(&info.path)?;
    let (target_range, target_selection_range) =
        title_heading_ranges(&target_content, &note_ref.id)?;

    Some(LocationLink {
        origin_selection_range: Some(note_ref.range),
        target_uri: Url::from_file_path(&info.path).ok()?,
        target_range,
        target_selection_range,
    })
}

pub fn get_metadata_definition(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
) -> Option<Location> {
    get_metadata_definition_with_loader(content, position, index, |path| {
        std::fs::read_to_string(path).ok()
    })
}

fn get_metadata_definition_with_loader<F>(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
    load_note: F,
) -> Option<Location>
where
    F: Fn(&std::path::Path) -> Option<String>,
{
    let id_pos =
        metadata::id_at_position(content, position.line as usize, position.character as usize)?;
    let target_id = id_pos.target_id;
    let info = index.notes.get(&target_id)?;
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

fn title_heading_ranges(content: &str, expected_id: &str) -> Option<(Range, Range)> {
    let header = parser::parse_header(content)?;
    if header.id != expected_id {
        return None;
    }

    let line = content.lines().nth(header.title_line_idx)?;
    let label = format!("<{expected_id}>");
    let label_start = line.rfind(&label)?;
    let line_idx = header.title_line_idx as u32;
    let target_range = Range {
        start: Position {
            line: line_idx,
            character: 0,
        },
        end: Position {
            line: line_idx,
            character: line.encode_utf16().count() as u32,
        },
    };
    let target_selection_range = Range {
        start: Position {
            line: line_idx,
            character: parser::byte_to_utf16(line, label_start),
        },
        end: Position {
            line: line_idx,
            character: parser::byte_to_utf16(line, label_start + label.len()),
        },
    };
    Some((target_range, target_selection_range))
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

    const NOTE_CONTENT: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = zk_metadata(\"2603110000\")\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Host <2603110000>\n",
    );

    const METADATA_CONTENT: &str = concat!(
        "format-version = 1\n\n",
        "[notes.\"2603110000\"]\n",
        "schema-version = 1\n",
        "relation-target = [\"2603110001\"]\n\n",
        "[notes.\"2603110001\"]\n",
        "schema-version = 1\n",
        "relation-target = []\n",
    );

    const TARGET_NOTE_CONTENT: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = zk_metadata(\"2603110001\")\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Target <2603110001>\n",
        "正文第一行\n",
    );

    #[test]
    fn note_title_id_jumps_to_metadata_record() {
        let uri = Url::parse("file:///wiki/metadata.toml").unwrap();
        let loc = get_note_definition(
            NOTE_CONTENT,
            Position {
                line: 4,
                character: 10,
            },
            &uri,
            METADATA_CONTENT,
        )
        .expect("expected definition");
        assert_eq!(loc.uri, uri);
        assert_eq!(loc.range.start.line, 2);
        assert_eq!(loc.range.start.character, 8);
    }

    #[test]
    fn body_note_ref_returns_location_link_to_target_label() {
        let path = PathBuf::from("/virtual/2603110001.typ");
        let index = make_index("2603110001", "Target Note", path.clone());
        let source = format!("{NOTE_CONTENT}中文😀 @2603110001\n");
        let link = get_note_ref_definition_with_loader(
            &source,
            Position {
                line: 5,
                character: 8,
            },
            &index,
            |load_path| (load_path == path.as_path()).then(|| TARGET_NOTE_CONTENT.to_string()),
        )
        .expect("expected body definition");

        assert_eq!(link.target_uri, Url::from_file_path(path).unwrap());
        assert_eq!(
            link.origin_selection_range,
            Some(Range::new(Position::new(5, 5), Position::new(5, 16)))
        );
        assert_eq!(link.target_range.start, Position::new(4, 0));
        assert_eq!(
            link.target_selection_range,
            Range::new(Position::new(4, 9), Position::new(4, 21))
        );
    }

    #[test]
    fn body_note_ref_definition_supports_self_reference() {
        let path = PathBuf::from("/virtual/2603110000.typ");
        let index = make_index("2603110000", "Host", path.clone());
        let source = format!("{NOTE_CONTENT}See @2603110000\n");
        let link = get_note_ref_definition_with_loader(
            &source,
            Position::new(5, 8),
            &index,
            |load_path| (load_path == path.as_path()).then(|| source.clone()),
        );
        assert!(link.is_some());
    }

    #[test]
    fn body_note_ref_definition_rejects_dead_and_overlong_ids() {
        let path = PathBuf::from("/virtual/2603119999.typ");
        let index = make_index("2603119999", "Other", path);
        assert!(get_note_ref_definition_with_loader(
            "See @2603110001",
            Position::new(0, 8),
            &index,
            |_| Some(TARGET_NOTE_CONTENT.to_string()),
        )
        .is_none());
        assert!(get_note_ref_definition_with_loader(
            "See @26031100011",
            Position::new(0, 8),
            &index,
            |_| Some(TARGET_NOTE_CONTENT.to_string()),
        )
        .is_none());
    }

    #[test]
    fn metadata_relation_target_jumps_to_target_title() {
        let path = PathBuf::from("/virtual/2603110001.typ");
        let index = make_index("2603110001", "Target Note", path.clone());
        let loc = get_metadata_definition_with_loader(
            METADATA_CONTENT,
            Position {
                line: 4,
                character: 22,
            },
            &index,
            |load_path| {
                if load_path == path.as_path() {
                    Some(TARGET_NOTE_CONTENT.to_string())
                } else {
                    None
                }
            },
        )
        .expect("expected definition");

        assert_eq!(loc.uri, Url::from_file_path(path).unwrap());
        assert_eq!(loc.range.start.line, 4);
    }
}

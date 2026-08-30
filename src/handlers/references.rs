use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::{metadata, parser};

pub fn find_references(
    index: &Arc<NoteIndex>,
    uri: &Url,
    content: &str,
    position: Position,
    metadata_uri: &Url,
    metadata_content: &str,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(id) = id_at_position(content, position) else {
        return Vec::new();
    };

    let mut locs = Vec::new();

    if include_declaration {
        if let Some(info) = index.get(&id) {
            if let Ok(note_content) = std::fs::read_to_string(&info.path) {
                if let Some(header) = parser::parse_header(&note_content) {
                    if let Ok(uri) = Url::from_file_path(&info.path) {
                        locs.push(Location {
                            uri,
                            range: Range {
                                start: Position {
                                    line: header.title_line_idx as u32,
                                    character: 0,
                                },
                                end: Position {
                                    line: header.title_line_idx as u32,
                                    character: 0,
                                },
                            },
                        });
                    }
                }
            }
        }
    }

    locs.extend(index.get_backlinks(&id).into_iter().map(|loc| Location {
        uri: Url::from_file_path(&loc.file).unwrap_or_else(|_| uri.clone()),
        range: Range {
            start: Position {
                line: loc.line,
                character: loc.start_char,
            },
            end: Position {
                line: loc.line,
                character: loc.end_char,
            },
        },
    }));

    locs.extend(
        metadata::all_id_positions(metadata_content)
            .into_iter()
            .filter(|id_pos| id_pos.target_id == id)
            .map(|id_pos| Location {
                uri: metadata_uri.clone(),
                range: Range {
                    start: Position {
                        line: id_pos.range.line as u32,
                        character: id_pos.range.start_col as u32,
                    },
                    end: Position {
                        line: id_pos.range.line as u32,
                        character: id_pos.range.end_col as u32,
                    },
                },
            }),
    );

    locs
}

fn id_at_position(content: &str, position: Position) -> Option<String> {
    if let Some(id_pos) =
        metadata::id_at_position(content, position.line as usize, position.character as usize)
    {
        return Some(id_pos.target_id);
    }
    if let Some(note_ref) = parser::note_ref_at_position(content, position) {
        return Some(note_ref.id);
    }
    parser::note_identity_at_position(content, position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use crate::index::BacklinkLocation;
    use std::path::PathBuf;

    fn make_index() -> Arc<NoteIndex> {
        Arc::new(NoteIndex::new(Arc::new(tokio::sync::RwLock::new(
            WikiConfig::from_root(PathBuf::from("/tmp/wiki")),
        ))))
    }

    #[test]
    fn body_references_reuse_utf16_note_ref_lookup() {
        let index = make_index();
        index
            .backlinks
            .entry("2603110001".to_string())
            .or_default()
            .push(BacklinkLocation {
                file: PathBuf::from("/tmp/wiki/note/2603110002.typ"),
                line: 3,
                start_char: 4,
                end_char: 15,
            });
        let uri = Url::parse("file:///tmp/wiki/note/2603110000.typ").unwrap();
        let metadata_uri = Url::parse("file:///tmp/wiki/metadata.toml").unwrap();
        let locations = find_references(
            &index,
            &uri,
            "中文😀 @2603110001",
            Position::new(0, 8),
            &metadata_uri,
            "format-version = 1\n",
            false,
        );

        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].range.start, Position::new(3, 4));
    }

    #[test]
    fn note_identity_and_metadata_reference_lookup_still_work() {
        let index = make_index();
        let uri = Url::parse("file:///tmp/wiki/note/2603110000.typ").unwrap();
        let metadata_uri = Url::parse("file:///tmp/wiki/metadata.toml").unwrap();
        let note = concat!(
            "#let zk-metadata = zk_metadata(\"2603110000\")\n",
            "= Host <2603110000>\n",
        );
        let metadata = concat!(
            "format-version = 1\n\n",
            "[notes.\"2603110000\"]\n",
            "relation-target = [\"2603110001\"]\n",
        );

        let from_title = find_references(
            &index,
            &uri,
            note,
            Position::new(1, 10),
            &metadata_uri,
            metadata,
            false,
        );
        assert_eq!(from_title.len(), 1);
        assert_eq!(from_title[0].range.start, Position::new(2, 8));

        let from_metadata = find_references(
            &index,
            &metadata_uri,
            metadata,
            Position::new(3, 22),
            &metadata_uri,
            metadata,
            false,
        );
        assert_eq!(from_metadata.len(), 1);
        assert_eq!(from_metadata[0].range.start.line, 3);
    }

    #[test]
    fn body_references_reject_overlong_note_ids() {
        let index = make_index();
        let uri = Url::parse("file:///tmp/wiki/note/2603110000.typ").unwrap();
        let metadata_uri = Url::parse("file:///tmp/wiki/metadata.toml").unwrap();
        assert!(find_references(
            &index,
            &uri,
            "@26031100011",
            Position::new(0, 3),
            &metadata_uri,
            "format-version = 1\n",
            false,
        )
        .is_empty());
    }
}

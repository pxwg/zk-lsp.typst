use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;
use crate::parser;

/// Generate TOML metadata completions for the given cursor position.
///
/// Returns completions only when the cursor is inside the TOML metadata block.
/// Context is inferred from the current line text:
/// - `checklist-status = "` → enum values
/// - `relation = "` → enum values
/// - `relation-target = [` → note IDs from the index
/// - blank / whitespace → missing field names
pub fn get_completions(
    content: &str,
    position: Position,
    index: &Arc<NoteIndex>,
) -> Vec<CompletionItem> {
    let Some(block) = parser::find_toml_metadata_block(content) else {
        return Vec::new();
    };

    let line_num = position.line as usize;
    if line_num < block.start_line || line_num > block.end_line {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let current_line = lines.get(line_num).copied().unwrap_or("");
    let trimmed = current_line.trim_start();

    if trimmed.starts_with("checklist-status") && trimmed.contains('"') {
        return ["none", "todo", "wip", "done"]
            .iter()
            .map(|val| CompletionItem {
                label: val.to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                ..Default::default()
            })
            .collect();
    }

    if trimmed.starts_with("relation")
        && !trimmed.starts_with("relation-target")
        && trimmed.contains('"')
    {
        return ["active", "archived", "legacy"]
            .iter()
            .map(|val| CompletionItem {
                label: val.to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                ..Default::default()
            })
            .collect();
    }

    if trimmed.starts_with("relation-target") && trimmed.contains('[') {
        // Determine whether the cursor sits inside an existing string literal.
        // Count `"` characters between the last `[` or `,` and the cursor; if
        // the count is odd the cursor is inside a string → insert bare id.
        // Otherwise wrap with quotes so the result is valid TOML.
        let col = position.character as usize;
        let prefix = &current_line[..col.min(current_line.len())];
        let after_bracket = prefix.rfind('[').map_or(prefix, |p| &prefix[p + 1..]);
        let after_delim = after_bracket
            .rfind(',')
            .map_or(after_bracket, |p| &after_bracket[p + 1..]);
        let inside_string = after_delim.chars().filter(|&c| c == '"').count() % 2 == 1;

        return index
            .notes
            .iter()
            .map(|entry| {
                let info = entry.value();
                let insert_text = if inside_string {
                    info.id.clone()
                } else {
                    format!("\"{}\"", info.id)
                };
                CompletionItem {
                    label: info.id.clone(),
                    insert_text: Some(insert_text),
                    detail: Some(info.title.clone()),
                    filter_text: Some(format!("{} {}", info.id, info.title)),
                    kind: Some(CompletionItemKind::REFERENCE),
                    ..Default::default()
                }
            })
            .collect();
    }

    // Blank line → suggest missing fields
    if trimmed.is_empty() {
        let present: Vec<&str> = block
            .toml_content
            .lines()
            .filter_map(|l| {
                let t = l.trim_start();
                if t.starts_with("schema-version") {
                    Some("schema-version")
                } else if t.starts_with("aliases") {
                    Some("aliases")
                } else if t.starts_with("abstract") {
                    Some("abstract")
                } else if t.starts_with("keywords") {
                    Some("keywords")
                } else if t.starts_with("generated") {
                    Some("generated")
                } else if t.starts_with("checklist-status") {
                    Some("checklist-status")
                } else if t.starts_with("relation-target") {
                    Some("relation-target")
                } else if t.starts_with("relation") {
                    Some("relation")
                } else if t.starts_with("title") {
                    Some("title")
                } else {
                    None
                }
            })
            .collect();

        let all_fields = [
            "schema-version",
            "title",
            "aliases",
            "abstract",
            "keywords",
            "generated",
            "checklist-status",
            "relation",
            "relation-target",
        ];

        return all_fields
            .iter()
            .filter(|f| !present.contains(f))
            .map(|f| CompletionItem {
                label: f.to_string(),
                insert_text: Some(format!("{f} = ")),
                kind: Some(CompletionItemKind::FIELD),
                ..Default::default()
            })
            .collect();
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn empty_index() -> Arc<NoteIndex> {
        Arc::new(NoteIndex::new(Arc::new(tokio::sync::RwLock::new(
            WikiConfig::from_root(PathBuf::from("/tmp")),
        ))))
    }

    const NOTE_TOML: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  checklist-status = \"none\"\n",
        "  relation = \"active\"\n",
        "  relation-target = []\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Test <2603110000>\n",
    );

    fn pos(line: u32) -> Position {
        Position { line, character: 0 }
    }

    #[test]
    fn test_completion_checklist_status() {
        let index = empty_index();
        // Line 4: `  checklist-status = "none"`
        let items = get_completions(NOTE_TOML, pos(4), &index);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"none"));
        assert!(labels.contains(&"todo"));
        assert!(labels.contains(&"wip"));
        assert!(labels.contains(&"done"));
        assert!(items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::ENUM_MEMBER)));
    }

    #[test]
    fn test_completion_relation() {
        let index = empty_index();
        // Line 5: `  relation = "active"`
        let items = get_completions(NOTE_TOML, pos(5), &index);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"active"));
        assert!(labels.contains(&"archived"));
        assert!(labels.contains(&"legacy"));
    }

    fn index_with_note(id: &str, title: &str) -> Arc<NoteIndex> {
        use crate::index::NoteInfo;
        use std::path::PathBuf;
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
                path: PathBuf::from(format!("/tmp/{id}.typ")),
            },
        );
        Arc::new(idx)
    }

    #[test]
    fn test_completion_relation_target() {
        let index = empty_index();
        // Line 6: `  relation-target = []`
        let items = get_completions(NOTE_TOML, pos(6), &index);
        // Empty index → no items, but should not panic
        assert!(items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::REFERENCE)));
    }

    #[test]
    fn test_completion_relation_target_insert_text_with_quotes() {
        // Cursor inside `[]` without an open string → insert_text should be `"id"`
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  relation-target = []\n", // line 4, cursor at col 21 (inside [ ])
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Test <2603110000>\n",
        );
        // `  relation-target = [` → col 21 puts cursor just after `[`
        let pos_inside = Position {
            line: 4,
            character: 21,
        };
        let index = index_with_note("2603110001", "Some Note");
        let items = get_completions(content, pos_inside, &index);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "2603110001");
        assert_eq!(items[0].insert_text.as_deref(), Some("\"2603110001\""));
    }

    #[test]
    fn test_completion_relation_target_insert_text_bare_inside_string() {
        // Cursor inside an already-opened `"` → insert_text should be bare id
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  relation-target = [\"]\n", // line 4, cursor after the `"` at col 22
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Test <2603110000>\n",
        );
        // col 22 is just after the `"` inside `["`
        let pos_inside_str = Position {
            line: 4,
            character: 22,
        };
        let index = index_with_note("2603110001", "Some Note");
        let items = get_completions(content, pos_inside_str, &index);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "2603110001");
        assert_eq!(items[0].insert_text.as_deref(), Some("2603110001"));
    }

    #[test]
    fn test_completion_outside_block_empty() {
        let index = empty_index();
        // Line 11: title line (outside TOML block)
        let items = get_completions(NOTE_TOML, pos(11), &index);
        assert!(items.is_empty());
    }

    #[test]
    fn test_completion_missing_fields() {
        // A note missing several fields
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  \n", // blank line at line 4 → should suggest missing fields
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Test <2603110000>\n",
        );
        let index = empty_index();
        let items = get_completions(content, pos(4), &index);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"checklist-status"));
        assert!(labels.contains(&"relation"));
        assert!(
            !labels.contains(&"schema-version"),
            "already present field must not appear"
        );
    }
}

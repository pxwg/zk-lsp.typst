/// Stateless parsing of Zettelkasten note headers and content.
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) static RE_ID_REF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"@(\d{10})").unwrap());
pub(crate) static RE_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^=\s+.*<(\d{10})>").unwrap());
pub(crate) static RE_EVO: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"#evolution_link\s*\(\s*<(\d{10})>\s*\)").unwrap());
pub(crate) static RE_ALT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"#alternative_link\s*\(\s*<(\d{10})>\s*\)").unwrap());

#[derive(Debug, Clone, PartialEq)]
pub enum ChecklistStatus {
    None,
    Todo,
    Wip,
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Relation {
    Active,
    Archived,
    Legacy,
}

#[derive(Debug, Clone)]
pub struct TomlMetadataBlock {
    pub start_line: usize, // line with `#let zk-metadata`
    pub end_line: usize,   // line with ```.text (closing fence)
    pub toml_content: String,
}

#[derive(Debug, Clone)]
pub struct ParsedToml {
    pub aliases: Vec<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub checklist_status: ChecklistStatus,
    pub relation: Relation,
    pub relation_target: Vec<String>,
}

impl Default for ParsedToml {
    fn default() -> Self {
        ParsedToml {
            aliases: Vec::new(),
            abstract_text: None,
            keywords: Vec::new(),
            checklist_status: ChecklistStatus::None,
            relation: Relation::Active,
            relation_target: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NoteHeader {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub legacy: bool,
    pub alt_id: Option<String>,
    pub evo_id: Option<String>,
    pub aliases: Vec<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub tag_line_idx: Option<usize>,        // 0-based; None for TOML-format notes
    #[allow(dead_code)]
    pub title_line_idx: usize,             // 0-based
    pub metadata_block: Option<TomlMetadataBlock>,
    pub checklist_status: Option<ChecklistStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct TodoStatus {
    pub completed: usize,
    pub incomplete: usize,
}

#[derive(Debug, Clone)]
pub struct RefOccurrence {
    pub id: String,
    pub line: u32,
    pub start_char: u32,
    pub end_char: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatusTag {
    Todo,
    Wip,
    Done,
}

/// Scan `content` for a `#let zk-metadata = toml(bytes(` block.
/// Returns the block's location and extracted TOML string, or None.
pub fn find_toml_metadata_block(content: &str) -> Option<TomlMetadataBlock> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the #let zk-metadata = toml(bytes( line
    let start_line = lines
        .iter()
        .position(|l| l.trim().starts_with("#let zk-metadata") && l.contains("toml(bytes("))?;

    // Find the ```toml fence line
    let toml_fence_offset = lines[start_line..]
        .iter()
        .position(|l| l.trim() == "```toml")?;
    let toml_fence = start_line + toml_fence_offset;

    // Collect TOML content until the closing ``` fence
    let mut toml_lines: Vec<&str> = Vec::new();
    let mut end_line = None;
    for (i, line) in lines[toml_fence + 1..].iter().enumerate() {
        if line.trim().starts_with("```") {
            end_line = Some(toml_fence + 1 + i);
            break;
        }
        toml_lines.push(line);
    }
    let end_line = end_line?;

    Some(TomlMetadataBlock {
        start_line,
        end_line,
        toml_content: toml_lines.join("\n"),
    })
}

/// Parse a raw TOML string extracted from a metadata block.
pub fn parse_toml_metadata(toml_str: &str) -> Option<ParsedToml> {
    let value: toml::Value = toml_str.parse().ok()?;
    let table = value.as_table()?;

    let aliases = table
        .get("aliases")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let abstract_text = table
        .get("abstract")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let keywords = table
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let checklist_status = table
        .get("checklist-status")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "todo" => ChecklistStatus::Todo,
            "wip" => ChecklistStatus::Wip,
            "done" => ChecklistStatus::Done,
            _ => ChecklistStatus::None,
        })
        .unwrap_or(ChecklistStatus::None);

    let relation = table
        .get("relation")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "archived" => Relation::Archived,
            "legacy" => Relation::Legacy,
            _ => Relation::Active,
        })
        .unwrap_or(Relation::Active);

    let relation_target = table
        .get("relation-target")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Some(ParsedToml {
        aliases,
        abstract_text,
        keywords,
        checklist_status,
        relation,
        relation_target,
    })
}

/// Parse the header of a TOML-format note.
/// Returns `None` for legacy comment-format notes (run `zk-lsp migrate` first).
pub fn parse_header(content: &str) -> Option<NoteHeader> {
    let lines: Vec<&str> = content.lines().collect();

    let block = find_toml_metadata_block(content)?;
    let parsed = parse_toml_metadata(&block.toml_content).unwrap_or_default();

    // Title line is the first heading after the TOML block
    let title_line_idx = lines[block.end_line + 1..]
        .iter()
        .position(|l| RE_TITLE.is_match(l))
        .map(|offset| block.end_line + 1 + offset)?;

    let title_line = lines[title_line_idx];
    let id = RE_TITLE
        .captures(title_line)?
        .get(1)?
        .as_str()
        .to_string();
    let title = RE_TITLE
        .captures(title_line)?
        .get(0)?
        .as_str()
        .trim_start_matches('=')
        .trim()
        .rsplit_once('<')
        .map(|(t, _)| t.trim().to_string())
        .unwrap_or_default();

    let archived = parsed.relation == Relation::Archived;
    let legacy = parsed.relation == Relation::Legacy;
    let alt_id = if archived {
        parsed.relation_target.first().cloned()
    } else {
        None
    };
    let evo_id = if legacy {
        parsed.relation_target.first().cloned()
    } else {
        None
    };
    let checklist_status = parsed.checklist_status.clone();

    Some(NoteHeader {
        id,
        title,
        archived,
        legacy,
        alt_id,
        evo_id,
        aliases: parsed.aliases,
        abstract_text: parsed.abstract_text,
        keywords: parsed.keywords,
        tag_line_idx: None,
        title_line_idx,
        metadata_block: Some(block),
        checklist_status: Some(checklist_status),
    })
}

/// Count todo items, skipping code blocks (``` fence heuristic).
pub fn count_todos(content: &str) -> TodoStatus {
    let mut status = TodoStatus::default();
    let mut in_code_block = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if trimmed.starts_with("- [") && trimmed.len() >= 5 {
            let marker = trimmed.chars().nth(3).unwrap_or(' ');
            if marker == 'x' || marker == 'X' {
                status.completed += 1;
            } else if marker == ' ' {
                status.incomplete += 1;
            }
        }
    }
    status
}

/// Convert a byte offset within `s` to a UTF-16 code-unit offset.
/// LSP `character` positions are UTF-16 code units, not bytes or scalar values.
pub fn byte_to_utf16(s: &str, byte_offset: usize) -> u32 {
    s[..byte_offset].chars().map(|c| c.len_utf16() as u32).sum()
}

/// Find all @ID occurrences in content (10-digit IDs).
/// `start_char` / `end_char` are **byte** offsets within the line (not UTF-16).
/// Convert with `byte_to_utf16` before using as LSP character positions.
pub fn find_all_refs(content: &str) -> Vec<RefOccurrence> {
    let mut refs = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        for cap in RE_ID_REF.captures_iter(line) {
            let m = cap.get(0).unwrap();
            let id_m = cap.get(1).unwrap();
            refs.push(RefOccurrence {
                id: id_m.as_str().to_string(),
                line: line_num as u32,
                start_char: m.start() as u32,
                end_char: m.end() as u32,
            });
        }
    }
    refs
}

/// Compute the status tag based on todo counts and archived flag.
pub fn compute_status_tag(todos: &TodoStatus, has_archived: bool) -> Option<StatusTag> {
    let has_todos = todos.completed > 0 || todos.incomplete > 0;
    if !has_todos {
        return None;
    }
    if has_archived {
        return Some(StatusTag::Done);
    }
    if todos.incomplete == 0 && todos.completed > 0 {
        Some(StatusTag::Done)
    } else if todos.completed > 0 {
        Some(StatusTag::Wip)
    } else {
        Some(StatusTag::Todo)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Legacy-format fixtures — kept for migration tests; parse_header returns None for these.
    pub(crate) const NOTE_WITH_META: &str = concat!(
        "/* Metadata:\n",
        "Aliases: ZK LSP\n",
        "Abstract: A test note.\n",
        "Keyword: test, rust\n",
        "Generated: true\n",
        "*/\n",
        "#import \"../include.typ\": *\n",
        "#show: zettel\n",
        "\n",
        "= Test Note <2602082037>\n",
        "#tag.archived #tag.done\n",
        "#alternative_link(<2602131642>)\n",
        "\n",
        "Some content here. @2602082135\n",
    );

    pub(crate) const NOTE_NO_META: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#show: zettel\n",
        "\n",
        "= Simple Note <2602082106>\n",
        "#tag.todo\n",
        "\n",
        "Content. @2602082037\n",
    );

    const NOTE_TOML_META: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  aliases = [\"ZK TOML\"]\n",
        "  abstract = \"A TOML test note.\"\n",
        "  keywords = [\"test\", \"toml\"]\n",
        "  generated = true\n",
        "  checklist-status = \"none\"\n",
        "  relation = \"active\"\n",
        "  relation-target = []\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= TOML Note <2603110000>\n",
    );

    const NOTE_TOML_ARCHIVED: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = toml(bytes(\n",
        "  ```toml\n",
        "  schema-version = 1\n",
        "  aliases = []\n",
        "  abstract = \"\"\n",
        "  keywords = []\n",
        "  generated = true\n",
        "  checklist-status = \"done\"\n",
        "  relation = \"archived\"\n",
        "  relation-target = [\"2603110001\"]\n",
        "  ```.text,\n",
        "))\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Archived Note <2603110002>\n",
    );

    #[test]
    fn test_parse_header_legacy_unsupported() {
        // Legacy comment-format notes are not parsed; use `zk-lsp migrate` first.
        assert!(parse_header(NOTE_WITH_META).is_none());
        assert!(parse_header(NOTE_NO_META).is_none());
    }

    #[test]
    fn test_parse_header_toml_active() {
        let h = parse_header(NOTE_TOML_META).unwrap();
        assert_eq!(h.id, "2603110000");
        assert_eq!(h.title, "TOML Note");
        assert!(!h.archived);
        assert!(!h.legacy);
        assert_eq!(h.aliases, vec!["ZK TOML"]);
        assert_eq!(h.keywords, vec!["test", "toml"]);
        assert_eq!(h.abstract_text.as_deref(), Some("A TOML test note."));
        assert_eq!(h.tag_line_idx, None);
        assert_eq!(h.checklist_status, Some(ChecklistStatus::None));
        assert!(h.metadata_block.is_some());
    }

    #[test]
    fn test_parse_header_toml_archived() {
        let h = parse_header(NOTE_TOML_ARCHIVED).unwrap();
        assert_eq!(h.id, "2603110002");
        assert!(h.archived);
        assert!(!h.legacy);
        assert_eq!(h.alt_id.as_deref(), Some("2603110001"));
        assert_eq!(h.evo_id, None);
        assert_eq!(h.checklist_status, Some(ChecklistStatus::Done));
        assert_eq!(h.tag_line_idx, None);
    }

    #[test]
    fn test_count_todos() {
        let content = "- [ ] incomplete\n- [x] done\n```\n- [ ] skipped\n```\n- [X] also done\n";
        let s = count_todos(content);
        assert_eq!(s.incomplete, 1);
        assert_eq!(s.completed, 2);
    }

    #[test]
    fn test_find_all_refs() {
        let refs = find_all_refs("see @2602082037 and @2602082106");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "2602082037");
        assert_eq!(refs[1].id, "2602082106");
    }

    #[test]
    fn test_byte_to_utf16_cjk() {
        // "你好 " = 3+3+1 = 7 bytes, but 3 UTF-16 code units
        let line = "Hello, world 你好 @2602171536";
        let refs = find_all_refs(line);
        assert_eq!(refs.len(), 1);
        // '@' byte offset = 13 + 3 + 3 + 1 = 20
        assert_eq!(refs[0].start_char, 20);
        // UTF-16 offset = 13 + 1 + 1 + 1 = 16
        assert_eq!(byte_to_utf16(line, refs[0].start_char as usize), 16);
        // end byte offset = 20 + 11 = 31, UTF-16 = 16 + 11 = 27
        assert_eq!(byte_to_utf16(line, refs[0].end_char as usize), 27);
    }

    #[test]
    fn test_compute_status_tag() {
        let all_done = TodoStatus {
            completed: 3,
            incomplete: 0,
        };
        assert_eq!(compute_status_tag(&all_done, false), Some(StatusTag::Done));

        let mixed = TodoStatus {
            completed: 1,
            incomplete: 2,
        };
        assert_eq!(compute_status_tag(&mixed, false), Some(StatusTag::Wip));

        let all_incomplete = TodoStatus {
            completed: 0,
            incomplete: 2,
        };
        assert_eq!(
            compute_status_tag(&all_incomplete, false),
            Some(StatusTag::Todo)
        );

        let archived_mixed = TodoStatus {
            completed: 1,
            incomplete: 1,
        };
        assert_eq!(
            compute_status_tag(&archived_mixed, true),
            Some(StatusTag::Done)
        );
    }
}

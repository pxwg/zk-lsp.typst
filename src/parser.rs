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
    #[allow(dead_code)]
    pub schema_version: u32,
    pub aliases: Vec<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    #[allow(dead_code)]
    pub generated: bool,
    pub checklist_status: ChecklistStatus,
    pub relation: Relation,
    pub relation_target: Vec<String>,
    /// Non-core fields (e.g., `[user]` table) preserved from the TOML block.
    pub extra: toml::Table,
}

impl Default for ParsedToml {
    fn default() -> Self {
        ParsedToml {
            schema_version: 1,
            aliases: Vec::new(),
            abstract_text: None,
            keywords: Vec::new(),
            generated: false,
            checklist_status: ChecklistStatus::None,
            relation: Relation::Active,
            relation_target: Vec::new(),
            extra: toml::Table::new(),
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
    pub relation_target: Vec<String>,
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

    let schema_version = table
        .get("schema-version")
        .and_then(|v| v.as_integer())
        .map(|n| n as u32)
        .unwrap_or(1);

    let generated = table
        .get("generated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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

    const CORE_FIELDS: &[&str] = &[
        "schema-version",
        "aliases",
        "abstract",
        "keywords",
        "generated",
        "checklist-status",
        "relation",
        "relation-target",
    ];
    let extra: toml::Table = table
        .iter()
        .filter(|(k, _)| !CORE_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    Some(ParsedToml {
        schema_version,
        aliases,
        abstract_text,
        keywords,
        generated,
        checklist_status,
        relation,
        relation_target,
        extra,
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
        relation_target: parsed.relation_target.clone(),
        aliases: parsed.aliases,
        abstract_text: parsed.abstract_text,
        keywords: parsed.keywords,
        tag_line_idx: None,
        title_line_idx,
        metadata_block: Some(block),
        checklist_status: Some(checklist_status),
    })
}

// ---------------------------------------------------------------------------
// Checklist item model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct RefTarget {
    pub target_id: String,
    pub byte_start: u32, // byte offset of '@' within the full line
    pub byte_end: u32,   // byte offset past the last digit
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChecklistItemKind {
    Local,
    Ref { targets: Vec<RefTarget> },
}

#[derive(Debug, Clone)]
pub struct ChecklistItem {
    pub checked: bool,
    pub kind: ChecklistItemKind,
    #[allow(dead_code)]
    pub text: String,
    pub line_idx: usize,
    pub indent: usize,
}

/// Parse all checklist items from `content`, skipping fenced code blocks.
/// Items with `@(\d{10})` in their text become `Ref` items; all others are `Local`.
/// `RefTarget.byte_start`/`byte_end` are byte offsets of `@ID` within the full line.
pub fn parse_checklist_items(content: &str) -> Vec<ChecklistItem> {
    let mut items = Vec::new();
    let mut in_fence = false;

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !(trimmed.starts_with("- [") && trimmed.len() >= 5) {
            continue;
        }
        let marker = trimmed.chars().nth(3).unwrap_or(' ');
        if marker != 'x' && marker != 'X' && marker != ' ' {
            continue;
        }
        let checked = marker == 'x' || marker == 'X';
        let indent = line.len() - trimmed.len();
        // prefix_len: bytes before the checklist body (indent + "- [x] ")
        let prefix_len = indent + 6;
        // text after `- [x] ` (or `- [ ] `)
        let body = trimmed.get(6..).unwrap_or("");
        let text = body.to_string();
        let targets: Vec<RefTarget> = RE_ID_REF
            .captures_iter(body)
            .map(|c| {
                let full = c.get(0).unwrap();
                let id = c.get(1).unwrap().as_str().to_string();
                RefTarget {
                    target_id: id,
                    byte_start: (prefix_len + full.start()) as u32,
                    byte_end: (prefix_len + full.end()) as u32,
                }
            })
            .collect();
        let kind = if targets.is_empty() {
            ChecklistItemKind::Local
        } else {
            ChecklistItemKind::Ref { targets }
        };
        items.push(ChecklistItem { checked, kind, text, line_idx, indent });
    }
    items
}

/// Evaluate the semantic truth of a single checklist item.
/// `Local` items: truth = checkbox state.
/// `Ref` items: truth = `∀ t ∈ targets: done_lookup(t.target_id)` — never the rendered checkbox.
pub fn eval_item_truth(item: &ChecklistItem, done_lookup: &impl Fn(&str) -> bool) -> bool {
    match &item.kind {
        ChecklistItemKind::Local => item.checked,
        ChecklistItemKind::Ref { targets } => targets.iter().all(|t| done_lookup(&t.target_id)),
    }
}

fn is_leaf(items: &[ChecklistItem], idx: usize) -> bool {
    idx + 1 >= items.len() || items[idx + 1].indent <= items[idx].indent
}

/// Compute whether a note is done based on its checklist items and a dependency lookup.
///
/// Only **leaf items** participate: a leaf is an item with no subsequent item
/// with strictly greater indent before the next same-or-lesser-indent item.
/// Non-leaf LocalItems are derived display views and must not be counted as source facts.
/// If there are no items, returns `false` (caller should check metadata separately).
pub fn compute_note_done_from_items(
    items: &[ChecklistItem],
    done_lookup: &impl Fn(&str) -> bool,
) -> bool {
    let leaves: Vec<&ChecklistItem> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| is_leaf(items, *i))
        .map(|(_, item)| item)
        .collect();
    if leaves.is_empty() {
        return false;
    }
    leaves.iter().all(|item| eval_item_truth(item, done_lookup))
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

/// Find all @ID occurrences in content, skipping:
/// - TOML metadata block lines
/// - Block comments (`/* ... */`, including multi-line)
/// - Fenced code blocks (``` ... ```)
pub fn find_all_refs_filtered(content: &str) -> Vec<RefOccurrence> {
    let mut refs = Vec::new();

    let toml_range = find_toml_metadata_block(content).map(|b| b.start_line..=b.end_line);

    let mut in_block_comment = false;
    let mut in_fence = false;

    for (line_num, line) in content.lines().enumerate() {
        // Skip TOML metadata block lines
        if let Some(ref range) = toml_range {
            if range.contains(&line_num) {
                continue;
            }
        }

        // Handle block comment continuation
        if in_block_comment {
            if let Some(end_offset) = line.find("*/") {
                in_block_comment = false;
                // Process visible content after end of block comment
                // by falling through with adjusted pos below
                let after_offset = end_offset + 2;
                let mut visible_segments: Vec<(usize, usize)> = Vec::new();
                let mut pos = after_offset;
                loop {
                    let remaining = &line[pos..];
                    if let Some(bc_start) = remaining.find("/*") {
                        visible_segments.push((pos, pos + bc_start));
                        let bc_abs = pos + bc_start;
                        if let Some(end_off) = line[bc_abs + 2..].find("*/") {
                            pos = bc_abs + 2 + end_off + 2;
                        } else {
                            in_block_comment = true;
                            break;
                        }
                    } else {
                        visible_segments.push((pos, line.len()));
                        break;
                    }
                }
                for (seg_start, seg_end) in visible_segments {
                    let segment = &line[seg_start..seg_end];
                    for cap in RE_ID_REF.captures_iter(segment) {
                        let m = cap.get(0).unwrap();
                        let id_m = cap.get(1).unwrap();
                        refs.push(RefOccurrence {
                            id: id_m.as_str().to_string(),
                            line: line_num as u32,
                            start_char: (seg_start + m.start()) as u32,
                            end_char: (seg_start + m.end()) as u32,
                        });
                    }
                }
            }
            // Whether we found */ or not, move to next line
            continue;
        }

        // Fence toggle (only when not in block comment)
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        // Normal line: scan for block comment boundaries and collect visible segments
        let mut visible_segments: Vec<(usize, usize)> = Vec::new();
        let mut pos = 0;
        loop {
            let remaining = &line[pos..];
            if let Some(bc_start) = remaining.find("/*") {
                visible_segments.push((pos, pos + bc_start));
                let bc_abs = pos + bc_start;
                if let Some(end_off) = line[bc_abs + 2..].find("*/") {
                    pos = bc_abs + 2 + end_off + 2;
                } else {
                    in_block_comment = true;
                    break;
                }
            } else {
                visible_segments.push((pos, line.len()));
                break;
            }
        }

        for (seg_start, seg_end) in visible_segments {
            let segment = &line[seg_start..seg_end];
            for cap in RE_ID_REF.captures_iter(segment) {
                let m = cap.get(0).unwrap();
                let id_m = cap.get(1).unwrap();
                refs.push(RefOccurrence {
                    id: id_m.as_str().to_string(),
                    line: line_num as u32,
                    start_char: (seg_start + m.start()) as u32,
                    end_char: (seg_start + m.end()) as u32,
                });
            }
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
    fn test_ref_target_spans() {
        // Line: "  - [ ] @1111111111 and @2222222222"
        // indent=2, prefix_len=8
        // "@1111111111" starts at byte 8, ends at 19
        // "@2222222222" starts at byte 24, ends at 35
        let line = "  - [ ] @1111111111 and @2222222222";
        let content = format!("{line}\n");
        let items = parse_checklist_items(&content);
        assert_eq!(items.len(), 1);
        if let ChecklistItemKind::Ref { targets } = &items[0].kind {
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0].target_id, "1111111111");
            assert_eq!(targets[0].byte_start, 8);
            assert_eq!(targets[0].byte_end, 19);
            assert_eq!(&line[8..19], "@1111111111");
            assert_eq!(targets[1].target_id, "2222222222");
            assert_eq!(targets[1].byte_start, 24);
            assert_eq!(targets[1].byte_end, 35);
            assert_eq!(&line[24..35], "@2222222222");
        } else {
            panic!("expected Ref kind");
        }
    }

    #[test]
    fn test_find_refs_skips_block_comment() {
        let content = "see @2602082037\n/* skip @9999999999 */\nand @2602082106\n";
        let refs = find_all_refs_filtered(content);
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"2602082037"));
        assert!(ids.contains(&"2602082106"));
        assert!(!ids.contains(&"9999999999"), "ID in block comment must be skipped");
    }

    #[test]
    fn test_find_refs_skips_toml_block() {
        let content = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  relation-target = [\"2603110001\"]\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Test <2603110000>\n",
            "\n",
            "See @2603110002\n",
        );
        let refs = find_all_refs_filtered(content);
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        // ID inside TOML block should be skipped
        assert!(!ids.contains(&"2603110001"), "ID in TOML block must be skipped");
        // ID in regular content should be found
        assert!(ids.contains(&"2603110002"));
    }

    #[test]
    fn test_find_refs_skips_fenced_block() {
        let content = "before @1111111111\n```\n@2222222222\n```\nafter @3333333333\n";
        let refs = find_all_refs_filtered(content);
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"1111111111"));
        assert!(!ids.contains(&"2222222222"), "ID in fenced block must be skipped");
        assert!(ids.contains(&"3333333333"));
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

    #[test]
    fn test_parse_toml_metadata_preserves_extra_fields() {
        let toml_str = concat!(
            "schema-version = 1\n",
            "aliases = []\n",
            "abstract = \"\"\n",
            "keywords = []\n",
            "generated = false\n",
            "checklist-status = \"none\"\n",
            "relation = \"active\"\n",
            "relation-target = []\n",
            "\n",
            "[user]\n",
            "course = \"QFT\"\n",
            "priority = \"high\"\n",
        );
        let parsed = parse_toml_metadata(toml_str).unwrap();
        // Core fields still work
        assert_eq!(parsed.relation, Relation::Active);
        assert_eq!(parsed.checklist_status, ChecklistStatus::None);
        // Extra fields preserved
        assert!(parsed.extra.contains_key("user"), "user table should be in extra");
        let user = parsed.extra["user"].as_table().unwrap();
        assert_eq!(user["course"].as_str(), Some("QFT"));
        assert_eq!(user["priority"].as_str(), Some("high"));
    }

    #[test]
    fn test_parse_toml_metadata_no_extra_fields() {
        let toml_str = concat!(
            "schema-version = 1\n",
            "aliases = []\n",
            "abstract = \"\"\n",
            "keywords = []\n",
            "generated = false\n",
            "checklist-status = \"none\"\n",
            "relation = \"active\"\n",
            "relation-target = []\n",
        );
        let parsed = parse_toml_metadata(toml_str).unwrap();
        assert_eq!(parsed.extra.len(), 0, "no extra fields expected");
    }

    #[test]
    fn test_parse_header_preserves_extra_in_parsed_toml() {
        let note = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  aliases = []\n",
            "  abstract = \"\"\n",
            "  keywords = []\n",
            "  generated = false\n",
            "  checklist-status = \"none\"\n",
            "  relation = \"active\"\n",
            "  relation-target = []\n",
            "\n",
            "  [user]\n",
            "  course = \"Topology\"\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= Extra Fields Note <2603120001>\n",
        );
        // parse_header should succeed
        let header = parse_header(note).unwrap();
        assert_eq!(header.id, "2603120001");
        // parse_toml_metadata directly should preserve extra
        let block = find_toml_metadata_block(note).unwrap();
        let parsed = parse_toml_metadata(&block.toml_content).unwrap();
        let user = parsed.extra["user"].as_table().unwrap();
        assert_eq!(user["course"].as_str(), Some("Topology"));
    }
}

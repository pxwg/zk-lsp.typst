/// Stateless parsing of Zettelkasten note headers and content.
use once_cell::sync::Lazy;
use regex::Regex;
use tower_lsp::lsp_types::{Position, Range};

pub(crate) static RE_ID_REF: Lazy<Regex> = Lazy::new(|| Regex::new(r"@([0-9]{10})").unwrap());
pub(crate) static RE_ANGLE_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"<([0-9]{10})>").unwrap());
pub(crate) static RE_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^=\s+.*<([0-9]{10})>").unwrap());
static RE_BINDING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^\s*#let\s+zk-metadata\s*=\s*zk_metadata\("([0-9]{10})"\)\s*$"#).unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecklistStatus {
    None,
    Todo,
    Wip,
    Done,
}

impl ChecklistStatus {
    pub const ALL: [Self; 4] = [Self::None, Self::Todo, Self::Wip, Self::Done];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Todo => "todo",
            Self::Wip => "wip",
            Self::Done => "done",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "todo" => Some(Self::Todo),
            "wip" => Some(Self::Wip),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Relation {
    Active,
    Archived,
    Legacy,
}

#[derive(Debug, Clone)]
pub struct MetadataBinding {
    pub line_idx: usize,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct NoteHeader {
    pub id: String,
    pub title: String,
    #[allow(dead_code)]
    pub title_line_idx: usize, // 0-based
}

#[derive(Debug, Clone)]
pub struct RefOccurrence {
    pub id: String,
    pub line: u32,
    pub start_char: u32,
    pub end_char: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRefAtPosition {
    pub id: String,
    pub range: Range,
}

#[derive(Debug, Clone, Copy)]
struct VisibleSegment {
    line: u32,
    start: usize,
    end: usize,
}

/// Scan `content` for a canonical central metadata binding:
/// `#let zk-metadata = zk_metadata("ID")`.
pub fn find_metadata_binding(content: &str) -> Option<MetadataBinding> {
    let mut found = None;
    for (line_idx, line) in content.lines().enumerate() {
        let Some(caps) = RE_BINDING.captures(line) else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some(MetadataBinding {
            line_idx,
            id: caps.get(1)?.as_str().to_string(),
        });
    }
    found
}

/// Parse the structural header of a central-metadata note.
pub fn parse_header(content: &str) -> Option<NoteHeader> {
    let mut binding = None;
    let mut title = None;

    for (line_idx, line) in content.lines().enumerate() {
        if let Some(captures) = RE_BINDING.captures(line) {
            if binding.is_some() {
                return None;
            }
            binding = Some(MetadataBinding {
                line_idx,
                id: captures.get(1)?.as_str().to_string(),
            });
        }

        if title.is_none() {
            if let Some(captures) = RE_TITLE.captures(line) {
                let matched = captures.get(0)?.as_str();
                title = Some(NoteHeader {
                    id: captures.get(1)?.as_str().to_string(),
                    title: matched
                        .trim_start_matches('=')
                        .trim()
                        .rsplit_once('<')
                        .map(|(title, _)| title.trim().to_string())
                        .unwrap_or_default(),
                    title_line_idx: line_idx,
                });
            }
        }
    }

    let binding = binding?;
    let title = title?;
    if binding.line_idx >= title.title_line_idx || binding.id != title.id {
        return None;
    }
    Some(title)
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
            .filter_map(|captures| {
                let full = captures.get(0)?;
                if is_followed_by_ascii_digit(body, full.end()) {
                    return None;
                }
                Some(RefTarget {
                    target_id: captures.get(1)?.as_str().to_string(),
                    byte_start: (prefix_len + full.start()) as u32,
                    byte_end: (prefix_len + full.end()) as u32,
                })
            })
            .collect();
        let kind = if targets.is_empty() {
            ChecklistItemKind::Local
        } else {
            ChecklistItemKind::Ref { targets }
        };
        items.push(ChecklistItem {
            checked,
            kind,
            text,
            line_idx,
            indent,
        });
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

/// Convert a byte offset within `s` to a UTF-16 code-unit offset.
/// LSP `character` positions are UTF-16 code units, not bytes or scalar values.
pub fn byte_to_utf16(s: &str, byte_offset: usize) -> u32 {
    s[..byte_offset].chars().map(|c| c.len_utf16() as u32).sum()
}

fn utf16_to_byte(s: &str, character: u32) -> Option<usize> {
    if character == 0 {
        return Some(0);
    }

    let mut utf16_units = 0;
    for (byte_idx, ch) in s.char_indices() {
        utf16_units += ch.len_utf16() as u32;
        if utf16_units == character {
            return Some(byte_idx + ch.len_utf8());
        }
        if utf16_units > character {
            return None;
        }
    }

    (utf16_units == character).then_some(s.len())
}

fn is_followed_by_ascii_digit(text: &str, byte_offset: usize) -> bool {
    text.as_bytes()
        .get(byte_offset)
        .is_some_and(|byte| byte.is_ascii_digit())
}

fn push_refs_from_segment(
    refs: &mut Vec<RefOccurrence>,
    line: u32,
    segment: &str,
    segment_start: usize,
) {
    for captures in RE_ID_REF.captures_iter(segment) {
        let Some(full) = captures.get(0) else {
            continue;
        };
        if is_followed_by_ascii_digit(segment, full.end()) {
            continue;
        }
        let Some(id) = captures.get(1) else {
            continue;
        };
        refs.push(RefOccurrence {
            id: id.as_str().to_string(),
            line,
            start_char: (segment_start + full.start()) as u32,
            end_char: (segment_start + full.end()) as u32,
        });
    }
}

fn visible_segments(content: &str) -> Vec<VisibleSegment> {
    let mut segments = Vec::new();
    let mut in_block_comment = false;
    let mut in_fence = false;

    for (line_idx, line) in content.lines().enumerate() {
        if !in_block_comment && line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        let mut pos = 0;
        while pos < line.len() {
            if in_block_comment {
                let Some(end_offset) = line[pos..].find("*/") else {
                    break;
                };
                pos += end_offset + 2;
                in_block_comment = false;
                continue;
            }

            if let Some(start_offset) = line[pos..].find("/*") {
                let comment_start = pos + start_offset;
                if pos < comment_start {
                    segments.push(VisibleSegment {
                        line: line_idx as u32,
                        start: pos,
                        end: comment_start,
                    });
                }
                pos = comment_start + 2;
                in_block_comment = true;
            } else {
                segments.push(VisibleSegment {
                    line: line_idx as u32,
                    start: pos,
                    end: line.len(),
                });
                break;
            }
        }
    }

    segments
}

fn range_contains_position(range: Range, position: Position) -> bool {
    position.line == range.start.line
        && position.character >= range.start.character
        && position.character <= range.end.character
}

fn line_byte_range_to_lsp(line_idx: u32, line: &str, start: usize, end: usize) -> Range {
    Range {
        start: Position {
            line: line_idx,
            character: byte_to_utf16(line, start),
        },
        end: Position {
            line: line_idx,
            character: byte_to_utf16(line, end),
        },
    }
}

/// Find all exact `@ID` occurrences in content, where `ID` is ten ASCII digits.
/// `start_char` / `end_char` are **byte** offsets within the line (not UTF-16).
/// Convert with `byte_to_utf16` before using as LSP character positions.
pub fn find_all_refs(content: &str) -> Vec<RefOccurrence> {
    let mut refs = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        push_refs_from_segment(&mut refs, line_num as u32, line, 0);
    }
    refs
}

/// Find all exact `@ID` occurrences in visible content, skipping block comments
/// and fenced code blocks.
pub fn find_all_refs_filtered(content: &str) -> Vec<RefOccurrence> {
    let lines: Vec<&str> = content.lines().collect();
    let mut refs = Vec::new();

    for visible in visible_segments(content) {
        let Some(line) = lines.get(visible.line as usize) else {
            continue;
        };
        push_refs_from_segment(
            &mut refs,
            visible.line,
            &line[visible.start..visible.end],
            visible.start,
        );
    }

    refs
}

/// Return the complete visible `@ID` token at an LSP position.
pub fn note_ref_at_position(content: &str, position: Position) -> Option<NoteRefAtPosition> {
    let lines: Vec<&str> = content.lines().collect();
    find_all_refs_filtered(content)
        .into_iter()
        .filter(|occurrence| occurrence.line == position.line)
        .find_map(|occurrence| {
            let line = lines.get(occurrence.line as usize)?;
            let range = line_byte_range_to_lsp(
                occurrence.line,
                line,
                occurrence.start_char as usize,
                occurrence.end_char as usize,
            );
            range_contains_position(range, position).then_some(NoteRefAtPosition {
                id: occurrence.id,
                range,
            })
        })
}

/// Return the zero-width insertion range after a visible bare `@` trigger.
pub fn note_ref_completion_range(content: &str, position: Position) -> Option<Range> {
    let line = content.lines().nth(position.line as usize)?;
    let cursor_byte = utf16_to_byte(line, position.character)?;
    let (at_byte, previous) = line[..cursor_byte].char_indices().next_back()?;
    if previous != '@' {
        return None;
    }

    let visible = visible_segments(content).into_iter().any(|segment| {
        segment.line == position.line && at_byte >= segment.start && at_byte < segment.end
    });
    visible.then_some(Range {
        start: position,
        end: position,
    })
}

/// Return the current note's identity when the position is on its canonical
/// metadata binding or title-heading ID.
pub fn note_identity_at_position(content: &str, position: Position) -> Option<String> {
    let header = parse_header(content)?;
    let binding = find_metadata_binding(content)?;
    let lines: Vec<&str> = content.lines().collect();

    let (line_idx, token, use_last_match) = if position.line == header.title_line_idx as u32 {
        (header.title_line_idx, format!("<{}>", header.id), true)
    } else if position.line == binding.line_idx as u32 {
        (binding.line_idx, format!("\"{}\"", header.id), false)
    } else {
        return None;
    };

    let line = *lines.get(line_idx)?;
    let start = if use_last_match {
        line.rfind(&token)?
    } else {
        line.find(&token)?
    };
    let range = line_byte_range_to_lsp(position.line, line, start, start + token.len());
    range_contains_position(range, position).then_some(header.id)
}

/// Find all link target IDs in content, combining both `@ID` and `<ID>` forms,
/// while skipping block comments and fenced code blocks.
///
/// The note's own title ID (`= Title <ID>`) is excluded to avoid treating the
/// note header as a self-link.
pub fn find_all_link_ids_filtered(content: &str) -> Vec<String> {
    let self_id = parse_header(content).map(|h| h.id);
    let mut ids: Vec<String> = find_all_refs_filtered(content)
        .into_iter()
        .map(|r| r.id)
        .collect();

    let mut in_block_comment = false;
    let mut in_fence = false;

    for line in content.lines() {
        let mut visible_segments = Vec::new();
        let mut pos = 0usize;
        loop {
            if in_block_comment {
                let Some(end_offset) = line[pos..].find("*/") else {
                    break;
                };
                in_block_comment = false;
                pos += end_offset + 2;
                continue;
            }

            let trimmed = line[pos..].trim_start();
            if pos == 0 && trimmed.starts_with("```") {
                in_fence = !in_fence;
                break;
            }
            if in_fence {
                break;
            }

            let remaining = &line[pos..];
            let bc_start = remaining.find("/*");
            let fence_start = remaining.find("```");
            let next_cut = match (bc_start, fence_start) {
                (Some(bc), Some(fence)) => Some(bc.min(fence)),
                (Some(bc), None) => Some(bc),
                (None, Some(fence)) => Some(fence),
                (None, None) => None,
            };

            match next_cut {
                Some(offset) => {
                    visible_segments.push((pos, pos + offset));
                    let abs = pos + offset;
                    if remaining[offset..].starts_with("/*") {
                        in_block_comment = true;
                        pos = abs + 2;
                    } else {
                        in_fence = true;
                        break;
                    }
                }
                None => {
                    visible_segments.push((pos, line.len()));
                    break;
                }
            }
        }

        for (seg_start, seg_end) in visible_segments {
            let segment = &line[seg_start..seg_end];
            for cap in RE_ANGLE_ID.captures_iter(segment) {
                let id = cap.get(1).unwrap().as_str();
                if self_id.as_deref() == Some(id) {
                    continue;
                }
                ids.push(id.to_string());
            }
        }
    }

    ids
}

/// A heading parsed from note content (outside fenced code).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Heading {
    pub level: u32,
    pub text: String,
    pub line_idx: usize,
}

/// Parse all headings from `content`, skipping fenced code blocks.
/// For the title heading (matching `= Title <YYMMDDHHMM>`), the ` <ID>` suffix is stripped.
#[allow(dead_code)]
pub fn parse_headings(content: &str) -> Vec<Heading> {
    static RE_HEADING: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(=+)\s+(.+)").unwrap());
    static RE_ID_SUFFIX: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+<[0-9]{10}>$").unwrap());

    let mut headings = Vec::new();
    let mut in_fence = false;

    for (line_idx, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(cap) = RE_HEADING.captures(line) {
            let level = cap[1].len() as u32;
            let raw_text = cap[2].trim().to_string();
            let text = RE_ID_SUFFIX.replace(&raw_text, "").to_string();
            headings.push(Heading {
                level,
                text,
                line_idx,
            });
        }
    }
    headings
}

#[cfg(test)]
mod tests {
    use super::*;

    const CENTRAL_NOTE: &str = concat!(
        "#import \"../include.typ\": *\n",
        "#let zk-metadata = zk_metadata(\"2603110000\")\n",
        "#show: zettel.with(metadata: zk-metadata)\n",
        "\n",
        "= Central Note <2603110000>\n",
        "\n",
        "See @2603110002\n",
    );

    #[test]
    fn parse_header_requires_central_binding() {
        let h = parse_header(CENTRAL_NOTE).unwrap();
        assert_eq!(h.id, "2603110000");
        assert_eq!(h.title, "Central Note");
        assert_eq!(h.title_line_idx, 4);
    }

    #[test]
    fn parse_header_rejects_missing_or_mismatched_binding() {
        let missing = "#import \"../include.typ\": *\n\n= Note <2603110000>\n";
        let mismatched = concat!(
            "#let zk-metadata = zk_metadata(\"2603110001\")\n",
            "= Note <2603110000>\n",
        );
        assert!(parse_header(missing).is_none());
        assert!(parse_header(mismatched).is_none());
    }

    #[test]
    fn checklist_status_roundtrip() {
        for status in ChecklistStatus::ALL {
            assert_eq!(ChecklistStatus::from_str(status.as_str()), Some(status));
        }
    }

    #[test]
    fn find_all_refs_extracts_ids() {
        let refs = find_all_refs("see @2602082037 and @2602082106");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "2602082037");
        assert_eq!(refs[1].id, "2602082106");
    }

    #[test]
    fn byte_to_utf16_cjk() {
        let line = "Hello, world 你好 @2602171536";
        let refs = find_all_refs(line);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].start_char, 20);
        assert_eq!(byte_to_utf16(line, refs[0].start_char as usize), 16);
        assert_eq!(byte_to_utf16(line, refs[0].end_char as usize), 27);
    }

    #[test]
    fn ref_target_spans() {
        let line = "  - [ ] @1111111111 and @2222222222";
        let content = format!("{line}\n");
        let items = parse_checklist_items(&content);
        assert_eq!(items.len(), 1);
        if let ChecklistItemKind::Ref { targets } = &items[0].kind {
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0].target_id, "1111111111");
            assert_eq!(targets[0].byte_start, 8);
            assert_eq!(targets[0].byte_end, 19);
            assert_eq!(targets[1].target_id, "2222222222");
            assert_eq!(targets[1].byte_start, 24);
            assert_eq!(targets[1].byte_end, 35);
        } else {
            panic!("expected Ref kind");
        }
    }

    #[test]
    fn find_refs_skips_block_comment_and_fenced_block() {
        let content = concat!(
            "see @2602082037\n",
            "/* skip @9999999999 */\n",
            "```\n",
            "@8888888888\n",
            "```\n",
            "and @2602082106\n",
        );
        let refs = find_all_refs_filtered(content);
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"2602082037"));
        assert!(ids.contains(&"2602082106"));
        assert!(!ids.contains(&"9999999999"));
        assert!(!ids.contains(&"8888888888"));
    }

    #[test]
    fn refs_require_exactly_ten_ascii_digits() {
        assert!(find_all_refs("@12345678901").is_empty());
        assert!(find_all_refs("@１２３４５６７８９０").is_empty());
        assert_eq!(find_all_refs("@1234567890x")[0].id, "1234567890");
        assert!(parse_checklist_items("- [ ] @12345678901\n")[0]
            .kind
            .eq(&ChecklistItemKind::Local));
    }

    #[test]
    fn note_ref_at_position_uses_utf16_ranges() {
        let prefix = "中文😀 ";
        let content = format!("{prefix}@2602082037\n");
        let start = prefix.encode_utf16().count() as u32;
        let note_ref = note_ref_at_position(
            &content,
            Position {
                line: 0,
                character: start + 3,
            },
        )
        .unwrap();

        assert_eq!(note_ref.id, "2602082037");
        assert_eq!(note_ref.range.start.character, start);
        assert_eq!(note_ref.range.end.character, start + 11);
    }

    #[test]
    fn note_ref_at_position_rejects_hidden_and_overlong_refs() {
        let content = concat!(
            "/* @1111111111 */\n",
            "```\n",
            "@2222222222\n",
            "```\n",
            "@33333333333\n",
        );
        for line in [0, 2, 4] {
            assert!(note_ref_at_position(content, Position { line, character: 3 },).is_none());
        }
    }

    #[test]
    fn note_ref_completion_requires_visible_at_immediately_before_cursor() {
        let prefix = "中文😀 ";
        let content = format!("{prefix}@\n/* @ */\n```\n@\n```\n");
        let character = prefix.encode_utf16().count() as u32 + 1;
        let range = note_ref_completion_range(&content, Position { line: 0, character }).unwrap();
        assert_eq!(range.start.character, character);
        assert_eq!(range.start, range.end);
        assert!(note_ref_completion_range(
            &content,
            Position {
                line: 1,
                character: 4,
            },
        )
        .is_none());
        assert!(note_ref_completion_range(
            &content,
            Position {
                line: 3,
                character: 1,
            },
        )
        .is_none());
        assert!(note_ref_completion_range("see @1", Position::new(0, 6)).is_none());
    }

    #[test]
    fn note_identity_position_is_limited_to_binding_and_title() {
        assert_eq!(
            note_identity_at_position(CENTRAL_NOTE, Position::new(1, 40)).as_deref(),
            Some("2603110000")
        );
        assert_eq!(
            note_identity_at_position(CENTRAL_NOTE, Position::new(4, 17)).as_deref(),
            Some("2603110000")
        );
        assert!(note_identity_at_position(CENTRAL_NOTE, Position::new(6, 6)).is_none());
    }

    #[test]
    fn link_ids_skip_self_title_angle_id() {
        let ids = find_all_link_ids_filtered(CENTRAL_NOTE);
        assert_eq!(ids, vec!["2603110002"]);
    }
}

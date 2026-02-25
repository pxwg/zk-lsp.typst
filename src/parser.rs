/// Stateless parsing of Zettelkasten note headers and content.
use once_cell::sync::Lazy;
use regex::Regex;

static RE_ID_REF: Lazy<Regex> = Lazy::new(|| Regex::new(r"@(\d{10})").unwrap());
static RE_TITLE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^=\s+.*<(\d{10})>").unwrap());
static RE_EVO: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"#evolution_link\s*\(\s*<(\d{10})>\s*\)").unwrap());
static RE_ALT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"#alternative_link\s*\(\s*<(\d{10})>\s*\)").unwrap());

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
    pub tag_line_idx: usize,   // 0-based
    pub title_line_idx: usize, // 0-based
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

/// Parse the header of a note. Returns None if the import line cannot be found.
pub fn parse_header(content: &str) -> Option<NoteHeader> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the #import "../include.typ": * line (0-based index)
    let import_idx = lines
        .iter()
        .position(|l| l.trim() == r#"#import "../include.typ": *"#)?;

    let title_line_idx = import_idx + 3;
    let tag_line_idx = import_idx + 4;

    // Extract ID and title from title line
    let title_line = lines.get(title_line_idx)?;
    let id = RE_TITLE.captures(title_line)?.get(1)?.as_str().to_string();
    let title = RE_TITLE
        .captures(title_line)?
        .get(0)?
        .as_str()
        .trim_start_matches('=')
        .trim()
        .rsplit_once('<')
        .map(|(t, _)| t.trim().to_string())
        .unwrap_or_default();

    // Parse tag line
    let tag_line = lines.get(tag_line_idx).copied().unwrap_or("");
    let archived = tag_line.contains("#tag.archived");
    let legacy = tag_line.contains("#tag.legacy");

    // Parse evo/alt links from import_idx + 5
    let link_line = lines.get(import_idx + 5).copied().unwrap_or("");
    let evo_id = RE_EVO
        .captures(link_line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    let alt_id = RE_ALT
        .captures(link_line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());

    // Parse metadata block (/* Metadata: ... */) if present before import line
    let mut aliases = Vec::new();
    let mut abstract_text = None;
    let mut keywords = Vec::new();

    if import_idx > 0 {
        let mut in_metadata = false;
        for line in &lines[..import_idx] {
            if line.trim() == "/* Metadata:" {
                in_metadata = true;
                continue;
            }
            if line.trim() == "*/" {
                break;
            }
            if in_metadata {
                if let Some(val) = line.strip_prefix("Aliases:") {
                    aliases = val
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                } else if let Some(val) = line.strip_prefix("Abstract:") {
                    let t = val.trim().to_string();
                    if !t.is_empty() {
                        abstract_text = Some(t);
                    }
                } else if let Some(val) = line.strip_prefix("Keyword:") {
                    keywords = val
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
        }
    }

    Some(NoteHeader {
        id,
        title,
        archived,
        legacy,
        alt_id,
        evo_id,
        aliases,
        abstract_text,
        keywords,
        tag_line_idx,
        title_line_idx,
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
mod tests {
    use super::*;

    const NOTE_WITH_META: &str = r#"/* Metadata:
Aliases: ZK LSP
Abstract: A test note.
Keyword: test, rust
Generated: true
*/
#import "../include.typ": *
#show: zettel

= Test Note <2602082037>
#tag.archived #tag.done
#alternative_link(<2602131642>)

Some content here. @2602082135
"#;

    const NOTE_NO_META: &str = r#"#import "../include.typ": *
#show: zettel

= Simple Note <2602082106>
#tag.todo

Content. @2602082037
"#;

    #[test]
    fn test_parse_header_with_meta() {
        let h = parse_header(NOTE_WITH_META).unwrap();
        assert_eq!(h.id, "2602082037");
        assert_eq!(h.title, "Test Note");
        assert!(h.archived);
        assert!(!h.legacy);
        assert_eq!(h.alt_id.as_deref(), Some("2602131642"));
        assert_eq!(h.evo_id, None);
        assert_eq!(h.aliases, vec!["ZK LSP"]);
        assert_eq!(h.keywords, vec!["test", "rust"]);
        assert_eq!(h.title_line_idx, 9); // import at 6, +3
        assert_eq!(h.tag_line_idx, 10); // import at 6, +4
    }

    #[test]
    fn test_parse_header_no_meta() {
        let h = parse_header(NOTE_NO_META).unwrap();
        assert_eq!(h.id, "2602082106");
        assert_eq!(h.title, "Simple Note");
        assert!(!h.archived);
        assert_eq!(h.title_line_idx, 3); // import at 0, +3
        assert_eq!(h.tag_line_idx, 4);
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

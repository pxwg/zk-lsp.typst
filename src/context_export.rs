/// BFS context export for AI consumption.
///
/// `export_context` starts at a given note ID, traverses outgoing links up to
/// `depth` hops, and returns a structured Markdown document.
use std::collections::{HashSet, VecDeque};

use crate::config::WikiConfig;
use crate::parser::{self, ChecklistStatus, Relation};

/// Export a BFS context document starting from `entry_id` to the given `depth`.
///
/// Returns the Markdown string or an error if the entry note cannot be read.
pub async fn export_context(
    entry_id: &str,
    depth: usize,
    config: &WikiConfig,
) -> anyhow::Result<String> {
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    queue.push_back((entry_id.to_string(), 0));
    visited.insert(entry_id.to_string());

    let mut sections: Vec<NoteSection> = Vec::new();

    while let Some((id, d)) = queue.pop_front() {
        let path = config.note_dir.join(format!("{id}.typ"));
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        let header = parser::parse_header(&content);
        let title = header.as_ref().map(|h| h.title.clone()).unwrap_or_default();
        let abstract_text =
            header.as_ref().and_then(|h| h.abstract_text.clone()).unwrap_or_default();
        let keywords = header.as_ref().map(|h| h.keywords.clone()).unwrap_or_default();
        let checklist_status = header
            .as_ref()
            .and_then(|h| h.checklist_status.clone())
            .unwrap_or(ChecklistStatus::None);
        let relation = header
            .as_ref()
            .map(|h| if h.archived { Relation::Archived } else if h.legacy { Relation::Legacy } else { Relation::Active })
            .unwrap_or(Relation::Active);

        // Extract outgoing refs (filtered: skips TOML block, comments, fences)
        let out_refs: Vec<String> = parser::find_all_refs_filtered(&content)
            .into_iter()
            .map(|r| r.id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        // Enqueue unvisited neighbours if within depth
        if d < depth {
            for ref_id in &out_refs {
                if visited.insert(ref_id.clone()) {
                    queue.push_back((ref_id.clone(), d + 1));
                }
            }
        }

        // Extract body: lines from the title heading onwards
        let body = extract_body(&content, &id);

        let mut sorted_refs = out_refs.clone();
        sorted_refs.sort();

        sections.push(NoteSection {
            id: id.clone(),
            title,
            abstract_text,
            keywords,
            checklist_status,
            relation,
            out_refs: sorted_refs,
            body,
        });
    }

    let entry_title = sections.first().map(|s| s.title.as_str()).unwrap_or("");
    let today = chrono_today();

    let mut out = String::new();
    out.push_str("# ZK Context Export\n\n");
    out.push_str(&format!("**Entry:** {entry_id} — {entry_title}\n"));
    out.push_str(&format!("**Depth:** {depth}\n"));
    out.push_str(&format!("**Generated:** {today}\n\n"));
    out.push_str("---\n\n");

    for section in &sections {
        out.push_str(&format!("## {} · {}\n\n", section.id, section.title));

        if !section.abstract_text.is_empty() {
            out.push_str(&format!("> {}\n\n", section.abstract_text));
        }

        if !section.keywords.is_empty() {
            out.push_str(&format!("**Keywords:** {}\n", section.keywords.join(", ")));
        }

        let cs = match section.checklist_status {
            ChecklistStatus::None => "none",
            ChecklistStatus::Todo => "todo",
            ChecklistStatus::Wip => "wip",
            ChecklistStatus::Done => "done",
        };
        let rel = match section.relation {
            Relation::Active => "active",
            Relation::Archived => "archived",
            Relation::Legacy => "legacy",
        };
        out.push_str(&format!("**Status:** checklist={cs} · relation={rel}\n"));

        if !section.out_refs.is_empty() {
            let refs_str = section.out_refs.iter().map(|r| format!("@{r}")).collect::<Vec<_>>().join(", ");
            out.push_str(&format!("**Outgoing links:** {refs_str}\n"));
        }

        out.push('\n');

        if !section.body.is_empty() {
            out.push_str(&section.body);
            out.push('\n');
        }

        out.push_str("---\n\n");
    }

    Ok(out)
}

struct NoteSection {
    id: String,
    title: String,
    abstract_text: String,
    keywords: Vec<String>,
    checklist_status: ChecklistStatus,
    relation: Relation,
    out_refs: Vec<String>,
    body: String,
}

/// Extract the body of a note: everything from the title line (`= ... <id>`) onwards.
fn extract_body(content: &str, id: &str) -> String {
    let needle = format!("<{id}>");
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.contains(&needle)).unwrap_or(lines.len());
    lines[start..].join("\n")
}

fn chrono_today() -> String {
    // Use std::time to get a simple date string without a heavy dependency
    // Format: YYYY-MM-DD approximated from UNIX time
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple conversion: days since epoch
    let days = secs / 86400;
    // Zeller-style calculation
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WikiConfig;
    use std::path::Path;

    fn make_note_content(id: &str, title: &str, refs: &[&str]) -> String {
        let refs_body: String = refs.iter().map(|r| format!("- [ ] @{r}\n")).collect();
        format!(
            "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\"\"\"\n\
             ```toml\n\
             schema-version = 1\n\
             title = \"{title}\"\n\
             tags = []\n\
             checklist-status = \"none\"\n\
             relation = \"active\"\n\
             relation-target = []\n\
             generated = false\n\
             ```\n\
             \"\"\"))\n\
             #show: zettel\n\
             \n\
             = {title} <{id}>\n\
             {refs_body}"
        )
    }

    fn write_note(dir: &Path, id: &str, title: &str, refs: &[&str]) {
        let content = make_note_content(id, title, refs);
        std::fs::write(dir.join(format!("{id}.typ")), content).unwrap();
    }

    fn make_test_dir(suffix: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("zk_export_test_{suffix}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("note")).unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_export_single_note_no_depth() {
        let tmp = make_test_dir("single");
        write_note(&tmp.join("note"), "1111111111", "Entry Note", &[]);
        let config = WikiConfig::from_root(tmp.clone());
        let out = export_context("1111111111", 0, &config).await.unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(out.contains("1111111111"));
        assert!(out.contains("Entry Note"));
        assert!(!out.contains("2222222222"));
    }

    #[tokio::test]
    async fn test_export_bfs_depth() {
        let tmp = make_test_dir("bfs");
        write_note(&tmp.join("note"), "1111111111", "Entry Note", &["2222222222"]);
        write_note(&tmp.join("note"), "2222222222", "Linked Note", &[]);
        let config = WikiConfig::from_root(tmp.clone());
        let out = export_context("1111111111", 1, &config).await.unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(out.contains("1111111111"));
        assert!(out.contains("Entry Note"));
        assert!(out.contains("2222222222"));
        assert!(out.contains("Linked Note"));
    }
}

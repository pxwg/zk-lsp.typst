/// Graph integrity checks: dead link detection and orphan note detection.
///
/// `check_graph` scans the wiki directory and returns a `CheckReport`.
/// `render_check_report` formats it for CLI output (Typst-error style).
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::PathBuf;

use unicode_width::UnicodeWidthStr;

use crate::config::WikiConfig;
use crate::parser;

#[derive(Debug)]
pub struct DeadLinkEntry {
    pub from_id: String,
    pub from_path: PathBuf,
    pub to_id: String,
    pub line: usize, // 0-based
    pub byte_start: u32,
    pub byte_end: u32,
    pub line_text: String,
}

#[derive(Debug)]
pub struct OrphanEntry {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
}

#[derive(Debug)]
pub struct CheckReport {
    pub dead_links: Vec<DeadLinkEntry>,
    pub orphans: Vec<OrphanEntry>,
}

/// Scan the wiki and produce a `CheckReport` of dead links and orphan notes.
pub async fn check_graph(config: &WikiConfig) -> anyhow::Result<CheckReport> {
    let mut rd = tokio::fs::read_dir(&config.note_dir).await?;
    // notes: id → (path, content)
    let mut notes: HashMap<String, (PathBuf, String)> = HashMap::new();
    // titles: id → title string
    let mut titles: HashMap<String, String> = HashMap::new();

    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("typ") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if s.len() == 10 && s.chars().all(|c| c.is_ascii_digit()) => s.to_string(),
            _ => continue,
        };
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(header) = parser::parse_header(&content) {
            titles.insert(stem.clone(), header.title);
        }
        notes.insert(stem, (path, content));
    }

    let mut dead_links = Vec::new();
    let mut referenced_ids: HashSet<String> = HashSet::new();
    // notes that have at least one outgoing ref
    let mut has_outgoing: HashSet<String> = HashSet::new();

    for (from_id, (from_path, content)) in &notes {
        let lines: Vec<&str> = content.lines().collect();
        let refs = parser::find_all_refs_filtered(content);
        if !refs.is_empty() {
            has_outgoing.insert(from_id.clone());
        }
        for r in refs {
            referenced_ids.insert(r.id.clone());
            if !notes.contains_key(&r.id) {
                let line_text = lines
                    .get(r.line as usize)
                    .copied()
                    .unwrap_or("")
                    .to_string();
                dead_links.push(DeadLinkEntry {
                    from_id: from_id.clone(),
                    from_path: from_path.clone(),
                    to_id: r.id.clone(),
                    line: r.line as usize,
                    byte_start: r.start_char,
                    byte_end: r.end_char,
                    line_text,
                });
            }
        }
    }

    // Sort dead links for deterministic output
    dead_links.sort_by(|a, b| {
        a.from_id
            .cmp(&b.from_id)
            .then(a.line.cmp(&b.line))
            .then(a.byte_start.cmp(&b.byte_start))
    });

    // Orphan: no inbound references AND no outgoing references
    let mut orphans: Vec<OrphanEntry> = notes
        .iter()
        .filter(|(id, _)| !referenced_ids.contains(*id) && !has_outgoing.contains(*id))
        .map(|(id, (path, _))| OrphanEntry {
            id: id.clone(),
            path: path.clone(),
            title: titles.get(id).cloned().unwrap_or_default(),
        })
        .collect();
    orphans.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(CheckReport {
        dead_links,
        orphans,
    })
}

/// Render the check report in Typst-error style for CLI output (stdout).
pub fn render_check_report(report: &CheckReport) -> String {
    let color = std::io::stdout().is_terminal();
    let mut out = String::new();

    for entry in &report.dead_links {
        if color {
            out.push_str("\x1b[1;31merror\x1b[0m\x1b[1m: dead link reference\x1b[0m\n");
        } else {
            out.push_str("error: dead link reference\n");
        }

        let col = entry.byte_start + 1; // 1-based
        let path = entry.from_path.display();
        let line_1based = entry.line + 1;
        let lnum_w = format!("{line_1based}").len();
        let pad = " ".repeat(lnum_w);

        let prefix_bytes = entry.byte_start as usize;
        let display_col =
            display_width(&entry.line_text[..prefix_bytes.min(entry.line_text.len())]);
        let underline_disp = display_width(
            &entry.line_text[prefix_bytes..(entry.byte_end as usize).min(entry.line_text.len())],
        );
        let underline = "^".repeat(underline_disp.max(1));
        let pointer_spaces = " ".repeat(display_col);

        if color {
            out.push_str(&format!(
                " \x1b[1;34m{pad}┌─\x1b[0m \x1b[36m{path}:{line_1based}:{col}\x1b[0m\n"
            ));
            out.push_str(&format!(" \x1b[1;34m{pad}│\x1b[0m\n"));
            out.push_str(&format!(
                "\x1b[1;34m{line_1based:>lnum_w$} │\x1b[0m {}\n",
                entry.line_text
            ));
            out.push_str(&format!(
                " \x1b[1;34m{pad}│\x1b[0m {pointer_spaces}\x1b[1;31m{underline}\x1b[0m @{} does not exist\n",
                entry.to_id
            ));
        } else {
            out.push_str(&format!(" {pad}┌─ {path}:{line_1based}:{col}\n"));
            out.push_str(&format!(" {pad}│\n"));
            out.push_str(&format!("{line_1based:>lnum_w$} │ {}\n", entry.line_text));
            out.push_str(&format!(
                " {pad}│ {pointer_spaces}{underline} @{} does not exist\n",
                entry.to_id
            ));
        }
        out.push('\n');
    }

    for entry in &report.orphans {
        if color {
            out.push_str(
                "\x1b[1;33mwarning\x1b[0m\x1b[1m: orphan note (no inbound or outgoing references)\x1b[0m\n",
            );
            out.push_str(&format!(
                " \x1b[1;34m┌─\x1b[0m \x1b[36m{}\x1b[0m\n",
                entry.path.display()
            ));
            out.push_str(&format!(
                " \x1b[1;34m│\x1b[0m  {} — \"{}\"\n",
                entry.id, entry.title
            ));
        } else {
            out.push_str("warning: orphan note (no inbound or outgoing references)\n");
            out.push_str(&format!(" ┌─ {}\n", entry.path.display()));
            out.push_str(&format!(" │  {} — \"{}\"\n", entry.id, entry.title));
        }
        out.push('\n');
    }

    let dl = report.dead_links.len();
    let or = report.orphans.len();
    if dl > 0 || or > 0 {
        out.push_str(&format!("{dl} dead link(s), {or} orphan(s) found.\n"));
    } else {
        out.push_str("No dead links or orphans found.\n");
    }

    out
}

fn display_width(s: &str) -> usize {
    s.width()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_notes(pairs: &[(&str, &str)]) -> HashMap<String, (PathBuf, String)> {
        pairs
            .iter()
            .map(|(id, content)| {
                (
                    id.to_string(),
                    (
                        PathBuf::from(format!("/wiki/note/{id}.typ")),
                        content.to_string(),
                    ),
                )
            })
            .collect()
    }

    fn build_report(notes: &HashMap<String, (PathBuf, String)>) -> CheckReport {
        let mut dead_links = Vec::new();
        let mut referenced_ids: HashSet<String> = HashSet::new();
        let mut has_outgoing: HashSet<String> = HashSet::new();

        for (from_id, (from_path, content)) in notes {
            let lines: Vec<&str> = content.lines().collect();
            let refs = parser::find_all_refs_filtered(content);
            if !refs.is_empty() {
                has_outgoing.insert(from_id.clone());
            }
            for r in refs {
                referenced_ids.insert(r.id.clone());
                if !notes.contains_key(&r.id) {
                    let line_text = lines
                        .get(r.line as usize)
                        .copied()
                        .unwrap_or("")
                        .to_string();
                    dead_links.push(DeadLinkEntry {
                        from_id: from_id.clone(),
                        from_path: from_path.clone(),
                        to_id: r.id.clone(),
                        line: r.line as usize,
                        byte_start: r.start_char,
                        byte_end: r.end_char,
                        line_text,
                    });
                }
            }
        }

        let mut orphans: Vec<OrphanEntry> = notes
            .iter()
            .filter(|(id, _)| !referenced_ids.contains(*id) && !has_outgoing.contains(*id))
            .map(|(id, (path, _))| OrphanEntry {
                id: id.clone(),
                path: path.clone(),
                title: String::new(),
            })
            .collect();
        orphans.sort_by(|a, b| a.id.cmp(&b.id));

        CheckReport {
            dead_links,
            orphans,
        }
    }

    #[test]
    fn test_check_detects_dead_link() {
        // A references B, but B doesn't exist
        let notes = make_notes(&[("1111111111", "- [ ] @2222222222\n")]);
        let report = build_report(&notes);
        assert_eq!(report.dead_links.len(), 1);
        assert_eq!(report.dead_links[0].to_id, "2222222222");
    }

    #[test]
    fn test_check_no_false_dead_link() {
        // A references B, B exists → no dead links
        let notes = make_notes(&[("1111111111", "- [ ] @2222222222\n"), ("2222222222", "")]);
        let report = build_report(&notes);
        assert!(report.dead_links.is_empty());
    }

    #[test]
    fn test_check_detects_orphan() {
        // Both notes have no inbound or outgoing refs → both orphans
        let notes = make_notes(&[
            ("1111111111", "no refs here\n"),
            ("2222222222", "no refs here\n"),
        ]);
        let report = build_report(&notes);
        assert_eq!(report.orphans.len(), 2);
    }

    #[test]
    fn test_check_no_false_orphan_inbound() {
        // A references B → B has inbound link, not orphan
        // A has outgoing link, not orphan
        let notes = make_notes(&[("1111111111", "- [ ] @2222222222\n"), ("2222222222", "")]);
        let report = build_report(&notes);
        // neither is an orphan: 1111111111 has outgoing, 2222222222 has inbound
        assert!(report.orphans.is_empty());
    }

    #[test]
    fn test_check_orphan_outgoing_only_not_orphan() {
        // A references nobody, B references A → A has inbound (not orphan), B has outgoing (not orphan)
        let notes = make_notes(&[
            ("1111111111", "no refs\n"),
            ("2222222222", "- [ ] @1111111111\n"),
        ]);
        let report = build_report(&notes);
        assert!(report.orphans.is_empty());
    }
}

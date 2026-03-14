/// Migration from legacy comment-format notes to TOML schema v1.
///
/// Legacy format:
///   /* Metadata:          ← optional 6-line block
///   Aliases: ...
///   Abstract: ...
///   Keyword: ...
///   Generated: true
///   */
///   #import "../include.typ": *
///   #show: zettel
///                         ← blank
///   = Title <YYMMDDHHMM>
///   #tag.xxx              ← status/relation tags + any user tags
///   #evolution_link(...)  ← optional link line
///
/// New format (schema-version = 1):
///   #import "../include.typ": *
///   #let zk-metadata = toml(bytes(
///     ```toml
///     schema-version = 1
///     aliases = [...]
///     abstract = "..."
///     keywords = [...]
///     generated = true
///     checklist-status = "none|todo|wip|done"
///     relation = "active|archived|legacy"
///     relation-target = [...]
///     ```.text,
///   ))
///   #show: zettel.with(metadata: zk-metadata)
///
///   = Title <YYMMDDHHMM>
///   #tag.custom           ← non-status/relation tags preserved here
///   <body content>
use anyhow::Result;
use tokio::fs;

use crate::config::WikiConfig;
use crate::parser::{find_toml_metadata_block, RE_ALT, RE_EVO, RE_TITLE};

pub struct MigrateStats {
    pub migrated: usize,
    pub already_current: usize,
    pub skipped: usize,
}

/// Migrate all legacy notes in `config.note_dir` to TOML schema v1 in-place.
pub async fn migrate_wiki(config: &WikiConfig) -> Result<MigrateStats> {
    let mut stats = MigrateStats {
        migrated: 0,
        already_current: 0,
        skipped: 0,
    };

    let mut entries = fs::read_dir(&config.note_dir).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("typ") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem.len() == 10 && stem.chars().all(|c| c.is_ascii_digit()) {
                    paths.push(path);
                }
            }
        }
    }

    for path in &paths {
        let content = match fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  skip {}: {e}", path.display());
                stats.skipped += 1;
                continue;
            }
        };

        if find_toml_metadata_block(&content).is_some() {
            stats.already_current += 1;
            continue;
        }

        match migrate_note(&content) {
            Some(new_content) => {
                // Atomic write via tmp → rename
                let tmp = path.with_extension("typ.migrate_tmp");
                fs::write(&tmp, &new_content).await?;
                fs::rename(&tmp, path).await?;
                eprintln!("  migrated: {}", path.display());
                stats.migrated += 1;
            }
            None => {
                eprintln!("  skip (unrecognised format): {}", path.display());
                stats.skipped += 1;
            }
        }
    }

    Ok(stats)
}

/// Convert a single legacy note to TOML schema v1.
/// Returns `None` if the content does not look like a legacy note.
pub fn migrate_note(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Locate the #import line — mandatory for both legacy variants.
    let import_idx = lines
        .iter()
        .position(|l| l.trim() == r#"#import "../include.typ": *"#)?;

    // ── Parse legacy comment metadata (optional block before import) ──────
    let mut aliases: Vec<String> = Vec::new();
    let mut abstract_text = String::new();
    let mut keywords: Vec<String> = Vec::new();

    if import_idx > 0 {
        let mut in_meta = false;
        for line in &lines[..import_idx] {
            if line.trim() == "/* Metadata:" {
                in_meta = true;
                continue;
            }
            if line.trim() == "*/" {
                break;
            }
            if in_meta {
                if let Some(val) = line.strip_prefix("Aliases:") {
                    aliases = val
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                } else if let Some(val) = line.strip_prefix("Abstract:") {
                    abstract_text = val.trim().to_string();
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

    // ── Fixed-offset header lines ─────────────────────────────────────────
    // import_idx + 1 : #show: zettel
    // import_idx + 2 : (blank)
    // import_idx + 3 : = Title <ID>
    // import_idx + 4 : #tag.xxx  (status / relation tags + user tags)
    // import_idx + 5 : #evolution_link / #alternative_link  (optional)

    let title_line_idx = import_idx + 3;
    let tag_line_idx = import_idx + 4;
    let link_line_idx = import_idx + 5;

    let title_line = lines.get(title_line_idx)?;
    // Reject if this line doesn't look like a Typst heading with an ID.
    RE_TITLE.captures(title_line)?;

    // ── Tag line ──────────────────────────────────────────────────────────
    let tag_line = lines.get(tag_line_idx).copied().unwrap_or("");
    let is_archived = tag_line.contains("#tag.archived");
    let is_legacy = tag_line.contains("#tag.legacy");
    let checklist_status = if tag_line.contains("#tag.done") {
        "done"
    } else if tag_line.contains("#tag.wip") {
        "wip"
    } else if tag_line.contains("#tag.todo") {
        "todo"
    } else {
        "none"
    };

    // Strip the status/relation tags; preserve everything else.
    let remaining_tags = strip_status_tags(tag_line);

    // ── Optional link line ────────────────────────────────────────────────
    let link_line = lines.get(link_line_idx).copied().unwrap_or("");
    let alt_id = RE_ALT
        .captures(link_line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    let evo_id = RE_EVO
        .captures(link_line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    let has_link_line = alt_id.is_some()
        || evo_id.is_some()
        || link_line.trim_start().starts_with("#evolution_link")
        || link_line.trim_start().starts_with("#alternative_link");

    // ── Derive TOML relation fields ───────────────────────────────────────
    let (relation, relation_target_ids): (&str, Vec<String>) = if is_archived {
        ("archived", alt_id.into_iter().collect())
    } else if is_legacy {
        ("legacy", evo_id.into_iter().collect())
    } else {
        ("active", Vec::new())
    };

    // ── Body = everything after the header lines ──────────────────────────
    let body_start = if has_link_line {
        link_line_idx + 1
    } else {
        tag_line_idx + 1
    };
    let body_lines = lines.get(body_start..).unwrap_or(&[]);

    // ── Assemble new content ──────────────────────────────────────────────
    let mut out = String::with_capacity(content.len() + 256);

    out.push_str("#import \"../include.typ\": *\n");
    out.push_str("#let zk-metadata = toml(bytes(\n");
    out.push_str("  ```toml\n");
    out.push_str("  schema-version = 1\n");
    out.push_str(&format!("  aliases = {}\n", toml_string_array(&aliases)));
    out.push_str(&format!("  abstract = {}\n", toml_quoted(&abstract_text)));
    out.push_str(&format!("  keywords = {}\n", toml_string_array(&keywords)));
    out.push_str("  generated = true\n");
    out.push_str(&format!(
        "  checklist-status = {}\n",
        toml_quoted(checklist_status)
    ));
    out.push_str(&format!("  relation = {}\n", toml_quoted(relation)));
    out.push_str(&format!(
        "  relation-target = {}\n",
        toml_string_array(&relation_target_ids)
    ));
    out.push_str("  ```.text,\n");
    out.push_str("))\n");
    out.push_str("#show: zettel.with(metadata: zk-metadata)\n");
    out.push('\n');
    out.push_str(title_line);
    out.push('\n');

    // Emit remaining user tags (if any) right after the title.
    if !remaining_tags.is_empty() {
        out.push_str(&remaining_tags);
        out.push('\n');
    }

    for line in body_lines {
        out.push_str(line);
        out.push('\n');
    }

    Some(out)
}

/// Remove status/relation tags from a tag line, preserving all other tags.
///
/// Stripped: `#tag.archived`, `#tag.legacy`, `#tag.todo`, `#tag.done`, `#tag.wip`
fn strip_status_tags(tag_line: &str) -> String {
    const STATUS_TAGS: &[&str] = &[
        "#tag.archived",
        "#tag.legacy",
        "#tag.todo",
        "#tag.done",
        "#tag.wip",
    ];
    let mut result = tag_line.to_string();
    for tag in STATUS_TAGS {
        result = result.replace(tag, "");
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── TOML serialisation helpers ────────────────────────────────────────────────

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn toml_quoted(s: &str) -> String {
    format!("\"{}\"", toml_escape(s))
}

fn toml_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let inner: Vec<String> = items.iter().map(|s| toml_quoted(s)).collect();
    format!("[{}]", inner.join(", "))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::tests::{NOTE_NO_META, NOTE_WITH_META};
    use crate::parser::{find_toml_metadata_block, parse_header, parse_toml_metadata};

    #[test]
    fn migrate_with_meta_round_trips() {
        let migrated = migrate_note(NOTE_WITH_META).expect("migration failed");

        // Must parse as a valid TOML-format note.
        let header = parse_header(&migrated).expect("migrated note not parseable");
        assert_eq!(header.id, "2602082037");
        assert_eq!(header.title, "Test Note");
        assert!(header.archived);
        assert!(!header.legacy);
        assert_eq!(header.alt_id.as_deref(), Some("2602131642"));
        assert_eq!(header.aliases, vec!["ZK LSP"]);
        assert_eq!(header.keywords, vec!["test", "rust"]);
        assert_eq!(header.abstract_text.as_deref(), Some("A test note."));

        // checklist-status migrated from #tag.done
        let block = find_toml_metadata_block(&migrated).unwrap();
        let parsed = parse_toml_metadata(&block.toml_content).unwrap();
        assert_eq!(
            parsed.checklist_status,
            crate::parser::ChecklistStatus::Done
        );

        // Body content is preserved.
        assert!(migrated.contains("Some content here. @2602082135"));
        // #show: zettel.with(metadata: zk-metadata) is present.
        assert!(migrated.contains("#show: zettel.with(metadata: zk-metadata)"));
        // Old header artefacts are gone.
        assert!(!migrated.contains("/* Metadata:"));
        assert!(!migrated.contains("#tag.archived"));
        assert!(!migrated.contains("#tag.done"));
        assert!(!migrated.contains("#alternative_link"));
    }

    #[test]
    fn migrate_no_meta_round_trips() {
        let migrated = migrate_note(NOTE_NO_META).expect("migration failed");

        let header = parse_header(&migrated).expect("migrated note not parseable");
        assert_eq!(header.id, "2602082106");
        assert_eq!(header.title, "Simple Note");
        assert!(!header.archived);
        assert!(header.aliases.is_empty());
        assert_eq!(header.abstract_text, None);

        let block = find_toml_metadata_block(&migrated).unwrap();
        let parsed = parse_toml_metadata(&block.toml_content).unwrap();
        assert_eq!(
            parsed.checklist_status,
            crate::parser::ChecklistStatus::Todo
        );
        assert_eq!(parsed.relation, crate::parser::Relation::Active);

        assert!(migrated.contains("Content. @2602082037"));
        assert!(migrated.contains("#show: zettel.with(metadata: zk-metadata)"));
        assert!(!migrated.contains("#tag.todo"));
    }

    #[test]
    fn migrate_preserves_non_status_tags() {
        let note = concat!(
            "#import \"../include.typ\": *\n",
            "#show: zettel\n",
            "\n",
            "= Research Note <2603110099>\n",
            "#tag.archived #tag.done #tag.research #tag.physics\n",
            "#alternative_link(<2603110001>)\n",
            "\n",
            "Body.\n",
        );
        let migrated = migrate_note(note).expect("migration failed");

        // Status/relation tags stripped.
        assert!(!migrated.contains("#tag.archived"));
        assert!(!migrated.contains("#tag.done"));
        // User tags preserved.
        assert!(migrated.contains("#tag.research"));
        assert!(migrated.contains("#tag.physics"));
        // They appear after the title line.
        let title_pos = migrated.find("= Research Note").unwrap();
        let research_pos = migrated.find("#tag.research").unwrap();
        assert!(research_pos > title_pos);
    }

    #[test]
    fn migrate_already_toml_skipped_by_caller() {
        let toml_note = concat!(
            "#import \"../include.typ\": *\n",
            "#let zk-metadata = toml(bytes(\n",
            "  ```toml\n",
            "  schema-version = 1\n",
            "  aliases = []\n",
            "  abstract = \"\"\n",
            "  keywords = []\n",
            "  generated = true\n",
            "  checklist-status = \"none\"\n",
            "  relation = \"active\"\n",
            "  relation-target = []\n",
            "  ```.text,\n",
            "))\n",
            "#show: zettel.with(metadata: zk-metadata)\n",
            "\n",
            "= New Note <2603110000>\n",
        );
        // The caller checks for an existing TOML block and skips migration.
        assert!(find_toml_metadata_block(toml_note).is_some());
    }

    #[test]
    fn strip_status_tags_only_removes_known_tags() {
        assert_eq!(
            strip_status_tags("#tag.archived #tag.done #tag.research"),
            "#tag.research"
        );
        assert_eq!(strip_status_tags("#tag.todo"), "");
        assert_eq!(strip_status_tags("#tag.wip #tag.physics"), "#tag.physics");
        assert_eq!(strip_status_tags("#tag.legacy"), "");
    }

    #[test]
    fn toml_escape_special_chars() {
        assert_eq!(toml_quoted(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(toml_quoted(r"a\b"), r#""a\\b""#);
    }
}

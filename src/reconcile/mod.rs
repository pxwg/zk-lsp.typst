/// Reconcile DSL v1 — workspace-wide checklist reconciliation.
///
/// Public API: `run_reconcile` and `ReconcileStats`.
///
/// Replaces the previous single-file `src/reconcile.rs` with a layered architecture:
/// types → ast → default_module → parser → typecheck → observe → eval → materialize
pub mod ast;
pub mod default_module;
pub mod eval;
pub mod materialize;
pub mod observe;
pub mod parser;
pub mod typecheck;
pub mod types;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::config::WikiConfig;
use crate::handlers::formatting::normalize_note_from_checked;

use self::default_module::DEFAULT_MODULE;
use self::eval::eval_all;
use self::materialize::materialize;
use self::observe::WorkspaceSnapshot;
use self::types::NoteId;

// ---------------------------------------------------------------------------
// Public stats
// ---------------------------------------------------------------------------

pub struct ReconcileStats {
    pub files_changed: usize,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run_reconcile(config: &WikiConfig, dry_run: bool) -> Result<ReconcileStats> {
    // 1. Scan notes
    let notes = scan_notes(&config.note_dir).await?;

    // 2. Build workspace snapshot from already-scanned notes (avoids double I/O).
    //    Cycle detection is handled by the evaluator via visiting_meta / CyclePolicy.
    let snapshot = WorkspaceSnapshot::from_note_map(&notes);

    // 3. Parse + type-check DEFAULT_MODULE (panics on failure — compile-time invariant)
    let module = parser::parse_module(DEFAULT_MODULE)
        .expect("DEFAULT_MODULE must always parse successfully");
    typecheck::type_check_module(&module)
        .expect("DEFAULT_MODULE must always typecheck successfully");

    // 4. Evaluate
    let eval_result = eval_all(&module, &snapshot);

    // 5. Materialize
    let reconcile_result = materialize(eval_result);

    // 5b. Report diagnostics
    for diag in &reconcile_result.diagnostics {
        tracing::warn!(
            note = %diag.note_id,
            kind = ?diag.kind,
            "reconcile diagnostic: {}",
            diag.message
        );
    }

    // 6. Write-back: for each note, apply pre-evaluated checkbox truth and write atomically if changed
    let mut files_changed = 0usize;
    for (_id, (path, content)) in &notes {
        let checked_by_line: HashMap<usize, bool> = reconcile_result
            .materialized_checked
            .iter()
            .filter(|(cid, _)| cid.note_id == *_id)
            .map(|(cid, checked)| (cid.line_idx, *checked))
            .collect();

        let new_content = normalize_note_from_checked(content, &checked_by_line);
        if new_content != *content {
            files_changed += 1;
            if !dry_run {
                // Atomic write: tmp → rename
                let tmp = path.with_extension("typ.tmp");
                tokio::fs::write(&tmp, new_content.as_bytes()).await?;
                tokio::fs::rename(&tmp, path).await?;
            } else {
                eprintln!("  would update: {}", path.display());
            }
        }
    }

    Ok(ReconcileStats { files_changed })
}

// ---------------------------------------------------------------------------
// Scan helper
// ---------------------------------------------------------------------------

async fn scan_notes(note_dir: &std::path::Path) -> Result<HashMap<NoteId, (PathBuf, String)>> {
    let mut map = HashMap::new();
    let mut rd = tokio::fs::read_dir(note_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("typ") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if stem.len() != 10 || !stem.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                map.insert(stem, (path, content));
            }
            Err(_) => continue,
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::reconcile::eval::eval_all;
    use crate::reconcile::observe::WorkspaceSnapshot;
    use crate::reconcile::parser::parse_module;
    use crate::reconcile::types::{DiagnosticKind, Status};

    fn make_toml_note(title: &str, id: &str, status: &str, body: &str) -> String {
        format!(
            "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\n\
             \x20 ```toml\n\
             \x20 schema-version = 1\n\
             \x20 title = \"{title}\"\n\
             \x20 tags = []\n\
             \x20 checklist-status = \"{status}\"\n\
             \x20 generated = false\n\
             \x20 ```.text,\n\
             ))\n\
             #show: zettel.with(metadata: zk-metadata)\n\
             \n\
             = {title} <{id}>\n\
             {body}"
        )
    }

    fn snapshot_from(notes: &[(&str, &str)]) -> WorkspaceSnapshot {
        let map: HashMap<NoteId, (PathBuf, String)> = notes
            .iter()
            .map(|(id, content)| {
                (
                    id.to_string(),
                    (PathBuf::from(format!("{id}.typ")), content.to_string()),
                )
            })
            .collect();
        WorkspaceSnapshot::from_note_map(&map)
    }

    #[test]
    fn full_pipeline_multi_note_dag() {
        // A done → B refs A → C refs B; expect all Done
        let a = make_toml_note("A", "1010101010", "done", "");
        let b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");

        let snap = snapshot_from(&[("1010101010", &a), ("2020202020", &b), ("3030303030", &c)]);
        let module = parse_module(DEFAULT_MODULE).expect("parse");
        let eval_result = eval_all(&module, &snap);
        let result = materialize(eval_result);

        assert_eq!(
            result.materialized_status.get("1010101010"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.materialized_status.get("2020202020"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.materialized_status.get("3030303030"),
            Some(&Status::Done)
        );
    }

    // -----------------------------------------------------------------------
    // Migrated tests from old reconcile.rs
    // -----------------------------------------------------------------------

    #[test]
    fn refitem_rendered_checked_not_source_truth() {
        use crate::handlers::formatting::is_note_done_with_deps;
        let content = "- [x] @2222222222\n";
        let deps = HashMap::from([("2222222222".to_string(), false)]);
        assert!(
            !is_note_done_with_deps(content, &deps),
            "rendered [x] on RefItem must not override semantic truth from dep_states"
        );
    }

    #[test]
    fn refitem_drives_note_status() {
        use crate::handlers::formatting::is_note_done_with_deps;
        let content_a = make_toml_note(
            "A",
            "1111111111",
            "none",
            "- [x] local task\n- [ ] @2222222222\n",
        );

        let deps_b_not_done = HashMap::from([("2222222222".to_string(), false)]);
        assert!(
            !is_note_done_with_deps(&content_a, &deps_b_not_done),
            "A not done when B is not done, despite local task done"
        );

        let deps_b_done = HashMap::from([("2222222222".to_string(), true)]);
        assert!(
            is_note_done_with_deps(&content_a, &deps_b_done),
            "A done when all leaf items (local + ref) are satisfied"
        );
    }

    #[test]
    fn normalize_note_is_local_only() {
        use crate::handlers::formatting::normalize_note;
        let content = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let deps = HashMap::from([("2222222222".to_string(), true)]);
        let result = normalize_note(&content, &deps);
        assert!(result.contains("- [x]"), "ref checkbox updated");
        assert!(result.contains("<1111111111>"), "still note A's content");
    }

    #[test]
    fn normalize_note_is_pure() {
        use crate::handlers::formatting::normalize_note;
        let content = "- [ ] @1234567890 do thing\n";
        let mut dep_states = HashMap::new();
        dep_states.insert("1234567890".to_string(), true);
        let result = normalize_note(content, &dep_states);
        assert!(result.contains("- [x]"), "checkbox should be checked");
    }

    #[test]
    fn chain_propagation() {
        use crate::handlers::formatting::{is_note_done, normalize_note};
        let content_a = make_toml_note("A", "1010101010", "done", "");
        let content_b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let content_c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");

        let dep_a: HashMap<String, bool> = HashMap::new();
        let normalized_a = normalize_note(&content_a, &dep_a);
        assert!(is_note_done(&normalized_a), "A should be done");

        let mut dep_a_done = HashMap::new();
        dep_a_done.insert("1010101010".to_string(), true);
        let normalized_b = normalize_note(&content_b, &dep_a_done);
        assert!(
            normalized_b.contains("- [x]"),
            "B's ref to A should be checked"
        );
        let b_done = is_note_done(&normalized_b);

        let mut dep_b = HashMap::new();
        dep_b.insert("2020202020".to_string(), b_done);
        let normalized_c = normalize_note(&content_c, &dep_b);
        if b_done {
            assert!(
                normalized_c.contains("- [x]"),
                "C's ref to B should be checked when B is done"
            );
        }
    }

    #[test]
    fn multi_ref_item_requires_all_done() {
        use crate::handlers::formatting::is_note_done_with_deps;
        let content = make_toml_note("X", "3333333333", "none", "- [ ] @1111111111 @2222222222\n");

        let deps_both = HashMap::from([
            ("1111111111".to_string(), true),
            ("2222222222".to_string(), true),
        ]);
        assert!(
            is_note_done_with_deps(&content, &deps_both),
            "done when all refs done"
        );

        let deps_one = HashMap::from([
            ("1111111111".to_string(), true),
            ("2222222222".to_string(), false),
        ]);
        assert!(
            !is_note_done_with_deps(&content, &deps_one),
            "not done when one ref is not done"
        );
    }

    #[test]
    fn no_checklist_note_uses_metadata_status() {
        use crate::handlers::formatting::is_note_done_with_deps;
        let content_done = make_toml_note("D", "4444444444", "done", "");
        let content_none = make_toml_note("N", "5555555555", "none", "");
        assert!(is_note_done_with_deps(&content_done, &HashMap::new()));
        assert!(!is_note_done_with_deps(&content_none, &HashMap::new()));
    }

    #[test]
    fn cycle_error_produces_diagnostic() {
        let a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let b = make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n");
        let snap = snapshot_from(&[("1111111111", &a), ("2222222222", &b)]);
        let module = parse_module(DEFAULT_MODULE).expect("parse");
        let result = eval_all(&module, &snap);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::Cycle),
            "cycle diagnostic emitted"
        );
    }
}

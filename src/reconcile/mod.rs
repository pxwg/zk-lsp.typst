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
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use unicode_width::UnicodeWidthStr;

use crate::config::WikiConfig;
use crate::cycle;
use crate::dependency_graph;
use crate::handlers::formatting::{apply_metadata_patch, normalize_note_from_checked};
use crate::parser as note_parser;

use self::default_module::load_module;
use self::eval::eval_all;
use self::materialize::materialize;
use self::observe::WorkspaceSnapshot;
use self::types::{
    DiagnosticKind, DiagnosticLocation, DiagnosticSeverity, NoteId, ReconcileDiagnostic, Value,
};

// ---------------------------------------------------------------------------
// Public stats
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReconcileStats {
    pub files_changed: usize,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run_reconcile(config: &WikiConfig, dry_run: bool) -> Result<ReconcileStats> {
    let notes = scan_notes(&config.note_dir).await?;
    let (eval_result, diagnostics) = evaluate_workspace(&notes, config)?;

    if !diagnostics.is_empty() {
        return Err(anyhow::anyhow!(render_diagnostics(&diagnostics, &notes)));
    }

    let reconcile_result = materialize(eval_result);
    let mut files_changed = 0usize;
    for (_id, (path, content)) in &notes {
        let checked_by_line: HashMap<usize, bool> = reconcile_result
            .materialized_checked
            .iter()
            .filter(|(cid, _)| cid.note_id == *_id)
            .map(|(cid, checked)| (cid.line_idx, *checked))
            .collect();

        let after_checked = normalize_note_from_checked(content, &checked_by_line);
        let new_content = apply_materialized_status(&_id, &after_checked, &reconcile_result)
            .unwrap_or_else(|| after_checked.clone());
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

pub async fn collect_diagnostics(
    config: &WikiConfig,
    overlay: Option<(&std::path::Path, &str)>,
) -> Result<Vec<ReconcileDiagnostic>> {
    let mut notes = scan_notes(&config.note_dir).await?;

    if let Some((path, content)) = overlay {
        if path.extension().and_then(|ext| ext.to_str()) == Some("typ") {
            if let Some(note_id) = path.file_stem().and_then(|stem| stem.to_str()) {
                if note_id.len() == 10 && note_id.chars().all(|c| c.is_ascii_digit()) {
                    notes.insert(
                        note_id.to_string(),
                        (path.to_path_buf(), content.to_string()),
                    );
                }
            }
        }
    }

    let (_, diagnostics) = evaluate_workspace(&notes, config)?;
    Ok(diagnostics)
}

fn apply_materialized_status(
    note_id: &str,
    content: &str,
    reconcile_result: &materialize::ReconcileResult,
) -> Option<String> {
    let status = reconcile_result
        .materialized_meta
        .get(&(note_id.to_string(), "checklist-status".to_string()))?;

    let Value::Status(status) = status else {
        return None;
    };

    let mut patch = HashMap::new();
    patch.insert(
        "checklist-status".to_string(),
        toml::Value::String(status.to_str().to_string()),
    );
    apply_metadata_patch(content, &patch).ok()
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

fn evaluate_workspace(
    notes: &HashMap<NoteId, (PathBuf, String)>,
    config: &WikiConfig,
) -> Result<(eval::EvalResult, Vec<ReconcileDiagnostic>)> {
    let snapshot =
        WorkspaceSnapshot::from_note_map_with_metadata(notes, &config.zk_config.metadata.fields);
    let module = load_module(
        &config.zk_config.reconcile_rules,
        config.zk_config.disable_default_reconcile_rules,
    )?;
    typecheck::type_check_module_with_metadata(&module, &config.zk_config.metadata.fields)
        .map_err(anyhow::Error::msg)?;

    let eval_result = eval_all(&module, &snapshot);
    let diagnostics = build_diagnostics(notes, eval_result.diagnostics.clone());
    Ok((eval_result, diagnostics))
}

fn build_diagnostics(
    notes: &HashMap<NoteId, (PathBuf, String)>,
    eval_diagnostics: Vec<ReconcileDiagnostic>,
) -> Vec<ReconcileDiagnostic> {
    let graph = dependency_graph::build_dependency_graph(notes);
    let cycles = cycle::detect_cycles(&graph);
    let mut diagnostics = cycle_diagnostics(&cycles);
    diagnostics.extend(non_leaf_ref_diagnostics(notes));
    diagnostics.extend(located_eval_diagnostics(eval_diagnostics, notes));
    diagnostics
}

fn cycle_diagnostics(cycles: &[cycle::DependencyCycle]) -> Vec<ReconcileDiagnostic> {
    let mut diagnostics = Vec::new();
    for cycle in cycles {
        for edge in &cycle.edges {
            diagnostics.push(ReconcileDiagnostic {
                note_id: edge.from_note_id.clone(),
                message: format!(
                    "Cyclic task dependency: {} -> ... -> {}",
                    edge.from_note_id, edge.from_note_id
                ),
                kind: DiagnosticKind::Cycle,
                severity: DiagnosticSeverity::Error,
                location: Some(DiagnosticLocation {
                    file_path: edge.file_path.clone(),
                    line: edge.line,
                    byte_start: edge.byte_start,
                    byte_end: edge.byte_end,
                }),
            });
        }
    }
    diagnostics
}

fn non_leaf_ref_diagnostics(
    notes: &HashMap<NoteId, (PathBuf, String)>,
) -> Vec<ReconcileDiagnostic> {
    let mut diagnostics = Vec::new();

    for (note_id, (path, content)) in notes {
        let items = note_parser::parse_checklist_items(content);
        let lines: Vec<&str> = content.lines().collect();

        for (i, item) in items.iter().enumerate() {
            let note_parser::ChecklistItemKind::Ref { targets } = &item.kind else {
                continue;
            };
            let is_non_leaf = i + 1 < items.len() && items[i + 1].indent > item.indent;
            if !is_non_leaf {
                continue;
            }

            let line_text = lines.get(item.line_idx).copied().unwrap_or("");
            let (start_byte, end_byte) = targets
                .first()
                .zip(targets.last())
                .map(|(first, last)| (first.byte_start, last.byte_end))
                .unwrap_or((0, line_text.len() as u32));

            diagnostics.push(ReconcileDiagnostic {
                note_id: note_id.clone(),
                message: "Ref item has child items; @ID targets will be semantically ignored (only leaf items are source facts)".to_string(),
                kind: DiagnosticKind::NonLeafRef,
                severity: DiagnosticSeverity::Error,
                location: Some(DiagnosticLocation {
                    file_path: path.clone(),
                    line: item.line_idx,
                    byte_start: start_byte,
                    byte_end: end_byte,
                }),
            });
        }
    }

    diagnostics
}

fn located_eval_diagnostics(
    diagnostics: Vec<ReconcileDiagnostic>,
    notes: &HashMap<NoteId, (PathBuf, String)>,
) -> Vec<ReconcileDiagnostic> {
    diagnostics
        .into_iter()
        .filter(|diag| diag.kind != DiagnosticKind::Cycle)
        .map(|mut diag| {
            if diag.location.is_none() {
                if let Some((path, _)) = notes.get(&diag.note_id) {
                    diag.location = Some(DiagnosticLocation {
                        file_path: path.clone(),
                        line: 0,
                        byte_start: 0,
                        byte_end: 0,
                    });
                }
            }
            diag
        })
        .collect()
}

fn render_diagnostics(
    diagnostics: &[ReconcileDiagnostic],
    notes: &HashMap<NoteId, (PathBuf, String)>,
) -> String {
    let color = std::io::stderr().is_terminal();
    let mut out = String::new();

    for diag in diagnostics {
        if color {
            out.push_str("\x1b[1;31merror\x1b[0m");
            out.push_str("\x1b[1m: ");
            out.push_str(&diag.message);
            out.push_str("\x1b[0m\n");
        } else {
            out.push_str("error: ");
            out.push_str(&diag.message);
            out.push('\n');
        }

        let Some(location) = &diag.location else {
            out.push('\n');
            continue;
        };

        let line_1based = location.line + 1;
        let col_1based = location.byte_start + 1;
        let line_text = notes
            .get(&diag.note_id)
            .and_then(|(_, content)| content.lines().nth(location.line).map(str::to_string))
            .or_else(|| {
                std::fs::read_to_string(&location.file_path)
                    .ok()
                    .and_then(|content| content.lines().nth(location.line).map(str::to_string))
            })
            .unwrap_or_default();

        let lnum_w = format!("{line_1based}").len();
        let pad = " ".repeat(lnum_w);
        let prefix_bytes = (location.byte_start as usize).min(line_text.len());
        let end_bytes = (location.byte_end as usize).min(line_text.len());
        let display_col = display_width(&line_text[..prefix_bytes]);
        let underline_disp = display_width(&line_text[prefix_bytes..end_bytes]).max(1);
        let underline = "^".repeat(underline_disp);
        let pointer_spaces = " ".repeat(display_col);
        let path = location.file_path.display();

        if color {
            out.push_str(&format!(
                " \x1b[1;34m{pad}┌─\x1b[0m \x1b[36m{path}:{line_1based}:{col_1based}\x1b[0m\n"
            ));
            out.push_str(&format!(" \x1b[1;34m{pad}│\x1b[0m\n"));
            out.push_str(&format!(
                "\x1b[1;34m{line_1based:>lnum_w$} │\x1b[0m {line_text}\n"
            ));
            out.push_str(&format!(
                " \x1b[1;34m{pad}│\x1b[0m {pointer_spaces}\x1b[1;31m{underline}\x1b[0m\n"
            ));
        } else {
            out.push_str(&format!(" {pad}┌─ {path}:{line_1based}:{col_1based}\n"));
            out.push_str(&format!(" {pad}│\n"));
            out.push_str(&format!("{line_1based:>lnum_w$} │ {line_text}\n"));
            out.push_str(&format!(" {pad}│ {pointer_spaces}{underline}\n"));
        }
        out.push('\n');
    }

    out
}

fn display_width(s: &str) -> usize {
    s.width()
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::reconcile::default_module::load_module;
    use crate::reconcile::eval::eval_all;
    use crate::reconcile::observe::WorkspaceSnapshot;
    use crate::reconcile::types::{DiagnosticKind, DiagnosticSeverity, Status, Value};

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

    fn note_map(notes: &[(&str, String)]) -> HashMap<NoteId, (PathBuf, String)> {
        notes
            .iter()
            .map(|(id, content)| {
                (
                    id.to_string(),
                    (PathBuf::from(format!("{id}.typ")), content.clone()),
                )
            })
            .collect()
    }

    fn test_rule_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join("rules")
            .join("checklist.lisp")
    }

    fn load_test_module() -> ast::Module {
        load_module(&[test_rule_path()], true).expect("load rule file")
    }

    fn make_test_config(root: PathBuf) -> WikiConfig {
        let note_dir = root.join("note");
        WikiConfig {
            root,
            note_dir,
            link_file: PathBuf::from("link.typ"),
            zk_config: crate::config::ZkLspConfig {
                reconcile_rules: vec![test_rule_path()],
                disable_default_reconcile_rules: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn full_pipeline_multi_note_dag() {
        // A done → B refs A → C refs B; expect all Done
        let a = make_toml_note("A", "1010101010", "done", "");
        let b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");

        let snap = snapshot_from(&[("1010101010", &a), ("2020202020", &b), ("3030303030", &c)]);
        let module = load_test_module();
        let eval_result = eval_all(&module, &snap);
        let result = materialize(eval_result);

        assert_eq!(
            result
                .materialized_meta
                .get(&("1010101010".to_string(), "checklist-status".to_string())),
            Some(&Value::Status(Status::Done))
        );
        assert_eq!(
            result
                .materialized_meta
                .get(&("2020202020".to_string(), "checklist-status".to_string())),
            Some(&Value::Status(Status::Done))
        );
        assert_eq!(
            result
                .materialized_meta
                .get(&("3030303030".to_string(), "checklist-status".to_string())),
            Some(&Value::Status(Status::Done))
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
        let module = load_test_module();
        let result = eval_all(&module, &snap);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::Cycle),
            "cycle diagnostic emitted"
        );
    }

    #[test]
    fn cycle_diagnostics_include_source_locations() {
        let notes = note_map(&[
            (
                "1111111111",
                make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n"),
            ),
            (
                "2222222222",
                make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n"),
            ),
        ]);

        let graph = dependency_graph::build_dependency_graph(&notes);
        let diagnostics = cycle_diagnostics(&cycle::detect_cycles(&graph));

        assert!(!diagnostics.is_empty());
        assert!(diagnostics.iter().all(|diag| diag.location.is_some()));
        assert!(diagnostics
            .iter()
            .all(|diag| diag.severity == DiagnosticSeverity::Error));
    }

    #[test]
    fn non_leaf_ref_diagnostics_include_source_locations() {
        let notes = note_map(&[(
            "1111111111",
            make_toml_note(
                "A",
                "1111111111",
                "none",
                "- [ ] @2222222222\n  - [ ] child\n",
            ),
        )]);

        let diagnostics = non_leaf_ref_diagnostics(&notes);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, DiagnosticKind::NonLeafRef);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(diagnostics[0].location.is_some());
    }

    #[tokio::test]
    async fn collect_diagnostics_loads_rule_file_and_reports_cycle_locations() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("zk_reconcile_diag_cycle_{suffix}"));
        let note_dir = root.join("note");
        std::fs::create_dir_all(&note_dir).expect("create note dir");

        let note_a = note_dir.join("1111111111.typ");
        let note_b = note_dir.join("2222222222.typ");
        std::fs::write(
            &note_a,
            make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n"),
        )
        .expect("write note a");
        std::fs::write(
            &note_b,
            make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n"),
        )
        .expect("write note b");

        let config = make_test_config(root.clone());
        let diagnostics = collect_diagnostics(&config, None)
            .await
            .expect("collect diagnostics");

        assert!(diagnostics.iter().any(|diag| {
            diag.kind == DiagnosticKind::Cycle
                && diag
                    .location
                    .as_ref()
                    .map(|loc| loc.file_path == note_a)
                    .unwrap_or(false)
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn collect_diagnostics_loads_rule_file_and_reports_non_leaf_ref_locations() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("zk_reconcile_diag_non_leaf_{suffix}"));
        let note_dir = root.join("note");
        std::fs::create_dir_all(&note_dir).expect("create note dir");

        let note = note_dir.join("1111111111.typ");
        std::fs::write(
            &note,
            make_toml_note(
                "A",
                "1111111111",
                "none",
                "- [ ] @2222222222\n  - [ ] child\n",
            ),
        )
        .expect("write note");

        let config = make_test_config(root.clone());
        let diagnostics = collect_diagnostics(&config, None)
            .await
            .expect("collect diagnostics");

        assert!(diagnostics.iter().any(|diag| {
            diag.kind == DiagnosticKind::NonLeafRef
                && diag
                    .location
                    .as_ref()
                    .map(|loc| loc.file_path == note)
                    .unwrap_or(false)
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn run_reconcile_returns_typst_style_cycle_errors_for_all_nodes() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("zk_reconcile_cli_cycle_{suffix}"));
        let note_dir = root.join("note");
        std::fs::create_dir_all(&note_dir).expect("create note dir");

        let note_a = note_dir.join("1111111111.typ");
        let note_b = note_dir.join("2222222222.typ");
        std::fs::write(
            &note_a,
            make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n"),
        )
        .expect("write note a");
        std::fs::write(
            &note_b,
            make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n"),
        )
        .expect("write note b");

        let config = make_test_config(root.clone());
        let err = run_reconcile(&config, true)
            .await
            .expect_err("cycle should fail");
        let rendered = err.to_string();

        assert!(rendered.contains("error: Cyclic task dependency"));
        assert!(rendered.contains(&note_a.display().to_string()));
        assert!(rendered.contains(&note_b.display().to_string()));
        assert!(rendered.contains("┌─"));
        assert!(rendered.contains("^"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn run_reconcile_returns_typst_style_non_leaf_ref_errors() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("zk_reconcile_cli_non_leaf_{suffix}"));
        let note_dir = root.join("note");
        std::fs::create_dir_all(&note_dir).expect("create note dir");

        let note = note_dir.join("1111111111.typ");
        std::fs::write(
            &note,
            make_toml_note(
                "A",
                "1111111111",
                "none",
                "- [ ] @2222222222\n  - [ ] child\n",
            ),
        )
        .expect("write note");

        let config = make_test_config(root.clone());
        let err = run_reconcile(&config, true)
            .await
            .expect_err("non-leaf ref should fail");
        let rendered = err.to_string();

        assert!(rendered.contains("error: Ref item has child items"));
        assert!(rendered.contains(&note.display().to_string()));
        assert!(rendered.contains("┌─"));
        assert!(rendered.contains("^"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn materialized_meta_drives_status_writeback() {
        let note = make_toml_note("A", "1111111111", "none", "- [x] finished\n");
        let snap = snapshot_from(&[("1111111111", &note)]);
        let module = load_test_module();
        let result = materialize(eval_all(&module, &snap));

        let updated = apply_materialized_status("1111111111", &note, &result).expect("status edit");
        assert!(updated.contains("checklist-status = \"done\""));
        assert_eq!(
            result
                .materialized_meta
                .get(&("1111111111".to_string(), "checklist-status".to_string())),
            Some(&Value::Status(Status::Done))
        );
    }
}

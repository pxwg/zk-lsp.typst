/// Workspace-wide reconciliation of cross-file checkbox states.
///
/// Builds a dependency graph from `RefItem` checklist entries, fails fast on cycles,
/// then evaluates note done-states in a single topological pass and rewrites changed files.
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::Result;

use crate::config::WikiConfig;
use crate::cycle;
use crate::dependency_graph;
use crate::handlers::formatting::{is_note_done_with_deps, normalize_note};
use crate::parser;

struct NoteRec {
    path: PathBuf,
    content: String,
}

pub struct ReconcileStats {
    pub files_changed: usize,
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

async fn scan_notes(note_dir: &std::path::Path) -> Result<HashMap<String, NoteRec>> {
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
        // Only 10-digit IDs
        if stem.len() != 10 || !stem.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                map.insert(stem, NoteRec { path, content });
            }
            Err(_) => continue,
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Dependency extraction helper (for normalizing individual notes)
// ---------------------------------------------------------------------------

fn extract_todo_deps(content: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    parser::parse_checklist_items(content)
        .into_iter()
        .flat_map(|item| match item.kind {
            parser::ChecklistItemKind::Ref { targets } => {
                targets.into_iter().map(|t| t.target_id).collect::<Vec<_>>()
            }
            parser::ChecklistItemKind::Local => vec![],
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Topological sort (Kahn's algorithm on adj where A→B means A depends on B)
// ---------------------------------------------------------------------------

fn topo_sort_dag(adj: &HashMap<String, Vec<String>>, all_nodes: &[String]) -> Vec<String> {
    let mut in_degree: HashMap<&str, usize> =
        all_nodes.iter().map(|n| (n.as_str(), 0)).collect();
    for targets in adj.values() {
        for t in targets {
            if let Some(d) = in_degree.get_mut(t.as_str()) {
                *d += 1;
            }
        }
    }
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut order: Vec<String> = Vec::new();
    while let Some(n) = queue.pop_front() {
        order.push(n.to_string());
        if let Some(targets) = adj.get(n) {
            for t in targets {
                if let Some(d) = in_degree.get_mut(t.as_str()) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(t.as_str());
                    }
                }
            }
        }
    }
    // Append any isolated nodes not reached by Kahn's traversal
    for n in all_nodes {
        if !order.contains(n) {
            order.push(n.clone());
        }
    }
    order
}

// ---------------------------------------------------------------------------
// Single-pass DAG evaluation
// ---------------------------------------------------------------------------

fn evaluate_dag(
    notes: &HashMap<String, NoteRec>,
    adj: &HashMap<String, Vec<String>>,
) -> HashMap<String, bool> {
    let all_nodes: Vec<String> = adj.keys().cloned().collect();
    let order = topo_sort_dag(adj, &all_nodes);

    let mut global: HashMap<String, bool> = HashMap::new();
    // Iterate in reverse so that dependencies (pointed-to nodes) are evaluated first.
    // adj[A] = [B] means A depends on B; Kahn gives [A, B]; rev gives [B, A] → B evaluated first.
    for id in order.iter().rev() {
        let content = match notes.get(id) {
            Some(r) => r.content.as_str(),
            None => {
                global.insert(id.clone(), false);
                continue;
            }
        };
        let done = is_note_done_with_deps(content, &global);
        global.insert(id.clone(), done);
    }
    global
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run_reconcile(config: &WikiConfig, dry_run: bool) -> Result<ReconcileStats> {
    let notes = scan_notes(&config.note_dir).await?;

    // Build positioned dependency graph
    let note_map: HashMap<String, (PathBuf, String)> = notes
        .iter()
        .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
        .collect();
    let graph = dependency_graph::build_dependency_graph(&note_map);

    // Fail fast on cycles
    let cycles = cycle::detect_cycles(&graph);
    if !cycles.is_empty() {
        let msg = cycle::render_cycle_errors(&cycles);
        eprintln!("{msg}");
        return Err(anyhow::anyhow!(
            "{} cyclic task dependency(ies) detected; aborting reconcile",
            cycles.len()
        ));
    }

    // Single-pass DAG evaluation
    let global = evaluate_dag(&notes, &graph.adj);

    // Write back changed files
    let mut files_changed = 0usize;
    for (_id, rec) in &notes {
        let dep_states: HashMap<String, bool> = extract_todo_deps(&rec.content)
            .into_iter()
            .map(|dep_id| {
                let done = global.get(&dep_id).copied().unwrap_or(false);
                (dep_id, done)
            })
            .collect();

        let new_content = normalize_note(&rec.content, &dep_states);
        if new_content != rec.content {
            files_changed += 1;
            if !dry_run {
                tokio::fs::write(&rec.path, new_content.as_bytes()).await?;
            } else {
                eprintln!("  would update: {}", rec.path.display());
            }
        }
    }

    Ok(ReconcileStats { files_changed })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cycle;
    use crate::dependency_graph;
    use crate::handlers::formatting::{is_note_done, is_note_done_with_deps, normalize_note};

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

    #[test]
    fn refitem_rendered_checked_not_source_truth() {
        let content = "- [x] @2222222222\n";
        let deps = HashMap::from([("2222222222".to_string(), false)]);
        assert!(
            !is_note_done_with_deps(content, &deps),
            "rendered [x] on RefItem must not override semantic truth from dep_states"
        );
    }

    #[test]
    fn refitem_drives_note_status() {
        let content_a = make_toml_note("A", "1111111111", "none",
            "- [x] local task\n- [ ] @2222222222\n");

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
        let content = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let deps = HashMap::from([("2222222222".to_string(), true)]);
        let result = normalize_note(&content, &deps);
        assert!(result.contains("- [x]"), "ref checkbox updated");
        assert!(result.contains("<1111111111>"), "still note A's content");
    }

    #[test]
    fn normalize_note_is_pure() {
        let content = "- [ ] @1234567890 do thing\n";
        let mut dep_states = HashMap::new();
        dep_states.insert("1234567890".to_string(), true);
        let result = normalize_note(content, &dep_states);
        assert!(result.contains("- [x]"), "checkbox should be checked");
    }

    #[test]
    fn cycle_reconcile_fails() {
        // A depends on B, B depends on A → detect_cycles must return non-empty
        let mut note_map: HashMap<String, (PathBuf, String)> = HashMap::new();
        note_map.insert(
            "1111111111".to_string(),
            (PathBuf::from("1111111111.typ"), "- [ ] @2222222222\n".to_string()),
        );
        note_map.insert(
            "2222222222".to_string(),
            (PathBuf::from("2222222222.typ"), "- [ ] @1111111111\n".to_string()),
        );
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let cycles = cycle::detect_cycles(&graph);
        assert!(!cycles.is_empty(), "cyclic notes must be detected as a cycle");
    }

    #[test]
    fn chain_propagation() {
        // A: done (TOML status = "done"); B references A; C references B
        let content_a = make_toml_note("A", "1010101010", "done", "");
        let content_b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let content_c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");

        let mut dep_a: HashMap<String, bool> = HashMap::new();
        let normalized_a = normalize_note(&content_a, &dep_a);
        assert!(is_note_done(&normalized_a), "A should be done");

        dep_a.insert("1010101010".to_string(), true);
        let normalized_b = normalize_note(&content_b, &dep_a);
        assert!(
            normalized_b.contains("- [x]"),
            "B's ref to A should be checked"
        );
        let b_done = is_note_done(&normalized_b);

        let mut dep_b: HashMap<String, bool> = HashMap::new();
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
        let content = make_toml_note("X", "3333333333", "none",
            "- [ ] @1111111111 @2222222222\n");

        let deps_both = HashMap::from([
            ("1111111111".to_string(), true),
            ("2222222222".to_string(), true),
        ]);
        assert!(is_note_done_with_deps(&content, &deps_both),
            "done when all refs done");

        let deps_one = HashMap::from([
            ("1111111111".to_string(), true),
            ("2222222222".to_string(), false),
        ]);
        assert!(!is_note_done_with_deps(&content, &deps_one),
            "not done when one ref is not done");
    }

    #[test]
    fn no_checklist_note_uses_metadata_status() {
        let content_done = make_toml_note("D", "4444444444", "done", "");
        let content_none = make_toml_note("N", "5555555555", "none", "");
        assert!(is_note_done_with_deps(&content_done, &HashMap::new()));
        assert!(!is_note_done_with_deps(&content_none, &HashMap::new()));
        let irrelevant_deps = HashMap::from([("9999999999".to_string(), true)]);
        assert!(is_note_done_with_deps(&content_done, &irrelevant_deps));
        assert!(!is_note_done_with_deps(&content_none, &irrelevant_deps));
    }

    #[test]
    fn test_dag_reconcile_still_works() {
        // A: done via metadata; B depends on A; C depends on B
        // Single-pass DAG evaluation should propagate correctly.
        let content_a = make_toml_note("A", "1010101010", "done", "");
        let content_b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let content_c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");

        let mut notes = HashMap::new();
        notes.insert(
            "1010101010".to_string(),
            NoteRec { path: PathBuf::from("1010101010.typ"), content: content_a.clone() },
        );
        notes.insert(
            "2020202020".to_string(),
            NoteRec { path: PathBuf::from("2020202020.typ"), content: content_b.clone() },
        );
        notes.insert(
            "3030303030".to_string(),
            NoteRec { path: PathBuf::from("3030303030.typ"), content: content_c.clone() },
        );

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let cycles = cycle::detect_cycles(&graph);
        assert!(cycles.is_empty(), "A→B→C DAG must have no cycles");

        let global = evaluate_dag(&notes, &graph.adj);
        assert!(global.get("1010101010").copied().unwrap_or(false), "A should be done");
        assert!(global.get("2020202020").copied().unwrap_or(false), "B should be done (A done)");
        assert!(global.get("3030303030").copied().unwrap_or(false), "C should be done (B done)");
    }

    #[test]
    fn reconcile_idempotent_in_memory() {
        let content_a = make_toml_note("A", "4040404040", "done", "");
        let content_b = make_toml_note("B", "5050505050", "none", "- [ ] @4040404040\n");

        let mut notes = HashMap::new();
        notes.insert(
            "4040404040".to_string(),
            NoteRec { path: PathBuf::from("4040404040.typ"), content: content_a.clone() },
        );
        notes.insert(
            "5050505050".to_string(),
            NoteRec { path: PathBuf::from("5050505050.typ"), content: content_b.clone() },
        );

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let global = evaluate_dag(&notes, &graph.adj);

        // Apply first round of rewrites in memory
        let mut updated: HashMap<String, String> = HashMap::new();
        for (id, rec) in &notes {
            let deps: HashMap<String, bool> = extract_todo_deps(&rec.content)
                .into_iter()
                .map(|dep_id| (dep_id.clone(), global.get(&dep_id).copied().unwrap_or(false)))
                .collect();
            updated.insert(id.clone(), normalize_note(&rec.content, &deps));
        }

        // Build second notes map from updated content
        let mut notes2: HashMap<String, NoteRec> = HashMap::new();
        for (id, content) in &updated {
            notes2.insert(
                id.clone(),
                NoteRec { path: notes[id].path.clone(), content: content.clone() },
            );
        }

        let note_map2: HashMap<String, (PathBuf, String)> = notes2
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph2 = dependency_graph::build_dependency_graph(&note_map2);
        let global2 = evaluate_dag(&notes2, &graph2.adj);

        let mut changed = 0usize;
        for (id, rec) in &notes2 {
            let deps: HashMap<String, bool> = extract_todo_deps(&rec.content)
                .into_iter()
                .map(|dep_id| (dep_id.clone(), global2.get(&dep_id).copied().unwrap_or(false)))
                .collect();
            let new_content = normalize_note(&rec.content, &deps);
            if new_content != rec.content {
                changed += 1;
                eprintln!("id={id} changed:\n---\n{}\n---\n{}\n---", rec.content, new_content);
            }
        }
        assert_eq!(changed, 0, "second round should produce no changes");
    }

    fn make_toml_note_full(
        title: &str,
        id: &str,
        status: &str,
        relation: &str,
        relation_target: &[&str],
        body: &str,
    ) -> String {
        let targets = relation_target
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\n\
             \x20 ```toml\n\
             \x20 schema-version = 1\n\
             \x20 title = \"{title}\"\n\
             \x20 tags = []\n\
             \x20 checklist-status = \"{status}\"\n\
             \x20 relation = \"{relation}\"\n\
             \x20 relation-target = [{targets}]\n\
             \x20 generated = false\n\
             \x20 ```.text,\n\
             ))\n\
             #show: zettel.with(metadata: zk-metadata)\n\
             \n\
             = {title} <{id}>\n\
             {body}"
        )
    }

    // -----------------------------------------------------------------------
    // Group 1: First-line checklist
    // -----------------------------------------------------------------------

    #[test]
    fn first_line_checklist_not_forced_done() {
        let content = make_toml_note("A", "1100000001", "none", "- [ ] task\n");
        assert!(!is_note_done_with_deps(&content, &HashMap::new()));
    }

    #[test]
    fn first_line_checklist_done_when_checked() {
        let content = make_toml_note("A", "1100000002", "none", "- [x] task\n");
        assert!(is_note_done_with_deps(&content, &HashMap::new()));
    }

    #[test]
    fn first_line_ref_checklist_evaluates_dep() {
        let content = make_toml_note("A", "1100000003", "none", "- [ ] @9900000001\n");
        assert!(!is_note_done_with_deps(&content, &HashMap::new()));
        let deps_done = HashMap::from([("9900000001".to_string(), true)]);
        assert!(is_note_done_with_deps(&content, &deps_done));
    }

    // -----------------------------------------------------------------------
    // Group 2: Nested checklist semantics
    // -----------------------------------------------------------------------

    #[test]
    fn nested_local_all_children_done() {
        // Parent is non-leaf; only children (leaves) count
        let body = "- [ ] parent\n  - [x] child1\n  - [x] child2\n";
        let content = make_toml_note("A", "1200000001", "none", body);
        assert!(is_note_done_with_deps(&content, &HashMap::new()), "all leaves checked → done");
    }

    #[test]
    fn nested_local_one_child_incomplete() {
        let body = "- [ ] parent\n  - [x] child1\n  - [ ] child2\n";
        let content = make_toml_note("A", "1200000002", "none", body);
        assert!(!is_note_done_with_deps(&content, &HashMap::new()), "one leaf unchecked → not done");
    }

    #[test]
    fn nested_ref_child_drives_parent_truth() {
        // Ref leaf under a local parent: ref dep decides truth
        let body = "- [ ] parent\n  - [ ] @9900000002\n";
        let content = make_toml_note("A", "1200000003", "none", body);
        assert!(!is_note_done_with_deps(&content, &HashMap::new()));
        let deps = HashMap::from([("9900000002".to_string(), true)]);
        assert!(is_note_done_with_deps(&content, &deps));
    }

    #[test]
    fn three_level_nesting_only_leaves_count() {
        // grandparent → parent → two leaf children (all done)
        let body = "- [ ] gp\n  - [ ] parent\n    - [x] leaf1\n    - [x] leaf2\n";
        let content = make_toml_note("A", "1200000004", "none", body);
        assert!(is_note_done_with_deps(&content, &HashMap::new()));
    }

    #[test]
    fn three_level_one_leaf_unchecked() {
        let body = "- [ ] gp\n  - [ ] parent\n    - [x] leaf1\n    - [ ] leaf2\n";
        let content = make_toml_note("A", "1200000005", "none", body);
        assert!(!is_note_done_with_deps(&content, &HashMap::new()));
    }

    // -----------------------------------------------------------------------
    // Group 3: Sub-task dependency pattern
    // -----------------------------------------------------------------------

    #[test]
    fn refitem_child_of_local_parent() {
        // "- [ ] task B\n  - [ ] @taskA" — Ref is a leaf
        let body = "- [ ] task B\n  - [ ] @9900000003\n";
        let content = make_toml_note("B", "1300000001", "none", body);
        let deps_done = HashMap::from([("9900000003".to_string(), true)]);
        assert!(is_note_done_with_deps(&content, &deps_done));
        assert!(!is_note_done_with_deps(&content, &HashMap::new()));
    }

    #[test]
    fn refitem_child_normalize_updates_checkbox() {
        let body = "- [ ] task B\n  - [ ] @9900000004\n";
        let content = make_toml_note("B", "1300000002", "none", body);
        let deps = HashMap::from([("9900000004".to_string(), true)]);
        let result = normalize_note(&content, &deps);
        // The ref child checkbox should be updated to [x]
        assert!(result.contains("- [x] @9900000004"), "ref child updated to [x]");
    }

    #[test]
    fn refitem_child_normalize_not_done() {
        let body = "- [ ] task B\n  - [ ] @9900000005\n";
        let content = make_toml_note("B", "1300000003", "none", body);
        let deps = HashMap::from([("9900000005".to_string(), false)]);
        let result = normalize_note(&content, &deps);
        assert!(result.contains("- [ ] @9900000005"), "ref child stays [ ] when dep not done");
    }

    #[test]
    fn mixed_local_and_ref_children() {
        // Local child done, Ref child not done → note not done
        let body = "- [ ] parent\n  - [x] local child\n  - [ ] @9900000006\n";
        let content = make_toml_note("B", "1300000004", "none", body);
        assert!(!is_note_done_with_deps(&content, &HashMap::new()), "ref leaf not done → not done");
        let deps = HashMap::from([("9900000006".to_string(), true)]);
        assert!(is_note_done_with_deps(&content, &deps), "both leaves done → done");
    }

    // -----------------------------------------------------------------------
    // Group 4: DAG with nested structures
    // -----------------------------------------------------------------------

    #[test]
    fn dag_nested_subtask_chain() {
        // A: has all checked items (done); B: "- [ ] task\n  - [ ] @A"; C: depends on B
        let content_a = make_toml_note("A", "1400000001", "none", "- [x] item\n");
        let content_b = make_toml_note("B", "1400000002", "none",
            "- [ ] task\n  - [ ] @1400000001\n");
        let content_c = make_toml_note("C", "1400000003", "none", "- [ ] @1400000002\n");

        let mut notes = HashMap::new();
        notes.insert("1400000001".to_string(),
            NoteRec { path: PathBuf::from("1400000001.typ"), content: content_a });
        notes.insert("1400000002".to_string(),
            NoteRec { path: PathBuf::from("1400000002.typ"), content: content_b });
        notes.insert("1400000003".to_string(),
            NoteRec { path: PathBuf::from("1400000003.typ"), content: content_c });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let cycles = cycle::detect_cycles(&graph);
        assert!(cycles.is_empty());

        let global = evaluate_dag(&notes, &graph.adj);
        assert!(global.get("1400000001").copied().unwrap_or(false), "A done");
        assert!(global.get("1400000002").copied().unwrap_or(false), "B done");
        assert!(global.get("1400000003").copied().unwrap_or(false), "C done");
    }

    #[test]
    fn dag_nested_subtask_chain_incomplete() {
        // A has unchecked item → all cascade to false
        let content_a = make_toml_note("A", "1400000004", "none", "- [ ] item\n");
        let content_b = make_toml_note("B", "1400000005", "none",
            "- [ ] task\n  - [ ] @1400000004\n");
        let content_c = make_toml_note("C", "1400000006", "none", "- [ ] @1400000005\n");

        let mut notes = HashMap::new();
        notes.insert("1400000004".to_string(),
            NoteRec { path: PathBuf::from("1400000004.typ"), content: content_a });
        notes.insert("1400000005".to_string(),
            NoteRec { path: PathBuf::from("1400000005.typ"), content: content_b });
        notes.insert("1400000006".to_string(),
            NoteRec { path: PathBuf::from("1400000006.typ"), content: content_c });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let global = evaluate_dag(&notes, &graph.adj);
        assert!(!global.get("1400000004").copied().unwrap_or(false), "A not done");
        assert!(!global.get("1400000005").copied().unwrap_or(false), "B not done");
        assert!(!global.get("1400000006").copied().unwrap_or(false), "C not done");
    }

    // -----------------------------------------------------------------------
    // Group 5: Diamond dependency
    // -----------------------------------------------------------------------

    #[test]
    fn diamond_all_done() {
        // D done → B and C both depend on D → A depends on B and C
        let content_d = make_toml_note("D", "1500000001", "done", "");
        let content_b = make_toml_note("B", "1500000002", "none", "- [ ] @1500000001\n");
        let content_c = make_toml_note("C", "1500000003", "none", "- [ ] @1500000001\n");
        let content_a = make_toml_note("A", "1500000004", "none",
            "- [ ] @1500000002\n- [ ] @1500000003\n");

        let mut notes = HashMap::new();
        notes.insert("1500000001".to_string(),
            NoteRec { path: PathBuf::from("1500000001.typ"), content: content_d });
        notes.insert("1500000002".to_string(),
            NoteRec { path: PathBuf::from("1500000002.typ"), content: content_b });
        notes.insert("1500000003".to_string(),
            NoteRec { path: PathBuf::from("1500000003.typ"), content: content_c });
        notes.insert("1500000004".to_string(),
            NoteRec { path: PathBuf::from("1500000004.typ"), content: content_a });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let cycles = cycle::detect_cycles(&graph);
        assert!(cycles.is_empty());

        let global = evaluate_dag(&notes, &graph.adj);
        assert!(global.get("1500000001").copied().unwrap_or(false), "D done");
        assert!(global.get("1500000002").copied().unwrap_or(false), "B done");
        assert!(global.get("1500000003").copied().unwrap_or(false), "C done");
        assert!(global.get("1500000004").copied().unwrap_or(false), "A done");
    }

    #[test]
    fn diamond_one_branch_incomplete() {
        // C has an extra unchecked item → A not done
        let content_d = make_toml_note("D", "1500000005", "done", "");
        let content_b = make_toml_note("B", "1500000006", "none", "- [ ] @1500000005\n");
        let content_c = make_toml_note("C", "1500000007", "none",
            "- [ ] @1500000005\n- [ ] extra task\n");
        let content_a = make_toml_note("A", "1500000008", "none",
            "- [ ] @1500000006\n- [ ] @1500000007\n");

        let mut notes = HashMap::new();
        notes.insert("1500000005".to_string(),
            NoteRec { path: PathBuf::from("1500000005.typ"), content: content_d });
        notes.insert("1500000006".to_string(),
            NoteRec { path: PathBuf::from("1500000006.typ"), content: content_b });
        notes.insert("1500000007".to_string(),
            NoteRec { path: PathBuf::from("1500000007.typ"), content: content_c });
        notes.insert("1500000008".to_string(),
            NoteRec { path: PathBuf::from("1500000008.typ"), content: content_a });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let global = evaluate_dag(&notes, &graph.adj);
        assert!(global.get("1500000006").copied().unwrap_or(false), "B done");
        assert!(!global.get("1500000007").copied().unwrap_or(false), "C not done (extra task)");
        assert!(!global.get("1500000008").copied().unwrap_or(false), "A not done");
    }

    // -----------------------------------------------------------------------
    // Group 6: Metadata status update via normalize
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_updates_status_to_done() {
        let content = make_toml_note("A", "1600000001", "none", "- [x] task\n");
        let result = normalize_note(&content, &HashMap::new());
        assert!(result.contains("checklist-status = \"done\""), "status updated to done");
    }

    #[test]
    fn normalize_updates_status_to_wip() {
        let content = make_toml_note("A", "1600000002", "none", "- [x] done\n- [ ] pending\n");
        let result = normalize_note(&content, &HashMap::new());
        assert!(result.contains("checklist-status = \"wip\""), "status updated to wip");
    }

    #[test]
    fn normalize_updates_status_to_todo() {
        let content = make_toml_note("A", "1600000003", "none", "- [ ] task1\n- [ ] task2\n");
        let result = normalize_note(&content, &HashMap::new());
        assert!(result.contains("checklist-status = \"todo\""), "status updated to todo");
    }

    // -----------------------------------------------------------------------
    // Group 7: Archived notes in chains
    // -----------------------------------------------------------------------

    #[test]
    fn archived_note_always_done() {
        // Archived note with unchecked items → still considered done
        let content = make_toml_note_full("A", "1700000001", "none", "archived",
            &["9900000099"], "- [ ] unchecked\n");
        assert!(is_note_done_with_deps(&content, &HashMap::new()), "archived → always done");
    }

    #[test]
    fn dag_with_archived_dependency() {
        let content_a = make_toml_note_full("A", "1700000002", "none", "archived",
            &["9900000099"], "- [ ] unchecked\n");
        let content_b = make_toml_note("B", "1700000003", "none", "- [ ] @1700000002\n");

        let mut notes = HashMap::new();
        notes.insert("1700000002".to_string(),
            NoteRec { path: PathBuf::from("1700000002.typ"), content: content_a });
        notes.insert("1700000003".to_string(),
            NoteRec { path: PathBuf::from("1700000003.typ"), content: content_b });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let global = evaluate_dag(&notes, &graph.adj);
        assert!(global.get("1700000002").copied().unwrap_or(false), "A (archived) → done");
        assert!(global.get("1700000003").copied().unwrap_or(false), "B done because A is done");
    }

    // -----------------------------------------------------------------------
    // Group 8: Multiple checklist groups & edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn two_independent_groups_partial() {
        let body = "- [x] group1\n\n- [ ] group2\n";
        let content = make_toml_note("A", "1800000001", "none", body);
        assert!(!is_note_done_with_deps(&content, &HashMap::new()), "one unchecked leaf → not done");
    }

    #[test]
    fn two_independent_groups_all_done() {
        let body = "- [x] group1\n\n- [x] group2\n";
        let content = make_toml_note("A", "1800000002", "none", body);
        assert!(is_note_done_with_deps(&content, &HashMap::new()), "all leaves checked → done");
    }

    #[test]
    fn non_leaf_refitem_ignored_by_solver() {
        // Ref parent with a Local child: Ref is non-leaf → ignored by solver; leaf (local) decides
        let body = "- [ ] @9900000007\n  - [x] local child\n";
        let content = make_toml_note("A", "1800000003", "none", body);
        // dep for 9900000007 is false, but it's non-leaf so only the local child leaf counts
        let deps = HashMap::from([("9900000007".to_string(), false)]);
        assert!(is_note_done_with_deps(&content, &deps), "non-leaf ref ignored; leaf child done → done");
    }

    #[test]
    fn extract_todo_deps_nested_and_dedup() {
        // Refs at multiple nesting levels; same ID repeated → should be deduped
        let content = "- [ ] @1111111111\n  - [ ] @2222222222\n  - [ ] @1111111111\n";
        let deps = extract_todo_deps(content);
        // 1111111111 appears twice but should be deduplicated
        let count_1 = deps.iter().filter(|id| *id == "1111111111").count();
        assert_eq!(count_1, 1, "duplicate @ID should be deduplicated");
        assert!(deps.contains(&"2222222222".to_string()), "nested ref included");
    }

    // -----------------------------------------------------------------------
    // Group 9: Idempotency with nesting
    // -----------------------------------------------------------------------

    #[test]
    fn idempotent_nested_normalize() {
        let body = "- [ ] parent\n  - [x] child1\n  - [x] child2\n";
        let content = make_toml_note("A", "1900000001", "none", body);
        let result1 = normalize_note(&content, &HashMap::new());
        let result2 = normalize_note(&result1, &HashMap::new());
        assert_eq!(result1, result2, "second normalize pass must produce no changes");
    }

    #[test]
    fn idempotent_dag_nested_structure() {
        let content_a = make_toml_note("A", "1900000002", "none",
            "- [x] item1\n  - [x] sub\n");
        let content_b = make_toml_note("B", "1900000003", "none",
            "- [ ] parent\n  - [ ] @1900000002\n");

        let mut notes = HashMap::new();
        notes.insert("1900000002".to_string(),
            NoteRec { path: PathBuf::from("1900000002.typ"), content: content_a });
        notes.insert("1900000003".to_string(),
            NoteRec { path: PathBuf::from("1900000003.typ"), content: content_b });

        let note_map: HashMap<String, (PathBuf, String)> = notes
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph = dependency_graph::build_dependency_graph(&note_map);
        let global = evaluate_dag(&notes, &graph.adj);

        // First pass
        let mut updated: HashMap<String, String> = HashMap::new();
        for (id, rec) in &notes {
            let deps: HashMap<String, bool> = extract_todo_deps(&rec.content)
                .into_iter()
                .map(|dep_id| (dep_id.clone(), global.get(&dep_id).copied().unwrap_or(false)))
                .collect();
            updated.insert(id.clone(), normalize_note(&rec.content, &deps));
        }

        // Second pass
        let notes2: HashMap<String, NoteRec> = updated
            .iter()
            .map(|(id, content)| {
                (id.clone(), NoteRec { path: notes[id].path.clone(), content: content.clone() })
            })
            .collect();
        let note_map2: HashMap<String, (PathBuf, String)> = notes2
            .iter()
            .map(|(id, rec)| (id.clone(), (rec.path.clone(), rec.content.clone())))
            .collect();
        let graph2 = dependency_graph::build_dependency_graph(&note_map2);
        let global2 = evaluate_dag(&notes2, &graph2.adj);

        let mut changed = 0usize;
        for (id, rec) in &notes2 {
            let deps: HashMap<String, bool> = extract_todo_deps(&rec.content)
                .into_iter()
                .map(|dep_id| (dep_id.clone(), global2.get(&dep_id).copied().unwrap_or(false)))
                .collect();
            let new_content = normalize_note(&rec.content, &deps);
            if new_content != rec.content {
                changed += 1;
                eprintln!("id={id} changed on second pass");
            }
        }
        assert_eq!(changed, 0, "second DAG normalize pass must produce no changes");
    }
}

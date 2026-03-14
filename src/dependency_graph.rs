/// Build a positioned dependency graph from note content.
///
/// Each `RefItem` checklist entry produces one `CycleEdgeOccurrence` per `RefTarget`,
/// carrying the file location needed for cycle error reporting.
use std::collections::HashMap;
use std::path::PathBuf;

use crate::parser;

#[derive(Debug, Clone)]
pub struct CycleEdgeOccurrence {
    pub from_note_id: String,
    pub to_note_id: String,
    pub file_path: PathBuf,
    pub line: usize,     // 0-based line index
    pub byte_start: u32, // byte offset of '@' within the line
    pub byte_end: u32,   // byte offset past the last digit
    #[allow(dead_code)]
    pub line_text: String, // full line text for display
}

pub struct DependencyGraph {
    pub nodes: Vec<String>,
    pub adj: HashMap<String, Vec<String>>,
    pub occurrences: Vec<CycleEdgeOccurrence>,
}

/// Build a `DependencyGraph` from a map of `id → (path, content)`.
///
/// Every `RefItem` checklist entry contributes one directed edge per `RefTarget`:
/// `from_note_id → to_note_id`. Duplicate edges in `adj` are deduplicated; all
/// positioned occurrences are retained for error reporting.
pub fn build_dependency_graph(notes: &HashMap<String, (PathBuf, String)>) -> DependencyGraph {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut occurrences: Vec<CycleEdgeOccurrence> = Vec::new();

    for (from_id, (path, content)) in notes {
        adj.entry(from_id.clone()).or_default();
        let items = parser::parse_checklist_items(content);
        let lines: Vec<&str> = content.lines().collect();

        for item in items {
            if let parser::ChecklistItemKind::Ref { targets } = item.kind {
                let line_text = lines.get(item.line_idx).copied().unwrap_or("").to_string();
                for target in targets {
                    occurrences.push(CycleEdgeOccurrence {
                        from_note_id: from_id.clone(),
                        to_note_id: target.target_id.clone(),
                        file_path: path.clone(),
                        line: item.line_idx,
                        byte_start: target.byte_start,
                        byte_end: target.byte_end,
                        line_text: line_text.clone(),
                    });
                    let targets_list = adj.entry(from_id.clone()).or_default();
                    if !targets_list.contains(&target.target_id) {
                        targets_list.push(target.target_id.clone());
                    }
                    // Ensure the dependency node appears in adj (even if it has no outgoing edges)
                    adj.entry(target.target_id.clone()).or_default();
                }
            }
        }
    }

    let nodes = adj.keys().cloned().collect();
    DependencyGraph {
        nodes,
        adj,
        occurrences,
    }
}

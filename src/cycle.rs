/// Cycle detection on the task dependency graph.
///
/// `detect_cycles` runs Tarjan SCC; `render_cycle_errors` formats results for CLI output.
/// This module has no I/O and does no parsing; it consumes a `DependencyGraph`.
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;

use unicode_width::UnicodeWidthStr;

use crate::dependency_graph::{CycleEdgeOccurrence, DependencyGraph};

#[derive(Debug)]
pub struct DependencyCycle {
    #[allow(dead_code)]
    pub nodes: Vec<String>,              // note IDs forming the cycle
    pub edges: Vec<CycleEdgeOccurrence>, // positioned occurrences within this cycle
}

// ---------------------------------------------------------------------------
// Tarjan SCC
// ---------------------------------------------------------------------------

struct TarjanState<'a> {
    graph: &'a HashMap<String, Vec<String>>,
    index_counter: usize,
    stack: Vec<String>,
    on_stack: HashMap<String, bool>,
    index: HashMap<String, usize>,
    lowlink: HashMap<String, usize>,
    sccs: Vec<Vec<String>>,
}

impl<'a> TarjanState<'a> {
    fn strongconnect(&mut self, v: &str) {
        self.index.insert(v.to_string(), self.index_counter);
        self.lowlink.insert(v.to_string(), self.index_counter);
        self.index_counter += 1;
        self.stack.push(v.to_string());
        self.on_stack.insert(v.to_string(), true);

        if let Some(neighbors) = self.graph.get(v) {
            let neighbors: Vec<String> = neighbors.clone();
            for w in neighbors {
                if !self.index.contains_key(&w) {
                    self.strongconnect(&w);
                    let lv = *self.lowlink.get(v).unwrap();
                    let lw = *self.lowlink.get(&w).unwrap_or(&usize::MAX);
                    self.lowlink.insert(v.to_string(), lv.min(lw));
                } else if *self.on_stack.get(&w).unwrap_or(&false) {
                    let lv = *self.lowlink.get(v).unwrap();
                    let iw = *self.index.get(&w).unwrap();
                    self.lowlink.insert(v.to_string(), lv.min(iw));
                }
            }
        }

        if self.lowlink.get(v) == self.index.get(v) {
            let mut scc = Vec::new();
            loop {
                let w = self.stack.pop().unwrap();
                self.on_stack.insert(w.clone(), false);
                scc.push(w.clone());
                if w == v {
                    break;
                }
            }
            self.sccs.push(scc);
        }
    }
}

fn tarjan_sccs(
    graph: &HashMap<String, Vec<String>>,
    all_nodes: &[String],
) -> Vec<Vec<String>> {
    let mut state = TarjanState {
        graph,
        index_counter: 0,
        stack: Vec::new(),
        on_stack: HashMap::new(),
        index: HashMap::new(),
        lowlink: HashMap::new(),
        sccs: Vec::new(),
    };
    for v in all_nodes {
        if !state.index.contains_key(v.as_str()) {
            state.strongconnect(v);
        }
    }
    state.sccs
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Detect cycles in `graph` using Tarjan SCC.
///
/// A cycle exists when an SCC has size > 1, or size == 1 with a self-edge.
/// Each cycle is returned with its member node IDs and all `CycleEdgeOccurrence`s
/// whose both endpoints lie within the same SCC.
pub fn detect_cycles(graph: &DependencyGraph) -> Vec<DependencyCycle> {
    let mut all_nodes: Vec<String> = graph.nodes.clone();
    all_nodes.sort(); // deterministic iteration order
    let sccs = tarjan_sccs(&graph.adj, &all_nodes);

    let mut cycles = Vec::new();
    for scc in sccs {
        let is_self_loop = scc.len() == 1
            && graph.adj.get(&scc[0]).map_or(false, |ns| ns.contains(&scc[0]));
        if scc.len() > 1 || is_self_loop {
            let scc_set: HashSet<&str> = scc.iter().map(String::as_str).collect();
            let edges: Vec<CycleEdgeOccurrence> = graph
                .occurrences
                .iter()
                .filter(|occ| {
                    scc_set.contains(occ.from_note_id.as_str())
                        && scc_set.contains(occ.to_note_id.as_str())
                })
                .cloned()
                .collect();
            cycles.push(DependencyCycle { nodes: scc, edges });
        }
    }
    cycles
}

/// Render Typst-style error messages for all detected cycles (CLI output).
///
/// Column numbers are 1-based byte offsets (not UTF-16; use `byte_to_utf16` for LSP).
/// ANSI colours are emitted only when stderr is a TTY.
#[allow(dead_code)]
pub fn render_cycle_errors(cycles: &[DependencyCycle]) -> String {
    let color = std::io::stderr().is_terminal();
    let mut out = String::new();
    for cycle in cycles {
        // "error: cyclic task dependency detected"
        if color {
            out.push_str("\x1b[1;31merror\x1b[0m\x1b[1m: cyclic task dependency detected\x1b[0m\n");
        } else {
            out.push_str("error: cyclic task dependency detected\n");
        }

        for edge in &cycle.edges {
            let col = edge.byte_start + 1; // 1-based byte column
            let path = edge.file_path.display();
            let line_1based = edge.line + 1;
            // Width of the line-number field for padding
            let lnum_w = format!("{line_1based}").len();
            let pad = " ".repeat(lnum_w); // spaces matching line-number width

            // Display-column count of the prefix before '@' (accounts for CJK double-width)
            let prefix_bytes = edge.byte_start as usize;
            let display_col = display_width(&edge.line_text[..prefix_bytes.min(edge.line_text.len())]);
            let underline_disp = display_width(
                &edge.line_text[prefix_bytes..
                    (edge.byte_end as usize).min(edge.line_text.len())],
            );
            let underline = "^".repeat(underline_disp.max(1));
            let pointer_spaces = " ".repeat(display_col);

            // ┌─ path:line:col
            if color {
                out.push_str(&format!(
                    " \x1b[1;34m{pad}┌─\x1b[0m \x1b[36m{path}:{line_1based}:{col}\x1b[0m\n"
                ));
                // blank │
                out.push_str(&format!(" \x1b[1;34m{pad}│\x1b[0m\n"));
                // line content
                out.push_str(&format!(
                    "\x1b[1;34m{line_1based:>lnum_w$} │\x1b[0m {}\n",
                    edge.line_text
                ));
                // underline
                out.push_str(&format!(
                    " \x1b[1;34m{pad}│\x1b[0m {pointer_spaces}\x1b[1;31m{underline}\x1b[0m this dependency participates in a cycle\n"
                ));
            } else {
                out.push_str(&format!(" {pad}┌─ {path}:{line_1based}:{col}\n"));
                out.push_str(&format!(" {pad}│\n"));
                out.push_str(&format!("{line_1based:>lnum_w$} │ {}\n", edge.line_text));
                out.push_str(&format!(
                    " {pad}│ {pointer_spaces}{underline} this dependency participates in a cycle\n"
                ));
            }
        }

        // Cycle chain
        let mut chain = cycle.nodes.clone();
        if let Some(first) = chain.first().cloned() {
            chain.push(first);
        }
        if color {
            out.push_str(&format!(
                "\n\x1b[1mcycle\x1b[0m:\n  {}\n\n",
                chain.join(" \x1b[1;33m->\x1b[0m ")
            ));
        } else {
            out.push_str(&format!("\ncycle:\n  {}\n\n", chain.join(" -> ")));
        }
    }
    out
}

/// Count terminal display columns for `s` (CJK chars = 2 columns, others = 1).
#[allow(dead_code)]
fn display_width(s: &str) -> usize {
    s.width()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency_graph::build_dependency_graph;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn simple_notes(pairs: &[(&str, &str, &str)]) -> HashMap<String, (PathBuf, String)> {
        // pairs: (id, content_without_checklist_header, checklist_body)
        // We just use raw content strings directly
        pairs
            .iter()
            .map(|(id, _title, content)| {
                (id.to_string(), (PathBuf::from(format!("{id}.typ")), content.to_string()))
            })
            .collect()
    }

    #[test]
    fn test_detect_simple_cycle() {
        // A → B → A
        let notes = simple_notes(&[
            ("1111111111", "A", "- [ ] @2222222222\n"),
            ("2222222222", "B", "- [ ] @1111111111\n"),
        ]);
        let graph = build_dependency_graph(&notes);
        let cycles = detect_cycles(&graph);
        assert_eq!(cycles.len(), 1, "expected 1 cycle");
        assert_eq!(cycles[0].nodes.len(), 2);
        assert_eq!(cycles[0].edges.len(), 2, "expected 2 edge occurrences in cycle");
    }

    #[test]
    fn test_detect_self_loop() {
        // A → A
        let notes = simple_notes(&[
            ("1111111111", "A", "- [ ] @1111111111\n"),
        ]);
        let graph = build_dependency_graph(&notes);
        let cycles = detect_cycles(&graph);
        assert_eq!(cycles.len(), 1, "expected 1 cycle (self-loop)");
        assert_eq!(cycles[0].nodes.len(), 1);
        assert_eq!(cycles[0].edges.len(), 1);
    }

    #[test]
    fn test_no_cycle_on_dag() {
        // A → B → C (DAG)
        let notes = simple_notes(&[
            ("1111111111", "A", "- [ ] @2222222222\n"),
            ("2222222222", "B", "- [ ] @3333333333\n"),
            ("3333333333", "C", ""),
        ]);
        let graph = build_dependency_graph(&notes);
        let cycles = detect_cycles(&graph);
        assert!(cycles.is_empty(), "expected no cycles in a DAG");
    }

    #[test]
    fn test_render_format() {
        let notes = simple_notes(&[
            ("1111111111", "A", "- [ ] @2222222222\n"),
            ("2222222222", "B", "- [ ] @1111111111\n"),
        ]);
        let graph = build_dependency_graph(&notes);
        let cycles = detect_cycles(&graph);
        let rendered = render_cycle_errors(&cycles);
        assert!(rendered.contains("error:"), "should contain error:");
        assert!(rendered.contains("┌─"), "should contain ┌─ location marker");
        assert!(rendered.contains('│'), "should contain │ border");
        assert!(rendered.contains('^'), "should contain ^ underline");
        assert!(rendered.contains("cycle:"), "should contain cycle:");
        assert!(rendered.contains("->"), "should contain -> in cycle chain");
    }
}

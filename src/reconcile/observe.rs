/// Workspace snapshot — the observation layer for the Reconcile DSL.
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use crate::parser::{self, ChecklistItemKind, ChecklistStatus, Relation};

use super::types::{CheckboxId, NoteId, Status, Value};

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CheckboxObs {
    pub checked: bool,
    /// Empty = Local item; non-empty = Ref item with these note IDs as targets.
    pub targets: Vec<NoteId>,
}

#[derive(Debug, Clone)]
pub struct NoteObs {
    #[allow(dead_code)]
    pub relation: Relation,
    pub checklist_status: ChecklistStatus,
    /// Raw string values for generic `observe_meta` queries (e.g. "relation", "checklist-status").
    pub raw_meta: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// WorkspaceSnapshot
// ---------------------------------------------------------------------------

pub struct WorkspaceSnapshot {
    /// Note id → NoteObs
    pub notes: HashMap<NoteId, NoteObs>,
    /// Checkbox id → CheckboxObs
    pub checkboxes: HashMap<CheckboxId, CheckboxObs>,
    /// Note id → leaf checkbox IDs (for effective_status eval)
    pub note_leaf_checkboxes: HashMap<NoteId, Vec<CheckboxId>>,
}

impl WorkspaceSnapshot {
    pub fn observe_checked(&self, id: &CheckboxId) -> Option<bool> {
        self.checkboxes.get(id).map(|c| c.checked)
    }

    /// Generic metadata observation. Returns `Value::Status` for "checklist-status",
    /// `Value::String` for all other fields, and an empty string for unknown notes/fields.
    pub fn observe_meta(&self, note_id: &NoteId, field: &str) -> Value {
        let obs = match self.notes.get(note_id) {
            Some(o) => o,
            None => {
                return if field == "checklist-status" {
                    Value::Status(Status::None)
                } else {
                    Value::String(String::new())
                };
            }
        };
        match field {
            "checklist-status" => {
                let s = match obs.checklist_status {
                    ChecklistStatus::Done => Status::Done,
                    ChecklistStatus::Wip => Status::Wip,
                    ChecklistStatus::Todo => Status::Todo,
                    ChecklistStatus::None => Status::None,
                };
                Value::Status(s)
            }
            _ => {
                let s = obs.raw_meta.get(field).cloned().unwrap_or_default();
                Value::String(s)
            }
        }
    }

    #[allow(dead_code)]
    pub fn observe_meta_status(&self, note_id: &NoteId) -> Status {
        match self.observe_meta(note_id, "checklist-status") {
            Value::Status(s) => s,
            _ => Status::None,
        }
    }

    pub fn targets(&self, id: &CheckboxId) -> &[NoteId] {
        self.checkboxes
            .get(id)
            .map(|c| c.targets.as_slice())
            .unwrap_or(&[])
    }

    /// Leaf checkboxes only (used by `effective_status` evaluation).
    pub fn local_checkboxes(&self, note_id: &NoteId) -> &[CheckboxId] {
        self.note_leaf_checkboxes
            .get(note_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn all_note_ids(&self) -> impl Iterator<Item = &NoteId> {
        self.notes.keys()
    }

    #[allow(dead_code)]
    pub fn note_obs(&self, note_id: &NoteId) -> Option<&NoteObs> {
        self.notes.get(note_id)
    }

    // ---------------------------------------------------------------------------
    // Construction helpers
    // ---------------------------------------------------------------------------

    /// Build a snapshot from a map of `note_id → (path, content)`.
    /// Used in tests and in production (after async scan).
    pub fn from_note_map(notes: &HashMap<NoteId, (PathBuf, String)>) -> Self {
        let mut note_obs_map: HashMap<NoteId, NoteObs> = HashMap::new();
        let mut checkboxes: HashMap<CheckboxId, CheckboxObs> = HashMap::new();
        let mut note_leaf_checkboxes: HashMap<NoteId, Vec<CheckboxId>> = HashMap::new();

        for (id, (_path, content)) in notes {
            // Parse header for relation + checklist_status
            let (relation, checklist_status) = if let Some(header) = parser::parse_header(content) {
                let relation = if header.archived {
                    Relation::Archived
                } else {
                    // Parse full TOML to get relation
                    parser::find_toml_metadata_block(content)
                        .and_then(|b| parser::parse_toml_metadata(&b.toml_content))
                        .map(|m| m.relation)
                        .unwrap_or(Relation::Active)
                };
                let status = header.checklist_status.unwrap_or(ChecklistStatus::None);
                (relation, status)
            } else {
                (Relation::Active, ChecklistStatus::None)
            };

            let relation_str = match relation {
                Relation::Active => "active",
                Relation::Archived => "archived",
                Relation::Legacy => "legacy",
            };
            let status_str = match checklist_status {
                ChecklistStatus::Done => "done",
                ChecklistStatus::Wip => "wip",
                ChecklistStatus::Todo => "todo",
                ChecklistStatus::None => "none",
            };
            let mut raw_meta = HashMap::new();
            raw_meta.insert("relation".to_string(), relation_str.to_string());
            raw_meta.insert("checklist-status".to_string(), status_str.to_string());

            note_obs_map.insert(
                id.clone(),
                NoteObs {
                    relation,
                    checklist_status,
                    raw_meta,
                },
            );

            // Parse checklist items — classify leaves
            let items = parser::parse_checklist_items(content);
            let total = items.len();
            let mut leaf_ids: Vec<CheckboxId> = Vec::new();

            for (i, item) in items.iter().enumerate() {
                let cid = CheckboxId {
                    note_id: id.clone(),
                    line_idx: item.line_idx,
                };

                // Leaf: next item has indent <= this item, or this is the last item
                let is_leaf = if i + 1 >= total {
                    true
                } else {
                    items[i + 1].indent <= item.indent
                };

                let (checked, targets) = match &item.kind {
                    ChecklistItemKind::Local => (item.checked, vec![]),
                    ChecklistItemKind::Ref { targets } => {
                        let ids: Vec<NoteId> =
                            targets.iter().map(|t| t.target_id.clone()).collect();
                        (item.checked, ids)
                    }
                };

                checkboxes.insert(cid.clone(), CheckboxObs { checked, targets });
                if is_leaf {
                    leaf_ids.push(cid);
                }
            }

            note_leaf_checkboxes.insert(id.clone(), leaf_ids);
        }

        WorkspaceSnapshot {
            notes: note_obs_map,
            checkboxes,
            note_leaf_checkboxes,
        }
    }
}

/// Async constructor for production use.
#[allow(dead_code)]
pub async fn build_workspace_snapshot(note_dir: &Path) -> anyhow::Result<WorkspaceSnapshot> {
    let mut map: HashMap<NoteId, (PathBuf, String)> = HashMap::new();
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
    Ok(WorkspaceSnapshot::from_note_map(&map))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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

    fn make_toml_note_with_relation(
        title: &str,
        id: &str,
        status: &str,
        relation: &str,
        body: &str,
    ) -> String {
        format!(
            "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\n\
             \x20 ```toml\n\
             \x20 schema-version = 1\n\
             \x20 title = \"{title}\"\n\
             \x20 tags = []\n\
             \x20 checklist-status = \"{status}\"\n\
             \x20 relation = \"{relation}\"\n\
             \x20 relation-target = []\n\
             \x20 generated = false\n\
             \x20 ```.text,\n\
             ))\n\
             #show: zettel.with(metadata: zk-metadata)\n\
             \n\
             = {title} <{id}>\n\
             {body}"
        )
    }

    fn single_note_snapshot(id: &str, content: &str) -> WorkspaceSnapshot {
        let mut map = HashMap::new();
        map.insert(
            id.to_string(),
            (PathBuf::from(format!("{id}.typ")), content.to_string()),
        );
        WorkspaceSnapshot::from_note_map(&map)
    }

    #[test]
    fn leaf_only_local_checkboxes() {
        // parent has children; only children are leaves
        let body = "- [ ] parent\n  - [x] child1\n  - [x] child2\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let leaves = snap.local_checkboxes(&"1111111111".to_string());
        // 3 items total: parent (non-leaf), child1 (leaf), child2 (leaf)
        assert_eq!(leaves.len(), 2, "only leaves");
        for leaf in leaves {
            assert_ne!(leaf.line_idx, 0, "parent line should not be a leaf (it's on line with import/title, actual body starts further)");
        }
    }

    #[test]
    fn ref_item_targets_populated() {
        let body = "- [ ] @2222222222\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let leaves = snap.local_checkboxes(&"1111111111".to_string());
        assert_eq!(leaves.len(), 1);
        let targets = snap.targets(&leaves[0]);
        assert_eq!(targets, &["2222222222".to_string()]);
    }

    #[test]
    fn local_item_empty_targets() {
        let body = "- [ ] local task\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let leaves = snap.local_checkboxes(&"1111111111".to_string());
        assert_eq!(leaves.len(), 1);
        let targets = snap.targets(&leaves[0]);
        assert!(targets.is_empty(), "local item has no targets");
    }

    #[test]
    fn archived_note_relation() {
        let content = make_toml_note_with_relation("A", "1111111111", "none", "archived", "");
        let snap = single_note_snapshot("1111111111", &content);
        let obs = snap
            .note_obs(&"1111111111".to_string())
            .expect("note exists");
        assert_eq!(obs.relation, Relation::Archived);
    }

    #[test]
    fn meta_status_fallback() {
        let content = make_toml_note("A", "1111111111", "done", "");
        let snap = single_note_snapshot("1111111111", &content);
        let status = snap.observe_meta_status(&"1111111111".to_string());
        assert_eq!(status, Status::Done);
    }
}

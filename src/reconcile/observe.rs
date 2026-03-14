/// Workspace snapshot — the observation layer for the Reconcile DSL.
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use crate::config::{MetadataFieldConfig, MetadataFieldKind};
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
    /// Typed metadata values for generic `observe_meta` queries.
    pub raw_meta: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// WorkspaceSnapshot
// ---------------------------------------------------------------------------

pub struct WorkspaceSnapshot {
    /// Note id → NoteObs
    pub notes: HashMap<NoteId, NoteObs>,
    /// Checkbox id → CheckboxObs
    pub checkboxes: HashMap<CheckboxId, CheckboxObs>,
    /// Note id → all checklist IDs in source order.
    pub note_checkboxes: HashMap<NoteId, Vec<CheckboxId>>,
    /// Checkbox id → direct child checkbox IDs in source order.
    pub checkbox_children: HashMap<CheckboxId, Vec<CheckboxId>>,
    /// Config-driven default values for known metadata fields.
    pub metadata_defaults: HashMap<String, Value>,
}

impl WorkspaceSnapshot {
    pub fn observe_checked(&self, id: &CheckboxId) -> Option<Status> {
        self.checkboxes.get(id).map(|c| {
            if c.checked {
                Status::Done
            } else {
                Status::Todo
            }
        })
    }

    /// Generic metadata observation.
    pub fn observe_meta(&self, note_id: &NoteId, field: &str) -> Value {
        self.notes
            .get(note_id)
            .and_then(|obs| obs.raw_meta.get(field).cloned())
            .or_else(|| self.metadata_defaults.get(field).cloned())
            .unwrap_or_else(|| Value::String(Rc::<str>::from("")))
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

    /// All checklist items in source order.
    pub fn local_checkboxes(&self, note_id: &NoteId) -> &[CheckboxId] {
        self.note_checkboxes
            .get(note_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn children(&self, checkbox_id: &CheckboxId) -> &[CheckboxId] {
        self.checkbox_children
            .get(checkbox_id)
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
        Self::from_note_map_with_metadata(notes, &[])
    }

    pub fn from_note_map_with_metadata(
        notes: &HashMap<NoteId, (PathBuf, String)>,
        metadata_fields: &[MetadataFieldConfig],
    ) -> Self {
        let mut note_obs_map: HashMap<NoteId, NoteObs> = HashMap::new();
        let mut checkboxes: HashMap<CheckboxId, CheckboxObs> = HashMap::new();
        let mut note_checkboxes: HashMap<NoteId, Vec<CheckboxId>> = HashMap::new();
        let mut checkbox_children: HashMap<CheckboxId, Vec<CheckboxId>> = HashMap::new();
        let metadata_kinds = metadata_kind_map(metadata_fields);
        let metadata_defaults = metadata_default_map(metadata_fields);

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

            let raw_meta = extract_raw_meta(content, &metadata_kinds, checklist_status, &relation);

            note_obs_map.insert(id.clone(), NoteObs { relation, raw_meta });

            // Parse checklist items.
            let items = parser::parse_checklist_items(content);
            let mut checkbox_ids: Vec<CheckboxId> = Vec::new();
            let mut stack: Vec<(usize, CheckboxId)> = Vec::new();

            for item in &items {
                let cid = CheckboxId {
                    note_id: id.clone(),
                    line_idx: item.line_idx,
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
                while stack
                    .last()
                    .is_some_and(|(indent, _)| *indent >= item.indent)
                {
                    stack.pop();
                }
                if let Some((_, parent_id)) = stack.last() {
                    checkbox_children
                        .entry(parent_id.clone())
                        .or_default()
                        .push(cid.clone());
                }
                checkbox_children.entry(cid.clone()).or_default();
                stack.push((item.indent, cid.clone()));
                checkbox_ids.push(cid);
            }

            note_checkboxes.insert(id.clone(), checkbox_ids);
        }

        WorkspaceSnapshot {
            notes: note_obs_map,
            checkboxes,
            note_checkboxes,
            checkbox_children,
            metadata_defaults,
        }
    }
}

fn metadata_kind_map(
    metadata_fields: &[MetadataFieldConfig],
) -> HashMap<String, MetadataFieldKind> {
    metadata_fields
        .iter()
        .map(|field| (field.path.clone(), field.kind.clone()))
        .collect()
}

fn metadata_default_map(metadata_fields: &[MetadataFieldConfig]) -> HashMap<String, Value> {
    let mut defaults: HashMap<String, Value> = metadata_fields
        .iter()
        .map(|field| {
            (
                field.path.clone(),
                toml_value_to_typed_value(&field.default, Some(&field.kind)),
            )
        })
        .collect();
    defaults
        .entry("checklist-status".to_string())
        .or_insert(Value::Status(Status::None));
    defaults
        .entry("relation".to_string())
        .or_insert(Value::String(Rc::from("active")));
    defaults
}

fn extract_raw_meta(
    content: &str,
    metadata_kinds: &HashMap<String, MetadataFieldKind>,
    checklist_status: ChecklistStatus,
    relation: &Relation,
) -> HashMap<String, Value> {
    let mut raw_meta = HashMap::new();

    let Some(block) = parser::find_toml_metadata_block(content) else {
        return raw_meta;
    };
    let Ok(value) = block.toml_content.parse::<toml::Value>() else {
        return raw_meta;
    };
    let Some(table) = value.as_table() else {
        return raw_meta;
    };

    flatten_toml_table("", table, metadata_kinds, &mut raw_meta);
    raw_meta.insert(
        "checklist-status".to_string(),
        Value::Status(match checklist_status {
            ChecklistStatus::Done => Status::Done,
            ChecklistStatus::Wip => Status::Wip,
            ChecklistStatus::Todo => Status::Todo,
            ChecklistStatus::None => Status::None,
        }),
    );
    raw_meta.insert(
        "relation".to_string(),
        Value::String(Rc::from(match relation {
            Relation::Archived => "archived",
            Relation::Active => "active",
            Relation::Legacy => "legacy",
        })),
    );
    raw_meta
}

fn flatten_toml_table(
    prefix: &str,
    table: &toml::Table,
    metadata_kinds: &HashMap<String, MetadataFieldKind>,
    raw_meta: &mut HashMap<String, Value>,
) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        match value {
            toml::Value::Table(inner) => {
                flatten_toml_table(&full_key, inner, metadata_kinds, raw_meta)
            }
            _ => {
                raw_meta.insert(
                    full_key.clone(),
                    toml_value_to_typed_value(value, metadata_kinds.get(&full_key)),
                );
            }
        }
    }
}

fn toml_value_to_typed_value(value: &toml::Value, kind: Option<&MetadataFieldKind>) -> Value {
    match kind {
        Some(MetadataFieldKind::Boolean) => Value::Bool(value.as_bool().unwrap_or(false)),
        Some(MetadataFieldKind::ArrayString) => Value::List(Rc::new(
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(|s| Value::String(Rc::from(s))))
                        .collect()
                })
                .unwrap_or_default(),
        )),
        Some(MetadataFieldKind::String) => Value::String(Rc::from(toml_value_to_string(value))),
        None => Value::String(Rc::from(toml_value_to_string(value))),
    }
}

fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(dt) => dt.to_string(),
        toml::Value::Array(items) => {
            let rendered: Vec<String> = items.iter().map(toml_value_to_inline_string).collect();
            format!("[{}]", rendered.join(", "))
        }
        toml::Value::Table(table) => {
            let mut parts = Vec::new();
            for (key, item) in table {
                parts.push(format!("{key} = {}", toml_value_to_inline_string(item)));
            }
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn toml_value_to_inline_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),
        _ => toml_value_to_string(value),
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
    use crate::config::{MetadataFieldConfig, MetadataFieldKind};

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

    fn make_toml_note_with_extra_metadata(
        title: &str,
        id: &str,
        status: &str,
        extra_metadata: &str,
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
             \x20 generated = false\n\
             {extra_metadata}\
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

    fn single_note_snapshot_with_metadata(
        id: &str,
        content: &str,
        metadata_fields: &[MetadataFieldConfig],
    ) -> WorkspaceSnapshot {
        let mut map = HashMap::new();
        map.insert(
            id.to_string(),
            (PathBuf::from(format!("{id}.typ")), content.to_string()),
        );
        WorkspaceSnapshot::from_note_map_with_metadata(&map, metadata_fields)
    }

    #[test]
    fn local_checkboxes_include_parent_and_children() {
        let body = "- [ ] parent\n  - [x] child1\n  - [x] child2\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let checkboxes = snap.local_checkboxes(&"1111111111".to_string());
        assert_eq!(checkboxes.len(), 3);
        assert!(checkboxes[0].line_idx < checkboxes[1].line_idx);
        assert!(checkboxes[1].line_idx < checkboxes[2].line_idx);
    }

    #[test]
    fn children_returns_direct_children_only() {
        let body = "- [ ] parent\n  - [x] child1\n    - [x] grandchild\n  - [ ] child2\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let checkboxes = snap.local_checkboxes(&"1111111111".to_string());
        let parent_children = snap.children(&checkboxes[0]);
        assert_eq!(parent_children.len(), 2);
        assert_eq!(parent_children[0].line_idx, checkboxes[1].line_idx);
        assert_eq!(parent_children[1].line_idx, checkboxes[3].line_idx);

        let child_children = snap.children(&checkboxes[1]);
        assert_eq!(child_children.len(), 1);
        assert_eq!(child_children[0].line_idx, checkboxes[2].line_idx);
    }

    #[test]
    fn children_respects_sibling_boundaries() {
        let body = "- [ ] a\n  - [x] a1\n- [ ] b\n  - [x] b1\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let checkboxes = snap.local_checkboxes(&"1111111111".to_string());
        let a_children = snap.children(&checkboxes[0]);
        let b_children = snap.children(&checkboxes[2]);
        assert_eq!(a_children.len(), 1);
        assert_eq!(b_children.len(), 1);
        assert_eq!(a_children[0].line_idx, checkboxes[1].line_idx);
        assert_eq!(b_children[0].line_idx, checkboxes[3].line_idx);
    }

    #[test]
    fn ref_item_targets_populated() {
        let body = "- [ ] @2222222222\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let checkboxes = snap.local_checkboxes(&"1111111111".to_string());
        assert_eq!(checkboxes.len(), 1);
        let targets = snap.targets(&checkboxes[0]);
        assert_eq!(targets, &["2222222222".to_string()]);
    }

    #[test]
    fn local_item_empty_targets() {
        let body = "- [ ] local task\n";
        let content = make_toml_note("A", "1111111111", "none", body);
        let snap = single_note_snapshot("1111111111", &content);

        let checkboxes = snap.local_checkboxes(&"1111111111".to_string());
        assert_eq!(checkboxes.len(), 1);
        let targets = snap.targets(&checkboxes[0]);
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

    #[test]
    fn observe_meta_reads_custom_keys() {
        let metadata_fields = vec![
            MetadataFieldConfig {
                path: "user.kind".to_string(),
                kind: MetadataFieldKind::String,
                default: toml::Value::String(String::new()),
            },
            MetadataFieldConfig {
                path: "user.priority".to_string(),
                kind: MetadataFieldKind::Boolean,
                default: toml::Value::Boolean(false),
            },
            MetadataFieldConfig {
                path: "user.tags".to_string(),
                kind: MetadataFieldKind::ArrayString,
                default: toml::Value::Array(Vec::new()),
            },
        ];
        let content = make_toml_note_with_extra_metadata(
            "A",
            "1111111111",
            "none",
            "             user.kind = \"project\"\n             user.priority = true\n             user.tags = [\"alpha\", \"beta\"]\n",
            "",
        );
        let snap = single_note_snapshot_with_metadata("1111111111", &content, &metadata_fields);

        assert_eq!(
            snap.observe_meta(&"1111111111".to_string(), "user.kind"),
            Value::String("project".into())
        );
        assert_eq!(
            snap.observe_meta(&"1111111111".to_string(), "user.priority"),
            Value::Bool(true)
        );
        assert_eq!(
            snap.observe_meta(&"1111111111".to_string(), "user.tags"),
            Value::List(vec![Value::String("alpha".into()), Value::String("beta".into())].into())
        );
    }
}

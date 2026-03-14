/// Materialize layer — v1 identity projection.
use std::collections::HashMap;

use super::eval::EvalResult;
use super::types::{CheckboxId, NoteId, Status, Value};

/// v1: materialized == effective (identity projection).
pub struct ReconcileResult {
    #[allow(dead_code)]
    pub materialized_meta: HashMap<(NoteId, String), Value>,
    pub materialized_checked: HashMap<CheckboxId, bool>,
}

/// v1: identity — materialized == effective.
pub fn materialize(eval: EvalResult) -> ReconcileResult {
    ReconcileResult {
        materialized_meta: eval.effective_meta,
        materialized_checked: eval
            .effective_checked
            .into_iter()
            .map(|(cid, status)| (cid, status == Status::Done))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::reconcile::default_module::DEFAULT_MODULE;
    use crate::reconcile::eval::eval_all;
    use crate::reconcile::observe::WorkspaceSnapshot;
    use crate::reconcile::parser::parse_module;

    #[test]
    fn v1_identity() {
        let content = "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\n\
             \x20 ```toml\n\
             \x20 schema-version = 1\n\
             \x20 title = \"Test\"\n\
             \x20 tags = []\n\
             \x20 checklist-status = \"done\"\n\
             \x20 generated = false\n\
             \x20 ```.text,\n\
             ))\n\
             #show: zettel.with(metadata: zk-metadata)\n\
             \n\
             = Test <1111111111>\n";
        let map: std::collections::HashMap<NoteId, (PathBuf, String)> = [(
            "1111111111".to_string(),
            (PathBuf::from("1111111111.typ"), content.to_string()),
        )]
        .into_iter()
        .collect();
        let snap = WorkspaceSnapshot::from_note_map(&map);
        let module = parse_module(DEFAULT_MODULE).expect("parse");
        let eval_result = eval_all(&module, &snap);

        let result = materialize(eval_result);

        // checklist-status now materializes from the generic meta map.
        assert_eq!(
            result
                .materialized_meta
                .get(&("1111111111".to_string(), "checklist-status".to_string())),
            Some(&Value::Status(Status::Done))
        );
    }
}

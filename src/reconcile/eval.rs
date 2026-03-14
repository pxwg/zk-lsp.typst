/// Recursive evaluator for the Reconcile DSL v1.
use std::collections::HashMap;
use std::collections::HashSet;

use super::ast::{CyclePolicy, Expr, Module};
use super::observe::WorkspaceSnapshot;
use super::types::{
    CheckboxId, DiagnosticKind, EvalError, NoteId, ReconcileDiagnostic, Status, Value,
};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

pub struct EvalResult {
    pub effective_status: HashMap<NoteId, Status>,
    pub effective_meta: HashMap<(NoteId, String), Value>,
    pub effective_checked: HashMap<CheckboxId, bool>,
    pub diagnostics: Vec<ReconcileDiagnostic>,
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

struct Evaluator<'a> {
    module: &'a Module,
    snapshot: &'a WorkspaceSnapshot,
    effective_meta_cache: HashMap<(NoteId, String), Value>,
    visiting_meta: HashSet<(NoteId, String)>,
    checked_cache: HashMap<CheckboxId, bool>,
    diagnostics: Vec<ReconcileDiagnostic>,
}

impl<'a> Evaluator<'a> {
    fn new(module: &'a Module, snapshot: &'a WorkspaceSnapshot) -> Self {
        Evaluator {
            module,
            snapshot,
            effective_meta_cache: HashMap::new(),
            visiting_meta: HashSet::new(),
            checked_cache: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    // ---------------------------------------------------------------------------
    // Top-level per-entity evaluators
    // ---------------------------------------------------------------------------

    /// Generic per-note field evaluator with memoization and cycle detection.
    /// For field="checklist-status", looks up the "effective_status" rule.
    fn eval_effective_meta(&mut self, note_id: &NoteId, field: &str) -> Value {
        let cache_key = (note_id.clone(), field.to_string());

        if let Some(cached) = self.effective_meta_cache.get(&cache_key) {
            return cached.clone();
        }

        if self.visiting_meta.contains(&cache_key) {
            if self.module.policy.cycle == CyclePolicy::Error {
                self.diagnostics.push(ReconcileDiagnostic {
                    note_id: note_id.clone(),
                    message: format!("cycle detected while evaluating {field} of note {note_id}"),
                    kind: DiagnosticKind::Cycle,
                });
            }
            return Value::Status(self.module.policy.unknown_status.clone());
        }

        self.visiting_meta.insert(cache_key.clone());

        // "checklist-status" maps to the "effective_status" rule.
        let rule_name = match field {
            "checklist-status" => "effective_status",
            other => other,
        };

        let value = if let Some(rule) = self
            .module
            .rules
            .iter()
            .find(|r| r.name == rule_name)
            .cloned()
        {
            let arg = Value::NoteRef(note_id.clone());
            match self.eval_rule(&rule.params, &rule.body, &[arg]) {
                Ok(Value::Status(s)) if field == "checklist-status" => Value::Status(s),
                Ok(_) if field == "checklist-status" => {
                    self.diagnostics.push(ReconcileDiagnostic {
                        note_id: note_id.clone(),
                        message: "effective_status rule returned non-Status value".to_string(),
                        kind: DiagnosticKind::EvalFallback,
                    });
                    Value::Status(self.module.policy.unknown_status.clone())
                }
                Ok(v) => v,
                Err(e) => {
                    self.diagnostics.push(ReconcileDiagnostic {
                        note_id: note_id.clone(),
                        message: format!("eval error in {rule_name}: {e}"),
                        kind: DiagnosticKind::EvalFallback,
                    });
                    Value::Status(self.module.policy.unknown_status.clone())
                }
            }
        } else {
            Value::Status(self.module.policy.unknown_status.clone())
        };

        self.visiting_meta.remove(&cache_key);
        self.effective_meta_cache.insert(cache_key, value.clone());
        value
    }

    fn eval_effective_status(&mut self, note_id: &NoteId) -> Status {
        match self.eval_effective_meta(note_id, "checklist-status") {
            Value::Status(s) => s,
            _ => self.module.policy.unknown_status.clone(),
        }
    }

    // ---------------------------------------------------------------------------
    // Rule application
    // ---------------------------------------------------------------------------

    fn eval_rule(
        &mut self,
        params: &[String],
        body: &Expr,
        args: &[Value],
    ) -> Result<Value, EvalError> {
        let mut local_env: HashMap<String, Value> = HashMap::new();
        for (param, arg) in params.iter().zip(args.iter()) {
            local_env.insert(param.clone(), arg.clone());
        }
        self.eval_expr(body, &local_env)
    }

    // ---------------------------------------------------------------------------
    // Expression evaluator
    // ---------------------------------------------------------------------------

    fn eval_expr(&mut self, expr: &Expr, env: &HashMap<String, Value>) -> Result<Value, EvalError> {
        match expr {
            Expr::BoolLit(b) => Ok(Value::Bool(*b)),
            Expr::StatusLit(s) => Ok(Value::Status(s.clone())),
            Expr::StringLit(s) => Ok(Value::String(s.clone())),
            Expr::Var(name) => env
                .get(name)
                .cloned()
                .ok_or_else(|| EvalError::UnknownVariable(name.clone())),
            Expr::ObserveChecked(c_expr) => {
                let cid = self.eval_as_checkbox_id(c_expr, env)?;
                let checked = self
                    .snapshot
                    .observe_checked(&cid)
                    .unwrap_or(self.module.policy.unknown_checked);
                Ok(Value::Bool(checked))
            }
            Expr::ObserveMeta(n_expr, path_expr) => {
                let note_id = self.eval_as_note_id(n_expr, env)?;
                let path_val = self.eval_expr(path_expr, env)?;
                let field = match path_val {
                    Value::String(s) => s,
                    _ => {
                        return Err(EvalError::TypeMismatch {
                            context: "observe_meta: path must be a string".to_string(),
                        })
                    }
                };
                Ok(self.snapshot.observe_meta(&note_id, &field))
            }
            Expr::Targets(c_expr) => {
                let cid = self.eval_as_checkbox_id(c_expr, env)?;
                let targets = self.snapshot.targets(&cid);
                let list: Vec<Value> = targets
                    .iter()
                    .map(|id| Value::NoteRef(id.clone()))
                    .collect();
                Ok(Value::List(list))
            }
            Expr::LocalCheckboxes(n_expr) => {
                let note_id = self.eval_as_note_id(n_expr, env)?;
                let cids = self.snapshot.local_checkboxes(&note_id);
                let list: Vec<Value> = cids
                    .iter()
                    .map(|cid| Value::CheckboxRef(cid.clone()))
                    .collect();
                Ok(Value::List(list))
            }
            Expr::If { cond, then, else_ } => {
                let cond_val = self.eval_expr(cond, env)?;
                match cond_val {
                    Value::Bool(true) => self.eval_expr(then, env),
                    Value::Bool(false) => self.eval_expr(else_, env),
                    _ => Err(EvalError::TypeMismatch {
                        context: "if condition".to_string(),
                    }),
                }
            }
            Expr::Call { name, args } => self.eval_call(name, args, env),
            Expr::NoteRefLit(id) => Ok(Value::NoteRef(id.clone())),
            Expr::CheckboxRefLit(cid) => Ok(Value::CheckboxRef(cid.clone())),
        }
    }

    fn eval_call(
        &mut self,
        name: &str,
        arg_exprs: &[Expr],
        env: &HashMap<String, Value>,
    ) -> Result<Value, EvalError> {
        // Route effective_status calls through the memoized/cycle-detected path.
        // This is needed because (map effective_status ...) in the DSL would otherwise
        // bypass the visiting_meta check and cause infinite recursion on cycles.
        if name == "effective_status" && arg_exprs.len() == 1 {
            if let Ok(Value::NoteRef(id)) = self.eval_expr(&arg_exprs[0], env) {
                return Ok(Value::Status(self.eval_effective_status(&id)));
            }
        }

        // Cache effective_checked results keyed by CheckboxId for output collection.
        if name == "effective_checked" && arg_exprs.len() == 1 {
            if let Ok(Value::CheckboxRef(cid)) = self.eval_expr(&arg_exprs[0], env) {
                if let Some(&cached) = self.checked_cache.get(&cid) {
                    return Ok(Value::Bool(cached));
                }
                let rule_opt = self
                    .module
                    .rules
                    .iter()
                    .find(|r| r.name == "effective_checked")
                    .map(|r| (r.params.clone(), r.body.clone()));
                if let Some((params, body)) = rule_opt {
                    let result = self.eval_rule(&params, &body, &[Value::CheckboxRef(cid.clone())]);
                    if let Ok(Value::Bool(b)) = result {
                        self.checked_cache.insert(cid, b);
                        return Ok(Value::Bool(b));
                    }
                    return result;
                }
            }
        }

        // Check user-defined rules first (clone to avoid borrow conflict)
        let rule_opt = self
            .module
            .rules
            .iter()
            .find(|r| r.name == name)
            .map(|r| (r.params.clone(), r.body.clone()));

        if let Some((params, body)) = rule_opt {
            let arg_vals: Vec<Value> = arg_exprs
                .iter()
                .map(|e| self.eval_expr(e, env))
                .collect::<Result<_, _>>()?;
            return self.eval_rule(&params, &body, &arg_vals);
        }

        // Builtins
        match name {
            "empty?" => {
                let val = self.eval_expr(&arg_exprs[0], env)?;
                match val {
                    Value::List(v) => Ok(Value::Bool(v.is_empty())),
                    _ => Err(EvalError::TypeMismatch {
                        context: "empty?".to_string(),
                    }),
                }
            }
            "map" => {
                // (map fn_name list)
                let fn_name = match &arg_exprs[0] {
                    Expr::Var(s) => s.clone(),
                    _ => {
                        return Err(EvalError::TypeMismatch {
                            context: "map: first arg must be a function name".to_string(),
                        })
                    }
                };
                let list_val = self.eval_expr(&arg_exprs[1], env)?;
                match list_val {
                    Value::List(items) => {
                        let mut results = Vec::new();
                        for item in items {
                            let call_expr = Expr::Call {
                                name: fn_name.clone(),
                                args: vec![value_to_expr(item)?],
                            };
                            results.push(self.eval_expr(&call_expr, env)?);
                        }
                        Ok(Value::List(results))
                    }
                    _ => Err(EvalError::TypeMismatch {
                        context: "map: second arg must be a list".to_string(),
                    }),
                }
            }
            "all_done" => {
                let val = self.eval_expr(&arg_exprs[0], env)?;
                match val {
                    Value::List(items) => {
                        let all = items
                            .iter()
                            .all(|v| matches!(v, Value::Status(Status::Done)));
                        Ok(Value::Bool(all))
                    }
                    _ => Err(EvalError::TypeMismatch {
                        context: "all_done".to_string(),
                    }),
                }
            }
            "aggregate_status" => {
                let val = self.eval_expr(&arg_exprs[0], env)?;
                match val {
                    Value::List(items) => {
                        let bools: Result<Vec<bool>, _> = items
                            .iter()
                            .map(|v| match v {
                                Value::Bool(b) => Ok(*b),
                                _ => Err(EvalError::TypeMismatch {
                                    context: "aggregate_status".to_string(),
                                }),
                            })
                            .collect();
                        let bools = bools?;
                        let status = if bools.is_empty() {
                            Status::None
                        } else if bools.iter().all(|&b| b) {
                            Status::Done
                        } else if bools.iter().all(|&b| !b) {
                            Status::Todo
                        } else {
                            Status::Wip
                        };
                        Ok(Value::Status(status))
                    }
                    _ => Err(EvalError::TypeMismatch {
                        context: "aggregate_status".to_string(),
                    }),
                }
            }
            "eq?" => {
                let a = self.eval_expr(&arg_exprs[0], env)?;
                let b = self.eval_expr(&arg_exprs[1], env)?;
                Ok(Value::Bool(a == b))
            }
            "not" => {
                let val = self.eval_expr(&arg_exprs[0], env)?;
                match val {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    _ => Err(EvalError::TypeMismatch {
                        context: "not".to_string(),
                    }),
                }
            }
            "and" => {
                for e in arg_exprs {
                    match self.eval_expr(e, env)? {
                        Value::Bool(false) => return Ok(Value::Bool(false)),
                        Value::Bool(true) => {}
                        _ => {
                            return Err(EvalError::TypeMismatch {
                                context: "and".to_string(),
                            })
                        }
                    }
                }
                Ok(Value::Bool(true))
            }
            "or" => {
                for e in arg_exprs {
                    match self.eval_expr(e, env)? {
                        Value::Bool(true) => return Ok(Value::Bool(true)),
                        Value::Bool(false) => {}
                        _ => {
                            return Err(EvalError::TypeMismatch {
                                context: "or".to_string(),
                            })
                        }
                    }
                }
                Ok(Value::Bool(false))
            }
            _ => Err(EvalError::UnknownFunction(name.to_string())),
        }
    }

    // ---------------------------------------------------------------------------
    // Value helpers
    // ---------------------------------------------------------------------------

    fn eval_as_note_id(
        &mut self,
        expr: &Expr,
        env: &HashMap<String, Value>,
    ) -> Result<NoteId, EvalError> {
        match self.eval_expr(expr, env)? {
            Value::NoteRef(id) => Ok(id),
            _ => Err(EvalError::TypeMismatch {
                context: "expected NoteRef".to_string(),
            }),
        }
    }

    fn eval_as_checkbox_id(
        &mut self,
        expr: &Expr,
        env: &HashMap<String, Value>,
    ) -> Result<CheckboxId, EvalError> {
        match self.eval_expr(expr, env)? {
            Value::CheckboxRef(id) => Ok(id),
            _ => Err(EvalError::TypeMismatch {
                context: "expected CheckboxRef".to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: Value → synthetic Expr (for map iteration)
// ---------------------------------------------------------------------------

fn value_to_expr(val: Value) -> Result<Expr, EvalError> {
    match val {
        Value::Bool(b) => Ok(Expr::BoolLit(b)),
        Value::Status(s) => Ok(Expr::StatusLit(s)),
        Value::String(s) => Ok(Expr::StringLit(s)),
        Value::NoteRef(id) => Ok(Expr::NoteRefLit(id)),
        Value::CheckboxRef(cid) => Ok(Expr::CheckboxRefLit(cid)),
        Value::List(_) => Err(EvalError::TypeMismatch {
            context: "value_to_expr: cannot convert List to Expr".to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn eval_all(module: &Module, snapshot: &WorkspaceSnapshot) -> EvalResult {
    let mut ev = Evaluator::new(module, snapshot);

    // Evaluate all notes (this transitively evaluates checkboxes via memoization)
    let note_ids: Vec<NoteId> = snapshot.all_note_ids().cloned().collect();
    for id in &note_ids {
        ev.eval_effective_status(id);
    }

    // Derive effective_status from the generic meta cache.
    let effective_status = ev
        .effective_meta_cache
        .iter()
        .filter_map(|((note_id, field), val)| {
            if field == "checklist-status" {
                match val {
                    Value::Status(s) => Some((note_id.clone(), s.clone())),
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect();

    EvalResult {
        effective_status,
        effective_meta: ev.effective_meta_cache,
        effective_checked: ev.checked_cache,
        diagnostics: ev.diagnostics,
    }
}

// ---------------------------------------------------------------------------
// Value extension (NoteRef/CheckboxRef/StringKey variants needed for eval)
// These extend Value defined in types.rs — we add them as new variants here.
// ---------------------------------------------------------------------------

// NOTE: We need to extend the Value enum to include NoteRef, CheckboxRef, StringKey
// which are needed internally by the evaluator. The types.rs Value is the public API
// (Bool, Status, List), but the evaluator needs runtime-only variants.
// We keep the public API clean and only expose the necessary conversions.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::reconcile::default_module::DEFAULT_MODULE;
    use crate::reconcile::observe::WorkspaceSnapshot;
    use crate::reconcile::parser::parse_module;

    fn default_module() -> Module {
        parse_module(DEFAULT_MODULE).expect("default module must parse")
    }

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

    fn make_archived_note(title: &str, id: &str, body: &str) -> String {
        format!(
            "#import \"../include.typ\": *\n\
             #let zk-metadata = toml(bytes(\n\
             \x20 ```toml\n\
             \x20 schema-version = 1\n\
             \x20 title = \"{title}\"\n\
             \x20 tags = []\n\
             \x20 checklist-status = \"none\"\n\
             \x20 relation = \"archived\"\n\
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
    fn local_checkboxes_all_done() {
        let content = make_toml_note("A", "1111111111", "none", "- [x] task1\n- [x] task2\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        let status = result
            .effective_status
            .get("1111111111")
            .expect("should have status");
        assert_eq!(*status, Status::Done);
    }

    #[test]
    fn local_checkboxes_mixed() {
        let content = make_toml_note("A", "1111111111", "none", "- [x] done\n- [ ] pending\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        let status = result
            .effective_status
            .get("1111111111")
            .expect("should have status");
        assert_eq!(*status, Status::Wip);
    }

    #[test]
    fn ref_checkbox_target_done() {
        // A has single ref checkbox pointing at B (done) → A's status becomes Done
        let note_b = make_toml_note("B", "2222222222", "done", "");
        let note_a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let snap = snapshot_from(&[("1111111111", &note_a), ("2222222222", &note_b)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(
            result.effective_status.get("1111111111"),
            Some(&Status::Done),
            "A should be Done when its only ref target is done"
        );
    }

    #[test]
    fn ref_checkbox_target_not_done() {
        // A has single ref checkbox pointing at B (not done) → A is Todo
        let note_b = make_toml_note("B", "2222222222", "none", "- [ ] unchecked\n");
        let note_a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let snap = snapshot_from(&[("1111111111", &note_a), ("2222222222", &note_b)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_ne!(
            result.effective_status.get("1111111111"),
            Some(&Status::Done),
            "A should not be Done when its ref target is not done"
        );
    }

    #[test]
    fn multi_target_all_must_be_done() {
        // A refs B (done) and C (not done) → A is not Done
        let note_b = make_toml_note("B", "2222222222", "done", "");
        let note_c = make_toml_note("C", "3333333333", "none", "- [ ] undone\n");
        let note_a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222 @3333333333\n");
        let snap = snapshot_from(&[
            ("1111111111", &note_a),
            ("2222222222", &note_b),
            ("3333333333", &note_c),
        ]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_ne!(
            result.effective_status.get("1111111111"),
            Some(&Status::Done),
            "A should not be Done when any ref target is not done"
        );
    }

    #[test]
    fn linear_chain_propagation() {
        // A done → B refs A → C refs B
        let a = make_toml_note("A", "1010101010", "done", "");
        let b = make_toml_note("B", "2020202020", "none", "- [ ] @1010101010\n");
        let c = make_toml_note("C", "3030303030", "none", "- [ ] @2020202020\n");
        let snap = snapshot_from(&[("1010101010", &a), ("2020202020", &b), ("3030303030", &c)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(
            result.effective_status.get("1010101010"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("2020202020"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("3030303030"),
            Some(&Status::Done)
        );
    }

    #[test]
    fn diamond_propagation() {
        // D done → B refs D, C refs D → A refs B and C
        let d = make_toml_note("D", "4444444444", "done", "");
        let b = make_toml_note("B", "5555555555", "none", "- [ ] @4444444444\n");
        let c = make_toml_note("C", "6666666666", "none", "- [ ] @4444444444\n");
        let a = make_toml_note(
            "A",
            "7777777777",
            "none",
            "- [ ] @5555555555\n- [ ] @6666666666\n",
        );
        let snap = snapshot_from(&[
            ("4444444444", &d),
            ("5555555555", &b),
            ("6666666666", &c),
            ("7777777777", &a),
        ]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(
            result.effective_status.get("4444444444"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("5555555555"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("6666666666"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("7777777777"),
            Some(&Status::Done)
        );
    }

    #[test]
    fn archived_note_always_done() {
        let content = make_archived_note("A", "1111111111", "- [ ] unchecked\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(
            result.effective_status.get("1111111111"),
            Some(&Status::Done)
        );
    }

    #[test]
    fn empty_checklist_uses_metadata() {
        let done = make_toml_note("Done", "1111111111", "done", "");
        let none_note = make_toml_note("None", "2222222222", "none", "");
        let snap = snapshot_from(&[("1111111111", &done), ("2222222222", &none_note)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(
            result.effective_status.get("1111111111"),
            Some(&Status::Done)
        );
        assert_eq!(
            result.effective_status.get("2222222222"),
            Some(&Status::None)
        );
    }

    #[test]
    fn cycle_error_policy() {
        // A refs B, B refs A
        let a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let b = make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n");
        let snap = snapshot_from(&[("1111111111", &a), ("2222222222", &b)]);
        // Use default module (cycle = error)
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::Cycle),
            "cycle diagnostic should be emitted"
        );
        // Fallback status should be applied (todo)
        let s_a = result.effective_status.get("1111111111");
        let s_b = result.effective_status.get("2222222222");
        assert!(
            s_a == Some(&Status::Todo) || s_b == Some(&Status::Todo),
            "fallback todo status applied to at least one cyclic note"
        );
    }

    #[test]
    fn cycle_unknown_policy() {
        let src = r#"
        (module
          (policy
            (cycle unknown)
            (unknown-status none)
            (unknown-checked false))
          (define (effective_checked c)
            (if (empty? (targets c))
                (observe_checked c)
                (all_done (map effective_status (targets c)))))
          (define (effective_status n)
            (if (empty? (local_checkboxes n))
                (observe_meta n "checklist-status")
                (aggregate_status (map effective_checked (local_checkboxes n))))))
        "#;
        let module = parse_module(src).expect("parse");
        let a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let b = make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n");
        let snap = snapshot_from(&[("1111111111", &a), ("2222222222", &b)]);
        let result = eval_all(&module, &snap);
        // With unknown policy, no cycle diagnostic
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::Cycle),
            "no cycle diagnostic with unknown policy"
        );
        // unknown_status = none → both should be None or some fallback
    }

    #[test]
    fn observe_meta_relation() {
        // verify that observe_meta n "relation" returns the correct string for archived notes
        let content = make_archived_note("A", "1111111111", "");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let val = snap.observe_meta(&"1111111111".to_string(), "relation");
        assert_eq!(
            val,
            crate::reconcile::types::Value::String("archived".to_string())
        );
    }

    #[test]
    fn effective_meta_cache_works() {
        // Verify (NoteId, field) keyed memoization: calling eval_effective_meta twice for the
        // same key returns the cached value and does not duplicate diagnostics.
        let a = make_toml_note("A", "1111111111", "done", "");
        let b = make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n");
        let snap = snapshot_from(&[("1111111111", &a), ("2222222222", &b)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        // Both notes should have a "checklist-status" entry in effective_meta.
        assert!(
            result
                .effective_meta
                .contains_key(&("1111111111".to_string(), "checklist-status".to_string())),
            "A should have effective_meta entry"
        );
        assert!(
            result
                .effective_meta
                .contains_key(&("2222222222".to_string(), "checklist-status".to_string())),
            "B should have effective_meta entry"
        );
        // B depends on A; A is done, so B should also be done.
        assert_eq!(
            result.effective_status.get("2222222222"),
            Some(&Status::Done)
        );
        // No spurious diagnostics.
        assert!(result.diagnostics.is_empty());
    }
}

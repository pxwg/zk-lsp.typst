/// Recursive evaluator for the Reconcile DSL v1.
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::{CyclePolicy, Expr, Module, Rule};
use super::observe::WorkspaceSnapshot;
use super::typecheck::{self, TypeInfo};
use super::types::{
    CheckboxId, DiagnosticKind, DiagnosticSeverity, EvalError, NoteId, ReconcileDiagnostic, Status,
    Type, Value,
};

pub struct EvalResult {
    pub effective_meta: HashMap<(NoteId, String), Value>,
    pub effective_checked: HashMap<CheckboxId, Status>,
    pub diagnostics: Vec<ReconcileDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CallKey {
    name: String,
    args: Vec<Value>,
}

struct EnvFrame<'a> {
    bindings: HashMap<String, Value>,
    parent: Option<&'a EnvFrame<'a>>,
}

impl<'a> EnvFrame<'a> {
    fn empty() -> Self {
        Self {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    fn child(parent: &'a EnvFrame<'a>, bindings: HashMap<String, Value>) -> Self {
        Self {
            bindings,
            parent: Some(parent),
        }
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.bindings
            .get(name)
            .or_else(|| self.parent.and_then(|parent| parent.get(name)))
    }
}

struct Evaluator<'a> {
    module: &'a Module,
    snapshot: &'a WorkspaceSnapshot,
    type_info: &'a TypeInfo,
    call_cache: HashMap<CallKey, Value>,
    call_stack: Vec<CallKey>,
    diagnostics: Vec<ReconcileDiagnostic>,
}

impl<'a> Evaluator<'a> {
    fn new(module: &'a Module, snapshot: &'a WorkspaceSnapshot, type_info: &'a TypeInfo) -> Self {
        Self {
            module,
            snapshot,
            type_info,
            call_cache: HashMap::new(),
            call_stack: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn eval_expr(&mut self, expr: &Expr, env: &EnvFrame<'_>) -> Result<Value, EvalError> {
        match expr {
            Expr::Lit(value) => Ok(value.clone()),
            Expr::Var(name) => env
                .get(name)
                .cloned()
                .ok_or_else(|| EvalError::UnknownVariable(name.clone())),
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
        }
    }

    fn eval_call(
        &mut self,
        name: &str,
        arg_exprs: &[Expr],
        env: &EnvFrame<'_>,
    ) -> Result<Value, EvalError> {
        match name {
            "and" => {
                for expr in arg_exprs {
                    match self.eval_expr(expr, env)? {
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
                for expr in arg_exprs {
                    match self.eval_expr(expr, env)? {
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
            "map" => {
                let fn_name = match arg_exprs.first() {
                    Some(Expr::Var(name)) => name.clone(),
                    _ => {
                        return Err(EvalError::TypeMismatch {
                            context: "map: first arg must be a function symbol".to_string(),
                        })
                    }
                };
                let list_val = self.eval_expr(
                    arg_exprs.get(1).ok_or_else(|| EvalError::TypeMismatch {
                        context: "map: missing list arg".to_string(),
                    })?,
                    env,
                )?;
                let Value::List(items) = list_val else {
                    return Err(EvalError::TypeMismatch {
                        context: "map: second arg must be a list".to_string(),
                    });
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items.iter() {
                    results.push(self.invoke_function(&fn_name, vec![item.clone()])?);
                }
                Ok(Value::List(Rc::new(results)))
            }
            "list" => {
                let values = arg_exprs
                    .iter()
                    .map(|expr| self.eval_expr(expr, env))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::List(Rc::new(values)))
            }
            _ => {
                let args = arg_exprs
                    .iter()
                    .map(|expr| self.eval_expr(expr, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.invoke_function(name, args)
            }
        }
    }

    fn invoke_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
        let key = CallKey {
            name: name.to_string(),
            args: args.clone(),
        };

        if let Some(cached) = self.call_cache.get(&key) {
            return Ok(cached.clone());
        }

        if self.call_stack.contains(&key) {
            let fallback = self.cycle_fallback(name, &args);
            return Ok(fallback);
        }

        self.call_stack.push(key.clone());
        let result = if let Some(rule) = self.module.rules.iter().find(|rule| rule.name == name) {
            self.eval_rule(rule, &args)
        } else {
            self.eval_builtin(name, &args)
        };
        self.call_stack.pop();

        if let Ok(value) = &result {
            self.call_cache.insert(key, value.clone());
        }

        result
    }

    fn eval_rule(&mut self, rule: &Rule, args: &[Value]) -> Result<Value, EvalError> {
        let mut bindings = HashMap::new();
        for (param, arg) in rule.params.iter().zip(args.iter()) {
            bindings.insert(param.clone(), arg.clone());
        }
        let root = EnvFrame::empty();
        let env = EnvFrame::child(&root, bindings);
        self.eval_expr(&rule.body, &env)
    }

    fn eval_builtin(&mut self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "empty?" => match args {
                [Value::List(items)] => Ok(Value::Bool(items.is_empty())),
                _ => Err(EvalError::TypeMismatch {
                    context: "empty?".to_string(),
                }),
            },
            "all_done" | "all_done?" => match args {
                [Value::List(items)] => Ok(Value::Bool(
                    items
                        .iter()
                        .all(|item| matches!(item, Value::Status(Status::Done))),
                )),
                _ => Err(EvalError::TypeMismatch {
                    context: "all_done".to_string(),
                }),
            },
            "aggregate_status" => match args {
                [Value::List(items)] => {
                    let statuses = items
                        .iter()
                        .map(|item| match item {
                            Value::Status(status) => Ok(status.clone()),
                            _ => Err(EvalError::TypeMismatch {
                                context: "aggregate_status".to_string(),
                            }),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Value::Status(aggregate_status(&statuses)))
                }
                _ => Err(EvalError::TypeMismatch {
                    context: "aggregate_status".to_string(),
                }),
            },
            "list" => Ok(Value::List(Rc::new(args.to_vec()))),
            "eq?" => match args {
                [left, right] => Ok(Value::Bool(left == right)),
                _ => Err(EvalError::TypeMismatch {
                    context: "eq?".to_string(),
                }),
            },
            "not" => match args {
                [Value::Bool(value)] => Ok(Value::Bool(!value)),
                _ => Err(EvalError::TypeMismatch {
                    context: "not".to_string(),
                }),
            },
            "done?" => status_predicate(args, |status| status == Status::Done),
            "todo?" => status_predicate(args, |status| status == Status::Todo),
            "wip?" => status_predicate(args, |status| status == Status::Wip),
            "none?" => status_predicate(args, |status| status == Status::None),
            "observe_checked" => match args {
                [Value::CheckboxRef(id)] => Ok(Value::Status(
                    self.snapshot
                        .observe_checked(id)
                        .unwrap_or_else(|| self.module.policy.unknown_status.clone()),
                )),
                _ => Err(EvalError::TypeMismatch {
                    context: "observe_checked".to_string(),
                }),
            },
            "observe_meta" => match args {
                [Value::NoteRef(id), Value::String(field)] => {
                    Ok(self.snapshot.observe_meta(id, field.as_ref()))
                }
                _ => Err(EvalError::TypeMismatch {
                    context: "observe_meta".to_string(),
                }),
            },
            "targets" => match args {
                [Value::CheckboxRef(id)] => Ok(Value::List(Rc::new(
                    self.snapshot
                        .targets(id)
                        .iter()
                        .map(|note_id| Value::NoteRef(note_id.clone()))
                        .collect(),
                ))),
                _ => Err(EvalError::TypeMismatch {
                    context: "targets".to_string(),
                }),
            },
            "children" => match args {
                [Value::CheckboxRef(id)] => Ok(Value::List(Rc::new(
                    self.snapshot
                        .children(id)
                        .iter()
                        .map(|child_id| Value::CheckboxRef(child_id.clone()))
                        .collect(),
                ))),
                _ => Err(EvalError::TypeMismatch {
                    context: "children".to_string(),
                }),
            },
            "local_checkboxes" => match args {
                [Value::NoteRef(id)] => Ok(Value::List(Rc::new(
                    self.snapshot
                        .local_checkboxes(id)
                        .iter()
                        .map(|cid| Value::CheckboxRef(cid.clone()))
                        .collect(),
                ))),
                _ => Err(EvalError::TypeMismatch {
                    context: "local_checkboxes".to_string(),
                }),
            },
            _ => Err(EvalError::UnknownFunction(name.to_string())),
        }
    }

    fn cycle_fallback(&mut self, name: &str, args: &[Value]) -> Value {
        let note_id = args.iter().find_map(|arg| match arg {
            Value::NoteRef(id) => Some(id.clone()),
            Value::CheckboxRef(cid) => Some(cid.note_id.clone()),
            _ => None,
        });

        if self.module.policy.cycle == CyclePolicy::Error {
            self.diagnostics.push(ReconcileDiagnostic {
                note_id: note_id.unwrap_or_default(),
                message: format!("cycle detected while evaluating {name}"),
                kind: DiagnosticKind::Cycle,
                severity: DiagnosticSeverity::Error,
                location: None,
            });
        }

        self.unknown_value_for_type(self.call_return_type(name, args))
    }

    fn call_return_type(&self, name: &str, args: &[Value]) -> Type {
        match name {
            "empty?" | "all_done" | "all_done?" | "eq?" | "not" | "and" | "or" => Type::Bool,
            "done?" | "todo?" | "wip?" | "none?" => Type::Bool,
            "observe_checked" | "aggregate_status" => Type::Status,
            "observe_meta" => match args.get(1) {
                Some(Value::String(field)) => self
                    .snapshot
                    .metadata_defaults
                    .get(field.as_ref())
                    .map(value_type)
                    .unwrap_or(Type::String),
                _ => Type::Any,
            },
            "targets" => Type::List(Box::new(Type::NoteRef)),
            "children" | "local_checkboxes" => Type::List(Box::new(Type::CheckboxRef)),
            "map" | "list" => Type::List(Box::new(Type::Any)),
            _ => self
                .type_info
                .rule_return_types
                .get(name)
                .cloned()
                .unwrap_or(Type::Any),
        }
    }

    fn unknown_value_for_type(&self, ty: Type) -> Value {
        match ty {
            Type::Bool => Value::Bool(false),
            Type::Status => Value::Status(self.module.policy.unknown_status.clone()),
            Type::String | Type::Any => Value::String(Rc::from("")),
            Type::List(_) => Value::List(Rc::new(Vec::new())),
            Type::NoteRef => Value::NoteRef(String::new()),
            Type::CheckboxRef => Value::CheckboxRef(CheckboxId {
                note_id: String::new(),
                line_idx: 0,
            }),
        }
    }
}

fn value_type(value: &Value) -> Type {
    match value {
        Value::Bool(_) => Type::Bool,
        Value::Status(_) => Type::Status,
        Value::List(items) => items
            .first()
            .map(value_type)
            .map(|inner| Type::List(Box::new(inner)))
            .unwrap_or(Type::List(Box::new(Type::Any))),
        Value::NoteRef(_) => Type::NoteRef,
        Value::CheckboxRef(_) => Type::CheckboxRef,
        Value::String(_) => Type::String,
    }
}

fn status_predicate(
    args: &[Value],
    predicate: impl Fn(Status) -> bool,
) -> Result<Value, EvalError> {
    match args {
        [Value::Status(status)] => Ok(Value::Bool(predicate(status.clone()))),
        _ => Err(EvalError::TypeMismatch {
            context: "status predicate".to_string(),
        }),
    }
}

fn aggregate_status(statuses: &[Status]) -> Status {
    if statuses.is_empty() {
        return Status::None;
    }
    if statuses.iter().all(|status| *status == Status::Done) {
        return Status::Done;
    }
    if statuses.iter().all(|status| *status == Status::Todo) {
        return Status::Todo;
    }
    Status::Wip
}

#[allow(dead_code)]
pub fn eval_all(module: &Module, snapshot: &WorkspaceSnapshot) -> EvalResult {
    let type_info =
        typecheck::type_check_module(module).expect("module must typecheck before evaluation");
    eval_all_typed(module, snapshot, &type_info)
}

pub fn eval_all_typed(
    module: &Module,
    snapshot: &WorkspaceSnapshot,
    type_info: &TypeInfo,
) -> EvalResult {
    let mut ev = Evaluator::new(module, snapshot, type_info);
    for note_id in snapshot.all_note_ids() {
        let fields = ev
            .invoke_function("materialized_fields", vec![Value::NoteRef(note_id.clone())])
            .unwrap_or_else(|_| Value::List(Rc::new(Vec::new())));
        let Value::List(fields) = fields else {
            ev.diagnostics.push(ReconcileDiagnostic {
                note_id: note_id.clone(),
                message: "materialized_fields returned non-List".to_string(),
                kind: DiagnosticKind::EvalFallback,
                severity: DiagnosticSeverity::Error,
                location: None,
            });
            continue;
        };
        for field in fields.iter() {
            let Value::String(field_name) = field else {
                ev.diagnostics.push(ReconcileDiagnostic {
                    note_id: note_id.clone(),
                    message: "materialized_fields contains non-String item".to_string(),
                    kind: DiagnosticKind::EvalFallback,
                    severity: DiagnosticSeverity::Error,
                    location: None,
                });
                continue;
            };
            let _ = ev.invoke_function(
                "effective_meta",
                vec![
                    Value::NoteRef(note_id.clone()),
                    Value::String(field_name.clone()),
                ],
            );
        }
    }

    let effective_meta = ev
        .call_cache
        .iter()
        .filter_map(|(key, value)| {
            if key.name == "effective_meta" && key.args.len() == 2 {
                match (&key.args[0], &key.args[1]) {
                    (Value::NoteRef(note_id), Value::String(field)) => {
                        Some(((note_id.clone(), field.to_string()), value.clone()))
                    }
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect::<HashMap<_, _>>();

    let effective_checked = ev
        .call_cache
        .iter()
        .filter_map(|(key, value)| {
            if key.name == "effective_checked" && key.args.len() == 1 {
                match (&key.args[0], value) {
                    (Value::CheckboxRef(cid), Value::Status(status)) => {
                        Some((cid.clone(), status.clone()))
                    }
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect::<HashMap<_, _>>();

    EvalResult {
        effective_meta,
        effective_checked,
        diagnostics: ev.diagnostics,
    }
}

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

    fn effective_status(result: &EvalResult, note_id: &str) -> Option<Status> {
        result
            .effective_meta
            .get(&(note_id.to_string(), "checklist-status".to_string()))
            .and_then(|value| match value {
                Value::Status(status) => Some(status.clone()),
                _ => None,
            })
    }

    #[test]
    fn local_checkboxes_all_done() {
        let content = make_toml_note("A", "1111111111", "none", "- [x] task1\n- [x] task2\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(effective_status(&result, "1111111111"), Some(Status::Done));
    }

    #[test]
    fn local_checkboxes_mixed() {
        let content = make_toml_note("A", "1111111111", "none", "- [x] done\n- [ ] pending\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(effective_status(&result, "1111111111"), Some(Status::Wip));
    }

    #[test]
    fn ref_checkbox_target_done() {
        let note_b = make_toml_note("B", "2222222222", "done", "");
        let note_a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let snap = snapshot_from(&[("1111111111", &note_a), ("2222222222", &note_b)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(effective_status(&result, "1111111111"), Some(Status::Done));
    }

    #[test]
    fn parent_with_partially_done_children_becomes_wip() {
        let content = make_toml_note(
            "A",
            "1111111111",
            "none",
            "- [ ] parent\n  - [x] child done\n  - [ ] child todo\n",
        );
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(effective_status(&result, "1111111111"), Some(Status::Wip));
    }

    #[test]
    fn cycle_error_policy() {
        let a = make_toml_note("A", "1111111111", "none", "- [ ] @2222222222\n");
        let b = make_toml_note("B", "2222222222", "none", "- [ ] @1111111111\n");
        let snap = snapshot_from(&[("1111111111", &a), ("2222222222", &b)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert!(result
            .diagnostics
            .iter()
            .any(|diag| diag.kind == DiagnosticKind::Cycle));
    }

    #[test]
    fn archived_note_always_done() {
        let content = make_archived_note("A", "1111111111", "- [ ] unchecked\n");
        let snap = snapshot_from(&[("1111111111", &content)]);
        let module = default_module();
        let result = eval_all(&module, &snap);
        assert_eq!(effective_status(&result, "1111111111"), Some(Status::Done));
    }
}

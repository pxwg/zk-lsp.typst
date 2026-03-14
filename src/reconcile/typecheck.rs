/// Static type checker for the Reconcile DSL v1.
use std::collections::{HashMap, HashSet};

use crate::config::{MetadataFieldConfig, MetadataFieldKind};

use super::ast::{Expr, Module, Rule};
use super::types::{Type, TypeError, Value};

#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub rule_return_types: HashMap<String, Type>,
}

struct TypeEnv<'a> {
    module: &'a Module,
    vars: HashMap<String, Type>,
    metadata_kinds: HashMap<String, Type>,
    return_types: &'a HashMap<String, Type>,
    visiting: HashSet<String>,
}

impl<'a> TypeEnv<'a> {
    fn new(module: &'a Module, return_types: &'a HashMap<String, Type>) -> Self {
        Self {
            module,
            vars: HashMap::new(),
            metadata_kinds: HashMap::new(),
            return_types,
            visiting: HashSet::new(),
        }
    }

    fn with_metadata(
        module: &'a Module,
        metadata_fields: &[MetadataFieldConfig],
        return_types: &'a HashMap<String, Type>,
    ) -> Self {
        let mut env = Self::new(module, return_types);
        env.metadata_kinds = metadata_type_map(metadata_fields);
        env
    }
}

fn metadata_type_map(metadata_fields: &[MetadataFieldConfig]) -> HashMap<String, Type> {
    metadata_fields
        .iter()
        .map(|field| {
            let ty = match field.kind {
                MetadataFieldKind::String => Type::String,
                MetadataFieldKind::Boolean => Type::Bool,
                MetadataFieldKind::ArrayString => Type::List(Box::new(Type::String)),
            };
            (field.path.clone(), ty)
        })
        .collect()
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "empty?"
            | "map"
            | "all_done"
            | "all_done?"
            | "aggregate_status"
            | "eq?"
            | "not"
            | "and"
            | "or"
            | "done?"
            | "todo?"
            | "wip?"
            | "none?"
            | "observe_checked"
            | "observe_meta"
            | "targets"
            | "children"
            | "local_checkboxes"
    )
}

fn type_from_value(value: &Value) -> Type {
    match value {
        Value::Bool(_) => Type::Bool,
        Value::Status(_) => Type::Status,
        Value::List(items) => items
            .first()
            .map(type_from_value)
            .map(|inner| Type::List(Box::new(inner)))
            .unwrap_or(Type::List(Box::new(Type::Any))),
        Value::NoteRef(_) => Type::NoteRef,
        Value::CheckboxRef(_) => Type::CheckboxRef,
        Value::String(_) => Type::String,
    }
}

fn unify_types(left: Type, right: Type) -> Type {
    if left == right {
        return left;
    }
    match (left, right) {
        (Type::Any, other) | (other, Type::Any) => other,
        (Type::List(a), Type::List(b)) => Type::List(Box::new(unify_types(*a, *b))),
        _ => Type::Any,
    }
}

fn ensure_type(actual: &Type, expected: &Type) -> Result<(), TypeError> {
    match (actual, expected) {
        (Type::Any, _) | (_, Type::Any) => Ok(()),
        (Type::List(actual_inner), Type::List(expected_inner)) => {
            ensure_type(actual_inner, expected_inner)
        }
        _ if actual == expected => Ok(()),
        _ => Err(TypeError::TypeMismatch {
            expected: expected.clone(),
            got: actual.clone(),
        }),
    }
}

fn builtin_return_type(
    name: &str,
    args: &[Type],
    arg_exprs: &[Expr],
    env: &TypeEnv<'_>,
) -> Result<Type, TypeError> {
    match name {
        "empty?" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            match &args[0] {
                Type::List(_) | Type::Any => Ok(Type::Bool),
                other => Err(TypeError::TypeMismatch {
                    expected: Type::List(Box::new(Type::Any)),
                    got: other.clone(),
                }),
            }
        }
        "all_done" | "all_done?" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            ensure_type(&args[0], &Type::List(Box::new(Type::Status)))?;
            Ok(Type::Bool)
        }
        "aggregate_status" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            ensure_type(&args[0], &Type::List(Box::new(Type::Status)))?;
            Ok(Type::Status)
        }
        "eq?" => {
            if args.len() != 2 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 2,
                    got: args.len(),
                });
            }
            ensure_type(&args[0], &args[1])?;
            Ok(Type::Bool)
        }
        "not" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            ensure_type(&args[0], &Type::Bool)?;
            Ok(Type::Bool)
        }
        "and" | "or" => {
            for arg in args {
                ensure_type(arg, &Type::Bool)?;
            }
            Ok(Type::Bool)
        }
        "done?" | "todo?" | "wip?" | "none?" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            ensure_type(&args[0], &Type::Status)?;
            Ok(Type::Bool)
        }
        "observe_checked" => {
            ensure_arity(name, args.len(), 1)?;
            ensure_type(&args[0], &Type::CheckboxRef)?;
            Ok(Type::Status)
        }
        "observe_meta" => {
            ensure_arity(name, args.len(), 2)?;
            ensure_type(&args[0], &Type::NoteRef)?;
            ensure_type(&args[1], &Type::String)?;
            match &arg_exprs[1] {
                Expr::Lit(Value::String(field)) if field.as_ref() == "checklist-status" => {
                    Ok(Type::Status)
                }
                Expr::Lit(Value::String(field)) => Ok(env
                    .metadata_kinds
                    .get(field.as_ref())
                    .cloned()
                    .unwrap_or(Type::String)),
                _ => Ok(Type::Any),
            }
        }
        "targets" => {
            ensure_arity(name, args.len(), 1)?;
            ensure_type(&args[0], &Type::CheckboxRef)?;
            Ok(Type::List(Box::new(Type::NoteRef)))
        }
        "children" => {
            ensure_arity(name, args.len(), 1)?;
            ensure_type(&args[0], &Type::CheckboxRef)?;
            Ok(Type::List(Box::new(Type::CheckboxRef)))
        }
        "local_checkboxes" => {
            ensure_arity(name, args.len(), 1)?;
            ensure_type(&args[0], &Type::NoteRef)?;
            Ok(Type::List(Box::new(Type::CheckboxRef)))
        }
        _ => Err(TypeError::UnknownFunction(name.to_string())),
    }
}

fn ensure_arity(name: &str, got: usize, expected: usize) -> Result<(), TypeError> {
    if got == expected {
        Ok(())
    } else {
        Err(TypeError::WrongArgCount {
            name: name.to_string(),
            expected,
            got,
        })
    }
}

fn infer_type(expr: &Expr, env: &TypeEnv<'_>) -> Result<Type, TypeError> {
    match expr {
        Expr::Lit(value) => Ok(type_from_value(value)),
        Expr::Var(name) => env
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| TypeError::UnknownVariable(name.clone())),
        Expr::If { cond, then, else_ } => {
            let cond_ty = infer_type(cond, env)?;
            ensure_type(&cond_ty, &Type::Bool)?;
            let then_ty = infer_type(then, env)?;
            let else_ty = infer_type(else_, env)?;
            if then_ty == else_ty {
                Ok(then_ty)
            } else if then_ty == Type::Any || else_ty == Type::Any {
                Ok(Type::Any)
            } else {
                Err(TypeError::IfBranchMismatch {
                    then_type: then_ty,
                    else_type: else_ty,
                })
            }
        }
        Expr::Call { name, args } if name == "map" => infer_map_type(args, env),
        Expr::Call { name, args } => {
            let arg_types = args
                .iter()
                .map(|arg| infer_type(arg, env))
                .collect::<Result<Vec<_>, _>>()?;

            if is_builtin(name) {
                return builtin_return_type(name, &arg_types, args, env);
            }

            let Some(rule) = env.module.rules.iter().find(|rule| rule.name == *name) else {
                return Err(TypeError::UnknownFunction(name.clone()));
            };

            ensure_arity(name, arg_types.len(), rule.params.len())?;

            if env.visiting.contains(name) {
                return Ok(env.return_types.get(name).cloned().unwrap_or(Type::Any));
            }

            let mut child = TypeEnv::new(env.module, env.return_types);
            child.metadata_kinds = env.metadata_kinds.clone();
            child.visiting = env.visiting.clone();
            child.visiting.insert(name.clone());
            for (param, ty) in rule.params.iter().zip(arg_types.iter()) {
                child.vars.insert(param.clone(), ty.clone());
            }
            infer_type(&rule.body, &child)
        }
    }
}

fn infer_map_type(args: &[Expr], env: &TypeEnv<'_>) -> Result<Type, TypeError> {
    ensure_arity("map", args.len(), 2)?;
    let fn_name = match &args[0] {
        Expr::Var(name) => name.clone(),
        _ => {
            return Err(TypeError::UnsupportedHigherOrderArg {
                name: "map".to_string(),
            })
        }
    };
    let list_ty = infer_type(&args[1], env)?;
    let item_ty = match list_ty {
        Type::List(inner) => *inner,
        Type::Any => Type::Any,
        other => {
            return Err(TypeError::TypeMismatch {
                expected: Type::List(Box::new(Type::Any)),
                got: other,
            })
        }
    };

    let return_ty = resolve_fn_return_type(&fn_name, &item_ty, env)?;
    Ok(Type::List(Box::new(return_ty)))
}

fn resolve_fn_return_type(
    fn_name: &str,
    item_type: &Type,
    env: &TypeEnv<'_>,
) -> Result<Type, TypeError> {
    if matches!(fn_name, "done?" | "todo?" | "wip?" | "none?") {
        ensure_type(item_type, &Type::Status)?;
        return Ok(Type::Bool);
    }
    if fn_name == "observe_checked" {
        ensure_type(item_type, &Type::CheckboxRef)?;
        return Ok(Type::Status);
    }
    if is_builtin(fn_name) {
        return Ok(match fn_name {
            "empty?" | "all_done" | "all_done?" | "eq?" | "not" | "and" | "or" => Type::Bool,
            "aggregate_status" => Type::Status,
            _ => Type::Any,
        });
    }
    if let Some(rule) = env.module.rules.iter().find(|rule| rule.name == fn_name) {
        if rule.params.len() != 1 {
            return Err(TypeError::WrongArgCount {
                name: fn_name.to_string(),
                expected: 1,
                got: rule.params.len(),
            });
        }
        return Ok(env
            .return_types
            .get(fn_name)
            .cloned()
            .unwrap_or(Type::Any));
    }
    Err(TypeError::UnknownFunction(fn_name.to_string()))
}

fn bootstrap_rule_return_type(rule: &Rule, metadata_kinds: &HashMap<String, Type>) -> Type {
    match &rule.body {
        Expr::Lit(value) => type_from_value(value),
        Expr::If { then, else_, .. } => unify_types(
            bootstrap_expr_type(then, metadata_kinds),
            bootstrap_expr_type(else_, metadata_kinds),
        ),
        Expr::Call { name, args } => bootstrap_call_type(name, args, metadata_kinds),
        Expr::Var(_) => Type::Any,
    }
}

fn bootstrap_expr_type(expr: &Expr, metadata_kinds: &HashMap<String, Type>) -> Type {
    match expr {
        Expr::Lit(value) => type_from_value(value),
        Expr::If { then, else_, .. } => unify_types(
            bootstrap_expr_type(then, metadata_kinds),
            bootstrap_expr_type(else_, metadata_kinds),
        ),
        Expr::Call { name, args } => bootstrap_call_type(name, args, metadata_kinds),
        Expr::Var(_) => Type::Any,
    }
}

fn bootstrap_call_type(name: &str, args: &[Expr], metadata_kinds: &HashMap<String, Type>) -> Type {
    match name {
        "empty?" | "all_done" | "all_done?" | "eq?" | "not" | "and" | "or" => Type::Bool,
        "done?" | "todo?" | "wip?" | "none?" => Type::Bool,
        "observe_checked" | "aggregate_status" => Type::Status,
        "targets" => Type::List(Box::new(Type::NoteRef)),
        "children" | "local_checkboxes" => Type::List(Box::new(Type::CheckboxRef)),
        "observe_meta" => match args.get(1) {
            Some(Expr::Lit(Value::String(field))) if field.as_ref() == "checklist-status" => {
                Type::Status
            }
            Some(Expr::Lit(Value::String(field))) => metadata_kinds
                .get(field.as_ref())
                .cloned()
                .unwrap_or(Type::String),
            _ => Type::Any,
        },
        "map" => Type::List(Box::new(Type::Any)),
        _ => Type::Any,
    }
}

fn infer_param_type(rule_name: &str, param: &str, index: usize) -> Type {
    if param == "field" || param == "path" {
        return Type::String;
    }
    if rule_name.contains("checked") || param == "c" || param == "cb" || param == "checkbox" {
        return Type::CheckboxRef;
    }
    if param.ends_with('s') && param.len() > 1 {
        let first = param.chars().next().unwrap_or('n');
        if first == 'c' {
            return Type::List(Box::new(Type::CheckboxRef));
        }
        if first == 'n' {
            return Type::List(Box::new(Type::NoteRef));
        }
    }
    if rule_name.contains("meta") && index == 1 {
        return Type::String;
    }
    Type::NoteRef
}

#[allow(dead_code)]
pub fn type_check_module(module: &Module) -> Result<TypeInfo, TypeError> {
    type_check_module_with_metadata(module, &[])
}

pub fn type_check_module_with_metadata(
    module: &Module,
    metadata_fields: &[MetadataFieldConfig],
) -> Result<TypeInfo, TypeError> {
    let metadata_kinds = metadata_type_map(metadata_fields);
    let mut return_types: HashMap<String, Type> = module
        .rules
        .iter()
        .map(|rule| {
            (
                rule.name.clone(),
                bootstrap_rule_return_type(rule, &metadata_kinds),
            )
        })
        .collect();

    for _ in 0..(module.rules.len() + 2).max(2) {
        let mut changed = false;
        for rule in &module.rules {
            let mut env = TypeEnv::with_metadata(module, metadata_fields, &return_types);
            for (index, param) in rule.params.iter().enumerate() {
                env.vars
                    .insert(param.clone(), infer_param_type(&rule.name, param, index));
            }
            env.visiting.insert(rule.name.clone());
            let inferred = infer_type(&rule.body, &env)?;
            let entry = return_types.entry(rule.name.clone()).or_insert(Type::Any);
            let merged = unify_types(entry.clone(), inferred);
            if *entry != merged {
                *entry = merged;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for rule in &module.rules {
        let mut env = TypeEnv::with_metadata(module, metadata_fields, &return_types);
        for (index, param) in rule.params.iter().enumerate() {
            env.vars
                .insert(param.clone(), infer_param_type(&rule.name, param, index));
        }
        env.visiting.insert(rule.name.clone());
        let inferred = infer_type(&rule.body, &env)?;
        let expected = return_types.get(&rule.name).cloned().unwrap_or(Type::Any);
        ensure_type(&inferred, &expected)?;
    }

    Ok(TypeInfo { rule_return_types: return_types })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MetadataFieldConfig, MetadataFieldKind};
    use crate::reconcile::default_module::DEFAULT_MODULE;
    use crate::reconcile::parser::parse_module;

    fn parse(src: &str) -> Module {
        parse_module(src).expect("parse error")
    }

    #[test]
    fn default_module_passes_typecheck() {
        let module = parse(DEFAULT_MODULE);
        type_check_module(&module).expect("default module should typecheck");
    }

    #[test]
    fn aggregate_status_list_status_ok() {
        let src = r#"
        (module
          (define (get_checked c) (observe_checked c))
          (define (test_rule cs)
            (aggregate_status (map get_checked cs))))
        "#;
        let module = parse(src);
        type_check_module(&module).expect("should typecheck");
    }

    #[test]
    fn aggregate_status_list_bool_fails() {
        let src = r#"
        (module
          (define (test_rule ns)
            (aggregate_status (map done? ns))))
        "#;
        let module = parse(src);
        let err = type_check_module(&module).expect_err("should fail");
        assert!(matches!(err, TypeError::TypeMismatch { .. }));
    }

    #[test]
    fn children_returns_checkbox_list() {
        let src = r#"
        (module
          (define (children_status c)
            (map effective_checked (children c)))
          (define (effective_checked c)
            (observe_checked c)))
        "#;
        let module = parse(src);
        type_check_module(&module).expect("should typecheck");
    }

    #[test]
    fn if_branch_mismatch() {
        let src = r#"
        (module
          (define (test n)
            (if true
                (observe_meta n "checklist-status")
                (observe_meta n "relation"))))
        "#;
        let module = parse(src);
        let err = type_check_module(&module).expect_err("should fail");
        assert!(matches!(err, TypeError::IfBranchMismatch { .. }));
    }

    #[test]
    fn map_to_unknown_function() {
        let src = r#"
        (module
          (define (test ns)
            (map nonexistent_fn ns)))
        "#;
        let module = parse(src);
        let err = type_check_module(&module).expect_err("should fail");
        assert!(matches!(err, TypeError::UnknownFunction(_)));
    }

    #[test]
    fn observe_meta_uses_configured_field_types() {
        let src = r#"
        (module
          (define (bool_field n) (observe_meta n "user.done"))
          (define (array_field n) (observe_meta n "user.tags")))
        "#;
        let module = parse(src);
        let metadata_fields = vec![
            MetadataFieldConfig {
                path: "user.done".to_string(),
                kind: MetadataFieldKind::Boolean,
                default: toml::Value::Boolean(false),
            },
            MetadataFieldConfig {
                path: "user.tags".to_string(),
                kind: MetadataFieldKind::ArrayString,
                default: toml::Value::Array(Vec::new()),
            },
        ];
        type_check_module_with_metadata(&module, &metadata_fields).expect("should typecheck");
    }
}

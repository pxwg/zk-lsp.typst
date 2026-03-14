/// Static type checker for the Reconcile DSL v1.
use std::collections::{HashMap, HashSet};

use super::ast::{Expr, Module, Rule};
use super::types::{Type, TypeError};

// ---------------------------------------------------------------------------
// Type environment
// ---------------------------------------------------------------------------

struct TypeEnv<'a> {
    module: &'a Module,
    vars: HashMap<String, Type>,
    /// Rules currently being type-checked (to break mutual recursion).
    visiting: HashSet<String>,
}

impl<'a> TypeEnv<'a> {
    fn new(module: &'a Module) -> Self {
        TypeEnv {
            module,
            vars: HashMap::new(),
            visiting: HashSet::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Builtin signatures
// ---------------------------------------------------------------------------

/// Return type of a builtin given the arg types (first arg used for dispatching).
fn builtin_return_type(name: &str, args: &[Type], env: &TypeEnv<'_>) -> Result<Type, TypeError> {
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
                Type::List(_) => Ok(Type::Bool),
                other => Err(TypeError::TypeMismatch {
                    expected: Type::List(Box::new(Type::Bool)),
                    got: other.clone(),
                }),
            }
        }
        "all_done" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            match &args[0] {
                Type::List(inner) if **inner == Type::Status => Ok(Type::Bool),
                other => Err(TypeError::TypeMismatch {
                    expected: Type::List(Box::new(Type::Status)),
                    got: other.clone(),
                }),
            }
        }
        "aggregate_status" => {
            if args.len() != 1 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 1,
                    got: args.len(),
                });
            }
            match &args[0] {
                Type::List(inner) if **inner == Type::Bool => Ok(Type::Status),
                other => Err(TypeError::TypeMismatch {
                    expected: Type::List(Box::new(Type::Bool)),
                    got: other.clone(),
                }),
            }
        }
        "map" => {
            // (map fn_name list) — fn_name is a Var holding a function name
            if args.len() != 2 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 2,
                    got: args.len(),
                });
            }
            // args[0] should be the fn name type — we encode it as NoteRef (the type of the arg)
            // The return type of map is List(return_type_of_fn)
            // We need to infer the return type of the function referenced by name
            // args[1] should be List(_)
            match &args[1] {
                Type::List(_) => {
                    // args[0] is a placeholder; the actual fn_name was carried as NoteRef for compat
                    // Just return List(Bool) or List(Status) depending on fn
                    // We can't determine without the fn_name string here, return List(Bool) as default
                    // This is resolved in infer_type by special-casing map
                    Ok(Type::List(Box::new(Type::Bool)))
                }
                other => Err(TypeError::TypeMismatch {
                    expected: Type::List(Box::new(Type::Bool)),
                    got: other.clone(),
                }),
            }
        }
        "eq?" => {
            if args.len() != 2 {
                return Err(TypeError::WrongArgCount {
                    name: name.to_string(),
                    expected: 2,
                    got: args.len(),
                });
            }
            if args[0] != args[1] {
                return Err(TypeError::TypeMismatch {
                    expected: args[0].clone(),
                    got: args[1].clone(),
                });
            }
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
            match &args[0] {
                Type::Bool => Ok(Type::Bool),
                other => Err(TypeError::TypeMismatch {
                    expected: Type::Bool,
                    got: other.clone(),
                }),
            }
        }
        "and" | "or" => {
            for arg in args {
                if *arg != Type::Bool {
                    return Err(TypeError::TypeMismatch {
                        expected: Type::Bool,
                        got: arg.clone(),
                    });
                }
            }
            Ok(Type::Bool)
        }
        _ => {
            // Check user-defined rules
            if env.module.rules.iter().any(|r| r.name == name) {
                // We can't easily resolve without calling rule_return_type; return generic
                Err(TypeError::UnknownFunction(name.to_string()))
            } else {
                Err(TypeError::UnknownFunction(name.to_string()))
            }
        }
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "empty?" | "map" | "all_done" | "aggregate_status" | "eq?" | "not" | "and" | "or"
    )
}

fn rule_return_type(
    rule: &Rule,
    module: &Module,
    visiting: &HashSet<String>,
) -> Result<Type, TypeError> {
    // Break mutual recursion: if we're already checking this rule, return its expected type.
    if visiting.contains(&rule.name) {
        // Infer expected type from name convention
        return Ok(rule_expected_type(&rule.name));
    }

    let mut param_env = TypeEnv::new(module);
    for param in &rule.params {
        let ty = infer_param_type(&rule.name, param);
        param_env.vars.insert(param.clone(), ty);
    }
    // Propagate visiting set + add current rule
    let mut child_visiting = visiting.clone();
    child_visiting.insert(rule.name.clone());
    param_env.visiting = child_visiting;

    infer_type(&rule.body, &param_env)
}

/// Heuristic expected return type for a rule based on its name.
fn rule_expected_type(name: &str) -> Type {
    if name.contains("status") {
        Type::Status
    } else if name.contains("checked") {
        Type::Bool
    } else {
        Type::Bool // default
    }
}

// ---------------------------------------------------------------------------
// Type inference
// ---------------------------------------------------------------------------

fn infer_type(expr: &Expr, env: &TypeEnv<'_>) -> Result<Type, TypeError> {
    match expr {
        Expr::BoolLit(_) => Ok(Type::Bool),
        Expr::StatusLit(_) => Ok(Type::Status),
        Expr::StringLit(_) => Ok(Type::String),
        Expr::Var(name) => env
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| TypeError::UnknownVariable(name.clone())),
        Expr::ObserveChecked(_c) => Ok(Type::Bool),
        Expr::ObserveMeta(_n, path) => {
            // "checklist-status" path → Status; any other string path → String.
            match path.as_ref() {
                Expr::StringLit(s) if s == "checklist-status" => Ok(Type::Status),
                _ => Ok(Type::String),
            }
        }
        Expr::Targets(_c) => Ok(Type::List(Box::new(Type::NoteRef))),
        Expr::LocalCheckboxes(_n) => Ok(Type::List(Box::new(Type::CheckboxRef))),
        Expr::If { cond, then, else_ } => {
            let cond_ty = infer_type(cond, env)?;
            if cond_ty != Type::Bool {
                return Err(TypeError::TypeMismatch {
                    expected: Type::Bool,
                    got: cond_ty,
                });
            }
            let then_ty = infer_type(then, env)?;
            let else_ty = infer_type(else_, env)?;
            if then_ty != else_ty {
                return Err(TypeError::IfBranchMismatch {
                    then_type: then_ty,
                    else_type: else_ty,
                });
            }
            Ok(then_ty)
        }
        // Runtime-only literals — should not appear in parsed DSL; treat as NoteRef/CheckboxRef
        Expr::NoteRefLit(_) => Ok(Type::NoteRef),
        Expr::CheckboxRefLit(_) => Ok(Type::CheckboxRef),
        Expr::Call { name, args } => {
            // Special case: map(fn_name, list) — first arg is a function name symbol
            if name == "map" {
                return infer_map_type(args, env);
            }

            // Break mutual recursion: if this rule is already being checked, return expected type
            if env.visiting.contains(name.as_str()) {
                return Ok(rule_expected_type(name));
            }

            // Check user-defined rules first
            if let Some(rule) = env.module.rules.iter().find(|r| r.name == name.as_str()) {
                // Evaluate args
                let arg_types: Vec<Type> = args
                    .iter()
                    .map(|a| infer_type(a, env))
                    .collect::<Result<_, _>>()?;
                if arg_types.len() != rule.params.len() {
                    return Err(TypeError::WrongArgCount {
                        name: name.clone(),
                        expected: rule.params.len(),
                        got: arg_types.len(),
                    });
                }
                // Build local env and recursively check body (propagate visiting set)
                let mut local_env = TypeEnv::new(env.module);
                for (param, ty) in rule.params.iter().zip(arg_types.iter()) {
                    local_env.vars.insert(param.clone(), ty.clone());
                }
                let mut child_visiting = env.visiting.clone();
                child_visiting.insert(name.to_string());
                local_env.visiting = child_visiting;
                return infer_type(&rule.body, &local_env);
            }

            if is_builtin(name) {
                let arg_types: Vec<Type> = args
                    .iter()
                    .map(|a| infer_type(a, env))
                    .collect::<Result<_, _>>()?;
                return builtin_return_type(name, &arg_types, env);
            }

            Err(TypeError::UnknownFunction(name.clone()))
        }
    }
}

fn infer_map_type(args: &[Expr], env: &TypeEnv<'_>) -> Result<Type, TypeError> {
    if args.len() != 2 {
        return Err(TypeError::WrongArgCount {
            name: "map".to_string(),
            expected: 2,
            got: args.len(),
        });
    }
    // First arg must be a Var naming a rule or builtin
    let fn_name = match &args[0] {
        Expr::Var(s) => s.clone(),
        other => {
            return Err(TypeError::TypeMismatch {
                expected: Type::NoteRef, // placeholder — "function name expected"
                got: infer_type(other, env)?,
            });
        }
    };

    // Check list arg
    let list_ty = infer_type(&args[1], env)?;
    match &list_ty {
        Type::List(_) => {}
        other => {
            return Err(TypeError::TypeMismatch {
                expected: Type::List(Box::new(Type::Bool)),
                got: other.clone(),
            });
        }
    }

    // Determine return type of fn_name
    let item_type = match &list_ty {
        Type::List(inner) => (**inner).clone(),
        _ => unreachable!(),
    };

    let return_type = resolve_fn_return_type(&fn_name, &item_type, env)?;
    Ok(Type::List(Box::new(return_type)))
}

fn resolve_fn_return_type(
    fn_name: &str,
    _item_type: &Type,
    env: &TypeEnv<'_>,
) -> Result<Type, TypeError> {
    // Check user rules
    if let Some(rule) = env.module.rules.iter().find(|r| r.name == fn_name) {
        return rule_return_type(rule, env.module, &env.visiting);
    }
    // Check builtins
    match fn_name {
        "empty?" | "all_done" | "aggregate_status" | "not" | "and" | "or" | "eq?" => {
            // These are valid but unusual in map; return Bool as approximation
            Ok(Type::Bool)
        }
        _ => Err(TypeError::UnknownFunction(fn_name.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Infer the type of a parameter by naming convention:
/// - `n`, `note` or any single-letter other than `c` → NoteRef
/// - `c`, `cb`, `checkbox` → CheckboxRef
/// - `ns`, `notes`, ends with 's' and starts with 'n' → List(NoteRef)
/// - `cs`, `cbs`, ends with 's' and starts with 'c' → List(CheckboxRef)
/// - anything else → NoteRef (default)
fn infer_param_type(rule_name: &str, param: &str) -> Type {
    // Rule-name override for well-known rules
    if rule_name == "effective_checked" || param == "c" || param == "cb" || param == "checkbox" {
        return Type::CheckboxRef;
    }
    // List params by convention (e.g. cs, ns, items_ending_in_s)
    if param.ends_with('s') && param.len() > 1 {
        let first = param.chars().next().unwrap_or('n');
        if first == 'c' {
            return Type::List(Box::new(Type::CheckboxRef));
        } else {
            return Type::List(Box::new(Type::NoteRef));
        }
    }
    // Single-letter params: n → NoteRef, c → CheckboxRef (already handled above)
    Type::NoteRef
}

pub fn type_check_module(module: &Module) -> Result<(), TypeError> {
    for rule in &module.rules {
        let mut env = TypeEnv::new(module);
        for param in &rule.params {
            let ty = infer_param_type(&rule.name, param);
            env.vars.insert(param.clone(), ty);
        }
        // Mark current rule as being visited to break mutual recursion
        env.visiting.insert(rule.name.clone());
        infer_type(&rule.body, &env)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
    fn all_done_list_status_ok() {
        let src = r#"
        (module
          (define (eff_checked c)
            (all_done (targets c))))
        "#;
        // targets returns List(NoteRef), but all_done expects List(Status)
        // This should fail — targets returns note refs, not statuses
        // Let's use a proper example that works
        let src2 = r#"
        (module
          (define (get_status n) (observe_meta n "checklist-status"))
          (define (test_rule ns)
            (all_done (map get_status ns))))
        "#;
        let module = parse(src2);
        type_check_module(&module).expect("should typecheck");
        let _ = src;
    }

    #[test]
    fn aggregate_status_list_bool_ok() {
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
    fn if_branch_mismatch() {
        let src = r#"
        (module
          (define (test n)
            (if (empty? (local_checkboxes n))
                (observe_meta n "checklist-status")
                (observe_checked n))))
        "#;
        // then: Status, else: Bool — mismatch
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
}

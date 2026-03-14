/// The default reconcile module, embedded from `examples/rules/checklist.lisp`.
pub const DEFAULT_MODULE: &str = include_str!("../../examples/rules/checklist.lisp");

use std::path::PathBuf;

use anyhow::{Context, Result};

use super::ast::{Module, Policy, Rule};
use super::parser::parse_module;

pub fn load_module(rule_paths: &[PathBuf], disable_default_rules: bool) -> Result<Module> {
    let mut merged = if disable_default_rules {
        Module {
            policy: Policy::default(),
            policy_explicit: false,
            rules: Vec::new(),
        }
    } else {
        parse_module(DEFAULT_MODULE).map_err(anyhow::Error::msg)?
    };

    for path in rule_paths {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read reconcile rule file '{}'", path.display()))?;
        let overlay = parse_module(&source).map_err(|err| {
            anyhow::anyhow!(
                "failed to parse reconcile rule file '{}': {err}",
                path.display()
            )
        })?;
        merged = merge_modules(merged, overlay);
    }

    Ok(merged)
}

fn merge_modules(mut base: Module, overlay: Module) -> Module {
    if overlay.policy_explicit {
        base.policy = overlay.policy;
        base.policy_explicit = true;
    }

    for rule in overlay.rules {
        upsert_rule(&mut base.rules, rule);
    }

    base
}

fn upsert_rule(rules: &mut Vec<Rule>, rule: Rule) {
    if let Some(existing) = rules
        .iter_mut()
        .find(|candidate| candidate.name == rule.name)
    {
        *existing = rule;
    } else {
        rules.push(rule);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn default_module_used_when_no_runtime_rule_is_configured() {
        let module = load_module(&[], false).expect("load");
        assert!(module
            .rules
            .iter()
            .any(|rule| rule.name == "effective_meta"));
    }

    #[test]
    fn default_module_can_be_disabled() {
        let module = load_module(&[], true).expect("load");
        assert!(module.rules.is_empty());
        assert!(!module.policy_explicit);
    }

    #[test]
    fn runtime_rule_file_is_loaded_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "zk-lsp-rule-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("custom.lisp");
        let source = r#"(module (define (custom_helper n) (observe_meta n "relation")))"#;
        std::fs::write(&path, source).expect("write");

        let loaded = load_module(&[path], false).expect("load");
        assert!(loaded.rules.iter().any(|rule| rule.name == "custom_helper"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_rule_file_changes_are_picked_up_on_next_load() {
        let dir = std::env::temp_dir().join(format!(
            "zk-lsp-rule-reload-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("custom.lisp");

        std::fs::write(&path, "(module (define (first n) n))").expect("write v1");
        let first = load_module(std::slice::from_ref(&path), false).expect("load v1");

        std::fs::write(&path, "(module (define (second n) n))").expect("write v2");
        let second = load_module(&[path], false).expect("load v2");

        assert!(first.rules.iter().any(|rule| rule.name == "first"));
        assert!(second.rules.iter().any(|rule| rule.name == "second"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multiple_runtime_rule_files_are_merged_in_order() {
        let dir = std::env::temp_dir().join(format!(
            "zk-lsp-rule-order-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let first = dir.join("first.lisp");
        let second = dir.join("second.lisp");
        std::fs::write(
            &first,
            r#"(module
                 (define (helper n) done)
                 (define (custom_a n) (helper n)))"#,
        )
        .expect("write first");
        std::fs::write(
            &second,
            r#"(module
                 (policy (cycle unknown))
                 (define (helper n) todo)
                 (define (custom_b n) (helper n)))"#,
        )
        .expect("write second");

        let loaded = load_module(&[first, second], false).expect("load");
        let helper = loaded
            .rules
            .iter()
            .find(|rule| rule.name == "helper")
            .expect("helper");
        assert_eq!(helper.params, vec!["n"]);
        assert!(loaded.rules.iter().any(|rule| rule.name == "custom_a"));
        assert!(loaded.rules.iter().any(|rule| rule.name == "custom_b"));
        assert!(matches!(
            loaded.policy.cycle,
            crate::reconcile::ast::CyclePolicy::Unknown
        ));
        assert!(loaded.policy_explicit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_rules_can_run_without_default_module() {
        let dir = std::env::temp_dir().join(format!(
            "zk-lsp-rule-disable-default-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("custom.lisp");
        std::fs::write(
            &path,
            r#"(module
                 (policy (cycle unknown))
                 (define (effective_checked c) (observe_checked c))
                 (define (effective_meta n field) (observe_meta n field)))"#,
        )
        .expect("write");

        let loaded = load_module(&[path], true).expect("load");
        assert!(loaded
            .rules
            .iter()
            .any(|rule| rule.name == "effective_checked"));
        assert!(loaded
            .rules
            .iter()
            .any(|rule| rule.name == "effective_meta"));
        assert!(matches!(
            loaded.policy.cycle,
            crate::reconcile::ast::CyclePolicy::Unknown
        ));

        let _ = std::fs::remove_dir_all(dir);
    }
}

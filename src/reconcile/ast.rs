/// AST for the Reconcile DSL v1.
use super::types::{Status, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum CyclePolicy {
    Error,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub cycle: CyclePolicy,
    /// Status to use when a note's status is unknown (e.g., cycle fallback).
    pub unknown_status: Status,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            cycle: CyclePolicy::Error,
            unknown_status: Status::Todo,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub policy: Policy,
    pub policy_explicit: bool,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    Var(String),
    /// `(if <cond> <then> <else>)`
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    /// Any call: builtins or user-defined rules.
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

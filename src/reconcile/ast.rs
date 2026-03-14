/// AST for the Reconcile DSL v1.
use super::types::{CheckboxId, NoteId, Status};

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
    /// Checked state to use when a checkbox's state is unknown.
    pub unknown_checked: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            cycle: CyclePolicy::Error,
            unknown_status: Status::Todo,
            unknown_checked: false,
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
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    BoolLit(bool),
    StatusLit(Status),
    StringLit(String),
    Var(String),
    /// `(observe_checked <checkbox_expr>)`
    ObserveChecked(Box<Expr>),
    /// `(observe_meta <note_expr> <path_str_expr>)`
    ObserveMeta(Box<Expr>, Box<Expr>),
    /// `(targets <checkbox_expr>)`
    Targets(Box<Expr>),
    /// `(local_checkboxes <note_expr>)`
    LocalCheckboxes(Box<Expr>),
    /// `(if <cond> <then> <else>)`
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    /// Any call: builtins (empty?, map, all_done, etc.) or user-defined rules.
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// Runtime-only: a note ID value (produced by `value_to_expr` in the evaluator).
    NoteRefLit(NoteId),
    /// Runtime-only: a checkbox ID value (produced by `value_to_expr` in the evaluator).
    CheckboxRefLit(CheckboxId),
}

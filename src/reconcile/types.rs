use std::fmt;
/// Core types for the Reconcile DSL v1.
use std::path::PathBuf;
use std::rc::Rc;

pub type NoteId = String;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckboxId {
    pub note_id: NoteId,
    pub line_idx: usize,
}

impl fmt::Display for CheckboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.note_id, self.line_idx)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Status {
    None,
    Todo,
    Wip,
    Done,
}

impl Status {
    #[allow(dead_code)]
    pub fn is_done(&self) -> bool {
        matches!(self, Status::Done)
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            Status::None => "none",
            Status::Todo => "todo",
            Status::Wip => "wip",
            Status::Done => "done",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Bool(bool),
    Status(Status),
    List(Rc<Vec<Value>>),
    /// Runtime-only: a reference to a note (used inside the evaluator).
    NoteRef(NoteId),
    /// Runtime-only: a reference to a checkbox (used inside the evaluator).
    CheckboxRef(CheckboxId),
    /// Runtime-only: a string value (e.g., from `observe_meta` for non-status fields).
    String(Rc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Any,
    Bool,
    Status,
    String,
    NoteRef,
    CheckboxRef,
    List(Box<Type>),
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Any => write!(f, "Any"),
            Type::Bool => write!(f, "Bool"),
            Type::Status => write!(f, "Status"),
            Type::String => write!(f, "String"),
            Type::NoteRef => write!(f, "NoteRef"),
            Type::CheckboxRef => write!(f, "CheckboxRef"),
            Type::List(inner) => write!(f, "List({inner})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    UnexpectedEof,
    UnexpectedToken { got: String, expected: String },
    InvalidExprHead(String),
    DuplicateRule(String),
    InvalidPolicyKey(String),
    InvalidPolicyValue { key: String, value: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedEof => write!(f, "unexpected end of input"),
            ParseError::UnexpectedToken { got, expected } => {
                write!(f, "unexpected token '{got}', expected {expected}")
            }
            ParseError::InvalidExprHead(s) => write!(f, "invalid expression head '{s}'"),
            ParseError::DuplicateRule(s) => write!(f, "duplicate rule name '{s}'"),
            ParseError::InvalidPolicyKey(k) => write!(f, "unknown policy key '{k}'"),
            ParseError::InvalidPolicyValue { key, value } => {
                write!(f, "invalid value '{value}' for policy key '{key}'")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    UnknownVariable(String),
    UnknownFunction(String),
    TypeMismatch {
        expected: Type,
        got: Type,
    },
    IfBranchMismatch {
        then_type: Type,
        else_type: Type,
    },
    WrongArgCount {
        name: String,
        expected: usize,
        got: usize,
    },
    UnsupportedHigherOrderArg { name: String },
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeError::UnknownVariable(v) => write!(f, "unknown variable '{v}'"),
            TypeError::UnknownFunction(n) => write!(f, "unknown function '{n}'"),
            TypeError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {expected}, got {got}")
            }
            TypeError::IfBranchMismatch {
                then_type,
                else_type,
            } => {
                write!(
                    f,
                    "if branch type mismatch: then={then_type}, else={else_type}"
                )
            }
            TypeError::WrongArgCount {
                name,
                expected,
                got,
            } => {
                write!(f, "'{name}' expected {expected} args, got {got}")
            }
            TypeError::UnsupportedHigherOrderArg { name } => {
                write!(f, "unsupported higher-order argument in '{name}'")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    UnknownVariable(String),
    UnknownFunction(String),
    TypeMismatch { context: String },
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::UnknownVariable(v) => write!(f, "unknown variable '{v}'"),
            EvalError::UnknownFunction(n) => write!(f, "unknown function '{n}'"),
            EvalError::TypeMismatch { context } => write!(f, "type mismatch in {context}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticKind {
    Cycle,
    EvalFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticLocation {
    pub file_path: PathBuf,
    pub line: usize,
    pub byte_start: u32,
    pub byte_end: u32,
}

#[derive(Debug, Clone)]
pub struct ReconcileDiagnostic {
    pub note_id: NoteId,
    pub message: String,
    #[allow(dead_code)]
    pub kind: DiagnosticKind,
    pub severity: DiagnosticSeverity,
    pub location: Option<DiagnosticLocation>,
}

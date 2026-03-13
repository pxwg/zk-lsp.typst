use std::collections::HashMap;

/// Byte and line/column span for a node in the note.
#[derive(Debug, Clone)]
pub struct HookSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

/// The title heading of the note with its location.
#[derive(Debug, Clone)]
pub struct HookTitle {
    pub text: String,
    pub span: HookSpan,
}

/// A checklist item (local `- [ ]` or ref `- [ ] @ID`).
#[derive(Debug, Clone)]
pub struct HookCheckbox {
    /// Stable identifier: "local:{line_idx}" for LocalItem; first target_id for RefItem.
    pub id: String,
    /// "local" or "ref"
    pub kind: String,
    pub checked: bool,
    /// target_id strings; empty for LocalItem
    pub targets: Vec<String>,
    pub text: String,
    pub span: HookSpan,
    /// 0-based line index of this checkbox within the note.
    pub line_idx: usize,
    /// Number of leading spaces (indentation level).
    pub indent: usize,
}

/// A heading in the note (level 1 = `=`, level 2 = `==`, etc.)
#[derive(Debug, Clone)]
pub struct HookHeading {
    pub level: u32,
    pub text: String,
    pub span: HookSpan,
}

/// Full note representation passed to the Lua hook.
#[derive(Debug, Clone)]
pub struct HookNoteInput {
    pub id: String,
    pub title: Option<HookTitle>,
    pub content: String,
    pub metadata: toml::Table,
    pub checkboxes: Vec<HookCheckbox>,
    pub headings: Vec<HookHeading>,
}

/// A single text replacement returned by the hook.
#[derive(Debug, Clone)]
pub struct HookTextEdit {
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
}

/// The result returned by the Lua `run(note)` function.
#[derive(Debug, Clone, Default)]
pub struct HookResult {
    /// Metadata keys to patch (merged into existing TOML block).
    pub metadata: HashMap<String, toml::Value>,
    /// Byte-range text edits to apply.
    pub edits: Vec<HookTextEdit>,
}

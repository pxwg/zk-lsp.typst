-- EmmyLua type definitions for zk-lsp Hook v1
-- Source these into your IDE for completions.

---@class Span
---@field start_byte integer  Byte offset of start within full note content
---@field end_byte   integer  Byte offset of end within full note content (exclusive)
---@field start_line integer  0-based line index of start
---@field start_col  integer  0-based column (byte offset within line) of start
---@field end_line   integer  0-based line index of end
---@field end_col    integer  0-based column (byte offset within line) of end

---@class Title
---@field text string  Title text with the `<ID>` suffix stripped
---@field span Span

---@class Checkbox
---@field id       string    "local:{line_idx}" for local items; first target_id for ref items
---@field kind     string    "local" | "ref"
---@field checked  boolean
---@field targets  string[]  Target note IDs (empty for local items)
---@field text     string    Checkbox body text (after `- [x] `)
---@field span     Span      Full-line span
---@field line_idx integer   0-based line index of this checkbox within the note
---@field indent   integer   Number of leading spaces (indentation level)

---@class Heading
---@field level  integer  Heading level (1 = `=`, 2 = `==`, …)
---@field text   string   Heading text with `<ID>` suffix stripped for title headings
---@field span   Span

---@class NoteInput
---@field id         string      10-digit note ID
---@field title      Title|nil   Title heading (nil if not parseable)
---@field content    string      Raw note content
---@field metadata   table<string, any>  TOML metadata key→value map
---@field checkboxes Checkbox[]
---@field headings   Heading[]

---@class TextEdit
---@field start_byte integer  Byte offset of range start in `content`
---@field end_byte   integer  Byte offset of range end in `content` (exclusive)
---@field text       string   Replacement text

---@class HookResult
---@field metadata  table<string, any>|nil  Metadata keys to patch (merged into TOML block)
---@field edits     TextEdit[]|nil          Byte-range text edits to apply

--- Entry point called by zk-lsp for each note.
--- Return a HookResult table (or nil / empty table for no changes).
---@param note NoteInput
---@return HookResult
local function run(note) return {} end -- luacheck: ignore (type-only placeholder)
return { run = run }

use std::path::Path;

use mlua::{Lua, Value as LuaValue};

use crate::parser::{self, ChecklistItemKind};
use super::types::{
    HookCheckbox, HookHeading, HookNoteInput, HookResult, HookSpan, HookTextEdit, HookTitle,
};

/// A loaded Lua hook script that exposes a `run(note) -> result` function.
pub struct HookRunner {
    lua: Lua,
}

impl HookRunner {
    /// Load a hook from a file path.
    pub fn load_file(path: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading hook file {}: {e}", path.display()))?;
        Self::load_str(&source)
    }

    /// Load a hook from a source string (also used by tests).
    pub fn load_str(source: &str) -> anyhow::Result<Self> {
        let lua = Lua::new();
        lua.load(source).exec()?;
        lua.globals()
            .get::<mlua::Function>("run")
            .map_err(|_| anyhow::anyhow!("Lua script must define a global `run` function"))?;
        Ok(HookRunner { lua })
    }

    /// Run the hook against a note input, returning patches to apply.
    pub fn run(&self, input: &HookNoteInput) -> anyhow::Result<HookResult> {
        let table = note_input_to_lua(&self.lua, input)?;
        let run_fn: mlua::Function = self.lua.globals().get("run")?;
        let result_val: LuaValue = run_fn.call(table)?;
        match result_val {
            LuaValue::Table(t) => lua_table_to_hook_result(t),
            LuaValue::Nil => Ok(HookResult::default()),
            _ => anyhow::bail!(
                "`run` must return a table or nil, got {:?}",
                result_val.type_name()
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Span helpers
// ---------------------------------------------------------------------------

fn build_line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in content.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

fn span_for_line(line_offsets: &[usize], line_idx: usize, line_len: usize) -> HookSpan {
    let start_byte = line_offsets.get(line_idx).copied().unwrap_or(0);
    let end_byte = start_byte + line_len;
    HookSpan {
        start_byte,
        end_byte,
        start_line: line_idx,
        start_col: 0,
        end_line: line_idx,
        end_col: line_len,
    }
}

// ---------------------------------------------------------------------------
// Build HookNoteInput from raw content
// ---------------------------------------------------------------------------

pub fn build_hook_note_input(content: &str) -> HookNoteInput {
    let lines: Vec<&str> = content.lines().collect();
    let line_offsets = build_line_byte_offsets(content);

    // Parse header for id and title
    let (id, title) = if let Some(header) = parser::parse_header(content) {
        let title_span = span_for_line(
            &line_offsets,
            header.title_line_idx,
            lines.get(header.title_line_idx).map(|l| l.len()).unwrap_or(0),
        );
        let hook_title = HookTitle {
            text: header.title.clone(),
            span: title_span,
        };
        (header.id, Some(hook_title))
    } else {
        (String::new(), None)
    };

    // Parse TOML metadata
    let metadata = parser::find_toml_metadata_block(content)
        .and_then(|b| b.toml_content.parse::<toml::Table>().ok())
        .unwrap_or_default();

    // Parse checkboxes
    let checkboxes = parser::parse_checklist_items(content)
        .into_iter()
        .map(|item| {
            let line_len = lines.get(item.line_idx).map(|l| l.len()).unwrap_or(0);
            let span = span_for_line(&line_offsets, item.line_idx, line_len);
            match &item.kind {
                ChecklistItemKind::Local => HookCheckbox {
                    id: format!("local:{}", item.line_idx),
                    kind: "local".to_string(),
                    checked: item.checked,
                    targets: Vec::new(),
                    text: item.text.clone(),
                    span,
                    line_idx: item.line_idx,
                    indent: item.indent,
                },
                ChecklistItemKind::Ref { targets } => {
                    let first_id = targets
                        .first()
                        .map(|t| t.target_id.clone())
                        .unwrap_or_default();
                    let target_ids: Vec<String> =
                        targets.iter().map(|t| t.target_id.clone()).collect();
                    HookCheckbox {
                        id: first_id,
                        kind: "ref".to_string(),
                        checked: item.checked,
                        targets: target_ids,
                        text: item.text.clone(),
                        span,
                        line_idx: item.line_idx,
                        indent: item.indent,
                    }
                }
            }
        })
        .collect();

    // Parse headings
    let headings = parser::parse_headings(content)
        .into_iter()
        .map(|h| {
            let line_len = lines.get(h.line_idx).map(|l| l.len()).unwrap_or(0);
            let span = span_for_line(&line_offsets, h.line_idx, line_len);
            HookHeading {
                level: h.level,
                text: h.text,
                span,
            }
        })
        .collect();

    HookNoteInput {
        id,
        title,
        content: content.to_string(),
        metadata,
        checkboxes,
        headings,
    }
}

// ---------------------------------------------------------------------------
// Rust → Lua conversions
// ---------------------------------------------------------------------------

fn span_to_lua(lua: &Lua, span: &HookSpan) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    t.set("start_byte", span.start_byte)?;
    t.set("end_byte", span.end_byte)?;
    t.set("start_line", span.start_line)?;
    t.set("start_col", span.start_col)?;
    t.set("end_line", span.end_line)?;
    t.set("end_col", span.end_col)?;
    Ok(t)
}

fn toml_value_to_lua(lua: &Lua, value: &toml::Value) -> mlua::Result<LuaValue> {
    match value {
        toml::Value::String(s) => Ok(LuaValue::String(lua.create_string(s.as_bytes())?)),
        toml::Value::Integer(n) => Ok(LuaValue::Integer(*n)),
        toml::Value::Float(f) => Ok(LuaValue::Number(*f)),
        toml::Value::Boolean(b) => Ok(LuaValue::Boolean(*b)),
        toml::Value::Array(arr) => {
            let t = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                t.set(i + 1, toml_value_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(t))
        }
        toml::Value::Table(tbl) => {
            let t = lua.create_table()?;
            for (k, v) in tbl {
                t.set(k.as_str(), toml_value_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(t))
        }
        toml::Value::Datetime(dt) => {
            Ok(LuaValue::String(lua.create_string(dt.to_string().as_bytes())?))
        }
    }
}

fn toml_table_to_lua(lua: &Lua, table: &toml::Table) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    for (k, v) in table {
        t.set(k.as_str(), toml_value_to_lua(lua, v)?)?;
    }
    Ok(t)
}

fn note_input_to_lua(lua: &Lua, input: &HookNoteInput) -> anyhow::Result<mlua::Table> {
    let t = lua.create_table()?;

    t.set("id", input.id.as_str())?;
    t.set("content", input.content.as_str())?;

    // title
    match &input.title {
        Some(title) => {
            let tt = lua.create_table()?;
            tt.set("text", title.text.as_str())?;
            tt.set("span", span_to_lua(lua, &title.span)?)?;
            t.set("title", tt)?;
        }
        None => {
            t.set("title", LuaValue::Nil)?;
        }
    }

    // metadata
    t.set("metadata", toml_table_to_lua(lua, &input.metadata)?)?;

    // checkboxes (1-indexed array)
    let cbs = lua.create_table()?;
    for (i, cb) in input.checkboxes.iter().enumerate() {
        let cbt = lua.create_table()?;
        cbt.set("id", cb.id.as_str())?;
        cbt.set("kind", cb.kind.as_str())?;
        cbt.set("checked", cb.checked)?;
        cbt.set("text", cb.text.as_str())?;
        cbt.set("span", span_to_lua(lua, &cb.span)?)?;
        cbt.set("line_idx", cb.line_idx)?;
        cbt.set("indent", cb.indent)?;
        let targets = lua.create_table()?;
        for (j, tgt) in cb.targets.iter().enumerate() {
            targets.set(j + 1, tgt.as_str())?;
        }
        cbt.set("targets", targets)?;
        cbs.set(i + 1, cbt)?;
    }
    t.set("checkboxes", cbs)?;

    // headings (1-indexed array)
    let hdgs = lua.create_table()?;
    for (i, h) in input.headings.iter().enumerate() {
        let ht = lua.create_table()?;
        ht.set("level", h.level)?;
        ht.set("text", h.text.as_str())?;
        ht.set("span", span_to_lua(lua, &h.span)?)?;
        hdgs.set(i + 1, ht)?;
    }
    t.set("headings", hdgs)?;

    Ok(t)
}

// ---------------------------------------------------------------------------
// Lua → Rust conversions
// ---------------------------------------------------------------------------

fn lua_value_to_toml(value: &LuaValue) -> anyhow::Result<toml::Value> {
    match value {
        LuaValue::String(s) => Ok(toml::Value::String(s.to_str()?.to_string())),
        LuaValue::Integer(n) => Ok(toml::Value::Integer(*n)),
        LuaValue::Number(f) => Ok(toml::Value::Float(*f)),
        LuaValue::Boolean(b) => Ok(toml::Value::Boolean(*b)),
        LuaValue::Table(t) => {
            // Determine if it's an array (sequential integer keys from 1)
            let len = t.raw_len();
            if len > 0 {
                let mut arr = Vec::new();
                for i in 1..=len {
                    let v: LuaValue = t.get(i)?;
                    arr.push(lua_value_to_toml(&v)?);
                }
                return Ok(toml::Value::Array(arr));
            }
            // Otherwise treat as a table
            let mut tbl = toml::Table::new();
            for pair in t.clone().pairs::<String, LuaValue>() {
                let (k, v) = pair?;
                tbl.insert(k, lua_value_to_toml(&v)?);
            }
            Ok(toml::Value::Table(tbl))
        }
        LuaValue::Nil => anyhow::bail!("nil is not a valid TOML value"),
        other => anyhow::bail!("unsupported Lua type for TOML conversion: {}", other.type_name()),
    }
}

fn lua_table_to_hook_result(table: mlua::Table) -> anyhow::Result<HookResult> {
    let mut result = HookResult::default();

    // metadata (optional)
    if let Ok(LuaValue::Table(meta_tbl)) = table.get::<LuaValue>("metadata") {
        for pair in meta_tbl.pairs::<String, LuaValue>() {
            let (k, v) = pair?;
            result.metadata.insert(k, lua_value_to_toml(&v)?);
        }
    }

    // edits (optional)
    if let Ok(LuaValue::Table(edits_tbl)) = table.get::<LuaValue>("edits") {
        let len = edits_tbl.raw_len();
        for i in 1..=len {
            let entry: mlua::Table = edits_tbl.get(i).map_err(|e| {
                anyhow::anyhow!("edit[{i}] is not a table: {e}")
            })?;
            let start_byte: usize = entry
                .get::<LuaValue>("start_byte")
                .ok()
                .and_then(|v| match v {
                    LuaValue::Integer(n) => Some(n as usize),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("edit[{i}].start_byte must be an integer"))?;
            let end_byte: usize = entry
                .get::<LuaValue>("end_byte")
                .ok()
                .and_then(|v| match v {
                    LuaValue::Integer(n) => Some(n as usize),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("edit[{i}].end_byte must be an integer"))?;
            let text: String = entry
                .get::<LuaValue>("text")
                .ok()
                .and_then(|v| match v {
                    LuaValue::String(s) => s.to_str().ok().as_deref().map(str::to_string),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("edit[{i}].text must be a string"))?;
            result.edits.push(HookTextEdit { start_byte, end_byte, text });
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;
    use crate::hooks::apply::{apply_hook_result, validate_hook_result};
    use crate::handlers::formatting::run_default_hooks;

    fn make_toml_note(title: &str, id: &str, status: &str, relation: &str, body: &str) -> String {
        format!(
            concat!(
                "#import \"../include.typ\": *\n",
                "#let zk-metadata = toml(bytes(\n",
                "  ```toml\n",
                "  schema-version = 1\n",
                "  aliases = []\n",
                "  abstract = \"\"\n",
                "  keywords = []\n",
                "  generated = false\n",
                "  checklist-status = \"{status}\"\n",
                "  relation = \"{relation}\"\n",
                "  relation-target = []\n",
                "  ```.text,\n",
                "))\n",
                "#show: zettel.with(metadata: zk-metadata)\n",
                "\n",
                "= {title} <{id}>\n",
                "\n",
                "{body}",
            ),
            status = status,
            relation = relation,
            title = title,
            id = id,
            body = body,
        )
    }

    #[test]
    fn test_load_and_call_empty_run() {
        let runner = HookRunner::load_str("function run(n) return {} end").unwrap();
        let input = build_hook_note_input(&make_toml_note("Test", "2601010000", "none", "active", ""));
        let result = runner.run(&input).unwrap();
        assert!(result.metadata.is_empty());
        assert!(result.edits.is_empty());
    }

    #[test]
    fn test_note_id_accessible_in_lua() {
        let runner = HookRunner::load_str(
            r#"function run(n) return { metadata = { ["checklist-status"] = n.id } } end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&make_toml_note("Test", "2601010001", "none", "active", ""));
        let result = runner.run(&input).unwrap();
        assert_eq!(
            result.metadata.get("checklist-status"),
            Some(&toml::Value::String("2601010001".to_string()))
        );
    }

    #[test]
    fn test_title_span_accessible() {
        let note = make_toml_note("My Title", "2601010002", "none", "active", "");
        let runner = HookRunner::load_str(
            r#"function run(n)
                 return { metadata = { ["checklist-status"] = tostring(n.title.span.start_line) } }
               end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();
        if let Some(toml::Value::String(s)) = result.metadata.get("checklist-status") {
            let line: usize = s.parse().expect("should be a line number");
            assert!(line > 0, "title should not be on line 0");
        } else {
            panic!("expected a string value");
        }
    }

    #[test]
    fn test_checkbox_span_accessible() {
        let body = "- [ ] a task\n";
        let note = make_toml_note("Test", "2601010003", "none", "active", body);
        let runner = HookRunner::load_str(
            r#"function run(n)
                 return { metadata = { ["checklist-status"] = tostring(n.checkboxes[1].span.start_byte) } }
               end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();
        if let Some(toml::Value::String(s)) = result.metadata.get("checklist-status") {
            let byte: usize = s.parse().expect("should be a byte offset");
            assert!(byte < note.len(), "byte offset in bounds");
            assert!(note[byte..].starts_with("- [ ]"), "should point to checkbox");
        } else {
            panic!("expected a string value");
        }
    }

    #[test]
    fn test_checkbox_line_idx_and_indent_accessible_in_lua() {
        // Single flat checkbox — verify line_idx and indent are reachable from Lua.
        let body = "- [ ] a task\n";
        let note = make_toml_note("Test", "2601010010", "none", "active", body);
        let runner = HookRunner::load_str(
            r#"function run(n)
                 local cb = n.checkboxes[1]
                 return { metadata = {
                   ["checklist-status"] = tostring(cb.line_idx) .. ":" .. tostring(cb.indent)
                 } }
               end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();
        if let Some(toml::Value::String(s)) = result.metadata.get("checklist-status") {
            let parts: Vec<&str> = s.splitn(2, ':').collect();
            assert_eq!(parts.len(), 2);
            let line_idx: usize = parts[0].parse().expect("line_idx should be an integer");
            let indent: usize = parts[1].parse().expect("indent should be an integer");
            // The checkbox is on the body line; there are header lines above it.
            assert!(line_idx > 0, "checkbox should not be on line 0 (header is above)");
            assert_eq!(indent, 0, "top-level checkbox has zero indent");
        } else {
            panic!("expected string metadata value");
        }
    }

    #[test]
    fn test_checkbox_line_idx_order_and_indent_nested() {
        // Three checkboxes: parent at indent=0, two children at indent=2.
        // Verify line_idx is strictly increasing and indent values are correct.
        let body = "- [ ] parent\n  - [ ] child one\n  - [x] child two\n";
        let note = make_toml_note("Test", "2601010011", "none", "active", body);
        let input = build_hook_note_input(&note);

        assert_eq!(input.checkboxes.len(), 3, "expected 3 checkboxes");

        // line_idx strictly increasing
        assert!(
            input.checkboxes[0].line_idx < input.checkboxes[1].line_idx,
            "parent line_idx < first child line_idx"
        );
        assert!(
            input.checkboxes[1].line_idx < input.checkboxes[2].line_idx,
            "first child line_idx < second child line_idx"
        );

        // indent: parent=0, children=2
        assert_eq!(input.checkboxes[0].indent, 0, "parent indent");
        assert_eq!(input.checkboxes[1].indent, 2, "child one indent");
        assert_eq!(input.checkboxes[2].indent, 2, "child two indent");
    }

    #[test]
    fn test_checkbox_indent_enables_tree_reconstruction_in_lua() {
        // Scenario from the task spec:
        //   - [x] @xxx      ← ref item, checked
        //     - [ ]         ← local child, unchecked
        // A Lua hook with access to line_idx+indent can detect that the child
        // is unchecked and should propagate upward to uncheck the parent.
        // This test verifies the runtime supplies enough info (correct indent/line_idx)
        // for that logic, using a Lua script that performs the propagation.
        let body = "- [x] @1234567890\n  - [ ] subtask\n";
        let note = make_toml_note("Test", "2601010012", "none", "active", body);

        let lua_script = r#"
function run(n)
  local cbs = n.checkboxes
  -- Build parent map: for each cb, find the nearest preceding cb with smaller indent
  local parent = {}
  for i = 1, #cbs do
    for j = i - 1, 1, -1 do
      if cbs[j].indent < cbs[i].indent then
        parent[i] = j
        break
      end
    end
  end

  -- Propagate upward: if any child is unchecked, uncheck its parent
  local should_uncheck = {}
  for i = 1, #cbs do
    if not cbs[i].checked then
      local p = parent[i]
      while p do
        should_uncheck[p] = true
        p = parent[p]
      end
    end
  end

  local edits = {}
  for i, cb in ipairs(cbs) do
    if should_uncheck[i] and cb.checked then
      -- Replace '[x]' with '[ ]' at the checkbox position
      local s = n.content:sub(cb.span.start_byte + 1, cb.span.end_byte)
      local new_s = s:gsub("%[x%]", "[ ]", 1)
      if new_s ~= s then
        table.insert(edits, { start_byte = cb.span.start_byte, end_byte = cb.span.end_byte, text = new_s })
      end
    end
  end
  return { edits = edits }
end
"#;
        let runner = HookRunner::load_str(lua_script).unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();

        // The hook should have emitted an edit unchecking the parent ref item.
        assert_eq!(result.edits.len(), 1, "expected one edit to uncheck the parent");
        assert!(
            result.edits[0].text.contains("[ ]"),
            "edit should replace [x] with [ ]"
        );
    }

    #[test]
    fn test_metadata_return_converts() {
        let runner = HookRunner::load_str(
            r#"function run(n) return { metadata = { ["checklist-status"] = "done" } } end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&make_toml_note("Test", "2601010004", "none", "active", ""));
        let result = runner.run(&input).unwrap();
        assert_eq!(
            result.metadata.get("checklist-status"),
            Some(&toml::Value::String("done".to_string()))
        );
    }

    #[test]
    fn test_edits_apply_directly() {
        // Hook returns a byte edit; apply_hook_result applies it as-is (no Rust normalizer).
        let body = "- [ ] my task\n";
        let note = make_toml_note("Test", "2601010005", "none", "active", body);
        let cb_byte = note.find("- [ ] my task").expect("checkbox in note");
        let edit_end = cb_byte + "- [ ] my task".len();
        let lua_src = format!(
            "function run(n)\n  return {{ edits = {{ {{ start_byte = {start}, end_byte = {end_b}, text = \"- [x] my task\" }} }} }}\nend",
            start = cb_byte,
            end_b = edit_end,
        );
        let runner = HookRunner::load_str(&lua_src).unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();
        let output = apply_hook_result(&result, &note).unwrap();
        assert!(output.contains("- [x] my task"), "edit was applied");
        // Status is NOT updated automatically — the Lua hook is responsible for that
        assert!(output.contains("checklist-status = \"none\""), "status unchanged without hook update");
    }

    #[test]
    fn test_invalid_edit_range_errors() {
        let result = HookResult {
            metadata: HashMap::new(),
            edits: vec![HookTextEdit { start_byte: 10, end_byte: 5, text: "x".to_string() }],
        };
        assert!(validate_hook_result(&result, "hello world").is_err());
    }

    #[test]
    fn test_overlapping_edits_error() {
        let content = "hello world";
        let result = HookResult {
            metadata: HashMap::new(),
            edits: vec![
                HookTextEdit { start_byte: 0, end_byte: 5, text: "a".to_string() },
                HookTextEdit { start_byte: 3, end_byte: 8, text: "b".to_string() },
            ],
        };
        assert!(validate_hook_result(&result, content).is_err());
    }

    #[test]
    fn test_missing_run_function_errors() {
        let err = HookRunner::load_str("-- no run function here");
        assert!(err.is_err(), "should error when run function is missing");
    }

    #[test]
    fn test_invalid_return_type_errors() {
        let runner = HookRunner::load_str(r#"function run(n) return "not a table" end"#).unwrap();
        let input = build_hook_note_input(&make_toml_note("Test", "2601010009", "none", "active", ""));
        assert!(runner.run(&input).is_err(), "should error on non-table return");
    }

    /// Hook result is applied directly — the Lua hook is the authority, no Rust correction.
    #[test]
    fn test_metadata_patch_applied_directly() {
        let body = "- [x] leaf a\n- [x] leaf b\n";
        let note = make_toml_note("Test", "2601010010", "none", "active", body);
        let runner = HookRunner::load_str(
            r#"function run(n) return { metadata = { ["checklist-status"] = "todo" } } end"#,
        )
        .unwrap();
        let input = build_hook_note_input(&note);
        let result = runner.run(&input).unwrap();
        let output = apply_hook_result(&result, &note).unwrap();
        // The hook's value is applied as-is — no Rust normalizer overrides it
        assert!(
            output.contains("checklist-status = \"todo\""),
            "hook value applied directly; got:\n{output}"
        );
    }

    #[test]
    fn test_idempotency() {
        let body = "- [x] task\n";
        let note = make_toml_note("Test", "2601010011", "done", "active", body);
        let result = HookResult::default();
        let first = apply_hook_result(&result, &note).unwrap();
        let second = apply_hook_result(&result, &first).unwrap();
        assert_eq!(first, second, "apply_hook_result must be idempotent");
    }

    // -----------------------------------------------------------------------
    // Default hook pipeline tests (run_default_hooks)
    // These verify that the embedded checklist.lua + relation_status.lua hooks
    // produce output equivalent to the original Rust formatter.
    // -----------------------------------------------------------------------

    #[test]
    fn default_hooks_all_children_done_parent_becomes_checked() {
        let body = "- [ ] parent\n  - [x] child one\n  - [x] child two\n";
        let note = make_toml_note("Test", "2601020001", "none", "active", body);
        let out = run_default_hooks(&note);
        assert!(out.contains("- [x] parent"), "parent should be checked");
        assert!(out.contains("checklist-status = \"done\""), "status should be done");
    }

    #[test]
    fn default_hooks_any_child_incomplete_parent_unchecked() {
        let body = "- [x] parent\n  - [x] child one\n  - [ ] child two\n";
        let note = make_toml_note("Test", "2601020002", "none", "active", body);
        let out = run_default_hooks(&note);
        assert!(out.contains("- [ ] parent"), "parent should be unchecked");
    }

    #[test]
    fn default_hooks_three_level_propagates() {
        let body = "- [ ] grandparent\n  - [ ] parent\n    - [x] grandchild\n";
        let note = make_toml_note("Test", "2601020003", "none", "active", body);
        let out = run_default_hooks(&note);
        assert!(out.contains("- [x] grandparent"), "grandparent propagated to done");
        assert!(out.contains("checklist-status = \"done\""), "status done");
    }

    #[test]
    fn default_hooks_archived_status_is_done() {
        let body = "- [ ] unfinished task\n";
        let note = make_toml_note("Test", "2601020004", "none", "archived", body);
        let out = run_default_hooks(&note);
        assert!(out.contains("checklist-status = \"done\""), "archived note → done");
    }

    #[test]
    fn default_hooks_legacy_status_is_done() {
        let note = make_toml_note("Test", "2601020005", "none", "legacy", "");
        let out = run_default_hooks(&note);
        assert!(out.contains("checklist-status = \"done\""), "legacy note → done");
    }

    #[test]
    fn default_hooks_idempotent() {
        let body = "- [ ] parent\n  - [x] child\n";
        let note = make_toml_note("Test", "2601020006", "none", "active", body);
        let first = run_default_hooks(&note);
        let second = run_default_hooks(&first);
        assert_eq!(first, second, "default hooks must be idempotent");
    }

    #[test]
    fn default_hooks_trailing_newline_preserved() {
        let body_with = "- [ ] parent\n  - [x] child\n";
        let body_without = "- [ ] parent\n  - [x] child";
        let note_with = make_toml_note("Test", "2601020007", "none", "active", body_with);
        let note_without = make_toml_note("Test", "2601020008", "none", "active", body_without);
        assert!(run_default_hooks(&note_with).ends_with('\n'), "trailing newline preserved");
        assert!(!run_default_hooks(&note_without).ends_with('\n'), "no spurious newline added");
    }
}

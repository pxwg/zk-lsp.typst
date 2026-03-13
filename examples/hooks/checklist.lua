-- Hook: nested checkbox propagation + checklist-status update
--
-- Requires each checkbox to expose:
--   cb.line_idx : integer
--   cb.indent   : integer
--
-- Semantics:
--   1. All checkboxes (local/ref) participate in the same indentation tree.
--   2. If a checkbox has children, its effective checked state is determined
--      solely by whether all children are checked.
--   3. If a checkbox is a leaf, its effective checked state is its observed checked state.
--   4. checklist-status is computed from all leaf checkboxes after propagation.
--
-- No cross-file information is read here.

---@class Span
---@field start_byte integer
---@field end_byte integer
---@field start_line integer
---@field start_col integer
---@field end_line integer
---@field end_col integer

---@class Checkbox
---@field id string
---@field kind '"local"'|'"ref"'
---@field checked boolean
---@field targets string[]
---@field text string
---@field span Span
---@field line_idx integer
---@field indent integer

---@class HookNode
---@field cb Checkbox
---@field line_idx integer
---@field indent integer
---@field observed_checked boolean
---@field effective_checked boolean
---@field parent HookNode|nil
---@field children HookNode[]

local M = {}

---@param checkboxes Checkbox[]
---@return HookNode[]
local function build_nodes(checkboxes)
  local nodes = {}

  for _, cb in ipairs(checkboxes) do
    if cb.line_idx ~= nil and cb.indent ~= nil then
      table.insert(nodes, {
        cb = cb,
        line_idx = cb.line_idx,
        indent = cb.indent,
        observed_checked = cb.checked,
        effective_checked = cb.checked,
        parent = nil,
        children = {},
      })
    end
  end

  table.sort(nodes, function(a, b)
    if a.line_idx ~= b.line_idx then
      return a.line_idx < b.line_idx
    end
    return a.indent < b.indent
  end)

  return nodes
end

---@param nodes HookNode[]
---@return HookNode[]
local function build_tree(nodes)
  local roots = {}
  local stack = {}

  for _, node in ipairs(nodes) do
    while #stack > 0 and stack[#stack].indent >= node.indent do
      table.remove(stack)
    end

    if #stack == 0 then
      table.insert(roots, node)
    else
      local parent = stack[#stack]
      node.parent = parent
      table.insert(parent.children, node)
    end

    table.insert(stack, node)
  end

  return roots
end

---@param node HookNode
---@return string
local function observed_state_char(node)
  return node.observed_checked and "x" or " "
end

---@param node HookNode
---@return string
local function effective_state_char(node)
  return node.effective_checked and "x" or " "
end

---@param node HookNode
---@return integer
local function state_byte(node)
  -- same convention as before:
  -- "- [ ] ..." => state char at start_byte + indent + 3
  return node.cb.span.start_byte + node.indent + 3
end

---@param edits table[]
---@param node HookNode
local function emit_edit_if_needed(edits, node)
  local old_char = observed_state_char(node)
  local new_char = effective_state_char(node)
  if old_char ~= new_char then
    local b = state_byte(node)
    table.insert(edits, {
      start_byte = b,
      end_byte = b + 1,
      text = new_char,
    })
  end
end

---@param node HookNode
---@param edits table[]
local function propagate_node(node, edits)
  for _, child in ipairs(node.children) do
    propagate_node(child, edits)
  end

  if #node.children > 0 then
    local all_done = true
    for _, child in ipairs(node.children) do
      if not child.effective_checked then
        all_done = false
        break
      end
    end
    node.effective_checked = all_done
  else
    node.effective_checked = node.observed_checked
  end

  emit_edit_if_needed(edits, node)
end

---@param roots HookNode[]
---@return table[]
local function propagate_tree(roots)
  local edits = {}
  for _, root in ipairs(roots) do
    propagate_node(root, edits)
  end
  return edits
end

---@param node HookNode
---@param leaves HookNode[]
local function collect_leaves(node, leaves)
  if #node.children == 0 then
    table.insert(leaves, node)
    return
  end
  for _, child in ipairs(node.children) do
    collect_leaves(child, leaves)
  end
end

---@param roots HookNode[]
---@return HookNode[]
local function leaf_nodes(roots)
  local leaves = {}
  for _, root in ipairs(roots) do
    collect_leaves(root, leaves)
  end
  return leaves
end

---@param leaves HookNode[]
---@return string|nil
local function compute_status(leaves)
  if #leaves == 0 then
    return nil
  end

  local all_done = true
  local any_done = false

  for _, leaf in ipairs(leaves) do
    if leaf.effective_checked then
      any_done = true
    else
      all_done = false
    end
  end

  if all_done then
    return "done"
  elseif any_done then
    return "wip"
  else
    return "todo"
  end
end

---@param note table
---@return table
function run(note)
  local nodes = build_nodes(note.checkboxes)
  if #nodes == 0 then
    return {}
  end

  local roots = build_tree(nodes)
  local edits = propagate_tree(roots)
  local leaves = leaf_nodes(roots)
  local status = compute_status(leaves)

  local result = { edits = edits }
  if status ~= nil then
    result.metadata = {
      ["checklist-status"] = status,
    }
  end
  return result
end

-- nvim-zio: register zio-lsp as a Neovim LSP client.
-- Pure configuration; no third-party plugins. Uses Neovim 0.11+ vim.lsp.start API.

local M = {}

---@class ZioLspConfig
---@field cmd? string|string[] Command (or argv) used to launch zio-lsp.
---@field cmd_args? string[] Additional arguments.
---@field capabilities? table LSP client capabilities.
---@field root_markers? string[] Markers used to identify the workspace root.

---Default command. Override through setup({ cmd = ... }).
M.default_cmd = "zio-lsp"

---@type ZioLspConfig
M.config = {
  cmd = M.default_cmd,
  cmd_args = {},
  root_markers = { ".git", "zio.toml", "Cargo.toml" },
}

---Merge a user-provided config over M.config.
---@param user? ZioLspConfig
---@return ZioLspConfig
local function resolve(user)
  if not user then return M.config end
  return vim.tbl_deep_extend("force", vim.deepcopy(M.config), user)
end

---Public setup entry point. Call this from init.lua or a filetype plugin.
---@param user? ZioLspConfig
function M.setup(user)
  M.config = resolve(user)
end

---Build the cmd list from M.config.
---@return string[]
local function build_cmd()
  local cmd = M.config.cmd
  if type(cmd) == "string" then return vim.list_extend({ cmd }, M.config.cmd_args or {}) end
  if type(cmd) == "table" then return vim.list_extend(vim.deepcopy(cmd), M.config.cmd_args or {}) end
  error("zio-lsp cmd must be a string or list")
end

---Capabilities advertised by the editor to zio-lsp.
---Includes semanticTokens, documentSymbol, definition, references, hover,
---completion (with trigger characters), and UTF-16 position encoding.
---@return table
function M.capabilities()
  local caps = vim.lsp.protocol.make_client_capabilities()
  caps.offsetEncoding = { "utf-16" }
  caps.textDocument.semanticTokens = {
    tokenTypes = { "namespace", "type", "function", "variable", "keyword", "string", "number", "comment", "operator" },
    tokenModifiers = { "declaration" },
    formats = { "relative" },
    full = { delta = false },
  }
  caps.textDocument.completion = caps.textDocument.completion or {}
  caps.textDocument.completion.completionItem = {
    snippetSupport = false,
  }
  return caps
end

---Attach zio-lsp to the current buffer. Called by a FileType autocmd.
---@param buf integer
function M.attach(buf)
  local cmd = build_cmd()
  local filetypes = { "zio" }
  vim.lsp.start({
    name = "zio-lsp",
    cmd = cmd,
    filetypes = filetypes,
    root_dir = function(start_path)
      local markers = M.config.root_markers or {}
      return vim.fs.root(start_path, markers) or start_path
    end,
    capabilities = M.capabilities(),
    init_options = { moduleRoots = {} },
  })
end

return M

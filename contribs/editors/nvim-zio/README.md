# nvim-zio

Zio language support for Neovim. Connects to the `zio-lsp` binary over stdio
using the built-in `vim.lsp.start` API (Neovim 0.11+).

## Layout

```
contribs/editors/nvim-zio/
├── ftdetect/
│   └── zio.lua        # sets filetype=zio for *.zio buffers
├── plugin/
│   └── zio.lua        # registers FileType autocmd → require('zio').attach(buf)
├── lua/
│   └── zio.lua        # M.setup, M.attach, M.capabilities (uses vim.lsp.start)
└── syntax/
    └── zio.vim        # baseline syntax fallback (regex)
```

## Install

Add the directory to Neovim's runtimepath. The simplest way is to symlink
the plugin directory into `~/.config/nvim/pack/zio/start/zio`:

```bash
mkdir -p ~/.config/nvim/pack/zio/start
ln -s /home/lszio/Projects/zio/contribs/editors/nvim-zio \
      ~/.config/nvim/pack/zio/start/zio
```

Or, with a plugin manager (lazy.nvim example):

```lua
{
  dir = "/home/lszio/Projects/zio/contribs/editors/nvim-zio",
  ft = { "zio" },
}
```

## Configuration

`require("zio").setup({...})` accepts:

| Field | Default | Description |
|-------|---------|-------------|
| `cmd` | `"zio-lsp"` | Command (or argv list) used to launch the server. |
| `cmd_args` | `{}` | Additional arguments passed to the server. |
| `root_markers` | `{ ".git", "zio.toml", "Cargo.toml" }` | Markers used by `vim.fs.root` to identify the workspace root. |

Example:

```lua
require("zio").setup({
  cmd = "/home/lszio/.cargo/bin/zio-lsp",
  cmd_args = { "--log-level=info" },
})
```

After `setup`, opening any `*.zio` buffer triggers `vim.lsp.start` which
spawns the server and registers the standard LSP keybindings (gd, gr, K,
`<C-x><C-o>`). The server advertises UTF-16 position encoding via the
`offsetEncoding` capability so LSP line/column conversions match across
CJK, emoji, and CRLF input.

## Optional: Tree-sitter highlighting

`contribs/tree-sitter-zio` is independent of this Neovim package. After the
grammar is generated and registered with nvim-treesitter, syntax highlighting
will use the parser instead of the regex fallback above.

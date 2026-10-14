-- plugin/zio.lua: filetype detection + autocmd hook for zio-lsp.

if vim.g.loaded_zio_nvim then return end
vim.g.loaded_zio_nvim = true

vim.api.nvim_create_autocmd("FileType", {
  pattern = "zio",
  callback = function(args)
    require("zio").attach(args.buf)
  end,
})

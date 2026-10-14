-- ftdetect/zio.lua: register the zio filetype for *.zio files.
vim.api.nvim_create_autocmd("BufRead", {
  pattern = "*.zio",
  callback = function(args)
    vim.bo[args.buf].filetype = "zio"
  end,
})

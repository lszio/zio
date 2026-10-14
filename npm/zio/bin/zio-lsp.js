#!/usr/bin/env node
// `zio-lsp` on PATH: the language server that ships inside the platform
// archive, reached through the CLI wrapper so an editor that starts
// `zio-lsp` gets the server that belongs to this install.
"use strict";

const path = require("path");
const { spawnSync } = require("child_process");
const { ensureBinary } = require("../install.js");

const binary = ensureBinary();
const sibling = path.join(path.dirname(binary), "zio-lsp");
const child = spawnSync(sibling, process.argv.slice(2), { stdio: "inherit" });
if (child.error) {
  console.error(`zio: cannot run the language server at ${sibling}: ${child.error.message}`);
  process.exit(1);
}
process.exit(child.status === null ? 1 : child.status);

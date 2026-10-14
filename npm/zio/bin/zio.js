#!/usr/bin/env node
// `zio` on PATH: make sure the platform binary is present, then run it
// with this process's stdio and exit with its status, so a wrapper in
// front of the real CLI is invisible to the shell.
"use strict";

const { spawnSync } = require("child_process");
const { ensureBinary } = require("../install.js");

const binary = ensureBinary();
const child = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (child.error) {
  console.error(`zio: cannot run ${binary}: ${child.error.message}`);
  process.exit(1);
}
process.exit(child.status === null ? 1 : child.status);

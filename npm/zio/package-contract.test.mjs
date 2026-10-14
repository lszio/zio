// The packaged layout is not the repository layout.
//
// npm publishes the files listed in `files`, rooted at the package, so a
// file the wrapper expects beside it must survive that flattening. The
// repository has npm/package.json next door, which is why
// `require("../package.json")` looked correct here and resolved to
// nothing once published: @lszio/zio@0.2.0 shipped a CLI that exited with
// "Cannot find module '../package.json'" on its first invocation.
//
// This stages the package the way npm does and requires it from there,
// which is the smallest thing that fails when that contract breaks.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const pkgDir = dirname(fileURLToPath(import.meta.url));
const manifest = JSON.parse(readFileSync(join(pkgDir, "package.json"), "utf8"));

const staged = mkdtempSync(join(tmpdir(), "zio-npm-package-"));
for (const entry of manifest.files) {
  assert.ok(existsSync(join(pkgDir, entry)), `files lists ${entry}, which does not exist`);
  cpSync(join(pkgDir, entry), join(staged, entry), { recursive: true });
}
// npm writes the manifest at the package root whatever `files` says.
cpSync(join(pkgDir, "package.json"), join(staged, "package.json"));

// Requiring the wrapper must not reach outside the package: it resolves
// its own version and modules from its own directory.
execFileSync(process.execPath, ["-e", "require(process.argv[1])", join(staged, "install.js")], {
  stdio: "pipe",
});

console.log(`PASS ${manifest.name}@${manifest.version} resolves inside its own package layout`);

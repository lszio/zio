// Fetch the platform binary for this @lszio/zio install.
//
// The npm package ships no binaries of its own: it downloads the archive
// that matches the installed version from the release tagged with that
// same version, verifies it against the published checksum, and unpacks
// it beside this file. The version the wrapper installs and the version
// of the CLI it runs are therefore the same number by construction —
// there is no second version to drift.
//
// The download happens on first run rather than on install, so an
// install inside an offline build does not fail; the first `zio`
// invocation reports the failure it actually hit.
"use strict";

const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const REPO = "lszio/zio";
// The version this package installs is its own version. It has to be
// read from the package root, not from a parent directory: npm publishes
// the files listed in `files` rooted at the package, so `../package.json`
// exists in the repository (it is the engine package next door) and
// resolves to nothing at all once published.
const VERSION = require("./package.json").version;
const BASE = `https://github.com/${REPO}/releases/download/v${VERSION}`;

const TARGETS = {
  "linux-x64": "zio-linux-x86_64.tar.gz",
  "linux-arm64": "zio-linux-aarch64.tar.gz",
  "darwin-x64": "zio-macos-x86_64.tar.gz",
  "darwin-arm64": "zio-macos-aarch64.tar.gz",
  "win32-x64": "zio-windows-x86_64.zip",
};

const key = `${process.platform}-${process.arch}`;
const archive = TARGETS[key];
if (!archive) {
  console.error(
    `zio: no release archive for ${key}; build from source or download a ` +
      `platform archive from https://github.com/${REPO}/releases`,
  );
  process.exit(1);
}

const vendor = path.join(__dirname, "..", "vendor");
const archivePath = path.join(vendor, archive);
const tree = path.join(vendor, archive.replace(/\.(tar\.gz|zip)$/, ""));
const binary = path.join(tree, "bin", process.platform === "win32" ? "zio.exe" : "zio");

/// Download, verify and unpack the archive for this platform, then
/// return the path of the `zio` binary inside it.
function ensureBinary() {
  if (!fs.existsSync(binary)) {
    download();
  }
  return binary;
}

function fail(message) {
  console.error(`zio: ${message}`);
  process.exit(1);
}

function fetch(url) {
  const curl = spawnSync("curl", ["-fsSL", "--retry", "3", url], {
    maxBuffer: 512 * 1024 * 1024,
  });
  if (curl.error) {
    fail(`cannot download ${url}: ${curl.error.message}`);
  }
  if (curl.status !== 0) {
    fail(
      `cannot download ${url} (curl exit ${curl.status}); ` +
        `check that release v${VERSION} publishes ${archive}`,
    );
  }
  return curl.stdout;
}

function download() {
  fs.mkdirSync(vendor, { recursive: true });
  process.stderr.write(`zio: downloading ${archive} (v${VERSION})…\n`);

  const bytes = fetch(`${BASE}/${archive}`);
  fs.writeFileSync(archivePath, bytes);

  // The release publishes a checksum beside the archive. A binary that
  // does not match its checksum is a supply-chain event, not a warning.
  const expected = fetch(`${BASE}/${archive}.sha256`).toString("utf8").trim().split(/\s+/)[0];
  const actual = crypto.createHash("sha256").update(bytes).digest("hex");
  if (expected !== actual) {
    fs.rmSync(archivePath, { force: true });
    fail(`checksum mismatch for ${archive}: expected ${expected}, got ${actual}`);
  }

  if (archive.endsWith(".zip")) {
    const unzip = spawnSync(
      "powershell",
      [
        "-NoProfile",
        "-Command",
        `Expand-Archive -LiteralPath '${archivePath}' -DestinationPath '${vendor}' -Force`,
      ],
      { stdio: "inherit" },
    );
    if (unzip.error || unzip.status !== 0) {
      fail(`cannot unpack ${archive}`);
    }
  } else {
    const tar = spawnSync("tar", ["-xzf", archivePath, "-C", vendor], { stdio: "inherit" });
    if (tar.error || tar.status !== 0) {
      fail(`cannot unpack ${archive} (GNU tar must be on PATH)`);
    }
  }
  fs.rmSync(archivePath, { force: true });
  if (!fs.existsSync(binary)) {
    fail(`the unpacked archive did not contain bin/${path.basename(binary)}`);
  }
}

module.exports = { ensureBinary, binary };

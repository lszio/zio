#!/usr/bin/env bash
# Assemble an independent Zio distribution.
#
# The point of this script is that the result does not need the checkout.
# A launcher that reads `../..` to find its own stdlib is a launcher that
# works until someone moves the directory, or installs it read-only. So
# everything that is loaded at runtime is copied into one tree, and the
# tree is what gets run.
#
# Usage:
#   tools/install.sh [target-dir]        # default: dist/zio
#   tools/install.sh /opt/zio
#
# Environment:
#   ZIO_PROFILE   build profile: release (default) or debug
#   ZIO_TARGET    rust target triple. When set — and the release
#                 workflow always sets it — the script builds FOR that
#                 triple and installs from target/<triple>/<profile>.
#                 It never silently installs the host binary in its
#                 place: an x86_64 macOS archive that carries an arm64
#                 binary is a wrong answer that looks like success.
#
# Layout produced:
#   <target>/bin/zio            the language CLI
#   <target>/bin/zio-lsp        the language server
#   <target>/bin/zio-env.sh     PATH and ZIO_PATH for the installed tree
#   <target>/bin/zio-env.ps1    the same for PowerShell (Windows targets)
#   <target>/share/zio/...      stdlib, libraries, toolchain sources
#   <target>/share/zio/grammar   Tree-sitter grammar and queries
#   <target>/share/zio/docs      documentation
#   <target>/share/tensor/      the tensor backend resource
#   <target>/manifest.json      components with checksums and versions
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-$ROOT/dist/zio}"
PROFILE="${ZIO_PROFILE:-release}"
TRIPLE="${ZIO_TARGET:-}"

case "$TRIPLE" in
  *windows*) EXE=".exe"; IS_WINDOWS=1 ;;
  *) EXE=""; IS_WINDOWS=0 ;;
esac

# sha256 of a file, by whichever tool this platform has. An archive
# built without any of them records no checksum rather than failing the
# whole install.
checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    printf 'unavailable'
  fi
}

# The release manifest: one file that says what this tree contains, and
# that the bytes on disk are the bytes that shipped. A consumer
# verifying an installed tree reads checksums from here instead of
# trusting the filesystem.
write_manifest() {
  local version commit
  version="$(awk -F'"' '/^version/ {print $2; exit}' "$ROOT/Cargo.toml")"
  if [ -d "$ROOT/.git" ]; then
    commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  else
    commit="unknown"
  fi

  {
    printf '{\n'
    printf '  "name": "zio",\n'
    printf '  "version": "%s",\n' "$version"
    printf '  "commit": "%s",\n' "$commit"
    printf '  "target": "%s",\n' "${TRIPLE:-host}"
    printf '  "profile": "%s",\n' "$PROFILE"
    printf '  "components": [\n'
    local first=1 f rel sum size
    while IFS= read -r f; do
      rel="${f#"$TARGET"/}"
      sum="$(checksum "$f")"
      size="$(wc -c < "$f" | tr -d ' ')"
      if [ "$first" = "1" ]; then first=0; else printf ',\n'; fi
      printf '    {"path": "%s", "bytes": %s, "sha256": "%s"}' "$rel" "$size" "$sum"
    done < <(cd "$TARGET" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do printf '%s\n' "$TARGET/$f"; done)
    printf '\n  ]\n'
    printf '}\n'
  } > "$TARGET/manifest.json"
}

# ── Build and stage ───────────────────────────────────────────────

CARGO_FLAGS=("--release")
if [ "$PROFILE" = "debug" ]; then
  CARGO_FLAGS=()
fi
# The build must target the triple the archive claims. An empty TRIPLE
# keeps the host build, which is what a local install.sh run wants.
if [ -n "$TRIPLE" ]; then
  CARGO_FLAGS+=("--target" "$TRIPLE")
fi

echo "==> building (profile: $PROFILE, target: ${TRIPLE:-host})"
cargo build -p zio-cli -p zio-lsp "${CARGO_FLAGS[@]}"

# When a triple is named, the binaries live under target/<triple>/…;
# otherwise they are the host build's target/<profile>/….
if [ -n "$TRIPLE" ]; then
  BIN="$ROOT/target/$TRIPLE/$PROFILE"
else
  BIN="$ROOT/target/$PROFILE"
fi

echo "==> staging into $TARGET"
rm -rf "$TARGET"
mkdir -p "$TARGET/bin" "$TARGET/share/zio" "$TARGET/share/tensor"

for name in zio-cli zio-lsp; do
  src="$BIN/$name$EXE"
  if [ ! -f "$src" ]; then
    echo "missing binary $src" >&2
    exit 1
  fi
  case "$name" in
    zio-cli) installed="zio$EXE" ;;
    *) installed="zio-lsp$EXE" ;;
  esac
  install -m 0755 "$src" "$TARGET/bin/$installed"
done

# The share tree mirrors the repository layout, because module paths are
# dotted names resolved against a root — `require :libs.loom.agent`
# becomes `libs/loom/agent.zio` under a granted root. Preserving the
# layout is what keeps those names meaningful outside the checkout.
if [ -d "$ROOT/libs" ]; then
  mkdir -p "$TARGET/share/zio"
  cp -R "$ROOT/libs" "$TARGET/share/zio/"
fi

# Zio sources: stdlib, metacircular interpreter, compiler. Their
# canonical home is inside the zio-core crate, which embeds them at
# build time; the installer stages them back under the paths a checkout
# would have had (`langs/interpreter`, `langs/compiler`, `std`), so the
# shipped tree carries the toolchain it runs rather than only its binary.
for part in std/core.zio langs/interpreter/reader.zio langs/interpreter/expand.zio langs/interpreter/evaluator.zio langs/compiler/analyze.zio langs/compiler/compiler.zio; do
  # The asset path inside the crate drops the staged `langs/` prefix.
  asset="langs/core/assets/${part#langs/}"
  if [ ! -f "$ROOT/$asset" ]; then
    echo "missing asset $asset" >&2
    exit 1
  fi
  mkdir -p "$TARGET/share/zio/$(dirname "$part")"
  cp "$ROOT/$asset" "$TARGET/share/zio/$part"
done

# Editor grammar: Tree-sitter sources and queries. Queries are what an
# editor loads; the rest is what regenerates them.
if [ -d "$ROOT/contribs/tree-sitter-zio" ]; then
  mkdir -p "$TARGET/share/zio/grammar"
  for f in grammar.js tree-sitter.json package.json queries; do
    if [ -e "$ROOT/contribs/tree-sitter-zio/$f" ]; then
      cp -R "$ROOT/contribs/tree-sitter-zio/$f" "$TARGET/share/zio/grammar/"
    fi
  done
  if [ -d "$ROOT/contribs/tree-sitter-zio/src" ]; then
    mkdir -p "$TARGET/share/zio/grammar/src"
    cp -R "$ROOT/contribs/tree-sitter-zio/src/." "$TARGET/share/zio/grammar/src/"
  fi
fi

# Documentation and examples travel with the tree: a language release
# whose reference lives only in a git remote is not self-contained.
for dir in docs examples; do
  if [ -d "$ROOT/$dir" ]; then
    mkdir -p "$TARGET/share/zio/$(dirname "$dir")"
    cp -R "$ROOT/$dir" "$TARGET/share/zio/$(dirname "$dir")/"
  fi
done

find "$TARGET/share/zio" -name '*.rs' -delete

if [ -d "$ROOT/contribs/tensor" ]; then
  cp -R "$ROOT/contribs/tensor/." "$TARGET/share/tensor/"
fi

# A published tree is not a working copy. Python test sources and
# interpreter caches are always present in a checkout and never needed
# at runtime, and shipping them makes the artifact claim a completeness
# it does not have. Only Python test files are removed: a directory
# named `tests` may hold data a program reads at runtime, and deleting
# the directory would be a silent data loss rather than a cleanup.
find "$TARGET/share" -type d -name '__pycache__' -prune -exec rm -rf {} + 2>/dev/null || true
find "$TARGET/share" -type f -name '*.pyc' -delete 2>/dev/null || true
find "$TARGET/share" -type f \( -name 'test_*.py' -o -name '*_test.py' \) -delete 2>/dev/null || true
find "$TARGET/share" -type d -name 'tests' -empty -delete 2>/dev/null || true

cat > "$TARGET/bin/zio-env.sh" <<'ENVSH'
# Source this to get a Zio that can load the installed libraries.
export ZIO_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/../share/zio" && pwd)"
export PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd):$PATH"
ENVSH
chmod 0644 "$TARGET/bin/zio-env.sh"

if [ "$IS_WINDOWS" = "1" ]; then
  cat > "$TARGET/bin/zio-env.ps1" <<'ENVPS1'
# Dot-source this to get a Zio that can load the installed libraries.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:ZIO_PATH = (Resolve-Path "$here\..\share\zio").Path
$env:PATH = "$here;$env:PATH"
ENVPS1
fi

write_manifest

echo
echo "installed to $TARGET"
echo "  ZIO_PATH=$TARGET/share/zio"

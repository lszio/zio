#!/usr/bin/env bash
# Assemble an independent Zio distribution.
#
# The point of this script is that the result does not need the checkout.
# A launcher that reads `../..` to find its own stdlib is a launcher that
# works until someone moves the directory, and a Grove that resolves its
# worker or its web assets relative to `CARGO_MANIFEST_DIR` cannot be
# installed at all. So everything that is loaded at runtime is copied
# into one tree, and the tree is what gets run.
#
# Usage:
#   tools/install.sh [target-dir]        # default: dist/zio
#
# Layout produced:
#   <target>/bin/zio            the language CLI
#   <target>/bin/zio-lsp        the language server
#   <target>/bin/grove          the Grove launcher
#   <target>/share/zio/...      stdlib, libraries, compiler, apps
#   <target>/share/tensor/      the tensor backend resource
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-$ROOT/dist/zio}"
PROFILE="${ZIO_PROFILE:-release}"
CARGO_FLAGS=("--release")
if [ "$PROFILE" = "debug" ]; then
  CARGO_FLAGS=()
fi

echo "==> building (profile: $PROFILE)"
cargo build -p zio-cli -p zio-lsp "${CARGO_FLAGS[@]}"

BIN="$ROOT/target/$PROFILE"
[ "$PROFILE" = "release" ] && BIN="$ROOT/target/release"

echo "==> staging into $TARGET"
rm -rf "$TARGET"
mkdir -p "$TARGET/bin" "$TARGET/share/zio" "$TARGET/share/tensor"

install -m 0755 "$BIN/zio-cli" "$TARGET/bin/zio"
if [ -x "$BIN/zio-lsp" ]; then
  install -m 0755 "$BIN/zio-lsp" "$TARGET/bin/zio-lsp"
fi

# The share tree mirrors the repository layout, because module paths are
# dotted names resolved against a root — `require :libs.loom.agent`
# becomes `libs/loom/agent.zio` under a granted root. Preserving the
# layout is what keeps those names meaningful outside the checkout.
for dir in libs apps/grove langs/compiler; do
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
# interpreter caches are always present in a checkout and never needed at
# runtime, and shipping them makes the artifact claim a completeness it
# does not have. Only Python test files are removed: a directory named
# `tests` may hold data a program reads at runtime, and deleting the
# directory would be a silent data loss rather than a cleanup.
find "$TARGET/share" -type d -name '__pycache__' -prune -exec rm -rf {} + 2>/dev/null || true
find "$TARGET/share" -type f -name '*.pyc' -delete 2>/dev/null || true
find "$TARGET/share" -type f \( -name 'test_*.py' -o -name '*_test.py' \) -delete 2>/dev/null || true
find "$TARGET/share" -type d -name 'tests' -empty -delete 2>/dev/null || true

# The CPU worker and the web assets are staged under share/grove; the
# launcher grants their directories to the jail read-only.
if [ -d "$ROOT/apps/grove/workers/torch" ]; then
  mkdir -p "$TARGET/share/grove/workers" "$TARGET/share/grove/web"
  cp -R "$ROOT/apps/grove/workers/torch" "$TARGET/share/grove/workers/torch"
  cp -R "$ROOT/apps/grove/web/." "$TARGET/share/grove/web/"
  find "$TARGET/share/grove" -name '__pycache__' -prune -exec rm -rf {} + 2>/dev/null || true
fi

echo "==> writing launcher"
cat > "$TARGET/bin/grove" <<LAUNCHER
#!/usr/bin/env bash
# Grove launcher. Grants are decided HERE, from explicit flags, and
# handed to the Zio entry as one keyword map. Nothing is inferred from
# the environment, and nothing is read out of the source it is about to
# run — a candidate that could name its own tensor backend would make
# the isolation profile advisory.
set -euo pipefail
HERE="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
SHARE="\$(readlink -f "\$HERE/../share")"

ROOT_DIR="\${GROVE_ROOT:-\$PWD}"
BIND="\${GROVE_BIND:-127.0.0.1:8787}"
PYTHON="\${GROVE_TENSOR_PYTHON:-python3}"
WORKERS="\${GROVE_WORKERS:-2}"

# The jail pivots everything away except what it mounts, so the worker's
# script directory and the real interpreter's tree must be granted
# read-only. The venv python is exec'd by its un-resolved path so CPython
# finds pyvenv.cfg; the mounts cover both the real install tree and the
# alias directory the symlink points through.
WORKER="\${GROVE_WORKER_SCRIPT:-\$SHARE/grove/workers/torch/worker.py}"
PYTHON_BIN="\$(command -v "\$PYTHON")"
PYTHON_REAL="\$(readlink -f "\$PYTHON_BIN")"
PYTHON_VENV="\$(readlink -f "\$(dirname "\$PYTHON_BIN")/..")"
PYTHON_BASE="\$(readlink -f "\$(dirname "\$PYTHON_REAL")/..")"
PYTHON_ALIAS="\$(readlink -f "\$(dirname "\$PYTHON_REAL")/../..")"
# A mount target nested inside another is refused by the jail, and /usr
# is granted whole by the service, so overlapping entries are collapsed.
WORKER_MOUNTS=(--app-worker-mount "\$(cd "\$(dirname "\$WORKER")" && pwd)")
WORKER_MOUNTS+=(--app-worker-mount "\$PYTHON_VENV")
case "\$PYTHON_ALIAS" in
  /|/usr|/usr/*) WORKER_MOUNTS+=(--app-worker-mount "\$PYTHON_BASE") ;;
  *) WORKER_MOUNTS+=(--app-worker-mount "\$PYTHON_ALIAS") ;;
esac

# Everything the caller passed belongs to the application, not to the
# launcher. Each becomes one --args token, so `grove --help` reaches
# the Grove CLI instead of being refused as an unknown launcher option.
app_args=()
for arg in "\$@"; do
  app_args+=(--args "\$arg")
done

exec "\$HERE/zio" \\
  --app-root "\$SHARE/zio/apps/grove" \\
  --app-share "\$SHARE/zio" \\
  --app-resource-root "\$SHARE" \\
  --app-web-root "\$SHARE/grove/web" \\
  --app-tensor-backend "\$SHARE/tensor/backend.py" \\
  --app-tensor-python "\$PYTHON" \\
  --app-root-dir "\$ROOT_DIR" \\
  --app-bind "\$BIND" \\
  --app-workers "\$WORKERS" \\
  --app-worker-script "\$WORKER" \\
  --app-worker-python "\$PYTHON_BIN" \\
  \${WORKER_MOUNTS[@]+"\${WORKER_MOUNTS[@]}"} \\
  --app "\$SHARE/zio/apps/grove/main.zio" \\
  \${app_args[@]+"\${app_args[@]}"}
LAUNCHER
chmod 0755 "$TARGET/bin/grove"

cat > "$TARGET/bin/zio-env.sh" <<'ENVSH'
# Source this to get a Zio that can load the installed libraries.
export ZIO_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/../share/zio" && pwd)"
export PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd):$PATH"
ENVSH
chmod 0644 "$TARGET/bin/zio-env.sh"

echo
echo "installed to $TARGET"
echo "  ZIO_PATH=$TARGET/share/zio"
echo "  grove    : \$TARGET/bin/grove demo --case dual --root /tmp/grove-dual"

#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

emit_status() {
  local tests special_forms native_bindings runnable_examples
  tests="$(cargo test --workspace -- --list | awk '/: test$/ { count += 1 } END { print count + 0 }')"
  special_forms="$(rg -n '=> Some\(' core/src/special/mod.rs | wc -l | tr -d ' ')"
  native_bindings="$(awk '
    /^pub fn setup_env\(env: &Arc<Env>\) \{/ { in_setup = 1; next }
    in_setup && /^}/ { in_setup = 0 }
    in_setup && /Value::NativeFunction/ { count += 1 }
    END { print count + 0 }
  ' core/src/builtins.rs)"
  runnable_examples="$(awk -F '|' '$1 !~ /^#/ && $2 == "runnable" { count += 1 } END { print count + 0 }' examples/manifest.tsv)"

  printf '# Zio Project Status\n\n'
  printf '> Generated with `tools/project-status.sh`. Verify with `tools/project-status.sh --check`.\n\n'
  printf '| Fact | Value |\n| --- | --- |\n'
  printf '| Workspace crates | 2 |\n'
  printf '| Rust tests | %s |\n' "$tests"
  printf '| Special forms | %s |\n' "$special_forms"
  printf '| Native bindings | %s |\n' "$native_bindings"
  printf '| Runnable examples | %s |\n' "$runnable_examples"
}

case "${1:-}" in
  "")
    emit_status
    ;;
  --check)
    temp_file="$(mktemp)"
    trap 'rm -f "$temp_file"' EXIT
    emit_status > "$temp_file"
    if cmp -s "$temp_file" docs/status.md; then
      exit 0
    fi
    printf 'docs/status.md is stale; run tools/project-status.sh > docs/status.md\n' >&2
    exit 1
    ;;
  *)
    printf 'usage: %s [--check]\n' "$0" >&2
    exit 2
    ;;
esac

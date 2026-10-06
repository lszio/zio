#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

emit_status() {
  local tests special_forms native_bindings runnable_examples crates
  tests="$(cargo test --workspace -- --list 2>/dev/null | awk '/: test$/ { count += 1 } END { print count + 0 }')"
  special_forms="$(rg -n '=> Some\(' langs/core/src/special/mod.rs | wc -l | tr -d ' ')"
  native_bindings="$(rg -n 'Value::NativeFunction\(NativeFn::new' langs/core/src/builtins/ | wc -l | tr -d ' ')"
  runnable_examples="$(awk -F '|' '$1 !~ /^#/ && $2 == "runnable" { count += 1 } END { print count + 0 }' examples/manifest.tsv)"
  # member count follows the workspace manifest, so adding a crate cannot
  # silently leave a stale count behind
  crates="$(python3 -c "
import re, pathlib
text = pathlib.Path('Cargo.toml').read_text()
members = re.search(r'members\s*=\s*\[(.*?)\]', text, re.S).group(1)
print(len([m for m in members.split(',') if m.strip().strip('\"')]))
")"

  printf '# Zio Project Status\n\n'
  printf '> Generated with `tools/project-status.sh`. Verify with `tools/project-status.sh --check`.\n\n'
  printf '| Fact | Value |\n| --- | --- |\n'
  printf '| Workspace crates | %s |\n' "$crates"
  printf '| Rust tests | %s |\n' "$tests"
  printf '| Special forms | %s |\n' "$special_forms"
  printf '| Native bindings | %s |\n' "$native_bindings"
  printf '| Runnable examples | %s |\n' "$runnable_examples"
}

# Every markdown link target in the feature matrix must exist in the repo.
# This keeps the authoritative status table honest: an Evidence file that
# disappears breaks the check instead of leaving a dangling claim.
check_matrix_evidence() {
  local docs_dir status=0 target
  docs_dir="$root/docs"
  while IFS= read -r target; do
    [ -z "$target" ] && continue
    case "$target" in
      http://*|https://*|\#*) continue ;;
    esac
    if [ ! -e "$docs_dir/$target" ]; then
      printf 'feature-matrix evidence missing: docs/%s\n' "$target" >&2
      status=1
    fi
  done < <(rg -o '\]\(([^)]+)\)' -or '$1' "$docs_dir/feature-matrix.md")
  return $status
}


case "${1:-}" in
  "")
    emit_status
    ;;
  --check)
    temp_file="$(mktemp)"
    trap 'rm -f "$temp_file"' EXIT
    emit_status > "$temp_file"
    if ! cmp -s "$temp_file" docs/status.md; then
      printf 'docs/status.md is stale; run tools/project-status.sh > docs/status.md\n' >&2
      exit 1
    fi
    check_matrix_evidence || exit 1
    printf 'status check ok\n'
    ;;
  *)
    printf 'usage: %s [--check]\n' "$0" >&2
    exit 2
    ;;
esac

#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

emit_status() {
  local tests special_forms native_bindings runnable_examples
  tests="$(cargo test --workspace -- --list | awk '/: test$/ { count += 1 } END { print count + 0 }')"
  special_forms="$(rg -n '=> Some\(' core/src/special/mod.rs | wc -l | tr -d ' ')"
  native_bindings="$(rg -n 'Value::NativeFunction\(NativeFn::new' core/src/builtins/ | wc -l | tr -d ' ')"
  runnable_examples="$(awk -F '|' '$1 !~ /^#/ && $2 == "runnable" { count += 1 } END { print count + 0 }' examples/manifest.tsv)"

  printf '# Zio Project Status\n\n'
  printf '> Generated with `tools/project-status.sh`. Verify with `tools/project-status.sh --check`.\n\n'
  printf '| Fact | Value |\n| --- | --- |\n'
  printf '| Workspace crates | 3 |\n'
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

# Assertions that key architecture claims are backed by code — the
# mechanical version of keeping ADR status columns honest.
check_core_invariants() {
  local status=0
  local -a checks=(
    "ADR-011 IoHost injected into EvalContext|core/src/context.rs|dyn crate::io::IoHost"
    "ADR-013 zos::apply protocol exists|core/src/zos/apply.rs|pub fn try_apply"
    "ADR-008 GF dispatch cache exists|core/src/zos/gf.rs|dispatch_cache"
    "load keeps spans via read_program_with_source|core/src/builtins/io.rs|read_program_with_source"
    "module export accumulator in EvalContext|core/src/context.rs|module_exports"
    "eval primitive registered|core/src/builtins/macroexpand.rs|pub fn eval_fn"
    "ADR-016 zio-ai capability-denied prefix|ai/src/lib.rs|capability-denied:"
    "ADR-016 replay miss is fail-fast|ai/src/mock.rs|HostErrorKind::ReplayMiss"
    "ADR-016 external attach registers llm-complete|ai/src/lib.rs|\"llm-complete\""
  )
  local entry name file pattern
  for entry in "${checks[@]}"; do
    IFS='|' read -r name file pattern <<< "$entry"
    if ! rg -q "$pattern" "$root/$file"; then
      printf 'core invariant broken (%s): %s not found in %s\n' "$name" "$pattern" "$file" >&2
      status=1
    fi
  done
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
    check_core_invariants || exit 1
    printf 'status check ok\n'
    ;;
  *)
    printf 'usage: %s [--check]\n' "$0" >&2
    exit 2
    ;;
esac

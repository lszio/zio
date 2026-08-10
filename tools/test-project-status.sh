#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$fixture/bin" "$fixture/core/src/special" "$fixture/docs" "$fixture/examples" "$fixture/tools"
cp "$root/tools/project-status.sh" "$fixture/tools/project-status.sh"

cat > "$fixture/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'fixture_test: test\n'
EOF
chmod +x "$fixture/bin/cargo"

cat > "$fixture/core/src/special/mod.rs" <<'EOF'
"if" => Some(do_if()),
EOF

cat > "$fixture/core/src/builtins.rs" <<'EOF'
fn configure_local(env: &Env) {
    env.set("outside", Value::Integer(1));
    env.set("outside-native", Value::NativeFunction(NativeFn::new("outside-native", native)));
}

pub fn setup_env(env: &Arc<Env>) {
    env.set("answer", Value::Integer(42));
    env.set("native", Value::NativeFunction(NativeFn::new("native", native)));
}

fn configure_test(env: &Env) {
    env.set("also-outside", Value::String("not a registration".into()));
}
EOF

cat > "$fixture/core/src/env.rs" <<'EOF'
fn bind_parameter(env: &Env) {
    env.set("parameter", Value::Integer(1));
}
EOF

cat > "$fixture/examples/manifest.tsv" <<'EOF'
example.zio|runnable|ok
EOF

output="$(PATH="$fixture/bin:$PATH" "$fixture/tools/project-status.sh")"
if ! grep -Fq '| Native bindings | 1 |' <<<"$output"; then
    printf 'expected only the native registration in builtins::setup_env; output was:\n%s\n' "$output" >&2
    exit 1
fi

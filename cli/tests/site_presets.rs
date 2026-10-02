use std::path::PathBuf;
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below workspace root")
        .to_path_buf()
}

fn run_program_source(name: &str, source: &str) -> Output {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, source).expect("write program to tmpdir");
    Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(&path)
        .current_dir(workspace_root())
        .output()
        .expect("run zio-cli on program")
}

/// Mirrors the REPL presets in site/app.js (`presets`). eval_zio in wasm.rs
/// creates a fresh Env per call, and the site replays cumulative programs per
/// displayed line — each program below is one such cumulative program, ending
/// in the line whose result the site shows.
///
/// The `jseval` preset is not covered here: js/eval, js/dom-set-text and
/// js/console-log are registered only in the WASM entrypoint (core/src/wasm.rs),
/// not in the CLI. It is exercised manually via the browser.
fn site_repl_presets() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "site-syntax-rules.zio",
            "(def x 1)\n(def y 2)\n(defmacro swap! (syntax-rules () (((swap! a b) (let [tmp a] (set! a b) (set! b tmp))))))\n(swap! x y)\n(list x y)\n",
            "(2 1)",
        ),
        (
            "site-zos.zio",
            "(defclass point () ((x :initarg :x) (y :initarg :y)))\n(def p (make-instance point :x 10 :y 20))\n(slot-value p :x)\n",
            "10",
        ),
        (
            "site-csp.zio",
            "(def c (chan 5))\n(send! c \"hello agent\")\n(recv! c)\n",
            "hello agent",
        ),
        (
            "site-json.zio",
            "(json-stringify {:agent \"ZioBot\" :status :active})\n(json-parse \"{\\\"val\\\": 42}\")\n",
            "42",
        ),
    ]
}

#[test]
fn site_repl_presets_exit_successfully_and_print_expected_result() {
    for (name, source, marker) in site_repl_presets() {
        let output = run_program_source(name, source);
        assert!(
            output.status.success(),
            "{name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("Error:"),
            "{name} reported an error: {stdout:?}"
        );
        assert!(
            stdout.contains(marker),
            "{name} did not print marker {marker:?}; stdout was {stdout:?}"
        );
    }
}

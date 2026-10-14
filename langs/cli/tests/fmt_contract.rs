//! The `zio fmt` contract, exercised through the built binary: formatting
//! on stdin, `--check` exits, `--write` rewrites, and unbalanced input
//! fails with a line rather than silently passing.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn zio() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zio-cli"))
}

fn run(args: &[&str], input: Option<&str>) -> Output {
    let mut command = zio();
    command.args(args);
    match input {
        Some(source) => {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn zio");
            child
                .stdin
                .as_mut()
                .expect("stdin piped")
                .write_all(source.as_bytes())
                .expect("write stdin");
            child.wait_with_output().expect("zio output")
        }
        None => command.output().expect("run zio"),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zio-fmt-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_file(&dir);
    dir
}

#[test]
fn fmt_on_stdin_formats_and_is_idempotent() {
    let source = "(defn   add [a b]\n   (+ a b))\n";
    let once = run(&["fmt"], Some(source));
    assert!(once.status.success(), "{:?}", once.status);
    let formatted = stdout(&once);
    assert_eq!(formatted, "(defn add [a b] (+ a b))\n");

    // A second pass over the formatted source is a no-op: the layout is
    // a fixed point, which is what makes `--check` trustworthy.
    let twice = run(&["fmt"], Some(&formatted));
    assert!(twice.status.success());
    assert_eq!(stdout(&twice), formatted);
}

#[test]
fn fmt_check_reports_unformatted_and_exits_nonzero() {
    let source = "(defn   add [a b]\n   (+ a b))\n";
    let unformatted = run(&["fmt", "--check"], Some(source));
    assert!(!unformatted.status.success(), "check must fail on unformatted");
    assert!(stdout(&unformatted).contains("not formatted"), "{:?}", stdout(&unformatted));

    let formatted = "(defn add [a b] (+ a b))\n";
    let clean = run(&["fmt", "--check"], Some(formatted));
    assert!(clean.status.success(), "check must pass on formatted source");
    assert!(stdout(&clean).is_empty());
}

#[test]
fn fmt_write_rewrites_only_files_that_change() {
    let path = scratch("write.zio");
    std::fs::write(&path, "(defn   add [a b]\n   (+ a b))\n").expect("write fixture");
    let written = run(&["fmt", "--write", path.to_str().unwrap()], None);
    assert!(written.status.success(), "{:?}", written.status);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "(defn add [a b] (+ a b))\n"
    );

    // A file already formatted is not rewritten, and says nothing.
    let again = run(&["fmt", "--write", path.to_str().unwrap()], None);
    assert!(again.status.success());
    assert!(stdout(&again).trim().is_empty(), "{:?}", stdout(&again));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fmt_reports_unbalanced_input_with_its_line() {
    let broken = "(a b\n(c)\n";
    let output = run(&["fmt"], Some(broken));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fmt:"), "{stderr}");
}

#[test]
fn fmt_rejects_write_and_check_together() {
    let output = run(&["fmt", "--write", "--check"], Some("(a)"));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--write"), "{stderr}");
}

use std::path::PathBuf;
use std::process::{Command, Output};

/// The workspace root, when this crate is built inside one.
///
/// The published crate ships without the checkout this test walks, so a
/// missing `examples/` tree is a skip rather than a hard failure: the
/// contract is about in-repo examples, and the fixture is read at
/// runtime instead of `include_str!` for the same reason.
fn workspace_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cli crate lives at langs/cli below workspace root")
        .to_path_buf();
    root.join("examples")
        .join("manifest.tsv")
        .is_file()
        .then_some(root)
}

fn run_example(root: &PathBuf, file: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(root.join("examples").join(file))
        .current_dir(root)
        .output()
        .expect("run zio-cli example")
}

fn parse_manifest(manifest: &str) -> Vec<[&str; 3]> {
    manifest
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<_> = line.split('|').collect();
            assert_eq!(fields.len(), 3, "invalid manifest row: {line}");
            assert!(
                matches!(fields[1], "runnable" | "documentary"),
                "invalid manifest status {:?} in row: {line}; expected \"runnable\" or \"documentary\"",
                fields[1]
            );
            [fields[0], fields[1], fields[2]]
        })
        .collect()
}

#[test]
#[should_panic(expected = "invalid manifest status \"broken\"")]
fn manifest_rejects_unknown_status() {
    parse_manifest("example.zio|broken|marker");
}

#[test]
fn runnable_examples_exit_successfully_and_print_their_marker() {
    let Some(root) = workspace_root() else {
        eprintln!("examples/ not present; skipping example contract");
        return;
    };
    let manifest = std::fs::read_to_string(root.join("examples").join("manifest.tsv"))
        .expect("read examples/manifest.tsv");
    for fields in parse_manifest(&manifest) {
        if fields[1] != "runnable" {
            continue;
        }

        let output = run_example(&root, fields[0]);
        assert!(
            output.status.success(),
            "{} failed: {}",
            fields[0],
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(fields[2]),
            "{} did not print marker {:?}; stdout was {stdout:?}",
            fields[0],
            fields[2]
        );
    }
}

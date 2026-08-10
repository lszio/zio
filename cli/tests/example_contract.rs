use std::path::PathBuf;
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below workspace root")
        .to_path_buf()
}

fn run_example(file: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(workspace_root().join("examples").join(file))
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
    for fields in parse_manifest(include_str!("../../examples/manifest.tsv")) {
        if fields[1] != "runnable" {
            continue;
        }

        let output = run_example(fields[0]);
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

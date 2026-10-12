//! Bundle contracts: the built application runs away from any checkout
//! with a stripped environment, an image entry keeps its restored state,
//! and a damaged payload is refused with a named error instead of
//! running half a program.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cli crate lives at langs/cli below workspace root")
        .to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zio-bundle-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Run an app with no zio/rustc on PATH and no ambient variables.
fn run_isolated(app: &Path, cwd: &Path) -> std::process::Output {
    Command::new(app)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("app runs")
}

#[test]
fn bundled_source_app_runs_without_a_checkout() {
    let work = scratch("source");
    let app = work.join("app");
    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .current_dir(workspace_root())
        .args([
            "build",
            "examples/toolchain/app.zio",
            "--target",
            "native",
            "-o",
        ])
        .arg(&app)
        .output()
        .expect("build runs");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = run_isolated(&app, &work);
    assert!(
        run.status.success(),
        "bundled app failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("49\n"), "static output missing: {stdout}");
    assert!(
        stdout.contains("50"),
        "dynamic eval output missing: {stdout}"
    );
}

#[test]
fn bundled_image_app_keeps_restored_state() {
    let work = scratch("image");
    let image = work.join("session.zio-image");
    let app = work.join("img-app");
    let save = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .current_dir(workspace_root())
        .args([
            "image",
            "save",
            "examples/toolchain/image-session.zio",
            "-o",
        ])
        .arg(&image)
        .output()
        .expect("save runs");
    assert!(
        save.status.success(),
        "image save failed: {}",
        String::from_utf8_lossy(&save.stderr)
    );

    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .args(["build"])
        .arg(&image)
        .args(["--target", "native", "-o"])
        .arg(&app)
        .args(["--entry", "((get root-0 :bump))"])
        .output()
        .expect("build runs");
    assert!(
        build.status.success(),
        "image bundle build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = run_isolated(&app, &work);
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1");
}

#[test]
fn damaged_payload_is_refused() {
    let work = scratch("corrupt");
    let app = work.join("app");
    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .current_dir(workspace_root())
        .args([
            "build",
            "examples/toolchain/app.zio",
            "--target",
            "native",
            "-o",
        ])
        .arg(&app)
        .output()
        .expect("build runs");
    assert!(build.status.success());

    // Flip one byte inside the payload region (footer is the last 36
    // bytes): the checksum must catch it before anything runs.
    let mut bytes = std::fs::read(&app).expect("app bytes");
    let len = bytes.len();
    let target = len - 36 - 8;
    bytes[target] ^= 0xff;
    std::fs::write(&app, &bytes).expect("write corrupted app");

    let run = run_isolated(&app, &work);
    assert!(!run.status.success(), "corrupt payload ran");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("checksum mismatch"),
        "corruption was not named: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&run.stdout).contains("49"),
        "corrupt payload produced output"
    );
}

#[test]
fn browser_bundle_runs_through_the_wasm_engine() {
    let work = scratch("browser");
    let html = work.join("app.html");
    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .current_dir(workspace_root())
        .args([
            "build",
            "examples/toolchain/app.zio",
            "--target",
            "browser",
            "-o",
        ])
        .arg(&html)
        .output()
        .expect("build runs");
    assert!(
        build.status.success(),
        "browser build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let text = std::fs::read_to_string(&html).expect("html bytes");
    assert!(text.contains("PAYLOAD_B64"), "payload is embedded");
    assert!(text.contains("run_bundle"), "the wasm entry is the runner");

    // The browser automation host is not always reachable from a test
    // machine, so the contract runs the real engine the page loads:
    // the web-target glue plus the module bytes, then the same session
    // call the page makes.
    let wasm_dir = workspace_root().join("apps/site/public/wasm");
    let harness = Command::new("node")
        .current_dir(workspace_root())
        .arg("tools/a01-browser-harness.mjs")
        .arg(wasm_dir.join("zio_core.js"))
        .arg(wasm_dir.join("zio_core_bg.wasm"))
        .arg(&html)
        .output()
        .expect("node harness runs");
    let stdout = String::from_utf8_lossy(&harness.stdout);
    assert!(
        harness.status.success(),
        "browser bundle failed in the wasm engine:\n{stdout}\n{}",
        String::from_utf8_lossy(&harness.stderr)
    );
    assert!(
        stdout.contains("PASS browser bundle"),
        "unexpected harness output: {stdout}"
    );
}

/// WASI contracts need both the wasm32-wasip1 target and a real
/// runner; they skip with a reason when either is missing rather than
/// claiming unrun behavior.
fn wasi_ready() -> bool {
    let wasmtime = Command::new("wasmtime").arg("--version").output();
    if wasmtime.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping WASI contracts: no wasmtime on PATH");
        return false;
    }
    true
}

fn build_wasi(source: &Path, app: &Path, entry: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zio-cli"));
    command.current_dir(workspace_root()).args(["build"]);
    command.arg(source).args(["--target", "wasm-wasi", "-o"]);
    command.arg(app);
    if let Some(entry) = entry {
        command.args(["--entry", entry]);
    }
    command.output().expect("build runs")
}

#[test]
fn wasi_app_runs_on_a_real_runtime() {
    if !wasi_ready() {
        return;
    }
    let work = scratch("wasi-source");
    let app = work.join("app.wasm");
    let build = build_wasi(
        &workspace_root().join("examples/toolchain/app.zio"),
        &app,
        None,
    );
    assert!(
        build.status.success(),
        "wasi build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new("wasmtime")
        .current_dir(&work)
        .arg("run")
        .arg(&app)
        .output()
        .expect("wasmtime runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "wasmtime failed: {stdout}");
    assert!(stdout.contains("49\n"), "static output missing: {stdout}");
    assert!(stdout.contains("50"), "dynamic eval missing: {stdout}");
}

#[test]
fn wasi_image_entry_keeps_restored_state() {
    if !wasi_ready() {
        return;
    }
    let work = scratch("wasi-image");
    let image = work.join("session.zio-image");
    let save = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .current_dir(workspace_root())
        .args([
            "image",
            "save",
            "examples/toolchain/image-session.zio",
            "-o",
        ])
        .arg(&image)
        .output()
        .expect("save runs");
    assert!(save.status.success());
    let app = work.join("img.wasm");
    let build = build_wasi(&image, &app, Some("((get root-0 :bump))"));
    assert!(
        build.status.success(),
        "wasi image build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new("wasmtime")
        .current_dir(&work)
        .arg("run")
        .arg(&app)
        .output()
        .expect("wasmtime runs");
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1");
}

#[test]
fn wasi_file_access_needs_preopen_and_grant() {
    if !wasi_ready() {
        return;
    }
    let work = scratch("wasi-grant");
    let granted = work.join("granted");
    std::fs::create_dir_all(&granted).expect("granted dir");
    std::fs::write(granted.join("resource.txt"), "granted payload").expect("resource");
    let source = work.join("reads-file.zio");
    let resource = granted.join("resource.txt");
    std::fs::write(
        &source,
        format!("(println (slurp {:?}))\n", resource.display().to_string()),
    )
    .expect("source");
    let app = work.join("file.wasm");
    let build = build_wasi(&source, &app, None);
    assert!(
        build.status.success(),
        "wasi build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let bare = Command::new("wasmtime")
        .current_dir(&work)
        .args(["run", "--dir"])
        .arg(&granted)
        .arg(&app)
        .output()
        .expect("wasmtime runs");
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(
        stderr.contains("outside every granted root"),
        "a preopen without a grant was not refused: {stderr}"
    );

    let granted_run = Command::new("wasmtime")
        .current_dir(&work)
        .args(["run", "--dir"])
        .arg(&granted)
        .arg(&app)
        .arg("--grant")
        .arg(&granted)
        .output()
        .expect("wasmtime runs");
    assert!(granted_run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&granted_run.stdout).trim(),
        "granted payload"
    );
}

#[test]
fn browser_only_dependencies_cannot_become_wasi_apps() {
    let work = scratch("wasi-refuse");
    let source = work.join("js-app.zio");
    std::fs::write(&source, "(println (js/eval \"1+1\"))\n").expect("write source");
    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .args(["build"])
        .arg(&source)
        .args(["--target", "wasm-wasi", "-o"])
        .arg(work.join("app.wasm"))
        .output()
        .expect("build runs");
    assert!(!build.status.success(), "a browser-only app was accepted");
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("browser-only"),
        "the refusal did not name the dependency: {stderr}"
    );
    assert!(
        !work.join("app.wasm").exists(),
        "a refused build left an app"
    );
}

#[test]
fn undeclared_module_refuses_the_build() {
    let work = scratch("closure");
    let source = work.join("needs.zio");
    std::fs::write(&source, "(require :absent :refer [thing])\n").expect("write source");
    let build = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .args(["build"])
        .arg(&source)
        .args(["--target", "native", "-o"])
        .arg(work.join("app"))
        .output()
        .expect("build runs");
    assert!(!build.status.success(), "build accepted a missing module");
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("does not declare"),
        "missing dependency was not named: {stderr}"
    );
    assert!(!work.join("app").exists(), "a refused build left an app");
}

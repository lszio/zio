use std::path::PathBuf;
use std::process::Command;

/// End-to-end contract for file-based modules: a module file declares
/// exports with the `(export ...)` form; `require` enforces encapsulation
/// and `ns/name` qualified access resolves exported symbols only.

struct ModuleFixture {
    dir: PathBuf,
}

impl ModuleFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("zio_modules_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create module fixture dir");
        std::fs::write(
            dir.join("mymath.zio"),
            "(defn twice [x] (* 2 x))\n\
             (defn hidden [x] x)\n\
             (export twice)\n",
        )
        .expect("write module file");
        ModuleFixture { dir }
    }

    fn run(&self, script: &str) -> (bool, String, String) {
        let script_path = self.dir.join("script.zio");
        std::fs::write(&script_path, script).expect("write script");
        let output = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
            .arg(&script_path)
            .env("ZIO_PATH", &self.dir)
            .output()
            .expect("run zio-cli");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

impl Drop for ModuleFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn file_modules_support_export_refer_and_qualified_access() {
    let fx = ModuleFixture::new();

    // Qualified access to an exported symbol
    let (ok, out, err) = fx.run("(require :mymath)\n(println \"res:\" (mymath/twice 21))\n");
    assert!(ok, "qualified access failed: {out}{err}");
    assert!(out.contains("42"), "unexpected output: {out}");

    // :refer imports exported names unqualified
    let (ok, out, err) = fx.run("(require :mymath :refer [twice])\n(println \"res:\" (twice 5))\n");
    assert!(ok, "refer failed: {out}{err}");
    assert!(out.contains("10"), "unexpected output: {out}");

    // Encapsulation: refer of a non-exported symbol is an error
    let (ok, _out, err) = fx.run("(require :mymath :refer [hidden])\n");
    assert!(!ok, "refer of non-exported symbol must fail");
    assert!(err.contains("does not export"), "unexpected error: {err}");

    // Encapsulation: qualified access to a non-exported symbol fails too
    let (ok, _out, err) = fx.run("(require :mymath)\n(println (mymath/hidden 1))\n");
    assert!(!ok, "qualified access to non-exported symbol must fail");
    assert!(err.contains("symbol not found"), "unexpected error: {err}");
}

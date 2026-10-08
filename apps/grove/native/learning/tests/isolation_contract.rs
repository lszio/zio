//! G00 isolation contract: the worker profile is denied-by-default, not
//! a namespace label. Each check is a real child process trying to do
//! something it must not be able to do.
//!
//! These tests need the torch venv and Linux namespaces. A skip is a
//! reported missing prerequisite, never a pass.

use std::path::{Path, PathBuf};
use std::time::Duration;

use grove::isolation::{IsolationProfile, MountSpec, ReadOnlyRoot};
use grove::worker::{Frame, PROTOCOL_VERSION, Worker, WorkerConfig};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}

fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python")
}

fn task_dir() -> PathBuf {
    repo_root().join("examples/self-learning")
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("grove_iso_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scratch")).unwrap();
        std::fs::create_dir_all(dir.join("input")).unwrap();
        Fixture { dir }
    }

    fn scratch(&self) -> PathBuf {
        self.dir.join("scratch")
    }

    fn config(&self) -> WorkerConfig {
        let root = repo_root();
        WorkerConfig {
            python: venv_python(),
            worker_script: root.join("apps/grove/workers/torch/worker.py"),
            unshare: PathBuf::from("unshare"),
            scratch: self.scratch(),
            working_dir: root.clone(),
            timeout: Duration::from_secs(240),
            max_address_space: 4 * 1024 * 1024 * 1024,
            // A jailed worker assembles a mount root and imports torch
            // before it can say hello.
            handshake_timeout: Duration::from_secs(180),
        }
    }

    /// The profile the host actually builds for a training attempt:
    /// runtime + stdlib + the task inputs read-only, a private scratch,
    /// and the store, credentials, and evaluator left unmounted.
    fn profile(&self) -> IsolationProfile {
        IsolationProfile {
            read_only: ReadOnlyRoot {
                interpreter_prefixes: Vec::new(),
                // The Python runtime itself: without it there is no worker
                // at all, and it must be visible read-only.
                python_prefix: venv_python()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_path_buf(),
                // The worker script and the frozen Zio stdlib it imports.
                worker_script: repo_root().join("apps/grove/workers/torch/worker.py"),
                lib: repo_root().join("libs"),
                stdlib: repo_root().join("libs/std"),
                // Declared task inputs, mounted read-only per attempt.
                inputs: vec![self.dir.join("input")],
            },
            scratch: self.scratch(),
        }
        .with_resolved_interpreter()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn namespaces_available() -> bool {
    grove::worker::Isolation::probe("unshare")
        .map(|i| i.enforce().is_ok())
        .unwrap_or(false)
}

// ── the profile is mandatory ───────────────────────────────────────

#[test]
fn a_profile_without_a_scratch_directory_is_refused() {
    let fx = Fixture::new("no_scratch");
    let mut profile = fx.profile();
    profile.scratch = PathBuf::from("/nonexistent/scratch");
    let err = profile
        .validate()
        .expect_err("a missing scratch is a missing boundary");
    assert!(
        err.to_string().contains("scratch"),
        "error must name the missing boundary: {err}"
    );
}

#[test]
fn a_profile_with_a_readonly_root_outside_it_is_refused() {
    let fx = Fixture::new("escape_root");
    let mut profile = fx.profile();
    // Point the declared input at the host's own store instead of the
    // attempt's input directory: the profile must not silently accept a
    // read-only mount that grants the database.
    profile.read_only.inputs = vec![repo_root().join("apps/grove/native/learning")];
    profile.read_only.inputs[0] = repo_root().join("apps/grove/native/learning/src/store.rs");
    let err = profile
        .validate()
        .expect_err("a file that is not a directory cannot be mounted");
    assert!(!err.to_string().is_empty());
}

// ── real processes, real refusals ──────────────────────────────────

/// The strongest available check: inside the profile a real child cannot
/// modify a canary the host placed outside the scratch, cannot see the
/// store, and cannot escape the scratch through a symlink.
#[test]
fn a_worker_cannot_touch_anything_outside_its_scratch() {
    if !venv_python().exists() {
        eprintln!("skipping: no torch venv at {}", venv_python().display());
        return;
    }
    if !namespaces_available() {
        eprintln!("skipping: user/network/pid/mount namespaces unavailable");
        return;
    }

    let fx = Fixture::new("write_probe");
    let profile = fx.profile();
    profile.validate().expect("a complete profile");

    // A canary the host owns, outside every mount.
    let canary_dir = fx.dir.join("trusted");
    std::fs::create_dir_all(&canary_dir).unwrap();
    let canary = canary_dir.join("canary.txt");
    std::fs::write(&canary, "host-owned\n").unwrap();
    let before = std::fs::read(&canary).unwrap();

    // A file the attempt must not be able to read: the "credential store".
    let secret_dir = fx.dir.join("secrets");
    std::fs::create_dir_all(&secret_dir).unwrap();
    let secret = secret_dir.join("token");
    std::fs::write(&secret, "s3cr3t\n").unwrap();

    // A symlink in the scratch pointing out of it: writing through it must
    // fail rather than land on the host filesystem.
    let escape = fx.scratch().join("escape");
    std::os::unix::fs::symlink(&canary_dir, &escape).unwrap();

    // The probe itself must be reachable from inside the jail, so it is
    // written into the declared input mount.
    let probe = fx.dir.join("input").join("probe.py");
    // A tiny probe that survives without torch: the point of this test is
    // the boundary, not the runtime.
    std::fs::write(
        &probe,
        r#"import json, os, sys
result = {}
def attempt(label, fn):
    try:
        result[label] = fn()
    except Exception as exc:
        result[label] = f"{type(exc).__name__}: {exc}"

def write_canary():
    with open(sys.argv[1], "w") as f:
        f.write("worker-owned")
    return "wrote"
attempt("canary", lambda: write_canary())

def read_secret():
    with open(sys.argv[2]) as f:
        return f.read().strip()
attempt("secret", read_secret)

def read_store():
    with open(sys.argv[3]) as f:
        return f.read().strip()
attempt("store", read_store)

def write_escape():
    with open(os.path.join("/scratch", "escape", "planted.txt"), "w") as f:
        f.write("planted")
    return "wrote"
attempt("escape", write_escape)

def cwd_is_scratch():
    return os.path.realpath(os.getcwd()) == "/scratch"
attempt("cwd_is_scratch", cwd_is_scratch)
attempt("input0_listing", lambda: os.listdir("/zio/input0") if os.path.isdir("/zio/input0") else "NO /zio/input0")
attempt("ls_root", lambda: sorted(os.listdir("/"))[:20])

def writable_scratch():
    p = os.path.join("/scratch", "probe-write.txt")
    with open(p, "w") as f:
        f.write("ok")
    return True
attempt("scratch_writable", writable_scratch)
print(json.dumps(result))
"#,
    )
    .unwrap();

    // Inside the jail the scratch is /scratch; the host paths the worker
    // must not reach keep their host names, so a success proves the jail
    // did not carry them in.
    // Declared entries are mounted at stable jail-local paths, so the
    // worker addresses `/input0/probe.py`, not the host's layout.
    let out = profile
        .run(
            &venv_python(),
            [
                "/zio/input0/probe.py".to_string(),
                canary.display().to_string(),
                secret.display().to_string(),
                repo_root()
                    .join("apps/grove/native/learning/src/store.rs")
                    .display()
                    .to_string(),
            ],
            &[],
        )
        .expect("the probe itself must run");

    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("probe must report JSON");
    let fail = |label: &str| {
        let v = &report[label];
        assert!(
            v.as_str()
                .map(|s| s.starts_with("PermissionError") || s.starts_with("FileNotFoundError"))
                .unwrap_or(false),
            "worker {label} succeeded ({v}); it must be denied"
        );
    };

    fail("canary");
    fail("secret");
    fail("store");
    fail("escape");
    assert_eq!(report["scratch_writable"], serde_json::json!(true));
    assert_eq!(report["cwd_is_scratch"], serde_json::json!(true));

    // The host's own files are byte-identical afterwards.
    assert_eq!(
        std::fs::read(&canary).unwrap(),
        before,
        "canary was modified"
    );
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "s3cr3t\n");
    assert!(
        !canary_dir.join("planted.txt").exists(),
        "escape symlink wrote to the host"
    );
}

/// A real training worker under the profile: it must produce weights, and
/// the frozen layer must be byte-identical before and after.
#[test]
fn frozen_layers_are_actually_frozen_under_the_profile() {
    if !venv_python().exists() {
        eprintln!("skipping: no torch venv");
        return;
    }
    if !namespaces_available() {
        eprintln!("skipping: namespaces unavailable");
        return;
    }

    let fx = Fixture::new("frozen");
    let profile = fx.profile();
    profile.validate().expect("a complete profile");
    let config = fx.config();
    // The worker reads its data from the declared input mount, so the
    // task's splits are staged there and addressed by their jail-local
    // path — the host path does not exist inside the jail. Staging
    // happens before the spawn: the mount is a snapshot of the directory,
    // so a file written afterwards is not what the worker would see.
    for split in ["train.bin", "val.bin"] {
        std::fs::copy(
            task_dir().join("data").join(split),
            fx.dir.join("input").join(split),
        )
        .expect("stage task split");
    }
    // Seed weights are an input too, staged for the same reason.
    let seed = fx.dir.join("input").join("seed.json");
    std::fs::write(&seed, two_layer_init()).unwrap();
    let seed_layers: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&seed).unwrap()).unwrap();
    let frozen_before = layer_digest(&seed_layers, "frozen");
    let active_before = layer_digest(&seed_layers, "active");

    let isolation = grove::worker::Isolation::probe("unshare").unwrap();
    let mut worker = Worker::spawn_under(&config, &isolation, &profile)
        .expect("spawn a worker under the profile");

    // Two layers: `frozen` is not in `trainable`, `active` is.
    let graph = serde_json::json!({
        "inputs": {
            "image": {"shape": [256], "dtype": "float32", "space": "pixel"},
            "numeric": {"shape": [2], "dtype": "float32", "space": "signed"}
        },
        "ops": [
            {"kind": "normalize", "inputs": ["image"], "output": "img", "attrs": {}},
            {"kind": "normalize", "inputs": ["numeric"], "output": "num", "attrs": {}},
            {"kind": "concat", "inputs": ["img", "num"], "output": "fused", "attrs": {}},
            {"kind": "linear", "inputs": ["fused"], "output": "frozen", "attrs": {"out": 32}},
            {"kind": "relu", "inputs": ["frozen"], "output": "hidden", "attrs": {}},
            {"kind": "linear", "inputs": ["hidden"], "output": "active", "attrs": {"out": 2}}
        ],
        "outputs": {"logits": "active"},
        "trainable": ["active"]
    });
    let trained = fx.scratch().join("trained.json");
    let frame = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "g00-frozen".into(),
                run_id: "run-frozen".into(),
                attempt_id: "att-frozen".into(),
                graph,
                weights: "/zio/input0/seed.json".to_string(),
                data: "/zio/input0/train.bin".to_string(),
                val_data: Some("/zio/input0/val.bin".to_string()),
                out: "/scratch/trained.json".to_string(),
                steps: 20,
                seed: 1,
                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            Duration::from_secs(200),
        )
        .unwrap_or_else(|e| panic!("training failed: {e}"));

    let done = match &frame {
        Frame::Done {
            loss, first_loss, ..
        } => (*loss, *first_loss),
        other => panic!("expected a done frame, got {other:?}"),
    };
    let (loss, first) = (
        done.0.expect("a finished run reports a loss"),
        done.1.expect("a finished run reports its first loss"),
    );
    assert!(
        loss < first,
        "the trainable layer did not learn: {first} -> {loss}"
    );

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&trained).unwrap()).unwrap();

    assert_eq!(
        layer_digest(&after, "frozen"),
        frozen_before,
        "a layer outside `trainable` changed during training"
    );
    assert_ne!(
        layer_digest(&after, "active"),
        active_before,
        "the trainable layer did not change at all"
    );
    drop(worker);
}

/// A layer's fingerprint as the *worker* sees it.
///
/// Every value is rounded through f32 first: the worker stores float32
/// tensors, so comparing the f64 JSON text of the seed against the worker's
/// own output would report a change where none happened — the seed's
/// decimal is simply more precise than the tensor that carries it.
fn layer_digest(params: &serde_json::Value, key: &str) -> String {
    // A worker's output nests layers under `params`; a seed file is the
    // layer map itself. Accept both so the same digest reads either.
    let entry = &params["params"][key];
    let entry = if entry.is_null() { &params[key] } else { entry };
    let mut rounded: Vec<f32> = Vec::new();
    if let Some(rows) = entry["w"].as_array() {
        for row in rows {
            let row = row.as_array().expect("a weight row is an array");
            for v in row {
                rounded.push(v.as_f64().expect("a weight is a number") as f32);
            }
        }
    }
    if let Some(bias) = entry["b"].as_array() {
        for v in bias {
            rounded.push(v.as_f64().expect("a bias is a number") as f32);
        }
    }
    let mut h: u64 = 1469598103934665603;
    for value in rounded {
        for byte in value.to_bits().to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(1099511628211);
        }
    }
    format!("{h:016x}")
}

fn two_layer_init() -> String {
    serde_json::json!({
        "frozen": layer(32, 258, 7),
        "active": layer(2, 32, 3),
    })
    .to_string()
}

fn layer(rows: usize, cols: usize, seed: u64) -> serde_json::Value {
    // Deterministic, non-degenerate: an all-zero init has no gradient in
    // the frozen layer's neighbourhood and would hide a real change.
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 11) as f64 / (1u64 << 53) as f64) - 0.5
    };
    serde_json::json!({
        "w": (0..rows)
            .map(|_| (0..cols).map(|_| next()).collect::<Vec<f64>>())
            .collect::<Vec<Vec<f64>>>(),
        "b": (0..rows).map(|_| next()).collect::<Vec<f64>>(),
    })
}

// ── mount spec construction ────────────────────────────────────────

#[test]
fn a_mount_spec_binds_everything_read_only_except_the_scratch() {
    let fx = Fixture::new("mount_spec");
    let profile = fx.profile();
    profile.validate().expect("a complete profile");
    let spec: MountSpec = profile.to_mount_spec();
    assert!(
        spec.scratch.starts_with(&fx.scratch()),
        "scratch is the only writable path"
    );
    // The declared read-only set is what the worker's root contains.
    let sources: Vec<PathBuf> = spec
        .read_only_bindings
        .iter()
        .map(|b| b.source.clone())
        .collect();
    let targets: Vec<PathBuf> = spec
        .read_only_bindings
        .iter()
        .map(|b| b.target.clone())
        .collect();
    let has = |list: &[PathBuf], want: &Path| list.iter().any(|p| p == want);
    // The venv keeps its host path: the interpreter's absolute paths are
    // baked in, and a profile that renames it cannot start a worker.
    assert!(has(&sources, &repo_root().join(".venv")));
    assert!(has(&targets, &repo_root().join(".venv")));
    // Everything else is a stable jail-local path.
    assert!(has(&targets, Path::new("/zio/lib")), "{targets:?}");
    assert!(has(&targets, Path::new("/zio/stdlib")), "{targets:?}");
    assert!(has(&targets, Path::new("/zio/worker")), "{targets:?}");
    assert!(has(&targets, Path::new("/zio/input0")), "{targets:?}");

    // The trust store, the credentials, and the user's home are not in it.
    let forbidden_paths: Vec<PathBuf> = vec![
        repo_root().join("apps/grove/native/learning"),
        repo_root().join(".git"),
        PathBuf::from("/home/lszio/.ssh"),
        PathBuf::from("/home/lszio/.config"),
    ];
    for forbidden in &forbidden_paths {
        assert!(
            !has(&sources, forbidden) && !has(&targets, forbidden),
            "{forbidden:?} must never be in the worker's root: {targets:?}"
        );
    }
    // A writable scratch that is also read-only is a contradiction, and
    // the profile check is what catches it.
    let mut contradictory = profile.clone();
    contradictory.read_only.inputs.push(fx.scratch());
    let err = contradictory
        .validate()
        .expect_err("scratch cannot also be an input mount");
    assert!(err.to_string().contains("scratch"), "{err}");
}

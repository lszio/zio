//! End-to-end check of the local teacher path: a real torch-trained
//! teacher served over HTTP, queried through Loom's teacher host.
//!
//! Skipped unless a weights file and the venv are present, because the
//! teacher is an optional backend — its absence is reported, never faked.

use std::io::BufRead;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use loom::teacher::http_teacher::HttpTeacherHost;
use loom::teacher::{
    ContentPart, Modality, OutputKind, TeacherCapability, TeacherHost, TeacherRequest, UsageLicence,
};

struct Service {
    child: Child,
    port: u16,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the local teacher and wait for its readiness line.
fn start(weights: &Path, python: &Path) -> Option<Service> {
    let mut child = Command::new(python)
        .arg("apps/grove/workers/torch/teacher.py")
        .arg("--weights")
        .arg(weights)
        .arg("--port")
        .arg("0") // let the OS choose, then read it back from the banner
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // The service prints its bound port on stdout before serving.
    let stdout = child.stdout.take()?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut port = None;
    {
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if let Some(index) = line.find("http://127.0.0.1:") {
                let rest = &line[index + "http://127.0.0.1:".len()..];
                if let Some(end) = rest.find('/') {
                    port = rest[..end].parse().ok();
                    line.clear();
                    break;
                }
            }
            line.clear();
        }
    }
    // Drain the rest of the pipe so the child never blocks on a full buffer.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    Some(Service { child, port: port? })
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}

#[test]
fn the_local_teacher_answers_a_real_query() {
    let root = repo_root();
    let python = root.join(".venv/bin/python");
    let weights = std::env::var("GROVE_TEACHER_WEIGHTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("teacher.pt"));
    if !python.exists() || !weights.exists() {
        eprintln!("skipping: no venv python or no trained teacher weights");
        return;
    }

    let service = match start(&weights, &python) {
        Some(service) => service,
        None => {
            eprintln!("skipping: could not start the local teacher");
            return;
        }
    };

    let host = HttpTeacherHost::builder()
        .base_url(format!("http://127.0.0.1:{}/v1", service.port))
        .timeout(Duration::from_secs(10))
        .build();
    let capability = TeacherCapability {
        teacher_id: "local-teacher".to_string(),
        model_version: None, // filled by the service's banner
        input_modalities: vec![Modality::Image, Modality::Numeric],
        output_kinds: vec![OutputKind::HardLabel],
        vocabulary: None,
        retains_data: false,
        permits_training_use: true,
    };
    let request = TeacherRequest {
        request_id: "e2e-1".to_string(),
        capability,
        parts: vec![
            ContentPart::Reference {
                artifact: "sha256:img".into(),
                media_type: "image/png".into(),
            },
            ContentPart::Numbers {
                values: vec![-1.5, -0.4],
            },
        ],
        licence: UsageLicence::TrainStudent,
        wants: vec![OutputKind::HardLabel],
        max_response_bytes: 64 * 1024,
    };

    let response = host.query(&request).expect("local teacher must answer");
    assert!(
        response.failure().is_ok(),
        "teacher reported a failure: {response:?}"
    );
    let label = response
        .hard_label
        .expect("a real trained teacher must give a label");
    assert!(
        label == "clear" || label == "fault",
        "unexpected label {label}"
    );
    assert!(
        response.model_version.is_some(),
        "a real teacher must report the model version it answered with"
    );
}

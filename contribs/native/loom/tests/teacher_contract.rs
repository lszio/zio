//! W03 contract: the teacher capability surface.
//!
//! The tests split into two groups. The protocol-level ones need no
//! network and pin the rules that keep a teacher honest: undeclared
//! modalities and outputs are refused, soft outputs need an aligned
//! vocabulary, a failed exchange stays visible, and a proposed program is
//! a proposal rather than an approved candidate. The HTTP ones talk to a
//! real local socket, so the transport limits are proven rather than
//! assumed.

#![cfg(feature = "http")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use loom::teacher::http_teacher::HttpTeacherHost;
use loom::teacher::{
    validate_request, ContentPart, Modality, OutputKind, ProposedProgram, RecordingTeacher,
    TeacherCapability, TeacherHost, TeacherRequest, TeacherResponse, UsageLicence,
};
use loom::HostErrorKind;

fn hard_teacher() -> TeacherCapability {
    TeacherCapability {
        teacher_id: "local-teacher".to_string(),
        model_version: Some("local-net-1".to_string()),
        input_modalities: vec![Modality::Image, Modality::Numeric],
        output_kinds: vec![OutputKind::HardLabel, OutputKind::Program],
        vocabulary: None,
        retains_data: false,
        permits_training_use: true,
    }
}

fn soft_teacher() -> TeacherCapability {
    TeacherCapability {
        teacher_id: "local-teacher".to_string(),
        model_version: Some("local-net-1".to_string()),
        input_modalities: vec![Modality::Image, Modality::Numeric],
        output_kinds: vec![OutputKind::HardLabel, OutputKind::SoftDistribution, OutputKind::Program],
        vocabulary: Some(vec!["clear".to_string(), "fault".to_string()]),
        retains_data: false,
        permits_training_use: true,
    }
}

fn request(capability: TeacherCapability, wants: Vec<OutputKind>, licence: UsageLicence) -> TeacherRequest {
    TeacherRequest {
        request_id: "req-1".to_string(),
        capability,
        parts: vec![
            ContentPart::Reference { artifact: "sha256:img".to_string(), media_type: "image/png".to_string() },
            ContentPart::Numbers { values: vec![0.4, -1.2] },
        ],
        licence,
        wants,
        max_response_bytes: 64 * 1024,
    }
}

/// A scripted teacher used to exercise the host-side rules without a socket.
struct ScriptedTeacher {
    answer: Box<dyn Fn(&TeacherRequest) -> Result<TeacherResponse, loom::HostError> + Send + Sync>,
}

impl TeacherHost for ScriptedTeacher {
    fn query(&self, request: &TeacherRequest) -> Result<TeacherResponse, loom::HostError> {
        validate_request(request)?;
        (self.answer)(request)
    }
}

// ── protocol rules ─────────────────────────────────────────────────

#[test]
fn an_undeclared_modality_is_refused_before_any_call() {
    let mut capability = hard_teacher();
    capability.input_modalities = vec![Modality::Image, Modality::Numeric];
    let mut req = request(capability, vec![OutputKind::HardLabel], UsageLicence::TrainStudent);
    // Text was never declared as an accepted modality.
    req.parts.push(ContentPart::Text { text: "notes".to_string() });

    let err = validate_request(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Config);
    assert!(err.to_string().contains("Text"), "unhelpful error: {err}");
}

#[test]
fn requesting_an_output_the_teacher_never_declared_is_refused() {
    let capability = hard_teacher();
    // No SoftDistribution in the capability, so the request cannot have it.
    let req = request(capability, vec![OutputKind::SoftDistribution], UsageLicence::TrainStudent);
    let err = validate_request(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Config);
    assert!(err.to_string().contains("SoftDistribution"), "{err}");
}

#[test]
fn a_hard_only_teacher_cannot_pretend_to_have_logits() {
    let capability = hard_teacher();
    assert!(!capability.supports_soft_targets());
    assert!(capability.supports(OutputKind::HardLabel));
}

#[test]
fn soft_scores_without_a_matching_vocabulary_are_a_protocol_failure() {
    let capability = soft_teacher();
    let response = TeacherResponse {
        request_id: "req-1".to_string(),
        teacher_id: "local-teacher".to_string(),
        model_version: None,
        hard_label: Some("fault".to_string()),
        // Three scores for a two-class vocabulary.
        soft_scores: Some(vec![0.1, 0.2, 0.7]),
        program: None,
        error: None,
    };
    let err = response.soft_targets(&capability).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Protocol);

    let mut no_vocab = soft_teacher();
    no_vocab.vocabulary = None;
    let err = response.soft_targets(&no_vocab).unwrap_err();
    assert!(err.to_string().contains("vocabulary"), "{err}");
}

#[test]
fn a_non_finite_soft_score_is_refused() {
    let capability = soft_teacher();
    let response = TeacherResponse {
        request_id: "req-1".to_string(),
        teacher_id: "local-teacher".to_string(),
        model_version: None,
        hard_label: None,
        soft_scores: Some(vec![0.5, f64::NAN]),
        program: None,
        error: None,
    };
    assert_eq!(response.soft_targets(&capability).unwrap_err().kind, HostErrorKind::Protocol);
}

#[test]
fn aligned_soft_scores_are_accepted() {
    let capability = soft_teacher();
    let response = TeacherResponse {
        request_id: "req-1".to_string(),
        teacher_id: "local-teacher".to_string(),
        model_version: Some("local-net-1".to_string()),
        hard_label: Some("fault".to_string()),
        soft_scores: Some(vec![0.2, 0.8]),
        program: None,
        error: None,
    };
    let scores = response.soft_targets(&capability).unwrap();
    assert_eq!(scores, &[0.2, 0.8]);
}

#[test]
fn a_failed_exchange_stays_visible() {
    let response = TeacherResponse {
        request_id: "req-1".to_string(),
        teacher_id: "local-teacher".to_string(),
        model_version: None,
        hard_label: None,
        soft_scores: None,
        program: None,
        error: Some("weights file missing".to_string()),
    };
    let err = response.failure().unwrap_err();
    assert!(err.to_string().contains("weights file missing"), "{err}");
    // A failure never becomes an empty success.
    assert!(response.hard_label.is_none());
}

#[test]
fn a_proposed_program_is_marked_as_a_proposal_with_its_author() {
    // A teacher can hand back a program, but it is a proposal that must
    // re-enter the reader and whitelist gates like any other candidate.
    let capability = hard_teacher();
    assert!(capability.supports(OutputKind::Program));
    let proposal = ProposedProgram {
        language: "zio".to_string(),
        source: "(+ x 1)".to_string(),
        proposed_by: "local-teacher".to_string(),
    };
    assert_eq!(proposal.proposed_by, "local-teacher");
    // The type carries no notion of approval: there is no `approved` field.
    let encoded = serde_json::to_value(&proposal).unwrap();
    let keys = encoded.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys, vec!["language", "proposed_by", "source"]);
}

#[test]
fn a_teacher_that_forbids_training_use_refuses_a_training_licence() {
    let mut capability = hard_teacher();
    capability.permits_training_use = false;
    let req = request(capability, vec![OutputKind::HardLabel], UsageLicence::TrainStudent);
    let err = validate_request(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Config);
    assert!(err.to_string().contains("training"), "{err}");
}

#[test]
fn a_forbidden_licence_is_refused() {
    let capability = hard_teacher();
    let req = request(capability, vec![OutputKind::HardLabel], UsageLicence::Forbidden);
    assert_eq!(validate_request(&req).unwrap_err().kind, HostErrorKind::Config);
}

#[test]
fn a_request_without_a_response_budget_is_refused() {
    let mut req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::DisplayOnly);
    req.max_response_bytes = 0;
    assert!(validate_request(&req).is_err());
}

#[test]
fn a_recording_teacher_captures_every_validated_call() {
    let teacher = RecordingTeacher::new(ScriptedTeacher {
        answer: Box::new(|req: &TeacherRequest| {
            Ok(TeacherResponse {
                request_id: req.request_id.clone(),
                teacher_id: "local-teacher".to_string(),
                model_version: Some("local-net-1".to_string()),
                hard_label: Some("fault".to_string()),
                soft_scores: None,
                program: None,
                error: None,
            })
        }),
    });
    let req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::TrainStudent);
    let answer = teacher.query(&req).unwrap();
    assert_eq!(answer.hard_label.as_deref(), Some("fault"));
    assert_eq!(teacher.recorded().len(), 1);

    // A refused request never reaches the inner host.
    let mut bad = req.clone();
    bad.wants = vec![OutputKind::SoftDistribution];
    assert!(teacher.query(&bad).is_err());
    assert_eq!(teacher.recorded().len(), 1, "a refused request must not be recorded as a call");
}

// ── real HTTP round trip ───────────────────────────────────────────

/// Serve `body` once on a loopback port and hand the port back.
fn serve_once(body: &'static str, delay: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf);
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes());
        let _ = socket.flush();
        std::thread::sleep(Duration::from_millis(200));
    });
    port
}

#[test]
fn a_real_teacher_call_round_trips_over_http() {
    let body = r#"{"request_id":"req-1","teacher_id":"local-teacher","model_version":"local-net-1","hard_label":"fault","soft_scores":null,"program":null,"error":null}"#;
    let port = serve_once(body, Duration::ZERO);
    let host = HttpTeacherHost::builder()
        .base_url(format!("http://127.0.0.1:{port}/v1"))
        .timeout(Duration::from_secs(2))
        .build();
    let req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::TrainStudent);
    let response = host.query(&req).unwrap();
    assert_eq!(response.hard_label.as_deref(), Some("fault"));
    assert_eq!(response.model_version.as_deref(), Some("local-net-1"));
}

#[test]
fn an_oversized_teacher_response_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf);
        let body = "x".repeat(8192);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes());
        let _ = socket.flush();
        std::thread::sleep(Duration::from_millis(200));
    });

    let host = HttpTeacherHost::builder()
        .base_url(format!("http://127.0.0.1:{port}/v1"))
        .timeout(Duration::from_secs(2))
        .max_response_bytes(512)
        .build();
    let req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::DisplayOnly);
    let err = host.query(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Protocol, "got: {err}");
}

#[test]
fn a_teacher_that_answers_the_wrong_request_is_a_protocol_failure() {
    let body = r#"{"request_id":"someone-elses-request","teacher_id":"local-teacher","hard_label":"fault"}"#;
    let port = serve_once(body, Duration::ZERO);
    let host = HttpTeacherHost::builder()
        .base_url(format!("http://127.0.0.1:{port}/v1"))
        .timeout(Duration::from_secs(2))
        .build();
    let req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::DisplayOnly);
    let err = host.query(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Protocol);
}

#[test]
fn a_silent_teacher_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_secs(5));
    });
    let host = HttpTeacherHost::builder()
        .base_url(format!("http://127.0.0.1:{port}/v1"))
        .timeout(Duration::from_millis(300))
        .build();
    let req = request(hard_teacher(), vec![OutputKind::HardLabel], UsageLicence::DisplayOnly);
    let err = host.query(&req).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Timeout, "got: {err}");
}

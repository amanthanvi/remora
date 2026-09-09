use remora_bridge_conformance::{Frame, FrameKind, upstream_schema};
use remora_codex_proto::WarningNotification;
use serde_json::json;

#[test]
fn local_warning_notification_conforms_to_pinned_codex_schema() {
    upstream_schema::schema_dir()
        .expect("upstream schema validation is required for this proof test");

    let notification = WarningNotification {
        thread_id: Some("thread-ci".to_string()),
        message: "deterministic schema fixture".to_string(),
    };
    let valid = Frame {
        step: "pinned-schema".to_string(),
        kind: FrameKind::Notification,
        method: "warning".to_string(),
        raw: json!({
            "jsonrpc": "2.0",
            "method": "warning",
            "params": serde_json::to_value(notification).expect("serialize local wire type"),
        }),
    };
    upstream_schema::validate(&valid)
        .expect("local warning notification must match the pinned upstream schema");

    let missing_required_message = Frame {
        raw: json!({
            "jsonrpc": "2.0",
            "method": "warning",
            "params": { "threadId": "thread-ci" },
        }),
        ..valid
    };
    let error = upstream_schema::validate(&missing_required_message)
        .expect_err("the pinned schema must load and reject a missing required field");
    assert!(
        error.contains("warning") && error.contains("not valid"),
        "unexpected schema error: {error}"
    );
}

#[test]
fn disabled_validation_cannot_pass_the_schema_proof_test() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "local_warning_notification_conforms_to_pinned_codex_schema",
            "--nocapture",
        ])
        .env("BRIDGE_CONFORMANCE_SKIP_UPSTREAM_SCHEMA", "1")
        .output()
        .expect("run isolated disabled-validation probe");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("upstream schema validation is required for this proof test")
    );
}

#[test]
fn experimental_response_fixtures_validate_payloads_and_reject_wrong_shapes() {
    upstream_schema::schema_dir()
        .expect("upstream schema validation is required for this proof test");
    for (method, valid, invalid) in [
        (
            "mock/experimentalMethod",
            json!({"echoed": "fixture"}),
            json!({"echoed": 42}),
        ),
        (
            "collaborationMode/list",
            json!({"data": [{"name": "Plan", "mode": "plan"}]}),
            json!({"data": [{"mode": "plan"}]}),
        ),
        ("thread/backgroundTerminals/clean", json!({}), json!(null)),
        (
            "thread/turns/list",
            json!({"data": [{"id": "turn-1", "items": [], "status": "completed"}]}),
            json!({"data": [{"id": "turn-1", "items": [], "status": "unknown"}]}),
        ),
    ] {
        let frame = Frame {
            step: "experimental-schema".into(),
            kind: FrameKind::Response,
            method: method.into(),
            raw: json!({"result": valid}),
        };
        upstream_schema::validate(&frame)
            .unwrap_or_else(|error| panic!("valid {method} fixture rejected: {error}"));
        let malformed = Frame {
            raw: json!({"result": invalid}),
            ..frame
        };
        assert!(upstream_schema::validate(&malformed).is_err(), "{method}");
    }
}

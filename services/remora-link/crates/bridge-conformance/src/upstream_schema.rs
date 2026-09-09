//! Validate captured wire frames against the canonical
//! `codex-rs/app-server-protocol/schema/json/v2/` JSON schemas.
//!
//! Why: our local `codex-proto` types are a hand-maintained mirror that can
//! drift from upstream — fields renamed, enum variants added, optional/required
//! flipped. The diff layer's typed-decode check only proves we're consistent
//! with the *mirror*. This pass loads the real schemas codex publishes and
//! validates each frame against them; a violation here is a real wire-spec
//! gap the bridge needs to fix.
//!
//! The schema directory defaults to Remora's retained Codex checkout. If absent,
//! validation panics unless `BRIDGE_CONFORMANCE_SKIP_UPSTREAM_SCHEMA=1` is
//! set. Silent schema skips hide exactly the drift this harness is meant to
//! catch.
//!
//! Responses use explicit mappings from upstream `protocol/common.rs`.
//! Notifications use upstream's complete `ServerNotification` envelope schema,
//! which owns method-to-payload routing. Unknown methods never silently pass.
//! A custom v2 directory must retain its sibling `v1` and parent envelope files.
//! Four experimental responses use exact generator output in test fixtures;
//! `scripts/experimental-schemas.sh --check` verifies retained-source parity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jsonschema::Validator;
use serde_json::Value;

use crate::{Frame, FrameKind};

const ENV_OVERRIDE: &str = "BRIDGE_CONFORMANCE_CODEX_SCHEMA_DIR";
const ENV_SKIP: &str = "BRIDGE_CONFORMANCE_SKIP_UPSTREAM_SCHEMA";
fn default_schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../shared/third_party/codex/codex-rs/app-server-protocol/schema/json/v2")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaSkipReason {
    ExplicitSkip,
}

/// Directory holding the upstream v2 JSON schema files.
pub fn schema_dir() -> Result<PathBuf, SchemaSkipReason> {
    if std::env::var_os(ENV_SKIP).as_deref() == Some(std::ffi::OsStr::new("1")) {
        warn_skip_once();
        return Err(SchemaSkipReason::ExplicitSkip);
    }
    if let Some(custom) = std::env::var_os(ENV_OVERRIDE) {
        let p = PathBuf::from(custom);
        if p.is_dir() {
            return Ok(p);
        }
        panic_missing(Some(p));
    }
    let candidate = default_schema_dir();
    if candidate.is_dir() {
        return Ok(candidate);
    }
    panic_missing(Some(candidate));
}

/// Validate a single captured frame against its upstream schema. Returns
/// `Ok(())` when validation is explicitly skipped or the frame validates;
/// otherwise returns the validator's error list joined into one human-readable
/// message.
pub fn validate(frame: &Frame) -> Result<(), String> {
    let dir = match schema_dir() {
        Ok(dir) => dir,
        Err(SchemaSkipReason::ExplicitSkip) => return Ok(()),
    };
    validate_in_dir(frame, &dir)
}

fn validate_in_dir(frame: &Frame, dir: &Path) -> Result<(), String> {
    let path = match frame.kind {
        FrameKind::Response => response_schema_path(&frame.method, dir)?,
        FrameKind::Notification => dir.join("../ServerNotification.json"),
    };
    validate_schema_file(frame, &path)
}

fn response_schema_path(method: &str, dir: &Path) -> Result<PathBuf, String> {
    let name = response_schema(method)?;
    let dir = if dir == default_schema_dir()
        && matches!(
            method,
            "mock/experimentalMethod"
                | "collaborationMode/list"
                | "thread/turns/list"
                | "thread/backgroundTerminals/clean"
        ) {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/experimental")
    } else {
        dir.to_path_buf()
    };
    Ok(dir.join(name))
}

fn validate_schema_file(frame: &Frame, schema_path: &Path) -> Result<(), String> {
    let validator = cached_validator(schema_path)?;
    let payload = match frame.kind {
        FrameKind::Response => frame
            .raw
            .get("result")
            .ok_or_else(|| format!("response {} is missing result", frame.method))?,
        FrameKind::Notification => {
            if frame.raw.get("method").and_then(Value::as_str) != Some(&frame.method) {
                return Err("notification method differs from captured method".into());
            }
            &frame.raw
        }
    };
    let errors: Vec<String> = validator
        .iter_errors(payload)
        .map(|e| format!("{}: {}", e.instance_path, e))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn warn_skip_once() {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            env = ENV_SKIP,
            "upstream schema validation explicitly skipped"
        );
    });
}

fn panic_missing(candidate: Option<PathBuf>) -> ! {
    let default = default_schema_dir().display().to_string();
    let checked = candidate
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| default.clone());
    panic!(
        "upstream codex JSON schemas are required for bridge conformance\n\
         checked: {checked}\n\
         set {ENV_OVERRIDE}=<schema-dir> to point at app-server-protocol/schema/json/v2\n\
         default path: {default}\n\
         set {ENV_SKIP}=1 only when this validation is intentionally disabled"
    );
}

fn response_schema(method: &str) -> Result<&'static str, String> {
    Ok(match method {
        "initialize" => "../v1/InitializeResponse.json",
        "config/read" => "ConfigReadResponse.json",
        "config/value/write" | "config/batchWrite" => "ConfigWriteResponse.json",
        "configRequirements/read" => "ConfigRequirementsReadResponse.json",
        "model/list" => "ModelListResponse.json",
        "experimentalFeature/list" => "ExperimentalFeatureListResponse.json",
        "collaborationMode/list" => "CollaborationModeListResponse.json",
        "mcpServerStatus/list" => "ListMcpServerStatusResponse.json",
        "config/mcpServer/reload" => "McpServerRefreshResponse.json",
        "mcpServer/oauth/login" => "McpServerOauthLoginResponse.json",
        "skills/list" => "SkillsListResponse.json",
        "skills/config/write" => "SkillsConfigWriteResponse.json",
        "account/read" => "GetAccountResponse.json",
        "account/rateLimits/read" => "GetAccountRateLimitsResponse.json",
        "account/login/start" => "LoginAccountResponse.json",
        "account/login/cancel" => "CancelLoginAccountResponse.json",
        "account/logout" => "LogoutAccountResponse.json",
        "feedback/upload" => "FeedbackUploadResponse.json",
        "thread/start" => "ThreadStartResponse.json",
        "thread/resume" => "ThreadResumeResponse.json",
        "thread/fork" => "ThreadForkResponse.json",
        "thread/read" => "ThreadReadResponse.json",
        "thread/list" => "ThreadListResponse.json",
        "thread/loaded/list" => "ThreadLoadedListResponse.json",
        "thread/archive" => "ThreadArchiveResponse.json",
        "thread/unarchive" => "ThreadUnarchiveResponse.json",
        "thread/name/set" => "ThreadSetNameResponse.json",
        "thread/compact/start" => "ThreadCompactStartResponse.json",
        "thread/rollback" => "ThreadRollbackResponse.json",
        "thread/turns/list" => "ThreadTurnsListResponse.json",
        "thread/backgroundTerminals/clean" => "ThreadBackgroundTerminalsCleanResponse.json",
        "turn/start" => "TurnStartResponse.json",
        "turn/steer" => "TurnSteerResponse.json",
        "turn/interrupt" => "TurnInterruptResponse.json",
        "review/start" => "ReviewStartResponse.json",
        "command/exec" => "CommandExecResponse.json",
        "command/exec/write" => "CommandExecWriteResponse.json",
        "command/exec/terminate" => "CommandExecTerminateResponse.json",
        "command/exec/resize" => "CommandExecResizeResponse.json",
        "mock/experimentalMethod" => "MockExperimentalMethodResponse.json",
        "skills/remote/list" | "skills/remote/export" => {
            return Err(format!(
                "{method} is absent from the retained upstream protocol"
            ));
        }
        _ => return Err(format!("unsupported upstream response schema for {method}")),
    })
}

fn cached_validator(path: &Path) -> Result<&'static Validator, String> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, &'static Validator>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("schema cache poisoned");
    if let Some(&v) = guard.get(path) {
        return Ok(v);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let json: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let validator =
        jsonschema::draft7::new(&json).map_err(|e| format!("compile {}: {e}", path.display()))?;
    let leaked: &'static Validator = Box::leak(Box::new(validator));
    guard.insert(path.to_path_buf(), leaked);
    Ok(leaked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schemas_come_from_retained_checkout() {
        assert!(
            default_schema_dir()
                .join("WarningNotification.json")
                .is_file()
        );
    }

    #[test]
    fn invalid_schema_is_a_failure_not_a_successful_validation() {
        let directory = tempfile::tempdir().unwrap();
        let schema = directory.path().join("WarningNotification.json");
        std::fs::write(&schema, "not JSON").unwrap();
        let frame = Frame {
            step: "schema-load".into(),
            kind: FrameKind::Notification,
            method: "warning".into(),
            raw: serde_json::json!({"params": {"message": "fixture"}}),
        };
        assert!(
            validate_schema_file(&frame, &schema)
                .unwrap_err()
                .starts_with("parse ")
        );
    }

    #[test]
    fn probed_responses_have_generated_schemas_or_explicitly_retired_methods() {
        let dir = default_schema_dir();
        for method in
            std::iter::once(&"initialize").chain(crate::method_surface::STANDARD_REQUEST_METHODS)
        {
            if matches!(*method, "skills/remote/list" | "skills/remote/export") {
                assert!(response_schema(method).unwrap_err().contains("absent"));
            } else {
                let path = response_schema_path(method, &dir).unwrap();
                cached_validator(&path).unwrap_or_else(|error| panic!("{method}: {error}"));
            }
        }
    }

    #[test]
    fn unknown_response_is_not_successful_validation() {
        let frame = Frame {
            step: "unmapped".into(),
            kind: FrameKind::Response,
            method: "bridge/unknown".into(),
            raw: serde_json::json!({"result": {}}),
        };
        assert!(validate_in_dir(&frame, &default_schema_dir()).is_err());
    }

    #[test]
    fn irregular_notification_name_does_not_skip_validation() {
        let frame = Frame {
            step: "delta".into(),
            kind: FrameKind::Notification,
            method: "item/agentMessage/delta".into(),
            raw: serde_json::json!({
                "method": "item/agentMessage/delta", "params": {}
            }),
        };
        assert!(validate_in_dir(&frame, &default_schema_dir()).is_err());
    }

    #[test]
    fn irregular_response_name_does_not_skip_validation() {
        let frame = Frame {
            step: "account".into(),
            kind: FrameKind::Response,
            method: "account/read".into(),
            raw: serde_json::json!({"result": {}}),
        };
        assert!(validate_in_dir(&frame, &default_schema_dir()).is_err());
    }

    #[test]
    fn missing_mapped_schema_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let frame = Frame {
            step: "missing".into(),
            kind: FrameKind::Response,
            method: "thread/start".into(),
            raw: serde_json::json!({"result": {}}),
        };
        assert!(
            validate_in_dir(&frame, dir.path())
                .unwrap_err()
                .starts_with("read ")
        );
    }

    #[test]
    fn canonical_notification_envelope_rejects_unknown_and_mismatched_methods() {
        for (captured, wire) in [("bridge/unknown", "bridge/unknown"), ("error", "warning")] {
            let frame = Frame {
                step: "method".into(),
                kind: FrameKind::Notification,
                method: captured.into(),
                raw: serde_json::json!({"method": wire, "params": {"message": "fixture"}}),
            };
            assert!(validate_in_dir(&frame, &default_schema_dir()).is_err());
        }
    }
}

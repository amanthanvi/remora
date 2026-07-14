use super::rpc;
use crate::MobileClient;
use crate::ffi::ClientError;
use crate::next_request_id;
use crate::types;
use base64::Engine;
use codex_app_server_protocol as upstream;
use url::Url;

const PET_RUNTIME_UNAVAILABLE_MESSAGE: &str = "pets require a connected Codex runtime; pair a remote host with the Codex agent or connect to a Codex server";

/// Execute a simple one-shot command on a remote server.
pub(crate) async fn exec_command_simple(
    client: &MobileClient,
    server_id: &str,
    command: &[&str],
    cwd: Option<&str>,
) -> Result<upstream::CommandExecResponse, ClientError> {
    let params = upstream::CommandExecParams {
        command: command.iter().map(|s| s.to_string()).collect(),
        process_id: None,
        tty: false,
        stream_stdin: false,
        stream_stdout_stderr: false,
        output_bytes_cap: None,
        disable_output_cap: false,
        disable_timeout: false,
        timeout_ms: None,
        cwd: cwd
            .and_then(crate::remote_path::normalize_thread_cwd)
            .map(std::path::PathBuf::from),
        env: None,
        size: None,
        sandbox_policy: Some(upstream::SandboxPolicy::DangerFullAccess),
        permission_profile: None,
    };
    rpc(
        client,
        server_id,
        req!(server_id, OneOffCommandExec, params),
    )
    .await
}

/// Tolerant wire-compat mirror of `upstream::FuzzyFileSearchResponse`.
///
/// `match_type` was added upstream in March 2026 (commit 10eb3ec7f, "Simple
/// directory mentions"). Older `codex` server binaries omit it, which would
/// cause strict deserialization against `upstream::FuzzyFileSearchResponse`
/// to fail for the entire response. Default to `File` when absent.
#[derive(serde::Deserialize)]
pub(super) struct WireFuzzyFileSearchResponse {
    #[serde(default)]
    pub(super) files: Vec<WireFuzzyFileSearchResult>,
}

#[derive(serde::Deserialize)]
pub(super) struct WireFuzzyFileSearchResult {
    root: String,
    path: String,
    #[serde(default = "default_match_type")]
    match_type: upstream::FuzzyFileSearchMatchType,
    file_name: String,
    score: u32,
    #[serde(default)]
    indices: Option<Vec<u32>>,
}

fn default_match_type() -> upstream::FuzzyFileSearchMatchType {
    upstream::FuzzyFileSearchMatchType::File
}

impl From<WireFuzzyFileSearchResult> for types::FileSearchResult {
    fn from(value: WireFuzzyFileSearchResult) -> Self {
        Self {
            root: value.root,
            path: value.path,
            match_type: value.match_type.into(),
            file_name: value.file_name,
            score: value.score,
            indices: value.indices,
        }
    }
}

pub(super) async fn resolve_image_view_bytes(
    client: &MobileClient,
    server_id: &str,
    raw_path: &str,
) -> Result<types::ResolvedImageViewResult, ClientError> {
    let source = ImageViewSource::parse(raw_path)
        .ok_or_else(|| ClientError::InvalidParams("image_view path is empty".to_string()))?;

    match source {
        ImageViewSource::InlineData(bytes) => Ok(types::ResolvedImageViewResult {
            path: raw_path.to_string(),
            bytes,
        }),
        ImageViewSource::FilePath(path) => {
            if let Ok(bytes) = std::fs::read(&path) {
                return Ok(types::ResolvedImageViewResult { path, bytes });
            }

            if server_id.trim().is_empty() {
                return Err(ClientError::Rpc(
                    "Image path could not be read locally and no server is available.".to_string(),
                ));
            }

            let response =
                exec_command_simple_owned(client, server_id, image_read_command(&path), None)
                    .await?;

            if response.exit_code != 0 {
                let stderr = response.stderr.trim();
                return Err(ClientError::Rpc(if stderr.is_empty() {
                    "Image read failed.".to_string()
                } else {
                    stderr.to_string()
                }));
            }

            let payload: String = response
                .stdout
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|error| {
                    ClientError::Serialization(format!("invalid image base64: {error}"))
                })?;

            Ok(types::ResolvedImageViewResult { path, bytes })
        }
    }
}

async fn exec_command_simple_owned(
    client: &MobileClient,
    server_id: &str,
    command: Vec<String>,
    cwd: Option<String>,
) -> Result<upstream::CommandExecResponse, ClientError> {
    let params = upstream::CommandExecParams {
        command,
        process_id: None,
        tty: false,
        stream_stdin: false,
        stream_stdout_stderr: false,
        output_bytes_cap: Some(20_000_000),
        disable_output_cap: false,
        disable_timeout: false,
        timeout_ms: Some(15_000),
        cwd: cwd
            .as_deref()
            .and_then(crate::remote_path::normalize_thread_cwd)
            .map(std::path::PathBuf::from),
        env: None,
        size: None,
        sandbox_policy: None,
        permission_profile: None,
    };
    rpc(
        client,
        server_id,
        req!(server_id, OneOffCommandExec, params),
    )
    .await
}

fn image_read_command(path: &str) -> Vec<String> {
    if is_windows_path(path) {
        return vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $p = $args[0]; if ($p.StartsWith('~/') -or $p.StartsWith('~\\\\')) { $p = Join-Path $HOME $p.Substring(2) }; [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($p))".to_string(),
            path.to_string(),
        ];
    }

    vec![
        "/usr/bin/env".to_string(),
        "sh".to_string(),
        "-lc".to_string(),
        r#"path="$1"; case "$path" in "~/"*) path="$HOME/${path#~/}" ;; esac; base64 < "$path""#
            .to_string(),
        "sh".to_string(),
        path.to_string(),
    ]
}

fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || path.starts_with("\\\\")
}

pub(super) struct RemotePetScanEntry {
    pub(super) summary: types::AppPetSummary,
    pub(super) manifest_json: String,
}

pub(super) async fn scan_remote_pets(
    client: &MobileClient,
    server_id: &str,
) -> Result<Vec<RemotePetScanEntry>, ClientError> {
    ensure_pet_runtime_available(client, server_id)?;

    let script = r#"root="${CODEX_HOME:-$HOME/.codex}/pets"
[ -d "$root" ] || exit 0
for manifest in "$root"/*/pet.json; do
  [ -f "$manifest" ] || continue
  dir=${manifest%/pet.json}
  printf '%s\t' "$(printf '%s' "$dir" | base64 | tr -d '\n')"
  base64 < "$manifest" | tr -d '\n'
  printf '\n'
done"#;
    let response = exec_command_simple_owned(
        client,
        server_id,
        vec![
            "/usr/bin/env".to_string(),
            "sh".to_string(),
            "-lc".to_string(),
            script.to_string(),
        ],
        None,
    )
    .await?;
    if response.exit_code != 0 {
        let stderr = response.stderr.trim();
        return Err(ClientError::Rpc(if stderr.is_empty() {
            "pet scan failed".to_string()
        } else {
            stderr.to_string()
        }));
    }

    let mut entries = Vec::new();
    for line in response.stdout.lines() {
        let Some((path_b64, manifest_b64)) = line.split_once('\t') else {
            continue;
        };
        let path = decode_base64_utf8(path_b64, "pet path")?;
        let manifest_json = decode_base64_utf8(manifest_b64, "pet manifest")?;
        let mut summary = crate::pets::summary_from_manifest(path.clone(), &manifest_json, false);
        if let Some(spritesheet_file) = summary.spritesheet_path.as_deref() {
            let spritesheet_path = crate::pets::local_spritesheet_path(&path, spritesheet_file)
                .map_err(ClientError::Serialization)?;
            summary.has_valid_spritesheet =
                remote_file_exists(client, server_id, &spritesheet_path).await?;
            if summary.has_valid_spritesheet {
                summary.validation_error = None;
            } else if summary.validation_error.is_none() {
                summary.validation_error = Some(format!("{spritesheet_file} is missing"));
            }
        }
        entries.push(RemotePetScanEntry {
            summary,
            manifest_json,
        });
    }
    entries.sort_by(|a, b| {
        a.summary
            .display_name
            .to_lowercase()
            .cmp(&b.summary.display_name.to_lowercase())
    });
    Ok(entries)
}

fn ensure_pet_runtime_available(client: &MobileClient, server_id: &str) -> Result<(), ClientError> {
    let runtime_kinds = client
        .get_session(server_id)
        .map_err(|error| ClientError::Rpc(error.to_string()))?
        .runtime_kinds();
    if runtime_kinds.contains(&"codex".to_string()) {
        return Ok(());
    }
    Err(ClientError::Rpc(
        PET_RUNTIME_UNAVAILABLE_MESSAGE.to_string(),
    ))
}

async fn remote_file_exists(
    client: &MobileClient,
    server_id: &str,
    path: &str,
) -> Result<bool, ClientError> {
    let response =
        exec_command_simple_owned(client, server_id, file_exists_command(path), None).await?;
    Ok(response.exit_code == 0)
}

pub(super) async fn read_remote_file_bytes(
    client: &MobileClient,
    server_id: &str,
    path: &str,
) -> Result<Vec<u8>, ClientError> {
    let response =
        exec_command_simple_owned(client, server_id, image_read_command(path), None).await?;
    if response.exit_code != 0 {
        let stderr = response.stderr.trim();
        return Err(ClientError::Rpc(if stderr.is_empty() {
            "file read failed".to_string()
        } else {
            stderr.to_string()
        }));
    }
    let payload: String = response
        .stdout
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|error| ClientError::Serialization(format!("invalid file base64: {error}")))
}

fn decode_base64_utf8(value: &str, label: &str) -> Result<String, ClientError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| ClientError::Serialization(format!("invalid {label} base64: {error}")))?;
    String::from_utf8(bytes)
        .map_err(|error| ClientError::Serialization(format!("invalid {label} utf8: {error}")))
}

fn file_exists_command(path: &str) -> Vec<String> {
    if is_windows_path(path) {
        return vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "$p = $args[0]; if ($p.StartsWith('~/') -or $p.StartsWith('~\\\\')) { $p = Join-Path $HOME $p.Substring(2) }; if (Test-Path -LiteralPath $p -PathType Leaf) { exit 0 } else { exit 1 }".to_string(),
            path.to_string(),
        ];
    }
    vec![
        "/usr/bin/env".to_string(),
        "sh".to_string(),
        "-lc".to_string(),
        r#"path="$1"; case "$path" in "~/"*) path="$HOME/${path#~/}" ;; esac; test -f "$path""#
            .to_string(),
        "sh".to_string(),
        path.to_string(),
    ]
}

enum ImageViewSource {
    InlineData(Vec<u8>),
    FilePath(String),
}

impl ImageViewSource {
    fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(bytes) = inline_image_data(trimmed) {
            return Some(Self::InlineData(bytes));
        }

        if let Some(path) = normalized_image_path(trimmed) {
            return Some(Self::FilePath(path));
        }

        None
    }
}

fn normalized_image_path(raw: &str) -> Option<String> {
    if raw.starts_with("file://") {
        let url = Url::parse(raw).ok()?;
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
        }
    }

    if raw.starts_with('/')
        || raw.starts_with("~/")
        || raw.starts_with("\\\\")
        || is_windows_path(raw)
    {
        return Some(raw.to_string());
    }

    None
}

fn inline_image_data(raw: &str) -> Option<Vec<u8>> {
    let source = raw.strip_prefix("data:image/")?;
    let (_, payload) = source.split_once(";base64,")?;
    let normalized: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{
        ImageViewSource, PET_RUNTIME_UNAVAILABLE_MESSAGE, image_read_command, normalized_image_path,
    };

    #[test]
    fn pet_runtime_error_uses_neutral_remote_pairing_copy() {
        assert_eq!(
            PET_RUNTIME_UNAVAILABLE_MESSAGE,
            "pets require a connected Codex runtime; pair a remote host with the Codex agent or connect to a Codex server"
        );
        assert!(!PET_RUNTIME_UNAVAILABLE_MESSAGE.contains("Alleycat"));
    }

    #[test]
    fn parses_inline_image_data() {
        let source = ImageViewSource::parse("data:image/png;base64,SGVsbG8=");
        match source {
            Some(ImageViewSource::InlineData(bytes)) => assert_eq!(bytes, b"Hello"),
            _ => panic!("expected inline image data"),
        }
    }

    #[test]
    fn normalizes_file_url_path() {
        assert_eq!(
            normalized_image_path("file:///tmp/example.png").as_deref(),
            Some("/tmp/example.png")
        );
    }

    #[test]
    fn builds_posix_image_read_command_with_remote_tilde_expansion() {
        let command = image_read_command("~/image.png");
        assert_eq!(command[0], "/usr/bin/env");
        assert_eq!(command[1], "sh");
        assert!(command[3].contains(r#"${path#~/}"#));
    }
}

//! Rust-owned source preview and unified-diff shaping.
//!
//! The public interface is deliberately narrow:
//!
//! - diff review is keyed only by [`ThreadKey`] and reads trusted patches from
//!   the canonical Rust store;
//! - source preview is keyed by [`ThreadKey`] plus a workspace-relative path;
//! - callers cannot supply a cwd, absolute path, or command fallback.
//!
//! Codex's current `fs/readFile` method accepts an unrestricted absolute path
//! and does not provide an atomic, root-confined read. It is therefore not a
//! valid adapter for this module. Until Remora Link exposes a capability that
//! enforces this contract on the host, source preview fails closed with
//! [`SourcePreviewUnsupportedReason::CapabilityUnavailable`].

use crate::MobileClient;
use crate::conversation_uniffi::HydratedConversationItemContent;
use crate::types::ThreadKey;
use sha1::{Digest, Sha1};

/// Maximum source bytes returned through UniFFI.
pub const MAX_SOURCE_PREVIEW_BYTES: usize = 1024 * 1024;

/// Maximum trusted diff bytes shaped in one request.
pub const MAX_DIFF_REVIEW_BYTES: usize = 1024 * 1024;

const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_DISPLAY_PATH_BYTES: usize = 512;
const MAX_DIFF_FILES: usize = 2048;
const MAX_DIFF_ROWS: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SourcePreviewUnsupportedReason {
    ThreadUnavailable,
    WorkspaceUnavailable,
    InvalidRelativePath,
    CapabilityUnavailable,
    OutsideWorkspace,
    NotFile,
    UnsupportedEncoding,
    ReadFailed,
}

/// Bounded source-preview result. Expected availability and validation
/// outcomes are typed values rather than transport errors so both native
/// clients render the same state.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SourcePreviewResult {
    Text {
        thread_key: ThreadKey,
        relative_path: String,
        text: String,
        byte_length: u64,
        truncated: bool,
    },
    Image {
        thread_key: ThreadKey,
        relative_path: String,
        mime_type: String,
        bytes: Vec<u8>,
        byte_length: u64,
        truncated: bool,
    },
    Binary {
        thread_key: ThreadKey,
        relative_path: String,
        byte_length: u64,
        truncated: bool,
    },
    Unsupported {
        thread_key: ThreadKey,
        relative_path: String,
        reason: SourcePreviewUnsupportedReason,
        byte_length: Option<u64>,
        truncated: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiffReviewUnavailableReason {
    ThreadUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum DiffReviewResult {
    Ready {
        review: DiffReview,
    },
    Empty {
        thread_key: ThreadKey,
    },
    Unsupported {
        thread_key: ThreadKey,
        reason: DiffReviewUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiffReview {
    pub thread_key: ThreadKey,
    pub files: Vec<DiffReviewFile>,
    pub additions: u32,
    pub deletions: u32,
    pub source_byte_length: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiffFileChangeKind {
    Added,
    Deleted,
    Renamed,
    Modified,
    Binary,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiffReviewFile {
    pub id: String,
    pub display_path: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub change_kind: DiffFileChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: Vec<DiffReviewHunk>,
    /// Retained for explicit copy/export. The aggregate raw-patch payload is
    /// bounded by `MAX_DIFF_REVIEW_BYTES` before parsing.
    pub raw_patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiffReviewHunk {
    pub id: String,
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub rows: Vec<DiffReviewRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiffReviewRowKind {
    Context,
    Addition,
    Deletion,
    Metadata,
    NoNewlineMarker,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiffReviewRow {
    pub id: String,
    pub kind: DiffReviewRowKind,
    pub old_line_number: Option<u32>,
    pub new_line_number: Option<u32>,
    /// Text excludes the unified-diff prefix for context/add/delete rows.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceRelativePath {
    normalized: String,
    components: Vec<String>,
}

impl WorkspaceRelativePath {
    fn parse(value: &str) -> Result<Self, SourcePreviewUnsupportedReason> {
        if value.is_empty()
            || value.len() > MAX_RELATIVE_PATH_BYTES
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains('\0')
            // Use one portable path grammar at the mobile seam. Backslashes
            // are rejected instead of acquiring host-dependent semantics.
            || value.contains('\\')
            // Reject Windows drive paths and alternate data streams.
            || value.contains(':')
        {
            return Err(SourcePreviewUnsupportedReason::InvalidRelativePath);
        }

        let components = value.split('/').map(str::to_string).collect::<Vec<_>>();
        if components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.chars().any(char::is_control)
        }) {
            return Err(SourcePreviewUnsupportedReason::InvalidRelativePath);
        }

        Ok(Self {
            normalized: components.join("/"),
            components,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedSourcePreview {
    thread_key: ThreadKey,
    workspace_root: String,
    relative_path: WorkspaceRelativePath,
}

fn prepare_source_preview(
    client: &MobileClient,
    thread_key: &ThreadKey,
    relative_path: &str,
) -> Result<PreparedSourcePreview, SourcePreviewUnsupportedReason> {
    let relative_path = WorkspaceRelativePath::parse(relative_path)?;
    let snapshot = client
        .snapshot_thread(thread_key)
        .map_err(|_| SourcePreviewUnsupportedReason::ThreadUnavailable)?;
    let cwd = snapshot
        .info
        .cwd
        .as_deref()
        .and_then(crate::remote_path::normalize_thread_cwd)
        .filter(|cwd| remote_path_is_absolute(cwd))
        .ok_or(SourcePreviewUnsupportedReason::WorkspaceUnavailable)?;

    Ok(PreparedSourcePreview {
        thread_key: thread_key.clone(),
        workspace_root: cwd,
        relative_path,
    })
}

fn remote_path_is_absolute(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Resolve the authoritative workspace from Rust thread state and fail closed
/// until a host adapter can enforce the same root throughout lookup and read.
///
/// This future does not detach work into `spawn_blocking`; when a safe remote
/// adapter is added its I/O future can be cancelled by dropping this call.
pub async fn source_preview_for_thread(
    client: &MobileClient,
    thread_key: ThreadKey,
    relative_path: String,
) -> SourcePreviewResult {
    tokio::task::yield_now().await;
    let display_path = bounded_display_path(&relative_path);
    let prepared = match prepare_source_preview(client, &thread_key, &relative_path) {
        Ok(prepared) => prepared,
        Err(reason) => {
            return SourcePreviewResult::Unsupported {
                thread_key,
                relative_path: display_path,
                reason,
                byte_length: None,
                truncated: false,
            };
        }
    };

    // Deliberately do not use Codex `fs/readFile` or OneOffCommandExec here:
    // neither binds the request atomically to `prepared.workspace_root`.
    let _authoritative_root = &prepared.workspace_root;
    SourcePreviewResult::Unsupported {
        thread_key: prepared.thread_key,
        relative_path: prepared.relative_path.normalized,
        reason: SourcePreviewUnsupportedReason::CapabilityUnavailable,
        byte_length: None,
        truncated: false,
    }
}

pub async fn diff_review_for_thread(
    client: &MobileClient,
    thread_key: ThreadKey,
) -> DiffReviewResult {
    tokio::task::yield_now().await;
    let snapshot = match client.snapshot_thread(&thread_key) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            return DiffReviewResult::Unsupported {
                thread_key,
                reason: DiffReviewUnavailableReason::ThreadUnavailable,
            };
        }
    };

    let mut inputs = Vec::new();
    for item in &snapshot.items {
        match &item.content {
            HydratedConversationItemContent::FileChange(data) => {
                for change in &data.changes {
                    if !change.diff.trim().is_empty() {
                        inputs.push(DiffInput {
                            path_hint: Some(change.path.as_str()),
                            patch: change.diff.as_str(),
                        });
                    }
                }
            }
            HydratedConversationItemContent::TurnDiff(data) if !data.diff.trim().is_empty() => {
                inputs.push(DiffInput {
                    path_hint: None,
                    patch: data.diff.as_str(),
                });
            }
            _ => {}
        }
    }

    if inputs.is_empty() {
        return DiffReviewResult::Empty { thread_key };
    }

    DiffReviewResult::Ready {
        review: normalize_trusted_diffs(thread_key, &inputs),
    }
}

#[derive(Clone, Copy)]
struct DiffInput<'a> {
    path_hint: Option<&'a str>,
    patch: &'a str,
}

fn normalize_trusted_diffs(thread_key: ThreadKey, inputs: &[DiffInput<'_>]) -> DiffReview {
    let source_byte_length = inputs.iter().fold(0_u64, |total, input| {
        total.saturating_add(input.patch.len() as u64)
    });
    let mut remaining = MAX_DIFF_REVIEW_BYTES;
    let mut truncated = source_byte_length > MAX_DIFF_REVIEW_BYTES as u64;
    let mut files = Vec::new();
    let mut rows_seen = 0_usize;
    let mut occurrence = 0_usize;

    for input in inputs {
        if remaining == 0 || files.len() >= MAX_DIFF_FILES || rows_seen >= MAX_DIFF_ROWS {
            truncated = true;
            break;
        }
        let bounded = utf8_prefix(input.patch, remaining);
        remaining = remaining.saturating_sub(bounded.len());
        if bounded.len() < input.patch.len() {
            truncated = true;
        }

        for chunk in split_diff_chunks(bounded) {
            if files.len() >= MAX_DIFF_FILES || rows_seen >= MAX_DIFF_ROWS {
                truncated = true;
                break;
            }
            if chunk.trim().is_empty() {
                continue;
            }
            let (file, file_rows, file_truncated) = parse_diff_file(
                &thread_key,
                input.path_hint,
                chunk,
                occurrence,
                MAX_DIFF_ROWS.saturating_sub(rows_seen),
            );
            occurrence += 1;
            rows_seen = rows_seen.saturating_add(file_rows);
            truncated |= file_truncated;
            merge_diff_file(&mut files, file);
        }
    }

    let additions = files
        .iter()
        .fold(0_u32, |sum, file| sum.saturating_add(file.additions));
    let deletions = files
        .iter()
        .fold(0_u32, |sum, file| sum.saturating_add(file.deletions));

    DiffReview {
        thread_key,
        files,
        additions,
        deletions,
        source_byte_length,
        truncated,
    }
}

fn split_diff_chunks(patch: &str) -> Vec<&str> {
    let mut starts = patch
        .match_indices("diff --git ")
        .filter_map(|(index, _)| {
            if index == 0 || patch.as_bytes().get(index.wrapping_sub(1)) == Some(&b'\n') {
                Some(index)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if starts.is_empty() {
        return vec![patch];
    }
    if starts[0] != 0 && !patch[..starts[0]].trim().is_empty() {
        starts.insert(0, 0);
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(patch.len());
            &patch[*start..end]
        })
        .collect()
}

#[derive(Debug)]
struct ParsedHunkHeader {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

fn parse_hunk_header(line: &str) -> Option<ParsedHunkHeader> {
    let range_end = line.get(2..)?.find(" @@")? + 2;
    let ranges = line.get(2..range_end)?.trim();
    let mut parts = ranges.split_whitespace();
    let (old_start, old_count) = parse_hunk_range(parts.next()?, '-')?;
    let (new_start, new_count) = parse_hunk_range(parts.next()?, '+')?;
    Some(ParsedHunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

fn parse_hunk_range(value: &str, prefix: char) -> Option<(u32, u32)> {
    let value = value.strip_prefix(prefix)?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn parse_diff_file(
    thread_key: &ThreadKey,
    path_hint: Option<&str>,
    patch: &str,
    occurrence: usize,
    row_budget: usize,
) -> (DiffReviewFile, usize, bool) {
    let mut old_path = None;
    let mut new_path = None;
    let mut rename_from = None;
    let mut rename_to = None;
    let mut marked_added = false;
    let mut marked_deleted = false;
    let mut marked_binary = false;
    let mut hunks = Vec::new();
    let mut current_hunk: Option<(DiffReviewHunk, u32, u32)> = None;
    let mut additions = 0_u32;
    let mut deletions = 0_u32;
    let mut row_count = 0_usize;
    let mut truncated = false;

    for line in patch.lines() {
        if let Some(header) = parse_hunk_header(line) {
            if let Some((hunk, _, _)) = current_hunk.take() {
                hunks.push(hunk);
            }
            let hunk_index = hunks.len();
            current_hunk = Some((
                DiffReviewHunk {
                    id: stable_id(&format!(
                        "{}\0{}\0hunk\0{}\0{}\0{}\0{}",
                        thread_key.server_id,
                        thread_key.thread_id,
                        occurrence,
                        hunk_index,
                        header.old_start,
                        header.new_start
                    )),
                    header: line.to_string(),
                    old_start: header.old_start,
                    old_count: header.old_count,
                    new_start: header.new_start,
                    new_count: header.new_count,
                    rows: Vec::new(),
                },
                header.old_start,
                header.new_start,
            ));
            continue;
        }

        if let Some((hunk, old_line, new_line)) = current_hunk.as_mut() {
            if row_count >= row_budget {
                truncated = true;
                continue;
            }
            let (kind, old_number, new_number, text) = if let Some(text) = line.strip_prefix('+') {
                let number = *new_line;
                *new_line = new_line.saturating_add(1);
                additions = additions.saturating_add(1);
                (DiffReviewRowKind::Addition, None, Some(number), text)
            } else if let Some(text) = line.strip_prefix('-') {
                let number = *old_line;
                *old_line = old_line.saturating_add(1);
                deletions = deletions.saturating_add(1);
                (DiffReviewRowKind::Deletion, Some(number), None, text)
            } else if let Some(text) = line.strip_prefix(' ') {
                let old_number = *old_line;
                let new_number = *new_line;
                *old_line = old_line.saturating_add(1);
                *new_line = new_line.saturating_add(1);
                (
                    DiffReviewRowKind::Context,
                    Some(old_number),
                    Some(new_number),
                    text,
                )
            } else if line.starts_with("\\ No newline at end of file") {
                (DiffReviewRowKind::NoNewlineMarker, None, None, line)
            } else {
                (DiffReviewRowKind::Metadata, None, None, line)
            };
            let row_index = hunk.rows.len();
            hunk.rows.push(DiffReviewRow {
                id: stable_id(&format!(
                    "{}\0row\0{}\0{:?}\0{}\0{}",
                    hunk.id,
                    row_index,
                    kind,
                    old_number.unwrap_or(0),
                    new_number.unwrap_or(0)
                )),
                kind,
                old_line_number: old_number,
                new_line_number: new_number,
                text: text.to_string(),
            });
            row_count += 1;
            continue;
        }

        if let Some(paths) = line.strip_prefix("diff --git ") {
            if let Some((old, new)) = parse_diff_git_paths(paths) {
                old_path = normalize_diff_path(&old);
                new_path = normalize_diff_path(&new);
            }
        } else if let Some(path) = line.strip_prefix("--- ") {
            old_path = normalize_diff_path(path_field(path));
        } else if let Some(path) = line.strip_prefix("+++ ") {
            new_path = normalize_diff_path(path_field(path));
        } else if let Some(path) = line.strip_prefix("rename from ") {
            rename_from = normalize_diff_path(path);
        } else if let Some(path) = line.strip_prefix("rename to ") {
            rename_to = normalize_diff_path(path);
        } else if line.starts_with("new file mode ") {
            marked_added = true;
        } else if line.starts_with("deleted file mode ") {
            marked_deleted = true;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            marked_binary = true;
        }
    }

    if let Some((hunk, _, _)) = current_hunk {
        hunks.push(hunk);
    }
    if rename_from.is_some() {
        old_path = rename_from;
    }
    if rename_to.is_some() {
        new_path = rename_to;
    }

    let hint = path_hint
        .filter(|path| !path.trim().is_empty())
        .map(|path| bounded_display_path(path.trim()));
    let display_path = new_path
        .as_ref()
        .or(old_path.as_ref())
        .or(hint.as_ref())
        .cloned()
        .unwrap_or_else(|| format!("Patch {}", occurrence + 1));
    let change_kind = if marked_binary {
        DiffFileChangeKind::Binary
    } else if old_path.is_none() && new_path.is_some() || marked_added {
        DiffFileChangeKind::Added
    } else if new_path.is_none() && old_path.is_some() || marked_deleted {
        DiffFileChangeKind::Deleted
    } else if old_path.is_some() && new_path.is_some() && old_path != new_path {
        DiffFileChangeKind::Renamed
    } else if !hunks.is_empty() {
        DiffFileChangeKind::Modified
    } else {
        DiffFileChangeKind::Unknown
    };
    let id = stable_id(&format!(
        "{}\0{}\0file\0{}\0{}",
        thread_key.server_id, thread_key.thread_id, display_path, occurrence
    ));

    (
        DiffReviewFile {
            id,
            display_path,
            old_path,
            new_path,
            change_kind,
            additions,
            deletions,
            hunks,
            raw_patch: patch.to_string(),
        },
        row_count,
        truncated,
    )
}

fn parse_diff_git_paths(value: &str) -> Option<(String, String)> {
    // Git's common unquoted form is `a/path b/path`. Splitting at the final
    // ` b/` preserves spaces in the old path. Quoted paths are retained as a
    // display-safe best effort; authoritative source reads never consume
    // paths parsed from patches.
    if let Some(split) = value.rfind(" b/") {
        return Some((value[..split].to_string(), value[split + 1..].to_string()));
    }
    let fields = shlex::split(value)?;
    (fields.len() == 2).then(|| (fields[0].clone(), fields[1].clone()))
}

fn path_field(value: &str) -> &str {
    value.split('\t').next().unwrap_or(value).trim()
}

fn normalize_diff_path(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"');
    if value.is_empty() || value == "/dev/null" {
        return None;
    }
    let value = value
        .strip_prefix("a/")
        .or_else(|| value.strip_prefix("b/"))
        .unwrap_or(value);
    Some(bounded_display_path(value))
}

fn merge_diff_file(files: &mut Vec<DiffReviewFile>, mut incoming: DiffReviewFile) {
    let Some(existing) = files
        .iter_mut()
        .find(|file| file.display_path == incoming.display_path)
    else {
        files.push(incoming);
        return;
    };

    existing.additions = existing.additions.saturating_add(incoming.additions);
    existing.deletions = existing.deletions.saturating_add(incoming.deletions);
    existing.change_kind = merge_change_kind(existing.change_kind, incoming.change_kind);
    if existing.old_path.is_none() {
        existing.old_path = incoming.old_path.take();
    }
    if incoming.new_path.is_some() {
        existing.new_path = incoming.new_path.take();
    }
    existing.hunks.append(&mut incoming.hunks);
    // Chunks already retain their trailing newline. Avoid inserting merge
    // separators so the aggregate copy/export payload cannot exceed the
    // global input-byte cap.
    existing.raw_patch.push_str(&incoming.raw_patch);
}

fn merge_change_kind(
    existing: DiffFileChangeKind,
    incoming: DiffFileChangeKind,
) -> DiffFileChangeKind {
    use DiffFileChangeKind::{Added, Binary, Deleted, Modified, Renamed, Unknown};
    match (existing, incoming) {
        (Binary, _) | (_, Binary) => Binary,
        (kind, Unknown) | (Unknown, kind) => kind,
        (left, right) if left == right => left,
        (Added, Modified) | (Modified, Added) => Added,
        (Deleted, Modified) | (Modified, Deleted) => Deleted,
        (Renamed, Modified) | (Modified, Renamed) => Renamed,
        _ => Modified,
    }
}

fn stable_id(value: &str) -> String {
    let digest = Sha1::digest(value.as_bytes());
    hex::encode(&digest[..12])
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn bounded_display_path(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect::<String>();
    utf8_prefix(&sanitized, MAX_DISPLAY_PATH_BYTES).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_uniffi::{HydratedConversationItem, HydratedTurnDiffData};
    use crate::store::ThreadSnapshot;
    use crate::types::{ThreadInfo, ThreadSummaryStatus};
    #[cfg(unix)]
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::fd::{AsRawFd, FromRawFd};
    #[cfg(unix)]
    use std::path::Path;

    fn key() -> ThreadKey {
        ThreadKey {
            server_id: "server".to_string(),
            thread_id: "thread".to_string(),
        }
    }

    fn thread_info(cwd: Option<String>) -> ThreadInfo {
        ThreadInfo {
            id: "thread".to_string(),
            title: None,
            model: None,
            status: ThreadSummaryStatus::Idle,
            preview: None,
            cwd,
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn client_with_thread(cwd: &str) -> MobileClient {
        let client = MobileClient::new();
        client
            .app_store
            .upsert_thread_snapshot(ThreadSnapshot::from_info(
                "server",
                thread_info(Some(cwd.into())),
            ));
        client
    }

    #[test]
    fn relative_path_rejects_absolute_traversal_and_mixed_separator_inputs() {
        for invalid in [
            "",
            "/etc/passwd",
            "../secret",
            "src/../secret",
            "./src/lib.rs",
            "src//lib.rs",
            "src\\..\\secret",
            "src\\nested/file.rs",
            "C:/Users/name/secret",
            "C:\\Users\\name\\secret",
            "file.txt:stream",
            "src/\u{0}secret",
        ] {
            assert_eq!(
                WorkspaceRelativePath::parse(invalid),
                Err(SourcePreviewUnsupportedReason::InvalidRelativePath),
                "input should be rejected: {invalid:?}"
            );
        }
        assert_eq!(
            WorkspaceRelativePath::parse("src/lib.rs")
                .expect("valid")
                .normalized,
            "src/lib.rs"
        );
    }

    #[test]
    fn source_request_resolves_cwd_only_from_the_thread_snapshot() {
        let client = client_with_thread("/authoritative/worktree");
        let request = prepare_source_preview(&client, &key(), "src/lib.rs").expect("prepared");
        assert_eq!(request.workspace_root, "/authoritative/worktree");
        assert_eq!(request.relative_path.normalized, "src/lib.rs");
    }

    #[tokio::test]
    async fn source_preview_fails_closed_without_safe_host_capability() {
        let client = client_with_thread("/authoritative/worktree");
        let result = source_preview_for_thread(&client, key(), "src/lib.rs".to_string()).await;
        assert_eq!(
            result,
            SourcePreviewResult::Unsupported {
                thread_key: key(),
                relative_path: "src/lib.rs".to_string(),
                reason: SourcePreviewUnsupportedReason::CapabilityUnavailable,
                byte_length: None,
                truncated: false,
            }
        );
    }

    #[tokio::test]
    async fn source_preview_reports_unknown_thread_without_touching_a_filesystem() {
        let client = MobileClient::new();
        let result = source_preview_for_thread(&client, key(), "src/lib.rs".to_string()).await;
        assert!(matches!(
            result,
            SourcePreviewResult::Unsupported {
                reason: SourcePreviewUnsupportedReason::ThreadUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn parses_added_deleted_renamed_and_binary_files() {
        let patch = r#"diff --git a/new.rs b/new.rs
new file mode 100644
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+one
+two
diff --git a/old.rs b/old.rs
deleted file mode 100644
--- a/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-gone
diff --git a/before.rs b/after.rs
similarity index 100%
rename from before.rs
rename to after.rs
diff --git a/picture.png b/picture.png
Binary files a/picture.png and b/picture.png differ
"#;
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                path_hint: None,
                patch,
            }],
        );
        assert_eq!(review.files.len(), 4);
        assert_eq!(review.files[0].change_kind, DiffFileChangeKind::Added);
        assert_eq!(review.files[0].additions, 2);
        assert_eq!(review.files[1].change_kind, DiffFileChangeKind::Deleted);
        assert_eq!(review.files[1].deletions, 1);
        assert_eq!(review.files[2].change_kind, DiffFileChangeKind::Renamed);
        assert_eq!(review.files[2].display_path, "after.rs");
        assert_eq!(review.files[3].change_kind, DiffFileChangeKind::Binary);
        assert_eq!(review.additions, 2);
        assert_eq!(review.deletions, 1);
    }

    #[test]
    fn hunk_rows_have_stable_ids_and_correct_line_numbers() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +20,3 @@ fn x()\n keep\n-old\n+new\n tail\n";
        let input = [DiffInput {
            path_hint: None,
            patch,
        }];
        let first = normalize_trusted_diffs(key(), &input);
        let second = normalize_trusted_diffs(key(), &input);
        assert_eq!(first, second);
        let rows = &first.files[0].hunks[0].rows;
        assert_eq!(
            (rows[0].old_line_number, rows[0].new_line_number),
            (Some(10), Some(20))
        );
        assert_eq!(
            (rows[1].old_line_number, rows[1].new_line_number),
            (Some(11), None)
        );
        assert_eq!(
            (rows[2].old_line_number, rows[2].new_line_number),
            (None, Some(21))
        );
        assert_eq!(
            (rows[3].old_line_number, rows[3].new_line_number),
            (Some(12), Some(22))
        );
    }

    #[test]
    fn quoted_git_paths_with_spaces_keep_file_identity() {
        let patch = "diff --git \"a/old name.rs\" \"b/new name.rs\"\n--- \"a/old name.rs\"\n+++ \"b/new name.rs\"\n@@ -1 +1 @@\n-old\n+new\n";
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                path_hint: None,
                patch,
            }],
        );
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(review.files[0].new_path.as_deref(), Some("new name.rs"));
        assert_eq!(review.files[0].display_path, "new name.rs");
    }

    #[test]
    fn malformed_diff_is_bounded_and_never_fabricates_line_numbers() {
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                path_hint: Some("odd.patch"),
                patch: "not a unified diff\n+or - maybe malformed\n@@ nonsense @@\n",
            }],
        );
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].display_path, "odd.patch");
        assert_eq!(review.files[0].change_kind, DiffFileChangeKind::Unknown);
        assert!(review.files[0].hunks.is_empty());
        assert_eq!(review.additions, 0);
        assert_eq!(review.deletions, 0);
    }

    #[test]
    fn large_diff_is_utf8_safely_truncated_to_the_global_cap() {
        let mut patch = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,999999 @@\n",
        );
        while patch.len() <= MAX_DIFF_REVIEW_BYTES + 1024 {
            patch.push_str("+ocean-\u{1f41f}\n");
        }
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                path_hint: None,
                patch: &patch,
            }],
        );
        assert!(review.truncated);
        assert_eq!(review.source_byte_length, patch.len() as u64);
        assert!(
            review
                .files
                .iter()
                .map(|file| file.raw_patch.len())
                .sum::<usize>()
                <= MAX_DIFF_REVIEW_BYTES
        );
    }

    #[tokio::test]
    async fn diff_review_reads_only_canonical_store_items() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.snapshot_thread(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "turn-diff".to_string(),
            content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                diff: "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            }),
            source_turn_id: Some("turn".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let result = diff_review_for_thread(&client, key()).await;
        let DiffReviewResult::Ready { review } = result else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.additions, 1);
        assert_eq!(review.deletions, 1);
    }

    /// Test-only adapter proving the host-side contract. It walks every path
    /// segment relative to an already-open root descriptor and sets
    /// `O_NOFOLLOW` at each hop, so traversal, symlink swaps, and non-files do
    /// not turn into broader reads. Production mobile code does not use this
    /// adapter; Remora Link must provide equivalent enforcement remotely.
    #[cfg(unix)]
    fn open_confined(root: &Path, path: &WorkspaceRelativePath) -> std::io::Result<File> {
        let root = File::open(root)?;
        let mut directory = root;
        for (index, component) in path.components.iter().enumerate() {
            let name = CString::new(component.as_bytes()).expect("validated NUL-free component");
            let is_last = index + 1 == path.components.len();
            let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            if !is_last {
                flags |= libc::O_DIRECTORY;
            } else {
                flags |= libc::O_NONBLOCK;
            }
            // SAFETY: directory is an open descriptor, name is NUL-terminated,
            // and openat returns a new owned descriptor on success.
            let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `fd` is freshly returned and ownership transfers once.
            let opened = unsafe { File::from_raw_fd(fd) };
            if is_last {
                if !opened.metadata()?.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "source preview target is not a regular file",
                    ));
                }
                return Ok(opened);
            }
            directory = opened;
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty relative path",
        ))
    }

    fn classify_open_file(
        thread_key: ThreadKey,
        relative_path: &str,
        mut file: File,
    ) -> SourcePreviewResult {
        let byte_length = file.metadata().expect("metadata").len();
        let truncated = byte_length > MAX_SOURCE_PREVIEW_BYTES as u64;
        let mut bytes = Vec::with_capacity(
            usize::try_from(byte_length.min(MAX_SOURCE_PREVIEW_BYTES as u64)).unwrap_or_default(),
        );
        Read::by_ref(&mut file)
            .take(MAX_SOURCE_PREVIEW_BYTES as u64)
            .read_to_end(&mut bytes)
            .expect("bounded read");

        if let Some(mime_type) = image_mime_type(&bytes) {
            return SourcePreviewResult::Image {
                thread_key,
                relative_path: relative_path.to_string(),
                mime_type: mime_type.to_string(),
                bytes: if truncated { Vec::new() } else { bytes },
                byte_length,
                truncated,
            };
        }
        if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
            return SourcePreviewResult::Unsupported {
                thread_key,
                relative_path: relative_path.to_string(),
                reason: SourcePreviewUnsupportedReason::UnsupportedEncoding,
                byte_length: Some(byte_length),
                truncated,
            };
        }
        if bytes.contains(&0) {
            return SourcePreviewResult::Binary {
                thread_key,
                relative_path: relative_path.to_string(),
                byte_length,
                truncated,
            };
        }

        match decode_text_prefix(&bytes, truncated) {
            Some(text) => SourcePreviewResult::Text {
                thread_key,
                relative_path: relative_path.to_string(),
                text,
                byte_length,
                truncated,
            },
            None => SourcePreviewResult::Binary {
                thread_key,
                relative_path: relative_path.to_string(),
                byte_length,
                truncated,
            },
        }
    }

    fn decode_text_prefix(bytes: &[u8], truncated: bool) -> Option<String> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Some(text.to_string()),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .ok()
                    .map(str::to_string)
            }
            Err(_) => None,
        }
    }

    fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some("image/jpeg")
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some("image/gif")
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some("image/webp")
        } else {
            None
        }
    }

    #[test]
    #[cfg(unix)]
    fn confined_reader_rejects_symlink_escape_and_non_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(root.join("directory")).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("secret"), "secret").expect("secret");
        symlink(&outside, root.join("escape")).expect("symlink");

        let escape = WorkspaceRelativePath::parse("escape/secret").expect("valid syntax");
        assert!(open_confined(&root, &escape).is_err());
        let directory = WorkspaceRelativePath::parse("directory").expect("valid syntax");
        assert!(open_confined(&root, &directory).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn opened_file_is_pinned_across_a_path_swap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("file"), "inside").expect("inside");
        std::fs::write(outside.join("file"), "outside").expect("outside file");
        let relative = WorkspaceRelativePath::parse("file").expect("path");
        let mut opened = open_confined(&root, &relative).expect("confined open");

        std::fs::rename(root.join("file"), root.join("original")).expect("rename");
        symlink(outside.join("file"), root.join("file")).expect("swap to symlink");
        let mut contents = String::new();
        opened
            .read_to_string(&mut contents)
            .expect("read pinned fd");
        assert_eq!(contents, "inside");
        assert!(open_confined(&root, &relative).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn confined_preview_classifies_text_image_binary_encoding_and_truncation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(root.join("text"), "hello \u{1f30a}").expect("text");
        std::fs::write(root.join("image"), b"\x89PNG\r\n\x1a\nrest").expect("image");
        std::fs::write(root.join("binary"), b"abc\0def").expect("binary");
        std::fs::write(root.join("utf16"), [0xff, 0xfe, b'h', 0]).expect("utf16");
        let mut large = File::create(root.join("large")).expect("large");
        large
            .write_all(&vec![b'x'; MAX_SOURCE_PREVIEW_BYTES + 1])
            .expect("write large");

        let preview = |name: &str| {
            let relative = WorkspaceRelativePath::parse(name).expect("path");
            let file = open_confined(root, &relative).expect("open");
            classify_open_file(key(), name, file)
        };
        assert!(matches!(
            preview("text"),
            SourcePreviewResult::Text {
                truncated: false,
                ..
            }
        ));
        assert!(
            matches!(preview("image"), SourcePreviewResult::Image { mime_type, .. } if mime_type == "image/png")
        );
        assert!(matches!(
            preview("binary"),
            SourcePreviewResult::Binary { .. }
        ));
        assert!(matches!(
            preview("utf16"),
            SourcePreviewResult::Unsupported {
                reason: SourcePreviewUnsupportedReason::UnsupportedEncoding,
                ..
            }
        ));
        assert!(matches!(
            preview("large"),
            SourcePreviewResult::Text {
                text,
                byte_length,
                truncated: true,
                ..
            } if text.len() == MAX_SOURCE_PREVIEW_BYTES && byte_length == (MAX_SOURCE_PREVIEW_BYTES + 1) as u64
        ));
    }
}

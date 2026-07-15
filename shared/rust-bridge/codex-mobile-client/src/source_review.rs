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

use crate::conversation_uniffi::HydratedConversationItemContent;
use crate::types::ThreadKey;
use crate::MobileClient;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};

/// Maximum source bytes returned through UniFFI.
pub const MAX_SOURCE_PREVIEW_BYTES: usize = 1024 * 1024;

/// Maximum trusted diff bytes shaped in one request.
pub const MAX_DIFF_REVIEW_BYTES: usize = 1024 * 1024;

const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_WORKSPACE_ROOT_BYTES: usize = 4096;
const MAX_REMOTE_PATH_BYTES: usize = MAX_RELATIVE_PATH_BYTES + MAX_WORKSPACE_ROOT_BYTES + 1;
const MAX_DISPLAY_PATH_BYTES: usize = 512;
const MAX_DIFF_FILES: usize = 2048;
const MAX_DIFF_HUNKS: usize = 10_000;
const MAX_DIFF_ROWS: usize = 50_000;
const MAX_DIFF_SOURCE_ITEMS: usize = 4096;
const MAX_DIFF_INPUTS: usize = 4096;
const MAX_SOURCE_TURN_ID_BYTES: usize = 512;
const MAX_DIFF_PRESENCE_SCAN_BYTES: usize = 4096;

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
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiffReviewFile {
    pub id: String,
    pub display_path: String,
    /// Canonical portable path that can be passed to `source_preview`.
    /// This is absent when trusted diff metadata is absolute but outside the
    /// authoritative workspace, malformed, or not portable across hosts.
    pub relative_path: Option<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub change_kind: DiffFileChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: Vec<DiffReviewHunk>,
    /// Retained for explicit copy/export. For upstream add/delete items this
    /// is the authoritative raw file content rather than a synthesized patch.
    /// The aggregate payload is bounded by `MAX_DIFF_REVIEW_BYTES`.
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
    let cwd = client
        .app_store
        .project_thread(thread_key, |snapshot| {
            snapshot
                .info
                .cwd
                .as_deref()
                .and_then(normalized_workspace_root)
        })
        .ok_or(SourcePreviewUnsupportedReason::ThreadUnavailable)?;
    let cwd = cwd.ok_or(SourcePreviewUnsupportedReason::WorkspaceUnavailable)?;

    Ok(PreparedSourcePreview {
        thread_key: thread_key.clone(),
        workspace_root: cwd,
        relative_path,
    })
}

fn normalized_workspace_root(value: &str) -> Option<String> {
    (value.len() <= MAX_WORKSPACE_ROOT_BYTES)
        .then_some(value)
        .and_then(crate::remote_path::normalize_thread_cwd)
        .filter(|cwd| remote_path_is_absolute(cwd))
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
/// The unavailable path performs no host I/O and completes after one yield.
/// Enabling a remote adapter also requires an explicit request-cancellation
/// protocol: current UniFFI platform wrappers do not propagate native task
/// cancellation into an already-started Rust future.
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
    let projection = match client
        .app_store
        .project_thread(&thread_key, project_diff_inputs)
    {
        Some(projection) => projection,
        None => {
            return DiffReviewResult::Unsupported {
                thread_key,
                reason: DiffReviewUnavailableReason::ThreadUnavailable,
            };
        }
    };

    if projection.inputs.is_empty() && !projection.truncated {
        return DiffReviewResult::Empty { thread_key };
    }
    let inputs = projection
        .inputs
        .iter()
        .map(OwnedDiffInput::borrowed)
        .collect::<Vec<_>>();
    let mut review = normalize_trusted_diffs(thread_key, &inputs);
    review.truncated |= projection.truncated;

    DiffReviewResult::Ready { review }
}

fn project_diff_inputs(snapshot: &crate::store::ThreadSnapshot) -> DiffInputProjection {
    let workspace_root = snapshot
        .info
        .cwd
        .as_deref()
        .and_then(normalized_workspace_root);
    let first_item = snapshot.items.len().saturating_sub(MAX_DIFF_SOURCE_ITEMS);
    let items = &snapshot.items[first_item..];
    let mut projection = DiffInputProjection {
        inputs: Vec::new(),
        truncated: first_item > 0,
    };

    // A TurnDiff is the cumulative authoritative aggregate for its turn. The
    // same changes also exist as individual FileChange items, so collecting
    // both would duplicate rows and counts.
    let aggregate_turns = items
        .iter()
        .filter_map(|item| match &item.content {
            HydratedConversationItemContent::TurnDiff(data)
                if bounded_has_non_whitespace(&data.diff) =>
            {
                bounded_source_turn_id(item.source_turn_id.as_deref())
            }
            _ => None,
        })
        .collect::<HashSet<_>>();

    let mut remaining_bytes = MAX_DIFF_REVIEW_BYTES;
    let mut source_records_seen = 0_usize;
    'items: for item in items {
        match &item.content {
            HydratedConversationItemContent::FileChange(data) => {
                // Only completed fallback items describe applied workspace
                // state. Pending/in-progress are proposals, while failed and
                // declined patches must never appear as committed review data.
                if data.status != crate::types::AppOperationStatus::Completed {
                    continue;
                }
                let has_aggregate = bounded_source_turn_id(item.source_turn_id.as_deref())
                    .is_some_and(|turn_id| aggregate_turns.contains(turn_id));
                for change in &data.changes {
                    if source_records_seen >= MAX_DIFF_INPUTS {
                        projection.truncated = true;
                        break 'items;
                    }
                    source_records_seen += 1;
                    let path_hint =
                        normalize_file_change_hint(workspace_root.as_deref(), &change.path);
                    let has_move = change.move_path.is_some();
                    let new_path_hint = change.move_path.as_deref().and_then(|path| {
                        normalize_file_change_hint(workspace_root.as_deref(), path)
                    });
                    let has_unconfined_path =
                        path_hint.is_none() || (has_move && new_path_hint.is_none());
                    if has_unconfined_path {
                        // The aggregate can be unrelated or omit empty files.
                        // Always retain a typed entry, without exposing the
                        // unconfined FileChange payload.
                        if !projection.push(
                            &mut remaining_bytes,
                            path_hint,
                            new_path_hint,
                            Some(bounded_private_identity(&change.path)),
                            workspace_root.clone(),
                            &change.diff,
                            DiffInputKind::RedactedPath,
                        ) {
                            break 'items;
                        }
                        continue;
                    }
                    if has_aggregate {
                        // TurnDiff includes applied content changes but core
                        // intentionally omits pure renames. Preserve only the
                        // typed move identity here, without duplicating hunks.
                        if has_move
                            && !projection.push(
                                &mut remaining_bytes,
                                path_hint,
                                new_path_hint,
                                Some(bounded_private_identity(&change.path)),
                                workspace_root.clone(),
                                "",
                                DiffInputKind::RenameMetadata,
                            )
                        {
                            break 'items;
                        }
                        continue;
                    }
                    let kind = if has_move && !bounded_has_non_whitespace(&change.diff) {
                        DiffInputKind::RenameMetadata
                    } else {
                        DiffInputKind::from_hydrated_kind(&change.kind)
                    };
                    if (kind.accepts_empty_content() || bounded_has_non_whitespace(&change.diff))
                        && !projection.push(
                            &mut remaining_bytes,
                            path_hint,
                            new_path_hint,
                            Some(bounded_private_identity(&change.path)),
                            workspace_root.clone(),
                            &change.diff,
                            kind,
                        )
                    {
                        break 'items;
                    }
                }
            }
            HydratedConversationItemContent::TurnDiff(data)
                if bounded_has_non_whitespace(&data.diff) =>
            {
                if !projection.push(
                    &mut remaining_bytes,
                    None,
                    None,
                    None,
                    workspace_root.clone(),
                    &data.diff,
                    DiffInputKind::Unified,
                ) {
                    break 'items;
                }
            }
            _ => {}
        }
    }

    projection
}

fn bounded_source_turn_id(value: Option<&str>) -> Option<&str> {
    value.filter(|value| value.len() <= MAX_SOURCE_TURN_ID_BYTES)
}

fn bounded_private_identity(value: &str) -> String {
    stable_id(&format!(
        "{}\0{}",
        value.len(),
        utf8_prefix(value, MAX_RELATIVE_PATH_BYTES)
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffInputKind {
    Unified,
    AddedContent,
    DeletedContent,
    UpdatedPatch,
    RenameMetadata,
    RedactedPath,
}

impl DiffInputKind {
    fn from_hydrated_kind(value: &str) -> Self {
        match value {
            "add" => Self::AddedContent,
            "delete" => Self::DeletedContent,
            "update" => Self::UpdatedPatch,
            _ => Self::Unified,
        }
    }

    fn accepts_empty_content(self) -> bool {
        matches!(
            self,
            Self::AddedContent | Self::DeletedContent | Self::RenameMetadata | Self::RedactedPath
        )
    }
}

#[derive(Clone)]
struct DiffInput<'a> {
    path_hint: Option<String>,
    new_path_hint: Option<String>,
    private_path_identity: Option<String>,
    workspace_root: Option<String>,
    patch: &'a str,
    source_byte_length: u64,
    kind: DiffInputKind,
}

struct OwnedDiffInput {
    path_hint: Option<String>,
    new_path_hint: Option<String>,
    private_path_identity: Option<String>,
    workspace_root: Option<String>,
    patch: String,
    source_byte_length: u64,
    kind: DiffInputKind,
}

impl OwnedDiffInput {
    fn borrowed(&self) -> DiffInput<'_> {
        DiffInput {
            path_hint: self.path_hint.clone(),
            new_path_hint: self.new_path_hint.clone(),
            private_path_identity: self.private_path_identity.clone(),
            workspace_root: self.workspace_root.clone(),
            patch: &self.patch,
            source_byte_length: self.source_byte_length,
            kind: self.kind,
        }
    }
}

struct DiffInputProjection {
    inputs: Vec<OwnedDiffInput>,
    truncated: bool,
}

impl DiffInputProjection {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        remaining_bytes: &mut usize,
        path_hint: Option<String>,
        new_path_hint: Option<String>,
        private_path_identity: Option<String>,
        workspace_root: Option<String>,
        patch: &str,
        kind: DiffInputKind,
    ) -> bool {
        if self.inputs.len() >= MAX_DIFF_INPUTS {
            self.truncated = true;
            return false;
        }
        if matches!(kind, DiffInputKind::RedactedPath) {
            self.inputs.push(OwnedDiffInput {
                path_hint,
                new_path_hint,
                private_path_identity,
                workspace_root,
                patch: String::new(),
                source_byte_length: patch.len() as u64,
                kind,
            });
            return true;
        }
        if *remaining_bytes == 0 && !patch.is_empty() {
            self.truncated = true;
            return false;
        }
        let bounded_prefix = utf8_prefix(patch, *remaining_bytes);
        let bounded_patch = if bounded_prefix.len() < patch.len() {
            self.truncated = true;
            complete_line_prefix(bounded_prefix)
        } else {
            bounded_prefix
        };
        // Account for the entire inspected prefix, including the discarded
        // partial line, so later inputs cannot reuse the global byte budget.
        *remaining_bytes = remaining_bytes.saturating_sub(bounded_prefix.len());
        self.inputs.push(OwnedDiffInput {
            path_hint,
            new_path_hint,
            private_path_identity,
            workspace_root,
            patch: bounded_patch.to_string(),
            source_byte_length: patch.len() as u64,
            kind,
        });
        true
    }
}

fn normalize_trusted_diffs(thread_key: ThreadKey, inputs: &[DiffInput<'_>]) -> DiffReview {
    let source_byte_length = inputs.iter().fold(0_u64, |total, input| {
        total.saturating_add(input.source_byte_length)
    });
    let payload_source_byte_length = inputs
        .iter()
        .filter(|input| !matches!(input.kind, DiffInputKind::RedactedPath))
        .fold(0_u64, |total, input| {
            total.saturating_add(input.source_byte_length)
        });
    let mut remaining = MAX_DIFF_REVIEW_BYTES;
    let mut truncated = payload_source_byte_length > MAX_DIFF_REVIEW_BYTES as u64
        || inputs
            .iter()
            .filter(|input| !matches!(input.kind, DiffInputKind::RedactedPath))
            .any(|input| input.source_byte_length > input.patch.len() as u64);
    let mut files = Vec::new();
    let mut hunks_seen = 0_usize;
    let mut rows_seen = 0_usize;
    let mut occurrence = 0_usize;

    for input in inputs {
        if files.len() >= MAX_DIFF_FILES
            || hunks_seen >= MAX_DIFF_HUNKS
            || rows_seen >= MAX_DIFF_ROWS
            || (remaining == 0
                && !input.patch.is_empty()
                && !matches!(input.kind, DiffInputKind::RedactedPath))
        {
            truncated = true;
            break;
        }
        let bounded_prefix = utf8_prefix(input.patch, remaining);
        let bounded = if bounded_prefix.len() < input.patch.len() {
            complete_line_prefix(bounded_prefix)
        } else {
            bounded_prefix
        };
        remaining = remaining.saturating_sub(bounded.len());
        if bounded_prefix.len() < input.patch.len() {
            truncated = true;
        }

        if matches!(
            input.kind,
            DiffInputKind::AddedContent | DiffInputKind::DeletedContent
        ) {
            let (file, file_rows, file_hunks, file_truncated) = parse_raw_file_content(
                &thread_key,
                input.path_hint.as_deref(),
                input.private_path_identity.as_deref(),
                bounded,
                input.kind,
                occurrence,
                MAX_DIFF_ROWS.saturating_sub(rows_seen),
            );
            occurrence += 1;
            rows_seen = rows_seen.saturating_add(file_rows);
            hunks_seen = hunks_seen.saturating_add(file_hunks);
            truncated |= file_truncated;
            merge_diff_file(&mut files, file);
            continue;
        }

        for chunk in split_diff_chunks(bounded) {
            if files.len() >= MAX_DIFF_FILES
                || hunks_seen >= MAX_DIFF_HUNKS
                || rows_seen >= MAX_DIFF_ROWS
            {
                truncated = true;
                break;
            }
            if chunk.trim().is_empty() && !input.kind.accepts_empty_content() {
                continue;
            }
            let (file, file_rows, file_hunks, file_truncated) = parse_diff_file(
                &thread_key,
                input.path_hint.as_deref(),
                input.new_path_hint.as_deref(),
                input.private_path_identity.as_deref(),
                input.workspace_root.as_deref(),
                chunk,
                input.kind,
                occurrence,
                MAX_DIFF_ROWS.saturating_sub(rows_seen),
                MAX_DIFF_HUNKS.saturating_sub(hunks_seen),
            );
            occurrence += 1;
            rows_seen = rows_seen.saturating_add(file_rows);
            hunks_seen = hunks_seen.saturating_add(file_hunks);
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

fn bounded_has_non_whitespace(value: &str) -> bool {
    !utf8_prefix(value, MAX_DIFF_PRESENCE_SCAN_BYTES)
        .trim()
        .is_empty()
}

fn complete_line_prefix(value: &str) -> &str {
    value
        .rfind('\n')
        .map(|index| &value[..=index])
        .unwrap_or_default()
}

/// Convert trusted server file metadata into the same portable relative path
/// grammar accepted by source preview. Absolute paths are retained only when
/// they are component-boundary descendants of the authoritative workspace.
fn normalize_file_change_hint(workspace_root: Option<&str>, value: &str) -> Option<String> {
    if value.len() > MAX_REMOTE_PATH_BYTES {
        return None;
    }
    let candidate = value.trim();
    if candidate.is_empty() {
        return None;
    }

    let relative = if remote_path_is_absolute(candidate) {
        let root = workspace_root?;
        strip_remote_workspace_root(root, candidate)?
    } else {
        candidate.replace('\\', "/")
    };
    WorkspaceRelativePath::parse(&relative)
        .ok()
        .map(|path| path.normalized)
}

fn strip_remote_workspace_root(root: &str, candidate: &str) -> Option<String> {
    let root_is_windows = crate::remote_path::RemotePath::parse(root).is_windows();
    let candidate_is_windows = crate::remote_path::RemotePath::parse(candidate).is_windows();
    if root_is_windows != candidate_is_windows {
        return None;
    }

    if root_is_windows {
        let root = root.replace('/', "\\");
        let candidate = candidate.replace('/', "\\");
        let root = root.trim_end_matches('\\');
        let prefix = candidate.get(..root.len())?;
        if !prefix.eq_ignore_ascii_case(root) {
            return None;
        }
        let remainder = candidate.get(root.len()..)?;
        if remainder.is_empty() {
            return None;
        }
        let remainder = remainder.strip_prefix('\\')?;
        Some(remainder.replace('\\', "/"))
    } else if root == "/" {
        Some(candidate.strip_prefix('/')?.to_string())
    } else {
        let root = root.trim_end_matches('/');
        let remainder = candidate.strip_prefix(root)?;
        let remainder = remainder.strip_prefix('/')?;
        Some(remainder.to_string())
    }
}

fn split_diff_chunks(patch: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut offset = 0_usize;
    for raw_line in patch.split_inclusive('\n') {
        lines.push((offset, raw_line.trim_end_matches(['\r', '\n'])));
        offset = offset.saturating_add(raw_line.len());
    }
    if offset < patch.len() {
        lines.push((offset, &patch[offset..]));
    }

    let mut starts = lines
        .iter()
        .filter_map(|(offset, line)| {
            (line.starts_with("diff --git ")
                || line.starts_with("diff --cc ")
                || line.starts_with("diff --combined "))
            .then_some(*offset)
        })
        .collect::<Vec<_>>();

    if starts.is_empty() {
        let mut old_remaining = 0_u32;
        let mut new_remaining = 0_u32;
        let mut in_hunk = false;
        for (index, (offset, line)) in lines.iter().enumerate() {
            if let Some(header) = parse_hunk_header(line) {
                old_remaining = header.old_count;
                new_remaining = header.new_count;
                in_hunk = true;
                continue;
            }
            if in_hunk {
                if old_remaining == 0 && new_remaining == 0 {
                    if line.starts_with("\\ No newline at end of file") {
                        continue;
                    }
                    in_hunk = false;
                } else {
                    consume_declared_hunk_line(line, &mut old_remaining, &mut new_remaining);
                    continue;
                }
            }
            if line.starts_with("--- ")
                && lines
                    .get(index + 1)
                    .is_some_and(|(_, next)| next.starts_with("+++ "))
            {
                starts.push(*offset);
            }
        }
    }

    if starts.is_empty() {
        return vec![patch];
    }
    if starts[0] != 0 && !patch[..starts[0]].trim().is_empty() {
        starts[0] = 0;
    }
    starts.sort_unstable();
    starts.dedup();
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(patch.len());
            &patch[*start..end]
        })
        .collect()
}

fn consume_declared_hunk_line(line: &str, old_remaining: &mut u32, new_remaining: &mut u32) {
    if line.starts_with('+') && *new_remaining > 0 {
        *new_remaining -= 1;
    } else if line.starts_with('-') && *old_remaining > 0 {
        *old_remaining -= 1;
    } else if line.starts_with(' ') && *old_remaining > 0 && *new_remaining > 0 {
        *old_remaining -= 1;
        *new_remaining -= 1;
    }
}

#[derive(Debug)]
struct ParsedHunkHeader {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

struct InProgressHunk {
    hunk: DiffReviewHunk,
    old_line: u32,
    new_line: u32,
    old_remaining: u32,
    new_remaining: u32,
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

fn parse_raw_file_content(
    thread_key: &ThreadKey,
    path_hint: Option<&str>,
    private_path_identity: Option<&str>,
    content: &str,
    input_kind: DiffInputKind,
    occurrence: usize,
    row_budget: usize,
) -> (DiffReviewFile, usize, usize, bool) {
    let (change_kind, row_kind) = match input_kind {
        DiffInputKind::AddedContent => (DiffFileChangeKind::Added, DiffReviewRowKind::Addition),
        DiffInputKind::DeletedContent => (DiffFileChangeKind::Deleted, DiffReviewRowKind::Deletion),
        _ => unreachable!("raw content parser requires add/delete input"),
    };
    let relative_path = path_hint.and_then(|path| {
        WorkspaceRelativePath::parse(path)
            .ok()
            .map(|path| path.normalized)
    });
    let full_identity = relative_path
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "raw:{}:{}",
                private_path_identity.unwrap_or("unknown"),
                stable_id(content)
            )
        });
    let display_path = relative_path
        .as_deref()
        .map(bounded_display_path)
        .unwrap_or_else(|| format!("Patch {}", occurrence + 1));
    let id = stable_id(&format!(
        "{}\0{}\0file\0{}",
        thread_key.server_id, thread_key.thread_id, full_identity
    ));
    let line_count = content.lines().count();
    let line_count_u32 = u32::try_from(line_count).unwrap_or(u32::MAX);
    let has_no_newline_marker = !content.is_empty() && !content.ends_with('\n');
    let total_rows = line_count.saturating_add(usize::from(has_no_newline_marker));
    let mut rows = Vec::with_capacity(total_rows.min(row_budget));

    for (index, text) in content.lines().take(row_budget).enumerate() {
        let line_number = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
        let (old_line_number, new_line_number) = match row_kind {
            DiffReviewRowKind::Addition => (None, Some(line_number)),
            DiffReviewRowKind::Deletion => (Some(line_number), None),
            _ => unreachable!(),
        };
        rows.push(DiffReviewRow {
            id: String::new(),
            kind: row_kind,
            old_line_number,
            new_line_number,
            text: text.to_string(),
        });
    }
    if has_no_newline_marker && rows.len() < row_budget {
        rows.push(DiffReviewRow {
            id: String::new(),
            kind: DiffReviewRowKind::NoNewlineMarker,
            old_line_number: None,
            new_line_number: None,
            text: "\\ No newline at end of file".to_string(),
        });
    }

    let hunks = if content.is_empty() {
        Vec::new()
    } else {
        let (old_start, old_count, new_start, new_count) = match input_kind {
            DiffInputKind::AddedContent => (0, 0, 1, line_count_u32),
            DiffInputKind::DeletedContent => (1, line_count_u32, 0, 0),
            _ => unreachable!(),
        };
        let header = format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@");
        vec![DiffReviewHunk {
            id: String::new(),
            header,
            old_start,
            old_count,
            new_start,
            new_count,
            rows,
        }]
    };
    let (old_path, new_path) = match input_kind {
        DiffInputKind::AddedContent => (None, relative_path.clone()),
        DiffInputKind::DeletedContent => (relative_path.clone(), None),
        _ => unreachable!(),
    };

    let mut file = DiffReviewFile {
        id,
        display_path,
        relative_path,
        old_path,
        new_path,
        change_kind,
        additions: if matches!(input_kind, DiffInputKind::AddedContent) {
            line_count_u32
        } else {
            0
        },
        deletions: if matches!(input_kind, DiffInputKind::DeletedContent) {
            line_count_u32
        } else {
            0
        },
        hunks,
        raw_patch: content.to_string(),
    };
    assign_stable_child_ids(&mut file);
    (
        file,
        total_rows.min(row_budget),
        usize::from(!content.is_empty()),
        total_rows > row_budget,
    )
}

fn parse_diff_file(
    thread_key: &ThreadKey,
    path_hint: Option<&str>,
    new_path_hint: Option<&str>,
    private_path_identity: Option<&str>,
    workspace_root: Option<&str>,
    patch: &str,
    input_kind: DiffInputKind,
    occurrence: usize,
    row_budget: usize,
    hunk_budget: usize,
) -> (DiffReviewFile, usize, usize, bool) {
    let hint = path_hint.and_then(canonical_decoded_path);
    let hinted_new_path = new_path_hint.and_then(canonical_decoded_path);
    let mut old_path = hinted_new_path.as_ref().and(hint.clone());
    let mut new_path = hinted_new_path;
    let mut rename_from: Option<CanonicalDiffPath> = None;
    let mut rename_to: Option<CanonicalDiffPath> = None;
    let mut marked_added = false;
    let mut marked_deleted = false;
    let mut marked_binary = false;
    let mut marked_unsupported = matches!(input_kind, DiffInputKind::RedactedPath);
    let mut redact_raw_patch = matches!(input_kind, DiffInputKind::RedactedPath);
    let mut hunks = Vec::new();
    let mut current_hunk: Option<InProgressHunk> = None;
    let mut additions = 0_u32;
    let mut deletions = 0_u32;
    let mut row_count = 0_usize;
    let mut truncated = false;
    let mut redact_hunks = false;

    for line in patch.lines() {
        let absolute_path_metadata = line_has_absolute_path_metadata(line);
        redact_raw_patch |= absolute_path_metadata;
        if absolute_path_metadata
            && current_hunk
                .as_ref()
                .is_some_and(|current| current.old_remaining > 0 || current.new_remaining > 0)
        {
            // Path-bearing metadata inside an incomplete hunk is malformed.
            // Fail closed instead of publishing it as an ordinary row.
            marked_unsupported = true;
            redact_hunks = true;
            truncated = true;
        }

        if let Some(header) = parse_hunk_header(line) {
            if let Some(current) = current_hunk.take() {
                if current.old_remaining > 0 || current.new_remaining > 0 {
                    truncated = true;
                }
                hunks.push(current.hunk);
            }
            if hunks.len() >= hunk_budget {
                truncated = true;
                break;
            }
            current_hunk = Some(InProgressHunk {
                hunk: DiffReviewHunk {
                    id: String::new(),
                    header: line.to_string(),
                    old_start: header.old_start,
                    old_count: header.old_count,
                    new_start: header.new_start,
                    new_count: header.new_count,
                    rows: Vec::new(),
                },
                old_line: header.old_start,
                new_line: header.new_start,
                old_remaining: header.old_count,
                new_remaining: header.new_count,
            });
            continue;
        }

        if current_hunk
            .as_ref()
            .is_some_and(|current| current.old_remaining == 0 && current.new_remaining == 0)
        {
            if line.starts_with("\\ No newline at end of file") {
                let current = current_hunk.as_mut().expect("checked above");
                if row_count >= row_budget {
                    truncated = true;
                } else {
                    current.hunk.rows.push(DiffReviewRow {
                        id: String::new(),
                        kind: DiffReviewRowKind::NoNewlineMarker,
                        old_line_number: None,
                        new_line_number: None,
                        text: line.to_string(),
                    });
                    row_count += 1;
                }
                continue;
            }
            hunks.push(current_hunk.take().expect("checked above").hunk);
        }

        if let Some(current) = current_hunk.as_mut() {
            let hunk = &mut current.hunk;
            let old_line = &mut current.old_line;
            let new_line = &mut current.new_line;
            if row_count >= row_budget {
                truncated = true;
            }
            let (kind, old_number, new_number, text) = if let Some(text) =
                line.strip_prefix('+').filter(|_| current.new_remaining > 0)
            {
                let number = *new_line;
                *new_line = new_line.saturating_add(1);
                current.new_remaining -= 1;
                additions = additions.saturating_add(1);
                (DiffReviewRowKind::Addition, None, Some(number), text)
            } else if let Some(text) = line.strip_prefix('-').filter(|_| current.old_remaining > 0)
            {
                let number = *old_line;
                *old_line = old_line.saturating_add(1);
                current.old_remaining -= 1;
                deletions = deletions.saturating_add(1);
                (DiffReviewRowKind::Deletion, Some(number), None, text)
            } else if let Some(text) = line
                .strip_prefix(' ')
                .filter(|_| current.old_remaining > 0 && current.new_remaining > 0)
            {
                let old_number = *old_line;
                let new_number = *new_line;
                *old_line = old_line.saturating_add(1);
                *new_line = new_line.saturating_add(1);
                current.old_remaining -= 1;
                current.new_remaining -= 1;
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
            if row_count < row_budget {
                hunk.rows.push(DiffReviewRow {
                    id: String::new(),
                    kind,
                    old_line_number: old_number,
                    new_line_number: new_number,
                    text: text.to_string(),
                });
                row_count += 1;
            }
            continue;
        }

        if let Some(paths) = line.strip_prefix("diff --git ") {
            if let Some((old, new)) = parse_diff_git_paths(paths) {
                redact_raw_patch |= git_path_is_absolute(&old) || git_path_is_absolute(&new);
                old_path = normalize_diff_path(&old, true, workspace_root);
                new_path = normalize_diff_path(&new, true, workspace_root);
            }
        } else if let Some(path) = line
            .strip_prefix("diff --cc ")
            .or_else(|| line.strip_prefix("diff --combined "))
        {
            redact_raw_patch |= git_path_is_absolute(path);
            let path = normalize_diff_path(path, false, workspace_root);
            old_path = path.clone();
            new_path = path;
            marked_unsupported = true;
        } else if line.starts_with("@@@ ") {
            marked_unsupported = true;
        } else if let Some(path) = line.strip_prefix("--- ") {
            redact_raw_patch |= git_path_is_absolute(path_field(path));
            old_path = normalize_diff_path(path_field(path), true, workspace_root);
        } else if let Some(path) = line.strip_prefix("+++ ") {
            redact_raw_patch |= git_path_is_absolute(path_field(path));
            new_path = normalize_diff_path(path_field(path), true, workspace_root);
        } else if let Some(path) = line.strip_prefix("rename from ") {
            redact_raw_patch |= git_path_is_absolute(path);
            rename_from = normalize_diff_path(path, false, workspace_root);
        } else if let Some(path) = line.strip_prefix("rename to ") {
            redact_raw_patch |= git_path_is_absolute(path);
            rename_to = normalize_diff_path(path, false, workspace_root);
        } else if let Some(path) = line.strip_prefix("Moved to: ") {
            redact_raw_patch |= git_path_is_absolute(path);
        } else if line.starts_with("new file mode ") {
            marked_added = true;
        } else if line.starts_with("deleted file mode ") {
            marked_deleted = true;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            marked_binary = true;
        }
    }

    if let Some(current) = current_hunk {
        if current.old_remaining > 0 || current.new_remaining > 0 {
            truncated = true;
        }
        if hunks.len() < hunk_budget {
            hunks.push(current.hunk);
        } else {
            truncated = true;
        }
    }
    if rename_from.is_some() {
        old_path = rename_from;
    }
    if rename_to.is_some() {
        new_path = rename_to;
    }

    // An authoritative diff may still mention a path outside the thread's
    // workspace. Preserve only a redacted identity for that entry: returning
    // its hunks or raw patch would turn trusted server output into a read-like
    // escape around the source-preview confinement contract.
    let outside_workspace = matches!(input_kind, DiffInputKind::RedactedPath)
        || old_path
            .iter()
            .chain(new_path.iter())
            .any(|path| path.identity.starts_with("outside-workspace:"));
    if outside_workspace || redact_hunks {
        marked_unsupported = true;
        additions = 0;
        deletions = 0;
        hunks.clear();
        row_count = 0;
    }

    let selected_path = new_path.as_ref().or(old_path.as_ref()).or(hint.as_ref());
    let display_path = selected_path
        .map(|path| path.display.clone())
        .unwrap_or_else(|| {
            if outside_workspace {
                "Outside workspace".to_string()
            } else {
                format!("Patch {}", occurrence + 1)
            }
        });
    let relative_path = selected_path.and_then(|path| path.relative_path.clone());
    let change_kind = if matches!(input_kind, DiffInputKind::RenameMetadata) {
        DiffFileChangeKind::Renamed
    } else if marked_unsupported {
        DiffFileChangeKind::Unsupported
    } else if marked_binary {
        DiffFileChangeKind::Binary
    } else if old_path.is_none() && new_path.is_some() || marked_added {
        DiffFileChangeKind::Added
    } else if new_path.is_none() && old_path.is_some() || marked_deleted {
        DiffFileChangeKind::Deleted
    } else if old_path.is_some()
        && new_path.is_some()
        && old_path.as_ref().map(|path| &path.identity)
            != new_path.as_ref().map(|path| &path.identity)
    {
        DiffFileChangeKind::Renamed
    } else if !hunks.is_empty() {
        DiffFileChangeKind::Modified
    } else if matches!(input_kind, DiffInputKind::UpdatedPatch) {
        DiffFileChangeKind::Modified
    } else {
        DiffFileChangeKind::Unknown
    };
    let full_identity = selected_path
        .map(|path| path.identity.clone())
        .unwrap_or_else(|| {
            format!(
                "patch:{}:{}",
                private_path_identity.unwrap_or("unknown"),
                stable_id(patch)
            )
        });
    let id = stable_id(&format!(
        "{}\0{}\0file\0{}",
        thread_key.server_id, thread_key.thread_id, full_identity
    ));
    let old_path = old_path.map(|path| path.display);
    let new_path = new_path.map(|path| path.display);

    let mut file = DiffReviewFile {
        id,
        display_path,
        relative_path,
        old_path,
        new_path,
        change_kind,
        additions,
        deletions,
        hunks,
        raw_patch: if outside_workspace || redact_raw_patch {
            String::new()
        } else {
            patch.to_string()
        },
    };
    assign_stable_child_ids(&mut file);
    let hunk_count = file.hunks.len();
    (file, row_count, hunk_count, truncated)
}

fn parse_diff_git_paths(value: &str) -> Option<(String, String)> {
    // Git's common unquoted form is `a/path b/path`. Splitting at the final
    // ` b/` preserves spaces in the old path. When core.quotePath applies,
    // retain each complete C-quoted token for the decoder below.
    if value.trim_start().starts_with('"') {
        let value = value.trim_start();
        let (first, remainder) = take_git_path_token(value)?;
        let (second, trailing) = take_git_path_token(remainder.trim_start())?;
        return trailing
            .trim()
            .is_empty()
            .then(|| (first.to_string(), second.to_string()));
    }
    let split = value
        .rfind(" b/")
        .or_else(|| value.rfind(" /"))
        .or_else(|| {
            let mut tokens = value.split_whitespace();
            let first = tokens.next()?;
            tokens.next()?;
            tokens.next().is_none().then_some(first.len())
        })?;
    Some((value[..split].to_string(), value[split + 1..].to_string()))
}

fn line_has_absolute_path_metadata(line: &str) -> bool {
    if let Some(paths) = line.strip_prefix("diff --git ") {
        return parse_diff_git_paths(paths)
            .is_some_and(|(old, new)| git_path_is_absolute(&old) || git_path_is_absolute(&new))
            || paths.split_whitespace().any(git_path_is_absolute);
    }
    if let Some(path) = line
        .strip_prefix("diff --cc ")
        .or_else(|| line.strip_prefix("diff --combined "))
    {
        return git_path_is_absolute(path);
    }
    for prefix in [
        "--- ",
        "+++ ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
        "Moved to: ",
    ] {
        if let Some(path) = line.strip_prefix(prefix) {
            return git_path_is_absolute(path_field(path));
        }
    }
    let Some(paths) = line
        .strip_prefix("Binary files ")
        .and_then(|value| value.strip_suffix(" differ"))
    else {
        return false;
    };
    parse_binary_paths(paths)
        .is_some_and(|(old, new)| git_path_is_absolute(old) || git_path_is_absolute(new))
        || paths.split_whitespace().any(git_path_is_absolute)
}

fn parse_binary_paths(value: &str) -> Option<(&str, &str)> {
    let value = value.trim();
    if value.starts_with('"') {
        let (first, remainder) = take_git_path_token(value)?;
        let remainder = remainder.strip_prefix(" and ")?;
        let (second, trailing) = take_git_path_token(remainder)?;
        return trailing.trim().is_empty().then_some((first, second));
    }
    value.split_once(" and ")
}

fn git_path_is_absolute(value: &str) -> bool {
    decode_git_path(value).is_some_and(|path| remote_path_is_absolute(&path))
}

fn path_field(value: &str) -> &str {
    let value = value.trim();
    if value.starts_with('"') {
        return take_git_path_token(value)
            .map(|(token, _)| token)
            .unwrap_or(value);
    }
    value.split('\t').next().unwrap_or(value).trim()
}

fn take_git_path_token(value: &str) -> Option<(&str, &str)> {
    if !value.starts_with('"') {
        let end = value.find(char::is_whitespace).unwrap_or(value.len());
        return Some((&value[..end], &value[end..]));
    }

    let bytes = value.as_bytes();
    let mut escaped = false;
    for index in 1..bytes.len() {
        match (escaped, bytes[index]) {
            (true, _) => escaped = false,
            (false, b'\\') => escaped = true,
            (false, b'"') => return Some((&value[..=index], &value[index + 1..])),
            _ => {}
        }
    }
    None
}

fn decode_git_path(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.starts_with('"') {
        return Some(value.to_string());
    }
    if value.len() < 2 || !value.ends_with('"') {
        return None;
    }

    let bytes = &value.as_bytes()[1..value.len() - 1];
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        let escaped = *bytes.get(index)?;
        index += 1;
        match escaped {
            b'a' => decoded.push(0x07),
            b'b' => decoded.push(0x08),
            b't' => decoded.push(b'\t'),
            b'n' => decoded.push(b'\n'),
            b'v' => decoded.push(0x0b),
            b'f' => decoded.push(0x0c),
            b'r' => decoded.push(b'\r'),
            b'\\' => decoded.push(b'\\'),
            b'"' => decoded.push(b'"'),
            b'0'..=b'7' => {
                let mut octal = u16::from(escaped - b'0');
                let mut digits = 1;
                while digits < 3 {
                    let Some(next @ b'0'..=b'7') = bytes.get(index).copied() else {
                        break;
                    };
                    octal = octal.saturating_mul(8) + u16::from(next - b'0');
                    index += 1;
                    digits += 1;
                }
                decoded.push(u8::try_from(octal).ok()?);
            }
            _ => return None,
        }
    }
    String::from_utf8(decoded).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalDiffPath {
    identity: String,
    display: String,
    relative_path: Option<String>,
}

fn canonical_decoded_path(value: &str) -> Option<CanonicalDiffPath> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let relative_path = WorkspaceRelativePath::parse(value)
        .ok()
        .map(|path| path.normalized);
    Some(CanonicalDiffPath {
        identity: value.to_string(),
        display: bounded_display_path(value),
        relative_path,
    })
}

fn normalize_diff_path(
    value: &str,
    strip_git_prefix: bool,
    workspace_root: Option<&str>,
) -> Option<CanonicalDiffPath> {
    let decoded = decode_git_path(value)?;
    if decoded.is_empty() || decoded == "/dev/null" {
        return None;
    }
    let path = if strip_git_prefix {
        decoded
            .strip_prefix("a/")
            .or_else(|| decoded.strip_prefix("b/"))
            .unwrap_or(&decoded)
    } else {
        &decoded
    };
    if remote_path_is_absolute(path) {
        if let Some(relative) = normalize_file_change_hint(workspace_root, path) {
            return canonical_decoded_path(&relative);
        }
        return Some(CanonicalDiffPath {
            identity: format!("outside-workspace:{}", stable_id(path)),
            display: "Outside workspace".to_string(),
            relative_path: None,
        });
    }
    canonical_decoded_path(path)
}

fn merge_diff_file(files: &mut Vec<DiffReviewFile>, mut incoming: DiffReviewFile) {
    let Some(existing) = files.iter_mut().find(|file| file.id == incoming.id) else {
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
    if existing.relative_path.is_none() {
        existing.relative_path = incoming.relative_path.take();
    }
    existing.hunks.append(&mut incoming.hunks);
    // Chunks already retain their trailing newline. Avoid inserting merge
    // separators so the aggregate copy/export payload cannot exceed the
    // global input-byte cap.
    existing.raw_patch.push_str(&incoming.raw_patch);
    assign_stable_child_ids(existing);
}

fn assign_stable_child_ids(file: &mut DiffReviewFile) {
    let mut hunk_occurrences = HashMap::<String, usize>::new();
    let mut row_occurrences = HashMap::<String, usize>::new();
    for hunk in &mut file.hunks {
        let semantic = format!(
            "{}\0hunk\0{}\0{}\0{}",
            file.id,
            hunk.old_start,
            hunk.new_start,
            hunk_context(&hunk.header)
        );
        let base_id = stable_id(&semantic);
        let occurrence = hunk_occurrences.entry(base_id.clone()).or_default();
        hunk.id = if *occurrence == 0 {
            base_id
        } else {
            stable_id(&format!("{base_id}\0duplicate\0{occurrence}"))
        };
        *occurrence += 1;

        for row in &mut hunk.rows {
            let row_semantic = format!(
                "{}\0row\0{:?}\0{}\0{}\0{}",
                semantic,
                row.kind,
                row.old_line_number.unwrap_or(0),
                row.new_line_number.unwrap_or(0),
                row.text
            );
            let base_id = stable_id(&row_semantic);
            let occurrence = row_occurrences.entry(base_id.clone()).or_default();
            row.id = if *occurrence == 0 {
                base_id
            } else {
                stable_id(&format!("{base_id}\0duplicate\0{occurrence}"))
            };
            *occurrence += 1;
        }
    }
}

fn hunk_context(header: &str) -> &str {
    let Some(after_ranges) = header.get(2..) else {
        return "";
    };
    let Some(end) = after_ranges.find("@@") else {
        return "";
    };
    after_ranges[end + 2..].trim()
}

fn merge_change_kind(
    existing: DiffFileChangeKind,
    incoming: DiffFileChangeKind,
) -> DiffFileChangeKind {
    use DiffFileChangeKind::{Added, Binary, Deleted, Modified, Renamed, Unknown, Unsupported};
    match (existing, incoming) {
        (Unsupported, _) | (_, Unsupported) => Unsupported,
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
    let mut sanitized = String::with_capacity(value.len().min(MAX_DISPLAY_PATH_BYTES));
    for character in value.chars() {
        let character = if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if sanitized.len().saturating_add(character.len_utf8()) > MAX_DISPLAY_PATH_BYTES {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_uniffi::{
        HydratedConversationItem, HydratedFileChangeData, HydratedFileChangeEntryData,
        HydratedTurnDiffData,
    };
    use crate::store::ThreadSnapshot;
    use crate::types::{AppOperationStatus, ThreadInfo, ThreadSummaryStatus};
    #[cfg(unix)]
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::fd::{AsRawFd, FromRawFd};
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
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

    fn unified_input(patch: &str) -> DiffInput<'_> {
        DiffInput {
            path_hint: None,
            new_path_hint: None,
            private_path_identity: None,
            workspace_root: None,
            patch,
            source_byte_length: patch.len() as u64,
            kind: DiffInputKind::Unified,
        }
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
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
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
        let input = [unified_input(patch)];
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
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
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
                path_hint: Some("odd.patch".to_string()),
                ..unified_input("not a unified diff\n+or - maybe malformed\n@@ nonsense @@\n")
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
        let review = normalize_trusted_diffs(key(), &[unified_input(&patch)]);
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

    #[test]
    fn canonical_store_projection_is_bounded_before_normalization() {
        let mut snapshot = ThreadSnapshot::from_info("server", thread_info(Some("/repo".into())));
        for index in 0..=MAX_DIFF_SOURCE_ITEMS {
            snapshot.items.push(HydratedConversationItem {
                id: format!("item-{index}"),
                content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                    diff: if index == MAX_DIFF_SOURCE_ITEMS {
                        "diff --git a/a b/a\n".to_string()
                    } else {
                        String::new()
                    },
                }),
                source_turn_id: Some(format!("turn-{index}")),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
        }
        let projection = project_diff_inputs(&snapshot);
        assert!(projection.truncated);
        assert_eq!(projection.inputs.len(), 1);

        let mut oversized = String::from("diff --git a/a b/a\n");
        oversized.push_str(&"+x\n".repeat(MAX_DIFF_REVIEW_BYTES));
        snapshot.items.clear();
        snapshot.items.push(HydratedConversationItem {
            id: "oversized".to_string(),
            content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                diff: oversized.clone(),
            }),
            source_turn_id: Some("turn".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        let projection = project_diff_inputs(&snapshot);
        assert!(projection.truncated);
        assert_eq!(projection.inputs.len(), 1);
        assert!(projection.inputs[0].patch.len() <= MAX_DIFF_REVIEW_BYTES);
        assert_eq!(
            projection.inputs[0].source_byte_length,
            oversized.len() as u64
        );
    }

    #[test]
    fn projection_discards_a_partial_final_line_at_the_byte_cap() {
        let mut patch = "diff --git a/a b/a\n".to_string();
        while patch.len() + "+whole\n".len() < MAX_DIFF_REVIEW_BYTES - 16 {
            patch.push_str("+whole\n");
        }
        let complete_length = patch.len();
        let bytes_to_cap = MAX_DIFF_REVIEW_BYTES - complete_length;
        patch.push('+');
        patch.push_str(&"x".repeat(bytes_to_cap + 32));
        patch.push('\n');

        let mut projection = DiffInputProjection {
            inputs: Vec::new(),
            truncated: false,
        };
        let mut remaining = MAX_DIFF_REVIEW_BYTES;
        assert!(projection.push(
            &mut remaining,
            None,
            None,
            None,
            None,
            &patch,
            DiffInputKind::Unified,
        ));

        assert!(projection.truncated);
        assert_eq!(remaining, 0);
        assert_eq!(projection.inputs.len(), 1);
        assert_eq!(projection.inputs[0].patch.len(), complete_length);
        assert_eq!(
            projection.inputs[0].patch.as_str(),
            &patch[..complete_length]
        );
        assert!(projection.inputs[0].patch.ends_with('\n'));
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

    #[tokio::test]
    async fn production_add_delete_file_changes_shape_raw_content() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "file-change".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![
                    HydratedFileChangeEntryData {
                        path: "/repo/new.txt".to_string(),
                        kind: "add".to_string(),
                        move_path: None,
                        diff: "new line\nsecond".to_string(),
                        // These are zero in the current upstream hydration for
                        // raw add/delete content; review derives the true count.
                        additions: 0,
                        deletions: 0,
                    },
                    HydratedFileChangeEntryData {
                        path: "/repo/old.txt".to_string(),
                        kind: "delete".to_string(),
                        move_path: None,
                        diff: "gone\n".to_string(),
                        additions: 0,
                        deletions: 0,
                    },
                ],
            }),
            source_turn_id: Some("turn-without-aggregate".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 2);
        assert_eq!(review.additions, 2);
        assert_eq!(review.deletions, 1);
        assert_eq!(review.files[0].change_kind, DiffFileChangeKind::Added);
        assert_eq!(review.files[0].relative_path.as_deref(), Some("new.txt"));
        assert_eq!(review.files[0].hunks[0].rows.len(), 3);
        assert_eq!(review.files[0].hunks[0].rows[0].new_line_number, Some(1));
        assert_eq!(review.files[1].change_kind, DiffFileChangeKind::Deleted);
        assert_eq!(review.files[1].relative_path.as_deref(), Some("old.txt"));
    }

    #[tokio::test]
    async fn turn_diff_replaces_overlapping_file_change_for_the_same_turn() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "file-change".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![HydratedFileChangeEntryData {
                    path: "/repo/a.txt".to_string(),
                    kind: "update".to_string(),
                    move_path: None,
                    diff: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
                    additions: 1,
                    deletions: 1,
                }],
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        snapshot.items.push(HydratedConversationItem {
            id: "turn-diff".to_string(),
            content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                diff: "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 1);
        assert_eq!((review.additions, review.deletions), (1, 1));
        assert_eq!(review.files[0].hunks.len(), 1);
        assert_eq!(review.files[0].hunks[0].rows.len(), 2);
    }

    #[tokio::test]
    async fn incomplete_failed_and_declined_file_changes_are_not_reviewed() {
        for status in [
            AppOperationStatus::Pending,
            AppOperationStatus::InProgress,
            AppOperationStatus::Failed,
            AppOperationStatus::Declined,
        ] {
            let client = client_with_thread("/repo");
            let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
            snapshot.items.push(HydratedConversationItem {
                id: format!("file-change-{status:?}"),
                content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                    status,
                    changes: vec![HydratedFileChangeEntryData {
                        path: "/repo/a.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: None,
                        diff: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
                        additions: 1,
                        deletions: 1,
                    }],
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(0),
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
            client.app_store.upsert_thread_snapshot(snapshot);

            assert_eq!(
                diff_review_for_thread(&client, key()).await,
                DiffReviewResult::Empty { thread_key: key() }
            );
        }
    }

    #[tokio::test]
    async fn standalone_pure_rename_survives_empty_patch_hydration() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "rename".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![HydratedFileChangeEntryData {
                    path: "/repo/old.rs".to_string(),
                    kind: "update".to_string(),
                    move_path: Some("/repo/new.rs".to_string()),
                    diff: String::new(),
                    additions: 0,
                    deletions: 0,
                }],
            }),
            source_turn_id: Some("rename-turn".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 1);
        let file = &review.files[0];
        assert_eq!(file.change_kind, DiffFileChangeKind::Renamed);
        assert_eq!(file.old_path.as_deref(), Some("old.rs"));
        assert_eq!(file.new_path.as_deref(), Some("new.rs"));
        assert_eq!(file.relative_path.as_deref(), Some("new.rs"));
        assert_eq!(file.display_path, "new.rs");
        assert!(file.hunks.is_empty());
        assert_eq!((review.additions, review.deletions), (0, 0));
    }

    #[tokio::test]
    async fn aggregate_turn_diff_keeps_pure_rename_without_duplicate_content() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "file-change".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![
                    HydratedFileChangeEntryData {
                        path: "/repo/a.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: None,
                        diff: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
                        additions: 1,
                        deletions: 1,
                    },
                    HydratedFileChangeEntryData {
                        path: "/repo/old.rs".to_string(),
                        kind: "update".to_string(),
                        move_path: Some("/repo/new.rs".to_string()),
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    },
                ],
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        snapshot.items.push(HydratedConversationItem {
            id: "turn-diff".to_string(),
            content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                diff: "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 2);
        assert_eq!((review.additions, review.deletions), (1, 1));
        assert_eq!(
            review
                .files
                .iter()
                .filter(|file| file.relative_path.as_deref() == Some("a.txt"))
                .count(),
            1
        );
        let rename = review
            .files
            .iter()
            .find(|file| file.relative_path.as_deref() == Some("new.rs"))
            .expect("rename retained");
        assert_eq!(rename.change_kind, DiffFileChangeKind::Renamed);
        assert!(rename.hunks.is_empty());
    }

    #[test]
    fn absolute_turn_diff_paths_are_relativized_or_redacted() {
        let inside = "diff --git /repo/src/a.rs /repo/src/a.rs\n--- /repo/src/a.rs\n+++ /repo/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let outside = "diff --git /secret/a.rs /secret/a.rs\n--- /secret/a.rs\n+++ /secret/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let inputs = [
            DiffInput {
                workspace_root: Some("/repo".to_string()),
                ..unified_input(inside)
            },
            DiffInput {
                workspace_root: Some("/repo".to_string()),
                ..unified_input(outside)
            },
        ];
        let review = normalize_trusted_diffs(key(), &inputs);
        assert_eq!(review.files.len(), 2);
        assert_eq!(review.files[0].relative_path.as_deref(), Some("src/a.rs"));
        assert_eq!(review.files[0].display_path, "src/a.rs");
        let redacted = &review.files[1];
        assert_eq!(redacted.display_path, "Outside workspace");
        assert!(redacted.relative_path.is_none());
        assert_eq!(redacted.old_path.as_deref(), Some("Outside workspace"));
        assert_eq!(redacted.new_path.as_deref(), Some("Outside workspace"));
        assert!(!format!("{redacted:?}").contains("/secret"));
    }

    #[tokio::test]
    async fn unconfined_file_change_payload_is_fully_redacted() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "outside-file-change".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![
                    HydratedFileChangeEntryData {
                        path: "/secret/add.txt".to_string(),
                        kind: "add".to_string(),
                        move_path: None,
                        diff: "private-add\n".to_string(),
                        additions: 1,
                        deletions: 0,
                    },
                    HydratedFileChangeEntryData {
                        path: "/secret/delete.txt".to_string(),
                        kind: "delete".to_string(),
                        move_path: None,
                        diff: "private-delete\n".to_string(),
                        additions: 0,
                        deletions: 1,
                    },
                    HydratedFileChangeEntryData {
                        path: "/secret/update.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: None,
                        diff: "@@ -1 +1 @@\n-private-old\n+private-new\n".to_string(),
                        additions: 1,
                        deletions: 1,
                    },
                ],
            }),
            source_turn_id: Some("turn".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected redacted entry");
        };
        assert_eq!(review.files.len(), 3);
        for file in &review.files {
            assert_eq!(file.change_kind, DiffFileChangeKind::Unsupported);
            assert!(file.relative_path.is_none());
            assert!(file.old_path.is_none());
            assert!(file.new_path.is_none());
            assert!(file.raw_patch.is_empty());
            assert!(file.hunks.is_empty());
        }
        assert_eq!((review.additions, review.deletions), (0, 0));
        let public_value = format!("{review:?}");
        assert!(!public_value.contains("/secret"));
        assert!(!public_value.contains("private-add"));
        assert!(!public_value.contains("private-delete"));
        assert!(!public_value.contains("private-old"));
        assert_eq!(review.source_byte_length, 65);
    }

    #[tokio::test]
    async fn empty_unconfined_file_changes_remain_typed_redacted_entries() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "empty-outside-file-changes".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![
                    HydratedFileChangeEntryData {
                        path: "/secret/add.txt".to_string(),
                        kind: "add".to_string(),
                        move_path: None,
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    },
                    HydratedFileChangeEntryData {
                        path: "/secret/delete.txt".to_string(),
                        kind: "delete".to_string(),
                        move_path: None,
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    },
                    HydratedFileChangeEntryData {
                        path: "/secret/update.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: None,
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    },
                    HydratedFileChangeEntryData {
                        path: "/secret/old.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: Some("/secret/new.txt".to_string()),
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    },
                ],
            }),
            source_turn_id: Some("turn".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected redacted entries");
        };
        assert_eq!(review.files.len(), 4);
        assert_eq!(review.source_byte_length, 0);
        for file in &review.files {
            assert_eq!(file.display_path, "Outside workspace");
            assert_eq!(file.change_kind, DiffFileChangeKind::Unsupported);
            assert!(file.relative_path.is_none());
            assert!(file.hunks.is_empty());
            assert!(file.raw_patch.is_empty());
        }
        assert!(!format!("{review:?}").contains("/secret"));
    }

    #[tokio::test]
    async fn empty_unconfined_change_survives_an_unrelated_same_turn_aggregate() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.extend([
            HydratedConversationItem {
                id: "empty-outside-file-change".to_string(),
                content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                    status: AppOperationStatus::Completed,
                    changes: vec![HydratedFileChangeEntryData {
                        path: "/secret/empty.txt".to_string(),
                        kind: "add".to_string(),
                        move_path: None,
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    }],
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(0),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
            HydratedConversationItem {
                id: "unrelated-aggregate".to_string(),
                content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                    diff: "diff --git a/safe.txt b/safe.txt\n--- a/safe.txt\n+++ b/safe.txt\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(1),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
        ]);
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 2);
        assert!(review.files.iter().any(|file| {
            file.display_path == "Outside workspace"
                && file.change_kind == DiffFileChangeKind::Unsupported
                && file.raw_patch.is_empty()
        }));
        assert!(review
            .files
            .iter()
            .any(|file| file.relative_path.as_deref() == Some("safe.txt")));
        assert!(!format!("{review:?}").contains("/secret"));
    }

    #[tokio::test]
    async fn redacted_payload_does_not_spend_the_safe_diff_byte_budget() {
        let private_payload = "private".repeat(MAX_DIFF_REVIEW_BYTES / 4);
        let safe_patch = "diff --git a/safe.txt b/safe.txt\n--- a/safe.txt\n+++ b/safe.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.extend([
            HydratedConversationItem {
                id: "large-outside-file-change".to_string(),
                content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                    status: AppOperationStatus::Completed,
                    changes: vec![HydratedFileChangeEntryData {
                        path: "/secret/large.txt".to_string(),
                        kind: "update".to_string(),
                        move_path: None,
                        diff: private_payload.clone(),
                        additions: 0,
                        deletions: 0,
                    }],
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(0),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
            HydratedConversationItem {
                id: "safe-aggregate".to_string(),
                content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                    diff: safe_patch.to_string(),
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(1),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
        ]);

        let projection = project_diff_inputs(&snapshot);
        assert!(!projection.truncated);
        assert_eq!(projection.inputs.len(), 2);
        assert!(projection.inputs[0].patch.is_empty());
        assert_eq!(
            projection.inputs[0].source_byte_length,
            private_payload.len() as u64
        );
        assert_eq!(projection.inputs[1].patch, safe_patch);

        client.app_store.upsert_thread_snapshot(snapshot);
        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert!(!review.truncated);
        assert!(review
            .files
            .iter()
            .any(|file| file.relative_path.as_deref() == Some("safe.txt")));
        assert!(review.files.iter().any(|file| {
            file.display_path == "Outside workspace"
                && file.change_kind == DiffFileChangeKind::Unsupported
                && file.raw_patch.is_empty()
        }));
        let public_value = format!("{review:?}");
        assert!(!public_value.contains("/secret"));
        assert!(!public_value.contains("privateprivate"));
    }

    #[tokio::test]
    async fn redacted_metadata_normalizes_after_an_aggregate_exhausts_the_byte_budget() {
        let header = "diff --git a/safe.txt b/safe.txt\n";
        let mut full_budget_patch = header.to_string();
        full_budget_patch.push_str(&"x".repeat(MAX_DIFF_REVIEW_BYTES - header.len() - 1));
        full_budget_patch.push('\n');
        assert_eq!(full_budget_patch.len(), MAX_DIFF_REVIEW_BYTES);

        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.extend([
            HydratedConversationItem {
                id: "full-budget-aggregate".to_string(),
                content: HydratedConversationItemContent::TurnDiff(HydratedTurnDiffData {
                    diff: full_budget_patch,
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(0),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
            HydratedConversationItem {
                id: "empty-outside-file-change".to_string(),
                content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                    status: AppOperationStatus::Completed,
                    changes: vec![HydratedFileChangeEntryData {
                        path: "/secret/empty.txt".to_string(),
                        kind: "add".to_string(),
                        move_path: None,
                        diff: String::new(),
                        additions: 0,
                        deletions: 0,
                    }],
                }),
                source_turn_id: Some("turn".to_string()),
                source_turn_index: Some(1),
                timestamp: None,
                is_from_user_turn_boundary: false,
            },
        ]);

        let projection = project_diff_inputs(&snapshot);
        assert_eq!(projection.inputs.len(), 2);
        assert_eq!(projection.inputs[0].patch.len(), MAX_DIFF_REVIEW_BYTES);
        assert!(projection.inputs[1].patch.is_empty());

        client.app_store.upsert_thread_snapshot(snapshot);
        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        assert_eq!(review.files.len(), 2);
        assert!(review.files.iter().any(|file| {
            file.display_path == "Outside workspace"
                && file.change_kind == DiffFileChangeKind::Unsupported
                && file.raw_patch.is_empty()
        }));
        assert!(!format!("{review:?}").contains("/secret"));
    }

    #[test]
    fn normalized_absolute_metadata_is_omitted_from_raw_export() {
        let patch = "diff --git /repo/src/a.rs /repo/src/a.rs\n--- /repo/src/a.rs\n+++ /repo/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                workspace_root: Some("/repo".to_string()),
                ..unified_input(patch)
            }],
        );
        let file = &review.files[0];
        assert_eq!(file.relative_path.as_deref(), Some("src/a.rs"));
        assert_eq!((file.additions, file.deletions), (1, 1));
        assert_eq!(file.hunks.len(), 1);
        assert!(file.raw_patch.is_empty());
        assert!(!format!("{file:?}").contains("/repo"));
    }

    #[test]
    fn absolute_binary_metadata_is_omitted_from_raw_export() {
        for patch in [
            "diff --git /repo/assets/a.bin /repo/assets/a.bin\nBinary files /repo/assets/a.bin and /repo/assets/a.bin differ\n",
            "Binary files /repo/assets/a.bin and /repo/assets/a.bin differ\n",
        ] {
            let review = normalize_trusted_diffs(
                key(),
                &[DiffInput {
                    workspace_root: Some("/repo".to_string()),
                    ..unified_input(patch)
                }],
            );
            assert_eq!(review.files.len(), 1);
            let file = &review.files[0];
            assert_eq!(file.change_kind, DiffFileChangeKind::Binary);
            assert!(file.raw_patch.is_empty());
            assert!(!format!("{file:?}").contains("/repo"));
        }
    }

    #[test]
    fn absolute_copy_metadata_is_omitted_from_raw_export() {
        let patch = "diff --git a/new.rs b/new.rs\nsimilarity index 100%\ncopy from /repo/old.rs\ncopy to /repo/new.rs\n";
        let review = normalize_trusted_diffs(
            key(),
            &[DiffInput {
                workspace_root: Some("/repo".to_string()),
                ..unified_input(patch)
            }],
        );
        assert_eq!(review.files.len(), 1);
        let file = &review.files[0];
        assert!(file.raw_patch.is_empty());
        assert!(!format!("{file:?}").contains("/repo"));
    }

    #[tokio::test]
    async fn absolute_move_metadata_is_omitted_from_raw_export() {
        let client = client_with_thread("/repo");
        let mut snapshot = client.app_store.thread_snapshot(&key()).expect("thread");
        snapshot.items.push(HydratedConversationItem {
            id: "move".to_string(),
            content: HydratedConversationItemContent::FileChange(HydratedFileChangeData {
                status: AppOperationStatus::Completed,
                changes: vec![HydratedFileChangeEntryData {
                    path: "/repo/old.rs".to_string(),
                    kind: "update".to_string(),
                    move_path: Some("/repo/new.rs".to_string()),
                    diff: "@@ -1 +1 @@\n-old\n+new\n\n\nMoved to: /repo/new.rs".to_string(),
                    additions: 1,
                    deletions: 1,
                }],
            }),
            source_turn_id: Some("turn".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        client.app_store.upsert_thread_snapshot(snapshot);

        let DiffReviewResult::Ready { review } = diff_review_for_thread(&client, key()).await
        else {
            panic!("expected ready review");
        };
        let file = &review.files[0];
        assert_eq!(file.change_kind, DiffFileChangeKind::Renamed);
        assert_eq!(file.old_path.as_deref(), Some("old.rs"));
        assert_eq!(file.new_path.as_deref(), Some("new.rs"));
        assert!(file.raw_patch.is_empty());
        assert!(!format!("{file:?}").contains("/repo"));
    }

    #[test]
    fn hunk_parser_enforces_declared_ranges() {
        let patch =
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n-extra\n+extra\n";
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        let file = &review.files[0];
        assert_eq!((file.additions, file.deletions), (1, 1));
        assert_eq!(file.hunks[0].rows.len(), 2);
        assert!(file.hunks[0].rows.iter().all(|row| row.text != "extra"));
    }

    #[test]
    fn incomplete_hunks_are_marked_truncated() {
        for patch in [
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n",
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n+new\n",
        ] {
            let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
            assert!(review.truncated, "incomplete patch: {patch:?}");
            assert_eq!(review.files.len(), 1);
            assert_eq!(review.files[0].hunks.len(), 1);
        }
    }

    #[test]
    fn absolute_metadata_inside_incomplete_hunks_is_fully_redacted() {
        for metadata in [
            "Moved to: /repo/private",
            "--- /repo/private",
            "+++ /repo/private",
            "copy from /repo/private",
            "copy to /repo/private",
        ] {
            let patch =
                format!("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n{metadata}\n");
            let review = normalize_trusted_diffs(
                key(),
                &[DiffInput {
                    workspace_root: Some("/repo".to_string()),
                    ..unified_input(&patch)
                }],
            );
            assert!(review.truncated, "incomplete patch: {patch:?}");
            assert_eq!(review.files.len(), 1);
            let file = &review.files[0];
            assert_eq!(file.change_kind, DiffFileChangeKind::Unsupported);
            assert!(file.hunks.is_empty());
            assert!(file.raw_patch.is_empty());
            assert!(!format!("{file:?}").contains("/repo"));
        }
    }

    #[test]
    fn plain_multi_file_unified_diff_splits_without_git_headers() {
        let patch = "--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-old\n+new\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-left\n+right\n";
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        assert_eq!(review.files.len(), 2);
        assert_eq!((review.additions, review.deletions), (2, 2));
        assert_eq!(review.files[0].relative_path.as_deref(), Some("one.rs"));
        assert_eq!(review.files[1].relative_path.as_deref(), Some("two.rs"));
    }

    #[test]
    fn combined_diff_is_explicitly_unsupported() {
        let patch = "diff --cc merge.rs\nindex aaa,bbb..ccc\n--- a/merge.rs\n+++ b/merge.rs\n@@@ -1,1 -1,1 +1,1 @@@\n--left\n -right\n++merged\n";
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].change_kind, DiffFileChangeKind::Unsupported);
        assert_eq!(review.files[0].raw_patch, patch);
        assert_eq!((review.additions, review.deletions), (0, 0));
    }

    #[test]
    fn paths_named_with_git_prefixes_are_not_stripped_from_rename_metadata() {
        let patch = "diff --git a/a/foo b/b/foo\nsimilarity index 100%\nrename from a/foo\nrename to b/foo\n";
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        let file = &review.files[0];
        assert_eq!(file.change_kind, DiffFileChangeKind::Renamed);
        assert_eq!(file.old_path.as_deref(), Some("a/foo"));
        assert_eq!(file.new_path.as_deref(), Some("b/foo"));
    }

    #[test]
    fn quoted_git_path_can_contain_the_unquoted_split_delimiter() {
        let patch = "diff --git \"a/name b/part.bin\" \"b/name b/part.bin\"\nBinary files \"a/name b/part.bin\" and \"b/name b/part.bin\" differ\n";
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].display_path, "name b/part.bin");
        assert_eq!(review.files[0].change_kind, DiffFileChangeKind::Binary);
    }

    #[test]
    fn existing_row_and_hunk_ids_survive_an_expanded_hunk() {
        let original = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let expanded = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n-old\n+new\n+later\n";
        let first = normalize_trusted_diffs(key(), &[unified_input(original)]);
        let second = normalize_trusted_diffs(key(), &[unified_input(expanded)]);
        assert_eq!(first.files[0].hunks[0].id, second.files[0].hunks[0].id);
        let first_new = first.files[0].hunks[0]
            .rows
            .iter()
            .find(|row| row.text == "new")
            .expect("original row");
        let second_new = second.files[0].hunks[0]
            .rows
            .iter()
            .find(|row| row.text == "new")
            .expect("expanded row");
        assert_eq!(first_new.id, second_new.id);
    }

    #[test]
    fn pathless_identical_patches_do_not_merge_across_private_identities() {
        let inputs = [
            DiffInput {
                private_path_identity: Some("first".to_string()),
                ..unified_input("@@ -1 +1 @@\n-old\n+new\n")
            },
            DiffInput {
                private_path_identity: Some("second".to_string()),
                ..unified_input("@@ -1 +1 @@\n-old\n+new\n")
            },
        ];
        let review = normalize_trusted_diffs(key(), &inputs);
        assert_eq!(review.files.len(), 2);
        assert_ne!(review.files[0].id, review.files[1].id);
    }

    #[test]
    fn absolute_file_change_hints_are_workspace_relative_or_omitted() {
        assert_eq!(
            normalize_file_change_hint(Some("/repo"), "/repo/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            normalize_file_change_hint(Some("C:\\Repo"), "c:/repo/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            normalize_file_change_hint(Some("/repo"), "/repository/secret"),
            None
        );
        assert_eq!(
            normalize_file_change_hint(Some("/repo"), "/outside/secret"),
            None
        );
    }

    #[test]
    fn stable_row_ids_survive_unrelated_prepended_files() {
        let unchanged =
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let prepended = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-left\n+right\n{unchanged}"
        );
        let first = normalize_trusted_diffs(key(), &[unified_input(unchanged)]);
        let second = normalize_trusted_diffs(key(), &[unified_input(&prepended)]);
        let first_file = first
            .files
            .iter()
            .find(|file| file.relative_path.as_deref() == Some("b.rs"))
            .expect("b file");
        let second_file = second
            .files
            .iter()
            .find(|file| file.relative_path.as_deref() == Some("b.rs"))
            .expect("b file");
        assert_eq!(first_file.id, second_file.id);
        assert_eq!(first_file.hunks[0].id, second_file.hunks[0].id);
        assert_eq!(
            first_file.hunks[0]
                .rows
                .iter()
                .map(|row| &row.id)
                .collect::<Vec<_>>(),
            second_file.hunks[0]
                .rows
                .iter()
                .map(|row| &row.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn full_path_identity_prevents_truncated_label_collisions() {
        let prefix = "x".repeat(MAX_DISPLAY_PATH_BYTES);
        let first_path = format!("{prefix}/a.rs");
        let second_path = format!("{prefix}/b.rs");
        let patch = format!(
            "diff --git a/{first_path} b/{first_path}\n--- a/{first_path}\n+++ b/{first_path}\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/{second_path} b/{second_path}\n--- a/{second_path}\n+++ b/{second_path}\n@@ -1 +1 @@\n-left\n+right\n"
        );
        let review = normalize_trusted_diffs(key(), &[unified_input(&patch)]);
        assert_eq!(review.files.len(), 2);
        assert_eq!(review.files[0].display_path, review.files[1].display_path);
        assert_ne!(review.files[0].id, review.files[1].id);
        assert_ne!(review.files[0].relative_path, review.files[1].relative_path);
    }

    #[test]
    fn git_c_quoted_paths_decode_octal_and_standard_escapes() {
        assert_eq!(
            decode_git_path(r#""a/\303\251\tquote\"slash\\name""#).as_deref(),
            Some("a/\u{e9}\tquote\"slash\\name")
        );
        let patch = r#"diff --git "a/\303\251 file.rs" "b/\303\251 file.rs"
--- "a/\303\251 file.rs"
+++ "b/\303\251 file.rs"
@@ -1 +1 @@
-old
+new
"#;
        let review = normalize_trusted_diffs(key(), &[unified_input(patch)]);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].display_path, "\u{e9} file.rs");
        assert_eq!(
            review.files[0].relative_path.as_deref(),
            Some("\u{e9} file.rs")
        );
    }

    /// Test-only adapter proving the host-side contract. It walks every path
    /// segment relative to an already-open root descriptor and sets
    /// `O_NOFOLLOW` at each hop, so traversal, symlink swaps, and non-files do
    /// not turn into broader reads. Production mobile code does not use this
    /// adapter; Remora Link must provide equivalent enforcement remotely.
    #[cfg(unix)]
    fn open_root_capability(root: &Path) -> std::io::Result<File> {
        let path = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        // SAFETY: path is NUL-terminated and open returns a new descriptor.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is freshly returned and ownership transfers once.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[cfg(unix)]
    fn open_confined(root: &File, path: &WorkspaceRelativePath) -> std::io::Result<File> {
        let mut directory = root.try_clone()?;
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
        let root_capability = open_root_capability(&root).expect("root capability");

        let escape = WorkspaceRelativePath::parse("escape/secret").expect("valid syntax");
        assert!(open_confined(&root_capability, &escape).is_err());
        let directory = WorkspaceRelativePath::parse("directory").expect("valid syntax");
        assert!(open_confined(&root_capability, &directory).is_err());
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
        let root_capability = open_root_capability(&root).expect("root capability");
        let mut opened = open_confined(&root_capability, &relative).expect("confined open");

        std::fs::rename(root.join("file"), root.join("original")).expect("rename");
        symlink(outside.join("file"), root.join("file")).expect("swap to symlink");
        let mut contents = String::new();
        opened
            .read_to_string(&mut contents)
            .expect("read pinned fd");
        assert_eq!(contents, "inside");
        assert!(open_confined(&root_capability, &relative).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn workspace_root_capability_is_pinned_across_path_swap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let moved_root = temp.path().join("moved-root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("file"), "inside").expect("inside");
        std::fs::write(outside.join("file"), "outside").expect("outside file");
        let root_capability = open_root_capability(&root).expect("root capability");

        std::fs::rename(&root, &moved_root).expect("move root");
        symlink(&outside, &root).expect("replace root path with symlink");
        let relative = WorkspaceRelativePath::parse("file").expect("path");
        let mut opened = open_confined(&root_capability, &relative).expect("pinned root read");
        let mut contents = String::new();
        opened.read_to_string(&mut contents).expect("read");
        assert_eq!(contents, "inside");
        assert!(open_root_capability(&root).is_err());
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
        let root_capability = open_root_capability(root).expect("root capability");

        let preview = |name: &str| {
            let relative = WorkspaceRelativePath::parse(name).expect("path");
            let file = open_confined(&root_capability, &relative).expect("open");
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

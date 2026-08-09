use std::path::PathBuf;

use codex_app_server_protocol as upstream;

use crate::conversation_uniffi::{
    HydratedConversationItem, HydratedConversationItemContent, HydratedUserInputResponseData,
    HydratedUserInputResponseOptionData, HydratedUserInputResponseQuestionData,
};
use crate::store::snapshot::ThreadSnapshot;
use crate::types::{
    PendingUserInputAnswer, PendingUserInputRequest, ThreadInfo, ThreadKey, ThreadSummaryStatus,
};

use super::AppStoreReducer;

const USER_INPUT_NOTE_PREFIX: &str = "user_note: ";
const USER_INPUT_OTHER_OPTION_LABEL: &str = "None of the above";
pub(super) const LOCAL_USER_MESSAGE_ITEM_PREFIX: &str = "local-user-message:";
const DESKTOP_FILE_CONTEXT_HEADER: &str = "# Files mentioned by the user:";
const DESKTOP_FILE_CONTEXT_REQUEST_HEADER: &str = "## My request for Codex:";
pub(super) const USER_INPUT_RESPONSE_ITEM_PREFIX: &str = "user-input-response:";

impl AppStoreReducer {
    pub(super) fn upsert_or_merge_thread<F>(&self, key: ThreadKey, info: ThreadInfo, mutate: F)
    where
        F: FnOnce(&mut ThreadSnapshot),
    {
        let inserted = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let inserted = !snapshot.threads.contains_key(&key);
            let thread = snapshot
                .threads
                .entry(key.clone())
                .or_insert_with(|| ThreadSnapshot::from_info(&key.server_id, info.clone()));
            thread.info.id = info.id;
            if info.title.is_some() {
                thread.info.title = info.title;
            }
            if info.preview.is_some() {
                thread.info.preview = info.preview;
            }
            if info.cwd.is_some() {
                thread.info.cwd = info.cwd;
            }
            if info.path.is_some() {
                thread.info.path = info.path;
            }
            if info.model_provider.is_some() {
                thread.info.model_provider = info.model_provider;
            }
            if info.agent_nickname.is_some() {
                thread.info.agent_nickname = info.agent_nickname;
            }
            if info.agent_role.is_some() {
                thread.info.agent_role = info.agent_role;
            }
            if info.created_at.is_some() {
                thread.info.created_at = info.created_at;
            }
            if info.updated_at.is_some() {
                thread.info.updated_at = info.updated_at;
            }
            // ThreadStatusChanged is metadata-only; it cannot close an in-flight
            // turn. Only TurnCompleted (or a rebuild with the turn list) is
            // allowed to downgrade Active → Idle.
            thread.info.status = if matches!(info.status, ThreadSummaryStatus::Idle)
                && thread.active_turn_id.is_some()
            {
                thread.info.status.clone()
            } else {
                info.status
            };
            mutate(thread);
            inserted
        };
        if inserted {
            self.emit_thread_upsert(&key);
        } else {
            self.emit_thread_metadata_changed(&key);
        }
    }
}

pub(super) fn local_user_message_overlay_item(
    inputs: &[upstream::UserInput],
) -> Option<HydratedConversationItem> {
    let (text, image_data_uris) = render_user_input(inputs);
    if text.is_empty() && image_data_uris.is_empty() {
        return None;
    }

    Some(HydratedConversationItem {
        id: format!(
            "{LOCAL_USER_MESSAGE_ITEM_PREFIX}{}",
            crate::next_request_id()
        ),
        content: HydratedConversationItemContent::User(
            crate::conversation_uniffi::HydratedUserMessageData {
                text,
                image_data_uris,
            },
        ),
        source_turn_id: None,
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: true,
    })
}

fn render_user_input(inputs: &[upstream::UserInput]) -> (String, Vec<String>) {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    for input in inputs {
        match input {
            upstream::UserInput::Text { text, .. } => {
                let trimmed = visible_user_text(text);
                if !trimmed.is_empty() {
                    text_parts.push(trimmed);
                }
            }
            upstream::UserInput::Image { url, .. } => images.push(url.clone()),
            upstream::UserInput::LocalImage { path, .. } => {
                images.push(format!("file://{}", path.display()));
            }
            upstream::UserInput::Skill { name, path } => {
                if !name.is_empty() && path != &PathBuf::new() {
                    text_parts.push(format!("[Skill] {} ({})", name, path.display()));
                } else if !name.is_empty() {
                    text_parts.push(format!("[Skill] {name}"));
                } else if path != &PathBuf::new() {
                    text_parts.push(format!("[Skill] {}", path.display()));
                }
            }
            upstream::UserInput::Mention { name, path } => {
                if !name.is_empty() && !path.is_empty() {
                    text_parts.push(format!("[Mention] {name} ({path})"));
                } else if !name.is_empty() {
                    text_parts.push(format!("[Mention] {name}"));
                } else if !path.is_empty() {
                    text_parts.push(format!("[Mention] {path}"));
                }
            }
        }
    }
    (text_parts.join("\n"), images)
}

fn visible_user_text(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with(DESKTOP_FILE_CONTEXT_HEADER) {
        return trimmed.to_string();
    }
    let Some((file_context, request)) = trimmed.split_once(DESKTOP_FILE_CONTEXT_REQUEST_HEADER)
    else {
        return trimmed.to_string();
    };
    let request = request.trim();
    if !request.is_empty() {
        return request.to_string();
    }
    file_context_summary(file_context).unwrap_or_else(|| trimmed.to_string())
}

fn file_context_summary(file_context: &str) -> Option<String> {
    let labels: Vec<String> = file_context
        .lines()
        .filter_map(|line| line.trim().strip_prefix("## "))
        .map(|line| line.split_once(':').map(|(label, _)| label).unwrap_or(line))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(|label| format!("[File] {label}"))
        .collect();
    if labels.is_empty() {
        None
    } else {
        Some(labels.join("\n"))
    }
}

pub(super) fn preserve_local_overlay_items(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    let active_turn_id = target.active_turn_id.as_deref();
    for item in &source.local_overlay_items {
        let item = bind_pending_local_user_overlay_to_target_turn(item, target);
        if target
            .items
            .iter()
            .all(|existing| !is_superseded_overlay_item(&item, existing, active_turn_id))
            && target
                .local_overlay_items
                .iter()
                .all(|existing| !is_superseded_overlay_item(&item, existing, active_turn_id))
        {
            target.local_overlay_items.push(item);
        }
    }
}

pub(super) fn duplicate_local_overlay_item_ids(thread: &ThreadSnapshot) -> Vec<String> {
    let active_turn_id = thread.active_turn_id.as_deref();
    thread
        .local_overlay_items
        .iter()
        .filter(|item| {
            thread
                .items
                .iter()
                .any(|existing| is_superseded_overlay_item(item, existing, active_turn_id))
        })
        .map(|item| item.id.clone())
        .collect()
}

pub(crate) fn remove_duplicate_local_overlay_items(thread: &mut ThreadSnapshot) {
    let active_turn_id = thread.active_turn_id.as_deref();
    thread.local_overlay_items.retain(|item| {
        thread
            .items
            .iter()
            .all(|existing| !is_superseded_overlay_item(item, existing, active_turn_id))
    });
}

fn bind_pending_local_user_overlay_to_target_turn(
    item: &HydratedConversationItem,
    target: &ThreadSnapshot,
) -> HydratedConversationItem {
    if item.id.starts_with(LOCAL_USER_MESSAGE_ITEM_PREFIX)
        && item.source_turn_id.is_none()
        && let Some(turn_id) = target.active_turn_id.as_ref()
    {
        let mut bound = item.clone();
        bound.source_turn_id = Some(turn_id.clone());
        return bound;
    }
    item.clone()
}

pub(super) fn preserve_thread_runtime_state(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    target.collaboration_mode = source.collaboration_mode;
    if target.model.is_none() {
        target.model = source.model.clone();
    }
    if target.reasoning_effort.is_none() {
        target.reasoning_effort = source.reasoning_effort.clone();
    }
    if target.active_plan_progress.is_none() {
        target.active_plan_progress = source.active_plan_progress.clone();
    }
    if target.pending_plan_implementation_turn_id.is_none() {
        target.pending_plan_implementation_turn_id =
            source.pending_plan_implementation_turn_id.clone();
    }
}

pub(super) fn preserve_thread_title(existing: &ThreadInfo, incoming: &mut ThreadInfo) {
    let incoming_blank = incoming
        .title
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if !incoming_blank {
        return;
    }

    let existing_title = existing
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if existing_title.is_some() {
        incoming.title = existing_title;
    }
}

pub(super) fn preserve_thread_preview(existing: &ThreadInfo, incoming: &mut ThreadInfo) {
    if incoming.preview.is_some() {
        return;
    }
    if existing.preview.is_some() {
        incoming.preview = existing.preview.clone();
    }
}

pub(super) fn preserve_thread_created_at(existing: &ThreadInfo, incoming: &mut ThreadInfo) {
    if let Some(existing_created) = existing.created_at {
        match incoming.created_at {
            None => incoming.created_at = Some(existing_created),
            Some(incoming_created) if existing_created < incoming_created => {
                incoming.created_at = Some(existing_created);
            }
            _ => {}
        }
    }
}

/// Fork lineage and sub-agent parent are immutable for the lifetime of a
/// thread — once known, they don't change. Older `thread/list` responses
/// (and some upstream snapshot paths) omit them, so without this guard a
/// later list-page upsert silently wipes a fork relationship learned via
/// an earlier `thread/read`. Mirrors `preserve_thread_title`.
pub(super) fn preserve_thread_fork_lineage(existing: &ThreadInfo, incoming: &mut ThreadInfo) {
    let is_blank = |value: &Option<String>| -> bool {
        value.as_deref().map(str::trim).is_none_or(str::is_empty)
    };
    if is_blank(&incoming.forked_from_id) && !is_blank(&existing.forked_from_id) {
        incoming.forked_from_id = existing.forked_from_id.clone();
    }
    if is_blank(&incoming.parent_thread_id) && !is_blank(&existing.parent_thread_id) {
        incoming.parent_thread_id = existing.parent_thread_id.clone();
    }
}

pub(super) fn preserve_queued_follow_ups(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    if target.queued_follow_ups.is_empty() {
        target.queued_follow_ups = source.queued_follow_ups.clone();
    }
    if target.queued_follow_up_drafts.is_empty() {
        target.queued_follow_up_drafts = source.queued_follow_up_drafts.clone();
    }
}

pub(super) fn sync_thread_follow_up_projection(thread: &mut ThreadSnapshot) {
    thread.queued_follow_ups = thread
        .queued_follow_up_drafts
        .iter()
        .map(|draft| draft.preview.clone())
        .collect();
}

pub(crate) fn remove_first_queued_follow_up(thread: &mut ThreadSnapshot) {
    if !thread.queued_follow_up_drafts.is_empty() {
        thread.queued_follow_up_drafts.remove(0);
        sync_thread_follow_up_projection(thread);
        return;
    }
    if !thread.queued_follow_ups.is_empty() {
        thread.queued_follow_ups.remove(0);
    }
}

pub(super) fn reanchor_queued_follow_ups(thread: &mut ThreadSnapshot, turn_id: &str) {
    for draft in &mut thread.queued_follow_up_drafts {
        draft.causal_anchor_turn_id = Some(turn_id.to_string());
    }
}

pub(super) fn is_duplicate_overlay_item(
    local: &HydratedConversationItem,
    existing: &HydratedConversationItem,
) -> bool {
    if local.id == existing.id && local.id.starts_with(USER_INPUT_RESPONSE_ITEM_PREFIX) {
        return true;
    }

    match (&local.content, &existing.content) {
        (
            HydratedConversationItemContent::UserInputResponse(local_data),
            HydratedConversationItemContent::UserInputResponse(existing_data),
        ) => local.source_turn_id == existing.source_turn_id && local_data == existing_data,
        (
            HydratedConversationItemContent::User(local_data),
            HydratedConversationItemContent::User(existing_data),
        ) => {
            local.id.starts_with(LOCAL_USER_MESSAGE_ITEM_PREFIX)
                && local.source_turn_id.is_some()
                && local.source_turn_id == existing.source_turn_id
                && local_data == existing_data
        }
        _ => false,
    }
}

pub(super) fn is_superseded_overlay_item(
    local: &HydratedConversationItem,
    existing: &HydratedConversationItem,
    _active_turn_id: Option<&str>,
) -> bool {
    if is_duplicate_overlay_item(local, existing) {
        return true;
    }

    match (&local.content, &existing.content) {
        (
            HydratedConversationItemContent::User(local_data),
            HydratedConversationItemContent::User(existing_data),
        ) => {
            local.id.starts_with(LOCAL_USER_MESSAGE_ITEM_PREFIX)
                && local.is_from_user_turn_boundary
                && existing.is_from_user_turn_boundary
                && local_data == existing_data
        }
        _ => false,
    }
}

pub(super) fn answered_user_input_item(
    request: &PendingUserInputRequest,
    answers: &[PendingUserInputAnswer],
) -> HydratedConversationItem {
    let content =
        HydratedConversationItemContent::UserInputResponse(HydratedUserInputResponseData {
            questions: request
                .questions
                .iter()
                .map(|question| {
                    let answer = answers
                        .iter()
                        .find(|answer| answer.question_id == question.id)
                        .map(|answer| {
                            let hide_other_placeholder = answer
                                .answers
                                .iter()
                                .any(|entry| is_user_input_note_answer(entry));
                            answer
                                .answers
                                .iter()
                                .filter_map(|entry| {
                                    display_user_input_answer(entry, hide_other_placeholder)
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    HydratedUserInputResponseQuestionData {
                        id: question.id.clone(),
                        header: question.header.clone(),
                        question: question.question.clone(),
                        answer,
                        options: question
                            .options
                            .iter()
                            .map(|option| HydratedUserInputResponseOptionData {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                    }
                })
                .collect(),
        });

    HydratedConversationItem {
        id: format!("{USER_INPUT_RESPONSE_ITEM_PREFIX}{}", request.id),
        content,
        source_turn_id: Some(request.turn_id.clone()),
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: false,
    }
}

fn is_user_input_note_answer(answer: &str) -> bool {
    answer.trim().starts_with(USER_INPUT_NOTE_PREFIX)
}

fn display_user_input_answer(answer: &str, hide_other_placeholder: bool) -> Option<String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(note) = trimmed.strip_prefix(USER_INPUT_NOTE_PREFIX) {
        let note = note.trim();
        return (!note.is_empty()).then(|| note.to_string());
    }
    if hide_other_placeholder && trimmed == USER_INPUT_OTHER_OPTION_LABEL {
        return None;
    }
    Some(trimmed.to_string())
}

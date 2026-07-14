use super::*;

const USER_INPUT_NOTE_PREFIX: &str = "user_note: ";
const USER_INPUT_OTHER_OPTION_LABEL: &str = "None of the above";
const USER_INPUT_RECONCILE_DELAYS_MS: [u64; 3] = [150, 800, 2500];
const MCP_APPROVAL_FIELD_ID: &str = "__approval";
const MCP_URL_ACTION_FIELD_ID: &str = "__url_action";
const MCP_APPROVAL_ACCEPT_ONCE_LABEL: &str = "Allow";
const MCP_APPROVAL_ACCEPT_SESSION_LABEL: &str = "Allow for this session";
const MCP_APPROVAL_ACCEPT_ALWAYS_LABEL: &str = "Always allow";
const MCP_APPROVAL_DECLINE_LABEL: &str = "Deny";
const MCP_APPROVAL_CANCEL_LABEL: &str = "Cancel";
const MCP_URL_FINISHED_LABEL: &str = "I finished";

pub(super) fn normalize_pending_user_input_answers(
    request: &PendingUserInputRequest,
    answers: &[PendingUserInputAnswer],
) -> Vec<PendingUserInputAnswer> {
    request
        .questions
        .iter()
        .map(|question| {
            let raw_answers = answers
                .iter()
                .find(|answer| answer.question_id == question.id)
                .map(|answer| answer.answers.as_slice())
                .unwrap_or(&[]);
            PendingUserInputAnswer {
                question_id: question.id.clone(),
                answers: normalize_pending_user_input_answer_entries(question, raw_answers),
            }
        })
        .collect()
}

fn normalize_pending_user_input_answer_entries(
    question: &crate::types::PendingUserInputQuestion,
    raw_answers: &[String],
) -> Vec<String> {
    let mut selected_options = Vec::new();
    let mut note_parts = Vec::new();

    for raw_answer in raw_answers {
        let trimmed = raw_answer.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(note) = trimmed.strip_prefix(USER_INPUT_NOTE_PREFIX) {
            let note = note.trim();
            if !note.is_empty() {
                note_parts.push(note.to_string());
            }
            continue;
        }

        if !question.options.is_empty()
            && (question
                .options
                .iter()
                .any(|option| option.label == trimmed)
                || trimmed == USER_INPUT_OTHER_OPTION_LABEL)
        {
            selected_options.push(trimmed.to_string());
        } else {
            note_parts.push(trimmed.to_string());
        }
    }

    if question.options.is_empty() {
        return if note_parts.is_empty() {
            Vec::new()
        } else {
            vec![format!("{USER_INPUT_NOTE_PREFIX}{}", note_parts.join("\n"))]
        };
    }

    if question.is_other_allowed && !note_parts.is_empty() && selected_options.is_empty() {
        selected_options.push(USER_INPUT_OTHER_OPTION_LABEL.to_string());
    }

    if note_parts.is_empty() {
        return selected_options;
    }

    let mut normalized = selected_options;
    normalized.push(format!("{USER_INPUT_NOTE_PREFIX}{}", note_parts.join("\n")));
    normalized
}

fn pending_user_input_first_answer<'a>(
    answers: &'a [PendingUserInputAnswer],
    question_id: &str,
) -> Option<&'a str> {
    answers
        .iter()
        .find(|answer| answer.question_id == question_id)
        .and_then(|answer| {
            answer
                .answers
                .iter()
                .find_map(|entry| non_empty_trimmed(entry))
        })
}

fn mcp_elicitation_response_json(
    seed: &PendingUserInputSeed,
    answers: &[PendingUserInputAnswer],
) -> Result<serde_json::Value, RpcError> {
    let params: upstream::McpServerElicitationRequestParams =
        serde_json::from_value(seed.raw_params.clone()).map_err(|error| {
            RpcError::Deserialization(format!("deserialize MCP elicitation params: {error}"))
        })?;
    let response = match &params.request {
        upstream::McpServerElicitationRequest::Form {
            requested_schema, ..
        } if requested_schema.properties.is_empty() => {
            let (action, meta) = mcp_approval_action_response(answers);
            upstream::McpServerElicitationRequestResponse {
                action,
                content: None,
                meta,
            }
        }
        upstream::McpServerElicitationRequest::Form {
            requested_schema, ..
        } => {
            let mut content = serde_json::Map::new();
            for (id, schema) in &requested_schema.properties {
                if let Some(value) = mcp_elicitation_answer_value(schema, id, answers) {
                    content.insert(id.clone(), value);
                }
            }
            upstream::McpServerElicitationRequestResponse {
                action: upstream::McpServerElicitationAction::Accept,
                content: Some(serde_json::Value::Object(content)),
                meta: None,
            }
        }
        upstream::McpServerElicitationRequest::Url { .. } => {
            let answer = pending_user_input_first_answer(answers, MCP_URL_ACTION_FIELD_ID);
            let action = match answer {
                Some(MCP_URL_FINISHED_LABEL) => upstream::McpServerElicitationAction::Accept,
                Some(MCP_APPROVAL_CANCEL_LABEL) => upstream::McpServerElicitationAction::Cancel,
                _ => upstream::McpServerElicitationAction::Cancel,
            };
            upstream::McpServerElicitationRequestResponse {
                action,
                content: None,
                meta: None,
            }
        }
    };
    serde_json::to_value(response)
        .map_err(|error| RpcError::Deserialization(format!("serialize MCP response: {error}")))
}

fn mcp_approval_action_response(
    answers: &[PendingUserInputAnswer],
) -> (
    upstream::McpServerElicitationAction,
    Option<serde_json::Value>,
) {
    match pending_user_input_first_answer(answers, MCP_APPROVAL_FIELD_ID) {
        Some(MCP_APPROVAL_ACCEPT_SESSION_LABEL) => (
            upstream::McpServerElicitationAction::Accept,
            Some(serde_json::json!({
                codex_protocol::mcp_approval_meta::PERSIST_KEY:
                    codex_protocol::mcp_approval_meta::PERSIST_SESSION,
            })),
        ),
        Some(MCP_APPROVAL_ACCEPT_ALWAYS_LABEL) => (
            upstream::McpServerElicitationAction::Accept,
            Some(serde_json::json!({
                codex_protocol::mcp_approval_meta::PERSIST_KEY:
                    codex_protocol::mcp_approval_meta::PERSIST_ALWAYS,
            })),
        ),
        Some(MCP_APPROVAL_DECLINE_LABEL) => (upstream::McpServerElicitationAction::Decline, None),
        Some(MCP_APPROVAL_CANCEL_LABEL) => (upstream::McpServerElicitationAction::Cancel, None),
        Some(MCP_APPROVAL_ACCEPT_ONCE_LABEL) => {
            (upstream::McpServerElicitationAction::Accept, None)
        }
        _ => (upstream::McpServerElicitationAction::Cancel, None),
    }
}

fn mcp_elicitation_answer_value(
    schema: &upstream::McpElicitationPrimitiveSchema,
    question_id: &str,
    answers: &[PendingUserInputAnswer],
) -> Option<serde_json::Value> {
    match schema {
        upstream::McpElicitationPrimitiveSchema::String(_) => {
            pending_user_input_first_answer(answers, question_id)
                .map(|value| serde_json::Value::String(value.to_string()))
        }
        upstream::McpElicitationPrimitiveSchema::Number(schema) => {
            let answer = pending_user_input_first_answer(answers, question_id)?;
            match schema.type_ {
                upstream::McpElicitationNumberType::Integer => answer
                    .parse::<i64>()
                    .ok()
                    .map(|value| serde_json::Value::Number(value.into())),
                upstream::McpElicitationNumberType::Number => answer
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(serde_json::Value::Number),
            }
        }
        upstream::McpElicitationPrimitiveSchema::Boolean(_) => {
            let answer = pending_user_input_first_answer(answers, question_id)?;
            parse_bool_answer(answer).map(serde_json::Value::Bool)
        }
        upstream::McpElicitationPrimitiveSchema::Enum(schema) => {
            mcp_enum_answer_value(schema, question_id, answers)
        }
    }
}

fn parse_bool_answer(answer: &str) -> Option<bool> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "allow" => Some(true),
        "false" | "no" | "n" | "0" | "deny" => Some(false),
        _ => None,
    }
}

fn mcp_enum_answer_value(
    schema: &upstream::McpElicitationEnumSchema,
    question_id: &str,
    answers: &[PendingUserInputAnswer],
) -> Option<serde_json::Value> {
    match schema {
        upstream::McpElicitationEnumSchema::Legacy(schema) => {
            let answer = pending_user_input_first_answer(answers, question_id)?;
            let enum_names = schema.enum_names.clone().unwrap_or_default();
            schema.enum_.iter().enumerate().find_map(|(index, value)| {
                let label = enum_names.get(index).unwrap_or(value);
                (answer == label || answer == value)
                    .then(|| serde_json::Value::String(value.clone()))
            })
        }
        upstream::McpElicitationEnumSchema::SingleSelect(schema) => {
            let answer = pending_user_input_first_answer(answers, question_id)?;
            match schema {
                upstream::McpElicitationSingleSelectEnumSchema::Untitled(schema) => schema
                    .enum_
                    .iter()
                    .find(|value| answer == value.as_str())
                    .map(|value| serde_json::Value::String(value.clone())),
                upstream::McpElicitationSingleSelectEnumSchema::Titled(schema) => schema
                    .one_of
                    .iter()
                    .find(|entry| answer == entry.title || answer == entry.const_)
                    .map(|entry| serde_json::Value::String(entry.const_.clone())),
            }
        }
        upstream::McpElicitationEnumSchema::MultiSelect(schema) => {
            let raw_answers = answers
                .iter()
                .find(|answer| answer.question_id == question_id)?
                .answers
                .iter()
                .filter_map(|answer| non_empty_trimmed(answer))
                .collect::<Vec<_>>();
            let values = match schema {
                upstream::McpElicitationMultiSelectEnumSchema::Untitled(schema) => raw_answers
                    .into_iter()
                    .filter_map(|answer| {
                        schema
                            .items
                            .enum_
                            .iter()
                            .find(|value| answer == value.as_str())
                            .cloned()
                    })
                    .map(serde_json::Value::String)
                    .collect::<Vec<_>>(),
                upstream::McpElicitationMultiSelectEnumSchema::Titled(schema) => raw_answers
                    .into_iter()
                    .filter_map(|answer| {
                        schema
                            .items
                            .any_of
                            .iter()
                            .find(|entry| answer == entry.title || answer == entry.const_)
                            .map(|entry| entry.const_.clone())
                    })
                    .map(serde_json::Value::String)
                    .collect::<Vec<_>>(),
            };
            Some(serde_json::Value::Array(values))
        }
    }
}

impl MobileClient {
    pub async fn respond_to_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecisionValue,
    ) -> Result<(), RpcError> {
        let approval = self.pending_approval(request_id)?;
        let approval_seed = self
            .app_store
            .pending_approval_seed(&approval.server_id, &approval.id);
        let session = self.get_session(&approval.server_id)?;
        let response_json = approval_response_json(&approval, approval_seed.as_ref(), decision)?;
        let response_request_id =
            server_request_id_json(approval_request_id(&approval, approval_seed.as_ref()));
        let runtime_kind = approval
            .thread_id
            .as_ref()
            .map(|thread_id| {
                self.runtime_for_thread(&ThreadKey {
                    server_id: approval.server_id.clone(),
                    thread_id: thread_id.clone(),
                })
            })
            .unwrap_or_else(|| "codex".to_string());
        let direct_command_id = self.app_store.begin_server_mutating_command(
            &approval.server_id,
            ServerMutatingCommandKind::ApprovalResponse,
            approval.thread_id.as_deref().unwrap_or(""),
        );
        if let Err(error) = session
            .respond_for_runtime(runtime_kind, response_request_id, response_json)
            .await
        {
            self.app_store
                .finish_server_mutating_command_failure(&approval.server_id, &direct_command_id);
            return Err(error);
        }
        self.app_store
            .finish_server_mutating_command_success(&approval.server_id, &direct_command_id);
        debug!(
            "MobileClient: approval response sent for server={} request_id={}",
            approval.server_id, request_id
        );
        self.app_store.resolve_approval(request_id);
        Ok(())
    }

    pub async fn respond_to_user_input(
        &self,
        request_id: &str,
        answers: Vec<PendingUserInputAnswer>,
    ) -> Result<(), RpcError> {
        let request = self.pending_user_input(request_id)?;
        let seed = self
            .app_store
            .pending_user_input_seed(&request.server_id, &request.id);
        let normalized_answers = normalize_pending_user_input_answers(&request, &answers);
        let answered_inputs = normalized_answers.clone();
        let session = self.get_session(&request.server_id)?;
        if let Some(seed) = seed.as_ref()
            && matches!(
                seed.response_kind,
                PendingUserInputResponseKind::McpServerElicitation
            )
        {
            let response_json = mcp_elicitation_response_json(seed, &answers)?;
            let response_request_id = server_request_id_json(seed.request_id.clone());
            let runtime_kind = self.runtime_for_thread(&ThreadKey {
                server_id: request.server_id.clone(),
                thread_id: request.thread_id.clone(),
            });
            let direct_command_id = self.app_store.begin_server_mutating_command(
                &request.server_id,
                ServerMutatingCommandKind::UserInputResponse,
                &request.thread_id,
            );
            if let Err(error) = session
                .respond_for_runtime(runtime_kind, response_request_id, response_json)
                .await
            {
                self.app_store
                    .finish_server_mutating_command_failure(&request.server_id, &direct_command_id);
                return Err(error);
            }
            self.app_store
                .finish_server_mutating_command_success(&request.server_id, &direct_command_id);
            debug!(
                "MobileClient: MCP elicitation response sent for server={} request_id={}",
                request.server_id, request_id
            );
            self.app_store
                .resolve_pending_user_input_with_response(request_id, answered_inputs);
            self.spawn_post_user_input_reconcile(
                request.server_id.clone(),
                request.thread_id.clone(),
                Arc::clone(&session),
            );
            return Ok(());
        }
        let response = upstream::ToolRequestUserInputResponse {
            answers: normalized_answers
                .into_iter()
                .map(|answer| {
                    (
                        answer.question_id,
                        upstream::ToolRequestUserInputAnswer {
                            answers: answer.answers,
                        },
                    )
                })
                .collect::<HashMap<_, _>>(),
        };
        let response_json = serde_json::to_value(response).map_err(|e| {
            RpcError::Deserialization(format!("serialize user input response: {e}"))
        })?;
        let response_request_id = server_request_id_json(
            seed.as_ref()
                .filter(|seed| {
                    matches!(
                        seed.response_kind,
                        PendingUserInputResponseKind::ToolRequestUserInput
                    )
                })
                .map(|seed| seed.request_id.clone())
                .unwrap_or_else(|| fallback_server_request_id(&request.id)),
        );
        let runtime_kind = self.runtime_for_thread(&ThreadKey {
            server_id: request.server_id.clone(),
            thread_id: request.thread_id.clone(),
        });
        let direct_command_id = self.app_store.begin_server_mutating_command(
            &request.server_id,
            ServerMutatingCommandKind::UserInputResponse,
            &request.thread_id,
        );
        if let Err(error) = session
            .respond_for_runtime(runtime_kind, response_request_id, response_json)
            .await
        {
            self.app_store
                .finish_server_mutating_command_failure(&request.server_id, &direct_command_id);
            return Err(error);
        }
        self.app_store
            .finish_server_mutating_command_success(&request.server_id, &direct_command_id);
        debug!(
            "MobileClient: user input response sent for server={} request_id={}",
            request.server_id, request_id
        );
        self.app_store
            .resolve_pending_user_input_with_response(request_id, answered_inputs);
        self.spawn_post_user_input_reconcile(
            request.server_id.clone(),
            request.thread_id.clone(),
            Arc::clone(&session),
        );
        Ok(())
    }

    pub(super) fn spawn_post_user_input_reconcile(
        &self,
        server_id: String,
        thread_id: String,
        session: Arc<ServerSession>,
    ) {
        let app_store = Arc::clone(&self.app_store);
        Self::spawn_detached(async move {
            for delay_ms in USER_INPUT_RECONCILE_DELAYS_MS {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                match read_thread_response_from_app_server(Arc::clone(&session), &thread_id, true)
                    .await
                {
                    Ok(response) => {
                        if let Err(error) = upsert_thread_snapshot_from_app_server_read_response(
                            &app_store, &server_id, response,
                        ) {
                            warn!(
                                "MobileClient: failed to reconcile thread after user input for server={} thread={}: {}",
                                server_id, thread_id, error
                            );
                            continue;
                        }
                        let key = ThreadKey {
                            server_id: server_id.clone(),
                            thread_id: thread_id.clone(),
                        };
                        let should_keep_polling = app_store
                            .snapshot()
                            .threads
                            .get(&key)
                            .is_some_and(|thread| thread.active_turn_id.is_some());
                        if !should_keep_polling {
                            break;
                        }
                    }
                    Err(error) => {
                        warn!(
                            "MobileClient: failed to refresh thread after user input for server={} thread={}: {}",
                            server_id, thread_id, error
                        );
                    }
                }
            }
        });
    }
}

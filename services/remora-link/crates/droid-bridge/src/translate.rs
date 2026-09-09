use std::collections::HashMap;

use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct CompletedTurn {
    pub turn_id: String,
    pub items: Vec<Value>,
    pub completed_at: i64,
    pub duration_ms: i64,
}

#[derive(Debug)]
pub struct DroidTurnTranslator {
    thread_id: String,
    turn_id: String,
    cwd: String,
    started_at: i64,
    items: Vec<Value>,
    assistant: HashMap<String, TextItem>,
    reasoning: HashMap<String, TextItem>,
    tools: HashMap<String, ToolItem>,
    completed: Option<CompletedTurn>,
}

#[derive(Debug, Default)]
struct TextItem {
    text: String,
    started: bool,
}

#[derive(Debug, Default)]
struct ToolItem {
    name: String,
    input: Value,
    started: bool,
    output: String,
}

impl DroidTurnTranslator {
    pub fn new(
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
        cwd: impl Into<String>,
        started_at: i64,
    ) -> Self {
        Self {
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
            cwd: cwd.into(),
            started_at,
            items: Vec::new(),
            assistant: HashMap::new(),
            reasoning: HashMap::new(),
            tools: HashMap::new(),
            completed: None,
        }
    }

    pub fn completed(&self) -> Option<CompletedTurn> {
        self.completed.clone()
    }

    pub fn translate_frame(&mut self, frame: &Value) -> Vec<(String, Value)> {
        if frame.get("method").and_then(Value::as_str) != Some("droid.session_notification") {
            return Vec::new();
        }
        let notification = frame
            .pointer("/params/notification")
            .cloned()
            .unwrap_or(Value::Null);
        let ty = notification
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match ty {
            "droid_working_state_changed" => self.working_state_changed(&notification),
            "create_message" => self.create_message(&notification),
            "assistant_text_delta" => self.assistant_delta(&notification),
            "assistant_text_complete" => self.assistant_complete(&notification),
            "thinking_text_delta" => self.reasoning_delta(&notification),
            "thinking_text_complete" => self.reasoning_complete(&notification),
            "tool_call" => self.tool_call(&notification),
            "tool_progress_update" => self.tool_progress_update(&notification),
            "tool_result" => self.tool_result(&notification),
            "session_token_usage_changed" => self.token_usage_changed(&notification),
            "session_title_updated" => self.title_updated(&notification),
            "error" => self.error_notification(&notification),
            _ => Vec::new(),
        }
    }

    fn working_state_changed(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(state) = notification.get("newState").and_then(Value::as_str) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let status = match state {
            "idle" => json!({"type": "idle"}),
            _ => json!({"type": "active", "activeFlags": []}),
        };
        out.push((
            "thread/status/changed".to_string(),
            json!({"threadId": self.thread_id, "status": status}),
        ));
        if state == "idle" && self.completed.is_none() {
            let completed_at = now_secs();
            let duration_ms = (completed_at - self.started_at).max(0) * 1000;
            let turn = completed_turn_json(
                &self.turn_id,
                &self.items,
                self.started_at,
                completed_at,
                duration_ms,
            );
            out.push((
                "turn/completed".to_string(),
                json!({"threadId": self.thread_id, "turn": turn}),
            ));
            self.completed = Some(CompletedTurn {
                turn_id: self.turn_id.clone(),
                items: self.items.clone(),
                completed_at,
                duration_ms,
            });
        }
        out
    }

    fn create_message(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let message = notification.get("message").unwrap_or(&Value::Null);
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() || role != "user" {
            return Vec::new();
        }
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|part| {
                let text = part.get("text").and_then(Value::as_str)?;
                Some(json!({"type": "text", "text": text, "text_elements": []}))
            })
            .collect::<Vec<_>>();
        if content.is_empty() {
            return Vec::new();
        }
        let item = json!({"type": "userMessage", "id": id, "content": content});
        self.items.push(item.clone());
        vec![
            (
                "item/started".to_string(),
                json!({"threadId": self.thread_id, "turnId": self.turn_id, "item": item}),
            ),
            (
                "item/completed".to_string(),
                json!({"threadId": self.thread_id, "turnId": self.turn_id, "item": item}),
            ),
        ]
    }

    fn assistant_delta(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(message_id) = notification.get("messageId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let delta = notification
            .get("textDelta")
            .and_then(Value::as_str)
            .unwrap_or("");
        let thread_id = self.thread_id.clone();
        let turn_id = self.turn_id.clone();
        let entry = self.assistant.entry(message_id.to_string()).or_default();
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push((
                "item/started".to_string(),
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "item": {"type": "agentMessage", "id": message_id, "text": ""}
                }),
            ));
        }
        entry.text.push_str(delta);
        if !delta.is_empty() {
            out.push((
                "item/agentMessage/delta".to_string(),
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.turn_id,
                    "itemId": message_id,
                    "delta": delta
                }),
            ));
        }
        out
    }

    fn assistant_complete(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(message_id) = notification.get("messageId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let entry = self.assistant.remove(message_id).unwrap_or_default();
        let item = json!({"type": "agentMessage", "id": message_id, "text": entry.text});
        self.items.push(item.clone());
        vec![(
            "item/completed".to_string(),
            json!({"threadId": self.thread_id, "turnId": self.turn_id, "item": item}),
        )]
    }

    fn reasoning_delta(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(message_id) = notification.get("messageId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let delta = notification
            .get("textDelta")
            .and_then(Value::as_str)
            .unwrap_or("");
        let content_index = notification
            .get("blockIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let item_id = format!("{message_id}:thinking:{content_index}");
        let entry = self.reasoning.entry(item_id.clone()).or_default();
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push((
                "item/started".to_string(),
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.turn_id,
                    "item": {"type": "reasoning", "id": item_id, "summary": [], "content": []}
                }),
            ));
        }
        entry.text.push_str(delta);
        if !delta.is_empty() {
            out.push((
                "item/reasoning/textDelta".to_string(),
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.turn_id,
                    "itemId": item_id,
                    "delta": delta,
                    "contentIndex": content_index
                }),
            ));
        }
        out
    }

    fn reasoning_complete(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(message_id) = notification.get("messageId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let content_index = notification
            .get("blockIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let item_id = format!("{message_id}:thinking:{content_index}");
        let Some(entry) = self.reasoning.remove(&item_id) else {
            return Vec::new();
        };
        let item = json!({
            "type": "reasoning",
            "id": item_id,
            "summary": [],
            "content": [entry.text]
        });
        self.items.push(item.clone());
        vec![(
            "item/completed".to_string(),
            json!({"threadId": self.thread_id, "turnId": self.turn_id, "item": item}),
        )]
    }

    fn tool_call(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let tool_use = notification.get("toolUse").unwrap_or(&Value::Null);
        let Some(id) = tool_use.get("id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let name = tool_use
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_string();
        let input = tool_use.get("input").cloned().unwrap_or_else(|| json!({}));
        let entry = self.tools.entry(id.to_string()).or_default();
        entry.name = name;
        entry.input = input;
        if !should_start_tool(entry) {
            return Vec::new();
        }
        entry.started = true;
        vec![(
            "item/started".to_string(),
            json!({
                "threadId": self.thread_id,
                "turnId": self.turn_id,
                "item": tool_item_json(id, entry, &self.cwd, false)
            }),
        )]
    }

    fn tool_progress_update(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(id) = notification.get("toolUseId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(entry) = self.tools.get_mut(id) else {
            return Vec::new();
        };
        if entry.name != "Execute" {
            return Vec::new();
        }
        let update = notification.get("update").unwrap_or(&Value::Null);
        let full = update
            .get("fullOutput")
            .and_then(Value::as_str)
            .or_else(|| update.get("text").and_then(Value::as_str))
            .unwrap_or("");
        if full.len() <= entry.output.len() {
            return Vec::new();
        }
        let delta = full[entry.output.len()..].to_string();
        entry.output = full.to_string();
        vec![(
            "item/commandExecution/outputDelta".to_string(),
            json!({
                "threadId": self.thread_id,
                "turnId": self.turn_id,
                "itemId": id,
                "delta": delta
            }),
        )]
    }

    fn tool_result(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let Some(id) = notification.get("toolUseId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let mut entry = self.tools.remove(id).unwrap_or_default();
        if entry.name.is_empty() {
            entry.name = notification
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
                .to_string();
        }
        let content = notification
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !content.is_empty() && entry.output.is_empty() {
            entry.output = content.clone();
        }
        let is_error = notification
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut out = Vec::new();
        if !entry.started {
            out.push((
                "item/started".to_string(),
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.turn_id,
                    "item": tool_item_json(id, &entry, &self.cwd, false)
                }),
            ));
        }
        let item = completed_tool_item_json(id, &entry, &self.cwd, &content, is_error);
        self.items.push(item.clone());
        out.push((
            "item/completed".to_string(),
            json!({"threadId": self.thread_id, "turnId": self.turn_id, "item": item}),
        ));
        out
    }

    fn token_usage_changed(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let total = usage_json(notification.get("tokenUsage").unwrap_or(&Value::Null));
        let last = usage_json(
            notification
                .get("lastCallTokenUsage")
                .unwrap_or(&Value::Null),
        );
        vec![(
            "thread/tokenUsage/updated".to_string(),
            json!({
                "threadId": self.thread_id,
                "turnId": self.turn_id,
                "tokenUsage": {
                    "total": total,
                    "last": last,
                    "modelContextWindow": null
                }
            }),
        )]
    }

    fn title_updated(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let title = notification.get("title").and_then(Value::as_str);
        vec![(
            "thread/name/updated".to_string(),
            json!({"threadId": self.thread_id, "threadName": title}),
        )]
    }

    fn error_notification(&mut self, notification: &Value) -> Vec<(String, Value)> {
        let message = notification
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("droid error");
        vec![(
            "error".to_string(),
            json!({
                "threadId": self.thread_id,
                "turnId": self.turn_id,
                "willRetry": false,
                "error": {
                    "message": message,
                    "type": "unexpected",
                    "codexErrorInfo": null,
                    "additionalDetails": null
                }
            }),
        )]
    }
}

fn should_start_tool(tool: &ToolItem) -> bool {
    if tool.started {
        return false;
    }
    if tool.name == "Execute" {
        tool.input.get("command").and_then(Value::as_str).is_some()
    } else {
        !matches!(tool.input, Value::Object(ref map) if map.is_empty())
    }
}

fn tool_item_json(id: &str, tool: &ToolItem, cwd: &str, completed: bool) -> Value {
    match tool.name.as_str() {
        "Execute" => command_execution_json(
            id,
            tool.input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or(""),
            cwd,
            if completed { "completed" } else { "inProgress" },
            Vec::new(),
            None,
        ),
        "Read" | "LS" | "Grep" | "Glob" => {
            let command = factory_tool_command(&tool.name, &tool.input);
            command_execution_json(
                id,
                &command,
                cwd,
                if completed { "completed" } else { "inProgress" },
                factory_command_actions(&tool.name, &tool.input, &command),
                None,
            )
        }
        "Edit" | "Create" | "ApplyPatch" => json!({
            "type": "fileChange",
            "id": id,
            "changes": [],
            "status": if completed { "completed" } else { "inProgress" }
        }),
        _ => json!({
            "type": "dynamicToolCall",
            "id": id,
            "tool": tool.name.clone(),
            "arguments": tool.input.clone(),
            "status": if completed { "completed" } else { "inProgress" }
        }),
    }
}

fn completed_tool_item_json(
    id: &str,
    tool: &ToolItem,
    cwd: &str,
    content: &str,
    is_error: bool,
) -> Value {
    match tool.name.as_str() {
        "Execute" => {
            let mut item = command_execution_json(
                id,
                tool.input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                cwd,
                if is_error { "failed" } else { "completed" },
                Vec::new(),
                Some(content),
            );
            if let Some(code) = parse_exit_code(content) {
                item["exitCode"] = json!(code);
            }
            item
        }
        "Read" | "LS" | "Grep" | "Glob" => {
            let command = factory_tool_command(&tool.name, &tool.input);
            command_execution_json(
                id,
                &command,
                cwd,
                if is_error { "failed" } else { "completed" },
                factory_command_actions(&tool.name, &tool.input, &command),
                Some(content),
            )
        }
        "Edit" | "Create" | "ApplyPatch" => json!({
            "type": "fileChange",
            "id": id,
            "changes": [],
            "status": if is_error { "failed" } else { "completed" }
        }),
        _ => json!({
            "type": "dynamicToolCall",
            "id": id,
            "tool": tool.name.clone(),
            "arguments": tool.input.clone(),
            "status": if is_error { "failed" } else { "completed" },
            "success": !is_error,
            "contentItems": [{"type": "text", "text": content}]
        }),
    }
}

fn command_execution_json(
    id: &str,
    command: &str,
    cwd: &str,
    status: &str,
    command_actions: Vec<Value>,
    aggregated_output: Option<&str>,
) -> Value {
    let mut item = json!({
        "type": "commandExecution",
        "id": id,
        "command": command,
        "cwd": cwd,
        "status": status,
        "commandActions": command_actions
    });
    if let Some(output) = aggregated_output {
        item["aggregatedOutput"] = json!(output);
    }
    item
}

fn factory_tool_command(name: &str, input: &Value) -> String {
    match name {
        "Read" => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .map(|path| format!("read {path}"))
            .unwrap_or_else(|| "read".to_string()),
        "LS" => input
            .get("directory_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .map(|path| format!("ls {path}"))
            .unwrap_or_else(|| "ls".to_string()),
        "Grep" => {
            let pattern = input.get("pattern").and_then(Value::as_str);
            let path = input.get("path").and_then(Value::as_str);
            match (pattern, path) {
                (Some(pattern), Some(path)) => format!("grep {pattern} {path}"),
                (Some(pattern), None) => format!("grep {pattern}"),
                (None, Some(path)) => format!("grep {path}"),
                (None, None) => "grep".to_string(),
            }
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .or_else(|| input.get("patterns"))
                .and_then(factory_pattern_text);
            let path = input
                .get("folder")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str);
            match (pattern, path) {
                (Some(pattern), Some(path)) => format!("glob {pattern} {path}"),
                (Some(pattern), None) => format!("glob {pattern}"),
                (None, Some(path)) => format!("glob {path}"),
                (None, None) => "glob".to_string(),
            }
        }
        _ => name.to_string(),
    }
}

fn factory_command_actions(name: &str, input: &Value, command: &str) -> Vec<Value> {
    match name {
        "Read" => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .map(|path| {
                json!({
                    "type": "read",
                    "command": command,
                    "name": file_name(path),
                    "path": path
                })
            })
            .into_iter()
            .collect(),
        "LS" => input
            .get("directory_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .map(|path| json!({"type": "listFiles", "command": command, "path": path}))
            .into_iter()
            .collect(),
        "Grep" => vec![json!({
            "type": "search",
            "command": command,
            "query": input.get("pattern").and_then(Value::as_str),
            "path": input.get("path").and_then(Value::as_str)
        })],
        "Glob" => vec![json!({
            "type": "search",
            "command": command,
            "query": input
                .get("pattern")
                .or_else(|| input.get("patterns"))
                .and_then(factory_pattern_text),
            "path": input
                .get("folder")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str)
        })],
        _ => Vec::new(),
    }
}

fn factory_pattern_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("file")
        .to_string()
}

fn usage_json(value: &Value) -> Value {
    let input = read_i64(value, "inputTokens");
    let output = read_i64(value, "outputTokens");
    let cached = read_i64(value, "cacheCreationTokens") + read_i64(value, "cacheReadTokens");
    let reasoning = read_i64(value, "thinkingTokens");
    json!({
        "totalTokens": input + output + cached + reasoning,
        "inputTokens": input,
        "cachedInputTokens": cached,
        "outputTokens": output,
        "reasoningOutputTokens": reasoning
    })
}

fn read_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn completed_turn_json(
    turn_id: &str,
    items: &[Value],
    started_at: i64,
    completed_at: i64,
    duration_ms: i64,
) -> Value {
    json!({
        "id": turn_id,
        "items": items,
        "itemsView": "full",
        "status": "completed",
        "error": null,
        "startedAt": started_at,
        "completedAt": completed_at,
        "durationMs": duration_ms,
    })
}

fn parse_exit_code(content: &str) -> Option<i32> {
    let marker = "[Process exited with code ";
    let start = content.find(marker)? + marker.len();
    let rest = &content[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

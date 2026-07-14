use super::*;

pub(super) fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(super) fn runtime_for_model_hint(value: &str) -> Option<AgentRuntimeKind> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "claude" | "claude-code" | "claude_code" => Some("claude".to_string()),
        "anthropic" => Some("claude".to_string()),
        "amp" | "ampcode" | "amp-code" | "amp_code" | "amp code" => Some("amp".to_string()),
        "opencode" | "open-code" | "open_code" | "open code" => Some("opencode".to_string()),
        "pi" | "pi.dev" | "pidev" | "pi dev" => Some("pi".to_string()),
        "droid" | "factory" | "factory-droid" | "factory_droid" | "factory droid" => {
            Some("droid".to_string())
        }
        "codex" => Some("codex".to_string()),
        // Match patterns like `anthropic/claude-opus-4-7` or
        // `claude-3-5-sonnet` — i.e. a `claude` token anywhere in the
        // hint, after stripping a leading provider prefix. We treat
        // `<segment>/claude...` as Claude even if the leading segment
        // is `anthropic`.
        _ if normalized.starts_with("claude") => Some("claude".to_string()),
        _ if normalized
            .split('/')
            .any(|segment| segment.starts_with("claude")) =>
        {
            Some("claude".to_string())
        }
        _ if normalized.contains("opencode")
            || normalized.contains("open-code")
            || normalized.contains("open_code")
            || normalized.contains("open code") =>
        {
            Some("opencode".to_string())
        }
        _ if normalized.starts_with("amp/")
            || normalized.starts_with("amp:")
            || normalized.starts_with("amp-")
            || normalized.contains("ampcode")
            || normalized.contains("amp-code")
            || normalized.contains("amp_code") =>
        {
            Some("amp".to_string())
        }
        _ if normalized.starts_with("pi.dev")
            || normalized.starts_with("pidev")
            || normalized.starts_with("pi/") =>
        {
            Some("pi".to_string())
        }
        _ if normalized.starts_with("factory/") || normalized.starts_with("droid/") => {
            Some("droid".to_string())
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedModelSelection {
    pub model: String,
    pub runtime_kind: AgentRuntimeKind,
}

impl MobileClient {
    pub(super) fn direct_resumed_threads(&self) -> std::sync::MutexGuard<'_, HashSet<ThreadKey>> {
        match self.direct_resumed_threads.lock() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned direct resume marker lock");
                error.into_inner()
            }
        }
    }

    pub(super) fn thread_runtime_routes(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ThreadKey, AgentRuntimeKind>> {
        match self.thread_runtime_routes.lock() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned thread runtime route lock");
                error.into_inner()
            }
        }
    }

    pub(crate) fn note_thread_runtime(&self, key: ThreadKey, runtime_kind: AgentRuntimeKind) {
        self.thread_runtime_routes()
            .insert(key.clone(), runtime_kind.clone());
        self.app_store.set_thread_agent_runtime(&key, runtime_kind);
    }

    pub(crate) fn runtime_for_thread(&self, key: &ThreadKey) -> AgentRuntimeKind {
        let routed_runtime = self.thread_runtime_routes().get(key).cloned();
        if let Some(runtime_kind) = routed_runtime.clone()
            && runtime_kind != "codex"
        {
            return runtime_kind;
        }

        if let Some(thread) = self.app_store.thread_snapshot(key) {
            if thread.agent_runtime_kind != "codex" {
                return thread.agent_runtime_kind;
            }
            if let Some(runtime_kind) = self.non_codex_runtime_for_thread_metadata(key, &thread) {
                return runtime_kind;
            }
        }

        routed_runtime.unwrap_or_else(|| "codex".to_string())
    }

    pub(crate) fn runtime_for_thread_start(
        &self,
        server_id: &str,
        explicit_runtime_kind: Option<AgentRuntimeKind>,
        model: Option<&str>,
    ) -> AgentRuntimeKind {
        if let Some(runtime_kind) = explicit_runtime_kind {
            return runtime_kind;
        }

        if let Some(model) = model.and_then(non_empty_trimmed) {
            if let Some(selection) = self.resolve_model_selection(server_id, model) {
                return selection.runtime_kind;
            }
            if let Some(runtime_kind) = runtime_for_model_hint(model) {
                return runtime_kind;
            }
        }

        "codex".to_string()
    }

    pub(crate) fn resolve_model_selection(
        &self,
        server_id: &str,
        model: &str,
    ) -> Option<ResolvedModelSelection> {
        let selected_model = non_empty_trimmed(model)?;
        let snapshot = self.app_store.snapshot();
        let models = snapshot.servers.get(server_id)?.available_models.as_ref()?;

        let exact = models
            .iter()
            .find(|candidate| candidate.id == selected_model);
        if let Some(candidate) = exact {
            return Some(ResolvedModelSelection {
                model: candidate.id.clone(),
                runtime_kind: candidate.agent_runtime_kind.clone(),
            });
        }

        if let Some(candidate) = models
            .iter()
            .find(|candidate| candidate.model == selected_model)
        {
            return Some(ResolvedModelSelection {
                model: candidate.id.clone(),
                runtime_kind: candidate.agent_runtime_kind.clone(),
            });
        }

        None
    }

    pub(super) fn runtime_for_selected_model(
        &self,
        server_id: &str,
        model: &str,
    ) -> Option<AgentRuntimeKind> {
        self.resolve_model_selection(server_id, model)
            .map(|selection| selection.runtime_kind)
    }

    pub(super) fn resolve_model_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        model: &str,
    ) -> Option<String> {
        let selected_model = non_empty_trimmed(model)?;
        let snapshot = self.app_store.snapshot();
        let models = snapshot.servers.get(server_id)?.available_models.as_ref()?;
        models
            .iter()
            .find(|candidate| {
                candidate.agent_runtime_kind == runtime_kind
                    && (candidate.id == selected_model || candidate.model == selected_model)
            })
            .map(|candidate| candidate.id.clone())
    }

    pub(crate) fn normalize_thread_model_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        model: &mut Option<String>,
    ) {
        let Some(selected_model) = model.as_deref().and_then(non_empty_trimmed) else {
            return;
        };
        if let Some(resolved) =
            self.resolve_model_for_runtime(server_id, runtime_kind, selected_model)
        {
            *model = Some(resolved);
        }
    }

    pub(crate) fn normalize_model_selection_for_request(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        request: &mut upstream::ClientRequest,
    ) {
        let supports_permission_overrides =
            self.runtime_supports_thread_permission_overrides(&runtime_kind);
        match request {
            upstream::ClientRequest::ThreadStart { params, .. } => {
                self.normalize_thread_model_for_runtime(
                    server_id,
                    runtime_kind.clone(),
                    &mut params.model,
                );
                if !supports_permission_overrides {
                    params.approval_policy = None;
                    params.sandbox = None;
                }
            }
            upstream::ClientRequest::ThreadResume { params, .. } => {
                self.normalize_thread_model_for_runtime(
                    server_id,
                    runtime_kind.clone(),
                    &mut params.model,
                );
                if !supports_permission_overrides {
                    params.approval_policy = None;
                    params.sandbox = None;
                }
            }
            upstream::ClientRequest::ThreadFork { params, .. } => {
                self.normalize_thread_model_for_runtime(
                    server_id,
                    runtime_kind.clone(),
                    &mut params.model,
                );
                if !supports_permission_overrides {
                    params.approval_policy = None;
                    params.sandbox = None;
                }
            }
            upstream::ClientRequest::TurnStart { params, .. } => {
                if let Some(selected_model) = params.model.as_deref().and_then(non_empty_trimmed)
                    && let Some(resolved) =
                        self.resolve_model_for_runtime(server_id, runtime_kind, selected_model)
                {
                    params.model = Some(resolved);
                }
                if !supports_permission_overrides {
                    params.approval_policy = None;
                    params.sandbox_policy = None;
                }
            }
            _ => {}
        }
    }

    pub(super) fn non_codex_runtime_for_thread_metadata(
        &self,
        key: &ThreadKey,
        thread: &ThreadSnapshot,
    ) -> Option<AgentRuntimeKind> {
        for model in [thread.model.as_deref(), thread.info.model.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(non_empty_trimmed)
        {
            let runtime_kind = self
                .runtime_for_selected_model(&key.server_id, model)
                .or_else(|| runtime_for_model_hint(model));
            if let Some(runtime_kind) = runtime_kind
                && runtime_kind != "codex".to_string()
            {
                return Some(runtime_kind);
            }
        }

        if let Some(runtime_kind) = thread
            .info
            .model_provider
            .as_deref()
            .and_then(runtime_for_model_hint)
            .filter(|runtime_kind| *runtime_kind != "codex".to_string())
        {
            return Some(runtime_kind);
        }

        None
    }

    pub(super) fn has_direct_resume_marker(&self, key: &ThreadKey) -> bool {
        self.direct_resumed_threads().contains(key)
    }

    pub(super) fn mark_direct_resumed_thread(&self, key: ThreadKey) {
        self.direct_resumed_threads().insert(key);
    }

    pub(crate) fn clear_direct_resume_markers_for_server(&self, server_id: &str) {
        self.direct_resumed_threads()
            .retain(|key| key.server_id != server_id);
        self.thread_runtime_routes()
            .retain(|key, _| key.server_id != server_id);
    }

    pub(super) fn runtime_supports_thread_permission_overrides(&self, runtime_kind: &str) -> bool {
        self.agent_metadata
            .get(runtime_kind)
            .and_then(|metadata| metadata.capabilities)
            .map(|capabilities| capabilities.supports_thread_permission_overrides)
            // Legacy daemon/client metadata did not expose this capability; keep
            // existing behaviour until a daemon explicitly says overrides are
            // unsupported for the runtime.
            .unwrap_or(true)
    }
}

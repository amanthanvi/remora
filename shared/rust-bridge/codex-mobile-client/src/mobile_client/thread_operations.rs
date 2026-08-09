use super::*;

#[cfg(not(test))]
const TURN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const TURN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

impl MobileClient {
    /// List threads from a specific server.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn list_threads(&self, server_id: &str) -> Result<Vec<ThreadInfo>, RpcError> {
        self.get_session(server_id)?;
        let response = self
            .server_thread_list(
                server_id,
                upstream::ThreadListParams {
                    limit: None,
                    cursor: None,
                    sort_key: None,
                    sort_direction: None,
                    model_providers: None,
                    source_kinds: None,
                    archived: None,
                    cwd: None,
                    search_term: None,
                    use_state_db_only: false,
                },
            )
            .await
            .map_err(map_rpc_client_error)?;
        let threads = response
            .data
            .into_iter()
            .filter_map(thread_info_from_upstream_thread)
            .collect::<Vec<_>>();
        self.app_store.sync_thread_list(server_id, &threads);
        Ok(threads)
    }

    pub async fn external_resume_thread(
        &self,
        server_id: &str,
        thread_id: &str,
        host_id: Option<String>,
    ) -> Result<(), RpcError> {
        self.external_resume_thread_inner(server_id, thread_id, host_id, false, None)
            .await
            .map(|_| ())
    }

    /// Force a fresh `thread/resume` against the server even if a direct
    /// listener was already attached for the current session, and feed
    /// `reconcile_active_turn` enough turn-status info to clear a
    /// locally-cached `active_turn_id` whose underlying turn has finished
    /// while the client was disconnected.
    ///
    /// On paginated remotes (`supports_turn_pagination`) the resume runs
    /// with `exclude_turns: true` and a small follow-up
    /// `thread/turns/list?limit=5&items_view=notLoaded` query supplies the
    /// turn skeletons for reconcile — pulling the entire embedded turn
    /// archive here would OOM mobile clients on long threads. Legacy
    /// remotes that don't implement `thread/turns/list` still pull the
    /// embedded turn list (`exclude_turns: false`), since there is no
    /// other way to learn turn status there.
    ///
    /// Use after a long resume / push wake — the in-flight turn the
    /// client believes is still running may have completed during the
    /// background window with no `TurnCompleted` event delivered.
    pub async fn force_refresh_thread_authoritative(
        &self,
        server_id: &str,
        thread_id: &str,
    ) -> Result<(), RpcError> {
        self.external_resume_thread_inner(server_id, thread_id, None, true, None)
            .await
            .map(|_| ())
    }

    pub(super) async fn force_refresh_thread_authoritative_if_ui_generation(
        &self,
        server_id: &str,
        thread_id: &str,
        expected_generation: u64,
    ) -> Result<bool, RpcError> {
        self.external_resume_thread_inner(
            server_id,
            thread_id,
            None,
            true,
            Some(expected_generation),
        )
        .await
    }

    fn apply_if_lag_refresh_current<R>(
        &self,
        server_id: &str,
        expected_session: &Arc<ServerSession>,
        expected_generation: u64,
        apply: impl FnOnce(&AppStoreReducer) -> R,
    ) -> Option<R> {
        let sessions = self.sessions_read();
        if !sessions
            .get(server_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected_session))
        {
            return None;
        }
        self.app_store
            .apply_if_server_event_generation(server_id, expected_generation, apply)
    }

    async fn external_resume_thread_inner(
        &self,
        server_id: &str,
        thread_id: &str,
        host_id: Option<String>,
        force_authoritative: bool,
        expected_ui_generation: Option<u64>,
    ) -> Result<bool, RpcError> {
        let session = self.get_session(server_id)?;
        if host_id.is_some() {
            trace!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                "external_resume_thread ignoring explicit host_id for server={} thread={}",
                server_id, thread_id
            );
        }
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };

        // Force path skips both short-circuits — caller has out-of-band
        // knowledge that the locally-cached snapshot may have missed
        // turn-completion events.
        if !force_authoritative {
            if self.has_direct_resume_marker(&key) {
                // The marker is set after a successful `thread/resume`
                // for the current session — server-side this means the
                // connection is in the per-thread subscription set. We
                // can skip a duplicate resume when *either* of:
                //   - the thread has loaded turns (items / initial_turns_loaded);
                //   - the server is using pagination (`supports_turn_pagination`),
                //     so a `thread/resume` under `exclude_turns: true`
                //     intentionally returned empty — the data path is
                //     `thread/turns/list`, not another resume.
                // Otherwise (thread truly empty AND pagination off), we
                // need to refresh because the previous resume returned
                // nothing usable.
                let thread_has_loaded_turns = self
                    .app_store
                    .thread_snapshot(&key)
                    .is_some_and(|thread| !thread.items.is_empty() || thread.initial_turns_loaded);
                let pagination_supported =
                    self.app_store.server_supports_turn_pagination(server_id);
                if thread_has_loaded_turns || pagination_supported {
                    debug!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "external_resume_thread: skipping RPC for server={} thread={} — direct listener already attached for current session (loaded={} pagination={})",
                        server_id, thread_id, thread_has_loaded_turns, pagination_supported
                    );
                    self.app_store.mark_thread_resumed(&key, true);
                    return Ok(true);
                }
                debug!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "external_resume_thread: direct listener exists but thread has no loaded turns and pagination is off, refreshing server={} thread={}",
                    server_id, thread_id
                );
            }
        } else {
            debug!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                "external_resume_thread: force-authoritative refresh server={} thread={}",
                server_id, thread_id
            );
        }
        let mut runtime_candidates = vec![self.runtime_for_thread(&key)];
        for runtime_kind in session.runtime_kinds() {
            if !runtime_candidates.contains(&runtime_kind) {
                runtime_candidates.push(runtime_kind);
            }
        }
        if !runtime_candidates.contains(&"codex".to_string()) {
            runtime_candidates.push("codex".to_string());
        }

        let mut lookup_errors = Vec::new();
        for runtime_kind in runtime_candidates.iter().cloned() {
            let supports_pagination = self.app_store.server_supports_turn_pagination(server_id);
            // Paginated servers always exclude turns from the resume
            // response; we never want to pull the full embedded archive,
            // even on the authoritative refresh path — for huge threads
            // that response can be hundreds of MB and OOMs the device.
            // For the authoritative refresh path on paginated servers we
            // run a separate small `thread/turns/list` probe below to give
            // `reconcile_active_turn` the turn-status info it needs to
            // clear a stale local `active_turn_id`.
            // Legacy servers that do not implement `thread/turns/list`
            // still need the embedded turn list, since there is no other
            // way to learn turn status — so `exclude_turns=false` there.
            let exclude_turns = supports_pagination;
            match self
                .resume_thread_for_runtime(
                    server_id,
                    thread_id,
                    &key,
                    runtime_kind.clone(),
                    exclude_turns,
                    expected_ui_generation,
                    expected_ui_generation.map(|_| &session),
                )
                .await
            {
                Ok(applied) => {
                    if !applied {
                        return Ok(false);
                    }
                    if expected_ui_generation.is_none() {
                        self.note_thread_runtime(key.clone(), runtime_kind.clone());
                    }
                    if force_authoritative && supports_pagination {
                        if !self
                            .reconcile_active_turn_via_turn_list_probe(
                                server_id,
                                thread_id,
                                &key,
                                runtime_kind,
                                expected_ui_generation,
                                expected_ui_generation.map(|_| &session),
                            )
                            .await
                        {
                            return Ok(false);
                        }
                    }
                    return Ok(true);
                }
                Err(error) if should_try_next_runtime_after_thread_lookup_error(&error) => {
                    info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "external_resume_thread: thread lookup missed runtime {:?} server={} thread={}: {}",
                        runtime_kind, server_id, thread_id, error
                    );
                    lookup_errors.push((runtime_kind, error));
                }
                Err(error) if should_fallback_to_thread_metadata_after_resume_error(&error) => {
                    warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "external_resume_thread: resume failed, falling back to metadata-only thread/read runtime={:?} server={} thread={} error={}",
                        runtime_kind, server_id, thread_id, error
                    );
                    let applied = self
                        .read_thread_metadata_only_for_runtime(
                            server_id,
                            thread_id,
                            runtime_kind.clone(),
                            expected_ui_generation,
                            expected_ui_generation.map(|_| &session),
                        )
                        .await
                        .map_err(|fallback_error| {
                            RpcError::Deserialization(format!(
                                "{error}; metadata fallback failed: {fallback_error}"
                            ))
                        })?;
                    if !applied {
                        return Ok(false);
                    }
                    if expected_ui_generation.is_none() {
                        self.note_thread_runtime(key.clone(), runtime_kind);
                    }
                    return Ok(true);
                }
                Err(error) => return Err(RpcError::Deserialization(error)),
            }
        }

        for (runtime_kind, resume_error) in lookup_errors {
            match self
                .read_thread_metadata_only_for_runtime(
                    server_id,
                    thread_id,
                    runtime_kind.clone(),
                    expected_ui_generation,
                    expected_ui_generation.map(|_| &session),
                )
                .await
            {
                Ok(true) => {
                    if expected_ui_generation.is_none() {
                        self.note_thread_runtime(key.clone(), runtime_kind);
                    }
                    return Ok(true);
                }
                Ok(false) => return Ok(false),
                Err(fallback_error)
                    if should_try_next_runtime_after_thread_lookup_error(
                        &fallback_error.to_string(),
                    ) =>
                {
                    info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "external_resume_thread: metadata lookup missed runtime {:?} server={} thread={}: {}",
                        runtime_kind, server_id, thread_id, fallback_error
                    );
                }
                Err(fallback_error) => {
                    return Err(RpcError::Deserialization(format!(
                        "{resume_error}; metadata fallback failed: {fallback_error}"
                    )));
                }
            }
        }

        Err(RpcError::Deserialization(format!(
            "thread {thread_id} was not found in any registered runtime for server {server_id}"
        )))
    }

    async fn resume_thread_for_runtime(
        &self,
        server_id: &str,
        thread_id: &str,
        key: &ThreadKey,
        runtime_kind: AgentRuntimeKind,
        exclude_turns: bool,
        expected_ui_generation: Option<u64>,
        expected_session: Option<&Arc<ServerSession>>,
    ) -> Result<bool, String> {
        // Use thread/resume (not thread/read) so the server attaches a
        // conversation listener for this connection. Without the listener
        // the WebSocket client only receives ThreadStatusChanged — no
        // TurnStarted, ItemStarted, MessageDelta, or TurnCompleted events.
        let resume_request = upstream::ClientRequest::ThreadResume {
            request_id: upstream::RequestId::Integer(crate::next_request_id()),
            params: upstream::ThreadResumeParams {
                thread_id: thread_id.to_string(),
                developer_instructions: None,
                exclude_turns,
                ..Default::default()
            },
        };
        let response = self
            .request_typed_for_server_runtime::<upstream::ThreadResumeResponse>(
                server_id,
                runtime_kind.clone(),
                resume_request,
            )
            .await?;
        let turns = response.thread.turns.clone();
        let server_honored_exclude_turns = exclude_turns && turns.is_empty();
        // Legacy v0.124 remotes ignore `exclude_turns` and return the
        // full embedded turn history. Flip the capability flag so
        // future code paths (load_thread_turns_page) short-circuit
        // and the UI keeps relying on embedded turns.
        let snapshot = thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread,
            Some(response.model),
            response
                .reasoning_effort
                .map(Into::into)
                .map(reasoning_effort_string),
            Some(response.approval_policy.into()),
            Some(response.sandbox.into()),
        )?;
        let apply_response = |app_store: &AppStoreReducer| {
            let existing = app_store.thread_snapshot(key);
            // Diagnostic for the pagination-cursor-lost bug (task #13):
            // capture what exists while the response is committed so a newer
            // streamed event cannot slip between this read and the upsert.
            tracing::info!(
                target: "store",
                server_id,
                thread_id,
                existing_present = existing.is_some(),
                existing_items = existing.as_ref().map(|e| e.items.len()).unwrap_or(0),
                existing_older_turns_cursor = existing
                    .as_ref()
                    .and_then(|e| e.older_turns_cursor.clone())
                    .unwrap_or_default(),
                existing_initial_turns_loaded = existing
                    .as_ref()
                    .map(|e| e.initial_turns_loaded)
                    .unwrap_or(false),
                "external_resume_thread existing snapshot"
            );
            if exclude_turns && !server_honored_exclude_turns {
                app_store.set_server_supports_turn_pagination(server_id, false);
            }
            let mut snapshot = snapshot;
            snapshot.agent_runtime_kind = runtime_kind.clone();
            // Preserve existing store items when the server returned empty turns
            // (paginated path); mark initial_turns_loaded so the UI spinner knows
            // to wait for load_thread_turns_page.
            if server_honored_exclude_turns {
                if let Some(current) = existing.as_ref() {
                    snapshot.items = current.items.clone();
                    snapshot.older_turns_cursor = current.older_turns_cursor.clone();
                    snapshot.initial_turns_loaded = current.initial_turns_loaded;
                } else {
                    snapshot.initial_turns_loaded = false;
                }
            } else {
                snapshot.initial_turns_loaded = true;
                snapshot.older_turns_cursor = None;
            }
            reconcile_active_turn(existing.as_ref(), &mut snapshot, &turns);
            snapshot.is_resumed = true;
            app_store.upsert_thread_snapshot(snapshot);
            if expected_ui_generation.is_some() {
                self.thread_runtime_routes()
                    .insert(key.clone(), runtime_kind.clone());
            }
            self.mark_direct_resumed_thread(key.clone());
        };
        let applied = if let Some(expected_generation) = expected_ui_generation {
            self.apply_if_lag_refresh_current(
                server_id,
                expected_session.expect("fenced refresh carries its session"),
                expected_generation,
                apply_response,
            )
            .is_some()
        } else {
            apply_response(&self.app_store);
            true
        };
        Ok(applied)
    }

    /// On the authoritative refresh path (`force_refresh_thread_authoritative`)
    /// for paginated remotes, run a small `thread/turns/list` query that
    /// returns turn skeletons only (no item bodies). The result is fed into
    /// `reconcile_active_turn` so a locally-cached `active_turn_id` whose
    /// underlying turn has already completed server-side gets cleared, even
    /// though we asked the resume to skip the embedded turn list. Failures
    /// here are logged and ignored — the worst case is a transient stale
    /// active-turn indicator until the next streamed event arrives.
    async fn reconcile_active_turn_via_turn_list_probe(
        &self,
        server_id: &str,
        thread_id: &str,
        key: &ThreadKey,
        runtime_kind: AgentRuntimeKind,
        expected_ui_generation: Option<u64>,
        expected_session: Option<&Arc<ServerSession>>,
    ) -> bool {
        const PROBE_LIMIT: u32 = 5;
        let request = upstream::ClientRequest::ThreadTurnsList {
            request_id: upstream::RequestId::Integer(crate::next_request_id()),
            params: upstream::ThreadTurnsListParams {
                thread_id: thread_id.to_string(),
                cursor: None,
                limit: Some(PROBE_LIMIT),
                sort_direction: Some(upstream::SortDirection::Desc),
                items_view: Some(upstream::TurnItemsView::NotLoaded),
            },
        };
        let response = match self
            .request_typed_for_server_runtime::<upstream::ThreadTurnsListResponse>(
                server_id,
                runtime_kind.clone(),
                request,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                if is_method_not_found(&error) {
                    // Some non-Codex runtimes can resume a thread but do not
                    // implement the lightweight turn-list probe. Fall back to
                    // one embedded-turn resume so reconcile_active_turn can
                    // still clear a stale active turn after mobile reconnects.
                    if runtime_kind == "codex" {
                        if let Some(generation) = expected_ui_generation {
                            if self
                                .apply_if_lag_refresh_current(
                                    server_id,
                                    expected_session.expect("fenced refresh carries its session"),
                                    generation,
                                    |store| {
                                        store.set_server_supports_turn_pagination(server_id, false)
                                    },
                                )
                                .is_none()
                            {
                                return false;
                            }
                        } else {
                            self.app_store
                                .set_server_supports_turn_pagination(server_id, false);
                        }
                    }
                    if let Err(fallback_error) = self
                        .resume_thread_for_runtime(
                            server_id,
                            thread_id,
                            key,
                            runtime_kind.clone(),
                            false,
                            expected_ui_generation,
                            expected_session,
                        )
                        .await
                    {
                        warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                            "force_authoritative: embedded resume fallback failed server={} thread={} runtime={:?} error={}",
                            server_id, thread_id, runtime_kind, fallback_error
                        );
                    }
                } else {
                    warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "force_authoritative: turn-list probe failed server={} thread={} error={}",
                        server_id, thread_id, error
                    );
                }
                return expected_ui_generation.is_none_or(|generation| {
                    self.apply_if_lag_refresh_current(
                        server_id,
                        expected_session.expect("fenced refresh carries its session"),
                        generation,
                        |_| (),
                    )
                    .is_some()
                });
            }
        };
        let needs_repair_page = self.app_store.thread_snapshot(key).is_some_and(|existing| {
            let was_active = existing.active_turn_id.is_some();
            let mut target = existing.clone();
            target.active_turn_id = None;
            reconcile_active_turn(Some(&existing), &mut target, &response.data);
            was_active && target.active_turn_id.is_none()
        });
        let repair_page = if needs_repair_page {
            let request = upstream::ClientRequest::ThreadTurnsList {
                request_id: upstream::RequestId::Integer(crate::next_request_id()),
                params: upstream::ThreadTurnsListParams {
                    thread_id: thread_id.to_string(),
                    cursor: None,
                    limit: Some(PROBE_LIMIT),
                    sort_direction: Some(upstream::SortDirection::Desc),
                    items_view: None,
                },
            };
            match self
                .request_typed_for_server_runtime::<upstream::ThreadTurnsListResponse>(
                    server_id,
                    runtime_kind,
                    request,
                )
                .await
            {
                Ok(response) => Some(crate::types::AppListThreadTurnsResponse::from(response)),
                Err(error) => {
                    warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                        "force_authoritative: completed-turn repair page failed server={} thread={}: {}",
                        server_id, thread_id, error
                    );
                    None
                }
            }
        } else {
            None
        };
        let apply_response = |app_store: &AppStoreReducer| {
            let Some(existing) = app_store.thread_snapshot(key) else {
                return false;
            };
            let was_active = existing.active_turn_id.is_some();
            let mut target = existing.clone();
            // Clear the field on the target so reconcile_active_turn can decide
            // whether to restore it from `existing` based on the turn list.
            target.active_turn_id = None;
            target.info.status = ThreadSummaryStatus::Idle;
            reconcile_active_turn(Some(&existing), &mut target, &response.data);
            let active_turn_cleared = was_active && target.active_turn_id.is_none();
            if active_turn_cleared && target.info.parent_thread_id.is_some() {
                target.info.agent_status = Some("completed".to_string());
            }
            if target.active_turn_id != existing.active_turn_id
                || target.info.status != existing.info.status
            {
                app_store.upsert_thread_snapshot(target);
            }
            if active_turn_cleared
                && let Some(page) = repair_page.as_ref()
                && let Err(error) = self.apply_thread_turns_page(
                    server_id,
                    thread_id,
                    page,
                    crate::types::AppTurnsSortDirection::Descending,
                )
            {
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "force_authoritative: completed-turn repair merge failed server={} thread={}: {}",
                    server_id, thread_id, error
                );
            }
            active_turn_cleared
        };
        let Some(_) = (if let Some(expected_generation) = expected_ui_generation {
            self.apply_if_lag_refresh_current(
                server_id,
                expected_session.expect("fenced refresh carries its session"),
                expected_generation,
                apply_response,
            )
        } else {
            Some(apply_response(&self.app_store))
        }) else {
            return false;
        };
        true
    }

    /// Composite action: page a thread's older turns via `thread/turns/list`
    /// and merge them into the canonical store.
    ///
    /// - When the server is known to not support pagination
    ///   (`supports_turn_pagination == false`), refreshes an empty/unloaded
    ///   thread with an embedded-turn resume. Already-loaded threads still
    ///   short-circuit because their embedded turns are already in the store.
    /// - When the RPC comes back as JSON-RPC -32601 (method not found),
    ///   flips `supports_turn_pagination = false` on the server snapshot
    ///   and returns the same short-circuit result.
    /// - On success, invokes the `apply_thread_turns_page` reducer.
    pub async fn load_thread_turns_page(
        &self,
        server_id: &str,
        thread_id: &str,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<crate::types::AppLoadThreadTurnsOutcome, RpcError> {
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        if !self.app_store.server_supports_turn_pagination(server_id) {
            let needs_embedded_resume = self
                .app_store
                .thread_snapshot(&key)
                .is_none_or(|thread| thread.items.is_empty() && !thread.initial_turns_loaded);
            if needs_embedded_resume {
                let runtime_kind = self.runtime_for_thread(&key);
                self.resume_thread_for_runtime(
                    server_id,
                    thread_id,
                    &key,
                    runtime_kind,
                    false,
                    None,
                    None,
                )
                .await
                .map_err(RpcError::Deserialization)?;
                return Ok(crate::types::AppLoadThreadTurnsOutcome {
                    loaded: true,
                    has_more: false,
                });
            }
            return Ok(crate::types::AppLoadThreadTurnsOutcome {
                loaded: false,
                has_more: false,
            });
        }
        let params = upstream::ThreadTurnsListParams {
            thread_id: thread_id.to_string(),
            cursor,
            limit,
            sort_direction: Some(upstream::SortDirection::Desc),
            items_view: None,
        };
        let request = upstream::ClientRequest::ThreadTurnsList {
            request_id: upstream::RequestId::Integer(crate::next_request_id()),
            params,
        };
        let runtime_kind = self.runtime_for_thread(&key);
        match self
            .request_typed_for_server_runtime::<upstream::ThreadTurnsListResponse>(
                server_id,
                runtime_kind.clone(),
                request,
            )
            .await
        {
            Ok(response) => {
                let has_more = response.next_cursor.is_some();
                let page: crate::types::AppListThreadTurnsResponse = response.into();
                self.apply_thread_turns_page(
                    server_id,
                    thread_id,
                    &page,
                    crate::types::AppTurnsSortDirection::Descending,
                )
                .map_err(RpcError::Deserialization)?;
                Ok(crate::types::AppLoadThreadTurnsOutcome {
                    loaded: true,
                    has_more,
                })
            }
            Err(error) if is_method_not_found(&error) => {
                if runtime_kind == "codex".to_string() {
                    self.app_store
                        .set_server_supports_turn_pagination(server_id, false);
                }
                self.resume_thread_for_runtime(
                    server_id,
                    thread_id,
                    &key,
                    runtime_kind,
                    false,
                    None,
                    None,
                )
                .await
                .map_err(RpcError::Deserialization)?;
                Ok(crate::types::AppLoadThreadTurnsOutcome {
                    loaded: true,
                    has_more: false,
                })
            }
            Err(error) => Err(RpcError::Deserialization(error)),
        }
    }

    async fn read_thread_metadata_only_for_runtime(
        &self,
        server_id: &str,
        thread_id: &str,
        runtime_kind: AgentRuntimeKind,
        expected_ui_generation: Option<u64>,
        expected_session: Option<&Arc<ServerSession>>,
    ) -> Result<bool, RpcError> {
        let response: upstream::ThreadReadResponse = self
            .request_typed_for_server_runtime(
                server_id,
                runtime_kind.clone(),
                upstream::ClientRequest::ThreadRead {
                    request_id: upstream::RequestId::Integer(crate::next_request_id()),
                    params: upstream::ThreadReadParams {
                        thread_id: thread_id.to_string(),
                        include_turns: false,
                    },
                },
            )
            .await
            .map_err(RpcError::Deserialization)?;
        if let Some(expected_generation) = expected_ui_generation {
            let mut response = Some(response);
            self.apply_if_lag_refresh_current(
                server_id,
                expected_session.expect("fenced refresh carries its session"),
                expected_generation,
                |store| {
                    upsert_thread_snapshot_from_app_server_read_response(
                        store,
                        server_id,
                        response.take().expect("response is applied once"),
                    )?;
                    let key = ThreadKey {
                        server_id: server_id.to_string(),
                        thread_id: thread_id.to_string(),
                    };
                    self.thread_runtime_routes()
                        .insert(key.clone(), runtime_kind.clone());
                    store.set_thread_agent_runtime(&key, runtime_kind.clone());
                    Ok(())
                },
            )
            .transpose()
            .map(|applied| applied.is_some())
        } else {
            upsert_thread_snapshot_from_app_server_read_response(
                &self.app_store,
                server_id,
                response,
            )?;
            Ok(true)
        }
    }

    pub async fn thread_unsubscribe(
        &self,
        server_id: &str,
        thread_id: &str,
    ) -> Result<(), RpcError> {
        self.get_session(server_id)?;
        let _: upstream::ThreadUnsubscribeResponse = self
            .request_typed_for_server(
                server_id,
                upstream::ClientRequest::ThreadUnsubscribe {
                    request_id: upstream::RequestId::Integer(crate::next_request_id()),
                    params: upstream::ThreadUnsubscribeParams {
                        thread_id: thread_id.to_string(),
                    },
                },
            )
            .await
            .map_err(RpcError::Deserialization)?;
        self.direct_resumed_threads().remove(&ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        });
        self.app_store.mark_thread_resumed(
            &ThreadKey {
                server_id: server_id.to_string(),
                thread_id: thread_id.to_string(),
            },
            false,
        );
        Ok(())
    }

    pub async fn start_turn(
        self: &Arc<Self>,
        server_id: &str,
        params: upstream::TurnStartParams,
    ) -> Result<(), RpcError> {
        self.start_turn_with_claim(server_id, params, None).await
    }

    pub(super) async fn start_turn_with_claim(
        self: &Arc<Self>,
        server_id: &str,
        params: upstream::TurnStartParams,
        autosend_claim_id: Option<String>,
    ) -> Result<(), RpcError> {
        self.get_session(server_id)?;
        let mut params = params;
        let mut autosend_claim_id = autosend_claim_id;
        let thread_key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: params.thread_id.clone(),
        };
        let turn_start_lock = self.turn_start_lock(&thread_key);
        let mut turn_start_guard = turn_start_lock.lock_owned().await;
        self.app_store
            .dismiss_plan_implementation_prompt(&thread_key);
        let thread_snapshot = self.snapshot_thread(&thread_key).ok();
        if let Some(claim_id) = autosend_claim_id.as_deref() {
            let claim_is_current = thread_snapshot.as_ref().is_some_and(|thread| {
                thread
                    .queued_follow_up_drafts
                    .first()
                    .is_some_and(|draft| draft.preview.id == claim_id && draft.autosend_claimed)
            });
            if !claim_is_current {
                return Ok(());
            }
        } else if thread_snapshot.as_ref().is_some_and(|thread| {
            thread.active_turn_id.is_none() && !thread.queued_follow_up_drafts.is_empty()
        }) {
            if let Some(draft) =
                queued_follow_up_draft_from_inputs(&params.input, AppQueuedFollowUpKind::Message)
            {
                self.app_store
                    .enqueue_thread_follow_up_draft(&thread_key, draft);
            }
            let Some(draft) = self.app_store.try_claim_first_queued_follow_up(&thread_key) else {
                // Another autosend task already owns the retained first draft.
                return Ok(());
            };
            params = upstream::TurnStartParams {
                thread_id: thread_key.thread_id.clone(),
                input: draft.inputs,
                responsesapi_client_metadata: None,
                cwd: None,
                runtime_workspace_roots: None,
                approval_policy: None,
                approvals_reviewer: None,
                sandbox_policy: None,
                environments: None,
                permissions: None,
                model: None,
                service_tier: None,
                effort: None,
                summary: None,
                personality: None,
                output_schema: None,
                collaboration_mode: None,
            };
            autosend_claim_id = Some(draft.preview.id.clone());
        }
        if let Some(thread) = thread_snapshot.as_ref()
            && thread.collaboration_mode == AppModeKind::Plan
            && params.collaboration_mode.is_none()
        {
            params.collaboration_mode = collaboration_mode_from_thread(
                thread,
                AppModeKind::Plan,
                params.model.clone(),
                params.effort,
            );
        }
        if let Some(thread) = thread_snapshot.as_ref()
            && !self.runtime_supports_thread_permission_overrides(&thread.agent_runtime_kind)
        {
            if params.approval_policy.is_some() || params.sandbox_policy.is_some() {
                info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    server_id = %server_id,
                    thread_id = %params.thread_id,
                    runtime = %thread.agent_runtime_kind,
                    "MobileClient: dropping non-authoritative turn permission overrides"
                );
            }
            params.approval_policy = None;
            params.sandbox_policy = None;
        }
        let has_active_turn = thread_snapshot
            .as_ref()
            .is_some_and(|thread| thread.active_turn_id.is_some());
        let direct_params = params.clone();
        // Stage an optimistic local overlay so the user sees their message
        // immediately, before the server echoes it back.
        let optimistic_overlay_id = if !has_active_turn {
            self.app_store
                .stage_local_user_message_overlay(&thread_key, &params.input)
        } else {
            None
        };
        let queued_draft = has_active_turn
            .then(|| {
                queued_follow_up_draft_from_inputs(&params.input, AppQueuedFollowUpKind::Message)
            })
            .flatten();
        if let Some(draft) = queued_draft.clone() {
            self.app_store
                .enqueue_thread_follow_up_draft(&thread_key, draft.clone());
        }

        // If there's an active turn and we didn't queue a follow-up draft,
        // try turn/steer first (injects input into the running turn).
        // When a draft was queued, the user can Steer it or it will auto-send
        // when the turn finishes.  Don't also auto-steer here.
        if queued_draft.is_some() {
            return Ok(());
        }

        // If there's an active turn, try turn/steer first (injects input
        // into the running turn).  Fall back to turn/start if the turn is
        // no longer steerable or has already finished.
        if let Some(active_turn_id) = thread_snapshot
            .as_ref()
            .and_then(|t| t.active_turn_id.clone())
        {
            let steer_thread_id = params.thread_id.clone();
            let steer_input = direct_params.input.clone();
            let client = Arc::clone(self);
            let request_server_id = server_id.to_string();
            let mut steer_task = tokio::spawn(async move {
                let result = client
                    .request_typed_for_server::<upstream::TurnSteerResponse>(
                        &request_server_id,
                        upstream::ClientRequest::TurnSteer {
                            request_id: upstream::RequestId::Integer(crate::next_request_id()),
                            params: upstream::TurnSteerParams {
                                thread_id: steer_thread_id,
                                input: steer_input,
                                responsesapi_client_metadata: None,
                                expected_turn_id: active_turn_id,
                            },
                        },
                    )
                    .await;
                (result, turn_start_guard)
            });
            let steer_result = tokio::time::timeout(TURN_REQUEST_TIMEOUT, &mut steer_task).await;
            match steer_result {
                Ok(Ok((Ok(_), _guard))) => {
                    // Draft cleanup happens via TurnStarted / item upsert;
                    // don't remove here so the user sees the queued preview.
                    return Ok(());
                }
                Ok(Ok((Err(_), guard))) => {
                    // Turn not steerable or gone — fall through to turn/start.
                    turn_start_guard = guard;
                }
                Ok(Err(error)) => {
                    return Err(RpcError::Deserialization(format!(
                        "turn/steer task failed to join: {error}"
                    )));
                }
                Err(_) => {
                    // Dropping a Tokio JoinHandle detaches rather than cancels
                    // its task. The task keeps the exact waiter and per-thread
                    // lock until this ambiguous request resolves definitively.
                    return Err(RpcError::Timeout);
                }
            }
        }

        let direct_command_id = self.app_store.begin_server_mutating_command(
            server_id,
            if queued_draft.is_some() {
                ServerMutatingCommandKind::SetQueuedFollowUpsState
            } else {
                ServerMutatingCommandKind::StartTurn
            },
            &params.thread_id,
        );
        let completion_client = Arc::clone(self);
        let completion_server_id = server_id.to_string();
        let completion_thread_key = thread_key.clone();
        let completion_command_id = direct_command_id.clone();
        let completion_overlay_id = optimistic_overlay_id.clone();
        let completion_claim_id = autosend_claim_id.clone();
        let mut response_task = tokio::spawn(async move {
            let _turn_start_guard = turn_start_guard;
            let response_result = completion_client
                .request_typed_for_server::<upstream::TurnStartResponse>(
                    &completion_server_id,
                    upstream::ClientRequest::TurnStart {
                        request_id: upstream::RequestId::Integer(crate::next_request_id()),
                        params: direct_params,
                    },
                )
                .await
                .map_err(RpcError::Deserialization);
            completion_client.finish_turn_start_request(
                &completion_server_id,
                &completion_thread_key,
                &completion_command_id,
                completion_overlay_id.as_deref(),
                completion_claim_id.as_deref(),
                response_result,
            )
        });
        match tokio::time::timeout(TURN_REQUEST_TIMEOUT, &mut response_task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(RpcError::Deserialization(format!(
                "turn/start task failed to join: {error}"
            ))),
            // The detached task owns finalization and the per-thread lock, so
            // neither timeout nor caller cancellation can reopen this input.
            Err(_) => Err(RpcError::Timeout),
        }
    }

    fn finish_turn_start_request(
        &self,
        server_id: &str,
        thread_key: &ThreadKey,
        direct_command_id: &str,
        optimistic_overlay_id: Option<&str>,
        consumed_claim_id: Option<&str>,
        response_result: Result<upstream::TurnStartResponse, RpcError>,
    ) -> Result<(), RpcError> {
        let response = match response_result {
            Ok(response) => response,
            Err(error) => {
                self.app_store
                    .finish_server_mutating_command_failure(server_id, direct_command_id);
                if let Some(overlay_id) = optimistic_overlay_id {
                    self.app_store
                        .remove_local_overlay_item(thread_key, overlay_id);
                }
                if let Some(claim_id) = consumed_claim_id {
                    self.app_store
                        .release_thread_follow_up_claim(thread_key, claim_id);
                }
                return Err(error);
            }
        };
        self.app_store
            .finish_server_mutating_command_success(server_id, direct_command_id);
        self.app_store.mark_turn_started_from_response(
            thread_key,
            &response.turn.id,
            consumed_claim_id,
        );
        if let Some(overlay_id) = optimistic_overlay_id {
            self.app_store.bind_local_user_message_overlay_to_turn(
                thread_key,
                overlay_id,
                &response.turn.id,
            );
        }
        Ok(())
    }

    pub async fn steer_queued_follow_up(
        &self,
        key: &ThreadKey,
        preview_id: &str,
    ) -> Result<(), RpcError> {
        self.get_session(&key.server_id)?;
        let thread = self.snapshot_thread(key)?;
        if !thread
            .queued_follow_up_drafts
            .iter()
            .any(|draft| draft.preview.id == preview_id)
        {
            return Err(RpcError::Deserialization(format!(
                "queued follow-up not found: {preview_id}"
            )));
        }

        // Atomically flip the draft's kind to PendingSteer. If it's already
        // pending (concurrent/duplicate tap), drop this request so we don't
        // fire a second steer that would inject another copy of the user
        // message.
        let Some((draft, _next_drafts)) = self
            .app_store
            .try_begin_steer_queued_follow_up(key, preview_id)
        else {
            return Ok(());
        };

        let active_turn_id = thread.active_turn_id.ok_or_else(|| {
            RpcError::Deserialization("no active turn available to steer".to_string())
        })?;

        let direct_command_id = self.app_store.begin_server_mutating_command(
            &key.server_id,
            ServerMutatingCommandKind::SteerQueuedFollowUp,
            &key.thread_id,
        );
        if let Err(error) = self
            .request_typed_for_server::<upstream::TurnSteerResponse>(
                &key.server_id,
                upstream::ClientRequest::TurnSteer {
                    request_id: upstream::RequestId::Integer(crate::next_request_id()),
                    params: upstream::TurnSteerParams {
                        thread_id: key.thread_id.clone(),
                        input: draft.inputs,
                        responsesapi_client_metadata: None,
                        expected_turn_id: active_turn_id,
                    },
                },
            )
            .await
        {
            self.app_store
                .finish_server_mutating_command_failure(&key.server_id, &direct_command_id);
            return Err(RpcError::Deserialization(error));
        }
        self.app_store
            .finish_server_mutating_command_success(&key.server_id, &direct_command_id);
        // Keep draft visible as PendingSteer; TurnCompleted will clean it up.
        Ok(())
    }

    pub async fn delete_queued_follow_up(
        &self,
        key: &ThreadKey,
        preview_id: &str,
    ) -> Result<(), RpcError> {
        self.get_session(&key.server_id)?;
        let thread = self.snapshot_thread(key)?;
        let next_drafts = thread
            .queued_follow_up_drafts
            .into_iter()
            .filter(|draft| draft.preview.id != preview_id)
            .collect::<Vec<_>>();

        let direct_command_id = self.app_store.begin_server_mutating_command(
            &key.server_id,
            ServerMutatingCommandKind::DeleteQueuedFollowUp,
            &key.thread_id,
        );
        self.app_store.set_thread_follow_up_drafts(key, next_drafts);
        self.app_store
            .finish_server_mutating_command_success(&key.server_id, &direct_command_id);
        Ok(())
    }

    /// Roll back the current thread to a selected user turn and return the
    /// message text that should be restored into the composer for editing.
    pub async fn edit_message(
        &self,
        key: &ThreadKey,
        selected_turn_index: u32,
    ) -> Result<String, RpcError> {
        self.get_session(&key.server_id)?;
        let current = self.snapshot_thread(key)?;
        ensure_thread_is_editable(&current)?;
        let rollback_depth = rollback_depth_for_turn(&current, selected_turn_index as usize)?;
        let prefill_text = user_boundary_text_for_turn(&current, selected_turn_index as usize)?;

        if rollback_depth > 0 {
            let response = self
                .server_thread_rollback(
                    &key.server_id,
                    upstream::ThreadRollbackParams {
                        thread_id: key.thread_id.clone(),
                        num_turns: rollback_depth,
                    },
                )
                .await
                .map_err(|e| RpcError::Deserialization(e.to_string()))?;
            let turns = response.thread.turns.clone();
            let mut snapshot = thread_snapshot_from_upstream_thread_with_overrides(
                &key.server_id,
                response.thread,
                current.model.clone(),
                current.reasoning_effort.clone(),
                current.effective_approval_policy.clone(),
                current.effective_sandbox_policy.clone(),
            )
            .map_err(RpcError::Deserialization)?;
            copy_thread_runtime_fields(&current, &mut snapshot);
            reconcile_active_turn(Some(&current), &mut snapshot, &turns);
            self.app_store.upsert_thread_snapshot(snapshot);
        }

        self.set_active_thread(Some(key.clone()));
        Ok(prefill_text)
    }

    /// Fork a thread from a selected user message boundary.
    pub async fn fork_thread_from_message(
        &self,
        key: &ThreadKey,
        selected_turn_index: u32,
        cwd: Option<String>,
        model: Option<String>,
        approval_policy: Option<crate::types::AppAskForApproval>,
        sandbox: Option<crate::types::AppSandboxMode>,
        developer_instructions: Option<String>,
        persist_extended_history: bool,
    ) -> Result<ThreadKey, RpcError> {
        self.get_session(&key.server_id)?;
        let source = self.snapshot_thread(key)?;
        ensure_thread_is_editable(&source)?;
        let rollback_depth = rollback_depth_for_turn(&source, selected_turn_index as usize)?;

        let response = self
            .server_thread_fork(
                &key.server_id,
                crate::types::AppForkThreadRequest {
                    thread_id: key.thread_id.clone(),
                    model,
                    cwd,
                    approval_policy,
                    sandbox,
                    developer_instructions,
                    persist_extended_history,
                    exclude_turns: false,
                }
                .try_into()
                .map_err(|e: crate::RpcClientError| RpcError::Deserialization(e.to_string()))?,
            )
            .await
            .map_err(|e| RpcError::Deserialization(e.to_string()))?;

        let fork_model = Some(response.model);
        let fork_reasoning = response
            .reasoning_effort
            .map(|value| reasoning_effort_string(value.into()));
        let mut snapshot = thread_snapshot_from_upstream_thread_with_overrides(
            &key.server_id,
            response.thread,
            fork_model.clone(),
            fork_reasoning.clone(),
            Some(response.approval_policy.into()),
            Some(response.sandbox.into()),
        )
        .map_err(RpcError::Deserialization)?;
        let next_key = snapshot.key.clone();

        if rollback_depth > 0 {
            let rollback_response = self
                .server_thread_rollback(
                    &key.server_id,
                    upstream::ThreadRollbackParams {
                        thread_id: next_key.thread_id.clone(),
                        num_turns: rollback_depth,
                    },
                )
                .await
                .map_err(|e| RpcError::Deserialization(e.to_string()))?;
            snapshot = thread_snapshot_from_upstream_thread_with_overrides(
                &key.server_id,
                rollback_response.thread,
                fork_model,
                fork_reasoning,
                snapshot.effective_approval_policy.clone(),
                snapshot.effective_sandbox_policy.clone(),
            )
            .map_err(RpcError::Deserialization)?;
        }

        self.app_store.upsert_thread_snapshot(snapshot);
        self.set_active_thread(Some(next_key.clone()));
        Ok(next_key)
    }
}

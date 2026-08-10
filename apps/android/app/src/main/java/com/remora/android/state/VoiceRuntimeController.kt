package com.remora.android.state

import android.content.Context
import com.remora.android.util.LLog
import com.remora.android.voice.RealtimeWebRtcSession
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppAskForApproval
import uniffi.codex_mobile_client.AppDynamicToolSpec
import uniffi.codex_mobile_client.AppRealtimeStartTransport
import uniffi.codex_mobile_client.AppSandboxMode
import uniffi.codex_mobile_client.AppSandboxPolicy
import uniffi.codex_mobile_client.AppStoreSubscriptionInterface
import uniffi.codex_mobile_client.AppStoreUpdateRecord
import uniffi.codex_mobile_client.HandoffManager
import uniffi.codex_mobile_client.PinnedThreadKey
import uniffi.codex_mobile_client.ReasoningEffort
import uniffi.codex_mobile_client.ServiceTier
import uniffi.codex_mobile_client.ThreadKey
import uniffi.codex_mobile_client.AppFinalizeRealtimeHandoffRequest
import uniffi.codex_mobile_client.AppResolveRealtimeHandoffRequest
import uniffi.codex_mobile_client.AppStartRealtimeSessionRequest
import uniffi.codex_mobile_client.AppStopRealtimeSessionRequest
import java.util.UUID

/**
 * Realtime voice session controller backed by a WebRTC peer connection to the
 * OpenAI realtime edge. Microphone capture and speaker playback flow through
 * libwebrtc natively; transcripts, items, handoff, and errors continue to
 * arrive over the RPC `AppStore` update stream.
 */
class VoiceRuntimeController {

    companion object {
        val shared: VoiceRuntimeController by lazy { VoiceRuntimeController() }
        private const val TAG = "VoiceRuntime"
        private const val LOCAL_SERVER_ID = "local"
        private const val VOICE_PREFS_NAME = "remora.voice"
        private const val PERSISTED_LOCAL_VOICE_THREAD_ID_KEY = "remora.voice.local.thread_id"
    }

    // ── State ────────────────────────────────────────────────────────────────

    data class VoiceSessionState(
        val threadKey: ThreadKey,
        val inputLevel: Float = 0f,
        val outputLevel: Float = 0f,
    )

    private val _activeSession = MutableStateFlow<VoiceSessionState?>(null)
    val activeVoiceSession: StateFlow<VoiceSessionState?> = _activeSession.asStateFlow()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var sessionJob: Job? = null
    private var handoffManager: HandoffManager? = null
    private var webRtcSession: RealtimeWebRtcSession? = null
    private var stopRequestedThreadKey: ThreadKey? = null
    private var speakerEnabled = true
    private val sessionLock = Any()

    // ── Session lifecycle ────────────────────────────────────────────────────

    suspend fun preparePinnedLocalVoiceThread(
        appModel: AppModel,
        cwd: String,
        model: String? = null,
    ): ThreadKey? = ensurePinnedLocalVoiceThread(appModel, cwd = cwd, model = model)

    suspend fun startPinnedLocalVoiceCall(
        appModel: AppModel, cwd: String, model: String? = null, effort: String? = null,
    ): ThreadKey? {
        val threadKey = preparePinnedLocalVoiceThread(appModel, cwd = cwd, model = model) ?: return null
        startRealtimeSession(appModel, threadKey)
        return threadKey
    }

    suspend fun startVoiceOnThread(appModel: AppModel, key: ThreadKey) {
        startRealtimeSession(appModel, key)
    }

    suspend fun stopActiveVoiceSession(appModel: AppModel) {
        val session = _activeSession.value ?: return
        val key = session.threadKey
        synchronized(sessionLock) {
            if (stopRequestedThreadKey == key) {
                return
            }
            stopRequestedThreadKey = key
        }
        cleanup(clearStopRequest = false)
        try {
            appModel.client.stopRealtimeSession(
                key.serverId,
                AppStopRealtimeSessionRequest(threadId = key.threadId),
            )
        } catch (_: Exception) {}
        synchronized(sessionLock) {
            if (stopRequestedThreadKey == key) {
                stopRequestedThreadKey = null
            }
        }
    }

    suspend fun stopVoiceSessionIfActive(appModel: AppModel, threadKey: ThreadKey) {
        val shouldStop = synchronized(sessionLock) {
            _activeSession.value?.threadKey == threadKey || stopRequestedThreadKey == threadKey
        }
        if (shouldStop) {
            stopActiveVoiceSession(appModel)
        }
    }

    fun clearPinnedLocalVoiceThreadIfMatches(appModel: AppModel, threadKey: ThreadKey) {
        val persistedThreadId = persistedLocalVoiceThreadId(appModel) ?: return
        if (threadKey.serverId == LOCAL_SERVER_ID && threadKey.threadId == persistedThreadId) {
            setPersistedLocalVoiceThreadId(appModel, null)
        }
    }

    fun isSpeakerEnabled(): Boolean = speakerEnabled

    fun setSpeakerEnabled(enabled: Boolean) {
        speakerEnabled = enabled
    }

    // ── Event handling ───────────────────────────────────────────────────────

    suspend fun retryActiveSession(appModel: AppModel) {
        val session = _activeSession.value ?: return
        val threadKey = session.threadKey
        LLog.debug(TAG) { "Retrying active session for ${threadKey.threadId}" }
        cleanup()
        startRealtimeSession(appModel, threadKey)
    }

    private suspend fun startRealtimeSession(appModel: AppModel, threadKey: ThreadKey) {
        // Check RECORD_AUDIO permission before anything else
        val hasPermission = android.content.pm.PackageManager.PERMISSION_GRANTED ==
            appModel.appContext.checkSelfPermission(android.Manifest.permission.RECORD_AUDIO)
        if (!hasPermission) {
            LLog.e(TAG, "Microphone permission unavailable for voice session")
            return
        }

        val resolvedThreadKey = appModel.ensureThreadLoaded(threadKey) ?: threadKey
        val hasKnownThread = appModel.snapshot.value?.threads?.any { it.key == resolvedThreadKey } == true
        if (!hasKnownThread) {
            LLog.w(TAG, "Refusing to start voice session for an unavailable thread")
            LLog.debug(TAG) {
                "Unavailable voice thread: ${resolvedThreadKey.serverId}/${resolvedThreadKey.threadId}"
            }
            return
        }

        LLog.debug(TAG) {
            "Starting realtime session for ${resolvedThreadKey.serverId}/${resolvedThreadKey.threadId}"
        }
        synchronized(sessionLock) {
            val active = _activeSession.value
            if (active?.threadKey == resolvedThreadKey && sessionJob?.isActive == true) {
                LLog.debug(TAG) {
                    "Realtime session already starting/active for ${resolvedThreadKey.threadId}"
                }
                return
            }
            if (sessionJob?.isActive == true || active != null) {
                cleanup()
            }
            _activeSession.value = VoiceSessionState(threadKey = resolvedThreadKey)
        }

        try {
            cleanupKnownRealtimeVoiceSessions(appModel, keepThreadKey = resolvedThreadKey)

            // Subscribe BEFORE starting realtime — otherwise we miss the RealtimeStarted event
            LLog.debug(TAG) { "Subscribing to realtime updates first" }
            val subscription = appModel.store.subscribeUpdates()

            // Enter the event loop immediately so startup does not depend on scheduler timing.
            sessionJob = scope.launchRealtimeUpdateLoop(
                subscription = subscription,
                onStarted = {
                    LLog.debug(TAG) { "Realtime event loop started" }
                },
                onUpdate = { update ->
                    LLog.debug(TAG) { "Received realtime update: ${update::class.simpleName}" }
                    handleRealtimeUpdate(appModel, update)
                },
                onFailure = { error ->
                    LLog.e(TAG, "Realtime event loop failed")
                    LLog.debug(TAG, error) { "Realtime event loop failure details" }
                },
            )

            LLog.debug(TAG) { "Creating WebRTC peer connection and offer" }
            val session = RealtimeWebRtcSession(appModel.appContext)
            val claimed = synchronized(sessionLock) {
                if (webRtcSession != null) {
                    false
                } else {
                    webRtcSession = session
                    true
                }
            }
            if (!claimed) {
                LLog.w(TAG, "Concurrent voice session start rejected")
                session.stop()
                return
            }
            val offerSdp = session.start()

            LLog.debug(TAG) { "Starting server realtime session with WebRTC offer" }
            _activeSession.value = VoiceSessionState(threadKey = resolvedThreadKey)
            appModel.client.startRealtimeSession(
                resolvedThreadKey.serverId,
                AppStartRealtimeSessionRequest(
                    threadId = resolvedThreadKey.threadId,
                    prompt = realtimePrompt(appModel),
                    sessionId = "remora-voice-${UUID.randomUUID().toString().lowercase()}",
                    transport = AppRealtimeStartTransport.Webrtc(sdp = offerSdp),
                    clientControlledHandoff = true,
                    dynamicTools = buildDynamicToolSpecs(),
                ),
            )
            LLog.debug(TAG) { "Server realtime session started; creating handoff manager" }
            handoffManager = HandoffManager.create(resolvedThreadKey.serverId)
        } catch (error: Exception) {
            LLog.e(TAG, "Voice session startup failed")
            LLog.debug(TAG, error) { "Voice session startup failure details" }
            cleanup()
        }
    }

    private suspend fun ensurePinnedLocalVoiceThread(
        appModel: AppModel,
        cwd: String,
        model: String? = null,
    ): ThreadKey? {
        val serverId = ensureLocalServerConnected(appModel) ?: return null
        val launchConfig = appModel.launchState.launchConfig(modelOverride = model)

        persistedLocalVoiceThreadId(appModel)?.let { storedThreadId ->
            val key = ThreadKey(serverId = serverId, threadId = storedThreadId)
            val knownThread = appModel.snapshot.value?.let { snapshot ->
                snapshot.threads.any { it.key == key } || snapshot.sessionSummaries.any { it.key == key }
            } == true

            if (knownThread) {
                appModel.store.setActiveThread(key)
                return key
            }

            val loadedKey = appModel.ensureThreadLoaded(key)
            if (loadedKey != null) {
                appModel.store.setActiveThread(loadedKey)
                setPersistedLocalVoiceThreadId(appModel, loadedKey.threadId)
                appModel.refreshSnapshot()
                return loadedKey
            }

            setPersistedLocalVoiceThreadId(appModel, null)
        }

        return try {
            val key = appModel.startThread(
                serverId,
                launchConfig.toAppStartThreadRequest(
                    preferredVoiceThreadCwd(appModel, key = null, fallback = cwd),
                ),
            )
            SavedThreadsStore.add(
                appModel.appContext,
                PinnedThreadKey(serverId = key.serverId, threadId = key.threadId),
            )
            appModel.store.setActiveThread(key)
            setPersistedLocalVoiceThreadId(appModel, key.threadId)
            appModel.refreshSnapshot()
            key
        } catch (_: Exception) {
            null
        }
    }

    private suspend fun ensureLocalServerConnected(appModel: AppModel): String? {
        appModel.snapshot.value?.servers?.firstOrNull { it.isLocal && it.isConnected }?.let { server ->
            return server.serverId
        }

        val currentLocal = appModel.snapshot.value?.servers?.firstOrNull { it.isLocal }
        val serverId = currentLocal?.serverId ?: LOCAL_SERVER_ID
        val displayName = currentLocal?.displayName ?: "Local"
        return try {
            appModel.serverBridge.connectLocalServer(serverId, displayName, "127.0.0.1", 0u)
            appModel.restoreStoredLocalAuthState(serverId)
            appModel.refreshSnapshot()
            serverId
        } catch (_: Exception) {
            null
        }
    }

    private suspend fun cleanupKnownRealtimeVoiceSessions(
        appModel: AppModel,
        keepThreadKey: ThreadKey? = null,
    ) {
        val candidates = linkedSetOf<ThreadKey>()
        _activeSession.value?.threadKey
            ?.takeIf { it.threadId.isNotBlank() }
            ?.let(candidates::add)
        persistedLocalVoiceThreadId(appModel)
            ?.takeIf { it.isNotBlank() }
            ?.let { candidates.add(ThreadKey(serverId = LOCAL_SERVER_ID, threadId = it)) }

        for (candidate in candidates) {
            if (candidate == keepThreadKey) continue
            runCatching {
                appModel.client.stopRealtimeSession(
                    candidate.serverId,
                    AppStopRealtimeSessionRequest(threadId = candidate.threadId),
                )
            }
        }
    }

    private suspend fun handleRealtimeUpdate(appModel: AppModel, update: AppStoreUpdateRecord) {
        when (update) {
            is AppStoreUpdateRecord.RealtimeStarted -> {
                LLog.debug(TAG) { "Realtime session started" }
            }

            is AppStoreUpdateRecord.RealtimeSdp -> {
                val threadId = update.notification.threadId
                LLog.debug(TAG) { "Realtime SDP received for thread=$threadId" }
                val active = _activeSession.value ?: return
                if (active.threadKey.threadId != threadId || isStopRequested(active.threadKey)) return
                val session = webRtcSession ?: run {
                    LLog.w(TAG, "Realtime SDP received without a local WebRTC session")
                    return
                }
                try {
                    session.applyAnswer(update.notification.sdp)
                    LLog.debug(TAG) { "Applied WebRTC answer SDP" }
                } catch (error: Exception) {
                    LLog.e(TAG, "Failed to apply WebRTC answer")
                    LLog.debug(TAG, error) { "WebRTC answer application failure details" }
                    cleanupForThread(active.threadKey)
                }
            }

            is AppStoreUpdateRecord.FullResync -> {
                // After a lagged resync we may have missed events — refresh snapshot so UI stays consistent.
                if (_activeSession.value != null) {
                    appModel.refreshSnapshot()
                }
            }

            is AppStoreUpdateRecord.VoiceSessionChanged -> {
                val voiceSession = appModel.snapshot.value?.voiceSession
                LLog.debug(TAG) {
                    "Voice session changed: active=${voiceSession?.activeThread != null} " +
                        "phase=${voiceSession?.phase} error=${voiceSession?.lastError}"
                }
            }

            is AppStoreUpdateRecord.RealtimeHandoffRequested -> {
                processHandoffActions(appModel)
            }

            is AppStoreUpdateRecord.RealtimeOutputAudioDelta -> {
                // With WebRTC transport the server does not emit output audio over RPC;
                // audio rides the peer connection. This branch is unreachable in practice.
            }

            is AppStoreUpdateRecord.RealtimeError -> {
                if (!matchesCurrentSession(update.key)) return
                LLog.e(TAG, "Realtime voice session reported an error")
                LLog.debug(TAG) {
                    "Realtime error for thread=${update.key.threadId}: ${update.notification.message}"
                }
                if (!update.notification.message.contains("active response in progress", ignoreCase = true)) {
                    cleanupForThread(update.key)
                }
            }
            is AppStoreUpdateRecord.RealtimeClosed -> {
                if (!matchesCurrentSession(update.key)) return
                LLog.debug(TAG) {
                    "Realtime session closed for thread=${update.key.threadId} " +
                        "reason=${update.notification.reason}"
                }
                cleanupForThread(update.key)
            }
            else -> {}
        }
    }

    // ── Handoff action dispatch ──────────────────────────────────────────────

    private suspend fun processHandoffActions(appModel: AppModel) {
        val hm = handoffManager ?: return
        val actions = hm.uniffiDrainActions()
        for (action in actions) {
            dispatchHandoffAction(appModel, action)
        }
    }

    private suspend fun dispatchHandoffAction(appModel: AppModel, action: uniffi.codex_mobile_client.HandoffAction) {
        when (action) {
            is uniffi.codex_mobile_client.HandoffAction.StartThread -> {
                try {
                    val key = appModel.startThread(
                        action.targetServerId,
                        AppThreadLaunchConfig(
                            model = null,
                            approvalPolicy = AppAskForApproval.Never,
                            sandboxMode = AppSandboxMode.DANGER_FULL_ACCESS,
                            developerInstructions = null,
                            persistHistory = true,
                        ).toAppStartThreadRequest(
                            cwd = action.cwd,
                            dynamicTools = null,
                        ),
                    )
                    SavedThreadsStore.add(
                        appModel.appContext,
                        PinnedThreadKey(serverId = key.serverId, threadId = key.threadId),
                    )
                    handoffManager?.uniffiReportThreadCreated(action.handoffId, action.targetServerId, key.threadId)
                } catch (e: Exception) {
                    handoffManager?.uniffiReportThreadFailed(action.handoffId, e.message ?: "Thread creation failed")
                }
            }

            is uniffi.codex_mobile_client.HandoffAction.SendTurn -> {
                try {
                    val payload = AppComposerPayload(
                        text = action.transcript,
                        approvalPolicy = AppAskForApproval.Never,
                        sandboxPolicy = AppSandboxPolicy.DangerFullAccess,
                        model = action.config.model,
                        reasoningEffort = reasoningEffortFromWireValue(action.config.effort),
                        serviceTier = if (action.config.fastMode) ServiceTier.FAST else null,
                    )
                    appModel.startTurn(
                        ThreadKey(serverId = action.targetServerId, threadId = action.threadId),
                        payload,
                    )
                    handoffManager?.uniffiReportTurnSent(action.handoffId, 0u)
                    val handoffKey = ThreadKey(serverId = action.targetServerId, threadId = action.threadId)
                    appModel.store.setVoiceHandoffThread(key = handoffKey)
                } catch (e: Exception) {
                    handoffManager?.uniffiReportTurnFailed(action.handoffId, e.message ?: "Turn failed")
                }
            }

            is uniffi.codex_mobile_client.HandoffAction.ResolveHandoff -> {
                try {
                    appModel.client.resolveRealtimeHandoff(
                        action.voiceThreadKey.serverId,
                        AppResolveRealtimeHandoffRequest(
                            threadId = action.voiceThreadKey.threadId,
                            toolCallOutput = action.text,
                        ),
                    )
                } catch (_: Exception) {}
            }

            is uniffi.codex_mobile_client.HandoffAction.FinalizeHandoff -> {
                try {
                    appModel.client.finalizeRealtimeHandoff(
                        action.voiceThreadKey.serverId,
                        AppFinalizeRealtimeHandoffRequest(
                            threadId = action.voiceThreadKey.threadId,
                        ),
                    )
                } catch (_: Exception) {}
                handoffManager?.uniffiReportFinalized(action.handoffId)
                appModel.store.setVoiceHandoffThread(key = null)
            }

            is uniffi.codex_mobile_client.HandoffAction.Error -> {
                LLog.e(TAG, "Voice handoff failed")
                LLog.debug(TAG) { "Voice handoff failure details: ${action.message}" }
            }

            else -> {}
        }
    }

    private fun realtimePrompt(appModel: AppModel): String {
        val remoteServers = appModel.snapshot.value?.servers
            ?.filter { !it.isLocal && it.isConnected }
            ?.map { "- \"${it.displayName}\" (${it.host})" }
            ?: emptyList()
        val serverLines = buildList {
            add("- \"local\" (this device)")
            addAll(remoteServers)
        }.joinToString("\n")
        return """
            You are Codex in a live voice conversation inside Remora. Keep responses short, spoken, and conversational. Avoid markdown and code formatting unless explicitly asked.

            Available servers:
            $serverLines
            When using the codex tool, you MUST specify the "server" parameter.
            IMPORTANT: Use the local discovery tools for server and session lookup.
            The "local" server has special tools that can see sessions across ALL connected servers in one call.
            After calling list_servers or list_sessions, always give the user a short spoken summary of what you found. Do not stop after the tool result alone.
            Remote servers do NOT have these tools - never ask a remote server to list sessions.
            Use a remote server name ONLY to run coding tasks, shell commands, or file operations on that machine.
        """.trimIndent()
    }

    private fun reasoningEffortFromWireValue(value: String?): ReasoningEffort? =
        when (value?.trim()?.lowercase()) {
            "none" -> ReasoningEffort.NONE
            "minimal" -> ReasoningEffort.MINIMAL
            "low" -> ReasoningEffort.LOW
            "medium" -> ReasoningEffort.MEDIUM
            "high" -> ReasoningEffort.HIGH
            "xhigh", "x-high" -> ReasoningEffort.X_HIGH
            "max" -> ReasoningEffort.MAX
            else -> null
        }

    private fun buildDynamicToolSpecs(): List<AppDynamicToolSpec> = listOf(
        AppDynamicToolSpec(
            name = "list_servers",
            description = "List all connected servers and their status. After calling this tool, briefly tell the user what you found.",
            inputSchemaJson = """{"type":"object","properties":{}}""",
            deferLoading = false,
        ),
        AppDynamicToolSpec(
            name = "list_sessions",
            description = "List recent sessions/threads on a specific server or all connected servers. After calling this tool, briefly tell the user what you found.",
            inputSchemaJson = """{"type":"object","properties":{"server":{"type":"string","description":"Server name to query. Omit to query all connected servers."}}}""",
            deferLoading = false,
        ),
    )

    private fun persistedLocalVoiceThreadId(appModel: AppModel): String? {
        val stored = voicePrefs(appModel)
            .getString(PERSISTED_LOCAL_VOICE_THREAD_ID_KEY, null)
            ?.trim()
            .orEmpty()
        return stored.ifEmpty { null }
    }

    private fun setPersistedLocalVoiceThreadId(appModel: AppModel, threadId: String?) {
        val trimmed = threadId?.trim().orEmpty()
        val editor = voicePrefs(appModel).edit()
        if (trimmed.isEmpty()) {
            editor.remove(PERSISTED_LOCAL_VOICE_THREAD_ID_KEY)
        } else {
            editor.putString(PERSISTED_LOCAL_VOICE_THREAD_ID_KEY, trimmed)
        }
        editor.apply()
    }

    private fun voicePrefs(appModel: AppModel) =
        appModel.appContext.getSharedPreferences(VOICE_PREFS_NAME, Context.MODE_PRIVATE)

    private fun preferredVoiceThreadCwd(
        appModel: AppModel,
        key: ThreadKey?,
        fallback: String,
    ): String {
        val existingCwd = key
            ?.let { threadKey ->
                appModel.snapshot.value
                    ?.threads
                    ?.firstOrNull { it.key == threadKey }
                    ?.info
                    ?.cwd
                    ?.trim()
            }
            .orEmpty()
        if (existingCwd.isNotEmpty()) {
            return existingCwd
        }

        val trimmedFallback = fallback.trim()
        if (trimmedFallback.isNotEmpty()) {
            return trimmedFallback
        }

        return appModel.launchState.snapshot.value.currentCwd.trim().ifEmpty { "/" }
    }

    // ── Cleanup ──────────────────────────────────────────────────────────────

    private fun isStopRequested(threadKey: ThreadKey): Boolean =
        synchronized(sessionLock) { stopRequestedThreadKey == threadKey }

    private fun matchesCurrentSession(threadKey: ThreadKey): Boolean =
        synchronized(sessionLock) {
            _activeSession.value?.threadKey == threadKey || stopRequestedThreadKey == threadKey
        }

    private fun cleanupForThread(threadKey: ThreadKey, clearStopRequest: Boolean = true) {
        if (!matchesCurrentSession(threadKey)) return
        cleanup(clearStopRequest = clearStopRequest)
    }

    private fun cleanup(clearStopRequest: Boolean = true) {
        sessionJob?.cancel()
        sessionJob = null
        try {
            webRtcSession?.stop()
        } catch (error: Throwable) {
            LLog.w(TAG, "WebRTC session cleanup failed")
            LLog.debug(TAG, error) { "WebRTC session cleanup failure details" }
        }
        webRtcSession = null
        handoffManager = null
        if (clearStopRequest) {
            synchronized(sessionLock) {
                stopRequestedThreadKey = null
            }
        }
        _activeSession.value = null
    }
}

internal fun CoroutineScope.launchRealtimeUpdateLoop(
    subscription: AppStoreSubscriptionInterface,
    onStarted: () -> Unit = {},
    onUpdate: suspend (AppStoreUpdateRecord) -> Unit,
    onFailure: (Exception) -> Unit,
): Job = launch(Dispatchers.Default, start = CoroutineStart.UNDISPATCHED) {
    onStarted()
    while (true) {
        try {
            onUpdate(subscription.nextUpdate())
        } catch (error: Exception) {
            onFailure(error)
            throw error
        }
    }
}

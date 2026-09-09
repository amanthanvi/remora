package com.remora.android.state

import com.remora.android.util.LLog
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.concurrent.atomic.AtomicLong
import uniffi.codex_mobile_client.AppClient
import uniffi.codex_mobile_client.AppMinigameRequest
import uniffi.codex_mobile_client.AppMinigameResult
import com.remora.android.ui.common.AgentRuntimeKind
import uniffi.codex_mobile_client.AppSessionSummary
import uniffi.codex_mobile_client.AppSnapshotRecord
import uniffi.codex_mobile_client.AppSortDirection
import uniffi.codex_mobile_client.AppStore
import uniffi.codex_mobile_client.AppStoreSubscription
import uniffi.codex_mobile_client.AppThreadSnapshot
import uniffi.codex_mobile_client.AppThreadSortKey
import uniffi.codex_mobile_client.AppThreadSourceKind
import uniffi.codex_mobile_client.ThreadStreamingDeltaKind
import uniffi.codex_mobile_client.AppStoreUpdateRecord
import uniffi.codex_mobile_client.DiscoveryBridge
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import uniffi.codex_mobile_client.HandoffManager
import uniffi.codex_mobile_client.MessageParser
import uniffi.codex_mobile_client.ReconnectController
import uniffi.codex_mobile_client.ServerBridge
import uniffi.codex_mobile_client.SshBridge
import uniffi.codex_mobile_client.ThreadKey
import uniffi.codex_mobile_client.AppListThreadsRequest
import uniffi.codex_mobile_client.AppLoginAccountRequest
import uniffi.codex_mobile_client.AppRefreshModelsRequest
import uniffi.codex_mobile_client.AppReadThreadRequest
import uniffi.codex_mobile_client.AppStartThreadRequest
import uniffi.codex_mobile_client.registerAndroidTools
import uniffi.codex_mobile_client.threadPermissionsAreAuthoritative

class LocalAccountLoginRequiredException(val serverId: String) :
    IllegalStateException("Local account login is required.")

/**
 * Central app state singleton. Thin wrapper over Rust [AppStore] — all business
 * logic, reconciliation, and state management lives in Rust.
 *
 * Exposes a [snapshot] StateFlow that the UI observes. Updated automatically
 * via the Rust subscription stream.
 */
class AppModel private constructor(context: android.content.Context) {

    data class ComposerPrefillRequest(
        val requestId: Long,
        val threadKey: ThreadKey,
        val text: String,
    )

    /**
     * Live composer draft (typed-but-unsent text plus any pasted/picked
     * attachment) for a thread. Cached on `AppModel` so the draft survives
     * `ComposerBar` recomposition / view-tree teardown when the user
     * backgrounds the app — otherwise the local `remember { mutableStateOf }`
     * inside `ComposerBar` is dropped and the user's text disappears.
     */
    data class ComposerDraft(
        val text: String = "",
        val attachment: ComposerImageAttachment? = null,
        val fileAttachments: List<ComposerFileAttachment> = emptyList(),
    ) {
        val isEmpty: Boolean
            get() = text.isEmpty() && attachment == null && fileAttachments.isEmpty()

        companion object {
            val EMPTY = ComposerDraft()
        }
    }

    companion object {
        private var _instance: AppModel? = null

        val shared: AppModel
            get() = _instance ?: throw IllegalStateException("AppModel not initialized — call init(context) first")

        @Synchronized
        fun init(context: android.content.Context): AppModel {
            if (_instance == null) {
                val appContext = context.applicationContext
                check(CurrentSecurityCutover.apply(appContext)) {
                    "Remora 1.6 security cutover did not complete"
                }
                _instance = AppModel(appContext)
            }
            return _instance!!
        }

        /**
         * Matches the iOS page sizes. Server clamps this at 100.
         */
        const val INITIAL_TURN_PAGE_LIMIT: UInt = 5u
        const val OLDER_TURN_PAGE_LIMIT: UInt = 5u
        private const val SESSION_LIST_PAGE_LIMIT: UInt = 80u
    }

    // --- Rust bridges (singletons behind the scenes) -------------------------

    val store: AppStore
    val client: AppClient
    val discovery: DiscoveryBridge
    val serverBridge: ServerBridge
    val ssh: SshBridge
    val sshSessionStore: SshSessionStore
    val parser: MessageParser
    val reconnectController: ReconnectController
    val launchState: AppLaunchState
    /** Observes Wi-Fi ↔ cellular handoffs etc. and hints iroh. */
    val reachability: NetworkReachabilityObserver
    /** Process-lifetime Remora Link v2 callbacks retained independently of UI lifecycle. */
    val remoraLinkJournalBackend: AndroidRemoraLinkJournalBackend
    val remoraLinkTransportIdentityBackend: AndroidRemoraLinkTransportIdentityBackend
    val remoraLinkDeviceKeyBackend: AndroidRemoraLinkDeviceKeyBackend
    private val runtime = AndroidRelayRuntime.get(context)
    val remoraLinkAvailable: StateFlow<Boolean>
        get() = runtime.available
    val appContext: android.content.Context = context
    init {
        store = AppStore()
        client = runtime.client
        discovery = DiscoveryBridge()
        serverBridge = ServerBridge()
        ssh = SshBridge()
        sshSessionStore = SshSessionStore(ssh)
        parser = MessageParser()
        reconnectController = runtime.reconnectController
        launchState = AppLaunchState(context)
        reachability = NetworkReachabilityObserver(context, this)
        reachability.start()

        remoraLinkJournalBackend = runtime.journal
        remoraLinkTransportIdentityBackend = runtime.transportIdentity
        remoraLinkDeviceKeyBackend = runtime.deviceKeys
        retryRemoraLinkConfiguration()

    }

    /** Retry-safe process-lifetime setup; callers may retry after a fail-closed preflight. */
    fun retryRemoraLinkConfiguration(): Job =
        runtime.retryLinkConfiguration()

    /** All Remora Link operations must pass the configured v2 gate. */
    suspend fun <T> withRemoraLinkV2(operation: suspend (AppClient) -> T): T =
        runtime.withLink(operation)

    // --- Observable state ----------------------------------------------------

    private val projectionOwner = AppModelProjectionOwner()
    private val navigationIntent = AppModelNavigationIntent()
    private val _snapshot = MutableStateFlow<AppSnapshotRecord?>(null)
    val snapshot: StateFlow<AppSnapshotRecord?> = _snapshot.asStateFlow()

    private val _lastError = MutableStateFlow<String?>(null)
    val lastError: StateFlow<String?> = _lastError.asStateFlow()
    private val loadingModelServerIds = mutableSetOf<String>()
    private val loadingRateLimitServerIds = mutableSetOf<String>()
    private val recentConversationMetadataLoads = mutableMapOf<String, Long>()
    private val cachedThreadSnapshots = AppModelThreadSnapshotCache()
    private val removedThreadKeys = mutableSetOf<ThreadKey>()
    private val sessionListMutex = Mutex()
    private var pendingActiveThreadHydrationKey: ThreadKey? = null
    private var pendingActiveThreadHydrationJob: Job? = null

    // --- Composer prefill queue (for edit message / slash commands) -----------

    private val nextComposerPrefillRequestId = AtomicLong(0)
    private val _composerPrefillRequest = MutableStateFlow<ComposerPrefillRequest?>(null)
    val composerPrefillRequest: StateFlow<ComposerPrefillRequest?> = _composerPrefillRequest.asStateFlow()

    fun queueComposerPrefill(threadKey: ThreadKey, text: String) {
        _composerPrefillRequest.value = ComposerPrefillRequest(
            requestId = nextComposerPrefillRequestId.incrementAndGet(),
            threadKey = threadKey,
            text = text,
        )
    }

    fun clearComposerPrefill(requestId: Long) {
        if (_composerPrefillRequest.value?.requestId == requestId) {
            _composerPrefillRequest.value = null
        }
    }

    // --- Live composer drafts (per-thread typed-but-unsent text + attachment)

    private val _composerDrafts = MutableStateFlow<Map<ThreadKey, ComposerDraft>>(emptyMap())
    val composerDrafts: StateFlow<Map<ThreadKey, ComposerDraft>> = _composerDrafts.asStateFlow()
    val recoverableComposerDrafts = ComposerDraftRecoveryStore(
        persistenceFactory = { androidComposerDraftRecoveryPersistence(appContext) },
    )
    private val homeComposerDrafts = MutableStateFlow<Map<ComposerDraftDestination.Home, ComposerDraft>>(emptyMap())

    fun homeComposerDraft(destination: ComposerDraftDestination.Home): ComposerDraft =
        homeComposerDrafts.value[destination] ?: ComposerDraft.EMPTY

    fun setHomeComposerDraft(destination: ComposerDraftDestination.Home, draft: ComposerDraft) {
        homeComposerDrafts.update { current ->
            if (draft.isEmpty) current - destination else current + (destination to draft)
        }
    }

    suspend fun beginComposerSubmission(
        destination: ComposerDraftDestination,
        draft: ComposerDraft,
        payload: AppComposerPayload,
        threadStartRequest: AppStartThreadRequest? = null,
    ): Long? = try {
        recoverableComposerDrafts.begin(destination, draft, payload, threadStartRequest)
    } catch (_: ComposerDraftPersistenceException) {
        _lastError.value = COMPOSER_RECOVERY_STORAGE_ERROR
        null
    }

    suspend fun restoreComposerDraft(id: Long, current: ComposerDraft): ComposerDraft? = try {
        recoverableComposerDrafts.restore(id, current)
    } catch (_: ComposerDraftPersistenceException) {
        _lastError.value = COMPOSER_RECOVERY_STORAGE_ERROR
        null
    }

    fun discardComposerDraft(id: Long) = launchComposerRecovery {
        try {
            recoverableComposerDrafts.discard(id)
        } catch (_: ComposerDraftPersistenceException) {
            _lastError.value = COMPOSER_RECOVERY_STORAGE_ERROR
        }
    }

    /** Preparation and restore survive a composer's navigation or teardown. */
    fun launchComposerRecovery(operation: suspend () -> Unit): Job = scope.launch(Dispatchers.Main.immediate) {
        operation()
    }

    fun replaceComposerDraftIfUnchanged(
        destination: ComposerDraftDestination,
        expected: ComposerDraft,
        replacement: ComposerDraft,
    ): Boolean {
        when (destination) {
            is ComposerDraftDestination.Conversation -> {
                if (composerDraft(destination.key) != expected) return false
                setComposerDraft(destination.key, replacement)
            }
            is ComposerDraftDestination.Home -> {
                if (homeComposerDraft(destination) != expected) return false
                setHomeComposerDraft(destination, replacement)
            }
        }
        return true
    }

    /** The submission owns its attachment handles even after its composer disappears. */
    fun submitComposerDraft(id: Long, operation: suspend () -> Unit): Job = scope.launch {
        try {
            withContext(Dispatchers.IO) { recoverableComposerDrafts.submit(id, operation) }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            _lastError.value = e.message
        }
    }

    fun composerDraft(threadKey: ThreadKey): ComposerDraft =
        _composerDrafts.value[threadKey] ?: ComposerDraft.EMPTY

    fun setComposerDraft(threadKey: ThreadKey, draft: ComposerDraft) {
        _composerDrafts.update { current ->
            if (draft.isEmpty) {
                if (threadKey in current) current - threadKey else current
            } else {
                current + (threadKey to draft)
            }
        }
    }

    fun updateComposerDraft(threadKey: ThreadKey, transform: (ComposerDraft) -> ComposerDraft) {
        _composerDrafts.update { current ->
            val draft = transform(current[threadKey] ?: ComposerDraft.EMPTY)
            if (draft.isEmpty) current - threadKey else current + (threadKey to draft)
        }
    }

    fun clearComposerDraft(threadKey: ThreadKey) {
        setComposerDraft(threadKey, ComposerDraft.EMPTY)
    }

    // --- Thinking-indicator minigame -----------------------------------------

    private val _minigameOverlay = MutableStateFlow<MinigameOverlayState>(MinigameOverlayState.Idle)
    val minigameOverlay: StateFlow<MinigameOverlayState> = _minigameOverlay.asStateFlow()
    private var minigameJob: Job? = null

    fun requestMinigame(
        parentThreadId: String,
        serverId: String,
        lastUserMessage: String?,
        lastAssistantMessage: String?,
    ) {
        if (!com.remora.android.ui.ExperimentalFeatures.isEnabled(
                com.remora.android.ui.RemoraFeature.THINKING_MINIGAME
            )) return
        if (_minigameOverlay.value !is MinigameOverlayState.Idle) return
        _minigameOverlay.value = MinigameOverlayState.Loading
        minigameJob?.cancel()
        minigameJob = scope.launch {
            try {
                val result: AppMinigameResult = client.startMinigame(
                    AppMinigameRequest(
                        serverId = serverId,
                        parentThreadId = parentThreadId,
                        lastUserMessage = lastUserMessage,
                        lastAssistantMessage = lastAssistantMessage,
                    )
                )
                _minigameOverlay.value = MinigameOverlayState.Shown(
                    MinigameContent(
                        html = result.widgetHtml,
                        title = result.title,
                        width = result.width.toFloat(),
                        height = result.height.toFloat(),
                    )
                )
            } catch (t: Throwable) {
                _minigameOverlay.value = MinigameOverlayState.Failed(t.message ?: t.toString())
            }
        }
    }

    fun dismissMinigame() {
        minigameJob?.cancel()
        minigameJob = null
        _minigameOverlay.value = MinigameOverlayState.Idle
    }

    // --- Subscription lifecycle ----------------------------------------------

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    init {
        scope.launch { recoverableComposerDrafts.load() }
    }
    private val activeThreadRequests = Channel<AppModelActivationRequest>(Channel.CONFLATED).also { requests ->
        scope.launch {
            for (request in requests) {
                if (!projectionOwner.read { navigationIntent.beginDispatch(request) }) continue
                try {
                    withContext(Dispatchers.IO) { store.setActiveThread(request.key) }
                    projectionOwner.write { navigationIntent.completed(request) }
                    snapshotRefreshRequests.trySend(Unit)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    projectionOwner.read { navigationIntent.failed(request) }
                    snapshotRefreshRequests.trySend(Unit)
                    _lastError.value = e.message
                }
            }
        }
    }
    private val snapshotRefreshRequests = Channel<Unit>(Channel.CONFLATED).also { requests ->
        scope.launch {
            for (request in requests) {
                delay(100)
                refreshSnapshot()
                val activeKey = projectionOwner.read { _snapshot.value?.activeThread }
                if (activeKey != null && threadSnapshot(activeKey) == null) {
                    refreshThreadSnapshot(activeKey)
                }
                scheduleDeferredActiveThreadHydrationIfNeeded(activeKey)
            }
        }
    }
    private val subscriptionOwner = AppModelSubscriptionOwner(
        projection = projectionOwner,
        scope = scope,
        onStop = {
            pendingActiveThreadHydrationJob?.cancel()
            pendingActiveThreadHydrationJob = null
            pendingActiveThreadHydrationKey = null
        },
        collect = {
            try {
                val subscription: AppStoreSubscription = store.subscribeUpdates()
                refreshSnapshot()
                while (true) {
                    try {
                        val update: AppStoreUpdateRecord = subscription.nextUpdate()
                        currentCoroutineContext().ensureActive()
                        handleUpdate(update)
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        LLog.e("AppModel", "AppStore subscription loop failed", e)
                        throw e
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                LLog.e("AppModel", "AppModel.start() subscription failed", e)
                _lastError.value = e.message
            }
        },
    )

    fun start() {
        if (!remoraLinkAvailable.value) retryRemoraLinkConfiguration()
        subscriptionOwner.start()
    }

    fun stop() {
        subscriptionOwner.stop()
    }

    // --- Snapshot refresh -----------------------------------------------------

    suspend fun refreshSnapshot() {
        try {
            val (ticket, navigationFence) = projectionOwner.read {
                projectionOwner.readTicket() to navigationIntent.snapshotFence()
            }
            val snap = withContext(Dispatchers.IO) { store.snapshot() }
            val adjusted = applySavedServerNames(snap)
            if (!projectionOwner.commitIfCurrent(ticket) { applySnapshot(adjusted, navigationFence) }) {
                // Retry off the caller's path, with one bounded pending refresh.
                // Navigation and stream updates must not starve an awaiting UI action.
                snapshotRefreshRequests.trySend(Unit)
                return
            }
            persistWakeMacs(adjusted)
            val serverSummary = snap.servers.joinToString(separator = " | ") { server ->
                "${server.serverId}:${server.displayName}:${server.host}:${server.port}:${server.health}"
            }
            LLog.d(
                "AppModel",
                "snapshot refreshed",
                fields = mapOf("servers" to snap.servers.size, "summary" to serverSummary),
            )
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            _lastError.value = e.message
        }
    }

    private fun applySnapshot(snapshot: AppSnapshotRecord?, navigationFence: Long) {
        return projectionOwner.write {
            val liveKeys = snapshot?.let { state ->
                (state.threads.map { it.key } + state.sessionSummaries.map { it.key }).toSet()
            }.orEmpty()
            val missingKeys = cachedThreadSnapshots.prune(liveKeys)
            removedThreadKeys.addAll(missingKeys)
            missingKeys.forEach(navigationIntent::removed)
            snapshot?.threads?.forEach { removedThreadKeys.remove(it.key) }
            val merged = snapshot?.let(::mergeCachedThreadSnapshots)?.let {
                it.copy(activeThread = navigationIntent.projectSnapshot(it.activeThread, navigationFence)
                    ?.takeUnless(removedThreadKeys::contains))
            }
            _snapshot.value = merged
            if (merged != null) {
                merged.threads.forEach(::cacheThreadSnapshot)
                _lastError.value = null
            }
        }
    }

    private fun persistWakeMacs(snapshot: AppSnapshotRecord) {
        snapshot.servers.forEach { server ->
            SavedServerStore.updateWakeMac(
                context = appContext,
                serverId = server.serverId,
                host = server.host,
                wakeMac = server.wakeMac,
            )
        }
    }

    private fun loadSavedServerNames(): Map<String, String> =
        SavedServerStore.load(appContext)
            .mapNotNull { server ->
                val trimmed = server.name.trim()
                if (trimmed.isEmpty()) null else server.id to trimmed
            }
            .toMap()

    private fun applySavedServerNames(snapshot: AppSnapshotRecord): AppSnapshotRecord {
        val nameByServerId = loadSavedServerNames()
        if (nameByServerId.isEmpty()) return snapshot

        return snapshot.copy(
            servers = snapshot.servers.map { server ->
                val savedName = nameByServerId[server.serverId]
                if (savedName != null && savedName != server.displayName) {
                    server.copy(displayName = savedName)
                } else {
                    server
                }
            },
            sessionSummaries = snapshot.sessionSummaries.map { summary ->
                val savedName = nameByServerId[summary.key.serverId]
                if (savedName != null && savedName != summary.serverDisplayName) {
                    summary.copy(serverDisplayName = savedName)
                } else {
                    summary
                }
            },
        )
    }

    private fun applySavedServerName(summary: AppSessionSummary): AppSessionSummary {
        val savedName = loadSavedServerNames()[summary.key.serverId] ?: return summary
        return if (savedName != summary.serverDisplayName) {
            summary.copy(serverDisplayName = savedName)
        } else {
            summary
        }
    }

    /// Patch a single `AppSessionSummary` in the snapshot. Called whenever
    /// the reducer emits a per-item summary update on `threadItemChanged`,
    /// so home-list derived fields track streaming items without waiting
    /// for a full snapshot rebuild.
    private fun applySessionSummary(summary: AppSessionSummary) {
        val adjusted = applySavedServerName(summary)
        return projectionOwner.write {
            val current = _snapshot.value ?: return@write
            val existingIndex = current.sessionSummaries.indexOfFirst { it.key == adjusted.key }
            val updatedSummaries = current.sessionSummaries.toMutableList().apply {
                if (existingIndex >= 0) {
                    this[existingIndex] = adjusted
                } else {
                    add(adjusted)
                }
            }
            _snapshot.value = current.copy(sessionSummaries = updatedSummaries)
        }
    }

    suspend fun restartLocalServer() {
        val currentLocal = snapshot.value?.servers?.firstOrNull { it.isLocal }
        val serverId = currentLocal?.serverId ?: "local"
        val displayName = currentLocal?.displayName ?: "This Device"
        runCatching { serverBridge.disconnectServer(serverId) }
        serverBridge.connectLocalServer(
            serverId = serverId,
            displayName = displayName,
            host = "127.0.0.1",
            port = 0u,
        )
        restoreStoredLocalAuthState(serverId)
        try {
            refreshSessions(listOf(serverId))
        } catch (_: Exception) {
        }
        refreshSnapshot()
    }

    suspend fun refreshSessions(serverIds: Collection<String>? = null) {
        val targetServerIds = (serverIds?.toList() ?: snapshot.value?.servers
            ?.filter { it.isConnected }
            ?.map { it.serverId }
            .orEmpty())
            .distinct()

        if (targetServerIds.isEmpty()) {
            return
        }

        sessionListMutex.withLock {
            try {
                for (serverId in targetServerIds) {
                    client.listThreads(
                        serverId,
                        AppListThreadsRequest(
                            cursor = null,
                            limit = SESSION_LIST_PAGE_LIMIT,
                            sortKey = AppThreadSortKey.UPDATED_AT,
                            sortDirection = AppSortDirection.DESC,
                            archived = null,
                            cwd = null,
                            searchTerm = null,
                            useStateDbOnly = false,
                            runtimeKinds = null,
                        ),
                    )
                }
                _lastError.value = null
            } catch (e: Exception) {
                _lastError.value = e.message
                throw e
            }
        }
    }

    suspend fun refreshThreadSearchSessions(
        query: String,
        runtimeKind: AgentRuntimeKind?,
        forceRepair: Boolean,
    ) {
        val trimmedQuery = query.trim()
        val servers = snapshot.value?.servers
            ?.filter { it.isConnected }
            .orEmpty()
        val targetServerIds = servers
            .filter { server ->
                runtimeKind == null || server.agentRuntimes.any {
                    it.available && it.kind == runtimeKind
                }
            }
            .map { it.serverId }
            .distinct()

        if (targetServerIds.isEmpty()) {
            return
        }

        sessionListMutex.withLock {
            try {
                for (serverId in targetServerIds) {
                    client.listThreads(
                        serverId,
                        AppListThreadsRequest(
                            cursor = null,
                            limit = 80u,
                            sortKey = AppThreadSortKey.UPDATED_AT,
                            sortDirection = AppSortDirection.DESC,
                            modelProviders = null,
                            sourceKinds = listOf(
                                AppThreadSourceKind.CLI,
                                AppThreadSourceKind.VS_CODE,
                                AppThreadSourceKind.APP_SERVER,
                            ),
                            archived = false,
                            cwd = null,
                            searchTerm = trimmedQuery.ifEmpty { null },
                            useStateDbOnly = !forceRepair,
                            runtimeKinds = runtimeKind?.let { listOf(it) },
                        ),
                    )
                }
                _lastError.value = null
            } catch (e: Exception) {
                _lastError.value = e.message
                throw e
            }
        }
    }

    suspend fun loadConversationMetadataIfNeeded(serverId: String) {
        if (hasFreshConversationMetadata(serverId)) return
        loadAvailableModelsIfNeeded(serverId)
        loadRateLimitsIfNeeded(serverId)
        projectionOwner.read { recentConversationMetadataLoads[serverId] = System.currentTimeMillis() }
    }

    suspend fun loadAvailableModelsIfNeeded(serverId: String) {
        val server = snapshot.value?.servers?.firstOrNull { it.serverId == serverId } ?: return
        if (!server.isConnected) return
        if (server.availableModels != null) return
        if (!projectionOwner.read { loadingModelServerIds.add(serverId) }) return
        try {
            client.refreshModels(
                serverId,
                AppRefreshModelsRequest(cursor = null, limit = null, includeHidden = false),
            )
            refreshSnapshot()
        } catch (e: Exception) {
            _lastError.value = e.message
        } finally {
            projectionOwner.read { loadingModelServerIds.remove(serverId) }
        }
    }

    suspend fun loadRateLimitsIfNeeded(serverId: String) {
        val server = snapshot.value?.servers?.firstOrNull { it.serverId == serverId } ?: return
        if (!server.isConnected) return
        if (server.account == null) return
        if (server.rateLimits != null) return
        if (!projectionOwner.read { loadingRateLimitServerIds.add(serverId) }) return
        try {
            client.refreshRateLimits(serverId)
            refreshSnapshot()
        } catch (e: Exception) {
            _lastError.value = e.message
        } finally {
            projectionOwner.read { loadingRateLimitServerIds.remove(serverId) }
        }
    }

    suspend fun restoreStoredLocalAuthState(serverId: String) {
        val apiKeyStore = OpenAIApiKeyStore(appContext)
        val storedApiKey = apiKeyStore.load()
        if (restoreStoredLocalChatGptAuth(serverId)) {
            return
        }
        apiKeyStore.applyToEnvironment()
        if (!storedApiKey.isNullOrBlank() && loginStoredLocalApiKeyAuth(serverId, storedApiKey)) {
            return
        }
    }

    suspend fun ensureLocalAuthForThreadStart(serverId: String): Boolean {
        val server = snapshot.value?.servers?.firstOrNull { it.serverId == serverId } ?: return true
        if (!server.isLocal) return true
        if (!server.needsAccountLogin) return true

        if (restoreStoredLocalAuthIfNeeded(serverId, reason = "startThread")) {
            return true
        }

        return false
    }

    private suspend fun restoreStoredLocalAuthIfNeeded(serverId: String, reason: String): Boolean {
        val server = snapshot.value?.servers?.firstOrNull { it.serverId == serverId } ?: return false
        if (!server.isLocal || !server.needsAccountLogin) return false

        val apiKeyStore = OpenAIApiKeyStore(appContext)
        val storedApiKey = apiKeyStore.load()?.trim().orEmpty()
        val hasStoredChatGptTokens = ChatGPTOAuthTokenStore(appContext).load() != null
        if (!hasStoredChatGptTokens && storedApiKey.isBlank()) return false

        LLog.i(
            "AppModel",
            "restoring stored local auth before local session operation",
            fields = mapOf(
                "serverId" to serverId,
                "reason" to reason,
            ),
        )
        restoreStoredLocalAuthState(serverId)
        refreshSnapshot()
        return snapshot.value?.servers?.firstOrNull { it.serverId == serverId }?.needsAccountLogin == false
    }

    suspend fun restoreStoredLocalChatGptAuth(serverId: String): Boolean {
        val storedTokens = ChatGPTOAuthTokenStore(appContext).load() ?: return false
        val refreshedTokens = runCatching {
            ChatGPTOAuth.refreshStoredTokens(
                context = appContext,
                previousAccountId = null,
            )
        }.getOrNull()
        if (refreshedTokens != null &&
            loginStoredLocalChatGptAuth(serverId, refreshedTokens)
        ) {
            return true
        }
        if (loginStoredLocalChatGptAuth(serverId, storedTokens)) {
            return true
        }
        if (refreshedTokens != null) {
            return false
        }
        delay(2_000)
        return runCatching {
            ChatGPTOAuth.refreshStoredTokens(
                context = appContext,
                previousAccountId = null,
            )
        }.getOrNull()?.let { retriedRefresh ->
            loginStoredLocalChatGptAuth(serverId, retriedRefresh)
        } == true
    }

    private suspend fun loginStoredLocalChatGptAuth(
        serverId: String,
        tokens: ChatGPTOAuthTokenBundle,
    ): Boolean {
        return runCatching {
            client.loginAccount(
                serverId,
                uniffi.codex_mobile_client.AppLoginAccountRequest.ChatgptAuthTokens(
                    accessToken = tokens.accessToken,
                    chatgptAccountId = tokens.accountId,
                    chatgptPlanType = tokens.planType,
                ),
            )
            true
        }.getOrElse { error ->
            _lastError.value = error.message
            false
        }
    }

    private suspend fun loginStoredLocalApiKeyAuth(serverId: String, apiKey: String): Boolean {
        return runCatching {
            client.loginAccount(
                serverId,
                AppLoginAccountRequest.ApiKey(apiKey.trim()),
            )
            _lastError.value = null
            true
        }.getOrElse { error ->
            LLog.w(
                "AppModel",
                "restoring stored local API key auth failed",
                fields = mapOf(
                    "serverId" to serverId,
                    "error" to (error.localizedMessage ?: error.message ?: error.toString()),
                ),
            )
            false
        }
    }

    suspend fun hydrateThreadPermissions(key: ThreadKey): ThreadKey? {
        val existing = threadSnapshot(key)
        if (existing != null && hasAuthoritativePermissions(existing)) {
            launchState.syncFromThread(existing)
            return key
        }

        if (existing != null) {
            launchState.syncFromThread(existing)
            scheduleBackgroundThreadPermissionHydration(key)
            return key
        }

        if (snapshot.value?.sessionSummaries?.any { it.key == key } == true) {
            scheduleBackgroundThreadPermissionHydration(key)
            return key
        }

        return try {
            val nextKey = client.readThread(
                key.serverId,
                AppReadThreadRequest(
                    threadId = key.threadId,
                    includeTurns = false,
                ),
            )
            val threadSnapshot = readAndApplyThreadSnapshot(nextKey)
            if (threadSnapshot != null) {
                launchState.syncFromThread(threadSnapshot)
            } else {
                refreshSnapshot()
                launchState.syncFromThread(snapshot.value?.threads?.firstOrNull { it.key == nextKey })
            }
            nextKey
        } catch (e: Exception) {
            _lastError.value = e.message
            null
        }
    }

    fun activateThread(key: ThreadKey?) {
        projectionOwner.read {
            val request = navigationIntent.request(key)
            restoreCachedThreadSnapshotIfNeeded(key)
            updateActiveThread(key)
            scheduleDeferredActiveThreadHydrationIfNeeded(key)
            activeThreadRequests.trySend(request)
        }
    }

    suspend fun startThread(
        serverId: String,
        params: AppStartThreadRequest,
    ): ThreadKey {
        if (!ensureLocalAuthForThreadStart(serverId)) {
            throw LocalAccountLoginRequiredException(serverId)
        }
        return client.startThread(serverId, params)
    }

    suspend fun startTurn(
        key: ThreadKey,
        payload: AppComposerPayload,
    ) {
        restoreStoredLocalAuthIfNeeded(key.serverId, reason = "startTurn")

        try {
            store.startTurn(key, payload.toAppStartTurnRequest(key.threadId))
            _lastError.value = null
        } catch (e: Exception) {
            _lastError.value = e.message
            throw e
        }
    }

    suspend fun externalResumeThread(
        key: ThreadKey,
        hostId: String? = null,
    ) {
        restoreStoredLocalAuthIfNeeded(key.serverId, reason = "resumeThread")

        try {
            store.externalResumeThread(key, hostId)
            _lastError.value = null
        } catch (e: Exception) {
            _lastError.value = e.message
            throw e
        }
    }

    /**
     * Force a fresh `thread/resume` (with `excludeTurns = false`) so the
     * store reconciles `active_turn_id` against the server's authoritative
     * turn list. Use after a long resume / push wake — the in-flight turn
     * the local snapshot shows as running may have completed during the
     * background window with no `TurnCompleted` event delivered.
     */
    suspend fun forceRefreshThreadAuthoritative(key: ThreadKey) {
        restoreStoredLocalAuthIfNeeded(key.serverId, reason = "forceRefreshAuthoritative")
        try {
            store.forceRefreshThreadAuthoritative(key)
            _lastError.value = null
        } catch (e: Exception) {
            _lastError.value = e.message
            throw e
        }
    }

    suspend fun refreshThreadIncludingTurns(key: ThreadKey): ThreadKey {
        try {
            // 1. Refresh thread metadata only — title, status, model,
            //    active_turn_id, etc. The full historical turn list is
            //    append-only on the server, so we don't need to re-pull it
            //    here. Sending `includeTurns = true` would have the server
            //    reconstruct the entire rollout in one response, which is
            //    unbounded and OOMs the device on long threads.
            val nextKey = client.readThread(
                key.serverId,
                AppReadThreadRequest(
                    threadId = key.threadId,
                    includeTurns = false,
                ),
            )
            // 2. Reload the most-recent N turns via the paginated path. On
            //    v0.125+ remotes this hits `thread/turns/list`; on older
            //    remotes that don't implement it, the Rust client falls back
            //    to `thread/resume(excludeTurns: false)` and pulls the
            //    embedded turn list — preserving the prior reload behavior
            //    for legacy servers.
            try {
                store.loadThreadTurnsPage(nextKey, null, INITIAL_TURN_PAGE_LIMIT)
            } catch (e: Exception) {
                LLog.w(
                    "Pagination",
                    "refreshThreadIncludingTurns: initial-turn page load failed",
                    fields = mapOf(
                        "threadId" to nextKey.threadId,
                        "error" to (e.message ?: e.toString()),
                    ),
                )
            }
            val threadSnapshot = readAndApplyThreadSnapshot(nextKey)
            if (threadSnapshot == null) {
                refreshThreadSnapshot(nextKey)
            }
            _lastError.value = null
            return nextKey
        } catch (e: Exception) {
            _lastError.value = e.message
            throw e
        }
    }

    /**
     * Load the first page of turns for a thread. Intended to be called when
     * the conversation view appears for a thread whose
     * `initialTurnsLoaded == false`. Rust reconciles the page into the
     * store, and owns the fallback for servers that do not support paginated
     * turn loading.
     */
    private val initialTurnsLoadingKeys = mutableSetOf<ThreadKey>()
    private val olderTurnsLoadingKeys = mutableSetOf<ThreadKey>()

    /**
     * Launch an initial-turn load on the AppModel-owned scope so it survives
     * recomposition / LaunchedEffect key changes. The suspend body is not
     * cancelled mid-flight when the caller goes out of scope — RPC result +
     * store reconciliation always complete.
     */
    fun loadInitialTurns(key: ThreadKey, limit: UInt = INITIAL_TURN_PAGE_LIMIT) {
        if (!projectionOwner.read { initialTurnsLoadingKeys.add(key) }) return
        scope.launch {
            try {
                val outcome = store.loadThreadTurnsPage(key, null, limit)
                LLog.i(
                    "Pagination",
                    "loadInitialTurns",
                    fields = mapOf(
                        "threadId" to key.threadId,
                        "limit" to limit.toString(),
                        "loaded" to outcome.loaded.toString(),
                        "hasMore" to outcome.hasMore.toString(),
                    ),
                )
                _lastError.value = null
            } catch (e: Exception) {
                LLog.w(
                    "Pagination",
                    "loadInitialTurns failed",
                    fields = mapOf(
                        "threadId" to key.threadId,
                        "error" to (e.message ?: e.toString()),
                    ),
                )
                _lastError.value = e.message
            } finally {
                projectionOwner.read { initialTurnsLoadingKeys.remove(key) }
            }
        }
    }

    fun loadInitialTurnsIfNeeded(key: ThreadKey, limit: UInt = INITIAL_TURN_PAGE_LIMIT) {
        _snapshot.value ?: return
        if (threadSnapshot(key)?.initialTurnsLoaded == true) return
        loadInitialTurns(key, limit)
    }

    /**
     * Fetch the next older page using the thread's stored
     * `older_turns_cursor`. No-op when the cursor is null.
     *
     * Returns a [Job] so the caller can `join()` to drive UI state (e.g.
     * spinner on the "Load earlier messages" button).
     */
    fun loadOlderTurns(key: ThreadKey, limit: UInt = OLDER_TURN_PAGE_LIMIT): Job {
        val cursor = threadSnapshot(key)?.olderTurnsCursor
        if (cursor == null || !projectionOwner.read { olderTurnsLoadingKeys.add(key) }) {
            return scope.launch { /* no-op */ }
        }
        return scope.launch {
            try {
                val outcome = store.loadThreadTurnsPage(key, cursor, limit)
                LLog.i(
                    "Pagination",
                    "loadOlderTurns",
                    fields = mapOf(
                        "threadId" to key.threadId,
                        "cursor" to cursor,
                        "limit" to limit.toString(),
                        "loaded" to outcome.loaded.toString(),
                        "hasMore" to outcome.hasMore.toString(),
                    ),
                )
                _lastError.value = null
            } catch (e: Exception) {
                LLog.w(
                    "Pagination",
                    "loadOlderTurns failed",
                    fields = mapOf(
                        "threadId" to key.threadId,
                        "error" to (e.message ?: e.toString()),
                    ),
                )
                _lastError.value = e.message
            } finally {
                projectionOwner.read { olderTurnsLoadingKeys.remove(key) }
            }
        }
    }

    suspend fun ensureThreadLoaded(
        key: ThreadKey,
        maxAttempts: Int = 5,
    ): ThreadKey? {
        if (threadSnapshot(key) != null) {
            return key
        }

        var currentKey = key
        repeat(maxAttempts) { attempt ->
            var readSucceeded = false
            try {
                externalResumeThread(currentKey, null)
                projectionOwner.read {
                    if (_snapshot.value?.activeThread == currentKey) activateThread(currentKey)
                }
                readSucceeded = true
            } catch (e: Exception) {
                _lastError.value = e.message
            }

            if (readSucceeded) {
                refreshLoadedThreadSnapshot(currentKey)
                if (threadSnapshot(currentKey) != null) {
                    return currentKey
                }
            }

            if (!readSucceeded) {
                try {
                    client.listThreads(
                        currentKey.serverId,
                        AppListThreadsRequest(
                            cursor = null,
                            limit = SESSION_LIST_PAGE_LIMIT,
                            sortKey = AppThreadSortKey.UPDATED_AT,
                            sortDirection = AppSortDirection.DESC,
                            archived = null,
                            cwd = null,
                            searchTerm = null,
                            useStateDbOnly = false,
                            runtimeKinds = null,
                        ),
                    )
                } catch (e: Exception) {
                    _lastError.value = e.message
                }

                refreshLoadedThreadSnapshot(currentKey)
                if (threadSnapshot(currentKey) != null) {
                    return currentKey
                }
            }

            if (attempt + 1 < maxAttempts) {
                delay(250)
            }
        }

        return null
    }

    private suspend fun refreshLoadedThreadSnapshot(key: ThreadKey) {
        try {
            val thread = readAndApplyThreadSnapshot(key)
            if (thread == null) {
                refreshSnapshot()
            }
        } catch (e: Exception) {
            _lastError.value = e.message
            refreshSnapshot()
        }
    }

    // --- Internal event handling ----------------------------------------------

    private suspend fun handleUpdate(update: AppStoreUpdateRecord) {
        when (update) {
            is AppStoreUpdateRecord.ThreadUpserted ->
                applyThreadUpsert(update.thread, update.sessionSummary, update.agentDirectoryVersion)
            is AppStoreUpdateRecord.ThreadMetadataChanged ->
                applyThreadStateUpdated(update.state, update.sessionSummary, update.agentDirectoryVersion)
            is AppStoreUpdateRecord.ThreadItemChanged -> {
                if (!applyThreadItemChanged(update.key, update.item)) {
                    recoverThreadDeltaApplication(update.key)
                }
                // Reducer piggybacks the refreshed per-thread summary on
                // every item change; patch our local session-summary cache
                // so home-list derived fields (stats, last tool label, etc.)
                // stay in sync with streaming items without waiting for a
                // full snapshot rebuild.
                applySessionSummary(update.sessionSummary)
            }
            is AppStoreUpdateRecord.ThreadStreamingDelta -> {
                if (!applyThreadStreamingDelta(update.key, update.itemId, update.kind, update.text)) {
                    recoverThreadDeltaApplication(update.key)
                }
            }
            is AppStoreUpdateRecord.ThreadRemoved ->
                removeThreadSnapshot(update.key, update.agentDirectoryVersion)
            is AppStoreUpdateRecord.ActiveThreadChanged -> {
                // This event can coalesce multiple setters; its payload may
                // already be stale. Refresh the canonical selection instead.
                snapshotRefreshRequests.trySend(Unit)
            }
            is AppStoreUpdateRecord.PendingApprovalsChanged -> refreshSnapshot()
            is AppStoreUpdateRecord.PendingUserInputsChanged -> refreshSnapshot()
            is AppStoreUpdateRecord.ServerChanged -> refreshSnapshot()
            is AppStoreUpdateRecord.ServerRemoved -> refreshSnapshot()
            is AppStoreUpdateRecord.FullResync -> refreshSnapshot()
            is AppStoreUpdateRecord.VoiceSessionChanged -> refreshSnapshot()
            is AppStoreUpdateRecord.RealtimeTranscriptUpdated -> Unit
            is AppStoreUpdateRecord.RealtimeHandoffRequested -> Unit
            is AppStoreUpdateRecord.RealtimeSpeechStarted -> Unit
            is AppStoreUpdateRecord.RealtimeStarted -> refreshSnapshot()
            is AppStoreUpdateRecord.RealtimeSdp -> Unit
            is AppStoreUpdateRecord.RealtimeOutputAudioDelta -> Unit
            is AppStoreUpdateRecord.RealtimeError -> refreshSnapshot()
            is AppStoreUpdateRecord.RealtimeClosed -> refreshSnapshot()
            is AppStoreUpdateRecord.SavedAppsChanged -> {
                // R3: Rust broadcasts this whenever the saved-apps index/HTML/
                // state changes (show_widget finalize, update, delete). Reload
                // the Kotlin mirror so home-row takeover and Apps list can
                // react without a full snapshot churn.
                try {
                    SavedAppsStore.reload(appContext)
                } catch (_: Exception) {}
            }
            is AppStoreUpdateRecord.DynamicWidgetStreaming ->
                applyStreamingWidget(update.key, update.itemId, update.widget)
            is AppStoreUpdateRecord.TerminalSessionsChanged -> refreshSnapshot()
        }
    }

    /// Mutate an in-flight widget bubble's data so the timeline WebView
    /// picks up the growing HTML via its existing pushWidgetContent path.
    /// The reducer guarantees `isFinalized == false` on these; the
    /// finalized update arrives separately as ThreadItemChanged and must
    /// win.
    private fun applyStreamingWidget(
        key: ThreadKey,
        itemId: String,
        widget: uniffi.codex_mobile_client.HydratedWidgetData,
    ) {
        return projectionOwner.write {
            val current = _snapshot.value ?: return@write
            val threadIndex = current.threads.indexOfFirst { it.key == key }
            if (threadIndex < 0) return@write
            val thread = current.threads[threadIndex]
            val itemIndex = thread.hydratedConversationItems.indexOfFirst { it.id == itemId }
            val updatedItems = thread.hydratedConversationItems.toMutableList()
            if (itemIndex >= 0) {
                val item = updatedItems[itemIndex]
                val content = item.content
                // Before the first delta the item is a generic DynamicToolCall
                // (no args → hydration returns None → item stays as tool-call).
                // Replace its content unconditionally with the hydrated widget,
                // except when it's already a finalized widget (stale delta).
                if (content is HydratedConversationItemContent.Widget) {
                    if (content.v1.isFinalized) return@write
                    if (content.v1 == widget) return@write
                }
                updatedItems[itemIndex] = item.copy(
                    content = HydratedConversationItemContent.Widget(widget),
                )
            } else {
                // First delta raced ThreadItemStarted. Synthesize a placeholder
                // so the bubble appears now; the later ThreadItemStarted/Changed
                // will overwrite with the canonical hydrated item.
                updatedItems.add(
                    HydratedConversationItem(
                        id = itemId,
                        content = HydratedConversationItemContent.Widget(widget),
                        sourceTurnId = thread.activeTurnId,
                        sourceTurnIndex = null,
                        timestamp = null,
                        isFromUserTurnBoundary = false,
                    ),
                )
            }
            applyThreadSnapshot(thread.copy(hydratedConversationItems = updatedItems))
        }
    }

    private suspend fun recoverThreadDeltaApplication(key: ThreadKey) {
        val current = _snapshot.value
        val threadMissing = current?.threads?.any { it.key == key } != true
        val summaryMissing = current?.sessionSummaries?.any { it.key == key } != true
        if (threadMissing && summaryMissing) {
            refreshSnapshot()
        } else {
            refreshThreadSnapshot(key)
        }
    }

    suspend fun refreshThreadSnapshot(key: ThreadKey) {
        if (_snapshot.value == null) {
            refreshSnapshot()
            return
        }

        try {
            val threadSnapshot = readAndApplyThreadSnapshot(key)
            if (threadSnapshot == null) {
                projectionOwner.read {
                    if (cachedThreadSnapshots[key] == null) {
                        removeThreadSnapshot(key, clearCache = false)
                    }
                }
                return
            }
        } catch (e: Exception) {
            _lastError.value = e.message
            refreshSnapshot()
        }
    }

    private suspend fun readAndApplyThreadSnapshot(key: ThreadKey): AppThreadSnapshot? {
        val ticket = projectionOwner.readTicket()
        val thread = withContext(Dispatchers.IO) { store.threadSnapshot(key) } ?: return null
        projectionOwner.commitIfCurrent(ticket) { applyThreadSnapshot(thread) }
        return threadSnapshot(key)
    }

    private fun scheduleBackgroundThreadPermissionHydration(key: ThreadKey) {
        scope.launch {
            try {
                val nextKey = client.readThread(
                    key.serverId,
                    AppReadThreadRequest(
                        threadId = key.threadId,
                        includeTurns = false,
                    ),
                )
                val threadSnapshot = readAndApplyThreadSnapshot(nextKey)
                if (threadSnapshot != null) {
                    launchState.syncFromThread(threadSnapshot)
                } else {
                    refreshSnapshot()
                    launchState.syncFromThread(snapshot.value?.threads?.firstOrNull { it.key == nextKey })
                }
            } catch (e: Exception) {
                _lastError.value = e.message
            }
        }
    }

    private fun scheduleDeferredActiveThreadHydrationIfNeeded(key: ThreadKey?) {
        return projectionOwner.read {
            if (key == null) {
                pendingActiveThreadHydrationJob?.cancel()
                pendingActiveThreadHydrationJob = null
                pendingActiveThreadHydrationKey = null
                return@read
            }

            val thread = threadSnapshot(key)
            if (thread == null || !shouldAttemptDeferredHydration(thread)) {
                if (pendingActiveThreadHydrationKey == key) {
                    pendingActiveThreadHydrationJob?.cancel()
                    pendingActiveThreadHydrationJob = null
                    pendingActiveThreadHydrationKey = null
                }
                return@read
            }

            if (pendingActiveThreadHydrationKey == key && pendingActiveThreadHydrationJob != null) {
                return@read
            }

            pendingActiveThreadHydrationJob?.cancel()
            pendingActiveThreadHydrationKey = key
            pendingActiveThreadHydrationJob = scope.launch {
                delay(300)
                hydrateActiveThreadIfNeeded(key)
            }
        }
    }

    private suspend fun hydrateActiveThreadIfNeeded(key: ThreadKey) {
        try {
            val current = snapshot.value
            val thread = threadSnapshot(key)
            if (current?.activeThread != key || thread == null || !shouldAttemptDeferredHydration(thread)) {
                return
            }

            val nextKey = client.readThread(
                key.serverId,
                AppReadThreadRequest(
                    threadId = key.threadId,
                    includeTurns = false,
                ),
            )
            val threadSnapshot = readAndApplyThreadSnapshot(nextKey)
            if (threadSnapshot == null) {
                refreshThreadSnapshot(nextKey)
            }
        } catch (e: Exception) {
            _lastError.value = e.message
        } finally {
            val hydrationJob = currentCoroutineContext()[Job]
            projectionOwner.read {
                if (pendingActiveThreadHydrationJob === hydrationJob) {
                    pendingActiveThreadHydrationJob = null
                    pendingActiveThreadHydrationKey = null
                }
            }
        }
    }

    private fun shouldAttemptDeferredHydration(thread: AppThreadSnapshot): Boolean {
        if (thread.hydratedConversationItems.isNotEmpty()) return false
        val preview = thread.info.preview?.trim().orEmpty()
        val title = thread.info.title?.trim().orEmpty()
        return preview.isNotEmpty() || title.isNotEmpty() || thread.hasActiveTurn
    }

    private fun applyThreadSnapshot(thread: AppThreadSnapshot) {
        return projectionOwner.write {
            if (thread.key in removedThreadKeys) return@write
            val mergedThread = mergedThreadSnapshotPreservingHydratedItems(thread)
            val current = _snapshot.value
            if (current == null) {
                cacheThreadSnapshot(mergedThread)
                return@write
            }
            val existingIndex = current.threads.indexOfFirst { it.key == thread.key }
            val updatedThreads = current.threads.toMutableList().apply {
                if (existingIndex >= 0) {
                    this[existingIndex] = mergedThread
                } else {
                    add(mergedThread)
                }
            }
            _snapshot.value = current.copy(threads = updatedThreads)
            cacheThreadSnapshot(mergedThread)
            _lastError.value = null
        }
    }

    private fun applyThreadUpsert(
        thread: AppThreadSnapshot,
        sessionSummary: AppSessionSummary,
        agentDirectoryVersion: ULong,
    ) {
        val adjustedSummary = applySavedServerName(sessionSummary)
        return projectionOwner.write {
            removedThreadKeys.remove(thread.key)
            val mergedThread = mergedThreadSnapshotPreservingHydratedItems(thread)
            val current = _snapshot.value ?: return@write
            val existingThreadIndex = current.threads.indexOfFirst { it.key == thread.key }

            // Race condition guard: during active streaming, if the old thread has
            // longer assistant text that starts with the new text, preserve the old
            // (more complete) text to avoid flickering backwards.
            val finalThread = if (existingThreadIndex >= 0) {
                val oldThread = current.threads[existingThreadIndex]
                if (oldThread.hasActiveTurn) {
                    preserveStreamingText(oldThread, mergedThread)
                } else {
                    mergedThread
                }
            } else {
                mergedThread
            }

            val updatedThreads = current.threads.toMutableList().apply {
                if (existingThreadIndex >= 0) {
                    this[existingThreadIndex] = finalThread
                } else {
                    add(finalThread)
                }
            }

            val existingSummaryIndex = current.sessionSummaries.indexOfFirst { it.key == adjustedSummary.key }
            val updatedSummaries = current.sessionSummaries.toMutableList().apply {
                if (existingSummaryIndex >= 0) {
                    this[existingSummaryIndex] = adjustedSummary
                } else {
                    add(adjustedSummary)
                }
                sortWith(compareByDescending<AppSessionSummary> { it.updatedAt ?: Long.MIN_VALUE }
                    .thenBy { it.key.serverId }
                    .thenBy { it.key.threadId })
            }

            _snapshot.value = current.copy(
                threads = updatedThreads,
                sessionSummaries = updatedSummaries,
                agentDirectoryVersion = agentDirectoryVersion,
            )
            cacheThreadSnapshot(finalThread)
            _lastError.value = null
        }
    }

    private fun preserveStreamingText(
        oldThread: AppThreadSnapshot,
        newThread: AppThreadSnapshot,
    ): AppThreadSnapshot {
        if (newThread.hydratedConversationItems.isEmpty()) return newThread
        val oldItemsById = oldThread.hydratedConversationItems.associateBy { it.id }
        var changed = false
        val mergedItems = newThread.hydratedConversationItems.map { newItem ->
            val oldItem = oldItemsById[newItem.id]
            if (oldItem != null) {
                val oldText = assistantText(oldItem.content)
                val newText = assistantText(newItem.content)
                if (oldText != null && newText != null &&
                    oldText.length > newText.length &&
                    oldText.startsWith(newText)
                ) {
                    changed = true
                    oldItem
                } else {
                    newItem
                }
            } else {
                newItem
            }
        }
        return if (changed) newThread.copy(hydratedConversationItems = mergedItems) else newThread
    }

    private fun assistantText(content: HydratedConversationItemContent): String? =
        when (content) {
            is HydratedConversationItemContent.Assistant -> content.v1.text
            else -> null
        }

    private fun applyThreadStateUpdated(
        state: uniffi.codex_mobile_client.AppThreadStateRecord,
        sessionSummary: AppSessionSummary,
        agentDirectoryVersion: ULong,
    ) {
        val adjustedSummary = applySavedServerName(sessionSummary)
        return projectionOwner.write {
            val current = _snapshot.value ?: return@write
            val existingThreadIndex = current.threads.indexOfFirst { it.key == state.key }
            if (existingThreadIndex < 0) return@write

            val existingThread = current.threads[existingThreadIndex]
            val updatedThread = existingThread.copy(
                info = state.info,
                collaborationMode = state.collaborationMode,
                model = state.model,
                reasoningEffort = state.reasoningEffort,
                effectiveApprovalPolicy = state.effectiveApprovalPolicy,
                effectiveSandboxPolicy = state.effectiveSandboxPolicy,
                queuedFollowUps = state.queuedFollowUps,
                activeTurnId = state.activeTurnId,
                activePlanProgress = state.activePlanProgress,
                pendingPlanImplementationPrompt = state.pendingPlanImplementationPrompt,
                contextTokensUsed = state.contextTokensUsed,
                modelContextWindow = state.modelContextWindow,
                rateLimits = state.rateLimits,
                realtimeSessionId = state.realtimeSessionId,
                goal = state.goal,
                olderTurnsCursor = state.olderTurnsCursor,
                initialTurnsLoaded = state.initialTurnsLoaded,
            )
            val updatedThreads = current.threads.toMutableList().apply {
                this[existingThreadIndex] = updatedThread
            }

            val existingSummaryIndex = current.sessionSummaries.indexOfFirst { it.key == adjustedSummary.key }
            val updatedSummaries = current.sessionSummaries.toMutableList().apply {
                if (existingSummaryIndex >= 0) {
                    this[existingSummaryIndex] = adjustedSummary
                } else {
                    add(adjustedSummary)
                }
                sortWith(compareByDescending<AppSessionSummary> { it.updatedAt ?: Long.MIN_VALUE }
                    .thenBy { it.key.serverId }
                    .thenBy { it.key.threadId })
            }

            _snapshot.value = current.copy(
                threads = updatedThreads,
                sessionSummaries = updatedSummaries,
                agentDirectoryVersion = agentDirectoryVersion,
            )
            cacheThreadSnapshot(updatedThread)
            _lastError.value = null
        }
    }

    private fun applyThreadItemChanged(
        key: ThreadKey,
        item: HydratedConversationItem,
    ): Boolean {
        return projectionOwner.write {
            val current = _snapshot.value ?: return@write false
            val threadIndex = current.threads.indexOfFirst { it.key == key }
            if (threadIndex < 0) return@write false

            val thread = current.threads[threadIndex]
            val updatedItems = thread.hydratedConversationItems.toMutableList()
            val existingItemIndex = updatedItems.indexOfFirst { it.id == item.id }
            if (existingItemIndex >= 0) {
                updatedItems[existingItemIndex] = item
            } else {
                val insertionIndex = insertionIndexForItem(updatedItems, item)
                updatedItems.add(insertionIndex, item)
            }
            applyThreadSnapshot(thread.copy(hydratedConversationItems = updatedItems))
            return@write true
        }
    }

    private fun applyThreadStreamingDelta(
        key: ThreadKey,
        itemId: String,
        kind: ThreadStreamingDeltaKind,
        text: String,
    ): Boolean {
        return projectionOwner.write {
            val current = _snapshot.value ?: return@write false
            val threadIndex = current.threads.indexOfFirst { it.key == key }
            if (threadIndex < 0) return@write false

            val thread = current.threads[threadIndex]
            val itemIndex = thread.hydratedConversationItems.indexOfFirst { it.id == itemId }
            if (itemIndex < 0) return@write false

            val updatedContent = applyStreamingDelta(kind, text, thread.hydratedConversationItems[itemIndex].content)
                ?: return@write false
            val updatedItems = thread.hydratedConversationItems.toMutableList().apply {
                this[itemIndex] = this[itemIndex].copy(content = updatedContent)
            }
            applyThreadSnapshot(thread.copy(hydratedConversationItems = updatedItems))
            return@write true
        }
    }

    private fun applyStreamingDelta(
        kind: ThreadStreamingDeltaKind,
        text: String,
        content: HydratedConversationItemContent,
    ): HydratedConversationItemContent? = when (kind) {
        ThreadStreamingDeltaKind.ASSISTANT_TEXT -> when (content) {
            is HydratedConversationItemContent.Assistant ->
                HydratedConversationItemContent.Assistant(content.v1.copy(text = content.v1.text + text))
            else -> null
        }
        ThreadStreamingDeltaKind.REASONING_TEXT -> when (content) {
            is HydratedConversationItemContent.Reasoning -> {
                val updatedContent = content.v1.content.toMutableList().apply {
                    if (isEmpty()) {
                        add(text)
                    } else {
                        this[lastIndex] = this[lastIndex] + text
                    }
                }
                HydratedConversationItemContent.Reasoning(content.v1.copy(content = updatedContent))
            }
            else -> null
        }
        ThreadStreamingDeltaKind.PLAN_TEXT -> when (content) {
            is HydratedConversationItemContent.ProposedPlan ->
                HydratedConversationItemContent.ProposedPlan(content.v1.copy(content = content.v1.content + text))
            else -> null
        }
        ThreadStreamingDeltaKind.COMMAND_OUTPUT -> when (content) {
            is HydratedConversationItemContent.CommandExecution ->
                HydratedConversationItemContent.CommandExecution(
                    content.v1.copy(output = (content.v1.output ?: "") + text)
                )
            else -> null
        }
        ThreadStreamingDeltaKind.MCP_PROGRESS -> when (content) {
            is HydratedConversationItemContent.McpToolCall -> {
                val updatedProgress = content.v1.progressMessages.toMutableList().apply {
                    if (text.isNotBlank()) {
                        add(text)
                    }
                }
                HydratedConversationItemContent.McpToolCall(
                    content.v1.copy(progressMessages = updatedProgress)
                )
            }
            else -> null
        }
    }

    private fun insertionIndexForItem(
        items: List<HydratedConversationItem>,
        item: HydratedConversationItem,
    ): Int {
        val targetTurn = item.sourceTurnIndex?.toInt() ?: return items.size
        val lastSameTurn = items.indexOfLast { it.sourceTurnIndex?.toInt() == targetTurn }
        if (lastSameTurn >= 0) return lastSameTurn + 1

        val nextTurn = items.indexOfFirst {
            val sourceTurn = it.sourceTurnIndex?.toInt()
            sourceTurn != null && sourceTurn > targetTurn
        }
        return if (nextTurn >= 0) nextTurn else items.size
    }

    private fun hasAuthoritativePermissions(thread: AppThreadSnapshot): Boolean =
        threadPermissionsAreAuthoritative(
            approvalPolicy = thread.effectiveApprovalPolicy,
            sandboxPolicy = thread.effectiveSandboxPolicy,
        )

    private fun removeThreadSnapshot(
        key: ThreadKey,
        agentDirectoryVersion: ULong? = null,
        clearCache: Boolean = true,
    ) {
        return projectionOwner.write {
            if (clearCache) {
                cachedThreadSnapshots.remove(key)
                removedThreadKeys.add(key)
                navigationIntent.removed(key)
                if (pendingActiveThreadHydrationKey == key) {
                    pendingActiveThreadHydrationJob?.cancel()
                    pendingActiveThreadHydrationJob = null
                    pendingActiveThreadHydrationKey = null
                }
            }
            val current = _snapshot.value ?: return@write
            _snapshot.value = current.copy(
                threads = current.threads.filterNot { it.key == key },
                sessionSummaries = current.sessionSummaries.filterNot { it.key == key },
                agentDirectoryVersion = agentDirectoryVersion ?: current.agentDirectoryVersion,
                activeThread = if (current.activeThread == key) null else current.activeThread,
            )
        }
    }

    private fun updateActiveThread(key: ThreadKey?): ThreadKey? {
        return projectionOwner.write {
            val activeKey = navigationIntent.project(key)?.takeUnless(removedThreadKeys::contains)
            _snapshot.value = _snapshot.value?.copy(activeThread = activeKey)
            activeKey
        }
    }

    fun threadSnapshot(key: ThreadKey): AppThreadSnapshot? =
        projectionOwner.read {
            _snapshot.value?.threads?.firstOrNull { it.key == key } ?: cachedThreadSnapshots[key]
        }

    private fun hasFreshConversationMetadata(serverId: String): Boolean {
        val server = snapshot.value?.servers?.firstOrNull { it.serverId == serverId } ?: return false
        val hasModels = server.availableModels != null
        val hasRateLimits = server.account == null || server.rateLimits != null
        if (hasModels && hasRateLimits) return true

        val lastLoad = projectionOwner.read { recentConversationMetadataLoads[serverId] } ?: return false
        return System.currentTimeMillis() - lastLoad < 10_000L
    }

    private fun restoreCachedThreadSnapshotIfNeeded(key: ThreadKey?) {
        return projectionOwner.write {
            if (key == null) return@write
            if (_snapshot.value?.threads?.any { it.key == key } == true) return@write
            val cached = cachedThreadSnapshots[key] ?: return@write
            applyThreadSnapshot(cached)
        }
    }

    private fun cacheThreadSnapshot(thread: AppThreadSnapshot) {
        cachedThreadSnapshots[thread.key] = thread
    }

    private fun mergedThreadSnapshotPreservingHydratedItems(thread: AppThreadSnapshot): AppThreadSnapshot {
        return cachedThreadSnapshots.preserveHydration(thread)
    }

    private fun mergeCachedThreadSnapshots(snapshot: AppSnapshotRecord): AppSnapshotRecord {
        val mergedThreads = snapshot.threads
            .map(::mergedThreadSnapshotPreservingHydratedItems)
            .toMutableList()

        cachedThreadSnapshots.values().forEach { cached ->
            val key = cached.key
            val alreadyPresent = mergedThreads.any { it.key == key }
            val shouldInclude = snapshot.activeThread == key || snapshot.sessionSummaries.any { it.key == key }
            if (!alreadyPresent && shouldInclude) {
                mergedThreads += cached
            }
        }

        return snapshot.copy(threads = mergedThreads)
    }
}

internal fun registerBundledCliTools() {
    val tools = emptyMap<String, String>()
    try {
        registerAndroidTools(tools)
        LLog.i(
            "AppModel",
            "Registered bundled CLI tools",
            fields = mapOf("count" to tools.size),
        )
        LLog.d(
            "AppModel",
            "Registered bundled CLI tool details",
            fields = mapOf("tools" to tools.keys.joinToString(",")),
        )
    } catch (e: Throwable) {
        LLog.w(
            "AppModel",
            "registerAndroidTools failed",
            fields = mapOf(
                "errorType" to e.javaClass.simpleName,
                "error" to e.message,
            ),
        )
    }
}

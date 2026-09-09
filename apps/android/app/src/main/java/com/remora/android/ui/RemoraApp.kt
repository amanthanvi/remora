package com.remora.android.ui

import android.provider.Settings
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.onKeyEvent
import com.remora.android.state.AppModel
import com.remora.android.state.LocalAccountLoginRequiredException
import com.remora.android.state.NetworkDiscovery
import com.remora.android.state.SavedThreadsStore
import com.remora.android.state.VoiceRuntimeController
import com.remora.android.state.connectionModeLabel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import com.remora.android.ui.conversation.ApprovalOverlay
import com.remora.android.ui.conversation.ConversationInfoScreen
import com.remora.android.ui.conversation.ConversationScreen
import com.remora.android.ui.discovery.DiscoveryScreen
import com.remora.android.ui.home.HomeDashboardScreen
import com.remora.android.ui.home.HomeDashboardSupport
import com.remora.android.ui.home.ProjectPickerSheet
import com.remora.android.state.SavedProjectStore
import com.remora.android.ui.settings.AccountSheet
import com.remora.android.ui.settings.SettingsSheet
import com.remora.android.ui.sessions.DirectoryPickerServerOption
import com.remora.android.ui.sessions.DirectoryPickerSheet
import com.remora.android.ui.sessions.SessionLaunchSupport
import com.remora.android.ui.sessions.SessionsUiState
import com.remora.android.ui.terminal.TerminalScreen
import com.remora.android.ui.terminal.RemoraLinkTerminalHost
import com.remora.android.ui.terminal.remoraLinkTerminalHost
import com.remora.android.ui.workflow.AdaptiveNavigationMode
import com.remora.android.ui.workflow.AdaptiveWorkflowScaffold
import com.remora.android.ui.workflow.CommandPaletteSheet
import com.remora.android.ui.workflow.WorkflowActionCatalog
import com.remora.android.ui.workflow.WorkflowActionContext
import com.remora.android.ui.workflow.WorkflowActionId
import com.remora.android.ui.workflow.actionForKeyEvent
import com.remora.android.ui.workflow.neighboringThread
import uniffi.codex_mobile_client.AppProject
import uniffi.codex_mobile_client.ApprovalKind
import uniffi.codex_mobile_client.PendingUserInputRequest
import uniffi.codex_mobile_client.PinnedThreadKey
import uniffi.codex_mobile_client.ThreadKey
import uniffi.codex_mobile_client.deriveProjects
import uniffi.codex_mobile_client.projectIdFor

/**
 * CompositionLocal for accessing [AppModel] from any composable.
 */
val LocalAppModel = staticCompositionLocalOf<AppModel> {
    error("AppModel not provided")
}

/**
 * UI-only ledger of pending-user-input request IDs the user has manually dismissed.
 * Lives at the app shell so the inline composer prompt and the global approval
 * overlay agree on what's been hidden.
 */
class DismissedUserInputState {
    var ids by mutableStateOf(setOf<Triple<String, String, String>>())
        private set

    fun dismiss(request: uniffi.codex_mobile_client.PendingUserInputRequest) {
        ids = ids + Triple(request.serverId, request.runtimeKind, request.id)
    }

    fun isDismissed(request: uniffi.codex_mobile_client.PendingUserInputRequest): Boolean =
        ids.contains(Triple(request.serverId, request.runtimeKind, request.id))
}

val LocalDismissedUserInputs = staticCompositionLocalOf<DismissedUserInputState> {
    error("DismissedUserInputState not provided")
}

/**
 * Root composable for the app. Manages navigation stack and global overlays.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RemoraApp(
    appModel: AppModel,
) {
    val context = LocalContext.current

    // Initialize text size preference
    LaunchedEffect(Unit) {
        TextSizePrefs.initialize(context)
        ConversationPrefs.initialize(context)
        com.remora.android.ui.home.DashboardZoomPrefs.initialize(context)
        ExperimentalFeatures.initialize(context)
        com.remora.android.state.DebugSettings.initialize(context)
    }

    // Read currentStep so Compose tracks it as a dependency and recomposes on change.
    val textScale = ConversationTextSize.fromStep(TextSizePrefs.currentStep).scale
    val dismissedUserInputs = remember { DismissedUserInputState() }
    CompositionLocalProvider(
        LocalAppModel provides appModel,
        LocalTextScale provides textScale,
        LocalDismissedUserInputs provides dismissedUserInputs,
    ) {
        val snapshot by appModel.snapshot.collectAsState()
        val scope = androidx.compose.runtime.rememberCoroutineScope()

        // Navigation state
        var navStack by rememberSaveable(stateSaver = RouteStackSaver) {
            mutableStateOf<List<Route>>(listOf(Route.Home))
        }
        val navigationWasRestored = rememberSaveable(saver = NavigationRestorationMarkerSaver) {
            false
        }
        val currentRoute = navStack.lastOrNull() ?: Route.Home
        val sessionsUiState = remember { SessionsUiState() }
        val rootFocusRequester = remember { FocusRequester() }
        var homeWorkflowBlocked by remember { mutableStateOf(false) }
        var conversationWorkflowBlocked by remember { mutableStateOf(false) }
        var navigationPaneInputFocused by remember { mutableStateOf(false) }
        var navigationMode by remember { mutableStateOf(AdaptiveNavigationMode.COMPACT) }
        var focusSearchRequest by remember { mutableIntStateOf(0) }
        var hasObservedInitialActiveThread by remember { mutableStateOf(false) }

        // Global sheet state
        var showDiscovery by remember { mutableStateOf(false) }
        var showSettings by remember { mutableStateOf(false) }
        var showAccountForServer by remember { mutableStateOf<String?>(null) }
        var directoryPickerServerId by remember { mutableStateOf<String?>(null) }
        var directoryPickerForProject by remember { mutableStateOf(false) }
        var showProjectPicker by remember { mutableStateOf(false) }
        var showCommandPalette by remember { mutableStateOf(false) }

        // Home selection state
        var selectedServerId by remember {
            mutableStateOf(SavedProjectStore.selectedServerId(context))
        }
        var selectedProject by remember { mutableStateOf<AppProject?>(null) }

        // Persist selections
        LaunchedEffect(selectedServerId) {
            SavedProjectStore.setSelectedServerId(context, selectedServerId)
        }
        LaunchedEffect(selectedProject?.id) {
            SavedProjectStore.setSelectedProjectId(context, selectedProject?.id)
        }

        // Derive projects from current sessions
        val projects = remember(snapshot) {
            snapshot?.let { deriveProjects(it.sessionSummaries) } ?: emptyList()
        }

        // Keep selectedServerId valid against connected servers. Default is
        // no filter — if the persisted/pinned server isn't connected, clear.
        LaunchedEffect(snapshot) {
            val connected = snapshot?.let { snap ->
                HomeDashboardSupport.sortedConnectedServers(snap).map { it.serverId }
            } ?: emptyList()
            if (selectedServerId != null && selectedServerId !in connected) {
                selectedServerId = null
            }
        }

        // Reconcile selectedProject against selectedServerId + projects
        LaunchedEffect(selectedServerId, projects) {
            val currentServerId = selectedServerId ?: run {
                selectedProject = null
                return@LaunchedEffect
            }
            val serverProjects = projects.filter { it.serverId == currentServerId }
            val current = selectedProject
            if (current != null && current.serverId == currentServerId) {
                val refreshed = serverProjects.firstOrNull { it.id == current.id }
                if (refreshed != null) {
                    selectedProject = refreshed
                }
                return@LaunchedEffect
            }
            val persistedId = SavedProjectStore.selectedProjectId(context)
            val match = serverProjects.firstOrNull { it.id == persistedId }
                ?: serverProjects.firstOrNull()
            selectedProject = match
        }

        // Network discovery
        val networkDiscovery = remember { NetworkDiscovery(appModel.discovery) }
        val voiceController = remember { VoiceRuntimeController.shared }

        // Navigate helpers
        val navigate = remember {
            { route: Route -> navStack = navStack + route }
        }
        val navigateBack = remember {
            { if (navStack.size > 1) navStack = navStack.dropLast(1) }
        }
        val navigateToConversation = remember {
            { key: ThreadKey -> navStack = conversationStack(key) }
        }
        val connectedServerOptions = remember(snapshot) {
            snapshot?.let { snap ->
                HomeDashboardSupport.sortedConnectedServers(snap).map { server ->
                    DirectoryPickerServerOption(
                        id = server.serverId,
                        name = server.displayName,
                        sourceLabel = server.connectionModeLabel,
                    )
                }
            } ?: emptyList()
        }

        suspend fun startNewSession(serverId: String, cwd: String) {
            val serverIsLocal = appModel.snapshot.value
                ?.servers
                ?.firstOrNull { it.serverId == serverId }
                ?.isLocal == true
            val startedKey = appModel.startThread(
                serverId,
                appModel.launchState.threadStartRequest(cwd, serverIsLocal = serverIsLocal),
            )
            RecentDirectoryStore(context).record(serverId, cwd)
            SavedThreadsStore.add(
                context,
                PinnedThreadKey(serverId = startedKey.serverId, threadId = startedKey.threadId),
            )
            appModel.store.setActiveThread(startedKey)
            appModel.refreshThreadSnapshot(startedKey)
            val resolvedKey = appModel.ensureThreadLoaded(startedKey)
                ?: appModel.snapshot.value?.threads?.firstOrNull { it.key == startedKey }?.key
                ?: startedKey
            navigateToConversation(resolvedKey)
        }

        fun openDirectoryPicker(preferredServerId: String? = null) {
            val targetServerId = SessionLaunchSupport.defaultConnectedServerId(
                connectedServerIds = connectedServerOptions.map { it.id },
                activeThreadKey = snapshot?.activeThread,
                preferredServerId = preferredServerId,
            )
            if (targetServerId == null) {
                showDiscovery = true
            } else {
                directoryPickerServerId = targetServerId
            }
        }

        val terminalEnabled = ExperimentalFeatures.isEnabled(RemoraFeature.TERMINAL)
        val workflowSessions = remember(snapshot) {
            snapshot?.sessionSummaries
                ?.distinctBy { it.key.serverId to it.key.threadId }
                ?.sortedByDescending { it.updatedAt ?: 0L }
                ?: emptyList()
        }
        val orderedThreadKeys = remember(workflowSessions) { workflowSessions.map { it.key } }

        fun currentWorkflowActionContext(): WorkflowActionContext = WorkflowActionContext(
            route = navStack.lastOrNull() ?: Route.Home,
            canNavigateBack = navStack.size > 1,
            connectedServerCount = connectedServerOptions.size,
            terminalEnabled = terminalEnabled,
            orderedThreadKeys = orderedThreadKeys,
        )

        fun executeWorkflowAction(id: WorkflowActionId): String? {
            val contextNow = currentWorkflowActionContext()
            val action = WorkflowActionCatalog.action(id, contextNow)
            if (!action.enabled) {
                return action.disabledReason ?: "That action is not available right now"
            }
            when (id) {
                WorkflowActionId.HOME -> navStack = listOf(Route.Home)
                WorkflowActionId.BACK -> navigateBack()
                WorkflowActionId.SEARCH_THREADS -> {
                    if (navigationMode != AdaptiveNavigationMode.EXPANDED || currentRoute == Route.Home) {
                        navStack = listOf(Route.Home)
                    }
                    focusSearchRequest += 1
                }
                WorkflowActionId.NEXT_THREAD,
                WorkflowActionId.PREVIOUS_THREAD,
                -> {
                    val direction = if (id == WorkflowActionId.NEXT_THREAD) 1 else -1
                    val key = neighboringThread(
                        orderedThreadKeys = orderedThreadKeys,
                        current = currentRoute.threadKeyOrNull,
                        direction = direction,
                    ) ?: return "No neighboring thread is available"
                    navigateToConversation(key)
                }
                WorkflowActionId.NEW_THREAD -> openDirectoryPicker()
                WorkflowActionId.OPEN_TERMINAL -> navigate(Route.Terminal())
                WorkflowActionId.OPEN_SETTINGS -> showSettings = true
                WorkflowActionId.SHOW_COMMAND_PALETTE -> showCommandPalette = true
                WorkflowActionId.OPEN_REVIEW,
                WorkflowActionId.OPEN_FILES,
                -> return action.disabledReason ?: "That surface is not available yet"
            }
            return null
        }

        val workflowActionContext = currentWorkflowActionContext()
        val workflowActions = WorkflowActionCatalog.actions(workflowActionContext)
        val visibleApprovals = snapshot?.pendingApprovals.orEmpty().filter {
            it.kind != ApprovalKind.MCP_ELICITATION
        }
        val visibleUserInputs = snapshot?.pendingUserInputs.orEmpty().filter {
            val currentThreadKey = currentRoute.threadKeyOrNull
            currentThreadKey != null &&
                it.isRelevantToThread(currentThreadKey) &&
                !dismissedUserInputs.isDismissed(it)
        }
        val globalInputBlocked = homeWorkflowBlocked ||
            conversationWorkflowBlocked ||
            navigationPaneInputFocused ||
            currentRoute is Route.Terminal ||
            currentRoute is Route.SavedApp ||
            visibleApprovals.isNotEmpty() ||
            visibleUserInputs.isNotEmpty() ||
            showCommandPalette ||
            showDiscovery ||
            showSettings ||
            showAccountForServer != null ||
            directoryPickerServerId != null ||
            showProjectPicker

        LaunchedEffect(globalInputBlocked) {
            if (!globalInputBlocked) {
                runCatching { rootFocusRequester.requestFocus() }
            }
        }

        val interceptSystemBack =
            showCommandPalette ||
                showDiscovery ||
                showSettings ||
                showAccountForServer != null ||
                directoryPickerServerId != null ||
                showProjectPicker ||
                navStack.size > 1

        BackHandler(enabled = interceptSystemBack) {
            when {
                showCommandPalette -> showCommandPalette = false
                showAccountForServer != null -> showAccountForServer = null
                directoryPickerServerId != null -> directoryPickerServerId = null
                showProjectPicker -> showProjectPicker = false
                showSettings -> showSettings = false
                showDiscovery -> {
                    showDiscovery = false
                    networkDiscovery.stopScanning()
                }
                navStack.size > 1 -> navStack = navStack.dropLast(1)
            }
        }

        // Auto-navigate to active thread when it changes.
        // Home-composer sends don't call setActiveThread, so this only triggers
        // for real "open a thread" actions (e.g. voice session handoff).
        LaunchedEffect(snapshot?.activeThread) {
            val activeKey = snapshot?.activeThread ?: return@LaunchedEffect
            val isInitialObservation = !hasObservedInitialActiveThread
            hasObservedInitialActiveThread = true
            if (
                shouldPreserveRestoredRoute(
                    navigationWasRestored = navigationWasRestored,
                    isInitialActiveThreadObservation = isInitialObservation,
                    currentRoute = currentRoute,
                )
            ) {
                return@LaunchedEffect
            }
            val alreadyShowing = currentRoute.threadKeyOrNull == activeKey
            if (!alreadyShowing) {
                navStack = conversationStack(activeKey)
            }
        }

        val workflowKeyboardModifier = Modifier
            .onKeyEvent { event ->
                val actionId = actionForKeyEvent(
                    event = event,
                    globalInputBlocked = globalInputBlocked,
                ) ?: return@onKeyEvent false
                executeWorkflowAction(actionId) == null
            }
            .focusRequester(rootFocusRequester)
            .focusable()

        val rootModifier = (if (currentRoute is Route.Conversation || currentRoute is Route.Terminal) {
            Modifier
                .fillMaxSize()
                .background(RemoraTheme.background)
        } else {
            Modifier
                .fillMaxSize()
                .background(RemoraTheme.background)
                .systemBarsPadding()
        }).then(workflowKeyboardModifier)

        Box(modifier = rootModifier) {
            AdaptiveWorkflowScaffold(
                route = currentRoute,
                sessions = workflowSessions,
                actions = workflowActions,
                terminalEnabled = terminalEnabled,
                focusSearchRequest = focusSearchRequest,
                onHome = { executeWorkflowAction(WorkflowActionId.HOME) },
                onNewThread = { executeWorkflowAction(WorkflowActionId.NEW_THREAD) },
                onSearch = { executeWorkflowAction(WorkflowActionId.SEARCH_THREADS) },
                onShowPalette = { executeWorkflowAction(WorkflowActionId.SHOW_COMMAND_PALETTE) },
                onOpenTerminal = { executeWorkflowAction(WorkflowActionId.OPEN_TERMINAL) },
                onShowSettings = { executeWorkflowAction(WorkflowActionId.OPEN_SETTINGS) },
                onOpenThread = navigateToConversation,
                onNavigationModeChanged = { navigationMode = it },
                onInputFocusChanged = { navigationPaneInputFocused = it },
            ) {
                when (val route = currentRoute) {
                is Route.Home -> {
                    HomeDashboardScreen(
                        onOpenConversation = navigateToConversation,
                        onShowDiscovery = { showDiscovery = true },
                        onShowSettings = { showSettings = true },
                        onShowApps = { navigate(Route.Apps) },
                        onOpenProjectPicker = { showProjectPicker = true },
                        onOpenAccount = { serverId -> showAccountForServer = serverId },
                        selectedProject = selectedProject,
                        selectedServerId = selectedServerId,
                        onSelectServer = { server ->
                            // Tap again to clear the filter and show all.
                            if (selectedServerId == server.serverId) {
                                selectedServerId = null
                                selectedProject = null
                            } else {
                                selectedServerId = server.serverId
                            }
                        },
                        onThreadCreated = { key ->
                            SavedThreadsStore.add(
                                context,
                                PinnedThreadKey(serverId = key.serverId, threadId = key.threadId),
                            )
                        },
                        onStartVoice = {
                            scope.launch {
                                val launchState = appModel.launchState.snapshot.value
                                val threadKey = voiceController.preparePinnedLocalVoiceThread(
                                    appModel = appModel,
                                    cwd = launchState.currentCwd.ifBlank { "~" },
                                    model = launchState.selectedModel.ifBlank { null },
                                )
                                if (threadKey != null) {
                                    navigate(Route.RealtimeVoice(threadKey))
                                }
                            }
                        },
                        onOpenSavedApp = { appId -> navigate(Route.SavedApp(appId)) },
                        onOpenTerminal = if (ExperimentalFeatures.isEnabled(RemoraFeature.TERMINAL)) {
                            { navigate(Route.Terminal()) }
                        } else {
                            null
                        },
                        focusSearchRequest = focusSearchRequest,
                        onInputFocusChanged = { homeWorkflowBlocked = it },
                    )
                }

                is Route.Sessions -> {
                    com.remora.android.ui.sessions.SessionsScreen(
                        serverId = route.serverId,
                        title = route.title,
                        sessionsUiState = sessionsUiState,
                        onOpenConversation = navigateToConversation,
                        onNewSession = { openDirectoryPicker(route.serverId) },
                        onBack = navigateBack,
                        onInfo = { navigate(Route.ServerInfo(route.serverId)) },
                    )
                }

                is Route.Conversation -> {
                    ConversationScreen(
                        threadKey = route.key,
                        onBack = navigateBack,
                        onInfo = { navigate(Route.ConversationInfo(route.key)) },
                        onShowDirectoryPicker = { openDirectoryPicker(route.key.serverId) },
                        onOpenSavedApp = { appId -> navigate(Route.SavedApp(appId)) },
                        onComposerFocusChanged = { conversationWorkflowBlocked = it },
                    )
                }

                is Route.ConversationInfo -> {
                    ConversationInfoScreen(
                        threadKey = route.key,
                        onBack = navigateBack,
                        onChangeWallpaper = { navigate(Route.WallpaperSelection(route.key)) },
                    )
                }

                is Route.WallpaperSelection -> {
                    com.remora.android.ui.settings.WallpaperSelectionScreen(
                        threadKey = route.key,
                        onBack = {
                            WallpaperManager.clearPendingWallpaper()
                            navigateBack()
                        },
                        onApplied = {
                            navStack = navStack.filter {
                                it !is Route.WallpaperSelection &&
                                    it !is Route.WallpaperAdjust
                            }
                        },
                    )
                }

                is Route.WallpaperAdjust -> {
                    com.remora.android.ui.settings.WallpaperAdjustScreen(
                        threadKey = route.key,
                        onBack = navigateBack,
                        onApplied = {
                            // Pop back to conversation info (keep it on the stack)
                            navStack = navStack.filter {
                                it !is Route.WallpaperSelection &&
                                    it !is Route.WallpaperAdjust
                            }
                        },
                    )
                }

                is Route.ServerInfo -> {
                    ConversationInfoScreen(
                        threadKey = null,
                        serverId = route.serverId,
                        onBack = navigateBack,
                        onChangeWallpaper = { navigate(Route.ServerWallpaperSelection(route.serverId)) },
                        onOpenShell = remoteShellLauncher(
                            appModel = appModel,
                            serverId = route.serverId,
                            terminalEnabled = ExperimentalFeatures.isEnabled(RemoraFeature.TERMINAL),
                            navigate = navigate,
                        ),
                    )
                }

                is Route.ServerWallpaperSelection -> {
                    com.remora.android.ui.settings.WallpaperSelectionScreen(
                        threadKey = null,
                        serverId = route.serverId,
                        onBack = {
                            WallpaperManager.clearPendingWallpaper()
                            navigateBack()
                        },
                        onApplied = {
                            navStack = navStack.filter {
                                it !is Route.ServerWallpaperSelection &&
                                    it !is Route.ServerWallpaperAdjust
                            }
                        },
                    )
                }

                is Route.ServerWallpaperAdjust -> {
                    com.remora.android.ui.settings.WallpaperAdjustScreen(
                        threadKey = null,
                        serverId = route.serverId,
                        onBack = navigateBack,
                        onApplied = {
                            navStack = navStack.filter {
                                it !is Route.ServerWallpaperSelection &&
                                    it !is Route.ServerWallpaperAdjust
                            }
                        },
                    )
                }

                is Route.RealtimeVoice -> {
                    com.remora.android.ui.voice.RealtimeVoiceScreen(
                        threadKey = route.key,
                        onBack = navigateBack,
                    )
                }

                is Route.Apps -> {
                    com.remora.android.ui.apps.AppsListScreen(
                        onBack = navigateBack,
                        onOpenApp = { appId -> navigate(Route.SavedApp(appId)) },
                    )
                }

                is Route.SavedApp -> {
                    com.remora.android.ui.apps.SavedAppScreen(
                        appId = route.appId,
                        onBack = navigateBack,
                        onOpenConversation = { key -> navigate(Route.Conversation(key)) },
                    )
                }

                is Route.Terminal -> {
                    TerminalScreen(
                        preferredRemoraLinkHostId = route.preferredRemoraLinkHostId,
                        onBack = navigateBack,
                    )
                }
                }
            }

            // Global approval overlay
            if (visibleApprovals.isNotEmpty() || visibleUserInputs.isNotEmpty()) {
                ApprovalOverlay(
                    approvals = visibleApprovals,
                    userInputs = visibleUserInputs,
                    appStore = appModel.store,
                    onDismissUserInput = { id -> dismissedUserInputs.dismiss(id) },
                )
            }
        }

        if (showCommandPalette) {
            CommandPaletteSheet(
                actions = workflowActions,
                onExecute = ::executeWorkflowAction,
                onDismiss = { showCommandPalette = false },
            )
        }

        // Discovery bottom sheet
        if (showDiscovery) {
            val discoveredServers by networkDiscovery.servers.collectAsState()
            val isScanning by networkDiscovery.isScanning.collectAsState()
            val scanProgress by networkDiscovery.scanProgress.collectAsState()
            val scanProgressLabel by networkDiscovery.scanProgressLabel.collectAsState()
            val context = LocalContext.current

            // Start scanning when discovery sheet opens
            LaunchedEffect(showDiscovery) {
                networkDiscovery.startScanning(context)
            }

            ModalBottomSheet(
                onDismissRequest = {
                    showDiscovery = false
                    networkDiscovery.stopScanning()
                },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                DiscoveryScreen(
                    discoveredServers = discoveredServers,
                    isScanning = isScanning,
                    scanProgress = scanProgress,
                    scanProgressLabel = scanProgressLabel,
                    onRefresh = { networkDiscovery.startScanning(context) },
                    onDismiss = {
                        showDiscovery = false
                        networkDiscovery.stopScanning()
                    },
                )
            }
        }

        // Settings bottom sheet
        if (showSettings) {
            ModalBottomSheet(
                onDismissRequest = { showSettings = false },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                SettingsSheet(
                    onDismiss = {
                        showSettings = false
                    },
                    onOpenAccount = { serverId ->
                        showSettings = false
                        showAccountForServer = serverId
                    },
                    onOpenApps = {
                        showSettings = false
                        navigate(Route.Apps)
                    },
                )
            }
        }

        if (directoryPickerServerId != null) {
            ModalBottomSheet(
                onDismissRequest = {
                    directoryPickerServerId = null
                    directoryPickerForProject = false
                },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                DirectoryPickerSheet(
                    servers = connectedServerOptions,
                    initialServerId = directoryPickerServerId!!,
                    onSelect = { serverId, cwd ->
                        directoryPickerServerId = null
                        val forProject = directoryPickerForProject
                        directoryPickerForProject = false
                        if (forProject) {
                            selectedServerId = serverId
                            val id = projectIdFor(serverId, cwd)
                            val match = projects.firstOrNull { it.id == id }
                            selectedProject = match ?: AppProject(
                                id = id,
                                serverId = serverId,
                                cwd = cwd,
                                lastUsedAtMs = null,
                            )
                            RecentDirectoryStore(context).record(serverId, cwd)
                        } else {
                            scope.launch {
                                runCatching { startNewSession(serverId, cwd) }
                                    .onFailure { error ->
                                        if (error is LocalAccountLoginRequiredException) {
                                            showAccountForServer = error.serverId
                                        }
                                    }
                            }
                        }
                    },
                    onDismiss = {
                        directoryPickerServerId = null
                        directoryPickerForProject = false
                    },
                )
            }
        }

        if (showProjectPicker) {
            ModalBottomSheet(
                onDismissRequest = { showProjectPicker = false },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                val serverNames = remember(snapshot) {
                    snapshot?.servers?.associate { it.serverId to it.displayName } ?: emptyMap()
                }
                val isLocalById = remember(snapshot) {
                    snapshot?.servers?.associate { it.serverId to it.isLocal } ?: emptyMap()
                }
                ProjectPickerSheet(
                    projects = projects,
                    serverNamesById = serverNames,
                    isLocalById = isLocalById,
                    onSelect = { project ->
                        selectedServerId = project.serverId
                        selectedProject = project
                    },
                    onCreateNew = {
                        showProjectPicker = false
                        val targetServerId = selectedServerId
                            ?: SessionLaunchSupport.defaultConnectedServerId(
                                connectedServerIds = connectedServerOptions.map { it.id },
                                activeThreadKey = snapshot?.activeThread,
                                preferredServerId = null,
                            )
                        if (targetServerId != null) {
                            directoryPickerForProject = true
                            directoryPickerServerId = targetServerId
                        } else {
                            showDiscovery = true
                        }
                    },
                    onDismiss = { showProjectPicker = false },
                )
            }
        }

        // Account bottom sheet
        showAccountForServer?.let { serverId ->
            ModalBottomSheet(
                onDismissRequest = { showAccountForServer = null },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                AccountSheet(
                    serverId = serverId,
                    onDismiss = { showAccountForServer = null },
                )
            }
        }
    }
}

@Composable
private fun remoteShellLauncher(
    appModel: AppModel,
    serverId: String,
    terminalEnabled: Boolean,
    navigate: (Route) -> Unit,
): (() -> Unit)? {
    val remoraLinkAvailable by appModel.remoraLinkAvailable.collectAsState()
    val remoraLinkJournalRevision by appModel.remoraLinkJournalBackend.revision.collectAsState()
    var terminalHost by remember(serverId) { mutableStateOf<RemoraLinkTerminalHost?>(null) }
    var refreshGeneration by remember(serverId) { mutableLongStateOf(0L) }

    LaunchedEffect(
        appModel,
        serverId,
        terminalEnabled,
        remoraLinkAvailable,
        remoraLinkJournalRevision,
    ) {
        val generation = ++refreshGeneration
        terminalHost = null
        val refreshedHost = if (terminalEnabled && remoraLinkAvailable) {
            try {
                appModel.withRemoraLinkV2 { it.remoraLinkHosts() }
                    .remoraLinkTerminalHost(serverId)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                null
            }
        } else {
            null
        }
        if (generation == refreshGeneration) {
            terminalHost = refreshedHost
        }
    }

    val hostId = terminalHost?.hostId ?: return null
    return { navigate(Route.Terminal(preferredRemoraLinkHostId = hostId)) }
}

private fun PendingUserInputRequest.isRelevantToThread(threadKey: ThreadKey): Boolean {
    if (serverId != threadKey.serverId) return false

    val requestThreadId = threadId.trim()
    return requestThreadId.isEmpty() || requestThreadId == threadKey.threadId
}

private fun android.content.Context.animationsDisabled(): Boolean {
    val scale = runCatching {
        Settings.Global.getFloat(contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE)
    }.getOrDefault(1f)
    return scale == 0f
}

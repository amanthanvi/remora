package com.remora.android.ui.workflow

import com.remora.android.ui.Route
import com.remora.android.ui.threadKeyOrNull
import uniffi.codex_mobile_client.ThreadKey

enum class WorkflowActionCategory(val title: String) {
    NAVIGATE("Navigate"),
    THREAD("Thread"),
    REVIEW("Review"),
    TERMINAL("Terminal"),
    SETTINGS("Settings"),
}

enum class WorkflowActionId {
    HOME,
    BACK,
    SEARCH_THREADS,
    NEXT_THREAD,
    PREVIOUS_THREAD,
    NEW_THREAD,
    OPEN_REVIEW,
    OPEN_FILES,
    OPEN_TERMINAL,
    OPEN_SETTINGS,
    SHOW_COMMAND_PALETTE,
}

data class WorkflowShortcut(
    val displayLabel: String,
)

data class WorkflowActionDefinition(
    val id: WorkflowActionId,
    val category: WorkflowActionCategory,
    val title: String,
    val searchTerms: Set<String>,
    val shortcut: WorkflowShortcut? = null,
    val showsInPalette: Boolean = true,
)

data class WorkflowActionContext(
    val route: Route,
    val canNavigateBack: Boolean,
    val connectedServerCount: Int,
    val terminalEnabled: Boolean,
    val orderedThreadKeys: List<ThreadKey>,
)

data class WorkflowAction(
    val definition: WorkflowActionDefinition,
    val enabled: Boolean,
    val disabledReason: String? = null,
)

object WorkflowActionCatalog {
    private val definitions = listOf(
        WorkflowActionDefinition(
            id = WorkflowActionId.HOME,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Home",
            searchTerms = setOf("dashboard", "sessions"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.BACK,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Back",
            searchTerms = setOf("previous", "navigate"),
            shortcut = WorkflowShortcut("Alt+←"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.SEARCH_THREADS,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Search threads",
            searchTerms = setOf("find", "session", "conversation"),
            shortcut = WorkflowShortcut("Ctrl/⌘+F"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.NEXT_THREAD,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Next thread",
            searchTerms = setOf("session", "conversation", "down"),
            shortcut = WorkflowShortcut("Alt+↓"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.PREVIOUS_THREAD,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Previous thread",
            searchTerms = setOf("session", "conversation", "up"),
            shortcut = WorkflowShortcut("Alt+↑"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.NEW_THREAD,
            category = WorkflowActionCategory.THREAD,
            title = "New thread",
            searchTerms = setOf("start", "session", "conversation"),
            shortcut = WorkflowShortcut("Ctrl/⌘+N"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.OPEN_REVIEW,
            category = WorkflowActionCategory.REVIEW,
            title = "Open review",
            searchTerms = setOf("diff", "changes", "files"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.OPEN_FILES,
            category = WorkflowActionCategory.REVIEW,
            title = "Open files",
            searchTerms = setOf("source", "browser", "workspace"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.OPEN_TERMINAL,
            category = WorkflowActionCategory.TERMINAL,
            title = "Open terminal",
            searchTerms = setOf("shell", "ghostty", "remote"),
            shortcut = WorkflowShortcut("Ctrl/⌘+T"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.OPEN_SETTINGS,
            category = WorkflowActionCategory.SETTINGS,
            title = "Settings",
            searchTerms = setOf("preferences", "appearance", "account"),
            shortcut = WorkflowShortcut("Ctrl/⌘+,"),
        ),
        WorkflowActionDefinition(
            id = WorkflowActionId.SHOW_COMMAND_PALETTE,
            category = WorkflowActionCategory.NAVIGATE,
            title = "Command palette",
            searchTerms = setOf("actions", "commands", "shortcuts"),
            shortcut = WorkflowShortcut("Ctrl/⌘+K"),
            showsInPalette = false,
        ),
    )

    fun actions(context: WorkflowActionContext): List<WorkflowAction> =
        definitions.map { definition -> resolve(definition, context) }

    fun action(id: WorkflowActionId, context: WorkflowActionContext): WorkflowAction =
        resolve(definitions.first { it.id == id }, context)

    private fun resolve(
        definition: WorkflowActionDefinition,
        context: WorkflowActionContext,
    ): WorkflowAction {
        val currentThread = context.route.threadKeyOrNull
        val currentThreadIndex = context.orderedThreadKeys.indexOf(currentThread)
        val availability: Pair<Boolean, String?> = when (definition.id) {
            WorkflowActionId.HOME -> (context.route !is Route.Home) to "Already on Home"
            WorkflowActionId.BACK -> context.canNavigateBack to "Nothing to go back to"
            WorkflowActionId.SEARCH_THREADS ->
                context.orderedThreadKeys.isNotEmpty() to "No threads are available yet"
            WorkflowActionId.NEXT_THREAD,
            WorkflowActionId.PREVIOUS_THREAD,
            -> (currentThreadIndex >= 0 && context.orderedThreadKeys.size > 1) to
                "Open a thread to move between conversations"
            WorkflowActionId.NEW_THREAD ->
                (context.connectedServerCount > 0) to "Connect a host before starting a thread"
            WorkflowActionId.OPEN_REVIEW -> false to
                "Open Review from a conversation's change summary"
            WorkflowActionId.OPEN_FILES -> false to
                "Safe workspace browsing is not available yet"
            WorkflowActionId.OPEN_TERMINAL -> when {
                !context.terminalEnabled -> false to "Enable Remote Terminal in Experimental settings"
                context.route is Route.Terminal -> false to "The terminal is already open"
                else -> true to null
            }
            WorkflowActionId.OPEN_SETTINGS,
            WorkflowActionId.SHOW_COMMAND_PALETTE,
            -> true to null
        }
        return WorkflowAction(
            definition = definition,
            enabled = availability.first,
            disabledReason = if (availability.first) null else availability.second,
        )
    }
}

internal fun neighboringThread(
    orderedThreadKeys: List<ThreadKey>,
    current: ThreadKey?,
    direction: Int,
): ThreadKey? {
    if (orderedThreadKeys.size < 2 || current == null || direction == 0) return null
    val index = orderedThreadKeys.indexOf(current)
    if (index < 0) return null
    val offset = if (direction > 0) 1 else -1
    val nextIndex = (index + offset + orderedThreadKeys.size) % orderedThreadKeys.size
    return orderedThreadKeys[nextIndex]
}

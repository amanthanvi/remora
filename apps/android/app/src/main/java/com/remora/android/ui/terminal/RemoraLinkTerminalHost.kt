package com.remora.android.ui.terminal

import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkScope
import uniffi.codex_mobile_client.TerminalBackendKind

internal data class RemoraLinkTerminalHost(
    val hostId: String,
    val displayName: String,
) {
    val backend: TerminalBackendKind
        get() = TerminalBackendKind.RemoteRemoraLink(
            hostId = hostId,
            shell = null,
        )
}

internal fun AppRemoraLinkHostSummary.isEligibleForRemoraLinkTerminal(): Boolean =
    state == AppRemoraLinkHostState.PAIRED &&
        selectedRuntimeIds.any { it == "shell" } &&
        grantedScopes.any { it == AppRemoraLinkScope.CONNECT_RUNTIME }

internal fun AppRemoraLinkHostSummary.toRemoraLinkTerminalHost(): RemoraLinkTerminalHost? =
    if (isEligibleForRemoraLinkTerminal()) {
        RemoraLinkTerminalHost(
            hostId = hostId,
            displayName = hostDisplayName,
        )
    } else {
        null
    }

internal fun List<AppRemoraLinkHostSummary>.remoraLinkTerminalHosts(): List<RemoraLinkTerminalHost> =
    mapNotNull(AppRemoraLinkHostSummary::toRemoraLinkTerminalHost)

internal fun List<AppRemoraLinkHostSummary>.remoraLinkTerminalHost(
    hostId: String,
): RemoraLinkTerminalHost? =
    firstOrNull { it.hostId == hostId }
        ?.toRemoraLinkTerminalHost()

internal fun resolveInitialTerminalBackendId(
    options: List<Pair<String, String?>>,
    preferredRemoraLinkHostId: String?,
): String? {
    if (preferredRemoraLinkHostId != null) {
        val preferred = preferredRemoraLinkHostId.trim().takeIf(String::isNotEmpty)
            ?: return null
        return options.firstOrNull { (_, hostId) -> hostId == preferred }?.first
    }
    return options.firstOrNull()?.first
}

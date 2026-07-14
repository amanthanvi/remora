package com.remora.android.state

import java.lang.ref.WeakReference
import uniffi.codex_mobile_client.AppStore
import uniffi.codex_mobile_client.TerminalRenderer
import uniffi.codex_mobile_client.TerminalSendToAssistantPayload
import uniffi.codex_mobile_client.ThreadKey

/**
 * Process-wide weak hook to the most recently bound [TerminalRenderer]. The
 * terminal screen uses this for renderer-only semantic context that is not
 * part of the Rust session/store surface, such as "send selection to AI"
 * metadata.
 */
object ActiveTerminalRegistry {
    fun interface SelectionSource {
        /** Return a freshly captured selection on the terminal view thread. */
        fun readFreshSelection(onResult: (String?) -> Unit)
    }

    @Volatile
    private var rendererRef: WeakReference<TerminalRenderer>? = null

    @Volatile
    private var selectionSourceRef: WeakReference<SelectionSource>? = null

    fun register(renderer: TerminalRenderer, selectionSource: SelectionSource) {
        rendererRef = WeakReference(renderer)
        selectionSourceRef = WeakReference(selectionSource)
    }

    fun unregister(renderer: TerminalRenderer) {
        val current = rendererRef?.get()
        if (current === renderer) {
            rendererRef = null
            selectionSourceRef = null
        }
    }

    /// Live `TerminalRenderer` if any. The terminal screen uses this to
    /// read cell metrics for grid sizing without threading a renderer
    /// reference through every Composable.
    fun current(): TerminalRenderer? = rendererRef?.get()

    /// Read the current painted selection, if any. The "Send to AI" path
    /// prefers this over the bundled output buffer so the user can scope
    /// the message to a specific block.
    fun readSelection(onResult: (String?) -> Unit) {
        val source = selectionSourceRef?.get()
        dispatchFreshSelection(source) { text ->
            // Discard a result from a view that was unregistered or replaced
            // while its main-thread refresh was queued.
            onResult(text.takeIf { selectionSourceRef?.get() === source })
        }
    }

    /**
     * Send `selection` to the assistant on `threadKey`, pulling OSC cwd +
     * last completed command from the active renderer's semantic state.
     * Suspends until [AppStore.startTurn] returns; throws on RPC error.
     */
    suspend fun sendTextToAssistant(
        store: AppStore,
        threadKey: ThreadKey,
        selection: String,
    ) {
        val renderer = rendererRef?.get() ?: return
        renderer.sendTextToAssistant(
            store = store,
            payload = TerminalSendToAssistantPayload(
                threadKey = threadKey,
                includeCwd = true,
                includeLastCommand = true,
            ),
            selection = selection,
        )
    }
}

/** Callback dispatcher extracted so the asynchronous freshness contract can
 * be unit-tested without constructing a native Ghostty surface. */
internal fun dispatchFreshSelection(
    source: ActiveTerminalRegistry.SelectionSource?,
    onResult: (String?) -> Unit,
) {
    if (source == null) {
        onResult(null)
        return
    }
    source.readFreshSelection { text ->
        onResult(text?.takeIf { it.isNotEmpty() })
    }
}

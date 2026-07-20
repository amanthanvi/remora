package com.remora.android.state

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppStoreInterface
import uniffi.codex_mobile_client.TerminalBackendKind
import uniffi.codex_mobile_client.TerminalOutputEventListener
import uniffi.codex_mobile_client.TerminalOutputSnapshot
import uniffi.codex_mobile_client.TerminalOutputStreamEvent
import uniffi.codex_mobile_client.TerminalOutputSubscription
import uniffi.codex_mobile_client.TerminalSession
import uniffi.codex_mobile_client.TerminalSize
import uniffi.codex_mobile_client.TerminalSshTrustStore

sealed interface TerminalRenderUpdate {
    data class Replace(val bytes: ByteArray) : TerminalRenderUpdate

    data class Append(val bytes: ByteArray) : TerminalRenderUpdate
}

class TerminalSessionController(
    private val scope: CoroutineScope,
    private val appStore: AppStoreInterface = AppModel.shared.store,
) {
    enum class Phase {
        IDLE,
        CONNECTING,
        RUNNING,
        EXITED,
        FAILED,
    }

    data class SshHostTrustChallenge(
        val host: String,
        val port: UShort,
        val fingerprint: String,
        val backend: TerminalBackendKind,
    )

    var phase by mutableStateOf(Phase.IDLE)
        private set
    var output by mutableStateOf("")
        private set
    var exitCode by mutableStateOf<Int?>(null)
        private set
    var errorMessage by mutableStateOf<String?>(null)
        private set
    var sshTrustChallenge by mutableStateOf<SshHostTrustChallenge?>(null)
        private set

    var sessionId: String? = null
        private set
    private var listener: TerminalOutputEventListener? = null
    private var outputSubscription: TerminalOutputSubscription? = null
    @Volatile
    private var outputSink: ((TerminalRenderUpdate) -> Unit)? = null
    private var outputBytes = ByteArray(0)
    private var expectedSequence: ULong? = null
    private val eventLock = Any()
    private val pendingEvents = ArrayDeque<TerminalOutputStreamEvent>()
    private var eventDrainScheduled = false
    @Volatile
    private var eventGeneration: Int = 0
    private var terminalCols: UShort = 80u
    private var terminalRows: UShort = 24u

    private fun activeSession(): TerminalSession? =
        sessionId?.let { appStore.terminalSessionHandle(it) }

    val canSendInput: Boolean
        get() = phase == Phase.RUNNING

    fun open(backend: TerminalBackendKind) {
        if (sessionId != null || phase == Phase.CONNECTING) return
        eventGeneration += 1
        val generation = eventGeneration
        phase = Phase.CONNECTING
        errorMessage = null
        exitCode = null
        sshTrustChallenge = null
        scope.launch {
            try {
                val size = TerminalSize(cols = terminalCols, rows = terminalRows)
                val id = if (backend is TerminalBackendKind.RemoteSsh) {
                    val backendImpl = SshTrustStore(AppModel.shared.appContext)
                    val trustStore = TerminalSshTrustStore(backendImpl)
                    appStore.openTerminalSessionWithTrustStore(backend, size, trustStore)
                } else {
                    appStore.openTerminalSession(backend, size)
                }
                if (generation != eventGeneration) {
                    runCatching { appStore.closeTerminalSession(id) }
                    return@launch
                }
                val opened = appStore.terminalSessionHandle(id) ?: run {
                    runCatching { appStore.closeTerminalSession(id) }
                    if (generation != eventGeneration) return@launch
                    errorMessage = "Session disappeared after open"
                    phase = Phase.FAILED
                    return@launch
                }
                sessionId = id
                appStore.setActiveTerminalId(id)
                val outputListener = object : TerminalOutputEventListener {
                    override fun onEvent(event: TerminalOutputStreamEvent) {
                        var scheduleDrain = false
                        synchronized(eventLock) {
                            if (generation == eventGeneration) {
                                pendingEvents.addLast(event)
                                if (!eventDrainScheduled) {
                                    eventDrainScheduled = true
                                    scheduleDrain = true
                                }
                            }
                        }
                        if (scheduleDrain) {
                            scope.launch(Dispatchers.Main.immediate) {
                                drainOutputEvents(generation)
                            }
                        }
                    }
                }
                outputSubscription = opened.subscribeOutputEvents(outputListener)
                listener = outputListener
                phase = Phase.RUNNING
            } catch (error: Exception) {
                if (generation != eventGeneration) return@launch
                sessionId = null
                val challenge = sshHostTrustChallenge(error, backend)
                if (challenge != null) {
                    sshTrustChallenge = challenge
                    errorMessage = "Unknown SSH host key ${challenge.fingerprint}"
                } else {
                    errorMessage = error.message ?: "Unable to open terminal"
                }
                phase = Phase.FAILED
            }
        }
    }

    fun trustUnknownSshHostAndRetry() {
        val challenge = sshTrustChallenge ?: return
        SshTrustStore(AppModel.shared.appContext).write(
            host = challenge.host,
            port = challenge.port,
            fingerprint = challenge.fingerprint,
        )
        sshTrustChallenge = null
        errorMessage = null
        phase = Phase.IDLE
        open(challenge.backend)
    }

    fun switchBackend(backend: TerminalBackendKind) {
        close()
        replaceOutput(byteArrayOf())
        open(backend)
    }

    fun send(value: String) {
        sendBytes(value.toByteArray(Charsets.UTF_8))
    }

    fun sendBytes(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        val activeSession = activeSession() ?: return
        if (!canSendInput) return
        scope.launch {
            try {
                activeSession.writeInput(bytes)
            } catch (error: Exception) {
                errorMessage = error.message ?: "Unable to write terminal input"
                phase = Phase.FAILED
            }
        }
    }

    fun sendLine(value: String) {
        send("$value\n")
    }

    fun clearOutput() {
        replaceOutput(byteArrayOf())
    }

    fun setOutputSink(sink: ((TerminalRenderUpdate) -> Unit)?) {
        outputSink = sink
        if (sink == null) {
            output = outputBytes.toString(Charsets.UTF_8)
        }
        sink?.invoke(TerminalRenderUpdate.Replace(outputBytes.copyOf()))
    }

    fun replayOutputToSink() {
        outputSink?.invoke(TerminalRenderUpdate.Replace(outputBytes.copyOf()))
    }

    private fun sshHostTrustChallenge(
        error: Exception,
        backend: TerminalBackendKind,
    ): SshHostTrustChallenge? {
        val sshBackend = backend as? TerminalBackendKind.RemoteSsh ?: return null
        val fingerprint = unknownHostFingerprint(error.message.orEmpty()) ?: return null
        return SshHostTrustChallenge(
            host = sshBackend.host,
            port = sshBackend.port,
            fingerprint = fingerprint,
            backend = backend,
        )
    }

    private fun unknownHostFingerprint(message: String): String? {
        val marker = "unknown-host:"
        val start = message.indexOf(marker)
        if (start < 0) return null
        return message
            .substring(start + marker.length)
            .trim()
            .trim('"', '\'', '(', ')', '[', ']')
            .takeIf { it.isNotEmpty() }
    }

    fun resize(cols: Int, rows: Int, notifyBackend: Boolean = true) {
        if (cols <= 0 || rows <= 0) return
        terminalCols = cols.coerceIn(1, UShort.MAX_VALUE.toInt()).toUShort()
        terminalRows = rows.coerceIn(1, UShort.MAX_VALUE.toInt()).toUShort()
        val activeSession = activeSession() ?: return
        if (!notifyBackend || !canSendInput) return
        val size = TerminalSize(cols = terminalCols, rows = terminalRows)
        scope.launch {
            try {
                activeSession.resize(size)
            } catch (error: Exception) {
                errorMessage = error.message ?: "Unable to resize terminal"
                phase = Phase.FAILED
            }
        }
    }

    fun close() {
        eventGeneration += 1
        outputSubscription?.cancel()
        outputSubscription = null
        listener = null
        synchronized(eventLock) {
            pendingEvents.clear()
            eventDrainScheduled = false
        }
        val id = sessionId
        sessionId = null
        phase = Phase.IDLE
        if (id == null) return
        scope.launch {
            runCatching { appStore.closeTerminalSession(id) }
        }
    }

    private fun drainOutputEvents(generation: Int) {
        val events = synchronized(eventLock) {
            val drained = pendingEvents.toList()
            pendingEvents.clear()
            eventDrainScheduled = false
            drained
        }
        if (generation != eventGeneration) return
        events.forEach(::applyOutputEvent)
    }

    private fun applyOutputEvent(event: TerminalOutputStreamEvent) {
        when (event) {
            is TerminalOutputStreamEvent.Snapshot -> applySnapshot(event.snapshot)
            is TerminalOutputStreamEvent.Reset -> applySnapshot(event.snapshot)
            is TerminalOutputStreamEvent.Output -> applyOutput(event.sequence, event.data)
            is TerminalOutputStreamEvent.Exited -> {
                val expected = expectedSequence
                if (expected == null || event.sequence >= expected) {
                    expectedSequence = event.sequence + 1u
                    exitCode = event.code
                    phase = Phase.EXITED
                }
            }
        }
    }

    private fun applySnapshot(snapshot: TerminalOutputSnapshot) {
        expectedSequence = snapshot.latestSequence?.plus(1u) ?: snapshot.baseSequence
        replaceOutput(snapshot.bytes)
        snapshot.exitCode?.let { code ->
            exitCode = code
            phase = Phase.EXITED
        }
    }

    private fun applyOutput(sequence: ULong, data: ByteArray) {
        val expected = expectedSequence
        if (expected != null) {
            if (sequence < expected) return
            if (sequence > expected) {
                activeSession()?.let { applySnapshot(it.outputSnapshot()) }
                return
            }
        }
        expectedSequence = sequence + 1u
        appendOutput(data)
    }

    private fun replaceOutput(data: ByteArray) {
        outputBytes = boundedOutput(data)
        output = outputBytes.toString(Charsets.UTF_8)
        outputSink?.invoke(TerminalRenderUpdate.Replace(outputBytes.copyOf()))
    }

    private fun appendOutput(data: ByteArray) {
        if (data.isEmpty()) return
        outputBytes = boundedOutput(outputBytes, data)
        if (outputSink == null) {
            output = outputBytes.toString(Charsets.UTF_8)
        }
        outputSink?.invoke(TerminalRenderUpdate.Append(data.copyOf()))
    }

    private fun boundedOutput(data: ByteArray): ByteArray {
        val limit = 64 * 1024
        if (data.size <= limit) return data.copyOf()
        return data.copyOfRange(data.size - limit, data.size)
    }

    private fun boundedOutput(existing: ByteArray, appended: ByteArray): ByteArray {
        val limit = 64 * 1024
        if (appended.size >= limit) {
            return appended.copyOfRange(appended.size - limit, appended.size)
        }
        val existingCount = minOf(existing.size, limit - appended.size)
        return ByteArray(existingCount + appended.size).also { combined ->
            existing.copyInto(
                destination = combined,
                destinationOffset = 0,
                startIndex = existing.size - existingCount,
            )
            appended.copyInto(combined, destinationOffset = existingCount)
        }
    }
}

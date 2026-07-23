package com.remora.android.voice

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import com.remora.android.util.LLog
import kotlinx.coroutines.suspendCancellableCoroutine
import org.webrtc.AudioSource
import org.webrtc.AudioTrack
import org.webrtc.DataChannel
import org.webrtc.DefaultVideoDecoderFactory
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.RtpReceiver
import org.webrtc.RtpTransceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.audio.JavaAudioDeviceModule
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

class RealtimeWebRtcSessionException(message: String, cause: Throwable? = null) : RuntimeException(message, cause)

class RealtimeWebRtcSession(private val context: Context) {

    private val audioManager: AudioManager =
        context.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    private var peerConnection: PeerConnection? = null
    private var dataChannel: DataChannel? = null
    private var audioSource: AudioSource? = null
    private var audioTrack: AudioTrack? = null
    private val isClosed = AtomicBoolean(false)

    private var previousAudioMode: Int = audioManager.mode
    private var previousCommunicationDevice: AudioDeviceInfo? = null
    private var previousSpeakerphoneOn: Boolean = false
    private var audioFocusRequest: AudioFocusRequest? = null
    private val didConfigureAudio = AtomicBoolean(false)

    private var iceGatheringContinuation: kotlinx.coroutines.CancellableContinuation<Unit>? = null

    suspend fun start(): String {
        if (peerConnection != null) {
            throw RealtimeWebRtcSessionException("session already started")
        }
        isClosed.set(false)

        LLog.debug(TAG) { "start: configuring audio session" }
        configureAudio()

        LLog.debug(TAG) { "start: acquiring shared PeerConnectionFactory" }
        val factory = sharedFactory(context)
        val rtcConfig = PeerConnection.RTCConfiguration(emptyList()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            continualGatheringPolicy = PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
        }

        LLog.debug(TAG) { "start: creating PeerConnection" }
        val connection = factory.createPeerConnection(rtcConfig, DelegateAdapter())
            ?: run {
                releaseAudio()
                throw RealtimeWebRtcSessionException("PeerConnectionFactory.createPeerConnection returned null")
            }
        peerConnection = connection

        val source = factory.createAudioSource(MediaConstraints())
        audioSource = source
        val track = factory.createAudioTrack("realtime-audio", source)
        audioTrack = track

        val transceiverInit = RtpTransceiver.RtpTransceiverInit(
            RtpTransceiver.RtpTransceiverDirection.SEND_RECV,
            listOf("realtime")
        )
        connection.addTransceiver(track, transceiverInit)

        dataChannel = connection.createDataChannel("oai-events", DataChannel.Init())

        val offerConstraints = MediaConstraints().apply {
            mandatory.add(MediaConstraints.KeyValuePair("OfferToReceiveAudio", "true"))
            mandatory.add(MediaConstraints.KeyValuePair("OfferToReceiveVideo", "false"))
        }

        LLog.debug(TAG) { "start: creating offer" }
        val offer = try {
            createOffer(connection, offerConstraints)
        } catch (error: Throwable) {
            LLog.debug(TAG, error) { "WebRTC offer creation failed" }
            cleanup()
            throw error
        }

        LLog.debug(TAG) { "start: setting local description" }
        try {
            setLocalDescription(connection, offer)
        } catch (error: Throwable) {
            LLog.debug(TAG, error) { "WebRTC local description setup failed" }
            cleanup()
            throw error
        }

        LLog.debug(TAG) { "start: awaiting ICE gathering completion" }
        awaitIceGatheringComplete(connection)

        if (isClosed.get() || peerConnection !== connection) {
            LLog.w(TAG, "WebRTC session closed before ICE gathering completed")
            throw RealtimeWebRtcSessionException("session stopped before local description was ready")
        }

        val localSdp = connection.localDescription?.description
            ?: run {
                LLog.debug(TAG) { "WebRTC local description unavailable after ICE gathering" }
                cleanup()
                throw RealtimeWebRtcSessionException("local description unavailable after ICE gathering")
            }
        LLog.debug(TAG) { "start: offer ready (${localSdp.length} bytes)" }
        return localSdp
    }

    suspend fun applyAnswer(sdp: String) {
        val connection = peerConnection
            ?: throw RealtimeWebRtcSessionException("cannot apply answer: session not started")
        LLog.debug(TAG) { "applyAnswer: setting remote description (${sdp.length} bytes)" }
        setRemoteDescription(connection, SessionDescription(SessionDescription.Type.ANSWER, sdp))
        LLog.debug(TAG) { "applyAnswer: remote description applied" }
    }

    fun stop() {
        LLog.debug(TAG) { "stop: closing peer connection" }
        cleanup()
    }

    private fun cleanup() {
        isClosed.set(true)
        iceGatheringContinuation?.let {
            iceGatheringContinuation = null
            if (it.isActive) it.resume(Unit)
        }
        try {
            dataChannel?.close()
        } catch (error: Throwable) {
            LLog.w(TAG, "WebRTC data channel cleanup failed")
            LLog.debug(TAG, error) { "WebRTC data channel cleanup failure details" }
        }
        dataChannel?.dispose()
        dataChannel = null
        audioTrack = null
        audioSource?.dispose()
        audioSource = null
        try {
            peerConnection?.close()
        } catch (error: Throwable) {
            LLog.w(TAG, "WebRTC peer connection cleanup failed")
            LLog.debug(TAG, error) { "WebRTC peer connection cleanup failure details" }
        }
        peerConnection?.dispose()
        peerConnection = null
        releaseAudio()
    }

    @SuppressLint("SetAndClearCommunicationDevice") // releaseAudio restores or clears every successful selection.
    private fun configureAudio() {
        if (!didConfigureAudio.compareAndSet(false, true)) return
        previousAudioMode = audioManager.mode
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            previousCommunicationDevice = audioManager.communicationDevice
        } else {
            previousSpeakerphoneOn = legacySpeakerphoneState()
        }

        val attrs = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()

        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
            .setAudioAttributes(attrs)
            .setOnAudioFocusChangeListener { }
            .build()
        audioFocusRequest = request
        audioManager.requestAudioFocus(request)

        audioManager.mode = AudioManager.MODE_IN_COMMUNICATION
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val speaker = audioManager.availableCommunicationDevices.firstOrNull {
                it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
            }
            if (speaker == null || !audioManager.setCommunicationDevice(speaker)) {
                LLog.w(TAG, "Unable to select the built-in speaker for voice communication")
            }
        } else {
            setLegacySpeakerphoneState(true)
        }
    }

    private fun releaseAudio() {
        if (!didConfigureAudio.compareAndSet(true, false)) return
        audioFocusRequest?.let { audioManager.abandonAudioFocusRequest(it) }
        audioFocusRequest = null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            restoreSelectedCommunicationDevice(
                previousDevice = previousCommunicationDevice,
                selectDevice = audioManager::setCommunicationDevice,
                clearDevice = audioManager::clearCommunicationDevice,
            )
            previousCommunicationDevice = null
        } else {
            setLegacySpeakerphoneState(previousSpeakerphoneOn)
        }
        audioManager.mode = previousAudioMode
    }

    @Suppress("DEPRECATION")
    private fun legacySpeakerphoneState(): Boolean = audioManager.isSpeakerphoneOn

    @Suppress("DEPRECATION")
    private fun setLegacySpeakerphoneState(enabled: Boolean) {
        audioManager.isSpeakerphoneOn = enabled
    }

    private suspend fun createOffer(
        connection: PeerConnection,
        constraints: MediaConstraints
    ): SessionDescription = suspendCancellableCoroutine { cont ->
        connection.createOffer(object : SdpObserver {
            override fun onCreateSuccess(description: SessionDescription) {
                if (cont.isActive) cont.resume(description)
            }

            override fun onCreateFailure(reason: String?) {
                if (cont.isActive) cont.resumeWithException(
                    RealtimeWebRtcSessionException("createOffer failed: $reason")
                )
            }

            override fun onSetSuccess() {}
            override fun onSetFailure(reason: String?) {}
        }, constraints)
    }

    private suspend fun setLocalDescription(
        connection: PeerConnection,
        description: SessionDescription
    ): Unit = suspendCancellableCoroutine { cont ->
        connection.setLocalDescription(object : SdpObserver {
            override fun onCreateSuccess(description: SessionDescription) {}
            override fun onCreateFailure(reason: String?) {}
            override fun onSetSuccess() {
                if (cont.isActive) cont.resume(Unit)
            }

            override fun onSetFailure(reason: String?) {
                if (cont.isActive) cont.resumeWithException(
                    RealtimeWebRtcSessionException("setLocalDescription failed: $reason")
                )
            }
        }, description)
    }

    private suspend fun setRemoteDescription(
        connection: PeerConnection,
        description: SessionDescription
    ): Unit = suspendCancellableCoroutine { cont ->
        connection.setRemoteDescription(object : SdpObserver {
            override fun onCreateSuccess(description: SessionDescription) {}
            override fun onCreateFailure(reason: String?) {}
            override fun onSetSuccess() {
                if (cont.isActive) cont.resume(Unit)
            }

            override fun onSetFailure(reason: String?) {
                if (cont.isActive) cont.resumeWithException(
                    RealtimeWebRtcSessionException("setRemoteDescription failed: $reason")
                )
            }
        }, description)
    }

    private suspend fun awaitIceGatheringComplete(connection: PeerConnection) {
        if (connection.iceGatheringState() == PeerConnection.IceGatheringState.COMPLETE) return
        suspendCancellableCoroutine<Unit> { cont ->
            iceGatheringContinuation = cont
            cont.invokeOnCancellation { iceGatheringContinuation = null }
        }
    }

    private fun onIceGatheringState(state: PeerConnection.IceGatheringState) {
        LLog.debug(TAG) { "onIceGatheringChange: $state" }
        if (state != PeerConnection.IceGatheringState.COMPLETE) return
        val cont = iceGatheringContinuation ?: return
        iceGatheringContinuation = null
        if (cont.isActive) cont.resume(Unit)
    }

    private inner class DelegateAdapter : PeerConnection.Observer {
        override fun onSignalingChange(newState: PeerConnection.SignalingState) {
            LLog.debug(TAG) { "onSignalingChange: $newState" }
        }

        override fun onIceConnectionChange(newState: PeerConnection.IceConnectionState) {
            LLog.debug(TAG) { "onIceConnectionChange: $newState" }
        }

        override fun onIceConnectionReceivingChange(receiving: Boolean) {}
        override fun onIceGatheringChange(newState: PeerConnection.IceGatheringState) {
            onIceGatheringState(newState)
        }

        override fun onIceCandidate(candidate: IceCandidate) {}
        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) {}
        override fun onAddStream(stream: MediaStream) {}
        override fun onRemoveStream(stream: MediaStream) {}
        override fun onDataChannel(channel: DataChannel) {
            LLog.debug(TAG) { "onDataChannel: ${channel.label()}" }
        }

        override fun onRenegotiationNeeded() {}
        override fun onAddTrack(receiver: RtpReceiver, streams: Array<out MediaStream>) {
            LLog.debug(TAG) { "onAddTrack: streams=${streams.size}" }
        }
    }

    companion object {
        private const val TAG = "RealtimeWebRtc"

        @Volatile
        private var sharedFactoryInstance: PeerConnectionFactory? = null

        @Volatile
        private var sharedEglBase: EglBase? = null

        private fun sharedFactory(context: Context): PeerConnectionFactory {
            val existing = sharedFactoryInstance
            if (existing != null) return existing
            return synchronized(this) {
                val again = sharedFactoryInstance
                if (again != null) return@synchronized again
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions
                        .builder(context.applicationContext)
                        .createInitializationOptions()
                )
                val egl = EglBase.create()
                sharedEglBase = egl
                val adm = JavaAudioDeviceModule.builder(context.applicationContext)
                    .setUseHardwareAcousticEchoCanceler(true)
                    .setUseHardwareNoiseSuppressor(true)
                    .createAudioDeviceModule()
                val factory = PeerConnectionFactory.builder()
                    .setAudioDeviceModule(adm)
                    .setVideoEncoderFactory(DefaultVideoEncoderFactory(egl.eglBaseContext, true, true))
                    .setVideoDecoderFactory(DefaultVideoDecoderFactory(egl.eglBaseContext))
                    .createPeerConnectionFactory()
                sharedFactoryInstance = factory
                factory
            }
        }
    }
}

internal fun <T> restoreSelectedCommunicationDevice(
    previousDevice: T?,
    selectDevice: (T) -> Boolean,
    clearDevice: () -> Unit,
) {
    if (previousDevice == null || !selectDevice(previousDevice)) {
        clearDevice()
    }
}

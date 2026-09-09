package com.remora.android.background

import com.remora.android.state.CustodyFixture
import java.lang.reflect.Proxy
import kotlin.coroutines.Continuation
import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import uniffi.codex_mobile_client.*

class BackgroundRelayIngressTest {
    private val token = "provider-registration-0123456".toByteArray()
    private val wake = OpaqueWakeHint(1, "installation0000001", "event000000000001", 42,
        OpaqueWakeEventClass.STATE_CHANGED, 2000)

    @Test fun forwardsOnlyTypedInputsAndReplaysSameGenerationAfterEnrollment() = runBlocking {
        val inputs = PushTokenInputStore(CustodyFixture().store()).apply { registered(token) }
        val calls = mutableListOf<String>()
        val observed = mutableListOf<ByteArray>()
        val client = client { name, args ->
            calls.add(name)
            when (name) {
                "backgroundRelayIngestWake" -> {
                    assertEquals(wake.toRelayHint(), args[0])
                    AppRelayReconcileReceipt("host-1", 42u, 42u, true)
                }
                "backgroundRelayObservePushToken" -> {
                    assertEquals(AppRelayPushTokenObservation(AppRelayPushProvider.FCM,
                        AppRelayPushEnvironment.PRODUCTION, 1u), args[0])
                    observed.add(args[1] as ByteArray)
                    assertArrayEquals(token, observed.last())
                    AppRelayFanoutReceipt(0u, 0u, 0u, 0u, 0u)
                }
                "backgroundRelayReconcile" -> emptyList<AppRelayReconcileOutcome>()
                else -> error("Unexpected call $name")
            }
        }
        assertTrue(reconcileRelayInputs(client, inputs, wake, nowMs = 1000))
        assertTrue(reconcileRelayInputs(client, inputs, nowMs = 1000))
        assertEquals(listOf("backgroundRelayIngestWake", "backgroundRelayObservePushToken",
            "backgroundRelayReconcile", "backgroundRelayObservePushToken", "backgroundRelayReconcile"), calls)
        assertTrue(observed.all { bytes -> bytes.all { it == 0.toByte() } })
    }

    @Test fun rejectedOrExpiredWakeStillRepairsEveryHost() = runBlocking {
        val inputs = PushTokenInputStore(CustodyFixture().store())
        var repairs = 0
        var ingests = 0
        val client = client { name, _ -> when (name) {
            "backgroundRelayIngestWake" -> { ingests++; throw BackgroundRelayException.UnknownInstallation() }
            "backgroundRelayReconcile" -> { repairs++; emptyList<AppRelayReconcileOutcome>() }
            else -> error("Unexpected call $name")
        } }
        assertTrue(reconcileRelayInputs(client, inputs, wake, 1000))
        assertTrue(reconcileRelayInputs(client, inputs, wake, 3000))
        assertEquals(1, ingests)
        assertEquals(2, repairs)
    }

    @Test fun tokenFailureCannotPreventRepairAndFailedHostRemainsRetryable() = runBlocking {
        val inputs = PushTokenInputStore(CustodyFixture().store()).apply { registered(token) }
        var repairs = 0
        val client = client { name, _ -> when (name) {
            "backgroundRelayObservePushToken" -> throw BackgroundRelayException.Retryable()
            "backgroundRelayReconcile" -> {
                repairs++
                listOf(AppRelayReconcileOutcome.Failed("host-1", AppRelayFailure.RETRYABLE))
            }
            else -> error("Unexpected call $name")
        } }
        assertFalse(reconcileRelayInputs(client, inputs))
        assertEquals(1, repairs)
    }

    @Test fun tombstoneUsesPriorGenerationAndCancellationDoesNotContinueNetworkCalls() = runBlocking {
        val inputs = PushTokenInputStore(CustodyFixture().store()).apply {
            registered(token)
            unregistered(token)
        }
        val client = client { name, args ->
            assertEquals("backgroundRelayTombstonePushToken", name)
            assertEquals(AppRelayPushTokenTombstone(AppRelayPushProvider.FCM,
                AppRelayPushEnvironment.PRODUCTION, 1u), args[0])
            throw CancellationException("worker cancelled")
        }
        try {
            reconcileRelayInputs(client, inputs)
            fail("Cancellation must reach WorkManager")
        } catch (_: CancellationException) { }
    }

    private fun client(call: (String, Array<out Any?>) -> Any?): AppClientInterface =
        Proxy.newProxyInstance(AppClientInterface::class.java.classLoader,
            arrayOf(AppClientInterface::class.java)) { _, method, args ->
            try {
                call(method.name, args ?: emptyArray())
            } catch (failure: Throwable) {
                @Suppress("UNCHECKED_CAST")
                val continuation = args!!.last() as Continuation<Any?>
                continuation.resumeWith(Result.failure(failure))
                COROUTINE_SUSPENDED
            }
        } as AppClientInterface
}

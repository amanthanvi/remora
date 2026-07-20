package com.remora.android.state

import java.lang.reflect.InvocationHandler
import java.lang.reflect.Method
import java.lang.reflect.Proxy
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.codex_mobile_client.AppStoreInterface
import uniffi.codex_mobile_client.TerminalBackendKind

class TerminalSessionControllerTest {
    @Test
    fun immediatelyExitedSessionIsClosedBeforeControllerPublishesIt() {
        val store = ImmediatelyExitedTerminalStore()
        val controller = TerminalSessionController(
            scope = CoroutineScope(Dispatchers.Unconfined),
            appStore = store.proxy,
        )

        controller.open(
            TerminalBackendKind.RemoteRemoraLink(
                hostId = "test-host",
                shell = null,
            ),
        )

        assertNull(controller.sessionId)
        assertEquals(TerminalSessionController.Phase.FAILED, controller.phase)
        assertEquals("Session disappeared after open", controller.errorMessage)
        assertNull(store.activeTerminalId)
        assertEquals(listOf("open", "handle", "close"), store.operations)
    }
}

private class ImmediatelyExitedTerminalStore : InvocationHandler {
    val operations = mutableListOf<String>()
    var activeTerminalId: String? = null
        private set

    val proxy: AppStoreInterface = Proxy.newProxyInstance(
        AppStoreInterface::class.java.classLoader,
        arrayOf(AppStoreInterface::class.java),
        this,
    ) as AppStoreInterface

    override fun invoke(proxy: Any, method: Method, args: Array<out Any?>?): Any? =
        when (method.name) {
            "openTerminalSession" -> {
                operations += "open"
                activeTerminalId = SESSION_ID
                SESSION_ID
            }
            "terminalSessionHandle" -> {
                operations += "handle"
                null
            }
            "closeTerminalSession" -> {
                operations += "close"
                activeTerminalId = null
                Unit
            }
            "activeTerminalId" -> activeTerminalId
            "setActiveTerminalId" -> {
                operations += "setActive"
                activeTerminalId = args?.firstOrNull() as String?
                Unit
            }
            "toString" -> "ImmediatelyExitedTerminalStore"
            "hashCode" -> System.identityHashCode(this)
            "equals" -> proxy === args?.firstOrNull()
            else -> error("Unexpected AppStore call: ${method.name}")
        }

    private companion object {
        const val SESSION_ID = "exited-session"
    }
}

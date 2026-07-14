package com.remora.android.auth

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.URI
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.Future
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException

class ChatGPTOAuthLoopbackServerTest {
    private val executor = Executors.newSingleThreadExecutor()

    @After
    fun tearDown() {
        executor.shutdownNow()
    }

    @Test
    fun bindHostsForRedirectHost_supportsIpv4AndIpv6LoopbackForLocalhost() {
        assertEquals(
            listOf("127.0.0.1", "::1"),
            ChatGPTOAuthLoopbackServer.bindHostsForRedirectHost("localhost"),
        )
        assertEquals(
            listOf("127.0.0.1", "::1"),
            ChatGPTOAuthLoopbackServer.bindHostsForRedirectHost("LOCALHOST"),
        )
    }

    @Test
    fun bindHostsForRedirectHost_keepsExplicitHosts() {
        assertEquals(
            listOf("127.0.0.1"),
            ChatGPTOAuthLoopbackServer.bindHostsForRedirectHost("127.0.0.1"),
        )
    }

    @Test
    fun requestTargetFromLine_parsesGetRequests() {
        assertEquals(
            "/auth/callback?code=abc&state=xyz",
            ChatGPTOAuthLoopbackServer.requestTargetFromLine(
                "GET /auth/callback?code=abc&state=xyz HTTP/1.1",
            ),
        )
    }

    @Test
    fun callbackUriForRequest_reusesRedirectOrigin() {
        val callbackUri = URI.create(
            ChatGPTOAuthLoopbackServer.callbackUriStringForRequest(
                redirectUri = "http://localhost:1455/auth/callback",
                requestTarget = "/auth/callback?code=abc&state=xyz",
            ),
        )

        assertEquals("http", callbackUri.scheme)
        assertEquals("localhost", callbackUri.host)
        assertEquals(1455, callbackUri.port)
        assertEquals("/auth/callback", callbackUri.path)
        assertEquals("code=abc&state=xyz", callbackUri.rawQuery)
    }

    @Test
    fun successHtml_mentionsReturnToApp() {
        val html = ChatGPTOAuthLoopbackServer.successHtml("remoraauth://chatgpt-auth-complete")
        assertTrue(html.contains("Returning to Remora"))
        assertTrue(html.contains("remoraauth://chatgpt-auth-complete"))
    }

    @Test
    fun awaitCallback_acceptsLiveSocketAndReturnsCallback() {
        withServer { server, port ->
            val callback = runAwait(server)
            Socket("127.0.0.1", port).use { client ->
                client.soTimeout = 2_000
                client.getOutputStream().writer(Charsets.UTF_8).apply {
                    write("GET /auth/callback?code=abc&state=xyz HTTP/1.1\r\n")
                    write("Host: localhost\r\n")
                    write("\r\n")
                    flush()
                }
                val responseStatus = BufferedReader(
                    InputStreamReader(client.getInputStream(), Charsets.UTF_8),
                ).readLine()
                assertEquals("HTTP/1.1 200 OK", responseStatus)
            }

            assertEquals(
                "http://127.0.0.1:$port/auth/callback?code=abc&state=xyz",
                callback.get(2, TimeUnit.SECONDS),
            )
        }
    }

    @Test
    fun close_cancelsBlockedAcceptAndIsIdempotent() {
        withServer { server, _ ->
            val callback = runAwait(server)

            server.close()
            server.close()

            assertFutureCancelled(callback)
        }
    }

    @Test
    fun close_closesAcceptedClientAndCancelsBlockedRead() {
        withServer(clientReadTimeoutMs = 5_000) { server, port ->
            val callback = runAwait(server)
            Socket("127.0.0.1", port).use { client ->
                client.soTimeout = 2_000
                client.getOutputStream().write(
                    "GET /auth/callback?code=abc&state=xyz HTTP/1.1\r\n".toByteArray(),
                )
                client.getOutputStream().flush()
                waitForActiveClient(server)

                server.close()

                assertFutureCancelled(callback)
                val socketWasClosed = runCatching { client.getInputStream().read() }
                    .fold(
                        onSuccess = { it == -1 },
                        onFailure = { it is SocketException },
                    )
                assertTrue("Expected the accepted client socket to close", socketWasClosed)
            }
        }
    }

    @Test
    fun acceptedClientReadTimeout_doesNotPreventLaterCallback() {
        withServer(clientReadTimeoutMs = 100) { server, port ->
            val callback = runAwait(server)
            Socket("127.0.0.1", port).use {
                waitForActiveClient(server)
                waitForNoActiveClient(server)
            }

            sendValidCallback(port)
            assertEquals(
                "http://127.0.0.1:$port/auth/callback?code=abc&state=xyz",
                callback.get(2, TimeUnit.SECONDS),
            )
        }
    }

    @Test
    fun malformedClient_doesNotPreventLaterCallback() {
        withServer { server, port ->
            val callback = runAwait(server)
            Socket("127.0.0.1", port).use { client ->
                client.getOutputStream().write("POST / HTTP/1.1\r\n\r\n".toByteArray())
                client.getOutputStream().flush()
            }

            sendValidCallback(port)
            assertEquals(
                "http://127.0.0.1:$port/auth/callback?code=abc&state=xyz",
                callback.get(2, TimeUnit.SECONDS),
            )
        }
    }

    private fun withServer(
        clientReadTimeoutMs: Int = 1_000,
        body: (ChatGPTOAuthLoopbackServer, Int) -> Unit,
    ) {
        val port = ServerSocket(0).use { it.localPort }
        ChatGPTOAuthLoopbackServer.createForTest(
            redirectUri = "http://127.0.0.1:$port/auth/callback",
            appReturnUri = "remoraauth://chatgpt-auth-complete",
            clientReadTimeoutMs = clientReadTimeoutMs,
        ).use { server ->
            body(server, port)
        }
    }

    private fun runAwait(server: ChatGPTOAuthLoopbackServer): Future<String> {
        return executor.submit<String> { server.awaitCallbackUriString() }
    }

    private fun sendValidCallback(port: Int) {
        Socket("127.0.0.1", port).use { client ->
            client.soTimeout = 2_000
            client.getOutputStream().writer(Charsets.UTF_8).apply {
                write("GET /auth/callback?code=abc&state=xyz HTTP/1.1\r\n")
                write("Host: localhost\r\n")
                write("\r\n")
                flush()
            }
            val responseStatus = BufferedReader(
                InputStreamReader(client.getInputStream(), Charsets.UTF_8),
            ).readLine()
            assertEquals("HTTP/1.1 200 OK", responseStatus)
        }
    }

    private fun assertFutureCancelled(future: Future<String>) {
        assertTrue(futureFailure(future) is CancellationException)
    }

    private fun waitForActiveClient(server: ChatGPTOAuthLoopbackServer) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (!server.hasActiveClientForTest() && System.nanoTime() < deadline) {
            Thread.yield()
        }
        assertTrue("Expected the server to accept the client", server.hasActiveClientForTest())
    }

    private fun waitForNoActiveClient(server: ChatGPTOAuthLoopbackServer) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (server.hasActiveClientForTest() && System.nanoTime() < deadline) {
            Thread.yield()
        }
        assertTrue("Expected the client read to time out", !server.hasActiveClientForTest())
    }

    private fun futureFailure(future: Future<String>): Throwable {
        return try {
            future.get(2, TimeUnit.SECONDS)
            fail("Expected awaitCallback to fail")
            error("unreachable")
        } catch (error: ExecutionException) {
            error.cause ?: error
        }
    }
}

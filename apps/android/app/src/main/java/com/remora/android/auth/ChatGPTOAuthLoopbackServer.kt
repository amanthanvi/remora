package com.remora.android.auth

import android.net.Uri
import com.remora.android.state.ChatGPTOAuthException
import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URI
import java.nio.channels.CancelledKeyException
import java.nio.channels.ClosedSelectorException
import java.nio.channels.SelectionKey
import java.nio.channels.Selector
import java.nio.channels.ServerSocketChannel
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException

internal class ChatGPTOAuthLoopbackServer private constructor(
    private val redirectUri: String,
    private val serverChannels: List<ServerSocketChannel>,
    private val selector: Selector,
    private val appReturnUri: String,
    private val clientReadTimeoutMs: Int,
) : AutoCloseable {
    private val closed = AtomicBoolean(false)
    private val activeClient = AtomicReference<Socket?>(null)

    fun awaitCallback(): Uri {
        return Uri.parse(awaitCallbackUriString())
    }

    internal fun awaitCallbackUriString(): String {
        while (!closed.get()) {
            try {
                selector.select()
                if (closed.get()) throw closedCancellation()
                val selectedKeys = selector.selectedKeys().iterator()
                while (selectedKeys.hasNext()) {
                    val key = selectedKeys.next()
                    selectedKeys.remove()
                    if (!key.isValid || !key.isAcceptable) continue

                    val client = (key.channel() as ServerSocketChannel).accept()?.socket() ?: continue
                    activeClient.set(client)
                    if (closed.get()) {
                        closeActiveClient(client)
                        throw closedCancellation()
                    }
                    try {
                        client.soTimeout = clientReadTimeoutMs
                        return handleCallbackSocket(client)
                    } catch (_: ChatGPTOAuthException) {
                        continue
                    } catch (_: IllegalArgumentException) {
                        continue
                    } catch (error: IOException) {
                        if (closed.get()) throw closedCancellation(error)
                        continue
                    } finally {
                        activeClient.compareAndSet(client, null)
                    }
                }
            } catch (error: ClosedSelectorException) {
                throw closedCancellation(error)
            } catch (error: CancelledKeyException) {
                if (closed.get()) throw closedCancellation(error)
            } catch (error: IOException) {
                if (closed.get()) throw closedCancellation(error)
            }
        }
        throw closedCancellation()
    }

    private fun handleCallbackSocket(socket: Socket): String {
        socket.use { client ->
            val requestTarget = readRequestTarget(client)
                ?: throw ChatGPTOAuthException("ChatGPT login callback was malformed.")
            val callbackUri = callbackUriStringForRequest(redirectUri, requestTarget)
            writeHtmlResponse(
                client = client,
                statusLine = "HTTP/1.1 200 OK",
                body = successHtml(appReturnUri),
            )
            return callbackUri
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        selector.wakeup()
        activeClient.getAndSet(null)?.let(::closeActiveClient)
        serverChannels.forEach { channel ->
            runCatching { channel.close() }
        }
        runCatching { selector.close() }
    }

    private fun closeActiveClient(client: Socket) {
        runCatching { client.close() }
    }

    internal fun hasActiveClientForTest(): Boolean = activeClient.get() != null

    private fun closedCancellation(cause: Throwable? = null): CancellationException {
        return CancellationException("ChatGPT login loopback server closed.").also { cancellation ->
            if (cause != null) cancellation.initCause(cause)
        }
    }

    companion object {
        private const val CLIENT_READ_TIMEOUT_MS = 5_000

        fun create(
            redirectUri: String,
            appReturnUri: Uri,
        ): ChatGPTOAuthLoopbackServer {
            return create(
                redirectUri = redirectUri,
                appReturnUri = appReturnUri.toString(),
                clientReadTimeoutMs = CLIENT_READ_TIMEOUT_MS,
            )
        }

        internal fun createForTest(
            redirectUri: String,
            appReturnUri: String,
            clientReadTimeoutMs: Int = CLIENT_READ_TIMEOUT_MS,
        ): ChatGPTOAuthLoopbackServer {
            return create(redirectUri, appReturnUri, clientReadTimeoutMs)
        }

        private fun create(
            redirectUri: String,
            appReturnUri: String,
            clientReadTimeoutMs: Int,
        ): ChatGPTOAuthLoopbackServer {
            val parsedRedirect = try {
                URI.create(redirectUri)
            } catch (_: IllegalArgumentException) {
                throw ChatGPTOAuthException("ChatGPT login redirect URI is malformed.")
            }
            val host = parsedRedirect.host?.takeIf { it.isNotBlank() }
                ?: throw ChatGPTOAuthException("ChatGPT login redirect URI is missing a host.")
            val port = parsedRedirect.port.takeIf { it > 0 }
                ?: throw ChatGPTOAuthException("ChatGPT login redirect URI is missing a port.")

            val selector = Selector.open()
            val channels = mutableListOf<ServerSocketChannel>()
            val errors = mutableListOf<String>()
            for (bindHost in bindHostsForRedirectHost(host)) {
                val channel = ServerSocketChannel.open()
                try {
                    channel.configureBlocking(false)
                    channel.socket().reuseAddress = true
                    channel.bind(
                        InetSocketAddress(
                            InetAddress.getByName(bindHost),
                            port,
                        ),
                        1,
                    )
                    channel.register(selector, SelectionKey.OP_ACCEPT)
                    channels += channel
                } catch (error: Exception) {
                    runCatching { channel.close() }
                    errors += "$bindHost: ${error.localizedMessage ?: error.message ?: error::class.java.simpleName}"
                }
            }

            if (channels.isEmpty()) {
                runCatching { selector.close() }
                throw ChatGPTOAuthException(
                    "ChatGPT login could not bind a localhost callback server. ${errors.joinToString("; ")}",
                )
            }

            return ChatGPTOAuthLoopbackServer(
                redirectUri = redirectUri,
                serverChannels = channels,
                selector = selector,
                appReturnUri = appReturnUri,
                clientReadTimeoutMs = clientReadTimeoutMs,
            )
        }

        internal fun bindHostsForRedirectHost(host: String): List<String> {
            if (!host.equals("localhost", ignoreCase = true)) {
                return listOf(host)
            }
            return listOf("127.0.0.1", "::1")
        }

        internal fun requestTargetFromLine(requestLine: String): String? {
            val parts = requestLine.trim().split(' ')
            if (parts.size < 2) return null
            if (!parts[0].equals("GET", ignoreCase = true)) return null
            return parts[1].takeIf { it.isNotBlank() }
        }

        internal fun callbackUriForRequest(redirectUri: Uri, requestTarget: String): Uri {
            return Uri.parse(callbackUriStringForRequest(redirectUri.toString(), requestTarget))
        }

        internal fun callbackUriStringForRequest(redirectUri: String, requestTarget: String): String {
            val base = URI.create(redirectUri.toString())
            val target = URI.create(requestTarget)
            val resolved = URI(
                base.scheme,
                base.userInfo,
                base.host,
                base.port,
                target.path ?: base.path,
                target.rawQuery,
                target.rawFragment,
            )
            return resolved.toString()
        }

        internal fun successHtml(appReturnUri: String): String = """
            <!doctype html>
            <html lang="en">
            <head>
              <meta charset="utf-8">
              <meta name="viewport" content="width=device-width, initial-scale=1">
              <title>Remora Login Complete</title>
              <meta http-equiv="refresh" content="0;url=$appReturnUri">
              <style>
                body {
                  margin: 0;
                  font-family: sans-serif;
                  background: #02082c;
                  color: #eafbff;
                  display: flex;
                  align-items: center;
                  justify-content: center;
                  min-height: 100vh;
                  padding: 24px;
                }
                main {
                  max-width: 420px;
                  line-height: 1.5;
                }
                h1 {
                  font-size: 24px;
                  margin: 0 0 12px 0;
                }
                p {
                  color: #a8dceb;
                  margin: 0;
                }
                a {
                  color: #0dd5f0;
                }
              </style>
              <script>
                window.location.replace("$appReturnUri");
              </script>
            </head>
            <body>
              <main>
                <h1>Login complete</h1>
                <p>Returning to Remora. If nothing happens, <a href="$appReturnUri">tap here</a>.</p>
              </main>
            </body>
            </html>
        """.trimIndent()

        private fun readRequestTarget(client: Socket): String? {
            val reader = BufferedReader(InputStreamReader(client.getInputStream(), Charsets.UTF_8))
            val requestLine = reader.readLine() ?: return null
            while (true) {
                val line = reader.readLine() ?: break
                if (line.isBlank()) break
            }
            return requestTargetFromLine(requestLine)
        }

        private fun writeHtmlResponse(
            client: Socket,
            statusLine: String,
            body: String,
        ) {
            val bytes = body.toByteArray(Charsets.UTF_8)
            OutputStreamWriter(client.getOutputStream(), Charsets.UTF_8).use { writer ->
                writer.appendLine(statusLine)
                writer.appendLine("Content-Type: text/html; charset=utf-8")
                writer.appendLine("Content-Length: ${bytes.size}")
                writer.appendLine("Connection: close")
                writer.appendLine()
                writer.append(body)
                writer.flush()
            }
        }
    }
}

package com.remora.android.ui.discovery

import com.remora.android.state.SavedServer
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

internal sealed interface WakeSignalResult {
    data class Codex(val port: Int) : WakeSignalResult
    data class Ssh(val port: Int) : WakeSignalResult
    data object None : WakeSignalResult
}

internal suspend fun waitForWakeSignal(
    host: String,
    preferredCodexPort: Int?,
    preferredSshPort: Int?,
    timeoutMillis: Long,
    wakeMac: String?,
): WakeSignalResult = withContext(Dispatchers.IO) {
    val codexPorts = orderedCodexPorts(preferredCodexPort)
    val sshPorts = orderedSshPorts(preferredSshPort)
    val deadline = System.currentTimeMillis() + maxOf(timeoutMillis, 500L)
    var lastWakePacketAt = 0L

    while (System.currentTimeMillis() < deadline) {
        val now = System.currentTimeMillis()
        if (!wakeMac.isNullOrBlank() && now - lastWakePacketAt >= 2_000L) {
            sendWakeMagicPacket(wakeMac, host)
            lastWakePacketAt = now
        }

        for (port in codexPorts) {
            if (isPortOpen(host, port, 700)) {
                return@withContext WakeSignalResult.Codex(port)
            }
        }

        for (port in sshPorts) {
            if (isPortOpen(host, port, 700)) {
                return@withContext WakeSignalResult.Ssh(port)
            }
        }

        delay(350)
    }

    WakeSignalResult.None
}

private fun orderedCodexPorts(preferred: Int?): List<Int> = buildList {
    preferred?.let(::add)
    addAll(listOf(8390, 9234, 4222))
}.filter { it in 1..65535 }.distinct()

private fun orderedSshPorts(preferred: Int?): List<Int> = buildList {
    preferred?.let(::add)
    add(22)
}.filter { it in 1..65535 }.distinct()

private fun sendWakeMagicPacket(wakeMac: String, hostHint: String) {
    val mac = SavedServer.normalizeWakeMac(wakeMac) ?: return
    val macBytes = mac.split(':').mapNotNull { it.toIntOrNull(16)?.toByte() }
    if (macBytes.size != 6) {
        return
    }

    val packet = ByteArray(6 + 16 * macBytes.size)
    repeat(6) { packet[it] = 0xFF.toByte() }
    for (index in 0 until 16) {
        macBytes.forEachIndexed { byteIndex, value ->
            packet[6 + index * macBytes.size + byteIndex] = value
        }
    }

    wakeBroadcastTargets(hostHint).forEach { target ->
        sendBroadcastUdp(packet, target, 9)
        sendBroadcastUdp(packet, target, 7)
    }
}

private fun wakeBroadcastTargets(host: String): Set<String> {
    val targets = linkedSetOf("255.255.255.255")
    val ipv4Parts = host.split('.')
    if (ipv4Parts.size == 4 && ipv4Parts.all { it.toIntOrNull() != null }) {
        targets += "${ipv4Parts[0]}.${ipv4Parts[1]}.${ipv4Parts[2]}.255"
    }
    return targets
}

private fun sendBroadcastUdp(packet: ByteArray, host: String, port: Int) {
    runCatching {
        DatagramSocket().use { socket ->
            socket.broadcast = true
            val address = InetAddress.getByName(host)
            socket.send(DatagramPacket(packet, packet.size, address, port))
        }
    }
}

private fun isPortOpen(host: String, port: Int, timeoutMillis: Int): Boolean =
    runCatching {
        Socket().use { socket ->
            socket.connect(InetSocketAddress(host, port), timeoutMillis)
            true
        }
    }.getOrDefault(false)

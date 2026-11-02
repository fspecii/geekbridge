// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader

/*
 * DEBUG BUILDS ONLY (src/debug). The app's own process log, redacted, kept in a bounded ring even while no
 * link exists and sent to the paired computer as `geekbridge.debug.log` packets (docs/debug-diagnostics.md).
 * Android lets an app read its own log without READ_LOGS, so this runs `logcat --pid=<own pid>`.
 */

/** Packet kinds of the debug extension (same strings as `bridge_core::debug_link`). */
object DebugKinds {
    const val LOG = "geekbridge.debug.log"
    const val STATE_REQUEST = "geekbridge.debug.state_request"
    const val STATE = "geekbridge.debug.state"
}

/** Removes anything personal from one log line before it is buffered, sent or shown. Pure; unit tested. */
object DebugRedactor {
    const val MAX_LINE = 512

    private val header = Regex("""^(\d\d-\d\d \d\d:\d\d:\d\d\.\d{3}\s+\d+\s+\d+\s+[VDIWEF]\s+)(.*)$""")

    /** `body=...`, `"address":"..."`: everything after a content-bearing key goes, to the end of the line. */
    private val sensitiveKey = Regex(
        """(?i)\b(body|address|addresses|number|phone|msisdn|display_?name|contact_?name|caller_?name|sender|recipient|""" +
            """title|snippet|subject|clip|clipboard|text|message|msg|vcard|description|location|email|attendees?|""" +
            """organizer|query|label|note|notes|cmd|payload)\b(\s*[=:]\s*|"\s*:\s*)""",
    )
    /** Credentials and pairing secrets: the key may carry a prefix/suffix (`qr_token`, `authToken`); the value goes. */
    private val secretKey = Regex(
        """(?i)[\w.-]*(?:token|secret|passw(?:or)?d|pwd|passphrase|authoriz\w*|cookie|api_?key|private_?key|""" +
            """verification_?code|pairing_?code)[\w.-]*(?:\s*[=:]\s*|"\s*:\s*)""",
    )
    private val shortSecretKey = Regex("""(?i)\b(?:pin|otp|auth)\b(?:\s*[=:]\s*|"\s*:\s*)""")
    private val bearer = Regex("""(?i)\b(bearer|basic)\s+\S+""")
    private val jwt = Regex("""eyJ[\w-]+\.[\w-]+\.[\w-]*""")
    /** A long mixed letter+digit run with no dots: base64 or similar credential material, whatever it was called. */
    private val blob = Regex("""(?<![\w/.+=-])(?=[A-Za-z0-9+/_=-]*\d)(?=[A-Za-z0-9+/_=-]*[A-Za-z])[A-Za-z0-9+/_=-]{24,}""")
    private val email = Regex("""[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}""")
    private val ipv4 = Regex("""(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?![\d.])""")
    private val colonHex = Regex("""(?<![0-9A-Za-z:])[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7}(?![0-9A-Za-z:])""")
    private val uuid = Regex("""\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b""")
    private val longHex = Regex("""\b[0-9a-fA-F]{16,}\b""")
    private val numberLike = Regex("""\+?\d[\d ()./-]{5,}\d""")
    private val storagePath = Regex("""(/storage/|/sdcard/|/mnt/media_rw/)\S*""")

    fun line(raw: String, allowedIps: Set<String> = emptySet()): String {
        val m = header.matchEntire(raw)
        val prefix = m?.groupValues?.get(1).orEmpty()
        var s = m?.groupValues?.get(2) ?: raw
        s = jwt.replace(s, "<token>")
        s = bearer.replace(s) { "${it.groupValues[1]} <redacted>" }
        for (key in listOf(secretKey, shortSecretKey, sensitiveKey)) {
            key.find(s)?.let { s = s.substring(0, it.range.last + 1) + "<redacted>" }
        }
        s = blob.replace(s, "<secret>")
        s = email.replace(s, "<email>")
        s = uuid.replace(s, "<id>")
        // A paired computer's own address is allowed; park it while the generic number rule runs.
        val parked = ArrayList<String>()
        s = ipv4.replace(s) {
            if (it.value in allowedIps) {
                parked += it.value
                "\u0001${parked.size - 1}\u0001"
            } else {
                "<ip>"
            }
        }
        s = colonHex.replace(s) { r ->
            val v = r.value
            // Times (10:11:12) and the like stay; real IPv6 / MAC strings contain a hex letter.
            if (v.any { it in 'a'..'f' || it in 'A'..'F' } && v.count { it == ':' } >= 2) "<addr>" else v
        }
        s = longHex.replace(s, "<hex>")
        s = numberLike.replace(s) { r -> if (r.value.count { it.isDigit() } >= 7) "<number>" else r.value }
        s = storagePath.replace(s, "<path>")
        for ((n, ip) in parked.withIndex()) s = s.replace("\u0001$n\u0001", ip)
        val out = prefix + s
        return if (out.length > MAX_LINE) out.substring(0, MAX_LINE) else out
    }

    /** Whole multi-line text (a state dump): every line through [line]. */
    fun text(raw: String, allowedIps: Set<String> = emptySet()): String =
        raw.lines().joinToString("\n") { line(it, allowedIps) }
}

/** Bounded buffer of redacted lines: at most [maxLines] lines and [maxBytes] bytes, oldest dropped first. */
class LogRing(val maxLines: Int = 2000, val maxBytes: Int = 256 * 1024) {
    private val lines = ArrayDeque<String>()
    private var bytes = 0
    private var first = 0L // absolute index of lines[0]

    data class Chunk(val lines: List<String>, val nextIndex: Long)

    @Synchronized fun add(line: String) {
        val b = Bounds.utf8Size(line) + 1
        lines.addLast(line)
        bytes += b
        while (lines.size > maxLines || (bytes > maxBytes && lines.size > 1)) {
            bytes -= Bounds.utf8Size(lines.removeFirst()) + 1
            first++
        }
    }

    @Synchronized fun firstIndex(): Long = first
    @Synchronized fun nextIndex(): Long = first + lines.size
    @Synchronized fun size(): Int = lines.size
    @Synchronized fun byteSize(): Int = bytes

    /** Up to [limitLines] lines / [limitBytes] bytes from absolute index [from] (clamped to what is still held). */
    @Synchronized fun read(from: Long, limitLines: Int, limitBytes: Int): Chunk {
        val start = maxOf(from, first)
        var i = (start - first).toInt()
        val out = ArrayList<String>()
        var used = 0
        while (i < lines.size && out.size < limitLines) {
            val l = lines[i]
            val b = Bounds.utf8Size(l) + 1
            if (used + b > limitBytes && out.isNotEmpty()) break
            out += l
            used += b
            i++
        }
        return Chunk(out, start + out.size)
    }

    @Synchronized fun snapshot(): List<String> = lines.toList()
}

sealed interface DebugSendResult {
    object Sent : DebugSendResult
    /** The core's rate limit: try again shortly. */
    object Later : DebugSendResult
    /** That computer does not take debug packets (or is gone): leave it alone for a while. */
    object Parked : DebugSendResult
}

/**
 * Sends new ring lines to each trusted computer that takes debug packets, from its own cursor, so a computer that
 * connects later (or reconnects) gets the buffered backlog and another never gets lines twice. Rate limited
 * (one packet per [MIN_SPACING_MS]) and size capped (<= [MAX_LINES] lines, <= [MAX_BYTES] bytes). Pure apart from the
 * injected [send] and [clock].
 */
class DebugLogStreamer(
    val ring: LogRing,
    private val send: (connectionId: Long, kind: String, body: JSONObject) -> DebugSendResult,
    private val clock: () -> Long,
) {
    private val cursors = HashMap<Long, Long>()
    /** Where each computer (not connection) has been served up to, so a reconnect does not repeat the backlog. */
    private val deviceCursors = HashMap<String, Long>()
    private var devices: Map<Long, String> = emptyMap()
    private val parkedUntil = HashMap<Long, Long>()
    private var connections: Set<Long> = emptySet()
    private var nextSendAt = 0L
    private var seq = 0L

    @Volatile var enabled = true

    @Synchronized fun setConnections(ids: Set<Long>, deviceOfConnection: Map<Long, String> = emptyMap()) {
        connections = ids
        devices = deviceOfConnection
        cursors.keys.retainAll(ids)
        parkedUntil.keys.retainAll(ids)
    }

    @Synchronized fun connectionCount(): Int = connections.size

    /** One step: at most one packet. True while something is still waiting to be sent. */
    @Synchronized fun pump(): Boolean {
        if (!enabled || connections.isEmpty()) return false
        val now = clock()
        if (now < nextSendAt) return true
        var pending = false
        for (id in connections.sorted()) {
            if (now < (parkedUntil[id] ?: 0L)) continue
            // A computer seen before resumes where it stopped; a new one starts with the whole buffered backlog.
            val from = cursorOf(id)
            val chunk = ring.read(from, MAX_LINES, MAX_BYTES)
            if (chunk.lines.isEmpty()) {
                setCursor(id, chunk.nextIndex)
                continue
            }
            val body = JSONObject().put("seq", seq + 1).put("lines", JSONArray(chunk.lines))
            when (send(id, DebugKinds.LOG, body)) {
                DebugSendResult.Sent -> {
                    seq++
                    setCursor(id, chunk.nextIndex)
                    nextSendAt = now + MIN_SPACING_MS
                    pending = pending || chunk.nextIndex < ring.nextIndex()
                    return pending || otherPending(id)
                }
                DebugSendResult.Later -> {
                    nextSendAt = now + LATER_MS
                    return true
                }
                DebugSendResult.Parked -> parkedUntil[id] = now + PARK_MS
            }
        }
        return pending
    }

    private fun cursorOf(id: Long): Long =
        cursors[id] ?: devices[id]?.let { deviceCursors[it] } ?: ring.firstIndex()

    private fun setCursor(id: Long, index: Long) {
        cursors[id] = index
        devices[id]?.let { deviceCursors[it] = index }
    }

    private fun otherPending(except: Long): Boolean =
        connections.any { it != except && cursorOf(it) < ring.nextIndex() }

    companion object {
        const val MAX_LINES = 150
        const val MAX_BYTES = 48 * 1024
        const val MIN_SPACING_MS = 500L
        const val LATER_MS = 1500L
        const val PARK_MS = 30_000L
    }
}

/** Reads this app's own log (`logcat --pid`), redacts each line and hands it on. Restarts if logcat ends. */
class OwnLogReader(private val pid: Int, private val onLine: (String) -> Unit) {
    @Volatile private var stopped = false
    private var thread: Thread? = null
    private var process: Process? = null

    @Synchronized fun start() {
        if (thread != null) return
        thread = Thread({ loop() }, "geekbridge-debug-logcat").apply { isDaemon = true; start() }
    }

    @Synchronized fun stop() {
        stopped = true
        process?.destroy()
        thread?.interrupt()
        thread = null
    }

    private fun loop() {
        var failures = 0
        while (!stopped) {
            try {
                val p = Runtime.getRuntime().exec(arrayOf("logcat", "-v", "threadtime", "--pid=$pid"))
                process = p
                BufferedReader(InputStreamReader(p.inputStream, Charsets.UTF_8)).use { r ->
                    while (!stopped) {
                        val l = r.readLine() ?: break
                        failures = 0
                        onLine(l)
                    }
                }
                p.destroy()
            } catch (_: java.io.IOException) {
            } catch (_: SecurityException) {
            }
            if (stopped) return
            failures++
            try { Thread.sleep(minOf(30_000L, 2_000L * failures)) } catch (_: InterruptedException) { return }
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/** Kotlin side of the `transport_clipboard_*` actions (docs/clipboard-sync.md). Pure; no Android types. */

sealed interface ClipEvent {
    val seq: Long

    /** The computer asks for the phone's latest item. */
    data class Pull(
        override val seq: Long,
        val connectionId: Long,
        val deviceId: String,
        val requestId: String,
        val maxAgeMs: Long,
    ) : ClipEvent {
        override fun toString() = "Pull($seq, $requestId)"
    }

    /** An item from the computer, to be written to the Android clipboard. */
    data class Incoming(
        override val seq: Long,
        val deviceId: String,
        val kind: ClipKind,
        val text: String?,
        val html: String?,
        /** Private spool file of a received image. */
        val imagePath: String?,
        val imageMime: String?,
    ) : ClipEvent {
        override fun toString() = "Incoming($seq, $kind)" // never the content
    }
}

data class WaitResult(val events: List<ClipEvent>, val lastSeq: Long, val stopped: Boolean, val enabled: Boolean)

object ClipboardProtocol {
    const val MAX_WAIT_MS = 50_000L
    private val idPattern = Regex("[A-Za-z0-9_-]{1,64}")
    private val devicePattern = Regex("[A-Za-z0-9_:.-]{1,128}")
    private val mimePattern = Regex("image/(png|jpeg)")

    private fun base(action: String) = JSONObject().put("version", CoreProtocol.VERSION).put("action", action)

    fun configRequest(enabled: Boolean, maxImageBytes: Long): String =
        base("transport_clipboard_config").put("enabled", enabled).put("max_image_bytes", maxImageBytes).toString()

    fun waitRequest(since: Long, timeoutMs: Long = MAX_WAIT_MS): String =
        base("transport_clipboard_wait").put("since", since).put("timeout_ms", timeoutMs.coerceIn(0, MAX_WAIT_MS)).toString()

    /** The `clipboard` object of `transport_start`. */
    fun startObject(s: ClipSettings): JSONObject =
        JSONObject().put("enabled", s.enabled).put("max_image_bytes", s.normalized().maxImageBytes)

    /** Answer a pull (or, with [requestId] null, an explicit send) with text, a URL or HTML. */
    fun sendTextRequest(
        kind: ClipKind,
        text: String,
        html: String?,
        requestId: String?,
        connectionId: Long?,
        ageMs: Long,
    ): String {
        require(kind != ClipKind.Image) { "use sendImageRequest" }
        val r = base("transport_clipboard_send").put("kind", kind.wire).put("text", text).put("age_ms", ageMs.coerceAtLeast(0))
        if (html != null) r.put("html", html)
        if (requestId != null) r.put("request_id", requestId)
        if (connectionId != null) r.put("connection_id", connectionId)
        return r.toString()
    }

    fun sendImageRequest(fd: Int, requestId: String?, connectionId: Long?, ageMs: Long): String {
        val r = base("transport_clipboard_send").put("kind", "image").put("image_fd", fd).put("age_ms", ageMs.coerceAtLeast(0))
        if (requestId != null) r.put("request_id", requestId)
        if (connectionId != null) r.put("connection_id", connectionId)
        return r.toString()
    }

    fun sendNoneRequest(requestId: String, connectionId: Long, reason: String): String =
        base("transport_clipboard_send").put("request_id", requestId).put("connection_id", connectionId).put("none", reason).toString()

    /** Strictly parses a `transport_clipboard_wait` result; null if it is not what the core documents. */
    fun parseWait(result: Any?): WaitResult? {
        val o = result as? JSONObject ?: return null
        val arr = o.optJSONArray("events") ?: return null
        if (arr.length() > 16) return null
        val events = ArrayList<ClipEvent>(arr.length())
        for (i in 0 until arr.length()) parseEvent(arr.optJSONObject(i))?.let { events += it }
        return WaitResult(
            events,
            o.optLong("last_seq", 0),
            o.optBoolean("stopped", false),
            o.optBoolean("enabled", false),
        )
    }

    private fun parseEvent(o: JSONObject?): ClipEvent? {
        o ?: return null
        val seq = (o.opt("seq") as? Number)?.toLong() ?: return null
        val device = o.optString("device_id", "")
        if (!devicePattern.matches(device)) return null
        return when (o.optString("kind")) {
            "pull" -> {
                val id = o.optString("request_id", "")
                val conn = (o.opt("connection_id") as? Number)?.toLong() ?: return null
                if (!idPattern.matches(id)) return null
                ClipEvent.Pull(seq, conn, device, id, o.optLong("max_age_ms", ClipboardLimits.DEFAULT_WINDOW_MS))
            }
            "item" -> {
                val kind = ClipKind.parse(o.optString("item")) ?: return null
                if (kind == ClipKind.Image) {
                    val mime = o.optString("mime", "")
                    val path = o.optString("path", "")
                    if (!mimePattern.matches(mime) || path.isEmpty()) return null
                    ClipEvent.Incoming(seq, device, kind, null, null, path, mime)
                } else {
                    val text = o.optString("text", "")
                    if (text.isEmpty() || Bounds.utf8Size(text) > ClipboardLimits.MAX_TEXT_BYTES) return null
                    val html = if (o.isNull("html")) null else o.optString("html", "").ifEmpty { null }
                    if (html != null && Bounds.utf8Size(html) > ClipboardLimits.MAX_TEXT_BYTES) return null
                    ClipEvent.Incoming(seq, device, kind, text, html, null, null)
                }
            }
            else -> null
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/*
 * Pure-Kotlin side of the `share_*` actions (docs/android-contract.md, "Share actions"). No Android types,
 * so everything here runs in JVM tests. File bytes never pass through here: the glue hands the native core a
 * file descriptor, this file only builds the JSON around it and interprets what the core reports back.
 */

enum class TransferDirection { Incoming, Outgoing }
enum class TransferKind { File, Text, Url }
enum class TransferState { Offered, Waiting, Active, Done, Failed, Cancelled, Declined;
    val finished: Boolean get() = this == Done || this == Failed || this == Cancelled || this == Declined
}

/** One entry of `transport_status.transfers`. [name] and [text] are untrusted display text. */
data class Transfer(
    val id: Long,
    val connectionId: Long,
    val deviceId: String,
    val peerName: String,
    val direction: TransferDirection,
    val kind: TransferKind,
    val name: String,
    val size: Long?,
    val bytes: Long,
    val state: TransferState,
    val code: String?,
    val text: String?,
)

object ShareLimits {
    const val MAX_FILES = 50
    const val MAX_TEXT_BYTES = 48 * 1024
    const val MAX_URL_BYTES = 4096
    const val MAX_NAME_CHARS = 255
    const val MAX_TRANSFERS = 32
    const val MAX_TEXT_CHARS_SHOWN = 16 * 1024
}

object ShareProtocol {
    private fun base(action: String) = JSONObject().put("version", CoreProtocol.VERSION).put("action", action)

    fun sendFile(connectionId: Long, fd: Int, name: String, size: Long, lastModifiedMs: Long?): String {
        val r = base("share_send_file").put("connection_id", connectionId).put("fd", fd)
            .put("name", NameRules.sanitize(name)).put("size", size)
        if (lastModifiedMs != null && lastModifiedMs > 0) r.put("last_modified_ms", lastModifiedMs)
        return r.toString()
    }

    fun sendText(connectionId: Long, kind: TransferKind, text: String): String =
        base("share_send_text").put("connection_id", connectionId)
            .put("kind", if (kind == TransferKind.Url) "url" else "text").put("text", text).toString()

    fun accept(transferId: Long, fd: Int): String = base("share_accept").put("transfer_id", transferId).put("fd", fd).toString()
    fun decline(transferId: Long): String = base("share_decline").put("transfer_id", transferId).toString()
    fun cancel(transferId: Long): String = base("share_cancel").put("transfer_id", transferId).toString()
    fun dismiss(transferId: Long): String = base("share_dismiss").put("transfer_id", transferId).toString()

    private val token = Regex("[A-Za-z0-9_:.-]{1,128}")

    private fun state(s: String?): TransferState? = when (s) {
        "offered" -> TransferState.Offered
        "waiting" -> TransferState.Waiting
        "active" -> TransferState.Active
        "done" -> TransferState.Done
        "failed" -> TransferState.Failed
        "cancelled" -> TransferState.Cancelled
        "declined" -> TransferState.Declined
        else -> null
    }

    private fun kind(s: String?): TransferKind? = when (s) {
        "file" -> TransferKind.File
        "text" -> TransferKind.Text
        "url" -> TransferKind.Url
        else -> null
    }

    private fun nonNegative(o: JSONObject, key: String): Long? =
        (o.opt(key) as? Number)?.toLong()?.takeIf { it >= 0 }

    /** Parses `transfers`; entries that are not exactly understood are dropped, never guessed at. */
    fun parseTransfers(arr: JSONArray?): List<Transfer> {
        if (arr == null) return emptyList()
        val out = ArrayList<Transfer>()
        for (i in 0 until minOf(arr.length(), ShareLimits.MAX_TRANSFERS)) {
            val o = arr.optJSONObject(i) ?: continue
            val id = nonNegative(o, "transfer_id") ?: continue
            val conn = nonNegative(o, "connection_id") ?: continue
            val device = (o.opt("device_id") as? String)?.takeIf { token.matches(it) } ?: continue
            val direction = when (o.opt("direction") as? String) {
                "incoming" -> TransferDirection.Incoming
                "outgoing" -> TransferDirection.Outgoing
                else -> continue
            }
            val kind = kind(o.opt("kind") as? String) ?: continue
            val state = state(o.opt("state") as? String) ?: continue
            val kindText = kind != TransferKind.File
            out += Transfer(
                id = id, connectionId = conn, deviceId = device,
                peerName = Bounds.name(o.optString("peer_name", ""), 48),
                direction = direction, kind = kind,
                name = if (kindText) "" else Bounds.clip(o.optString("name", ""), ShareLimits.MAX_NAME_CHARS),
                size = nonNegative(o, "size"),
                bytes = nonNegative(o, "bytes") ?: 0,
                state = state,
                code = (o.opt("code") as? String)?.takeIf { Regex("[a-z0-9_]{1,32}").matches(it) },
                text = if (kindText) (o.opt("text") as? String)?.take(ShareLimits.MAX_TEXT_CHARS_SHOWN) else null,
            )
        }
        return out
    }
}

object NameRules {
    private const val MAX_BYTES = 120

    /** One safe path component: no separators, control characters or leading dots; bounded; never empty. */
    fun sanitize(raw: String?): String {
        var s = (raw ?: "").substringAfterLast('/').substringAfterLast('\\')
        s = s.map { c -> if (Character.isISOControl(c) || c in "<>:\"|?*") '_' else c }.joinToString("")
        s = s.trim().trimEnd('.', ' ').trimStart('.')
        if (s.isEmpty()) s = "file"
        while (Bounds.utf8Size(s) > MAX_BYTES) {
            val dot = s.lastIndexOf('.')
            s = if (dot > 0 && s.length - dot <= 12) dropLastCodePoint(s.substring(0, dot)) + s.substring(dot) else dropLastCodePoint(s)
            if (s.isEmpty()) return "file"
        }
        return s
    }

    /** Removes the last character, both halves of a surrogate pair together (a lone half is not valid UTF-8). */
    private fun dropLastCodePoint(t: String): String =
        if (t.length >= 2 && Character.isLowSurrogate(t[t.length - 1]) && Character.isHighSurrogate(t[t.length - 2])) t.dropLast(2) else t.dropLast(1)

    /** [name] if free, else "stem (1).ext", "stem (2).ext", ... [taken] answers for existing names. */
    fun unique(name: String, taken: (String) -> Boolean): String {
        if (!taken(name)) return name
        val dot = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
        val stem = name.substring(0, dot)
        val ext = name.substring(dot)
        for (n in 1..999) {
            val c = "$stem ($n)$ext"
            if (!taken(c)) return c
        }
        return "$stem (${System.nanoTime()})$ext"
    }

    private val mimes = mapOf(
        "jpg" to "image/jpeg", "jpeg" to "image/jpeg", "png" to "image/png", "gif" to "image/gif",
        "webp" to "image/webp", "heic" to "image/heic", "pdf" to "application/pdf", "txt" to "text/plain",
        "mp4" to "video/mp4", "mkv" to "video/x-matroska", "mp3" to "audio/mpeg", "m4a" to "audio/mp4",
        "zip" to "application/zip", "json" to "application/json", "apk" to "application/vnd.android.package-archive",
    )

    fun mimeFor(name: String): String =
        mimes[name.substringAfterLast('.', "").lowercase()] ?: "application/octet-stream"
}

/** What an ACTION_SEND / ACTION_SEND_MULTIPLE intent asks for, as plain data. */
sealed interface ShareRequest {
    data class Files(val uris: List<String>, val truncated: Boolean) : ShareRequest
    data class Text(val text: String, val kind: TransferKind) : ShareRequest
    data class Invalid(val reason: String) : ShareRequest
}

object ShareIntentLogic {
    private val BLOCKED_AUTHORITIES = setOf(
        "com.android.contacts", "contacts", "com.android.calendar", "calendar",
        "call_log", "sms", "mms", "mms-sms", "telephony", "com.android.voicemail",
    )

    /**
     * ShareActivity is exported, so any app can hand it any content:// Uri, which it would read with this
     * app's own identity. Refuse this app's own providers and the system providers it holds permissions for.
     */
    fun isBlockedAuthority(authority: String?, ownPackage: String): Boolean {
        val a = authority?.lowercase()?.substringAfterLast('@') ?: return true // "user@authority" form too
        return a.isEmpty() || a == ownPackage.lowercase() || a.startsWith("$ownPackage.".lowercase()) || a in BLOCKED_AUTHORITIES
    }

    fun isWebUrl(s: String): Boolean {
        val t = s.trim()
        val lower = t.lowercase()
        return (lower.startsWith("http://") || lower.startsWith("https://")) &&
            t.none { it.isWhitespace() || Character.isISOControl(it) } && Bounds.utf8Size(t) <= ShareLimits.MAX_URL_BYTES
    }

    /**
     * Streams win over text (a photo shared with a caption sends the photo). Text that is exactly one web
     * URL is a link; anything else is text. Oversized text is refused, not truncated.
     */
    fun classify(streams: List<String>, text: CharSequence?, s: Strings): ShareRequest {
        val uris = streams.filter { it.isNotBlank() }.distinct()
        if (uris.isNotEmpty()) {
            return ShareRequest.Files(uris.take(ShareLimits.MAX_FILES), uris.size > ShareLimits.MAX_FILES)
        }
        val t = text?.toString()?.trim().orEmpty()
        if (t.isEmpty()) return ShareRequest.Invalid(s.get(R.string.share_nothing))
        if (isWebUrl(t)) return ShareRequest.Text(t, TransferKind.Url)
        if (Bounds.utf8Size(t) > ShareLimits.MAX_TEXT_BYTES) return ShareRequest.Invalid(s.get(R.string.share_text_too_long, s.get(R.string.share_size_kib, java.text.NumberFormat.getIntegerInstance().format(ShareLimits.MAX_TEXT_BYTES / 1024))))
        return ShareRequest.Text(t, TransferKind.Text)
    }
}

/** Per-device "auto-accept files" choice. Default is to ask every time. */
interface PolicyStore {
    fun getBoolean(key: String): Boolean
    fun putBoolean(key: String, value: Boolean)
}

class ReceivePolicy(private val store: PolicyStore) {
    private fun key(deviceId: String) = "auto_accept:$deviceId"
    fun autoAccept(deviceId: String): Boolean = store.getBoolean(key(deviceId))
    fun setAutoAccept(deviceId: String, on: Boolean) = store.putBoolean(key(deviceId), on)
}

data class ShareTarget(val deviceId: String, val name: String, val connectionId: Long?, val reachableAt: DiscoveredPeer?)

object TargetPicker {
    /** Trusted connections first; paired devices seen on the network can be connected on demand. */
    fun targets(s: TransportSnapshot?): List<ShareTarget> {
        if (s == null) return emptyList()
        val out = ArrayList<ShareTarget>()
        for (c in s.connections) if (c.trusted) out += ShareTarget(c.peer.deviceId, c.peer.name, c.id, null)
        for (p in s.paired) {
            if (out.any { it.deviceId == p.deviceId }) continue
            val seen = s.discovered.firstOrNull { it.deviceId == p.deviceId }
            out += ShareTarget(p.deviceId, p.name, null, seen)
        }
        return out
    }

    /** The one unambiguous choice, if there is exactly one usable target. */
    fun autoChoice(targets: List<ShareTarget>): ShareTarget? {
        val connected = targets.filter { it.connectionId != null }
        if (connected.size == 1) return connected[0]
        if (connected.isEmpty() && targets.size == 1) return targets[0]
        return null
    }
}

object ProgressText {
    /** Locale-aware size; the unit symbols (B, KiB, MiB, GiB) live in resources so a locale can localise them. */
    fun bytes(n: Long, s: Strings, locale: java.util.Locale = java.util.Locale.getDefault()): String {
        fun num(v: Double, digits: Int): String =
            java.text.NumberFormat.getNumberInstance(locale).apply {
                minimumFractionDigits = digits; maximumFractionDigits = digits
            }.format(v)
        return when {
            n < 1024 -> s.get(R.string.share_size_b, java.text.NumberFormat.getIntegerInstance(locale).format(n))
            n < 1024L * 1024 -> s.get(R.string.share_size_kib, num(n / 1024.0, 1))
            n < 1024L * 1024 * 1024 -> s.get(R.string.share_size_mib, num(n / 1048576.0, 1))
            else -> s.get(R.string.share_size_gib, num(n / 1073741824.0, 2))
        }
    }

    /** 0..100, or null when the total is unknown. */
    fun percent(t: Transfer): Int? {
        val size = t.size ?: return null
        if (size <= 0) return null
        return (t.bytes.coerceAtMost(size) * 100 / size).toInt()
    }

    fun line(t: Transfer, s: Strings): String {
        val what = if (t.kind == TransferKind.File) t.name.ifEmpty { s.get(R.string.share_what_file) }
            else if (t.kind == TransferKind.Url) s.get(R.string.share_what_link) else s.get(R.string.share_what_text)
        val state = when (t.state) {
            TransferState.Offered -> s.get(R.string.share_state_offered)
            TransferState.Waiting -> s.get(R.string.share_state_waiting)
            TransferState.Active -> percent(t)?.let { s.get(R.string.share_state_active, it, bytes(t.bytes, s)) } ?: bytes(t.bytes, s)
            TransferState.Done -> s.get(R.string.share_state_done)
            TransferState.Cancelled -> s.get(R.string.share_state_cancelled)
            TransferState.Declined -> s.get(R.string.share_state_declined)
            TransferState.Failed -> t.code?.let { s.get(R.string.share_state_failed_code, it) } ?: s.get(R.string.share_state_failed)
        }
        return s.get(R.string.share_line, what, state)
    }
}

object ShareText {
    fun error(code: String, s: Strings): String = when (code) {
        "not_running" -> s.get(R.string.err_not_running)
        "unknown_connection", "not_trusted" -> s.get(R.string.share_err_not_connected)
        "unknown_transfer", "wrong_state" -> s.get(R.string.share_err_not_waiting)
        "invalid_fd" -> s.get(R.string.share_err_invalid_fd)
        "too_large" -> s.get(R.string.share_err_too_large)
        "empty" -> s.get(R.string.share_err_empty)
        "busy" -> s.get(R.string.share_err_busy)
        "invalid_field" -> s.get(R.string.share_err_invalid_field)
        else -> s.get(R.string.err_core_refused, code)
    }

    fun failure(code: String?, s: Strings): String = when (code) {
        "cancelled" -> s.get(R.string.share_fail_cancelled)
        "timeout" -> s.get(R.string.share_fail_timeout)
        "too_large" -> s.get(R.string.share_fail_too_large)
        "pin_mismatch" -> s.get(R.string.share_fail_pin_mismatch)
        "tls_failed" -> s.get(R.string.share_fail_tls)
        "truncated" -> s.get(R.string.share_fail_truncated)
        "expired" -> s.get(R.string.share_fail_expired)
        null -> s.get(R.string.share_state_failed)
        else -> s.get(R.string.share_state_failed_code, code)
    }
}

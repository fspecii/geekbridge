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
                peerName = Bounds.clip(o.optString("peer_name", ""), 48),
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
            s = if (dot > 0 && s.length - dot <= 12) s.substring(0, dot - 1) + s.substring(dot) else s.dropLast(1)
            if (s.isEmpty()) return "file"
        }
        return s
    }

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
    fun classify(streams: List<String>, text: CharSequence?): ShareRequest {
        val uris = streams.filter { it.isNotBlank() }.distinct()
        if (uris.isNotEmpty()) {
            return ShareRequest.Files(uris.take(ShareLimits.MAX_FILES), uris.size > ShareLimits.MAX_FILES)
        }
        val t = text?.toString()?.trim().orEmpty()
        if (t.isEmpty()) return ShareRequest.Invalid("There is nothing to send.")
        if (isWebUrl(t)) return ShareRequest.Text(t, TransferKind.Url)
        if (Bounds.utf8Size(t) > ShareLimits.MAX_TEXT_BYTES) return ShareRequest.Invalid("That text is too long to send (limit 48 KiB).")
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
    fun bytes(n: Long): String = when {
        n < 1024 -> "$n B"
        n < 1024L * 1024 -> "%.1f KiB".format(java.util.Locale.ROOT, n / 1024.0)
        n < 1024L * 1024 * 1024 -> "%.1f MiB".format(java.util.Locale.ROOT, n / 1048576.0)
        else -> "%.2f GiB".format(java.util.Locale.ROOT, n / 1073741824.0)
    }

    /** 0..100, or null when the total is unknown. */
    fun percent(t: Transfer): Int? {
        val size = t.size ?: return null
        if (size <= 0) return null
        return (t.bytes.coerceAtMost(size) * 100 / size).toInt()
    }

    fun line(t: Transfer): String {
        val what = if (t.kind == TransferKind.File) t.name.ifEmpty { "file" } else if (t.kind == TransferKind.Url) "link" else "text"
        val state = when (t.state) {
            TransferState.Offered -> "waiting for your answer"
            TransferState.Waiting -> "waiting for the computer"
            TransferState.Active -> percent(t)?.let { "$it% (${bytes(t.bytes)})" } ?: bytes(t.bytes)
            TransferState.Done -> "done"
            TransferState.Cancelled -> "cancelled"
            TransferState.Declined -> "declined"
            TransferState.Failed -> "failed" + (t.code?.let { " ($it)" } ?: "")
        }
        return "$what — $state"
    }
}

object ShareText {
    fun error(code: String): String = when (code) {
        "not_running" -> "The link is not running."
        "unknown_connection", "not_trusted" -> "The computer is not connected and paired."
        "unknown_transfer", "wrong_state" -> "That transfer is no longer waiting."
        "invalid_fd" -> "The file could not be opened for transfer."
        "too_large" -> "The file is over the 8 GiB limit."
        "empty" -> "Empty files cannot be sent."
        "busy" -> "Too many transfers at once."
        "invalid_field" -> "The request was not acceptable."
        else -> "The native core refused the request ($code)."
    }

    fun failure(code: String?): String = when (code) {
        "cancelled" -> "cancelled"
        "timeout" -> "timed out"
        "too_large" -> "too large"
        "pin_mismatch" -> "SECURITY: certificate mismatch"
        "tls_failed" -> "secure channel failed"
        "truncated" -> "connection ended early"
        "expired" -> "offer expired"
        null -> "failed"
        else -> "failed ($code)"
    }
}

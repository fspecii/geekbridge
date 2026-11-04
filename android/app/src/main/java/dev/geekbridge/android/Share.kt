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

/** What the computer says became of a share (`geekbridge.share.result`, docs/kdeconnect-interop.md). */
enum class ShareOutcome { Accepted, Declined, ReceiveOff, TooLarge, Busy, Saved, LinkSaved;
    /** An answer that ends the share without the file moving. */
    val refuses: Boolean get() = this == Declined || this == ReceiveOff || this == TooLarge || this == Busy
}

/** One entry of `transport_status.share_results`. [id] is the transfer id of a file, or the text id of a text or link. */
data class ShareAnswer(
    val id: Long,
    val isText: Boolean,
    val connectionId: Long,
    val outcome: ShareOutcome,
    val reason: String?,
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

    private fun outcome(s: String?): ShareOutcome? = when (s) {
        "accepted" -> ShareOutcome.Accepted
        "declined" -> ShareOutcome.Declined
        "receive_off" -> ShareOutcome.ReceiveOff
        "too_large" -> ShareOutcome.TooLarge
        "busy" -> ShareOutcome.Busy
        "saved" -> ShareOutcome.Saved
        "link_saved" -> ShareOutcome.LinkSaved
        else -> null
    }

    /** Parses `share_results`; an entry that is not exactly understood (or pairs a file with a text-only outcome) is dropped. */
    fun parseAnswers(arr: JSONArray?): List<ShareAnswer> {
        if (arr == null) return emptyList()
        val out = ArrayList<ShareAnswer>()
        for (i in 0 until minOf(arr.length(), ShareLimits.MAX_TRANSFERS)) {
            val o = arr.optJSONObject(i) ?: continue
            val id = nonNegative(o, "id")?.takeIf { it > 0 } ?: continue
            val conn = nonNegative(o, "connection_id") ?: continue
            val isText = when (o.opt("kind") as? String) { "file" -> false; "text" -> true; else -> continue }
            val outcome = outcome(o.opt("outcome") as? String) ?: continue
            val savedOutcome = outcome == ShareOutcome.Saved || outcome == ShareOutcome.LinkSaved
            if (savedOutcome && !isText || outcome == ShareOutcome.Accepted && isText) continue
            out += ShareAnswer(id, isText, conn, outcome, (o.opt("reason") as? String)?.takeIf { Regex("[a-z0-9_]{1,32}").matches(it) })
        }
        return out
    }

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

/** The computer's answer, in plain words. Old computers send none; the screen then keeps its older hints. */
object ShareAnswerText {
    /** How long a text waits for the answer before the screen falls back to "sent" (the computer may be old or busy). */
    const val TEXT_WAIT_MS = 8_000L

    fun phrase(a: ShareAnswer, s: Strings): String = when (a.outcome) {
        ShareOutcome.Accepted -> s.get(R.string.share_answer_accepted)
        ShareOutcome.Declined -> s.get(if (a.reason == "expired") R.string.share_answer_expired else R.string.share_answer_declined)
        ShareOutcome.ReceiveOff -> s.get(if (a.isText) R.string.share_answer_receive_off_text else R.string.share_answer_receive_off)
        ShareOutcome.TooLarge -> s.get(R.string.share_answer_too_large)
        ShareOutcome.Busy -> s.get(R.string.share_answer_busy)
        ShareOutcome.Saved -> s.get(R.string.share_answer_saved)
        ShareOutcome.LinkSaved -> s.get(R.string.share_answer_link_saved)
    }

    fun tone(a: ShareAnswer): Tone = when (a.outcome) {
        ShareOutcome.Saved, ShareOutcome.LinkSaved -> Tone.Ok
        ShareOutcome.Accepted -> Tone.Normal
        else -> Tone.Warn
    }

    /**
     * The line for one item of the share session, or null when the older wording applies. [transfer] is the file's
     * entry in the snapshot; [answer] the computer's answer if one arrived; [waitedMs] how long a text has waited.
     */
    fun line(label: String, isText: Boolean, transfer: Transfer?, answer: ShareAnswer?, handedOver: Boolean, resultExpected: Boolean, waitedMs: Long, s: Strings): Pair<String, Tone>? {
        if (answer != null) {
            // Once bytes move the file's own progress is the better news; only a refusal or a bare "accepted" speaks here.
            val moving = transfer != null && (transfer.state == TransferState.Active || transfer.state == TransferState.Done)
            if (!isText && moving) return null
            if (!isText && transfer != null && answer.outcome == ShareOutcome.Accepted && transfer.state.finished) return null
            return s.get(R.string.share_line, label, phrase(answer, s)) to tone(answer)
        }
        if (isText && handedOver && resultExpected && waitedMs <= TEXT_WAIT_MS) {
            return s.get(R.string.share_line, label, s.get(R.string.share_answer_waiting)) to Tone.Muted
        }
        return null
    }

    /** Keep the session (and the link) open a moment for a text whose answer is still on its way. */
    fun textStillWaiting(resultExpected: Boolean, answered: Boolean, waitedMs: Long): Boolean =
        resultExpected && !answered && waitedMs <= TEXT_WAIT_MS
}

object ShareSessionLogic {
    /** How long a started transfer may stay missing from the snapshot before it counts as gone. */
    const val UNSEEN_GRACE_MS = 15_000L

    /** Does this item of a share session still need the session (and the link) to stay open? */
    fun itemOpen(transferId: Long?, inSnapshot: Transfer?, seen: Boolean, ageMs: Long): Boolean = when {
        transferId == null -> false
        inSnapshot != null -> !inSnapshot.state.finished
        else -> !seen && ageMs <= UNSEEN_GRACE_MS // not listed yet (the answer comes before the next poll), or pruned once finished
    }
}

object NameRules {
    private const val MAX_BYTES = 120

    /** One safe path component: no separators, control characters or leading dots; bounded; never empty. */
    fun sanitize(raw: String?): String {
        var s = (raw ?: "").substringAfterLast('/').substringAfterLast('\\')
        s = s.map { c -> if (Character.isISOControl(c) || isBidiControl(c) || c in "<>:\"|?*") '_' else c }.joinToString("")
        s = s.trim().trimEnd('.', ' ').trimStart('.')
        if (s.isEmpty()) s = "file"
        while (Bounds.utf8Size(s) > MAX_BYTES) {
            val dot = s.lastIndexOf('.')
            s = if (dot > 0 && s.length - dot <= 12) dropLastCodePoint(s.substring(0, dot)) + s.substring(dot) else dropLastCodePoint(s)
            if (s.isEmpty()) return "file"
        }
        return s
    }

    /**
     * Bidirectional controls and line separators: "photo<RLO>gpj.exe" reads as "photoexe.jpg" in a file manager, on the
     * Share screen and in a notification. The same set the computer's core replaces (bridge-kde `sanitize_filename`).
     */
    fun isBidiControl(c: Char): Boolean =
        c == '\u061C' || c == '\u200E' || c == '\u200F' || c == '\u2028' || c == '\u2029' ||
            c in '\u202A'..'\u202E' || c in '\u2066'..'\u2069'

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
            TransferState.Failed -> t.code?.let { s.get(R.string.share_state_failed_code, if (ShareText.knownFailure(it)) ShareText.failure(it, s) else it) } ?: s.get(R.string.share_state_failed)
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

    private val known = setOf("cancelled", "timeout", "too_large", "pin_mismatch", "tls_failed", "truncated", "expired", "link_lost", "io_error", "port_rejected", "no_port", "busy")

    /** A code that has wording of its own (shown in words on the Share screen instead of the raw code). */
    fun knownFailure(code: String): Boolean = code in known

    fun failure(code: String?, s: Strings): String = when (code) {
        "cancelled" -> s.get(R.string.share_fail_cancelled)
        "timeout" -> s.get(R.string.share_fail_timeout)
        "too_large" -> s.get(R.string.share_fail_too_large)
        "pin_mismatch" -> s.get(R.string.share_fail_pin_mismatch)
        "tls_failed" -> s.get(R.string.share_fail_tls)
        "truncated" -> s.get(R.string.share_fail_truncated)
        "expired" -> s.get(R.string.share_fail_expired)
        "link_lost" -> s.get(R.string.share_fail_link_lost)
        "io_error" -> s.get(R.string.share_fail_io)
        "port_rejected", "no_port" -> s.get(R.string.share_fail_no_port)
        "busy" -> s.get(R.string.share_fail_busy)
        null -> s.get(R.string.share_state_failed)
        else -> s.get(R.string.share_state_failed_code, code)
    }
}

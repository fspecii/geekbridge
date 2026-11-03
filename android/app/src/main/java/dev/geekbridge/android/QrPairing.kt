// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject

data class QrAddress(val host: String, val port: Int)

/**
 * What a scanned pairing code pinned: the computer, the SHA-256 of its certificate and the one-time token.
 * The token is a secret: it is never logged, shown or saved, and is sent only after the core has checked
 * that the certificate on the live connection is [certSha256].
 */
/** What the confirmation of a link from outside shows. The name is chosen by whoever made the link, so it is marked as such. */
object QrConfirmText {
    /** The first 16 hex digits of the certificate fingerprint, in groups of four (what the computer's QR screen can show too). */
    fun fingerprintPrefix(certSha256: String): String =
        certSha256.filter { it.isLetterOrDigit() }.uppercase().take(16).chunked(4).joinToString(" ")

    /** The advertised name with control, direction-override and zero-width characters removed, bounded, never empty. */
    fun safeName(raw: String): String {
        val s = raw.filter { c ->
            !Character.isISOControl(c) && Character.getType(c) != Character.FORMAT.toInt() && c != '\u2028' && c != '\u2029'
        }.trim()
        return Bounds.clip(s, 40)
    }

    /** Where the link would make this phone connect: host:port of the first address. */
    fun address(t: QrTarget): String = t.addresses.first().let { "${it.host}:${it.port}" }
}

data class QrTarget(
    val deviceId: String,
    val name: String,
    val certSha256: String,
    val token: String,
    val addresses: List<QrAddress>,
) {
    override fun toString() = "QrTarget(deviceId=$deviceId, name=$name)" // never prints the token
}

object QrProtocol {
    const val MAX_TEXT_CHARS = 700
    private const val PREFIX = "geekbridge://pair?"

    /** Text that looks like a pairing link, trimmed; null for anything else (so it is never sent to the core). */
    fun linkText(raw: CharSequence?): String? {
        val t = raw?.toString()?.trim() ?: return null
        if (t.length > MAX_TEXT_CHARS || !t.startsWith(PREFIX)) return null
        return t
    }

    fun parseRequest(text: String): String = JSONObject()
        .put("version", CoreProtocol.VERSION)
        .put("action", "pairing_parse_qr")
        .put("text", text)
        .toString()

    fun parseTarget(result: Any?): QrTarget? {
        val o = result as? JSONObject ?: return null
        val id = o.optString("device_id")
        val cert = o.optString("cert_sha256")
        val token = o.optString("token")
        if (id.length !in 32..38 || cert.length != 64 || token.length != 32) return null
        val arr = o.optJSONArray("addresses") ?: return null
        val addresses = (0 until minOf(arr.length(), 8)).mapNotNull { i ->
            val a = arr.optJSONObject(i) ?: return@mapNotNull null
            val host = a.optString("host")
            val port = a.optInt("port", 0)
            if (host.isEmpty() || host.length > 64 || port !in 1..65535) null else QrAddress(host, port)
        }
        if (addresses.isEmpty()) return null
        return QrTarget(id, Bounds.clip(o.optString("name"), 48), cert, token, addresses)
    }

    /** What to tell the user when the core rejects the text. */
    fun parseError(s: Strings, code: String): String = when (code) {
        "qr_not_geekbridge" -> s.get(R.string.qr_err_not_geekbridge)
        "qr_version" -> s.get(R.string.qr_err_version)
        "qr_malformed" -> s.get(R.string.qr_err_malformed)
        else -> s.get(R.string.qr_err_unreadable)
    }

    fun pairRequest(connectionId: Long, token: String, certSha256: String): String = JSONObject()
        .put("version", CoreProtocol.VERSION)
        .put("action", "transport_pair_request")
        .put("connection_id", connectionId)
        .put("qr_token", token)
        .put("qr_cert_sha256", certSha256)
        .toString()
}

sealed interface QrStep {
    data class Dial(val address: QrAddress) : QrStep
    data class Pair(val connectionId: Long) : QrStep
    data object Waiting : QrStep
    /** Scanning means "pair again": a computer this phone still remembers is forgotten first (it may have forgotten the phone). */
    data class Forget(val deviceId: String) : QrStep
    data object Paired : QrStep
    data class Failed(val why: QrFailure) : QrStep
}

private const val FORGET_WAIT_MS = 5_000L

enum class QrFailure { NoAddressAnswered, ConnectionLost }

/**
 * Drives one scan from a status snapshot: dial each address in turn (each gets [dialTimeoutMs]), send the
 * pairing request once the secure channel is parked, then wait for the phone-side approval and the trust record.
 * Pure, so the order and the time-outs are unit-tested.
 */
class QrPlan(val target: QrTarget, private val dialTimeoutMs: Long = 6_000) {
    private var index = -1
    private var dialStartedAt = 0L
    private var pairSent = false
    private var forgetAt = -1L

    fun next(s: TransportSnapshot?, nowMs: Long): QrStep {
        if (s == null) return QrStep.Waiting
        val known = s.paired.firstOrNull { it.deviceId == target.deviceId }
        if (known != null) {
            // Trust recorded by this scan.
            if (pairSent) return QrStep.Paired
            // Not by this scan: the computer may have forgotten this phone (then "Paired" would be a lie and the
            // computer would never see the request), so forget it here and pair for real. Never wait for ever.
            if (forgetAt < 0) { forgetAt = nowMs; return QrStep.Forget(known.deviceId) }
            if (nowMs - forgetAt < FORGET_WAIT_MS) return QrStep.Waiting
        }
        val c = s.connections.firstOrNull { it.peer.deviceId == target.deviceId }
        if (c != null) {
            return when {
                c.trusted -> QrStep.Paired
                c.phase == "parked" && !pairSent -> { pairSent = true; QrStep.Pair(c.id) }
                else -> QrStep.Waiting
            }
        }
        // The connection was there and is gone before trust was recorded: refused, closed or timed out.
        if (pairSent) return QrStep.Failed(QrFailure.ConnectionLost)
        if (index >= 0 && nowMs - dialStartedAt < dialTimeoutMs) return QrStep.Waiting
        index++
        if (index >= target.addresses.size) return QrStep.Failed(QrFailure.NoAddressAnswered)
        dialStartedAt = nowMs
        return QrStep.Dial(target.addresses[index])
    }
}

/** Whether a recreated QrPairActivity reads its launch link again. */
object QrReplay {
    /** A fresh start always does; a recreation only if the confirmation of the link was still waiting. */
    fun shouldHandle(hasSavedState: Boolean, wasWaiting: Boolean): Boolean = !hasSavedState || wasWaiting
}

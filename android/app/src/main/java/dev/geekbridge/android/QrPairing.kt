package dev.geekbridge.android

import org.json.JSONObject

data class QrAddress(val host: String, val port: Int)

/**
 * What a scanned pairing code pinned: the computer, the SHA-256 of its certificate and the one-time token.
 * The token is a secret: it is never logged, shown or saved, and is sent only after the core has checked
 * that the certificate on the live connection is [certSha256].
 */
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
    fun parseError(code: String): String = when (code) {
        "qr_not_geekbridge" -> "That is not a GeekBridge pairing code."
        "qr_version" -> "That pairing code is from a different GeekBridge version. Update both apps."
        "qr_malformed" -> "That pairing code is damaged. Show a new one on the computer and try again."
        else -> "The pairing code could not be read."
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
    data object Paired : QrStep
    data class Failed(val why: QrFailure) : QrStep
}

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

    fun next(s: TransportSnapshot?, nowMs: Long): QrStep {
        if (s == null) return QrStep.Waiting
        if (s.paired.any { it.deviceId == target.deviceId }) return QrStep.Paired
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

package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/*
 * Kotlin side of the `transport_*` actions documented in docs/android-transport.md. Everything here is
 * pure (no Android types) so it runs in JVM tests. Nothing in this file implies a peer is trusted:
 * that is only ever read from `connections[].trusted` / `paired[]` in a native status snapshot.
 */

data class LocalInfo(val deviceId: String, val deviceName: String, val certSha256: String)
data class ListenInfo(val host: String, val port: Int)
data class DiscoveryInfo(val listening: Boolean, val port: Int?, val announcing: Boolean, val error: String?)

data class DiscoveredPeer(
    val deviceId: String,
    val name: String,
    val deviceType: String,
    val ip: String,
    val tcpPort: Int,
    val paired: Boolean,
)

data class PeerInfo(val deviceId: String, val name: String, val deviceType: String, val certSha256: String, val ip: String)

data class LinkConnection(
    val id: Long,
    val role: String,
    val phase: String,
    val trusted: Boolean,
    val peer: PeerInfo,
    val pingsReceived: Long,
)

/** A pending local decision. [code] and [peerCertSha256] are exactly what is shown and later sent back. */
data class PairPrompt(
    val requestId: Long,
    val connectionId: Long,
    val direction: String,
    val peerDeviceId: String,
    val peerName: String,
    val peerCertSha256: String,
    val ownCertSha256: String,
    val code: String,
    val expiresInMs: Long,
)

data class PairedPeer(val deviceId: String, val name: String, val deviceType: String, val certSha256: String)

data class LinkEvent(val seq: Long, val kind: String, val connectionId: Long?, val deviceId: String?, val code: String?)

data class TransportSnapshot(
    val state: String,
    val local: LocalInfo?,
    val listen: ListenInfo?,
    val discovery: DiscoveryInfo?,
    val discovered: List<DiscoveredPeer>,
    val connections: List<LinkConnection>,
    val prompts: List<PairPrompt>,
    /** Prompts the core listed but that cannot be shown exactly (malformed code/fingerprint); never approvable. */
    val undisplayablePrompts: Int,
    val paired: List<PairedPeer>,
    val pairedError: Boolean,
    val events: List<LinkEvent>,
    val lastEventSeq: Long,
    /** Share transfers (docs/android-contract.md); empty when the core reports none. */
    val transfers: List<Transfer> = emptyList(),
) {
    val isRunning: Boolean get() = state == "running"
}

data class StartOptions(
    val stateDir: String,
    val deviceName: String,
    val listen: Boolean,
    val listenPort: Int?,
    val discoveryListen: Boolean,
    val discoveryAnnounce: Boolean,
    val allowLoopback: Boolean,
    /** Negotiate the clipboard packet types (see ClipboardRuntime); null leaves clipboard out entirely. */
    val clipboard: ClipSettings? = null,
    /** Negotiate the call-control packet types (see CallControlRuntime); null leaves them out entirely. */
    val calls: CallLinkSettings? = null,
)

object TransportProtocol {
    const val MAX_LIST = 64
    private val token = Regex("[A-Za-z0-9_:.-]{1,128}")
    private val kindPattern = Regex("[a-z0-9_]{1,48}")

    private fun base(action: String) = JSONObject().put("version", CoreProtocol.VERSION).put("action", action)

    fun startRequest(o: StartOptions): String {
        val r = base("transport_start")
            .put("state_dir", o.stateDir)
            .put("device_name", o.deviceName)
        if (!o.listen) {
            r.put("listen", JSONObject().put("enabled", false))
        } else if (o.listenPort != null) {
            r.put("listen", JSONObject().put("port", o.listenPort))
        }
        if (o.discoveryListen || o.discoveryAnnounce) {
            r.put("discovery", JSONObject().put("listen", o.discoveryListen).put("announce", o.discoveryAnnounce))
        }
        if (o.allowLoopback) r.put("allow_loopback", true)
        if (o.clipboard != null) r.put("clipboard", ClipboardProtocol.startObject(o.clipboard))
        if (o.calls != null) r.put("calls", CallsProtocol.startObject(o.calls))
        return r.toString()
    }

    fun statusRequest(since: Long): String = base("transport_status").also { if (since > 0) it.put("since", since) }.toString()
    fun stopRequest(): String = base("transport_stop").toString()

    fun connectRequest(host: String, port: Int, deviceId: String): String =
        base("transport_connect").put("host", host).put("port", port).put("device_id", deviceId).toString()

    fun pairRequest(connectionId: Long, await: Boolean): String =
        base("transport_pair_request").put("connection_id", connectionId)
            .also { if (await) it.put("mode", "await") }.toString()

    /**
     * Built from the prompt *as it was displayed*, never from a fresh read, so a prompt that changed
     * underneath the dialog is refused by the core instead of approved. Declining sends only the ids.
     */
    fun decisionRequest(p: PairPrompt, approve: Boolean): String {
        val r = base("transport_pair_decision")
            .put("request_id", p.requestId)
            .put("connection_id", p.connectionId)
            .put("approve", approve)
        if (approve) r.put("verification_code", p.code).put("peer_cert_sha256", p.peerCertSha256)
        return r.toString()
    }

    fun unpairRequest(deviceId: String): String = base("transport_unpair").put("device_id", deviceId).toString()
    fun disconnectRequest(connectionId: Long): String = base("transport_disconnect").put("connection_id", connectionId).toString()
    fun pingRequest(connectionId: Long): String = base("transport_ping").put("connection_id", connectionId).toString()

    // ------------------------------------------------------------------ parsing

    private fun JSONObject.text(key: String, max: Int): String = Bounds.clip(optString(key, ""), max)

    /** Identifier-like value that is safe to send back verbatim; null if it is not. */
    private fun JSONObject.id(key: String): String? =
        (opt(key) as? String)?.takeIf { token.matches(it) }

    private fun JSONObject.long(key: String): Long? = (opt(key) as? Number)?.toLong()

    private fun JSONArray?.objects(): List<JSONObject> {
        if (this == null) return emptyList()
        val out = ArrayList<JSONObject>(minOf(length(), MAX_LIST))
        for (i in 0 until minOf(length(), MAX_LIST)) optJSONObject(i)?.let { out += it }
        return out
    }

    /** Parses a `transport_status` result. Returns null if it is not a recognisable snapshot. */
    fun parseSnapshot(result: Any?): TransportSnapshot? {
        val o = result as? JSONObject ?: return null
        val state = (o.opt("state") as? String)?.takeIf { kindPattern.matches(it) } ?: return null
        if (state != "running") {
            return TransportSnapshot(state, null, null, null, emptyList(), emptyList(), emptyList(), 0, emptyList(), false, emptyList(), 0)
        }
        val local = o.optJSONObject("local")?.let {
            val id = it.id("device_id") ?: return@let null
            val cert = it.id("cert_sha256") ?: return@let null
            LocalInfo(id, it.text("device_name", 64), cert)
        } ?: return null

        val listen = o.optJSONObject("listen")?.let {
            val port = it.long("port")?.toInt() ?: return@let null
            ListenInfo(it.text("host", 64), port)
        }
        val discovery = o.optJSONObject("discovery")?.let {
            DiscoveryInfo(
                listening = it.optBoolean("listening", false),
                port = it.long("port")?.toInt(),
                announcing = it.optBoolean("announcing", false),
                error = (it.opt("error") as? String)?.let { e -> Bounds.clip(e, 48) },
            )
        }
        val discovered = o.optJSONArray("discovered").objects().mapNotNull {
            DiscoveredPeer(
                deviceId = it.id("device_id") ?: return@mapNotNull null,
                name = it.text("name", 48),
                deviceType = it.text("device_type", 16),
                ip = it.text("ip", 64),
                tcpPort = it.long("tcp_port")?.toInt() ?: return@mapNotNull null,
                paired = it.optBoolean("paired", false),
            )
        }
        val connections = o.optJSONArray("connections").objects().mapNotNull { c ->
            val peer = c.optJSONObject("peer") ?: return@mapNotNull null
            LinkConnection(
                id = c.long("connection_id") ?: return@mapNotNull null,
                role = c.text("role", 16),
                phase = c.text("phase", 24),
                trusted = c.optBoolean("trusted", false),
                peer = PeerInfo(
                    deviceId = peer.id("device_id") ?: return@mapNotNull null,
                    name = peer.text("name", 48),
                    deviceType = peer.text("device_type", 16),
                    certSha256 = peer.id("cert_sha256") ?: "",
                    ip = peer.text("ip", 64),
                ),
                pingsReceived = c.long("pings_received") ?: 0,
            )
        }
        var undisplayable = 0
        val prompts = ArrayList<PairPrompt>()
        for (p in o.optJSONArray("prompts").objects()) {
            val parsed = run {
                PairPrompt(
                    requestId = p.long("request_id") ?: return@run null,
                    connectionId = p.long("connection_id") ?: return@run null,
                    direction = p.text("direction", 16),
                    peerDeviceId = p.id("peer_device_id") ?: return@run null,
                    peerName = p.text("peer_name", 48),
                    peerCertSha256 = p.id("peer_cert_sha256") ?: return@run null,
                    ownCertSha256 = p.id("own_cert_sha256") ?: "",
                    code = p.id("verification_code") ?: return@run null,
                    expiresInMs = p.long("expires_in_ms") ?: 0,
                )
            }
            if (parsed != null) prompts += parsed else undisplayable++
        }
        val paired = o.optJSONArray("paired").objects().mapNotNull {
            PairedPeer(
                deviceId = it.id("device_id") ?: return@mapNotNull null,
                name = it.text("name", 48),
                deviceType = it.text("device_type", 16),
                certSha256 = it.id("cert_sha256") ?: "",
            )
        }
        val events = o.optJSONArray("events").objects().mapNotNull {
            LinkEvent(
                seq = it.long("seq") ?: return@mapNotNull null,
                kind = (it.opt("kind") as? String)?.takeIf { k -> kindPattern.matches(k) } ?: "unknown",
                connectionId = it.long("connection_id"),
                deviceId = it.id("device_id"),
                code = (it.opt("code") as? String)?.takeIf { c -> kindPattern.matches(c) },
            )
        }
        return TransportSnapshot(
            state = state,
            local = local,
            listen = listen,
            discovery = discovery,
            discovered = discovered,
            connections = connections,
            prompts = prompts,
            undisplayablePrompts = undisplayable,
            paired = paired,
            pairedError = o.optBoolean("paired_error", false),
            events = events,
            lastEventSeq = o.long("last_event_seq") ?: 0,
            transfers = ShareProtocol.parseTransfers(o.optJSONArray("transfers")),
        )
    }
}

/** User-facing wording. Unknown codes and kinds fall back to generic text and never echo native strings. */
object TransportText {
    /** Is this code a security-relevant state rather than an ordinary failure? */
    fun isSecurity(code: String?): Boolean = code == "pin_mismatch" || code == "identity_mismatch" || code == "peer_requested_repair"

    fun error(code: String): String = when (code) {
        "not_running" -> "The link is not running."
        "already_running" -> "The link is already running."
        "busy" -> "The link is busy starting or stopping. Try again in a moment."
        "invalid_state_dir", "insecure_state", "identity_unusable", "state_corrupt", "state_unavailable" ->
            "Could not open this app's private identity storage ($code)."
        "invalid_device_name" -> "This phone's name is not usable as a device name."
        "invalid_endpoint" -> "Enter an IP address (not a host name) and a port from 1 to 65535."
        "invalid_device_id" -> "A device id is 32–38 letters, digits, '-' or '_'."
        "loopback_not_allowed" -> "Loopback is only allowed in debug builds with the testing option on."
        "address_rejected" -> "That address is not allowed. Only private local-network addresses can be used."
        "already_connected" -> "A connection or attempt to that device already exists."
        "too_many_connections" -> "Too many connections. Disconnect one first."
        "unknown_connection" -> "That connection no longer exists."
        "wrong_state" -> "That connection is not waiting for a pairing choice."
        "not_trusted" -> "Only a paired (trusted) connection can do that."
        "unknown_request", "request_expired" -> "That pairing request is gone (expired or replaced). Nothing was approved."
        "verification_mismatch", "certificate_mismatch" -> "The code or certificate no longer matches the request. Nothing was approved."
        "too_many_attempts" -> "Pairing request rejected after too many mismatches."
        "listen_failed" -> "Could not listen on that port. Another app may be using it."
        "discovery_requires_listener" -> "Announcing needs listening for connections to be on."
        "unsupported_platform", "runtime_failed" -> "The native transport could not run here ($code)."
        "store_error" -> "The trust record could not be updated."
        else -> "The native core refused the request ($code)."
    }

    fun event(e: LinkEvent): String = when (e.kind) {
        "pairing_prompt" -> "A pairing request is waiting for your decision."
        "paired" -> "Paired: the other device’s exact certificate is now trusted."
        "pairing_failed" -> "Pairing did not complete: " + when (e.code) {
            "pairing_rejected_locally" -> "declined on this phone."
            "pairing_rejected_by_peer" -> "the other device declined."
            "pairing_timeout" -> "timed out."
            "clock_skew" -> "the two clocks differ too much."
            "pairing_binding_changed" -> "the request changed while open."
            "link_closed" -> "the connection closed."
            "tls_failed" -> "the secure channel failed."
            null -> "no reason given."
            else -> "${e.code}."
        }
        "connect_failed" -> when (e.code) {
            "pin_mismatch" -> "SECURITY: a paired device presented a different certificate. The connection was refused."
            "identity_mismatch" -> "SECURITY: the device at that address is not the device id you chose. The connection was refused."
            "timeout" -> "Connection timed out."
            "tls_failed" -> "Secure channel could not be set up."
            null -> "Connection failed."
            else -> "Connection failed (${e.code})."
        }
        "link_closed" -> "A connection closed" + (e.code?.let { " ($it)" } ?: "") + "."
        "peer_unpaired" -> "The other device removed the pairing; it is gone from this phone too."
        "peer_requested_repair" ->
            "SECURITY: the other device forgot this phone and asks to pair again. Unpair here first, then pair again only if you expect it."
        "unpaired" -> "Unpaired on this phone."
        else -> "Event (${e.kind})."
    }

    fun eventTone(e: LinkEvent): Tone = when {
        isSecurity(e.code) || e.kind == "peer_requested_repair" -> Tone.Bad
        e.kind == "paired" -> Tone.Ok
        e.kind.endsWith("_failed") -> Tone.Warn
        else -> Tone.Muted
    }

    /** Groups a hex fingerprint in fours for reading aloud / comparing; leaves anything else untouched. */
    fun group(hex: String): String =
        if (hex.length in 8..128 && hex.all { it.isLetterOrDigit() }) hex.chunked(4).joinToString(" ") else hex
}

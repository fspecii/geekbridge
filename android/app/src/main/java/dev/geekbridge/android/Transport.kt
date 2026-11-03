// SPDX-License-Identifier: GPL-3.0-or-later
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
    /** The computer's certificate was pinned from a scanned QR code, so there is no code to compare. */
    val qrPinned: Boolean = false,
)

data class PairedPeer(val deviceId: String, val name: String, val deviceType: String, val certSha256: String)

data class LinkEvent(val seq: Long, val kind: String, val connectionId: Long?, val deviceId: String?, val code: String?, val quietMs: Long? = null)

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
    /** Contacts / calendar / SMS capabilities that are enabled AND permitted (PhoneDataRuntime); null or all-off declares none. */
    val data: ActiveFeatures? = null,
    /** Which paired computer may use which opt-in; the default is what the app has stored (nobody before it loads). */
    val permits: ComputerGrants = GrantsHolder.current,
)

object TransportProtocol {
    const val MAX_LIST = 64
    /** Matches the core's bound. The wait blocks without a timer in between, so only each expiry is a wakeup. */
    const val MAX_WAIT_MS = 600_000L
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
        o.data?.let { PhoneDataProtocol.startObject(it) }?.let { r.put("data", it) }
        // Always sent: a computer has no calls / contacts / calendar / SMS rights until the user allowed it.
        r.put("permits", o.permits.permitsObject())
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

    fun setNameRequest(name: String): String = base("transport_set_name").put("device_name", name).toString()

    fun unpairRequest(deviceId: String): String = base("transport_unpair").put("device_id", deviceId).toString()

    /** Blocks in the core until an event newer than [since] exists or the transport stops. Off the main thread only. */
    fun waitRequest(since: Long, timeoutMs: Long = MAX_WAIT_MS): String =
        base("transport_wait").put("since", since).put("timeout_ms", timeoutMs.coerceIn(0, MAX_WAIT_MS)).toString()

    /** Newest event sequence, or null if the result is not recognisable or the transport stopped. */
    fun parseWait(result: Any?): Long? {
        val o = result as? JSONObject ?: return null
        if (o.optBoolean("stopped", true) || !o.has("last_event_seq")) return null
        return o.optLong("last_event_seq", -1).takeIf { it >= 0 }
    }
    fun disconnectRequest(connectionId: Long): String = base("transport_disconnect").put("connection_id", connectionId).toString()
    /** Tells the core whether the screen is on: with it off the heartbeat is sent at the slower idle cadence. */
    fun screenRequest(on: Boolean): String = base("transport_screen").put("on", on).toString()
    /** Sends the heartbeat ping on every link now: Doze opened a maintenance window or ended. */
    fun heartbeatNowRequest(): String = base("transport_heartbeat_now").toString()
    /** The phone's Wi-Fi IPv4 address for dials and announcements; no [ip] clears it (no Wi-Fi, or ambiguous). */
    fun wifiBindRequest(ip: String?): String = base("transport_wifi_bind").also { if (ip != null) it.put("ip", ip) }.toString()
    fun pingRequest(connectionId: Long): String = base("transport_ping").put("connection_id", connectionId).toString()

    // ------------------------------------------------------------------ parsing

    private fun JSONObject.text(key: String, max: Int): String = Bounds.clip(optString(key, ""), max)

    private fun JSONObject.deviceText(key: String, max: Int): String = Bounds.name(optString(key, ""), max)

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
            LocalInfo(id, it.deviceText("device_name", 64), cert)
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
                name = it.deviceText("name", 48),
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
                    name = peer.deviceText("name", 48),
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
                    peerName = p.deviceText("peer_name", 48),
                    peerCertSha256 = p.id("peer_cert_sha256") ?: return@run null,
                    ownCertSha256 = p.id("own_cert_sha256") ?: "",
                    code = p.id("verification_code") ?: return@run null,
                    expiresInMs = p.long("expires_in_ms") ?: 0,
                    qrPinned = p.optBoolean("qr_pinned", false),
                )
            }
            if (parsed != null) prompts += parsed else undisplayable++
        }
        val paired = o.optJSONArray("paired").objects().mapNotNull {
            PairedPeer(
                deviceId = it.id("device_id") ?: return@mapNotNull null,
                name = it.deviceText("name", 48),
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
                quietMs = it.long("quiet_ms")?.takeIf { q -> q >= 0 },
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

    fun error(s: Strings, code: String): String = when (code) {
        "not_running" -> s.get(R.string.err_not_running)
        "already_running" -> s.get(R.string.err_already_running)
        "busy" -> s.get(R.string.err_busy)
        "invalid_state_dir", "insecure_state", "identity_unusable", "state_corrupt", "state_unavailable" ->
            s.get(R.string.err_identity_storage, code)
        "invalid_device_name" -> s.get(R.string.err_invalid_device_name)
        "invalid_endpoint" -> s.get(R.string.err_invalid_endpoint)
        "invalid_device_id" -> s.get(R.string.err_invalid_device_id)
        "loopback_not_allowed" -> s.get(R.string.err_loopback_not_allowed)
        "address_rejected" -> s.get(R.string.err_address_rejected)
        "already_connected" -> s.get(R.string.err_already_connected)
        "too_many_connections" -> s.get(R.string.err_too_many_connections)
        "unknown_connection" -> s.get(R.string.err_unknown_connection)
        "wrong_state" -> s.get(R.string.err_wrong_state)
        "not_trusted" -> s.get(R.string.err_not_trusted)
        "unknown_request", "request_expired" -> s.get(R.string.err_request_gone)
        "verification_mismatch", "certificate_mismatch" -> s.get(R.string.err_verification_mismatch)
        "too_many_attempts" -> s.get(R.string.err_too_many_attempts)
        "listen_failed" -> s.get(R.string.err_listen_failed)
        "discovery_requires_listener" -> s.get(R.string.err_discovery_requires_listener)
        "unsupported_platform", "runtime_failed" -> s.get(R.string.err_native_transport, code)
        "store_error" -> s.get(R.string.err_store_error)
        else -> s.get(R.string.err_core_refused, code)
    }

    fun event(s: Strings, e: LinkEvent): String = when (e.kind) {
        "pairing_prompt" -> s.get(R.string.event_pairing_prompt)
        "paired" -> s.get(R.string.event_paired)
        "pairing_failed" -> when (e.code) {
            "pairing_rejected_locally" -> s.get(R.string.event_pair_failed_local)
            "pairing_rejected_by_peer" -> s.get(R.string.event_pair_failed_peer)
            "pairing_timeout" -> s.get(R.string.event_pair_failed_timeout)
            "clock_skew" -> s.get(R.string.event_pair_failed_clock)
            "pairing_binding_changed" -> s.get(R.string.event_pair_failed_changed)
            "link_closed" -> s.get(R.string.event_pair_failed_closed)
            "tls_failed" -> s.get(R.string.event_pair_failed_tls)
            null -> s.get(R.string.event_pair_failed_none)
            else -> s.get(R.string.event_pair_failed_code, e.code)
        }
        "connect_failed" -> when (e.code) {
            "pin_mismatch" -> s.get(R.string.event_security_pin)
            "identity_mismatch" -> s.get(R.string.event_security_identity)
            "timeout" -> s.get(R.string.event_connect_timeout)
            "tls_failed" -> s.get(R.string.event_connect_tls)
            null -> s.get(R.string.event_connect_failed)
            else -> s.get(R.string.event_connect_failed_code, e.code)
        }
        "link_closed" -> e.code?.let { s.get(R.string.event_link_closed_code, it) } ?: s.get(R.string.event_link_closed)
        "peer_unpaired" -> s.get(R.string.event_peer_unpaired)
        "peer_requested_repair" -> s.get(R.string.event_peer_requested_repair)
        "unpaired" -> s.get(R.string.event_unpaired)
        else -> s.get(R.string.event_unknown, e.kind)
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

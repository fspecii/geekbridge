package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val DEV = "_peer_device_id_0123456789abcdef_"

private fun transfer(
    id: Long = 5, direction: String = "incoming", kind: String = "file", state: String = "offered",
    name: String = "report.pdf", size: Any? = 1000, bytes: Any = 250,
) = JSONObject().put("transfer_id", id).put("connection_id", 2).put("device_id", DEV).put("peer_name", "Desk")
    .put("direction", direction).put("kind", kind).put("name", name).put("size", size).put("bytes", bytes).put("state", state)

class ShareProtocolTest {
    @Test fun requestShapes() {
        val f = JSONObject(ShareProtocol.sendFile(2, 41, "a/b/pic.jpg", 1234, 1_700_000_000_000))
        assertEquals("share_send_file", f.getString("action")); assertEquals(1, f.getInt("version"))
        assertEquals(2, f.getLong("connection_id")); assertEquals(41, f.getInt("fd"))
        assertEquals("pic.jpg", f.getString("name")); assertEquals(1234, f.getLong("size"))
        assertEquals(1_700_000_000_000, f.getLong("last_modified_ms"))
        assertFalse(JSONObject(ShareProtocol.sendFile(2, 3, "x", 1, null)).has("last_modified_ms"))

        val t = JSONObject(ShareProtocol.sendText(2, TransferKind.Url, "https://a.b"))
        assertEquals("share_send_text", t.getString("action")); assertEquals("url", t.getString("kind"))
        assertEquals("text", JSONObject(ShareProtocol.sendText(2, TransferKind.Text, "hi")).getString("kind"))

        val a = JSONObject(ShareProtocol.accept(9, 12))
        assertEquals("share_accept", a.getString("action")); assertEquals(9, a.getLong("transfer_id")); assertEquals(12, a.getInt("fd"))
        for ((req, action) in listOf(
            ShareProtocol.decline(9) to "share_decline", ShareProtocol.cancel(9) to "share_cancel", ShareProtocol.dismiss(9) to "share_dismiss",
        )) {
            val j = JSONObject(req)
            assertEquals(action, j.getString("action")); assertEquals(9, j.getLong("transfer_id")); assertEquals(3, j.length())
        }
    }

    @Test fun parsesTransfers() {
        val list = ShareProtocol.parseTransfers(JSONArray().put(transfer()).put(transfer(id = 6, direction = "outgoing", state = "active")))
        assertEquals(2, list.size)
        val t = list[0]
        assertEquals(TransferState.Offered, t.state); assertEquals(TransferDirection.Incoming, t.direction)
        assertEquals("report.pdf", t.name); assertEquals(1000L, t.size); assertEquals(250L, t.bytes); assertEquals(25, ProgressText.percent(t))
        assertEquals(TransferDirection.Outgoing, list[1].direction)
    }

    @Test fun dropsMalformedAndUnknownEntries() {
        val bad = JSONArray()
            .put(transfer(state = "exploding")).put(transfer(direction = "sideways")).put(transfer(kind = "video"))
            .put(JSONObject().put("transfer_id", -1)).put("not an object")
            .put(transfer(id = 7).put("device_id", "bad id!")).put(transfer(id = 8).put("transfer_id", "x"))
            .put(transfer(id = 9, size = JSONObject.NULL, bytes = -5))
        val list = ShareProtocol.parseTransfers(bad)
        assertEquals(listOf(9L), list.map { it.id })
        assertNull(list[0].size); assertEquals(0L, list[0].bytes); assertNull(ProgressText.percent(list[0]))
        assertTrue(ShareProtocol.parseTransfers(null).isEmpty())
    }

    @Test fun boundsEverything() {
        val many = JSONArray(); for (i in 1..100) many.put(transfer(id = i.toLong()))
        assertEquals(ShareLimits.MAX_TRANSFERS, ShareProtocol.parseTransfers(many).size)
        val huge = "x".repeat(100_000)
        val t = ShareProtocol.parseTransfers(JSONArray().put(transfer(name = huge)).put(transfer(id = 2, kind = "text", state = "done").put("text", huge)))
        assertTrue(t[0].name.length <= ShareLimits.MAX_NAME_CHARS)
        assertEquals("", t[1].name); assertEquals(ShareLimits.MAX_TEXT_CHARS_SHOWN, t[1].text!!.length)
        assertNull(ShareProtocol.parseTransfers(JSONArray().put(transfer(state = "failed").put("code", "Bad Code!")))[0].code)
    }

    @Test fun snapshotCarriesTransfers() {
        val j = JSONObject()
            .put("state", "running")
            .put("local", JSONObject().put("device_id", "_local_device_id_0123456789abcdef").put("device_name", "P").put("cert_sha256", "aa"))
            .put("transfers", JSONArray().put(transfer()))
        val s = TransportProtocol.parseSnapshot(j)!!
        assertEquals(1, s.transfers.size)
        j.remove("transfers")
        assertTrue(TransportProtocol.parseSnapshot(j)!!.transfers.isEmpty())
        assertTrue(TransportProtocol.parseSnapshot(JSONObject().put("state", "stopped"))!!.transfers.isEmpty())
    }
}

class ShareLogicTest {
    @Test fun sanitisesNames() {
        assertEquals("passwd", NameRules.sanitize("../../etc/passwd"))
        assertEquals("boot.ini", NameRules.sanitize("..\\..\\boot.ini"))
        assertEquals("file", NameRules.sanitize(""))
        assertEquals("file", NameRules.sanitize(".."))
        assertEquals("file", NameRules.sanitize(null))
        assertEquals("hidden", NameRules.sanitize(".hidden"))
        assertEquals("a_b_c.txt", NameRules.sanitize("a:b*c.txt"))
        assertEquals("tab_x.txt", NameRules.sanitize("tab\tx.txt"))
        val long = NameRules.sanitize("é".repeat(300) + ".jpeg")
        assertTrue(Bounds.utf8Size(long) <= 120); assertTrue(long.endsWith(".jpeg"))
    }

    @Test fun truncationNeverSplitsASurrogatePair() {
        for (name in listOf("😀".repeat(40), "😀".repeat(40) + ".png", "a" + "😀".repeat(40) + ".png")) {
            val clean = NameRules.sanitize(name)
            assertTrue(Bounds.utf8Size(clean) <= 120)
            for (i in clean.indices) {
                val c = clean[i]
                if (Character.isHighSurrogate(c)) assertTrue(i + 1 < clean.length && Character.isLowSurrogate(clean[i + 1]))
                if (Character.isLowSurrogate(c)) assertTrue(i > 0 && Character.isHighSurrogate(clean[i - 1]))
            }
        }
    }

    @Test fun uniqueNames() {
        val taken = setOf("a.txt", "a (1).txt", "noext")
        assertEquals("a (2).txt", NameRules.unique("a.txt") { it in taken })
        assertEquals("b.txt", NameRules.unique("b.txt") { it in taken })
        assertEquals("noext (1)", NameRules.unique("noext") { it in taken })
    }

    @Test fun mimeTypes() {
        assertEquals("image/jpeg", NameRules.mimeFor("X.JPG"))
        assertEquals("application/octet-stream", NameRules.mimeFor("noext"))
        assertEquals("application/octet-stream", NameRules.mimeFor("a.weird"))
    }

    @Test fun classifiesIntents() {
        val files = ShareIntentLogic.classify(listOf("content://a/1", "content://a/2", "content://a/1"), "caption")
        assertEquals(ShareRequest.Files(listOf("content://a/1", "content://a/2"), false), files)
        val many = ShareIntentLogic.classify((1..80).map { "content://a/$it" }, null) as ShareRequest.Files
        assertEquals(ShareLimits.MAX_FILES, many.uris.size); assertTrue(many.truncated)
        assertEquals(ShareRequest.Text("https://example.com/x", TransferKind.Url), ShareIntentLogic.classify(emptyList(), "  https://example.com/x \n"))
        assertEquals(ShareRequest.Text("hello world", TransferKind.Text), ShareIntentLogic.classify(emptyList(), "hello world"))
        assertEquals(TransferKind.Text, (ShareIntentLogic.classify(emptyList(), "see https://a.b now") as ShareRequest.Text).kind)
        assertEquals(TransferKind.Text, (ShareIntentLogic.classify(emptyList(), "javascript:alert(1)") as ShareRequest.Text).kind)
        assertTrue(ShareIntentLogic.classify(emptyList(), null) is ShareRequest.Invalid)
        assertTrue(ShareIntentLogic.classify(emptyList(), "   ") is ShareRequest.Invalid)
        assertTrue(ShareIntentLogic.classify(emptyList(), "x".repeat(ShareLimits.MAX_TEXT_BYTES + 1)) is ShareRequest.Invalid)
        assertFalse(ShareIntentLogic.isWebUrl("ftp://x")); assertFalse(ShareIntentLogic.isWebUrl("https://a b"))
    }

    private class MemStore : PolicyStore {
        val m = HashMap<String, Boolean>()
        override fun getBoolean(key: String) = m[key] ?: false
        override fun putBoolean(key: String, value: Boolean) { m[key] = value }
    }

    @Test fun receivePolicyDefaultsToAsk() {
        val p = ReceivePolicy(MemStore())
        assertFalse(p.autoAccept("dev1"))
        p.setAutoAccept("dev1", true)
        assertTrue(p.autoAccept("dev1")); assertFalse(p.autoAccept("dev2"))
        p.setAutoAccept("dev1", false)
        assertFalse(p.autoAccept("dev1"))
    }

    private fun snap(conns: List<LinkConnection>, paired: List<PairedPeer>, seen: List<DiscoveredPeer> = emptyList()) =
        TransportSnapshot("running", null, null, null, seen, conns, emptyList(), 0, paired, false, emptyList(), 0)

    private fun conn(id: Long, dev: String, trusted: Boolean) =
        LinkConnection(id, "acceptor", if (trusted) "trusted" else "parked", trusted, PeerInfo(dev, "N$dev", "desktop", "", "1.2.3.4"), 0)

    @Test fun targetSelection() {
        assertTrue(TargetPicker.targets(null).isEmpty())
        val one = TargetPicker.targets(snap(listOf(conn(1, "a", true), conn(2, "b", false)), listOf(PairedPeer("a", "A", "", ""))))
        assertEquals(1, one.size); assertEquals(1L, TargetPicker.autoChoice(one)!!.connectionId)

        val two = TargetPicker.targets(snap(listOf(conn(1, "a", true), conn(2, "b", true)), emptyList()))
        assertNull(TargetPicker.autoChoice(two))

        val seen = DiscoveredPeer("p", "P", "desktop", "10.0.0.5", 1716, true)
        val offline = TargetPicker.targets(snap(emptyList(), listOf(PairedPeer("p", "P", "", ""), PairedPeer("q", "Q", "", "")), listOf(seen)))
        assertEquals(2, offline.size); assertNotNull(offline[0].reachableAt); assertNull(offline[1].reachableAt)
        assertNull(TargetPicker.autoChoice(offline))
        // A single paired device that is not connected yet is still the unambiguous choice (but not ready).
        val lone = TargetPicker.targets(snap(emptyList(), listOf(PairedPeer("p", "P", "", ""))))
        assertNull(TargetPicker.autoChoice(lone)!!.connectionId)
    }

    @Test fun progressWording() {
        val t = ShareProtocol.parseTransfers(JSONArray().put(transfer(direction = "outgoing", state = "active", bytes = 500)))[0]
        assertEquals("report.pdf — 50% (500 B)", ProgressText.line(t))
        assertEquals("1.5 MiB", ProgressText.bytes(1_572_864))
        val failed = ShareProtocol.parseTransfers(JSONArray().put(transfer(state = "failed").put("code", "timeout")))[0]
        assertEquals("report.pdf — failed (timeout)", ProgressText.line(failed))
        assertTrue(failed.state.finished); assertFalse(TransferState.Active.finished)
    }
}

class ShareAuthorityTest {
    private val own = "dev.geekbridge.android"

    @Test fun blocksOwnAndSensitiveSystemProviders() {
        for (a in listOf(
            "dev.geekbridge.android.updates", "dev.geekbridge.android", "com.android.contacts", "com.android.calendar",
            "call_log", "sms", "mms", "COM.ANDROID.CONTACTS", "7@com.android.contacts", null, "",
        )) {
            assertTrue("$a", ShareIntentLogic.isBlockedAuthority(a, own))
        }
    }

    @Test fun allowsOrdinaryProviders() {
        for (a in listOf("com.android.providers.media.documents", "media", "com.example.files", "dev.geekbridgeevil.files")) {
            assertFalse("$a", ShareIntentLogic.isBlockedAuthority(a, own))
        }
    }
}

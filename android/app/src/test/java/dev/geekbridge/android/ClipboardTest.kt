package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ClipboardPolicyTest {
    private val now = 1_000_000L

    private fun snap(
        text: String? = "hello",
        copiedAgo: Long = 1_000,
        capturedAgo: Long = 500,
        sensitive: Boolean = false,
        kind: ClipKind = ClipKind.Text,
        html: String? = null,
    ) = ClipSnapshot(kind, text, html, null, null, now - copiedAgo, now - capturedAgo, sensitive)

    private fun decide(
        s: ClipSnapshot?,
        fresh: Boolean = false,
        enabled: Boolean = true,
        locked: Boolean = false,
        setting: Long = 30_000,
        requested: Long? = null,
    ) = PullGate.decide(enabled, locked, s, fresh, now, setting, requested)

    private fun reason(v: Verdict) = (v as Verdict.Refuse).reason

    @Test fun freshUnlockedRecentItemIsSent() {
        assertTrue(decide(snap()) is Verdict.Send)
    }

    @Test fun neverSendsWhileLockedNorWhenOff() {
        assertEquals("locked", reason(decide(snap(), locked = true)))
        assertEquals("locked", reason(decide(snap(), locked = true, fresh = true)))
        assertEquals("disabled", reason(decide(snap(), enabled = false)))
        // Disabled wins over locked: the answer says what the user can change.
        assertEquals("disabled", reason(decide(snap(), enabled = false, locked = true)))
    }

    @Test fun lockedIsDecidedWithoutLookingAtTheSnapshot() {
        // A null snapshot would otherwise be "needs_gesture"; locked must come first.
        assertEquals("locked", reason(decide(null, locked = true)))
    }

    @Test fun itemsOlderThanTheWindowAreNotOffered() {
        assertTrue(decide(snap(copiedAgo = 30_000)) is Verdict.Send)
        // From a cache we can't tell if the clipboard changed since, so ask for a tap.
        assertEquals("needs_gesture", reason(decide(snap(copiedAgo = 30_001))))
        // When it was just read in a focused window, it's simply too old.
        assertEquals("expired", reason(decide(snap(copiedAgo = 30_001), fresh = true)))
    }

    @Test fun theWindowIsASettingAndTheComputerCanOnlyNarrowIt() {
        assertTrue(decide(snap(copiedAgo = 50_000), setting = 60_000) is Verdict.Send)
        assertEquals("expired", reason(decide(snap(copiedAgo = 50_000), fresh = true, setting = 60_000, requested = 20_000)))
        // A computer asking for more than the user allowed gets the user's limit.
        assertEquals("expired", reason(decide(snap(copiedAgo = 50_000), fresh = true, setting = 30_000, requested = 600_000)))
        assertEquals(5_000L, PullGate.effectiveWindowMs(1, null))
        assertEquals(600_000L, PullGate.effectiveWindowMs(Long.MAX_VALUE, null))
    }

    @Test fun aCachedSnapshotIsOnlyTrustedBrieflyBecauseAndroidHidesLaterCopies() {
        assertTrue(decide(snap(capturedAgo = PullGate.CACHE_TRUST_MS)) is Verdict.Send)
        assertEquals("needs_gesture", reason(decide(snap(capturedAgo = PullGate.CACHE_TRUST_MS + 1))))
        // A tap reads the clipboard right now, so the capture age is irrelevant.
        assertTrue(decide(snap(capturedAgo = 120_000), fresh = true) is Verdict.Send)
    }

    @Test fun nothingCachedMeansAskForATapUnlessWeJustLooked() {
        assertEquals("needs_gesture", reason(decide(null)))
        assertEquals("empty", reason(decide(null, fresh = true)))
    }

    @Test fun sensitiveAndOversizedItemsAreRefused() {
        assertEquals("sensitive", reason(decide(snap(sensitive = true))))
        assertEquals("sensitive", reason(decide(snap(sensitive = true), fresh = true)))
        assertEquals("too_large", reason(decide(snap(text = "x".repeat(ClipboardLimits.MAX_TEXT_BYTES + 1)))))
        assertEquals("too_large", reason(decide(snap(text = "é".repeat(ClipboardLimits.MAX_TEXT_BYTES / 2 + 1)))))
        assertEquals(
            "too_large",
            reason(decide(snap(kind = ClipKind.Html, html = "y".repeat(ClipboardLimits.MAX_TEXT_BYTES + 1)))),
        )
    }

    @Test fun explicitSendsIgnoreAgeButNotLockOrSensitivity() {
        val old = snap(copiedAgo = 3_600_000)
        assertTrue(PullGate.decideExplicit(true, false, old) is Verdict.Send)
        assertEquals("locked", reason(PullGate.decideExplicit(true, true, old)))
        assertEquals("sensitive", reason(PullGate.decideExplicit(true, false, snap(sensitive = true))))
        assertEquals("disabled", reason(PullGate.decideExplicit(false, false, old)))
        assertEquals("empty", reason(PullGate.decideExplicit(true, false, null)))
    }

    @Test fun classification() {
        assertEquals(ClipKind.Text, ClipClassifier.classify("hi", null, null))
        assertEquals(ClipKind.Url, ClipClassifier.classify("https://example.org/a?b=1", null, null))
        assertEquals(ClipKind.Text, ClipClassifier.classify("see https://example.org", null, null))
        assertEquals(ClipKind.Html, ClipClassifier.classify("plain", "<b>plain</b>", null))
        assertEquals(ClipKind.Image, ClipClassifier.classify(null, null, "image/PNG"))
        assertNull(ClipClassifier.classify(null, null, "image/gif"))
        assertNull(ClipClassifier.classify("", null, null))
        assertNull(ClipClassifier.imageMimeOf("application/pdf"))
    }

    @Test fun settingsAreClamped() {
        val s = ClipSettings(enabled = true, windowMs = 1, maxImageBytes = Long.MAX_VALUE).normalized()
        assertEquals(ClipboardLimits.MIN_WINDOW_MS, s.windowMs)
        assertEquals(ClipboardLimits.HARD_MAX_IMAGE_BYTES, s.maxImageBytes)
        assertFalse(ClipSettings().enabled) // opt-in
        assertEquals(ClipboardLimits.DEFAULT_WINDOW_MS, ClipSettings().windowMs)
    }

    @Test fun wordsAreHonestAndNeverLeakCodes() {
        assertTrue(ClipboardText.HONEST_NOTE.contains("background"))
        assertTrue(ClipboardText.summary(ClipSettings(enabled = true)).contains("unlocked"))
        assertTrue(ClipboardText.summary(ClipSettings()).startsWith("Off"))
        for (r in listOf("locked", "expired", "empty", "disabled", "too_large", "sensitive", "not_connected", "weird")) {
            assertFalse(ClipboardText.refusal(r).contains('_'))
        }
        assertEquals("30 s", ClipboardText.windowLabel(30_000))
        assertEquals("2 min", ClipboardText.windowLabel(120_000))
    }

    @Test fun snapshotToStringHidesContent() {
        assertFalse(snap(text = "TOP-SECRET").toString().contains("TOP-SECRET"))
    }
}

class ClipboardProtocolTest {
    @Test fun requestsHaveTheDocumentedShape() {
        val c = JSONObject(ClipboardProtocol.configRequest(true, 1234))
        assertEquals("transport_clipboard_config", c.getString("action"))
        assertTrue(c.getBoolean("enabled"))
        val w = JSONObject(ClipboardProtocol.waitRequest(7, timeoutMs = 999_999))
        assertEquals(7, w.getLong("since"))
        assertEquals(ClipboardProtocol.MAX_WAIT_MS, w.getLong("timeout_ms"))
        val t = JSONObject(ClipboardProtocol.sendTextRequest(ClipKind.Html, "p", "<b>p</b>", "r-1", 4, 250))
        assertEquals("html", t.getString("kind"))
        assertEquals("r-1", t.getString("request_id"))
        assertEquals(4, t.getLong("connection_id"))
        val e = JSONObject(ClipboardProtocol.sendTextRequest(ClipKind.Text, "p", null, null, null, -5))
        assertFalse(e.has("request_id")); assertFalse(e.has("connection_id")); assertFalse(e.has("html"))
        assertEquals(0, e.getLong("age_ms"))
        val n = JSONObject(ClipboardProtocol.sendNoneRequest("r-2", 9, "locked"))
        assertEquals("locked", n.getString("none"))
        val i = JSONObject(ClipboardProtocol.sendImageRequest(42, "r-3", 1, 10))
        assertEquals(42, i.getInt("image_fd")); assertEquals("image", i.getString("kind"))
    }

    @Test fun startObjectCarriesTheOptInAndImageLimit() {
        val o = ClipboardProtocol.startObject(ClipSettings(enabled = false, maxImageBytes = 1))
        assertFalse(o.getBoolean("enabled"))
        assertEquals(ClipboardLimits.MIN_IMAGE_BYTES, o.getLong("max_image_bytes"))
    }

    private fun wait(vararg events: JSONObject, stopped: Boolean = false) = JSONObject()
        .put("events", JSONArray(events.toList())).put("last_seq", 5).put("stopped", stopped).put("enabled", true)

    @Test fun parsesPullsAndItems() {
        val r = ClipboardProtocol.parseWait(
            wait(
                JSONObject("""{"seq":1,"kind":"pull","connection_id":3,"device_id":"abc","request_id":"r-1","max_age_ms":20000}"""),
                JSONObject("""{"seq":2,"kind":"item","item":"html","device_id":"abc","text":"p","html":"<b>p</b>"}"""),
                JSONObject("""{"seq":3,"kind":"item","item":"image","device_id":"abc","path":"/p/in-1.img","mime":"image/png","size":10}"""),
            ),
        )!!
        assertEquals(3, r.events.size)
        val pull = r.events[0] as ClipEvent.Pull
        assertEquals(20_000, pull.maxAgeMs); assertEquals("r-1", pull.requestId)
        val html = r.events[1] as ClipEvent.Incoming
        assertEquals(ClipKind.Html, html.kind); assertEquals("<b>p</b>", html.html)
        assertEquals("image/png", (r.events[2] as ClipEvent.Incoming).imageMime)
        assertEquals(5, r.lastSeq)
    }

    @Test fun malformedEventsAreDroppedNotTrusted() {
        val bad = listOf(
            """{"seq":1,"kind":"pull","connection_id":3,"device_id":"abc","request_id":"bad id!"}""",
            """{"seq":2,"kind":"pull","device_id":"abc","request_id":"r"}""",
            """{"seq":3,"kind":"item","item":"image","device_id":"abc","path":"/p","mime":"image/gif"}""",
            """{"seq":4,"kind":"item","item":"text","device_id":"abc","text":""}""",
            """{"seq":5,"kind":"item","item":"movie","device_id":"abc","text":"x"}""",
            """{"seq":6,"kind":"item","item":"text","device_id":"bad id","text":"x"}""",
            """{"kind":"item","item":"text","device_id":"abc","text":"x"}""",
        )
        val r = ClipboardProtocol.parseWait(wait(*bad.map { JSONObject(it) }.toTypedArray()))!!
        assertTrue(r.events.isEmpty())
        assertNull(ClipboardProtocol.parseWait("nope"))
        assertNull(ClipboardProtocol.parseWait(JSONObject()))
    }

    @Test fun oversizedTextIsDroppedAndEventsNeverPrintTheirContent() {
        val big = "x".repeat(ClipboardLimits.MAX_TEXT_BYTES + 1)
        val r = ClipboardProtocol.parseWait(
            wait(JSONObject().put("seq", 1).put("kind", "item").put("item", "text").put("device_id", "abc").put("text", big)),
        )!!
        assertTrue(r.events.isEmpty())
        val ev = ClipEvent.Incoming(1, "abc", ClipKind.Text, "TOP-SECRET", null, null, null)
        assertFalse(ev.toString().contains("TOP-SECRET"))
    }
}

class ClipboardEngineTest {
    private class Host : ClipboardHost {
        var locked = false
        val written = ArrayList<ClipEvent.Incoming>()
        var asks = 0
        var dismissed = 0
        var imageFd: Int? = 77
        var imageReads = 0
        override fun isLocked() = locked
        override fun write(item: ClipEvent.Incoming) { written += item }
        override fun askForTap() { asks++ }
        override fun dismissTapRequest() { dismissed++ }
        override fun <T> withImageFd(snapshot: ClipSnapshot, maxBytes: Long, block: (Int) -> T): T? {
            imageReads++
            return imageFd?.let(block)
        }
    }

    private var now = 10_000_000L
    private val host = Host()
    private val sent = ArrayList<JSONObject>()
    private var coreReply: CoreOutcome = CoreOutcome.Ok(JSONObject().put("queued", 1))
    private var settings = ClipSettings(enabled = true)
    private val engine = ClipboardEngine(
        core = { sent += JSONObject(it); coreReply },
        host = host,
        settings = { settings },
        clock = { now },
    )

    private fun snap(text: String = "hello", copiedAgo: Long = 1_000, sensitive: Boolean = false, kind: ClipKind = ClipKind.Text) =
        ClipSnapshot(kind, text, null, if (kind == ClipKind.Image) "content://x/1" else null, if (kind == ClipKind.Image) "image/png" else null,
            now - copiedAgo, now, sensitive)

    private fun pull(id: String = "r-1", maxAge: Long = 30_000) = ClipEvent.Pull(1, 3, "dev", id, maxAge)

    @Test fun pullWithAFreshUnlockedCopyIsAnsweredAtOnce() {
        engine.capture(snap("copied text", copiedAgo = 2_000))
        now += 3_000
        engine.onEvents(listOf(pull()))
        val r = sent.single()
        assertEquals("transport_clipboard_send", r.getString("action"))
        assertEquals("r-1", r.getString("request_id")); assertEquals(3, r.getLong("connection_id"))
        assertEquals("copied text", r.getString("text"))
        assertEquals(5_000, r.getLong("age_ms"))
        assertEquals(0, host.asks)
    }

    @Test fun aLockedPhoneAnswersLockedAndNeverAsksForATap() {
        engine.capture(snap())
        host.locked = true
        engine.onEvents(listOf(pull()))
        assertEquals("locked", sent.single().getString("none"))
        assertEquals(0, host.asks)
        assertFalse(sent.single().has("text"))
    }

    @Test fun withNothingRecentThePhoneAsksForATapAndThatTapAnswersTheQuestion() {
        engine.onEvents(listOf(pull()))
        assertTrue("no answer yet: the question stays open", sent.isEmpty())
        assertEquals(1, host.asks)
        now += 5_000
        val result = engine.onGesture(snap("fresh copy", copiedAgo = 4_000))
        assertTrue(result.sent)
        val r = sent.single()
        assertEquals("r-1", r.getString("request_id")); assertEquals("fresh copy", r.getString("text"))
        assertTrue(host.dismissed >= 1)
    }

    @Test fun aTapShowingAnOldCopyAnswersExpiredInsteadOfSendingIt() {
        engine.onEvents(listOf(pull()))
        val result = engine.onGesture(snap("old", copiedAgo = 120_000))
        assertFalse(result.sent)
        assertEquals("expired", sent.single().getString("none"))
        assertEquals("That was copied too long ago. Copy it again.", result.message)
    }

    @Test fun anUnansweredPullExpiresAndALaterTapIsAnExplicitSend() {
        engine.onEvents(listOf(pull()))
        now += ClipboardEngine.PENDING_MS + 1
        val result = engine.onGesture(snap("later", copiedAgo = 3_600_000))
        assertTrue("an explicit send has no age limit", result.sent)
        val r = sent.single()
        assertFalse(r.has("request_id")); assertFalse(r.has("connection_id"))
    }

    @Test fun sensitiveItemsAreNeverSentEvenWhenTheUserTaps() {
        engine.onEvents(listOf(pull()))
        val r1 = engine.onGesture(snap("pw", sensitive = true))
        assertFalse(r1.sent)
        assertEquals("sensitive", sent.single().getString("none"))
        sent.clear()
        val r2 = engine.onGesture(snap("pw", sensitive = true))
        assertFalse(r2.sent); assertTrue(sent.isEmpty())
    }

    @Test fun disabledSettingNeverSendsAndIgnoresIncoming() {
        settings = ClipSettings(enabled = false)
        engine.capture(snap())
        engine.onEvents(listOf(pull(), ClipEvent.Incoming(2, "d", ClipKind.Text, "x", null, null, null)))
        assertEquals("disabled", sent.single().getString("none"))
        assertTrue(host.written.isEmpty())
        assertFalse(engine.sendText("hi").sent)
        assertEquals(1, sent.size)
    }

    @Test fun incomingItemsAreWrittenWithoutATap_andNotSentBackAsIfCopiedByTheUser() {
        engine.onEvents(listOf(ClipEvent.Incoming(1, "d", ClipKind.Text, "from pc", null, null, null)))
        assertEquals("from pc", host.written.single().text)
        // The next time the app looks, the clipboard holds that same text: not the user's copy.
        engine.capture(snap("from pc"))
        engine.onEvents(listOf(pull()))
        assertEquals(1, host.asks)
        assertTrue(sent.isEmpty())
    }

    @Test fun aTapAnsweringAPullDoesNotHandBackTextTheComputerJustSent() {
        engine.onEvents(listOf(ClipEvent.Incoming(1, "d", ClipKind.Text, "from pc", null, null, null)))
        engine.onEvents(listOf(pull()))
        assertEquals(1, host.asks)
        val r = engine.onGesture(snap("from pc"))
        assertFalse(r.sent)
        assertEquals("empty", sent.single().getString("none"))
    }

    @Test fun lockedWhileTheUserTapsStillRefuses() {
        engine.onEvents(listOf(pull()))
        host.locked = true
        val r = engine.onGesture(snap())
        assertFalse(r.sent)
        assertEquals("locked", sent.single().getString("none"))
    }

    @Test fun selectedTextIsSentExplicitlyAndClassified() {
        assertTrue(engine.sendText("https://example.org/page").sent)
        assertEquals("url", sent.single().getString("kind"))
        assertFalse(engine.sendText("").sent)
    }

    @Test fun imagesGoThroughADescriptorAndAreRefusedWhenUnreadable() {
        engine.capture(snap("", kind = ClipKind.Image))
        engine.onEvents(listOf(pull("img-1")))
        val r = sent.single()
        assertEquals("image", r.getString("kind")); assertEquals(77, r.getInt("image_fd"))
        sent.clear()
        host.imageFd = null
        engine.capture(snap("", kind = ClipKind.Image))
        engine.onEvents(listOf(pull("img-2")))
        assertEquals("too_large", sent.single().getString("none"))
    }

    @Test fun aCoreRefusalIsReportedAsNotSent() {
        coreReply = CoreOutcome.Rejected("not_connected", "no computer", local = false)
        val r = engine.sendText("hello")
        assertFalse(r.sent)
        assertEquals("Not connected to a computer.", r.message)
    }

    @Test fun resetForgetsEverythingHeldInMemory() {
        engine.capture(snap())
        engine.onEvents(listOf(pull("a")))
        sent.clear()
        engine.reset()
        engine.onEvents(listOf(pull("b")))
        assertTrue("cache was dropped, so it asks instead of answering", sent.isEmpty())
        assertNotNull(host.asks)
    }
}

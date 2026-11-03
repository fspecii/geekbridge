// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationMirrorTest {
    private fun n(key: String = "k1", pkg: String = "com.chat", title: String = "Ada", text: String = "hi", block: MirrorInfo.() -> MirrorInfo = { this }) =
        MirrorInfo(key, pkg, "Chat", title, text, whenMs = 1_700_000_000_000, actions = listOf("Archive")).block()

    private class FakePort(var showing: List<MirrorInfo> = emptyList()) : MirrorPort {
        val cancelled = ArrayList<String>()
        val pressed = ArrayList<Pair<String, String>>()
        override fun active() = showing
        override fun cancel(key: String): Boolean { cancelled += key; return true }
        override fun press(key: String, title: String): Boolean { pressed += key to title; return true }
    }

    private class Rig(val port: FakePort = FakePort(), var on: Boolean = true, var apps: Set<String> = setOf("com.chat")) {
        val sent = ArrayList<JSONObject>()
        var outcome: CoreOutcome = CoreOutcome.Ok(JSONObject().put("queued", 1))
        val mirror = NotificationMirror(port, { sent += JSONObject(it); outcome }, { on }, { apps }, "dev.geekbridge.android")
        fun body(i: Int) = sent[i].getJSONObject("body")
    }

    @Test fun nothingIsMirroredByDefaultBecauseTheAppListIsEmpty() {
        assertEquals(emptySet<String>(), PhoneDataSettings().notificationApps)
        assertFalse(PhoneDataSettings().notifications)
        val r = Rig(apps = emptySet())
        r.mirror.onPosted(n())
        assertTrue(r.sent.isEmpty())
        assertFalse(MirrorRules.mirrors(n(), emptySet(), "me"))
    }

    @Test fun theSwitchAndAccessGateTheFeature() {
        val s = PhoneDataSettings(notifications = true)
        assertFalse(PhoneDataGate.active(s, PhoneDataPermissions()).notifications)
        assertTrue(PhoneDataGate.active(s, PhoneDataPermissions(notificationAccess = true)).notifications)
        assertFalse(PhoneDataGate.active(PhoneDataSettings(), PhoneDataPermissions(notificationAccess = true)).notifications)
        val r = Rig(on = false)
        r.mirror.onPosted(n())
        assertTrue(r.sent.isEmpty())
    }

    @Test fun ongoingForegroundServiceOwnSecretCallsAndSummariesAreNeverMirrored() {
        val own = "dev.geekbridge.android"
        val apps = setOf("com.chat", own)
        assertTrue(MirrorRules.mirrors(n(), apps, own))
        assertFalse(MirrorRules.mirrors(n(pkg = own), apps, own))
        assertFalse(MirrorRules.mirrors(n { copy(ongoing = true) }, apps, own))
        assertFalse(MirrorRules.mirrors(n { copy(foregroundService = true) }, apps, own))
        assertFalse(MirrorRules.mirrors(n { copy(groupSummary = true) }, apps, own))
        assertFalse(MirrorRules.mirrors(n { copy(secret = true) }, apps, own))
        for (c in listOf("call", "progress", "service", "transport", "alarm")) assertFalse(c, MirrorRules.mirrors(n { copy(category = c) }, apps, own))
        assertTrue(MirrorRules.mirrors(n { copy(category = "msg") }, apps, own))
        assertFalse(MirrorRules.mirrors(n(title = " ", text = ""), apps, own))
        assertFalse(MirrorRules.mirrors(n(pkg = "com.other"), apps, own))
    }

    @Test fun thePostUsesStockFieldNamesAndIsBounded() {
        val r = Rig()
        r.mirror.onPosted(n(title = "T".repeat(500), text = "line\nbreak " + "x".repeat(5000)) { copy(actions = listOf("A", "B", "C", "D", "")) })
        val o = r.sent.single()
        assertEquals("transport_data_send", o.getString("action"))
        assertEquals("kdeconnect.notification", o.getString("kind"))
        assertFalse(o.has("connection_id"))
        val b = r.body(0)
        assertEquals("gb1", b.getString("id"))
        assertEquals("Chat", b.getString("appName"))
        assertEquals(MirrorRules.MAX_TITLE, b.getString("title").length)
        assertEquals(MirrorRules.MAX_TEXT, b.getString("text").length)
        assertFalse(b.getString("text").contains('\n'))
        assertEquals("1700000000000", b.getString("time"))
        assertEquals(true, b.getBoolean("isClearable"))
        assertEquals(false, b.getBoolean("silent"))
        assertEquals(MirrorRules.MAX_ACTIONS, b.getJSONArray("actions").length())
        assertFalse(b.has("payloadHash"))
    }

    @Test fun theAndroidKeyIsNeverSentAndTheIdIsStablePerNotification() {
        val r = Rig()
        r.mirror.onPosted(n(key = "0|com.chat|7|null|10001"))
        r.mirror.onPosted(n(key = "0|com.chat|7|null|10001", text = "again"))
        assertEquals(r.body(0).getString("id"), r.body(1).getString("id"))
        assertFalse(r.sent.any { it.toString().contains("10001") })
        r.mirror.onPosted(n(key = "other"))
        assertNotEquals(r.body(0).getString("id"), r.body(2).getString("id"))
    }

    @Test fun anUnchangedRepostIsNotSentAgain() {
        val r = Rig()
        r.mirror.onPosted(n())
        r.mirror.onPosted(n())
        assertEquals(1, r.sent.size)
        r.mirror.onPosted(n(text = "changed"))
        assertEquals(2, r.sent.size)
    }

    @Test fun aQuietNotificationIsSentSilent() {
        val r = Rig()
        r.mirror.onPosted(n { copy(quiet = true) })
        assertEquals(true, r.body(0).getBoolean("silent"))
    }

    @Test fun removalIsToldOnlyForWhatWasMirrored() {
        val r = Rig()
        r.mirror.onRemoved("never-sent")
        assertTrue(r.sent.isEmpty())
        r.mirror.onPosted(n())
        r.mirror.onRemoved("k1")
        assertEquals(2, r.sent.size)
        assertEquals(true, r.body(1).getBoolean("isCancel"))
        assertEquals(r.body(0).getString("id"), r.body(1).getString("id"))
    }

    @Test fun aRequestForEverythingGoesToThatConnectionSilentAndBounded() {
        val port = FakePort((1..40).map { n(key = "k$it") { copy(whenMs = it * 1000L) } } + n(key = "x", pkg = "com.other") + n(key = "ong") { copy(ongoing = true) })
        val r = Rig(port)
        r.mirror.sendAll(7)
        assertEquals(MirrorRules.MAX_BULK, r.sent.size)
        assertTrue(r.sent.all { it.getLong("connection_id") == 7L })
        assertTrue((0 until r.sent.size).all { r.body(it).getBoolean("silent") })
        // Newest first, and only from the allowed app.
        assertEquals("40000", r.body(0).getString("time"))
    }

    @Test fun aFailedSendIsNotRememberedSoItIsTriedAgain() {
        val r = Rig()
        r.outcome = CoreOutcome.Rejected("not_connected", "x")
        r.mirror.onPosted(n())
        r.outcome = CoreOutcome.Ok(JSONObject())
        r.mirror.onPosted(n())
        assertEquals(2, r.sent.size)
    }

    @Test fun dismissAndButtonsActOnlyOnWhatWasMirroredAndStillShowsAndIsListed() {
        val showing = n()
        val port = FakePort(listOf(showing, n(key = "secret-app", pkg = "com.other")))
        val r = Rig(port)
        r.mirror.onPosted(showing)
        val id = r.body(0).getString("id")
        r.mirror.onDismiss("gb999")
        r.mirror.onAction("gb999", "Archive")
        assertTrue(port.cancelled.isEmpty() && port.pressed.isEmpty())
        r.mirror.onAction(id, "Delete everything")
        assertTrue(port.pressed.isEmpty())
        r.mirror.onAction(id, "Archive")
        assertEquals(listOf("k1" to "Archive"), port.pressed)
        r.mirror.onDismiss(id)
        assertEquals(listOf("k1"), port.cancelled)
        // The app was taken off the list afterwards: the old id no longer works.
        r.apps = emptySet()
        r.mirror.onDismiss(id)
        assertEquals(1, port.cancelled.size)
        // Something that cannot be cleared is never cancelled on a computer's word.
        val sticky = n(key = "s") { copy(clearable = false) }
        r.apps = setOf("com.chat")
        port.showing = listOf(sticky)
        r.mirror.onPosted(sticky)
        r.mirror.onDismiss(r.body(r.sent.size - 1).getString("id"))
        assertEquals(1, port.cancelled.size)
    }

    @Test fun requestsAreParsedStrictly() {
        assertEquals(MirrorRules.Ask.All, MirrorRules.parseRequest(JSONObject().put("request", true)))
        assertEquals(MirrorRules.Ask.Dismiss("gb1"), MirrorRules.parseRequest(JSONObject().put("cancel", "gb1")))
        assertNull(MirrorRules.parseRequest(JSONObject().put("request", false)))
        assertNull(MirrorRules.parseRequest(JSONObject().put("cancel", 5)))
        assertNull(MirrorRules.parseRequest(JSONObject().put("cancel", "a\nb")))
        assertNull(MirrorRules.parseRequest(JSONObject()))
        assertEquals("gb1" to "Archive", MirrorRules.parseAction(JSONObject().put("key", "gb1").put("action", "Archive")))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "gb1")))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "gb1").put("action", "")))
    }

    @Test fun idsAreBoundedAndForgotten() {
        val ids = MirrorIds(max = 3)
        val first = ids.idFor("a")
        repeat(5) { ids.idFor("k$it") }
        assertNull(ids.keyOf(first))
        assertEquals("k4", ids.keyOf(ids.idFor("k4")))
        ids.forget("k4")
        assertNull(ids.idOfKey("k4"))
    }

    @Test fun readinessNamesTheNextStepInOrder() {
        fun step(on: Boolean = true, granted: Boolean = true, restricted: Boolean = false, apps: Int = 1, paired: Int = 1, allowed: Int = 1) =
            MirrorReadiness.evaluate(on, granted, restricted, apps, paired, allowed)
        assertEquals(MirrorStep.TurnOn, step(on = false))
        assertEquals(MirrorStep.GrantAccess, step(granted = false))
        assertEquals(MirrorStep.GrantAccessRestricted, step(granted = false, restricted = true))
        assertEquals(MirrorStep.ChooseApps, step(apps = 0))
        assertEquals(MirrorStep.PairComputer, step(paired = 0))
        assertEquals(MirrorStep.ChooseComputer, step(allowed = 0))
        assertEquals(MirrorStep.Ready, step())
        for (s in MirrorStep.entries) assertTrue(MirrorReadiness.message(s, TestStrings).isNotBlank())
    }

    @Test fun grantsKnowNotificationsAndStartConfigDeclaresIt() {
        val g = ComputerGrants().set("dev1", GrantFeature.Notifications, true)
        assertTrue(g.allows("dev1", GrantFeature.Notifications))
        assertFalse(g.allows("dev2", GrantFeature.Notifications))
        assertEquals(listOf("dev1"), (0 until g.permitsObject().getJSONArray("notifications").length()).map { g.permitsObject().getJSONArray("notifications").getString(it) })
        assertEquals(0, ComputerGrants().permitsObject().getJSONArray("notifications").length())
        assertTrue(PhoneDataProtocol.startObject(ActiveFeatures(notifications = true))!!.getBoolean("notifications"))
        assertNull(PhoneDataProtocol.startObject(ActiveFeatures()))
    }

    // ---- test-gaps-4: risky branches

    @Test fun aLongOrPaddedButtonTitleStillPressesTheRealButton() {
        // The computer is given the clipped title (<= 48 chars, trimmed, controls removed) and sends it back.
        val long = "Reply later " + "x".repeat(60)
        val padded = "  Archive\u0007  "
        val showing = n { copy(actions = listOf(long, padded)) }
        val port = FakePort(listOf(showing))
        val r = Rig(port)
        r.mirror.onPosted(showing)
        val id = r.body(0).getString("id")
        val sent = r.body(0).getJSONArray("actions")
        assertEquals(2, sent.length())
        r.mirror.onAction(id, sent.getString(0))
        r.mirror.onAction(id, sent.getString(1))
        // The phone side matches the real button by the same clipping, so the clipped title is what is passed on.
        assertEquals(listOf("k1" to sent.getString(0), "k1" to "Archive"), port.pressed)
        assertEquals(MirrorRules.MAX_ACTION, sent.getString(0).length)
        // A title that was never offered still presses nothing.
        r.mirror.onAction(id, "x".repeat(48))
        assertEquals(2, port.pressed.size)
    }

    @Test fun clippingNeverLeavesALoneSurrogateAtTheCut() {
        val emoji = "\uD83D\uDE00"
        val s = "a".repeat(MirrorRules.MAX_APP - 1) + emoji
        val clipped = MirrorRules.clip(s, MirrorRules.MAX_APP)
        assertTrue(clipped.length <= MirrorRules.MAX_APP)
        assertFalse(Character.isHighSurrogate(clipped.last()))
        val body = MirrorRules.postBody(n { copy(appName = s, title = s, text = s) }, "gb1", false)
        for (f in listOf("appName", "title", "text")) {
            val v = body.getString(f)
            assertFalse(f, v.isNotEmpty() && Character.isHighSurrogate(v.last()))
        }
        // A whole pair that fits is kept.
        assertEquals(emoji, MirrorRules.clip(emoji, 2))
    }

    @Test fun clipCleansControlsTrimsAndHonoursTheBoundary() {
        assertEquals("a b", MirrorRules.clip("a\nb", 10))
        assertEquals("", MirrorRules.clip(" \t\u0000 ", 10))
        assertEquals("abc", MirrorRules.clip("abc", 3))
        assertEquals("ab", MirrorRules.clip("ab c", 3)) // cut at 3 leaves "ab " and is trimmed
        assertEquals("", MirrorRules.clip("", 5))
    }

    @Test fun theTimeIsNeverNegativeAndTheTickerFallsBackToTheTitle() {
        val b = MirrorRules.postBody(n(text = "") { copy(whenMs = -5) }, "gb1", true)
        assertEquals("0", b.getString("time"))
        assertEquals("Ada", b.getString("ticker"))
        assertEquals(true, b.getBoolean("silent"))
        assertEquals("Ada: hi", MirrorRules.postBody(n(), "gb1", false).getString("ticker"))
        assertEquals(false, MirrorRules.postBody(n(), "gb1", false).getBoolean("onlyOnce"))
        assertEquals("gb2", MirrorRules.cancelBody("gb2").getString("id"))
    }

    @Test fun requestParsingBoundariesAndPrecedence() {
        assertEquals(MirrorRules.Ask.Dismiss("x".repeat(128)), MirrorRules.parseRequest(JSONObject().put("cancel", "x".repeat(128))))
        assertNull(MirrorRules.parseRequest(JSONObject().put("cancel", "x".repeat(129))))
        assertNull(MirrorRules.parseRequest(JSONObject().put("cancel", "")))
        // A body with both asks the narrower thing; it never turns into "send everything".
        assertEquals(MirrorRules.Ask.Dismiss("gb1"), MirrorRules.parseRequest(JSONObject().put("cancel", "gb1").put("request", true)))
        assertNull(MirrorRules.parseRequest(JSONObject().put("cancel", "bad\nid").put("request", true)))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", 5).put("action", "A")))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "gb1").put("action", 7)))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "").put("action", "A")))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "a\u0000b").put("action", "A")))
        assertNull(MirrorRules.parseAction(JSONObject().put("key", "gb1").put("action", "x".repeat(201))))
        assertEquals("gb1" to "x".repeat(200), MirrorRules.parseAction(JSONObject().put("key", "gb1").put("action", "x".repeat(200))))
    }

    @Test fun aBulkSendStopsAtTheFirstRefusalAndNeverRunsWhileOff() {
        val port = FakePort((1..5).map { n(key = "k$it") { copy(whenMs = it * 1000L) } })
        val r = Rig(port)
        r.outcome = CoreOutcome.Rejected("rate_limited", "x")
        r.mirror.sendAll(3)
        assertEquals("one refusal ends the burst", 1, r.sent.size)
        r.sent.clear()
        r.on = false
        r.mirror.sendAll(3)
        r.mirror.onDismiss("gb1")
        r.mirror.onAction("gb1", "Archive")
        r.mirror.onRemoved("k1")
        assertTrue(r.sent.isEmpty() && port.cancelled.isEmpty() && port.pressed.isEmpty())
        // Without a connection id (a broadcast request) nothing is addressed.
        r.on = true
        r.outcome = CoreOutcome.Ok(JSONObject())
        r.mirror.sendAll(null)
        assertFalse(r.sent.first().has("connection_id"))
    }

    @Test fun resetForgetsIdsSoAnOldIdCannotActAndAnUnchangedPostGoesOutAgain() {
        val showing = n()
        val port = FakePort(listOf(showing))
        val r = Rig(port)
        r.mirror.onPosted(showing)
        val id = r.body(0).getString("id")
        r.mirror.reset()
        r.mirror.onDismiss(id)
        r.mirror.onAction(id, "Archive")
        assertTrue(port.cancelled.isEmpty() && port.pressed.isEmpty())
        r.mirror.onPosted(showing)
        assertEquals(2, r.sent.size)
        assertNotEquals("ids are never reused after a reset", id, r.body(1).getString("id"))
    }

    @Test fun removalWithoutTheFeatureIsSilentButStillForgetsTheId() {
        val r = Rig()
        r.mirror.onPosted(n())
        r.on = false
        r.mirror.onRemoved("k1")
        assertEquals(1, r.sent.size)
        r.on = true
        r.mirror.onRemoved("k1") // already forgotten: no second cancel
        assertEquals(1, r.sent.size)
    }

    @Test fun theRepostMemoryIsBoundedButNeverBlocksNewNotifications() {
        val r = Rig()
        repeat(MirrorRules.MAX_IDS + 20) { r.mirror.onPosted(n(key = "k$it")) }
        assertEquals(MirrorRules.MAX_IDS + 20, r.sent.size)
        // The oldest was forgotten, so the same thing posted again is sent again; a recent one is not.
        r.mirror.onPosted(n(key = "k${MirrorRules.MAX_IDS + 19}"))
        assertEquals(MirrorRules.MAX_IDS + 20, r.sent.size)
        r.mirror.onPosted(n(key = "k0"))
        assertEquals(MirrorRules.MAX_IDS + 21, r.sent.size)
    }

    @Test fun anIdIsNeverGivenToTwoKeysAndEvictionKeepsBothMapsInStep() {
        val ids = MirrorIds(max = 2)
        val a = ids.idFor("a")
        val b = ids.idFor("b")
        assertNotEquals(a, b)
        assertEquals(a, ids.idFor("a"))
        val c = ids.idFor("c") // evicts a
        assertNull(ids.keyOf(a))
        assertNull(ids.idOfKey("a"))
        assertEquals("b", ids.keyOf(b))
        assertEquals("c", ids.keyOf(c))
        val a2 = ids.idFor("a") // evicts b; a gets a fresh id
        assertNotEquals(a, a2)
        assertNull(ids.keyOf(b))
        ids.clear()
        assertNull(ids.keyOf(c))
        ids.forget("never") // no-op
    }

    @Test fun skipReasonsAreReportedInTheirDocumentedOrder() {
        val own = "me"
        assertEquals("own", MirrorRules.skipReason(n(pkg = own) { copy(ongoing = true) }, own))
        assertEquals("ongoing", MirrorRules.skipReason(n { copy(ongoing = true, secret = true) }, own))
        assertEquals("foreground_service", MirrorRules.skipReason(n { copy(foregroundService = true, groupSummary = true) }, own))
        assertEquals("group_summary", MirrorRules.skipReason(n { copy(groupSummary = true, secret = true) }, own))
        assertEquals("secret", MirrorRules.skipReason(n { copy(secret = true, category = "call") }, own))
        for (c in listOf("sys", "navigation", "status", "stopwatch")) assertEquals(c, "category", MirrorRules.skipReason(n { copy(category = c) }, own))
        assertEquals("empty", MirrorRules.skipReason(n(title = "", text = "  "), own))
        assertNull(MirrorRules.skipReason(n(title = "", text = "only text"), own))
        assertNull(MirrorRules.skipReason(n { copy(category = null) }, own))
    }
}

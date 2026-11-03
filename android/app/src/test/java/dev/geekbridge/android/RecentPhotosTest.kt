// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RecentPhotosTest {
    private fun photo(n: Long, name: String = "IMG_%04d.jpg".format(n)) =
        PhotoInfo(n, name, 1_700_000_000_000 + n, 2_000_000 + n, 4000, 3000)

    private class FakePort(
        var access: PhotoAccess = PhotoAccess.Full,
        var library: List<PhotoInfo> = emptyList(),
        var thumb: (PhotoInfo) -> PhotoThumb? = { PhotoThumb(160, 120, byteArrayOf(-1, -40, -1, -32, 1, 2)) },
        var present: (PhotoInfo) -> Boolean = { true },
    ) : PhotoPort {
        var latestCalls = 0
        val fullSends = ArrayList<Pair<PhotoInfo, Long>>()
        override fun access() = access
        override fun latest(max: Int): List<PhotoInfo> { latestCalls++; return library.take(max) }
        override fun thumbnail(p: PhotoInfo) = thumb(p)
        override fun exists(p: PhotoInfo) = present(p)
        override fun sendFull(p: PhotoInfo, connectionId: Long): Boolean { fullSends += p to connectionId; return true }
    }

    private class Rig(val port: FakePort = FakePort(), var on: Boolean = true) {
        val sent = ArrayList<Triple<String, JSONObject, Long>>()
        val photos = RecentPhotos(port, { k, b, c -> sent += Triple(k, b, c); true }, { on })
        fun ask(conn: Long, body: JSONObject) = photos.onRequest(conn, body)
        fun list(conn: Long = 7, limit: Int = 24) = ask(conn, JSONObject().put("op", "list").put("limit", limit))
        fun ids() = sent.last().second.getJSONArray("items").let { a -> (0 until a.length()).map { a.getJSONObject(it).getString("id") } }
    }

    // ------------------------------------------------------------------ permissions

    @Test fun androidVersionsAskForTheRightPhotoPermissions() {
        assertEquals(listOf(PhotoAccessRules.READ_MEDIA_IMAGES, PhotoAccessRules.READ_MEDIA_VISUAL_USER_SELECTED), PhotoAccessRules.requestPermissions(34).toList())
        assertEquals(listOf(PhotoAccessRules.READ_MEDIA_IMAGES), PhotoAccessRules.requestPermissions(33).toList())
        assertEquals(listOf(PhotoAccessRules.READ_EXTERNAL_STORAGE), PhotoAccessRules.requestPermissions(32).toList())
        assertEquals(listOf(PhotoAccessRules.READ_EXTERNAL_STORAGE), PhotoAccessRules.requestPermissions(26).toList())
    }

    @Test fun partialAccessOnAndroid14IsSeenAsPartialAndOnlyThere() {
        fun has(vararg p: String) = { x: String -> x in p }
        assertEquals(PhotoAccess.Full, PhotoAccessRules.evaluate(34, has(PhotoAccessRules.READ_MEDIA_IMAGES, PhotoAccessRules.READ_MEDIA_VISUAL_USER_SELECTED)))
        assertEquals(PhotoAccess.Partial, PhotoAccessRules.evaluate(34, has(PhotoAccessRules.READ_MEDIA_VISUAL_USER_SELECTED)))
        assertEquals(PhotoAccess.None, PhotoAccessRules.evaluate(34, has()))
        // Before 14 the partial permission does not exist, so it counts for nothing.
        assertEquals(PhotoAccess.None, PhotoAccessRules.evaluate(33, has(PhotoAccessRules.READ_MEDIA_VISUAL_USER_SELECTED)))
        assertEquals(PhotoAccess.Full, PhotoAccessRules.evaluate(33, has(PhotoAccessRules.READ_MEDIA_IMAGES)))
        assertEquals(PhotoAccess.Full, PhotoAccessRules.evaluate(30, has(PhotoAccessRules.READ_EXTERNAL_STORAGE)))
        assertEquals(PhotoAccess.None, PhotoAccessRules.evaluate(30, has(PhotoAccessRules.READ_MEDIA_IMAGES)))
    }

    @Test fun theFeatureIsActiveOnlyWithTheSwitchAndSomePhotoAccess() {
        val on = PhoneDataSettings(photos = true)
        assertFalse(PhoneDataGate.active(on, PhoneDataPermissions()).photos)
        assertFalse(PhoneDataGate.active(PhoneDataSettings(), PhoneDataPermissions(photoAccess = PhotoAccess.Full)).any)
        assertTrue(PhoneDataGate.active(on, PhoneDataPermissions(photoAccess = PhotoAccess.Partial)).photos)
        assertTrue(PhoneDataGate.active(on, PhoneDataPermissions(photoAccess = PhotoAccess.Full)).photos)
        // Nothing is declared to the core until it is active, and then only the flag is added.
        assertNull(PhoneDataProtocol.startObject(ActiveFeatures()))
        val o = PhoneDataProtocol.startObject(ActiveFeatures(photos = true))!!
        assertEquals(true, o.getBoolean("photos"))
        assertEquals(false, o.getBoolean("sms"))
        assertEquals(true, JSONObject(PhoneDataProtocol.configRequest(ActiveFeatures(photos = true))).getBoolean("photos"))
    }

    @Test fun photosAreGrantedPerComputerAndNeverInheritedByANewOne() {
        val g = ComputerGrants().set("a", GrantFeature.Photos, true)
        assertTrue(g.allows("a", GrantFeature.Photos))
        assertFalse(g.allows("b", GrantFeature.Photos))
        val permits = g.permitsObject()
        assertEquals(listOf("a"), permits.getJSONArray("photos").let { a -> (0 until a.length()).map { a.getString(it) } })
        assertEquals(0, permits.getJSONArray("sms").length())
        assertEquals(g, ComputerGrants.fromJson(g.toJson()))
        assertEquals(0, g.clear(GrantFeature.Photos).devicesFor(GrantFeature.Photos).size)
    }

    // ------------------------------------------------------------------ the wire

    @Test fun requestsAreParsedStrictly() {
        fun p(s: String) = PhotoRules.parseRequest(JSONObject(s))
        assertEquals(PhotoRules.Ask.List(24), p("""{"op":"list","limit":24}"""))
        assertEquals(PhotoRules.Ask.Thumb("p1"), p("""{"op":"thumb","id":"p1"}"""))
        assertEquals(PhotoRules.Ask.Get("p-1_x"), p("""{"op":"get","id":"p-1_x"}"""))
        for (bad in listOf(
            "{}", """{"op":"list"}""", """{"op":"list","limit":0}""", """{"op":"list","limit":25}""", """{"op":"list","limit":"5"}""",
            """{"op":"get"}""", """{"op":"get","id":""}""", """{"op":"get","id":"../x"}""", """{"op":"get","id":"${"a".repeat(33)}"}""",
            """{"op":"get","id":5}""", """{"op":"wipe","id":"p1"}""", """{"op":7}""",
        )) assertNull(bad, p(bad))
    }

    @Test fun onlyPlainNamedPicturesAreListed() {
        assertTrue(PhotoRules.listable(photo(1)))
        for (name in listOf("", " ", " lead.jpg", "trail.jpg ", "a\nb.jpg", "a\u0000b.jpg", "rtl‮evil.jpg", "x".repeat(101) + ".jpg")) {
            assertFalse("'$name'", PhotoRules.listable(photo(1, name)))
        }
        assertTrue(PhotoRules.listable(photo(1, "x".repeat(96) + ".jpg")))
        assertFalse(PhotoRules.listable(photo(1).copy(size = 0)))
        assertFalse(PhotoRules.listable(photo(1).copy(size = PhotoRules.MAX_PHOTO_BYTES + 1)))
        assertFalse(PhotoRules.listable(photo(1).copy(width = 0)))
        assertFalse(PhotoRules.listable(photo(1).copy(height = 100_001)))
    }

    @Test fun theListHasTheAgreedShapeAndIdsAreOpaque() {
        val r = Rig(FakePort(library = (1L..5L).map { photo(it) }))
        r.list(conn = 9, limit = 3)
        val (kind, body, conn) = r.sent.single()
        assertEquals(PhotoRules.LIST_KIND, kind)
        assertEquals(9L, conn) // to the asking computer only
        assertEquals(false, body.getBoolean("partial"))
        val items = body.getJSONArray("items")
        assertEquals(3, items.length())
        val first = items.getJSONObject(0)
        assertEquals(setOf("id", "name", "taken", "size", "w", "h"), first.keys().asSequence().toSet())
        assertEquals("IMG_0001.jpg", first.getString("name"))
        assertTrue(PhotoRules.validId(first.getString("id")))
        // No media-store id, uri or path leaves the phone.
        assertFalse(body.toString().contains("content:"))
        assertFalse(body.toString().contains("/storage"))
    }

    @Test fun partialAccessIsSaidAndOddNamesAreSkippedNotReplacedByGaps() {
        val lib = listOf(photo(1), photo(2, "bad\nname.jpg"), photo(3), photo(4))
        val r = Rig(FakePort(access = PhotoAccess.Partial, library = lib))
        r.list(limit = 3)
        val body = r.sent.single().second
        assertEquals(true, body.getBoolean("partial"))
        assertEquals(listOf("IMG_0001.jpg", "IMG_0003.jpg", "IMG_0004.jpg"), body.getJSONArray("items").let { a -> (0 until a.length()).map { a.getJSONObject(it).getString("name") } })
    }

    // ------------------------------------------------------------------ privacy gates

    @Test fun nothingIsReadOrSentWhenOffOrWithoutAccess() {
        val port = FakePort(library = listOf(photo(1)))
        val r = Rig(port, on = false)
        r.list(); r.ask(7, JSONObject().put("op", "thumb").put("id", "p1")); r.ask(7, JSONObject().put("op", "get").put("id", "p1"))
        assertTrue(r.sent.isEmpty())
        assertEquals(0, port.latestCalls)
        r.on = true
        port.access = PhotoAccess.None
        r.list()
        assertTrue(r.sent.isEmpty())
        assertEquals(0, port.latestCalls)
        assertTrue(port.fullSends.isEmpty())
    }

    @Test fun onlyListedIdsCanBeAskedForAndAFullPictureNeedsAGetOfAListedId() {
        val port = FakePort(library = (1L..3L).map { photo(it) })
        val r = Rig(port)
        // Before any list there is nothing to ask for: a thumbnail request is answered "gone", a get is ignored.
        r.ask(7, JSONObject().put("op", "thumb").put("id", "p1"))
        assertTrue(r.sent.single().second.getBoolean("gone"))
        r.ask(7, JSONObject().put("op", "get").put("id", "p1"))
        assertTrue(port.fullSends.isEmpty())
        r.sent.clear()

        r.list()
        val ids = r.ids()
        // Listing and thumbnails never send the whole file.
        ids.forEach { r.ask(7, JSONObject().put("op", "thumb").put("id", it)) }
        assertTrue(port.fullSends.isEmpty())
        assertEquals(4, r.sent.size)
        // An id the phone did not hand out is refused, even if it is a real media id.
        r.ask(7, JSONObject().put("op", "get").put("id", "p999"))
        r.ask(7, JSONObject().put("op", "get").put("id", "2"))
        assertTrue(port.fullSends.isEmpty())
        // A listed id works, for the connection that asked.
        r.ask(11, JSONObject().put("op", "get").put("id", ids[1]))
        assertEquals(listOf(photo(2) to 11L), port.fullSends)
        // A picture deleted (or no longer visible after the user narrowed access) since the list is not sent.
        port.present = { false }
        r.ask(11, JSONObject().put("op", "get").put("id", ids[0]))
        assertEquals(1, port.fullSends.size)
    }

    @Test fun thumbnailsAreBoundedAndEncodedForTheWire() {
        val r = Rig(FakePort(library = listOf(photo(1))))
        r.list()
        val id = r.ids().single()
        r.ask(7, JSONObject().put("op", "thumb").put("id", id))
        val t = r.sent.last().second
        assertEquals(PhotoRules.THUMB_KIND, r.sent.last().first)
        assertEquals(id, t.getString("id"))
        assertEquals(160, t.getInt("w")); assertEquals(120, t.getInt("h"))
        assertEquals(listOf<Byte>(-1, -40, -1, -32, 1, 2), java.util.Base64.getDecoder().decode(t.getString("jpeg")).toList())
        // Too big or unreadable: "gone", never an oversized packet.
        r.port.thumb = { PhotoThumb(160, 120, ByteArray(PhotoRules.MAX_THUMB_BYTES + 1)) }
        r.ask(7, JSONObject().put("op", "thumb").put("id", id))
        assertTrue(r.sent.last().second.getBoolean("gone"))
        r.port.thumb = { PhotoThumb(300, 120, ByteArray(10)) }
        r.ask(7, JSONObject().put("op", "thumb").put("id", id))
        assertTrue(r.sent.last().second.getBoolean("gone"))
        r.port.thumb = { null }
        r.ask(7, JSONObject().put("op", "thumb").put("id", id))
        assertTrue(r.sent.last().second.getBoolean("gone"))
    }

    @Test fun idsAreNeverReusedAreBoundedAndForgottenOnReset() {
        val ids = PhotoIds(max = 5)
        val a = ids.assign((1L..3L).map { photo(it) })
        val b = ids.assign((4L..6L).map { photo(it) })
        assertEquals(6, (a + b).map { it.first }.toSet().size)
        assertNull("the oldest fell out of the bound", ids.get(a[0].first))
        assertNotEquals(null, ids.get(b[2].first))
        ids.clear()
        assertNull(ids.get(b[2].first))
        val c = ids.assign(listOf(photo(1)))
        assertTrue("no id from before the reset comes back", c[0].first !in (a + b).map { it.first })
    }

    @Test fun theThumbnailEncoderTriesLowerQualitiesAndGivesUpHonestly() {
        val tried = ArrayList<Int>()
        val out = PhotoRules.fit(100) { q -> tried += q; ByteArray(if (q <= 45) 90 else 400) }
        assertEquals(listOf(75, 60, 45), tried)
        assertEquals(90, out!!.size)
        assertNull(PhotoRules.fit(10) { ByteArray(500) })
    }

    // ------------------------------------------------------------------ in the engine

    private class Core {
        val sent = ArrayList<JSONObject>()
        fun call(req: String): CoreOutcome {
            val o = JSONObject(req)
            if (o.getString("action") == "transport_data_send") sent += o
            return CoreOutcome.Ok(JSONObject().put("queued", 1))
        }
    }

    private class NoContacts : ContactsPort {
        override fun listUids(max: Int) = emptyList<Pair<String, String>>()
        override fun vcard(uid: String): Pair<String, String>? = null
    }

    private class NoCal : CalendarPort {
        override fun calendars() = emptyList<CalendarInfo>()
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = emptyList<CalInstance>()
    }

    private class NoSms : SmsPort {
        override fun latestPerThread(max: Int) = emptyList<SmsRow>()
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
    }

    private class NoSender : SmsSender {
        override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {}
    }

    private fun engine(core: Core, f: ActiveFeatures, port: PhotoPort) =
        PhoneDataEngine(core::call, { f }, { emptySet() }, NoContacts(), NoCal(), NoSms(), NoSender(), photoPort = port)

    @Test fun theEngineAnswersToTheAskingConnectionOnlyWhileTheFeatureIsActive() {
        val port = FakePort(library = listOf(photo(1)))
        val core = Core()
        val ev = DataEvent(1, 42, PhotoRules.REQUEST_KIND, JSONObject().put("op", "list").put("limit", 5))
        engine(core, ActiveFeatures(sms = true), port).onEvents(listOf(ev))
        assertTrue("feature off: nothing read or sent", core.sent.isEmpty() && port.latestCalls == 0)
        engine(core, ActiveFeatures(photos = true), port).onEvents(listOf(ev))
        val o = core.sent.single()
        assertEquals(PhotoRules.LIST_KIND, o.getString("kind"))
        assertEquals(42L, o.getLong("connection_id"))
    }
}

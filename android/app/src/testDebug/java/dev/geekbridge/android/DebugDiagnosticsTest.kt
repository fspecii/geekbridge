// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Debug-build diagnostics (src/debug): ring buffer, streaming, redaction of seeded fake personal data, reasons. */
class DebugDiagnosticsTest {
    /** Made-up personal data. None of it may survive redaction in a log line or a state dump. */
    private val pii = listOf(
        "Alice Example", "+1 555 123 4567", "07911 123456", "5551234567", "alice@example.org",
        "Secret meeting text", "10.20.30.40", "fe80::1c2b:3d4e:5f60:7182", "aa:bb:cc:dd:ee:ff",
        "0123456789abcdef0123456789abcdef", "123e4567-e89b-12d3-a456-426614174000",
        "/storage/emulated/0/DCIM/Alice Example/photo.jpg",
    )

    private fun hostile(): List<String> = listOf(
        "11-02 10:00:00.000  1234  1234 D SmsEngine: body=Secret meeting text",
        "11-02 10:00:00.001  1234  1234 D Calls: incoming number=+1 555 123 4567 name=Alice Example",
        "11-02 10:00:00.002  1234  1234 I Net: connect to 10.20.30.40 port 17380",
        "11-02 10:00:00.003  1234  1234 W Net: peer fe80::1c2b:3d4e:5f60:7182 mac aa:bb:cc:dd:ee:ff",
        "11-02 10:00:00.004  1234  1234 E X: {\"address\":\"07911 123456\",\"body\":\"Secret meeting text\"}",
        "11-02 10:00:00.005  1234  1234 I Id: device 0123456789abcdef0123456789abcdef uuid 123e4567-e89b-12d3-a456-426614174000",
        "11-02 10:00:00.006  1234  1234 I Mail: from alice@example.org digits 5551234567",
        "11-02 10:00:00.007  1234  1234 I File: /storage/emulated/0/DCIM/Alice Example/photo.jpg saved",
        "clipboard: Secret meeting text",
        "text: Alice Example wrote Secret meeting text",
    )

    private fun leaks(s: String) = pii.filter { it in s }

    @Test fun seededPersonalDataNeverSurvivesRedaction() {
        for (raw in hostile()) {
            val out = DebugRedactor.line(raw)
            assertEquals("$raw -> $out", emptyList<String>(), leaks(out))
        }
    }

    /** Credentials and pairing secrets: none may reach the ring, the wire or the state dump. */
    @Test fun tokensAndSecretsNeverSurviveRedaction() {
        val secrets = listOf(
            "s3cr3tT0kenValue", "hunter2hunter2", "QWxhZGRpbjpvcGVuIHNlc2FtZQ", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl",
            "482913", "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MDEy",
        )
        val raws = listOf(
            "11-02 10:00:00.000  1  1 D Auth: token=s3cr3tT0kenValue",
            "11-02 10:00:00.001  1  1 D Auth: {\"token\":\"s3cr3tT0kenValue\"}",
            "11-02 10:00:00.002  1  1 D Net: Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl",
            "11-02 10:00:00.003  1  1 D Net: Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ",
            "11-02 10:00:00.004  1  1 D Pair: password=hunter2hunter2 secret=hunter2hunter2",
            "11-02 10:00:00.005  1  1 D Pair: verification_code=482913",
            "11-02 10:00:00.006  1  1 D Pair: qr_token Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MDEy",
            "11-02 10:00:00.007  1  1 D Pair: pin: 482913",
            "11-02 10:00:00.008  1  1 D Net: api_key=hunter2hunter2",
        )
        for (raw in raws) {
            val out = DebugRedactor.line(raw)
            assertEquals("$raw -> $out", emptyList<String>(), secrets.filter { it in out })
        }
    }

    @Test fun diagnosticCodesStayReadable() {
        val raw = "11-02 10:00:00.000  1  1 I GB: link: event link_closed code=peer_closed"
        assertEquals(raw, DebugRedactor.line(raw))
    }

    @Test fun theHeaderAndHarmlessLinesStayReadable() {
        val raw = "11-02 10:00:00.123  4321  4321 I GB: link: phase Stopped -> Starting (pid 4321)"
        assertEquals(raw, DebugRedactor.line(raw))
        assertEquals("tcp 10:11:12 ok", DebugRedactor.line("tcp 10:11:12 ok"))
    }

    @Test fun onlyThePairedComputersIpSurvives() {
        val raw = "11-02 10:00:00.000  1  1 I Net: link to 192.168.1.20 and 10.20.30.40"
        val out = DebugRedactor.line(raw, setOf("192.168.1.20"))
        assertTrue(out, "192.168.1.20" in out)
        assertFalse(out, "10.20.30.40" in out)
    }

    @Test fun lineLengthIsBounded() {
        assertEquals(DebugRedactor.MAX_LINE, DebugRedactor.line("x".repeat(5000)).length)
    }

    // ------------------------------------------------------------ ring

    @Test fun ringKeepsAtMostTwoThousandLinesAndDropsTheOldest() {
        val ring = LogRing()
        for (i in 0 until 2500) ring.add("line $i")
        assertEquals(2000, ring.size())
        assertEquals(500L, ring.firstIndex())
        assertEquals(2500L, ring.nextIndex())
        assertEquals("line 500", ring.snapshot().first())
        assertEquals("line 2499", ring.snapshot().last())
    }

    @Test fun ringIsAlsoCappedInBytes() {
        val ring = LogRing()
        val fat = "y".repeat(500)
        for (i in 0 until 1500) ring.add(fat)
        assertTrue(ring.byteSize().toString(), ring.byteSize() <= 256 * 1024)
        assertTrue(ring.size() in 400..600)
        // A single huge line never empties the buffer completely.
        val tiny = LogRing(maxLines = 10, maxBytes = 8)
        tiny.add("a line longer than the byte cap")
        assertEquals(1, tiny.size())
    }

    @Test fun readClampsToWhatIsStillHeldAndRespectsLimits() {
        val ring = LogRing(maxLines = 5)
        for (i in 0 until 8) ring.add("l$i")
        val c = ring.read(0, 100, 10_000)
        assertEquals(listOf("l3", "l4", "l5", "l6", "l7"), c.lines)
        assertEquals(8L, c.nextIndex)
        assertEquals(2, ring.read(5, 2, 10_000).lines.size)
        assertEquals(1, ring.read(0, 100, 4).lines.size) // byte cap, but always at least one line
        assertTrue(ring.read(8, 10, 100).lines.isEmpty())
    }

    // ------------------------------------------------------------ streamer

    private class Fake {
        var now = 0L
        val sent = ArrayList<Triple<Long, String, JSONObject>>()
        var result: DebugSendResult = DebugSendResult.Sent
        val send: (Long, String, JSONObject) -> DebugSendResult = { id, kind, body ->
            if (result == DebugSendResult.Sent) sent += Triple(id, kind, body)
            result
        }
    }

    private fun linesOf(t: Triple<Long, String, JSONObject>): List<String> =
        t.third.getJSONArray("lines").let { a -> (0 until a.length()).map { a.getString(it) } }

    @Test fun logsBufferWithoutALinkAndTheBacklogFlushesOnConnect() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        for (i in 0 until 400) ring.add("early $i")
        assertFalse(s.pump()) // no link: nothing sent, nothing lost
        assertTrue(f.sent.isEmpty())
        s.setConnections(setOf(7L))
        var rounds = 0
        while (s.pump() && rounds++ < 50) f.now += 600
        val all = f.sent.flatMap(::linesOf)
        assertEquals((0 until 400).map { "early $it" }, all)
        assertTrue(f.sent.all { it.first == 7L && it.second == DebugKinds.LOG })
        val seqs = f.sent.map { it.third.getLong("seq") }
        assertEquals(seqs.sorted(), seqs)
        assertEquals(seqs.size, seqs.toSet().size)
    }

    @Test fun packetsAreSizeCappedAndRateLimited() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        s.setConnections(setOf(1L))
        for (i in 0 until 1000) ring.add("z".repeat(300))
        s.pump()
        assertEquals(1, f.sent.size)
        val b = f.sent[0].third.toString().toByteArray().size
        assertTrue("$b", b <= DebugLogStreamer.MAX_BYTES + 2048)
        assertTrue(linesOf(f.sent[0]).size <= DebugLogStreamer.MAX_LINES)
        // Less than half a second later: no second packet.
        f.now += 100
        s.pump()
        assertEquals(1, f.sent.size)
        f.now += 500
        s.pump()
        assertEquals(2, f.sent.size)
    }

    @Test fun eachComputerHasItsOwnCursorAndANewOneGetsTheBacklog() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        ring.add("one"); ring.add("two")
        s.setConnections(setOf(1L))
        s.pump(); f.now += 600
        ring.add("three")
        s.setConnections(setOf(1L, 2L))
        repeat(6) { s.pump(); f.now += 600 }
        val to1 = f.sent.filter { it.first == 1L }.flatMap(::linesOf)
        val to2 = f.sent.filter { it.first == 2L }.flatMap(::linesOf)
        assertEquals(listOf("one", "two", "three"), to1)
        assertEquals(listOf("one", "two", "three"), to2)
    }

    @Test fun review3AComputerThatReconnectsDoesNotGetTheBacklogAgain() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        ring.add("one"); ring.add("two")
        s.setConnections(setOf(1L), mapOf(1L to "pc-a"))
        s.pump(); f.now += 600
        s.setConnections(emptySet()) // link lost
        ring.add("three")
        s.setConnections(setOf(2L), mapOf(2L to "pc-a")) // the same computer, a new connection
        repeat(4) { s.pump(); f.now += 600 }
        assertEquals(listOf("one", "two"), f.sent.filter { it.first == 1L }.flatMap(::linesOf))
        assertEquals(listOf("three"), f.sent.filter { it.first == 2L }.flatMap(::linesOf))
        // A different computer still gets the whole buffer.
        s.setConnections(setOf(2L, 3L), mapOf(2L to "pc-a", 3L to "pc-b"))
        repeat(4) { s.pump(); f.now += 600 }
        assertEquals(listOf("one", "two", "three"), f.sent.filter { it.first == 3L }.flatMap(::linesOf))
    }

    @Test fun aRateLimitedPacketIsRetriedNotLostAndAnUnwillingComputerIsParked() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        s.setConnections(setOf(1L))
        ring.add("kept")
        f.result = DebugSendResult.Later
        s.pump()
        f.now += 2000
        f.result = DebugSendResult.Sent
        s.pump()
        assertEquals(listOf("kept"), f.sent.flatMap(::linesOf))
        // Parked: not tried again for a while.
        val g = Fake()
        val s2 = DebugLogStreamer(ring, g.send) { g.now }
        s2.setConnections(setOf(9L))
        g.result = DebugSendResult.Parked
        s2.pump()
        g.result = DebugSendResult.Sent
        g.now += 5_000
        s2.pump()
        assertTrue(g.sent.isEmpty())
        g.now += 40_000
        s2.pump()
        assertEquals(1, g.sent.size)
    }

    @Test fun switchedOffSendsNothingButStillBuffers() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        s.setConnections(setOf(1L))
        s.enabled = false
        ring.add("quiet")
        assertFalse(s.pump())
        assertTrue(f.sent.isEmpty())
        s.enabled = true
        s.pump()
        assertEquals(listOf("quiet"), f.sent.flatMap(::linesOf))
    }

    @Test fun nothingPersonalLeavesThePhoneInAPacket() {
        val f = Fake()
        val ring = LogRing()
        val s = DebugLogStreamer(ring, f.send) { f.now }
        s.setConnections(setOf(1L))
        for (raw in hostile()) ring.add(DebugRedactor.line(raw))
        s.pump()
        assertEquals(emptyList<String>(), leaks(f.sent.joinToString("\n") { it.third.toString() }))
    }

    // ------------------------------------------------------------ reasons

    private fun inputs(
        sms: Boolean = false, contacts: Boolean = false, calendar: Boolean = false, edit: Boolean = false,
        calls: Boolean = false, clipboard: Boolean = false, debug: Boolean = true,
        perms: List<PermState> = emptyList(), computers: List<ComputerState> = emptyList(),
    ) = CapabilityInputs(sms, contacts, calendar, edit, calls, clipboard, debug, perms, computers)

    private fun smsPerms(read: Boolean, send: Boolean = true, recv: Boolean = true, restricted: Boolean = false) = listOf(
        PermState("READ_SMS", read, !read && restricted), PermState("SEND_SMS", send), PermState("RECEIVE_SMS", recv),
    )

    private fun computer(vararg g: GrantFeature, connected: Boolean = true) =
        ComputerState("computer 1 (abcdef)", connected, "192.168.1.20", g.toSet())

    private fun line(i: CapabilityInputs, key: String) = CapabilityReasons.table(i).first { it.feature == key }.text()

    @Test fun smsReasonsExplainEachWayItCanBeOff() {
        assertEquals(
            "sms: not offered: Messages is switched OFF in the app (master switch)",
            line(inputs(sms = false, perms = smsPerms(true), computers = listOf(computer(GrantFeature.Sms))), "sms"),
        )
        val restricted = line(
            inputs(sms = true, perms = smsPerms(read = false, restricted = true), computers = listOf(computer(GrantFeature.Sms))),
            "sms",
        )
        assertTrue(restricted, restricted.startsWith("sms: not offered: READ_SMS not granted (restricted setting"))
        assertEquals(
            "sms: not offered: READ_SMS not granted",
            line(inputs(sms = true, perms = smsPerms(read = false), computers = listOf(computer(GrantFeature.Sms))), "sms"),
        )
        assertEquals(
            "sms: not offered: per-computer Messages switch is OFF (no paired computer is allowed)",
            line(inputs(sms = true, perms = smsPerms(true), computers = listOf(computer(GrantFeature.Contacts))), "sms"),
        )
        assertEquals(
            "sms: not offered: no computer is paired",
            line(inputs(sms = true, perms = smsPerms(true)), "sms"),
        )
        assertEquals(
            "sms: offered",
            line(inputs(sms = true, perms = smsPerms(true), computers = listOf(computer(GrantFeature.Sms))), "sms"),
        )
    }

    @Test fun everyCapabilityHasADecisionAndEveryRefusalAReason() {
        val all = CapabilityReasons.table(inputs(debug = false))
        assertEquals(listOf("sms", "contacts", "calendar", "calendar_edit", "calls", "clipboard", "share", "debug"), all.map { it.feature })
        for (l in all) assertTrue(l.text(), l.offered || !l.reason.isNullOrBlank())
        assertEquals("debug: not offered: 'Send debug log to computer' is OFF in Diagnostics", all.last().text())
        assertTrue(all.first { it.feature == "share" }.offered)
    }

    @Test fun calendarEditingNeedsReadingAndWritePermission() {
        val perms = listOf(PermState("READ_CALENDAR", true), PermState("WRITE_CALENDAR", false))
        val c = computer(GrantFeature.Calendar, GrantFeature.CalendarEdit)
        assertEquals(
            "calendar_edit: not offered: WRITE_CALENDAR not granted",
            line(inputs(calendar = true, edit = true, perms = perms, computers = listOf(c)), "calendar_edit"),
        )
        assertTrue(
            line(inputs(calendar = false, edit = true, perms = perms, computers = listOf(c)), "calendar_edit")
                .startsWith("calendar_edit: not offered: Calendar is not shared"),
        )
        val ok = listOf(PermState("READ_CALENDAR", true), PermState("WRITE_CALENDAR", true))
        assertEquals("calendar_edit: offered", line(inputs(calendar = true, edit = true, perms = ok, computers = listOf(c)), "calendar_edit"))
    }

    @Test fun perComputerLinesNameTheSwitchThatIsOff() {
        val i = inputs(sms = true, contacts = true, perms = smsPerms(true) + PermState("READ_CONTACTS", true), computers = listOf(computer(GrantFeature.Contacts)))
        val lines = CapabilityReasons.forComputer(i, i.computers[0]).associate { it.feature to it.text() }
        assertEquals("sms: not offered: per-computer Messages switch is OFF", lines["sms"])
        assertEquals("contacts: offered", lines["contacts"])
    }

    // ------------------------------------------------------------ state dump

    private fun model(history: List<String>, exceptions: List<String>) = StateModel(
        "0.1.0", 100, "abc123def", "debug", "14", 34, "samsung SM-F946B", "Samsung One UI",
        inputs(
            sms = true, perms = smsPerms(read = false, restricted = true) + PermState("POST_NOTIFICATIONS", true),
            computers = listOf(computer(GrantFeature.Contacts)),
        ),
        listOf("clipboard", "share", "debug"), "Optimised", "ACTIVE", dozing = false, powerSave = false,
        service = "running; Stay connected=true", network = "wifi, internet validated, unmetered",
        linkPhase = "Running, 1 trusted connection(s)", linkHistory = history, exceptions = exceptions,
        logLines = 12, logBytes = 900, streamingTo = 1,
    )

    @Test fun stateDumpShowsPermissionsSwitchesPerComputerAndReasons() {
        val text = DebugStateText.render(model(emptyList(), emptyList()))
        for (needle in listOf(
            "READ_SMS: DENIED (restricted setting)", "SEND_SMS: granted", "POST_NOTIFICATIONS: granted",
            "battery optimisation: Optimised", "computer 1 (abcdef): connected (192.168.1.20)",
            "sms: not offered: READ_SMS not granted (restricted setting", "standby bucket: ACTIVE",
            "foreground service: running", "network: wifi", "commit abc123def", "SM-F946B", "Samsung One UI",
        )) assertTrue("missing '$needle' in:\n$text", needle in text)
    }

    @Test fun linkHealthExplainsTheLastDrop() {
        val drop = DropNote(atMs = 1_000_000, code = "ping_timeout", network = "wifi, internet validated, unmetered", quietMs = 74_000)
        val lines = LinkHealthText.render(drop, 3, nowMs = 1_000_000 + 300_000)
        val text = lines.joinToString("\n")
        for (needle in listOf("5 min ago", "reason ping_timeout", "network then: wifi", "for 74 s before it closed", "closes since the app started: 3"))
            assertTrue("missing '$needle' in:\n$text", needle in text)
        assertTrue(LinkHealthText.render(null, 0, 5).any { it.contains("no link has closed") })
        assertTrue("unknown time" in LinkHealthText.render(drop.copy(quietMs = null), 1, 2_000_000).joinToString())
        val dump = DebugStateText.render(model(emptyList(), emptyList()).copy(linkHealth = lines))
        assertTrue("== Link health ==" in dump && "reason ping_timeout" in dump)
    }

    @Test fun linkHealthShowsTheWifiBindSwitchAndFallback() {
        val none = LinkHealthText.render(null, 0, 5).joinToString("\n")
        assertTrue("Wi-Fi binding (bind_wifi): on" in none && "fallbacks since the app started: none" in none)
        assertTrue("OFF (kill switch)" in LinkHealthText.render(null, 0, 5, bindEnabled = false).joinToString())
        val note = BindNote(atMs = 1_000_000, code = "bound_stalled")
        val soon = LinkHealthText.render(null, 0, 1_000_000 + 120_000, true, note, 2).joinToString("\n")
        for (needle in listOf("fallbacks since the app started: 2", "last 2 min ago", "reason bound_stalled", "stay unbound for about 8 min more"))
            assertTrue("missing '$needle' in:\n$soon", needle in soon)
        val later = LinkHealthText.render(null, 0, 1_000_000 + LinkHealthText.UNHEALTHY_MS + 1, true, note, 2).joinToString("\n")
        assertTrue("bound dials are tried again" in later)
        assertFalse(listOf(soon, later).any { Regex("""\d+\.\d+\.\d+\.\d+""").containsMatchIn(it) })
    }

    @Test fun stateDumpNeverContainsSeededPersonalData() {
        val history = EventLog(50).also { l -> hostile().forEach(l::add) }.all()
        val exceptions = EventLog(20).also { l -> hostile().forEach(l::add) }.all()
        val text = DebugStateText.render(model(history, exceptions))
        assertEquals(text, emptyList<String>(), leaks(text))
        // Even a hostile line that reaches the model directly is cleaned by the final pass.
        val direct = DebugStateText.render(model(hostile(), hostile()))
        assertEquals(direct, emptyList<String>(), leaks(direct))
    }

    @Test fun historiesAreBounded() {
        val h = EventLog(50)
        for (i in 0 until 120) h.add("transition $i")
        assertEquals(50, h.all().size)
        assertTrue(h.all().last().endsWith("transition 119"))
        val e = EventLog(20)
        for (i in 0 until 30) e.add("java.lang.IllegalStateException $i")
        assertEquals(20, e.all().size)
    }

    @Test fun debugPacketKindsMatchTheComputersWireNames() {
        assertEquals("geekbridge.debug.log", DebugKinds.LOG)
        assertEquals("geekbridge.debug.state_request", DebugKinds.STATE_REQUEST)
        assertEquals("geekbridge.debug.state", DebugKinds.STATE)
    }
}

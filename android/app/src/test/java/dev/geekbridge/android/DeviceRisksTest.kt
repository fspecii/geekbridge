// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class DeviceRisksTest {
    @Test fun nothingPhonesAreRecognised() {
        assertEquals(Oem.Nothing, OemDetector.detect("Nothing", "Nothing"))
        assertEquals(Oem.Nothing, OemDetector.detect("Nothing", "CMF"))
    }

    @Test fun nothingNeedsNoExtraVendorSwitch() {
        val i = SetupInputs(
            sdk = 36, pairedCount = 1, notifications = PermissionStatus.Granted, contacts = PermissionStatus.Granted,
            calendar = PermissionStatus.Granted, phoneState = PermissionStatus.Granted, battery = BatteryState.Unrestricted,
            oemAcknowledged = false, oem = Oem.Nothing,
        )
        assertEquals(StepStatus.Done, SetupModel.steps(TestStrings, i).first { it.id == StepId.Battery }.status)
        assertEquals(StepStatus.Todo, SetupModel.steps(TestStrings, i.copy(oem = Oem.Xiaomi)).first { it.id == StepId.Battery }.status)
    }

    @Test fun launcherFallsBackWhenEveryVendorScreenIsRefused() {
        var fallback = 0
        val r = OemLauncher.launch(OemIntents.candidates(Oem.Samsung), { false }) { fallback++ }
        assertEquals(LaunchResult.OpenedFallback, r)
        assertEquals(1, fallback)
    }

    @Test fun ipv4ChangeOnTheSameNetworkRestartsOnlyAfterAFirstReport() {
        assertFalse(ServicePolicy.addressesChanged(null, setOf("192.168.1.5")))
        assertFalse(ServicePolicy.addressesChanged(setOf("192.168.1.5"), setOf("192.168.1.5")))
        assertTrue(ServicePolicy.addressesChanged(setOf("192.168.1.5"), setOf("10.0.0.7")))
        assertTrue(ServicePolicy.addressesChanged(setOf("192.168.1.5"), emptySet()))
    }

    // --- crash note: redacted, bounded, never throws -------------------------------------------------

    @Test fun crashNoteDropsExceptionMessagesAndKeepsClassNames() {
        val e = IllegalStateException("contact Alice +49 170 1234567 clipboard=secret", RuntimeException("cause with 192.168.1.20"))
        val note = CrashNote.render(e, "main", 1700000000000, "1.2.3")
        assertTrue(note, note.contains("java.lang.IllegalStateException"))
        assertTrue(note, note.contains("caused by java.lang.RuntimeException"))
        for (leak in listOf("Alice", "1234567", "secret", "192.168")) assertFalse("$leak in $note", note.contains(leak))
        assertTrue(note.contains("app 1.2.3"))
    }

    @Test fun crashNoteIsBoundedAndSanitisesTheThreadName() {
        var e: Throwable = RuntimeException()
        repeat(50) { e = RuntimeException(e) }
        val note = CrashNote.render(e, "evil\nthread name!" + "x".repeat(100), 1, "v")
        assertTrue(note.length <= 1200)
        assertFalse(note.lines().first().contains("name!"))
        assertTrue(note.lines().count { it.startsWith("caused by") } <= 2)
    }

    // --- early open: the share grant may be gone by the time a queued file is sent ---------------------

    private class Handle(val id: Int) : AutoCloseable {
        var closed = false
        override fun close() { closed = true }
    }

    @Test fun earlyOpenOpensOnceAndHandsOutOnce() {
        var opens = 0
        val e = EarlyOpen { Handle(++opens) }
        e.prefetch(); e.prefetch()
        val h = e.take()
        assertEquals(1, opens)
        assertEquals(1, h.id)
        assertEquals(2, e.take().id) // nothing held any more: opens again
    }

    @Test fun earlyOpenSurvivesARevokedGrantOnceOpened() {
        var granted = true
        val e = EarlyOpen { if (!granted) throw SecurityException("grant revoked") else Handle(1) }
        e.prefetch()
        granted = false // the Share screen is gone
        assertEquals(1, e.take().id)
        try { e.take(); fail() } catch (_: SecurityException) { }
    }

    @Test fun earlyOpenKeepsAHandleGivenBackAndClosesOnDiscard() {
        val e = EarlyOpen { Handle(1) }
        val h = e.take()
        e.giveBack(h)
        assertSame(h, e.take())
        e.giveBack(h)
        e.discard()
        assertTrue(h.closed)
        val late = Handle(2)
        e.giveBack(late)
        assertTrue(late.closed)
    }

    @Test fun earlyOpenPrefetchFailureIsReportedWhenTheFileIsNeeded() {
        val e = EarlyOpen<Handle> { throw java.io.IOException("provider died") }
        e.prefetch() // must not throw
        try { e.take(); fail() } catch (_: java.io.IOException) { }
    }

    @Test fun earlyOpenSaysWhenTheGrantNoLongerMatters() {
        // The Share screen may only go away once every file has been opened (or has failed to): the temporary read
        // grant ends with its task, and a file opened later than that cannot be read any more.
        val ok = EarlyOpen { Handle(1) }
        val bad = EarlyOpen<Handle> { throw java.io.IOException("provider died") }
        assertFalse(ok.isSettled()); assertFalse(bad.isSettled())
        ok.prefetch(); bad.prefetch()
        assertTrue(ok.isSettled()); assertTrue(bad.isSettled())
        val taken = EarlyOpen { Handle(2) }
        taken.take()
        assertTrue(taken.isSettled())
    }
}

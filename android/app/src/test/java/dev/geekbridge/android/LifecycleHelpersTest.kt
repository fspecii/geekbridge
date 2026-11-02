// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ShareHoldTest {
    private var acquired = 0
    private var released = 0
    private fun hold() = ShareHold({ acquired++ }, { released++ })

    @Test fun aRotationHandsTheSameHoldToTheNextScreenAndItIsReleasedOnce() {
        val token = hold()
        token.take() // first screen
        // Rotation: the new instance gets the same token, sees it held and does not take a second hold;
        // the old instance's onDestroy skips the release (isChangingConfigurations).
        token.take()
        assertEquals(1, acquired)
        assertEquals(0, released)
        // The user finishes the screen: one release, however often onDestroy runs.
        token.giveBack()
        token.giveBack()
        assertEquals(1, released)
        assertFalse(token.held)
    }

    @Test fun releasingWithoutEverHoldingDoesNothing() {
        hold().giveBack()
        assertEquals(0, released)
    }

    @Test fun theControllerCountBalancesAcrossARotation() {
        val c = TransportController({ _ -> CoreOutcome.Ok(null) }, { it.run() }, { it.run() })
        val token = ShareHold({ c.hold() }, { c.release() })
        token.take(); token.take(); token.giveBack()
        assertEquals(0, c.holds)
    }
}

class RecentIdsTest {
    @Test fun reportsFirstSightingOnly() {
        val ids = RecentIds(4)
        assertTrue(ids.add(1)); assertFalse(ids.add(1))
        assertTrue(1L in ids)
    }

    @Test fun staysBoundedAndForgetsTheOldest() {
        val ids = RecentIds(3)
        for (n in 1L..10L) ids += n
        assertEquals(3, ids.size)
        assertFalse(1L in ids)
        assertTrue(10L in ids && 8L in ids)
    }
}

class WaiterSlotTest {
    @Test fun onlyOneWaiterAtATime() {
        val slot = WaiterSlot()
        assertTrue(slot.claim())
        assertFalse(slot.claim())
    }

    @Test fun aWaiterThatIsLeavingCannotSwallowARestart() {
        val slot = WaiterSlot()
        assertTrue(slot.claim())
        // The setting went off: the waiter decides to leave and gives up its claim in the same step ...
        assertTrue(slot.leaveUnless { false })
        // ... so switching it on again straight away starts a new one, although the old thread may still be alive.
        assertTrue(slot.claim())
    }

    @Test fun aWaiterThatIsStillWantedKeepsItsClaim() {
        val slot = WaiterSlot()
        slot.claim()
        // Off and on again before the waiter looked: it stays and no second one is started.
        assertFalse(slot.leaveUnless { true })
        assertFalse(slot.claim())
    }
}

class SpoolKeeperTest {
    private val deleted = ArrayList<String>()
    private val spool = SpoolKeeper { deleted += it }

    @Test fun aNewerImageReplacesTheOlderOne() {
        spool.received("/a"); spool.received("/b")
        assertEquals(listOf("/a"), deleted)
    }

    @Test fun aLaterTextOrAResetDeletesTheImage() {
        spool.received("/a"); spool.received(null)
        assertEquals(listOf("/a"), deleted)
        spool.received("/c"); spool.clear(); spool.clear()
        assertEquals(listOf("/a", "/c"), deleted)
    }

    @Test fun theSameFileIsNotDeletedWhenItIsReceivedAgain() {
        spool.received("/a"); spool.received("/a")
        assertTrue(deleted.isEmpty())
    }
}

class PromoteOrStopTest {
    @Test fun aRefusedForegroundStartStopsTheServiceInsteadOfCrashing() {
        var stopped = 0
        val ok = ServicePolicy.promoteOrStop({ throw IllegalStateException("not allowed") }, { stopped++ })
        assertFalse(ok)
        assertEquals(1, stopped)
    }

    @Test fun anAllowedStartKeepsRunning() {
        var stopped = 0
        assertTrue(ServicePolicy.promoteOrStop({}, { stopped++ }))
        assertEquals(0, stopped)
    }
}

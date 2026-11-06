// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class ShareShortcutsTest {
    private val peers = listOf("dev-a" to "Desk", "dev-b" to "Laptop")

    @Test fun oneEntryPerComputerWithOnlyItsName() {
        val p = ShareShortcuts.plan(peers)
        assertEquals(listOf("Desk", "Laptop"), p.map { it.label })
        assertEquals(listOf("send:dev-a", "send:dev-b"), p.map { it.id })
    }

    @Test fun capsBlankNamesAndDuplicates() {
        val many = (1..9).map { "d$it" to "C$it" }
        assertEquals(ShareShortcuts.MAX, ShareShortcuts.plan(many).size)
        assertEquals("Computer", ShareShortcuts.plan(listOf("d" to " ")).single().label)
        assertEquals(1, ShareShortcuts.plan(listOf("d" to "A", "d" to "B")).size)
        assertTrue(ShareShortcuts.plan(listOf("" to "x")).isEmpty())
        assertEquals(48, ShareShortcuts.plan(listOf("d" to "n".repeat(200))).single().label.length)
    }

    @Test fun unpairedOrRenamedComputersAreDropped() {
        val want = ShareShortcuts.plan(listOf("dev-a" to "Renamed"))
        assertEquals(listOf("send:dev-b"), ShareShortcuts.stale(listOf("send:dev-a", "send:dev-b", "other"), want))
        assertEquals(listOf("send:dev-a"), ShareShortcuts.stale(listOf("send:dev-a"), emptyList()))
    }

    @Test fun shortcutIdRoundTrips() {
        assertEquals("dev-a", ShareShortcuts.deviceIdOf(ShareShortcuts.plan(peers)[0].id))
        assertNull(ShareShortcuts.deviceIdOf(null))
        assertNull(ShareShortcuts.deviceIdOf("send:"))
        assertNull(ShareShortcuts.deviceIdOf("something"))
    }

    @Test fun tappedShortcutPicksOnlyItsComputer() {
        val a = ShareTarget("dev-a", "Desk", 7, null)
        val b = ShareTarget("dev-b", "Laptop", 8, null)
        assertSame(b, TargetPicker.forShortcut(listOf(a, b), "send:dev-b"))
        assertNull(TargetPicker.forShortcut(listOf(a), "send:dev-b")) // unpaired since: normal picker
        assertNull(TargetPicker.forShortcut(listOf(a, b), null))
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BoundsTest {
    @Test
    fun utf8SizeMatchesEncoder() {
        val samples = listOf("", "abc", "é", "€", "😀", "a😀€é")
        for (s in samples) {
            assertEquals("for ${s.map { it.code }}", s.toByteArray(Charsets.UTF_8).size, Bounds.utf8Size(s))
        }
    }

    @Test
    fun utf8SizeOverestimatesLoneSurrogates() {
        // Guard only: a lone surrogate counts as 3 bytes (like modified UTF-8), never less than the encoder.
        assertEquals(3, Bounds.utf8Size("\uD83D"))
    }

    @Test
    fun clipCollapsesWhitespaceAndControlChars() {
        assertEquals("a b c", Bounds.clip("  a \n\t b\u0000 c  ", 80))
        assertEquals("", Bounds.clip(null, 10))
        assertEquals("", Bounds.clip("   ", 10))
    }

    @Test
    fun clipBoundsLengthIncludingEllipsis() {
        val out = Bounds.clip("abcdefghij", 5)
        assertEquals("abcd…", out)
        assertEquals(5, out.length)
        assertEquals("abcde", Bounds.clip("abcde", 5))
    }

    @Test
    fun clipDoesNotSplitSurrogatePairs() {
        val s = "ab😀cd" // chars: a b H L c d
        val out = Bounds.clip(s, 4) // max-1 = 3 chars would end between the pair
        assertEquals("ab…", out)
        assertFalse(out.any { Character.isHighSurrogate(it) })
    }

    @Test
    fun clipHandlesHugeInputWithoutScanningItAll() {
        val out = Bounds.clip("x".repeat(10_000_000), 20)
        assertEquals(20, out.length)
        assertTrue(out.endsWith("…"))
    }
}

class PermissionLogicTest {
    @Test
    fun resultClassification() {
        assertEquals(PermissionUi.Granted, PermissionLogic.afterRequest(true, false))
        assertEquals(PermissionUi.Denied, PermissionLogic.afterRequest(false, true))
        assertEquals(PermissionUi.Blocked, PermissionLogic.afterRequest(false, false))
    }

    @Test
    fun currentStateTracksRevocationAndKeepsDenial() {
        assertEquals(PermissionUi.Granted, PermissionLogic.current(true, PermissionUi.NotGranted))
        assertEquals(PermissionUi.NotGranted, PermissionLogic.current(false, PermissionUi.Granted))
        assertEquals(PermissionUi.Blocked, PermissionLogic.current(false, PermissionUi.Blocked))
        assertEquals(PermissionUi.Denied, PermissionLogic.current(false, PermissionUi.Denied))
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.net.URLDecoder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ComputerInviteTest {
    private val s = TestStrings

    @Test fun theMailHasTheSubjectTheReleasesPageAndThreeShortLines() {
        assertEquals("GeekBridge for your computer", ComputerInvite.subject(s))
        val body = ComputerInvite.body(s)
        assertTrue(body.contains("https://github.com/fspecii/geekbridge/releases/latest"))
        val numbered = body.lines().filter { Regex("^[123]\\. ").containsMatchIn(it) }
        assertEquals(3, numbered.size)
        numbered.forEach { assertTrue("short: $it", it.length <= 100) }
    }

    @Test fun theMailContainsNoSecretsNamesOrAddresses() {
        val all = ComputerInvite.subject(s) + "\n" + ComputerInvite.body(s)
        assertFalse(all.contains("geekbridge://"))
        assertFalse(all.contains("token", ignoreCase = true))
        assertFalse(Regex("\\b\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\b").containsMatchIn(all))
        assertFalse(Regex("[0-9a-f]{32}").containsMatchIn(all))
        // The only link in it is the public releases page.
        assertEquals(listOf(ComputerInvite.RELEASES_URL), Regex("https?://\\S+").findAll(all).map { it.value }.toList())
    }

    @Test fun theMailtoVariantIsPercentEncodedAndRoundTrips() {
        val m = ComputerInvite.mailtoText(s)
        assertTrue(m.startsWith("mailto:?subject="))
        assertFalse(m.contains(" "))
        assertFalse(m.contains("\n"))
        val body = URLDecoder.decode(m.substringAfter("&body=").replace("%20", "+"), "UTF-8")
        assertEquals(ComputerInvite.body(s), body)
    }

    @Test fun theShortAddressLeadsToTheSamePage() {
        assertEquals("https://" + ComputerInvite.SHORT_URL, ComputerInvite.RELEASES_URL)
        assertTrue(ComputerInvite.SHORT_URL.length < 50)
    }
}

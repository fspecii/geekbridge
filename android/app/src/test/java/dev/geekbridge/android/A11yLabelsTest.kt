// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class A11yLabelsTest {
    @Test fun dotMatrixTextIsSpokenAsWordsNotLettersOrDots() {
        assertEquals("Online", A11yLabels.spoken("Online"))
        assertEquals("Needs permission", A11yLabels.spoken("  Needs   permission "))
        assertEquals("12:30", A11yLabels.spoken("12:30"))
    }

    @Test fun paddedNumeralsAreSpokenAsNumbers() {
        assertEquals("7", A11yLabels.spoken("07"))
        assertEquals("30", A11yLabels.spoken("30"))
        assertEquals("0", A11yLabels.spoken("00"))
        // Long digit strings (phone numbers, codes) are left alone, digit grouping is the reader's job.
        assertEquals("0123456", A11yLabels.spoken("0123456"))
    }

    @Test fun heroIsOneSentenceListWithTheStateFirst() {
        val d = A11yLabels.hero("Computer link", "Online", "Connected to Work laptop", "Files and clipboard are ready.")
        assertEquals("Computer link: Online. Connected to Work laptop. Files and clipboard are ready.", d)
        assertFalse(d.contains(".."))
    }

    @Test fun heroSkipsEmptyPartsAndDoubledPunctuation() {
        assertEquals("Computer link: Offline. Not connected.", A11yLabels.hero("Computer link", "Offline", "Not connected.", ""))
    }

    @Test fun tileNamesTheStateAndSaysWhenItNeedsYou() {
        assertEquals("Contacts, Ready", A11yLabels.tile("Contacts", "Ready", false, "Needs attention"))
        assertEquals("Calls, Allow. Needs attention", A11yLabels.tile("Calls", "Allow", true, "Needs attention"))
    }

    /** "Other ways to link" and "More call options" show or hide more of the screen: they must say their state. */
    @Test fun showHideButtonsGoThroughTheDisclosureHelper() {
        val dir = java.io.File("src/main/java/dev/geekbridge/android")
        for (file in listOf("PairPanel.kt", "SetupActivity.kt")) {
            val src = java.io.File(dir, file).readText()
            assertTrue("$file builds its show/hide button with kit.disclosure", src.contains("kit.disclosure("))
            val plain = Regex("""kit\.button\([^\n]*(pair_other_ways|setup_calls_more)""")
            assertFalse("$file: a show/hide button without a state", plain.containsMatchIn(src))
        }
        val ui = java.io.File(dir, "UiKit.kt").readText()
        assertTrue(ui.contains("R.string.state_expanded") && ui.contains("R.string.state_collapsed"))
    }

    /** The answer to the battery dialog arrives as a rebuilt page: the "denied" sentence is a polite live region. */
    @Test fun theBatteryDeniedSentenceIsSpokenWithoutMovingFocus() {
        val src = java.io.File("src/main/java/dev/geekbridge/android/SetupActivity.kt").readText()
        val denied = src.lines().first { it.contains("R.string.setup_battery_denied") }
        assertTrue(denied.contains("ACCESSIBILITY_LIVE_REGION_POLITE"))
    }

    /** The Quick Settings tile is spoken with a comma, not the "·" of its visible subtitle. */
    @Test fun theTileDescriptionHasNoMiddleDot() {
        val strings = java.io.File("src/main/res/values/gb_widgets.xml").readText()
        assertTrue(Regex("""tile_link_sub_spoken">[^<·]*</string>""").containsMatchIn(strings))
        val svc = java.io.File("src/main/java/dev/geekbridge/android/LinkTileService.kt").readText()
        assertTrue(svc.contains("getString(R.string.tile_link_a11y, spoken)"))
    }
}

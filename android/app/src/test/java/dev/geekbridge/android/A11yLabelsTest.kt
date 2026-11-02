package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.StringReader
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Messages review 2: a second identical text is its own row; a big contact photo does not cost the contact. */
class MessagesFollowUpTest {
    private fun row(id: Long, body: String, date: Long) =
        SmsRow(id = id, threadId = 1, address = "+15550100", body = body, date = date, type = 1, read = false, subId = 1)

    @Test
    fun aSecondIdenticalTextIsNotLinkedToTheFirstRow() {
        val first = row(10, "OK", 1_000)
        // The second "OK" broadcast arrived, but the default app has not written its row yet.
        assertNull(pickIncoming(listOf(first), "OK", setOf(10L), 0))
        // Once it is written it is the one found.
        val second = row(11, "OK", 2_000)
        assertEquals(second, pickIncoming(listOf(second, first), "OK", setOf(10L), 0))
    }

    @Test
    fun aRowOlderThanTheBroadcastIsNotTheMatch() {
        val old = row(5, "OK", 1_000)
        assertNull(pickIncoming(listOf(old), "OK", emptySet(), 50_000))
        assertEquals(old, pickIncoming(listOf(old), "OK", emptySet(), 900))
        assertNull(pickIncoming(listOf(old), "other", emptySet(), 0))
    }

    @Test
    fun aHugePhotoIsDroppedAndTheFieldsAfterItSurvive() {
        val photo = "A".repeat(2_000_000)
        val card = "BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nPHOTO;ENCODING=b;TYPE=JPEG:$photo\n continued\nTEL:+15550100\nEND:VCARD\n"
        val out = stripVCardPhotos(StringReader(card), 512 * 1024)
        assertTrue(out, out.contains("FN:Ada") && out.contains("TEL:+15550100") && out.trimEnd().endsWith("END:VCARD"))
        assertFalse(out.contains("PHOTO") || out.contains("continued"))
    }

    @Test
    fun aVersion21PhotoWithUnfoldedBase64LinesIsDropped() {
        val card = "BEGIN:VCARD\r\nVERSION:2.1\r\nN:Lee;Ann\r\nPHOTO;ENCODING=BASE64;JPEG:\r\n/9j/4AAQSkZJRg==\r\nAAAA\r\n\r\nTEL:555\r\nEND:VCARD\r\n"
        val out = stripVCardPhotos(StringReader(card), 4096)
        assertEquals("BEGIN:VCARD\nVERSION:2.1\nN:Lee;Ann\nTEL:555\nEND:VCARD\n", out)
    }

    @Test
    fun aCardOverTheCapIsClosedNotCutMidLine() {
        val note = "NOTE:" + "n".repeat(100)
        val card = "BEGIN:VCARD\nFN:Ada\n$note\n$note\nEND:VCARD\n"
        val out = stripVCardPhotos(StringReader(card), 150)
        assertTrue(out, out.startsWith("BEGIN:VCARD\nFN:Ada\n") && out.trimEnd().endsWith("END:VCARD"))
        assertTrue(out.length <= 150 + "END:VCARD\n".length)
    }
}

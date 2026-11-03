// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * MMS is not carried to the computer, but a picture message must never vanish from a thread: the phone lists it as a
 * body-less row marked as MMS, and the computer shows "MMS message - open on phone".
 */
class SmsMmsTest {
    private fun sms(id: Long, thread: Long, date: Long) = SmsRow(id, thread, "+4915112345678", "text $id", date, 1, true, 1)
    private fun mms(id: Long, thread: Long, date: Long) = SmsRow(id, thread, "+4915112345678", "", date, 1, false, 1, mms = true)

    @Test fun anMmsRowGoesOnTheWireWithoutBodyAndWithAnAttachmentMarker() {
        val j = PhoneDataProtocol.smsMessage(mms(7, 5, 1_700_000_000_000))
        assertEquals("", j.getString("body"))
        assertEquals(1, j.getJSONArray("attachments").length())
        assertEquals(7, j.getLong("_id")); assertEquals(5, j.getLong("thread_id")); assertEquals(1_700_000_000_000, j.getLong("date"))
        assertFalse("a plain text carries no attachments array", PhoneDataProtocol.smsMessage(sms(7, 5, 1)).has("attachments"))
    }

    @Test fun anMmsBodyIsNeverReadEvenIfARowCarriesOne() {
        val j = PhoneDataProtocol.smsMessage(mms(1, 1, 1).copy(body = "secret caption"))
        assertEquals("", j.getString("body"))
    }

    @Test fun theNewestOfSmsAndMmsDecidesEachThreadsLastMessage() {
        val out = SmsMerge.latest(
            sms = listOf(sms(10, 1, 500), sms(9, 2, 400)),
            mms = listOf(mms(3, 1, 900), mms(2, 3, 100)),
            max = 10,
        )
        assertEquals(listOf(1L to true, 2L to false, 3L to true), out.map { it.threadId to it.mms })
        assertEquals(900L, out.first().date)
    }

    @Test fun anMmsOnlyThreadIsListed() {
        val out = SmsMerge.latest(sms = emptyList(), mms = listOf(mms(1, 8, 5)), max = 5)
        assertEquals(1, out.size); assertTrue(out.single().mms)
    }

    @Test fun latestHonoursMaxAndKeepsNewestThreads() {
        val s = (1..20L).map { sms(it, it, it * 10) }
        val m = (21..30L).map { mms(it, it, it * 10) }
        val out = SmsMerge.latest(s, m, 5)
        assertEquals(listOf(30L, 29L, 28L, 27L, 26L), out.map { it.threadId })
    }

    @Test fun aThreadPageInterleavesBothKindsNewestFirstAndRespectsTheLimit() {
        val out = SmsMerge.thread(
            sms = listOf(sms(5, 1, 500), sms(4, 1, 300), sms(3, 1, 100)),
            mms = listOf(mms(2, 1, 400), mms(1, 1, 50)),
            limit = 4,
        )
        assertEquals(listOf(500L, 400L, 300L, 100L), out.map { it.date })
        assertEquals(listOf(false, true, false, false), out.map { it.mms })
    }

    @Test fun equalDatesKeepASteadyOrderAndNeverDropAMessage() {
        val out = SmsMerge.thread(listOf(sms(2, 1, 100)), listOf(mms(2, 1, 100)), 10)
        assertEquals(2, out.size)
        assertEquals(out, SmsMerge.thread(listOf(sms(2, 1, 100)), listOf(mms(2, 1, 100)), 10))
    }

    @Test fun unreadCountsAddBothTables() {
        assertEquals(mapOf(1L to 3, 2L to 1), SmsMerge.unread(mapOf(1L to 2), mapOf(1L to 1, 2L to 1)))
    }

    @Test fun mmsDatesAreSecondsOnAndroidAndMillisecondsOnTheWire() {
        assertEquals(1_700_000_000_000, MmsRows.dateMs(1_700_000_000))
        assertEquals(0, MmsRows.dateMs(-5))
        assertEquals(Long.MAX_VALUE / 2, MmsRows.dateMs(Long.MAX_VALUE)) // never overflows
    }

    @Test fun theSenderOfAnMmsIsTheFromAddressAndAnOutgoingOneGoesToTheFirstRecipient() {
        val addrs = listOf("insert-address-token" to 137, "+4915111111111" to 151, "+4915122222222" to 151)
        assertEquals("+4915111111111", MmsRows.pickAddress(addrs, incoming = false))
        val inc = listOf("+4915133333333" to 137, "insert-address-token" to 151)
        assertEquals("+4915133333333", MmsRows.pickAddress(inc, incoming = true))
        assertEquals("", MmsRows.pickAddress(emptyList(), incoming = true))
        assertEquals("", MmsRows.pickAddress(listOf("insert-address-token" to 137), incoming = true))
    }

    // The computer refuses a message with no address, so one such picture message must not hide its whole thread.
    @Test fun aPictureMessageWithoutAnAddressBorrowsTheThreadsTextAddressOrIsLeftOut() {
        val text = sms(1, 5, 1_000)
        val noAddr = mms(2, 5, 2_000).copy(address = "")
        val orphan = mms(3, 9, 3_000).copy(address = "")
        val out = MmsRows.fillAddress(listOf(noAddr, orphan, mms(4, 5, 4_000)), listOf(text))
        assertEquals(listOf(2L, 4L), out.map { it.id })
        assertEquals("+4915112345678", out[0].address)
        assertTrue(out.all { it.address.isNotBlank() })
        assertEquals(listOf(text), MmsRows.fillAddress(listOf(text), emptyList())) // texts are never touched
    }
}

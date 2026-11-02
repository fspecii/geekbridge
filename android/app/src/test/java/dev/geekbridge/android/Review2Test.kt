// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Second code review (Android): regressions for bugs found in the phone-data engine and per-computer grants. */
class Review2Test {
    private class Sender : SmsSender {
        val calls = ArrayList<String>()
        override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) { calls += address }
    }

    private class NoSms : SmsPort {
        override fun latestPerThread(max: Int) = emptyList<SmsRow>()
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
    }

    private class NoCal : CalendarPort {
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = emptyList<CalInstance>()
        override fun calendars() = emptyList<CalendarInfo>()
    }

    private class NoContacts : ContactsPort {
        override fun listUids(max: Int) = emptyList<Pair<String, String>>()
        override fun vcard(uid: String): Pair<String, String>? = null
    }

    @Test fun allowingShortCodesNeverLetsANonNumericDestinationThrough() {
        val sender = Sender()
        val e = PhoneDataEngine({ CoreOutcome.Ok(JSONObject()) }, { ActiveFeatures(sms = true) }, { emptySet() },
            NoContacts(), NoCal(), NoSms(), sender, allowSpecialNumbers = { true })
        for (a in listOf("Bank", "+49abc1234567", "12;34567890")) {
            val body = JSONObject().put("requestId", "r" + a.length).put("messageBody", "hi")
                .put("addresses", JSONArray().put(JSONObject().put("address", a)))
            e.onEvents(listOf(DataEvent(1, 1, PhoneDataKinds.SMS_REQUEST, body)))
        }
        assertEquals(emptyList<String>(), sender.calls)
    }

    @Test fun aComputerMayNotEditACalendarItMayNotRead() {
        val laptop = "a".repeat(32)
        val g = ComputerGrants().set(laptop, GrantFeature.CalendarEdit, true)
        val permits = g.permitsObject()
        assertFalse(permits.getJSONArray("calendar_edit").toString(), permits.getJSONArray("calendar_edit").length() > 0)
        val both = g.set(laptop, GrantFeature.Calendar, true).permitsObject()
        assertTrue(both.getJSONArray("calendar").length() == 1)
    }
}

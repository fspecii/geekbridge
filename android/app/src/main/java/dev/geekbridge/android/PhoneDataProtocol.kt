package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import java.security.MessageDigest

/** Which phone-data features may talk over the link right now (setting on AND Android permission granted). */
data class ActiveFeatures(
    val contacts: Boolean = false,
    val calendar: Boolean = false,
    val sms: Boolean = false,
    /** Computer may create/change/delete events: its own switch AND WRITE_CALENDAR, never without [calendar]. */
    val calendarEdit: Boolean = false,
) {
    val any: Boolean get() = contacts || calendar || sms
}

/** The user's opt-ins. All off by default; [calendarIds] is the read-only calendar selection (default none). */
data class PhoneDataSettings(
    val contacts: Boolean = false,
    val calendar: Boolean = false,
    val sms: Boolean = false,
    val calendarIds: Set<String> = emptySet(),
    /** Allow the computer to text short codes and premium-rate numbers. Off by default; they can cost money. */
    val smsSpecialNumbers: Boolean = false,
    /** "Let the computer edit my calendar" - off by default, asked for separately from reading. */
    val calendarEdit: Boolean = false,
)

data class PhoneDataPermissions(
    val readContacts: Boolean = false,
    val readCalendar: Boolean = false,
    val readSms: Boolean = false,
    val sendSms: Boolean = false,
    val receiveSms: Boolean = false,
    val writeCalendar: Boolean = false,
) {
    val smsComplete: Boolean get() = readSms && sendSms && receiveSms
}

object PhoneDataGate {
    fun active(s: PhoneDataSettings, p: PhoneDataPermissions) = ActiveFeatures(
        contacts = s.contacts && p.readContacts,
        calendar = s.calendar && p.readCalendar,
        sms = s.sms && p.smsComplete,
        calendarEdit = s.calendarEdit && s.calendar && p.readCalendar && p.writeCalendar,
    )
}

/** Packet kinds of docs/phone-data-link.md. */
object PhoneDataKinds {
    const val CONTACTS_REQ_UIDS = "kdeconnect.contacts.request_all_uids_timestamps"
    const val CONTACTS_REQ_VCARDS = "kdeconnect.contacts.request_vcards_by_uid"
    const val CONTACTS_RES_UIDS = "kdeconnect.contacts.response_uids_timestamps"
    const val CONTACTS_RES_VCARDS = "kdeconnect.contacts.response_vcards"
    const val CALENDAR_REQ = "geekbridge.calendar.request"
    const val CALENDAR_SYNC = "geekbridge.calendar.sync"
    const val CALENDAR_WRITE = "geekbridge.calendar.write"
    const val CALENDAR_WRITE_RESULT = "geekbridge.calendar.write_result"
    const val SMS_REQ_CONVERSATIONS = "kdeconnect.sms.request_conversations"
    const val SMS_REQ_CONVERSATION = "kdeconnect.sms.request_conversation"
    const val SMS_REQUEST = "kdeconnect.sms.request"
    const val SMS_MESSAGES = "kdeconnect.sms.messages"
    const val SMS_STATUS = "geekbridge.sms.status"

    val outgoing: Set<String> =
        setOf(CONTACTS_RES_UIDS, CONTACTS_RES_VCARDS, CALENDAR_SYNC, CALENDAR_WRITE_RESULT, SMS_MESSAGES, SMS_STATUS)
}

/** One desktop request relayed by the native core. */
data class DataEvent(val seq: Long, val connectionId: Long, val kind: String, val body: JSONObject)
data class DataWait(val events: List<DataEvent>, val seq: Long, val stopped: Boolean)

object PhoneDataLimits {
    const val MAX_PACKET_BYTES = 900 * 1024
    const val MAX_CONTACTS = 5000
    const val MAX_VCARD_BYTES = 64 * 1024
    const val MAX_VCARD_REQUEST = 25
    const val MAX_EVENTS = 1000
    const val MAX_CALENDARS = 32
    const val MAX_DELETED = 500
    const val MAX_TITLE = 512
    const val MAX_LOCATION = 512
    const val MAX_DESCRIPTION = 2000
    const val MAX_REMINDERS = 5
    const val MAX_ATTENDEE_NAMES = 6
    /** Events whose reminders / attendees are looked up per provider query. */
    const val DETAIL_BATCH = 200
    const val MAX_OPS_REMEMBERED = 200
    const val MAX_SMS_PER_PACKET = 100
    const val MAX_SMS_BODY_CHARS = 20_000
    const val MAX_CONVERSATION_PAGE = 50
    const val WINDOW_PAST_MS = 7L * 86_400_000
    const val WINDOW_FUTURE_MS = 90L * 86_400_000
    const val CALENDAR_PUSH_SPACING_MS = 30_000L
    const val WAIT_MS = 25_000L
    const val SMS_SENDS_PER_MINUTE = 10
    const val SMS_SENDS_PER_HOUR = 60
    const val MAX_SMS_REQUESTS_REMEMBERED = 200
}

object PhoneDataProtocol {
    private val requestId = Regex("[A-Za-z0-9_-]{1,64}")
    private val kind = Regex("[a-z0-9_.]{1,64}")

    private fun base(action: String) = JSONObject().put("version", CoreProtocol.VERSION).put("action", action)

    /** The `data` object of `transport_start`; null when nothing is active so the app declares nothing. */
    fun startObject(a: ActiveFeatures): JSONObject? =
        if (!a.any) null else JSONObject().put("contacts", a.contacts).put("calendar", a.calendar).put("sms", a.sms)
            .put("calendar_edit", a.calendarEdit && a.calendar)

    fun configRequest(a: ActiveFeatures): String =
        base("transport_data_config").put("contacts", a.contacts).put("calendar", a.calendar).put("sms", a.sms)
            .put("calendar_edit", a.calendarEdit && a.calendar).toString()

    fun waitRequest(since: Long, timeoutMs: Long = PhoneDataLimits.WAIT_MS): String =
        base("transport_data_wait").put("since", since)
            .put("timeout_ms", timeoutMs.coerceIn(0, PhoneDataLimits.WAIT_MS)).toString()

    fun sendRequest(kind: String, body: JSONObject): String =
        base("transport_data_send").put("kind", kind).put("body", body).toString()

    fun validRequestId(s: String?): Boolean = s != null && requestId.matches(s)

    fun parseWait(result: Any?): DataWait? {
        val o = result as? JSONObject ?: return null
        val arr = o.optJSONArray("events") ?: return null
        if (arr.length() > 64) return null
        val out = ArrayList<DataEvent>(arr.length())
        for (i in 0 until arr.length()) {
            val e = arr.optJSONObject(i) ?: continue
            val seq = (e.opt("seq") as? Number)?.toLong() ?: continue
            val conn = (e.opt("connection_id") as? Number)?.toLong() ?: 0L
            val k = (e.opt("kind") as? String)?.takeIf { kind.matches(it) } ?: continue
            val body = e.optJSONObject("body") ?: JSONObject()
            out += DataEvent(seq, conn, k, body)
        }
        val seq = (o.opt("seq") as? Number)?.toLong() ?: (o.opt("last_seq") as? Number)?.toLong() ?: 0L
        return DataWait(out, seq, o.optBoolean("stopped", false))
    }

    // ------------------------------------------------------------------ text hygiene

    /** Cuts at [max] chars without splitting a surrogate pair; control characters become spaces. */
    fun clean(raw: String?, max: Int, multiline: Boolean = false): String {
        if (raw == null) return ""
        val head = if (raw.length > max + 1) raw.substring(0, max + 1) else raw
        val sb = StringBuilder(head.length)
        for (ch in head) {
            sb.append(if (Character.isISOControl(ch) && !(multiline && (ch == '\n' || ch == '\t' || ch == '\r'))) ' ' else ch)
        }
        var end = minOf(sb.length, max)
        if (end > 0 && end < sb.length + 1 && Character.isHighSurrogate(sb[end - 1])) end--
        return sb.substring(0, end).trim()
    }

    fun sha(parts: List<String>): String {
        val md = MessageDigest.getInstance("SHA-256")
        for (p in parts) {
            md.update(p.toByteArray(Charsets.UTF_8))
            md.update(0)
        }
        return md.digest().take(8).joinToString("") { "%02x".format(it) }
    }

    // ------------------------------------------------------------------ vCards

    /** Drops PHOTO/LOGO/SOUND properties including their folded (leading-whitespace) continuation lines. */
    fun stripBinaryProperties(vcard: String): String {
        val out = StringBuilder(vcard.length)
        var skipping = false
        for (line in vcard.split("\r\n", "\n")) {
            if (line.isNotEmpty() && (line[0] == ' ' || line[0] == '\t')) {
                if (!skipping) out.append(line).append("\r\n")
                continue
            }
            val name = line.substringBefore(':').substringBefore(';').uppercase()
            skipping = name == "PHOTO" || name == "LOGO" || name == "SOUND" || name.endsWith(".PHOTO")
            if (!skipping && line.isNotEmpty()) out.append(line).append("\r\n")
        }
        return out.toString()
    }

    /** vCard as the desktop expects: no binary properties, one trailing `REV:<timestamp>` before END:VCARD. */
    fun finishVcard(vcard: String, timestamp: String): String? {
        val stripped = stripBinaryProperties(vcard).lines().filterNot { it.startsWith("REV:", ignoreCase = true) }
            .joinToString("\r\n").trim()
        val endAt = stripped.lastIndexOf("END:VCARD", ignoreCase = true)
        if (!stripped.startsWith("BEGIN:VCARD", ignoreCase = true) || endAt < 0) return null
        val out = stripped.substring(0, endAt).trimEnd() + "\r\nREV:" + clean(timestamp, 40) + "\r\nEND:VCARD\r\n"
        return out.takeIf { Bounds.utf8Size(it) <= PhoneDataLimits.MAX_VCARD_BYTES }
    }

    // ------------------------------------------------------------------ contacts

    fun contactsUids(entries: List<Pair<String, String>>): JSONObject {
        val uids = JSONArray()
        val o = JSONObject()
        for ((uid, ts) in entries.asSequence().filter { it.first != "uids" && it.first.isNotEmpty() && it.first.length <= 512 }
            .distinctBy { it.first }.take(PhoneDataLimits.MAX_CONTACTS)) {
            uids.put(uid)
            o.put(uid, ts)
        }
        return o.put("uids", uids)
    }

    /** `request_vcards_by_uid` body -> at most 25 distinct uids; garbage is ignored. */
    fun requestedUids(body: JSONObject): List<String> {
        val a = body.optJSONArray("uids") ?: return emptyList()
        val out = LinkedHashSet<String>()
        for (i in 0 until minOf(a.length(), 200)) {
            val u = a.opt(i) as? String ?: continue
            if (u.isNotEmpty() && u.length <= 512 && u != "uids") out += u
            if (out.size >= PhoneDataLimits.MAX_VCARD_REQUEST) break
        }
        return out.toList()
    }

    // ------------------------------------------------------------------ calendar

    fun calendarEvent(i: CalInstance): JSONObject {
        val title = clean(i.title, PhoneDataLimits.MAX_TITLE)
        val location = clean(i.location, PhoneDataLimits.MAX_LOCATION)
        val description = clean(i.description, PhoneDataLimits.MAX_DESCRIPTION, multiline = true)
        val tz = if (i.allDay) "UTC" else clean(i.timeZone, 64).ifEmpty { "UTC" }
        val id = "${i.eventId}@${i.begin}"
        val reminders = i.reminders.filter { it in 0..40_320 }.distinct().sorted().take(PhoneDataLimits.MAX_REMINDERS)
        val names = i.attendees.map { clean(it, 128) }.filter { it.isNotEmpty() }.take(PhoneDataLimits.MAX_ATTENDEE_NAMES)
        val rev = sha(
            listOf(
                title, i.allDay.toString(), i.begin.toString(), i.end.toString(), tz, location, description, i.calendarId,
                i.recurring.toString(), reminders.joinToString(","), i.attendeeCount.toString(), names.joinToString("|"),
            ),
        )
        val o = JSONObject().put("id", id).put("rev", rev).put("title", title).put("allDay", i.allDay)
            .put("startMs", i.begin).put("endMs", i.end).put("tz", tz).put("calendarId", i.calendarId)
            .put("eventId", i.eventId).put("recurring", i.recurring)
        if (reminders.isNotEmpty()) o.put("reminders", JSONArray(reminders))
        if (i.attendeeCount > 0) {
            o.put("attendees", JSONObject().put("count", i.attendeeCount.coerceAtMost(10_000)).put("names", JSONArray(names)))
        }
        if (location.isNotEmpty()) o.put("location", location)
        if (description.isNotEmpty()) o.put("description", description)
        return o
    }

    // ------------------------------------------------------------------ SMS

    fun smsMessage(r: SmsRow): JSONObject = JSONObject()
        .put("addresses", JSONArray().put(JSONObject().put("address", clean(r.address, 64))))
        .put("body", clean(r.body, PhoneDataLimits.MAX_SMS_BODY_CHARS, multiline = true))
        .put("date", r.date).put("type", r.type).put("read", if (r.read) 1 else 0)
        .put("thread_id", r.threadId).put("_id", r.id).put("sub_id", r.subId).put("event", 1)

    fun smsStatus(requestId: String, state: String, reason: String? = null): JSONObject =
        JSONObject().put("requestId", requestId).put("state", state).also { if (reason != null) it.put("reason", reason) }

    /** A validated outgoing text request, or the failure reason code for `geekbridge.sms.status`. */
    sealed interface SmsParse {
        data class Ok(val requestId: String?, val address: String, val text: String, val subId: Int?) : SmsParse
        data class Bad(val requestId: String?, val reason: String) : SmsParse
    }

    fun parseSmsRequest(body: JSONObject): SmsParse {
        val rid = (body.opt("requestId") as? String)?.takeIf { validRequestId(it) }
        val addresses = LinkedHashSet<String>()
        val arr = body.optJSONArray("addresses")
        if (arr != null) {
            for (i in 0 until minOf(arr.length(), 8)) {
                val a = arr.optJSONObject(i)?.optString("address", "")?.trim().orEmpty()
                if (a.isNotEmpty()) addresses += a
            }
        } else {
            (body.opt("phoneNumber") as? String)?.trim()?.takeIf { it.isNotEmpty() }?.let { addresses += it }
        }
        if (addresses.size != 1) return SmsParse.Bad(rid, if (addresses.isEmpty()) "invalid" else "group_unsupported")
        val address = addresses.first()
        if (address.length > Limits.SMS_MAX_ADDRESS_CHARS || address.any { Character.isISOControl(it) }) return SmsParse.Bad(rid, "invalid")
        val attachments = body.optJSONArray("attachments")
        if (attachments != null && attachments.length() > 0) return SmsParse.Bad(rid, "mms_unsupported")
        val text = body.optString("messageBody", "")
        if (text.isBlank() || text.codePointCount(0, text.length) > Limits.SMS_MAX_TEXT_CHARS) return SmsParse.Bad(rid, "invalid")
        if (text.any { Character.isISOControl(it) && it != '\n' && it != '\r' && it != '\t' }) return SmsParse.Bad(rid, "invalid")
        val sub = (body.opt("subID") as? Number)?.toLong()?.takeIf { it in 1 until Int.MAX_VALUE }?.toInt()
        return SmsParse.Ok(rid, address, text, sub)
    }

    /** Android `Telephony.Sms.Sent` result codes -> the short reason vocabulary of the contract. */
    fun failureReason(resultCode: Int): String = when (resultCode) {
        // SmsManager.RESULT_ERROR_GENERIC_FAILURE=1, RADIO_OFF=2, NULL_PDU=3, NO_SERVICE=4, LIMIT_EXCEEDED=5 (+ newer 6..)
        1 -> "generic"
        2 -> "radio_off"
        4 -> "no_service"
        5 -> "limit"
        else -> "generic"
    }
}

data class CalInstance(
    val eventId: String,
    val begin: Long,
    val end: Long,
    val title: String?,
    val allDay: Boolean,
    val timeZone: String?,
    val location: String?,
    val description: String?,
    val calendarId: String,
    /** The event is part of a series (or an exception of one): shown, never editable from the computer. */
    val recurring: Boolean = false,
    /** Minutes before the start. */
    val reminders: List<Int> = emptyList(),
    val attendeeCount: Int = 0,
    val attendees: List<String> = emptyList(),
)

data class CalendarInfo(
    val id: String,
    val name: String,
    /** Account the calendar syncs with (a Google address for Google Calendar). */
    val account: String? = null,
    /** `com.google` for Google accounts. */
    val accountType: String? = null,
    val color: Int? = null,
    /** Contributor access or better: Android lets this app add events here. */
    val writable: Boolean = false,
)

data class SmsRow(
    val id: Long,
    val threadId: Long,
    val address: String,
    val body: String,
    val date: Long,
    val type: Int,
    val read: Boolean,
    val subId: Int,
)

/**
 * Where a text from the computer may go. Short codes and premium-rate ranges can bill the owner of the SIM, so they are
 * refused unless the user allowed them. A prefix list cannot be complete (Android's own premium-SMS prompt stays the
 * last barrier), but it covers the common ranges and every number too short to be a normal subscriber number.
 */
object SmsDestination {
    private const val MIN_DIGITS = 7
    private val premiumInternational = listOf(
        "+1900", "+1976", "+449", "+4470", "+43900", "+43930", "+43939", "+49900", "+49137", "+49138",
        "+3389", "+33 89".replace(" ", ""), "+34803", "+34806", "+34807", "+41900", "+41901", "+41906", "+3190", "+3191",
    )
    private val premiumNational = listOf("0900", "0901", "0137", "0138", "1900", "1976")

    enum class Verdict { Ok, Invalid, ShortCode, Premium }

    fun check(address: String): Verdict {
        val s = address.filter { !it.isWhitespace() && it != '(' && it != ')' && it != '-' && it != '.' }
        val plus = s.startsWith("+")
        val digits = if (plus) s.drop(1) else s
        if (digits.isEmpty() || !digits.all { it in '0'..'9' }) return Verdict.Invalid
        if (digits.length < MIN_DIGITS) return Verdict.ShortCode
        val international = if (plus) s else if (s.startsWith("00")) "+" + s.drop(2) else null
        if (international != null && premiumInternational.any { international.startsWith(it) }) return Verdict.Premium
        if (international == null && premiumNational.any { s.startsWith(it) }) return Verdict.Premium
        return Verdict.Ok
    }
}

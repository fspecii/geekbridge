package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import kotlin.math.abs

/** Contacts provider behind a small interface (read-only). Null = the provider could not be read. */
interface ContactsPort {
    /** (LOOKUP_KEY, CONTACT_LAST_UPDATED_TIMESTAMP) pairs, at most [max]. */
    fun listUids(max: Int): List<Pair<String, String>>?
    /** Raw system vCard for [uid] and its timestamp, or null if it vanished. */
    fun vcard(uid: String): Pair<String, String>?
}

interface CalendarPort {
    fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int): List<CalInstance>?
    fun calendars(): List<CalendarInfo>
}

interface SmsPort {
    /** Newest message of each thread, newest thread first. */
    fun latestPerThread(max: Int): List<SmsRow>
    /** Messages of one thread with `date <= rangeStartMs` (inclusive, null = newest), newest first, at most [limit]. */
    fun thread(threadId: Long, rangeStartMs: Long?, limit: Int): List<SmsRow>
}

/** Sends one text. Statuses arrive on [listener], possibly later and from another thread. */
interface SmsSender {
    fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener)
}

interface SmsStatusListener {
    fun onStatus(state: String, reason: String? = null)
}

/**
 * Answers the computer's contacts / calendar / SMS requests (docs/phone-data-link.md). Pure Kotlin: Android
 * providers, the clock and the native core are injected. A feature that is not [features] active is never
 * answered and never read. Content (names, event text, message bodies) is never logged. All entry points are
 * synchronized and must run off the main thread.
 */
class PhoneDataEngine(
    private val core: (String) -> CoreOutcome,
    private val features: () -> ActiveFeatures,
    private val calendarIds: () -> Set<String>,
    private val contacts: ContactsPort,
    private val calendar: CalendarPort,
    private val sms: SmsPort,
    private val sender: SmsSender,
    private val clock: () -> Long = System::currentTimeMillis,
    private val bootId: String = java.lang.Long.toString(System.nanoTime() and 0xffffff, 36),
) {
    // calendar sync state for the current link
    private var calToken: String? = null
    private var calCounter = 0
    private var calSent: Map<String, String> = emptyMap()
    private var calLastPush = Long.MIN_VALUE / 2

    private var smsRequested = false
    private val smsSendTimes = ArrayDeque<Long>()
    private var backoffUntil = 0L

    /** Link went down or features changed: the next link starts from scratch. */
    @Synchronized
    fun reset() {
        calToken = null
        calCounter = 0
        calSent = emptyMap()
        smsRequested = false
        smsSendTimes.clear()
    }

    @Synchronized
    fun onEvents(events: List<DataEvent>) {
        for (e in events) {
            try {
                handle(e)
            } catch (_: RuntimeException) {
                // One bad request (or a revoked permission mid-query) must not stop the others.
            }
        }
    }

    private fun handle(e: DataEvent) {
        val f = features()
        when (e.kind) {
            PhoneDataKinds.CONTACTS_REQ_UIDS -> if (f.contacts) contactsUids()
            PhoneDataKinds.CONTACTS_REQ_VCARDS -> if (f.contacts) contactsVcards(e.body)
            PhoneDataKinds.CALENDAR_REQ -> if (f.calendar) calendarRequest(e.body)
            PhoneDataKinds.SMS_REQ_CONVERSATIONS -> if (f.sms) {
                smsRequested = true
                pushSms(sms.latestPerThread(PhoneDataLimits.MAX_SMS_PER_PACKET * 2))
            }
            PhoneDataKinds.SMS_REQ_CONVERSATION -> if (f.sms) smsConversation(e.body)
            PhoneDataKinds.SMS_REQUEST -> if (f.sms) smsRequest(e.body)
        }
    }

    // ------------------------------------------------------------------ transport

    /** True if the core accepted the packet. Rate-limit rejections back off for 10 s and drop. */
    private fun emit(kind: String, body: JSONObject): Boolean {
        if (kind !in PhoneDataKinds.outgoing) return false
        val now = clock()
        if (now < backoffUntil) return false
        val request = PhoneDataProtocol.sendRequest(kind, body)
        if (Bounds.utf8Size(request) > PhoneDataLimits.MAX_PACKET_BYTES + 512) return false
        return when (val o = core(request)) {
            is CoreOutcome.Ok -> true
            is CoreOutcome.Rejected -> {
                if (o.code == "rate_limited") backoffUntil = now + 10_000
                false
            }
            else -> false
        }
    }

    private fun size(o: JSONObject) = Bounds.utf8Size(o.toString())

    // ------------------------------------------------------------------ contacts

    private fun contactsUids() {
        val list = contacts.listUids(PhoneDataLimits.MAX_CONTACTS) ?: return
        emit(PhoneDataKinds.CONTACTS_RES_UIDS, PhoneDataProtocol.contactsUids(list))
    }

    private fun contactsVcards(body: JSONObject) {
        val uids = JSONArray()
        val out = JSONObject()
        var total = 0
        for (uid in PhoneDataProtocol.requestedUids(body)) {
            val (raw, ts) = contacts.vcard(uid) ?: continue
            val card = PhoneDataProtocol.finishVcard(raw, ts) ?: continue
            val cost = Bounds.utf8Size(card) + uid.length * 2 + 16
            if (total + cost > PhoneDataLimits.MAX_PACKET_BYTES) break
            total += cost
            uids.put(uid)
            out.put(uid, card)
        }
        out.put("uids", uids)
        emit(PhoneDataKinds.CONTACTS_RES_VCARDS, out)
    }

    // ------------------------------------------------------------------ calendar

    private fun calendarRequest(body: JSONObject) {
        val since = (body.opt("since") as? String)?.takeIf { it.isNotEmpty() }
        val rid = (body.opt("requestId") as? String)?.takeIf { PhoneDataProtocol.validRequestId(it) }
        pushCalendar(since, rid)
    }

    /** Unsolicited update from the provider observer. False if skipped (nothing requested yet, or too soon). */
    @Synchronized
    fun onCalendarChanged(): Boolean {
        if (!features().calendar) return false
        val token = calToken ?: return false
        val now = clock()
        if (now - calLastPush < PhoneDataLimits.CALENDAR_PUSH_SPACING_MS) return false
        return pushCalendar(token, null)
    }

    private fun pushCalendar(since: String?, requestId: String?): Boolean {
        val now = clock()
        val ids = calendarIds()
        val all = calendar.calendars()
        val chosen = all.filter { it.id in ids }.take(PhoneDataLimits.MAX_CALENDARS)
        val from = now - PhoneDataLimits.WINDOW_PAST_MS
        val to = now + PhoneDataLimits.WINDOW_FUTURE_MS
        val raw = if (chosen.isEmpty()) emptyList() else
            calendar.instances(chosen.map { it.id }.toSet(), from, to, PhoneDataLimits.MAX_EVENTS * 2) ?: return false
        val truncated0 = raw.size > PhoneDataLimits.MAX_EVENTS
        // Nearest first so the cut drops the far future / far past.
        var current = raw.map { PhoneDataProtocol.calendarEvent(it) }
            .sortedBy { abs(it.getLong("startMs") - now) }
            .take(PhoneDataLimits.MAX_EVENTS)
        var truncated = truncated0

        var full = since == null || since != calToken || calToken == null
        var deleted = emptyList<String>()
        if (!full) {
            val currentIds = current.map { it.getString("id") }.toSet()
            deleted = calSent.keys.filter { it !in currentIds }
            if (deleted.size > PhoneDataLimits.MAX_DELETED) {
                full = true
                deleted = emptyList()
            }
        }
        fun build(events: List<JSONObject>): JSONObject {
            val token = "$bootId:${calCounter + 1}"
            val o = JSONObject().put("version", 1).put("full", full)
                .put("base", if (full) JSONObject.NULL else calToken)
                .put("token", token).put("windowStartMs", from).put("windowEndMs", to)
                .put("truncated", truncated)
            if (requestId != null) o.put("requestId", requestId)
            o.put("calendars", JSONArray().also { a ->
                chosen.forEach { c -> a.put(JSONObject().put("id", c.id).put("name", PhoneDataProtocol.clean(c.name, 128))) }
            })
            val send = if (full) events else events.filter { calSent[it.getString("id")] != it.getString("rev") }
            o.put("events", JSONArray(send))
            o.put("deleted", JSONArray().also { a -> deleted.forEach { a.put(JSONObject().put("id", it)) } })
            return o
        }
        var packet = build(current)
        while (size(packet) > PhoneDataLimits.MAX_PACKET_BYTES && current.size > 1) {
            current = current.take(current.size / 2)
            truncated = true
            packet = build(current)
        }
        if (size(packet) > PhoneDataLimits.MAX_PACKET_BYTES) return false
        val ok = emit(PhoneDataKinds.CALENDAR_SYNC, packet)
        if (ok) {
            calToken = packet.getString("token")
            calCounter++
            calSent = current.associate { it.getString("id") to it.getString("rev") }
            calLastPush = now
        }
        return ok
    }

    // ------------------------------------------------------------------ SMS

    private fun smsConversation(body: JSONObject) {
        val thread = (body.opt("threadID") as? Number)?.toLong()?.takeIf { it >= 0 } ?: return
        val start = (body.opt("rangeStartTimestamp") as? Number)?.toLong()?.takeIf { it >= 0 }
        val n = (body.opt("numberToRequest") as? Number)?.toInt()?.takeIf { it > 0 } ?: PhoneDataLimits.MAX_CONVERSATION_PAGE
        smsRequested = true
        pushSms(sms.thread(thread, start, minOf(n, PhoneDataLimits.MAX_CONVERSATION_PAGE)))
    }

    /** Splits [rows] into packets of at most 100 messages / 900 KiB. An empty page still answers (end of history). */
    private fun pushSms(rows: List<SmsRow>) {
        var batch = JSONArray()
        var bytes = 0
        fun flush() {
            emit(PhoneDataKinds.SMS_MESSAGES, JSONObject().put("version", 2).put("messages", batch))
            batch = JSONArray()
            bytes = 0
        }
        for (r in rows) {
            val m = PhoneDataProtocol.smsMessage(r)
            val cost = Bounds.utf8Size(m.toString())
            if (batch.length() >= PhoneDataLimits.MAX_SMS_PER_PACKET || bytes + cost > PhoneDataLimits.MAX_PACKET_BYTES) flush()
            batch.put(m)
            bytes += cost
        }
        if (batch.length() > 0 || rows.isEmpty()) flush()
    }

    /** A message that just arrived. Pushed only if the computer already asked for messages on this link. */
    @Synchronized
    fun onIncomingSms(row: SmsRow) {
        if (!features().sms || !smsRequested) return
        pushSms(listOf(row))
    }

    private fun status(requestId: String?, state: String, reason: String? = null) {
        if (requestId != null) emit(PhoneDataKinds.SMS_STATUS, PhoneDataProtocol.smsStatus(requestId, state, reason))
    }

    private fun smsRequest(body: JSONObject) {
        when (val p = PhoneDataProtocol.parseSmsRequest(body)) {
            is PhoneDataProtocol.SmsParse.Bad -> status(p.requestId, "failed", p.reason)
            is PhoneDataProtocol.SmsParse.Ok -> {
                val now = clock()
                while (smsSendTimes.isNotEmpty() && now - smsSendTimes.first() > 60_000) smsSendTimes.removeFirst()
                if (smsSendTimes.size >= PhoneDataLimits.SMS_SENDS_PER_MINUTE) return status(p.requestId, "failed", "limit")
                smsSendTimes.addLast(now)
                try {
                    sender.send(p.address, p.text, p.subId, object : SmsStatusListener {
                        override fun onStatus(state: String, reason: String?) {
                            synchronized(this@PhoneDataEngine) { status(p.requestId, state, reason) }
                        }
                    })
                } catch (_: SecurityException) {
                    status(p.requestId, "failed", "no_permission")
                } catch (_: RuntimeException) {
                    status(p.requestId, "failed", "generic")
                }
            }
        }
    }
}

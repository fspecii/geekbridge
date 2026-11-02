// SPDX-License-Identifier: GPL-3.0-or-later
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
    /**
     * Up to [max] instances on each side of [pivotMs] inside the window, nearest the pivot first on both sides
     * (the provider's own order is by start, which would hand back the far past when the window holds more than
     * [max]). Ports that cannot do better fall back to [instances].
     */
    fun instancesAround(calendarIds: Set<String>, fromMs: Long, toMs: Long, pivotMs: Long, max: Int): List<CalInstance>? =
        instances(calendarIds, fromMs, toMs, max)
    fun calendars(): List<CalendarInfo>
}

/** One active SIM subscription, reported to the computer so it can offer a choice. */
data class SimInfo(val id: Int, val slot: Int, val label: String)

interface SmsPort {
    /** Active SIMs (empty = unknown or no permission). Only these ids may be chosen by the computer. */
    fun subscriptions(): List<SimInfo> = emptyList()
    /** Unread incoming messages per thread id (empty = unknown); sent with the conversation list. */
    fun unreadCounts(): Map<Long, Int> = emptyMap()
    /** Newest message of each thread, newest thread first. */
    fun latestPerThread(max: Int): List<SmsRow>
    /** How many threads the phone holds (at least; a bounded scan), or null if unknown. Sent so the computer can say "newest N of M". */
    fun threadCount(): Int? = null
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

private const val RATE_RETRIES = 25
private const val RATE_RETRY_MS = 120L

/** Paging rule for "newest message of each thread", independent of Android so it is unit-tested at 20 000 rows. */
object SmsPaging {
    /**
     * [page] returns up to `limit` rows strictly older than the (date, id) cursor, newest first. Stops at [max] threads,
     * an empty page, or [PhoneDataLimits.SMS_SCAN_ROWS] rows scanned.
     */
    fun latestPerThread(max: Int, page: (date: Long, id: Long, limit: Int) -> List<SmsRow>): List<SmsRow> {
        val seen = HashSet<Long>()
        val out = ArrayList<SmsRow>()
        var scanned = 0
        var lastDate = Long.MAX_VALUE
        var lastId = Long.MAX_VALUE
        while (out.size < max && scanned < PhoneDataLimits.SMS_SCAN_ROWS) {
            val rows = page(lastDate, lastId, PhoneDataLimits.SMS_SCAN_CHUNK)
            if (rows.isEmpty()) break
            for (r in rows) if (out.size < max && seen.add(r.threadId)) out += r
            scanned += rows.size
            lastDate = rows.last().date
            lastId = rows.last().id
        }
        return out
    }
}

/** Raised inside a provider read when the link went away: the answer would have nowhere to go. */
class QueryCancelled : RuntimeException("cancelled")

/**
 * One switch shared by the engine and the Android ports. [cancel] fires the provider's own CancellationSignal (so a
 * long query stops inside the provider) and makes every later [check] throw until [rearm]. Thread safe.
 */
class QueryCancel {
    @Volatile var cancelled = false
        private set
    @Volatile private var hook: (() -> Unit)? = null

    fun cancel() {
        cancelled = true
        hook?.invoke()
    }

    fun rearm() {
        cancelled = false
    }

    fun check() {
        if (cancelled) throw QueryCancelled()
    }

    /** Runs [body] with [onCancel] armed (fired at once if already cancelled). */
    fun <T> guard(onCancel: () -> Unit, body: () -> T): T {
        hook = onCancel
        try {
            if (cancelled) onCancel()
            return body()
        } finally {
            hook = null
        }
    }
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
    private val calendarWriter: CalendarWriter? = null,
    private val allowSpecialNumbers: () -> Boolean = { false },
    private val smsLedger: RequestLedger = RequestLedger(),
    private val cancel: QueryCancel = QueryCancel(),
    /** Waits between retries of a rate-limited packet; injected so tests do not sleep. */
    private val pause: (Long) -> Unit = { Thread.sleep(it) },
    /** Answers a computer's `kdeconnect.battery.request`; null = the phone has no battery reporting. */
    private val battery: BatteryReporter? = null,
) {
    // calendar sync state for the current link
    private var calToken: String? = null
    private var calCounter = 0
    private var calSent: Map<String, String> = emptyMap()
    private var calLastPush = Long.MIN_VALUE / 2

    private var smsRequested = false
    private val smsSendTimes = ArrayDeque<Long>()
    private val smsHourTimes = ArrayDeque<Long>()
    private var backoffUntil = 0L

    /**
     * Link went down: stop whatever provider read is running. Not synchronized on purpose, it must get past a long
     * read that holds the engine lock; [reset] re-arms once it has the lock.
     */
    fun cancelInFlight() = cancel.cancel()

    /** Link went down or features changed: the next link starts from scratch. */
    @Synchronized
    fun reset() {
        cancel.rearm()
        calToken = null
        calCounter = 0
        calSent = emptyMap()
        smsRequested = false
        smsSendTimes.clear()
        smsHourTimes.clear() // the request-id ledger is kept on purpose: a replay on the next link must not send again
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
            PhoneDataKinds.CALENDAR_WRITE -> if (f.calendar && f.calendarEdit) calendarWrite(e.body)
            PhoneDataKinds.SMS_REQ_CONVERSATIONS -> if (f.sms) {
                smsRequested = true
                pushSms(sms.latestPerThread(PhoneDataLimits.SMS_THREADS_LISTED), withUnread = true)
            }
            PhoneDataKinds.SMS_REQ_CONVERSATION -> if (f.sms) smsConversation(e.body)
            PhoneDataKinds.SMS_REQUEST -> if (f.sms) smsRequest(e.body)
            PhoneDataKinds.BATTERY_REQ -> if (f.battery) battery?.sendNow(e.connectionId)
            else -> if (e.kind.startsWith("geekbridge.debug.")) DebugHooks.onDataEvent?.invoke(e)
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
        // A rate-limited answer is retried for a few seconds (the core refills 8 packets a second): dropping it
        // would leave the computer waiting for a reply that never comes. Only after that does it back off.
        var tries = 0
        while (true) {
            when (val o = core(request)) {
                is CoreOutcome.Ok -> return true
                is CoreOutcome.Rejected -> {
                    if (o.code != "rate_limited") return false
                    if (++tries > RATE_RETRIES) {
                        backoffUntil = now + 10_000
                        return false
                    }
                    cancel.check()
                    pause(RATE_RETRY_MS)
                }
                else -> return false
            }
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
            // One more than the cap per side, so a side that holds more than the cap shows up as truncated.
            calendar.instancesAround(chosen.map { it.id }.toSet(), from, to, now, PhoneDataLimits.MAX_EVENTS + 1) ?: return false
        // One row per (calendar, event, begin): the same instance twice must not eat the cap or be sent twice.
        val unique = raw.distinctBy { Triple(it.calendarId, it.eventId, it.begin) }
        val truncated0 = unique.size > PhoneDataLimits.MAX_EVENTS
        // Nearest first so the cut drops the far future / far past.
        var current = unique.map { PhoneDataProtocol.calendarEvent(it) }
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
                chosen.forEach { c ->
                    val j = JSONObject().put("id", c.id).put("name", PhoneDataProtocol.clean(c.name, 128)).put("writable", c.writable)
                    PhoneDataProtocol.clean(c.account, 128).takeIf { it.isNotEmpty() }?.let { j.put("account", it) }
                    PhoneDataProtocol.clean(c.accountType, 64).takeIf { it.isNotEmpty() }?.let { j.put("accountType", it) }
                    c.color?.let { j.put("color", it) }
                    a.put(j)
                }
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

    // ------------------------------------------------------------------ calendar writes

    /**
     * One create / update / delete from the computer. Only runs with the edit switch on and WRITE_CALENDAR granted
     * (the caller checked [features]); every operation id is answered, applied at most once, with the honest state.
     */
    private fun calendarWrite(body: JSONObject) {
        val writer = calendarWriter
        when (val p = CalendarWriteProtocol.parse(body)) {
            is WriteParse.Bad -> p.opId?.let { emitResult(it, WriteOutcome("failed", p.reason), false) }
            is WriteParse.Ok -> {
                val (outcome, duplicate) = writer?.handle(p.request) ?: (WriteOutcome("failed", "disabled") to false)
                emitResult(p.request.opId, outcome, duplicate)
                // The provider changed: let the computer see it without waiting for the observer's debounce.
                if (outcome.state == "applied" && !duplicate) pushCalendar(calToken, null)
            }
        }
    }

    private fun emitResult(opId: String, o: WriteOutcome, duplicate: Boolean) {
        emit(PhoneDataKinds.CALENDAR_WRITE_RESULT, CalendarWriteProtocol.result(opId, o, duplicate))
    }

    // ------------------------------------------------------------------ SMS

    private fun smsConversation(body: JSONObject) {
        val thread = (body.opt("threadID") as? Number)?.toLong()?.takeIf { it >= 0 } ?: return
        val start = (body.opt("rangeStartTimestamp") as? Number)?.toLong()?.takeIf { it >= 0 }
        val n = (body.opt("numberToRequest") as? Number)?.toInt()?.takeIf { it > 0 } ?: PhoneDataLimits.MAX_CONVERSATION_PAGE
        smsRequested = true
        pushSms(sms.thread(thread, start, minOf(n, PhoneDataLimits.MAX_CONVERSATION_PAGE)))
    }

    /** The SIM list rides on every message packet (a GeekBridge extension; stock desktops ignore it). */
    private fun addSims(packet: JSONObject) {
        val sims = PhoneDataProtocol.simsJson(sms.subscriptions())
        if (sims.length() > 0) packet.put("sims", sims)
    }

    /** Splits [rows] into packets of at most 100 messages / 900 KiB. An empty page still answers (end of history). */
    private fun pushSms(rows: List<SmsRow>, withUnread: Boolean = false) {
        var batch = JSONArray()
        var bytes = 0
        var unread: JSONObject? = if (withUnread) PhoneDataProtocol.unreadJson(sms.unreadCounts()) else null
        var threads: Int? = if (withUnread) sms.threadCount() else null
        fun flush() {
            emit(PhoneDataKinds.SMS_MESSAGES, JSONObject().put("version", 2).put("messages", batch).also {
                addSims(it)
                unread?.let { u -> it.put("unread", u) }
                threads?.let { n -> it.put("threadsTotal", n) }
                unread = null // once per list, on its first packet
                threads = null
            })
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

    /** Re-push the newest message of every thread (when the computer asked for messages on this link). */
    @Synchronized
    fun refreshConversations() {
        if (!features().sms || !smsRequested) return
        pushSms(sms.latestPerThread(PhoneDataLimits.SMS_THREADS_LISTED), withUnread = true)
    }

    private fun status(requestId: String?, state: String, reason: String? = null) {
        if (requestId != null) emit(PhoneDataKinds.SMS_STATUS, PhoneDataProtocol.smsStatus(requestId, state, reason))
    }

    private fun smsRequest(body: JSONObject) {
        when (val p = PhoneDataProtocol.parseSmsRequest(body)) {
            is PhoneDataProtocol.SmsParse.Bad -> status(p.requestId, "failed", p.reason)
            is PhoneDataProtocol.SmsParse.Ok -> {
                // A repeated request id is the same send: never text twice (its first status is already on its way).
                if (p.requestId != null && !smsLedger.firstTime(p.requestId)) return
                val verdict = SmsDestination.check(p.address)
                // "Allow short codes" never covers a destination that is not a number at all.
                if (verdict != SmsDestination.Verdict.Ok && (!allowSpecialNumbers() || verdict == SmsDestination.Verdict.Invalid)) {
                    return status(p.requestId, "failed", "blocked_number")
                }
                val now = clock()
                while (smsSendTimes.isNotEmpty() && now - smsSendTimes.first() > 60_000) smsSendTimes.removeFirst()
                while (smsHourTimes.isNotEmpty() && now - smsHourTimes.first() > 3_600_000) smsHourTimes.removeFirst()
                if (smsSendTimes.size >= PhoneDataLimits.SMS_SENDS_PER_MINUTE || smsHourTimes.size >= PhoneDataLimits.SMS_SENDS_PER_HOUR) {
                    return status(p.requestId, "failed", "limit")
                }
                // A SIM the phone did not list would silently fall back to another one: refuse instead.
                if (p.subId != null && sms.subscriptions().none { it.id == p.subId }) {
                    return status(p.requestId, "failed", "unknown_sim")
                }
                smsSendTimes.addLast(now)
                smsHourTimes.addLast(now)
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

/** Remembers the last request ids so a repeated packet is recognised. Ids only, never text; optionally restart-safe. */
class RequestLedger(private val blob: BlobStore? = null, private val max: Int = PhoneDataLimits.MAX_SMS_REQUESTS_REMEMBERED) {
    private val ids = LinkedHashSet<String>()

    init {
        blob?.read().orEmpty().split('\n').filter { PhoneDataProtocol.validRequestId(it) }.takeLast(max).forEach { ids += it }
    }

    /** True the first time [id] is seen (and records it); false for a repeat. */
    @Synchronized
    fun firstTime(id: String): Boolean {
        if (!ids.add(id)) return false
        while (ids.size > max) ids.remove(ids.first())
        blob?.write(ids.joinToString("\n"))
        return true
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/**
 * The computer's request to change this phone's calendar (`geekbridge.calendar.write`, docs/phone-data-link.md).
 *
 * Everything here is pure Kotlin so it is unit-tested on the JVM; the Android Calendar Provider sits behind
 * [CalendarWriteStore]. Nothing is written unless the user switched "Let the computer edit my calendar" on AND
 * Android granted WRITE_CALENDAR (see [PhoneDataGate]); each operation id is applied at most once.
 */
data class EventDraft(
    val title: String,
    val allDay: Boolean,
    val startMs: Long,
    val endMs: Long,
    val tz: String,
    val location: String?,
    val description: String?,
    val reminderMin: Int?,
)

sealed interface WriteRequest {
    val opId: String

    data class Create(override val opId: String, val calendarId: String, val draft: EventDraft) : WriteRequest
    data class Update(override val opId: String, val eventId: String, val draft: EventDraft) : WriteRequest
    data class Delete(override val opId: String, val eventId: String) : WriteRequest
}

sealed interface WriteParse {
    data class Ok(val request: WriteRequest) : WriteParse
    data class Bad(val opId: String?, val reason: String) : WriteParse
}

/** `state` is `applied`, `failed` or `unknown` (an earlier attempt was interrupted mid-write). */
data class WriteOutcome(val state: String, val reason: String? = null, val eventId: String? = null)

object CalendarWriteProtocol {
    private val idPattern = Regex("[0-9]{1,18}")
    private val zonePattern = Regex("[A-Za-z0-9_+\\-/:]{1,64}")
    private const val DAY_MS = 86_400_000L
    private const val MAX_TIME_MS = 4_102_444_800_000L

    private fun badText(s: String, max: Int, multiline: Boolean): Boolean =
        s.length > max || s.any { Character.isISOControl(it) && !(multiline && (it == '\n' || it == '\r' || it == '\t')) }

    /** Checks an event exactly as the desktop does before sending; the phone never trusts the desktop to have. */
    fun validate(d: EventDraft): String? {
        if (d.title.isBlank() || badText(d.title, PhoneDataLimits.MAX_TITLE, false)) return "invalid"
        if (d.endMs <= d.startMs || d.startMs !in 0..MAX_TIME_MS || d.endMs !in 0..(MAX_TIME_MS + DAY_MS)) return "invalid"
        if (d.allDay && (d.startMs % DAY_MS != 0L || d.endMs % DAY_MS != 0L)) return "invalid"
        if (!d.allDay && !zonePattern.matches(d.tz)) return "invalid"
        if (d.location != null && badText(d.location, PhoneDataLimits.MAX_LOCATION, false)) return "invalid"
        if (d.description != null && badText(d.description, PhoneDataLimits.MAX_DESCRIPTION, true)) return "invalid"
        if (d.reminderMin != null && d.reminderMin !in 0..40_320) return "invalid"
        return null
    }

    fun parse(body: JSONObject): WriteParse {
        val op = (body.opt("opId") as? String)?.takeIf { PhoneDataProtocol.validRequestId(it) }
            ?: return WriteParse.Bad(null, "invalid")
        if (body.optInt("version", 0) != 1) return WriteParse.Bad(op, "invalid")
        val action = body.optString("action", "")
        fun id(key: String) = (body.opt(key) as? String)?.takeIf { idPattern.matches(it) }
        fun draft(): EventDraft? {
            val e = body.optJSONObject("event") ?: return null
            val title = (e.opt("title") as? String) ?: return null
            val start = (e.opt("startMs") as? Number)?.toLong() ?: return null
            val end = (e.opt("endMs") as? Number)?.toLong() ?: return null
            val allDay = e.opt("allDay") as? Boolean ?: false
            val tz = (e.opt("tz") as? String)?.ifBlank { null } ?: "UTC"
            val reminder = (e.opt("reminderMin") as? Number)?.toLong()?.let { if (it in 0..40_320) it.toInt() else return null }
            fun opt(k: String) = (e.opt(k) as? String)?.trim()?.takeIf { it.isNotEmpty() }
            val d = EventDraft(title.trim(), allDay, start, end, if (allDay) "UTC" else tz, opt("location"), opt("description"), reminder)
            return d
        }
        return when (action) {
            "create" -> {
                val cal = id("calendarId") ?: return WriteParse.Bad(op, "invalid")
                val d = draft() ?: return WriteParse.Bad(op, "invalid")
                WriteParse.Ok(WriteRequest.Create(op, cal, d))
            }
            "update" -> {
                val ev = id("eventId") ?: return WriteParse.Bad(op, "invalid")
                val d = draft() ?: return WriteParse.Bad(op, "invalid")
                WriteParse.Ok(WriteRequest.Update(op, ev, d))
            }
            "delete" -> WriteParse.Ok(WriteRequest.Delete(op, id("eventId") ?: return WriteParse.Bad(op, "invalid")))
            else -> WriteParse.Bad(op, "invalid")
        }
    }

    fun result(opId: String, o: WriteOutcome, duplicate: Boolean = false): JSONObject =
        JSONObject().put("opId", opId).put("state", o.state).also {
            if (o.reason != null) it.put("reason", o.reason)
            if (o.eventId != null) it.put("eventId", o.eventId)
            if (duplicate) it.put("duplicate", true)
        }
}

/** A reminder row of an event as the provider holds it. */
data class ExistingReminder(val id: Long, val minutes: Int, val alert: Boolean)

/**
 * What to change so the event's reminder equals [wanted]. The editor owns one reminder (the first alert);
 * further reminders (other methods, or more than the five that are shown) are never deleted. The first
 * alert is changed in place, so there is no moment without a reminder.
 */
data class ReminderPlan(
    val updateId: Long? = null,
    val updateMinutes: Int? = null,
    val insert: Int? = null,
    val deleteId: Long? = null,
) {
    fun isEmpty() = this == ReminderPlan()

    /** Whether the event still has any reminder once the plan is applied. */
    fun hasAlarmAfter(existing: List<ExistingReminder>, wanted: Int?): Boolean =
        wanted != null || existing.any { it.id != deleteId }

    companion object {
        fun of(existing: List<ExistingReminder>, wanted: Int?): ReminderPlan {
            val first = existing.filter { it.alert }.minByOrNull { it.id }
            return when {
                first == null -> if (wanted != null) ReminderPlan(insert = wanted) else ReminderPlan()
                wanted == null -> ReminderPlan(deleteId = first.id)
                first.minutes == wanted -> ReminderPlan()
                else -> ReminderPlan(updateId = first.id, updateMinutes = wanted)
            }
        }
    }
}

/** What the provider says about one calendar / event; the writer decides from this, never from the packet. */
data class CalendarAccess(val writable: Boolean)
data class ExistingEvent(val calendarId: String, val recurring: Boolean)

interface CalendarWriteStore {
    fun calendar(id: String): CalendarAccess?
    fun event(id: String): ExistingEvent?

    /** Returns the new `Events._ID`, or null when the provider refused. May throw [SecurityException]. */
    fun insert(calendarId: String, draft: EventDraft): String?
    fun update(eventId: String, draft: EventDraft): Boolean
    fun delete(eventId: String): Boolean
}

/** Whole-blob persistence for [OpLedger] (SharedPreferences in the app, a string in tests). */
interface BlobStore {
    fun read(): String
    fun write(text: String)
}

/**
 * Remembers which operation ids were seen, so a retried request is answered from memory and never written twice,
 * even after the app restarted. An id is recorded as `started` BEFORE the provider is touched: if the process
 * died in between, the retry learns the earlier attempt's fate is unknown and says so honestly.
 * Holds ids and short result codes only, never event text. Bounded to [PhoneDataLimits.MAX_OPS_REMEMBERED].
 */
class OpLedger(private val blob: BlobStore, private val max: Int = PhoneDataLimits.MAX_OPS_REMEMBERED) {
    private val entries = LinkedHashMap<String, WriteOutcome?>() // null = started, not finished

    init {
        try {
            val arr = JSONArray(blob.read().ifBlank { "[]" })
            for (i in 0 until minOf(arr.length(), max)) {
                val o = arr.optJSONObject(i) ?: continue
                val op = o.optString("op", "").takeIf { PhoneDataProtocol.validRequestId(it) } ?: continue
                entries[op] = if (o.has("state")) {
                    WriteOutcome(o.getString("state"), o.optString("reason", "").ifEmpty { null }, o.optString("eventId", "").ifEmpty { null })
                } else {
                    null
                }
            }
        } catch (_: org.json.JSONException) {
            entries.clear()
        }
    }

    fun known(op: String) = entries.containsKey(op)

    /** The recorded outcome; an interrupted attempt reads as `unknown`. */
    fun outcome(op: String): WriteOutcome? =
        if (!entries.containsKey(op)) null else entries[op] ?: WriteOutcome("unknown", "interrupted")

    fun begin(op: String) {
        entries[op] = null
        trim()
        save()
    }

    fun finish(op: String, o: WriteOutcome) {
        entries[op] = o
        trim()
        save()
    }

    private fun trim() {
        while (entries.size > max) entries.remove(entries.keys.first())
    }

    private fun save() {
        val arr = JSONArray()
        for ((op, o) in entries) {
            val j = JSONObject().put("op", op)
            if (o != null) {
                j.put("state", o.state)
                if (o.reason != null) j.put("reason", o.reason)
                if (o.eventId != null) j.put("eventId", o.eventId)
            }
            arr.put(j)
        }
        blob.write(arr.toString())
    }
}

/**
 * Applies a validated request to the calendar provider. Only calendars the user ticked for sharing
 * ([sharedIds]) that Android lets this app write to are touched; repeating events are refused.
 */
class CalendarWriter(
    private val store: CalendarWriteStore,
    private val ledger: OpLedger,
    private val sharedIds: () -> Set<String>,
) {
    /** The answer to send back: the outcome and whether it is a repeat of an earlier answer. */
    fun handle(req: WriteRequest): Pair<WriteOutcome, Boolean> {
        ledger.outcome(req.opId)?.let { return it to true }
        val problem = precheck(req)
        if (problem != null) {
            val o = WriteOutcome("failed", problem)
            ledger.finish(req.opId, o)
            return o to false
        }
        ledger.begin(req.opId)
        val o = try {
            apply(req)
        } catch (_: SecurityException) {
            WriteOutcome("failed", "no_permission")
        } catch (_: RuntimeException) {
            WriteOutcome("failed", "generic")
        }
        ledger.finish(req.opId, o)
        return o to false
    }

    private fun writable(calendarId: String): String? {
        if (calendarId !in sharedIds()) return "calendar_missing"
        val c = store.calendar(calendarId) ?: return "calendar_missing"
        return if (c.writable) null else "not_writable"
    }

    private fun precheck(req: WriteRequest): String? = when (req) {
        is WriteRequest.Create -> CalendarWriteProtocol.validate(req.draft) ?: writable(req.calendarId)
        is WriteRequest.Update -> CalendarWriteProtocol.validate(req.draft) ?: existing(req.eventId)
        is WriteRequest.Delete -> existing(req.eventId)
    }

    private fun existing(eventId: String): String? {
        val e = store.event(eventId) ?: return "not_found"
        if (e.recurring) return "recurring"
        return writable(e.calendarId)
    }

    private fun apply(req: WriteRequest): WriteOutcome = when (req) {
        is WriteRequest.Create -> store.insert(req.calendarId, req.draft)
            ?.let { WriteOutcome("applied", eventId = it) } ?: WriteOutcome("failed", "generic")
        is WriteRequest.Update ->
            if (store.update(req.eventId, req.draft)) WriteOutcome("applied", eventId = req.eventId) else WriteOutcome("failed", "not_found")
        is WriteRequest.Delete ->
            if (store.delete(req.eventId)) WriteOutcome("applied", eventId = req.eventId) else WriteOutcome("failed", "not_found")
    }
}

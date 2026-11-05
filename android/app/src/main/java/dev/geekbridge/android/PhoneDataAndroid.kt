// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ContentProviderOperation
import android.content.ContentResolver
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.database.ContentObserver
import android.net.Uri
import android.os.BatteryManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.CancellationSignal
import android.os.Looper
import android.provider.CalendarContract
import android.provider.ContactsContract
import android.provider.Telephony
import android.telephony.SmsManager
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/**
 * Tracks the parts of one (possibly multipart) text. `sent` fires once all parts left the phone, `failed` at the
 * first part error, `delivered` once all delivery reports are in. Pure so it is unit-tested.
 */
class SmsPartTracker(private val parts: Int, private val listener: SmsStatusListener) {
    private var sent = 0
    private var delivered = 0
    private var failed = false

    /** True when this tracker needs no more callbacks. */
    @Synchronized
    fun onSent(resultCode: Int): Boolean {
        if (failed) return true
        if (resultCode != android.app.Activity.RESULT_OK) {
            failed = true
            listener.onStatus("failed", PhoneDataProtocol.failureReason(resultCode))
            return true
        }
        if (++sent == parts) listener.onStatus("sent")
        return false
    }

    @Synchronized
    fun onDelivered(resultCode: Int): Boolean {
        if (failed) return true
        if (resultCode != android.app.Activity.RESULT_OK) {
            failed = true
            listener.onStatus("failed", "undelivered")
            return true
        }
        if (++delivered == parts) {
            listener.onStatus("delivered")
            return true
        }
        return false
    }
}

class AndroidContactsPort(context: Context, private val cancel: QueryCancel = QueryCancel()) : ContactsPort {
    private val resolver: ContentResolver = context.applicationContext.contentResolver
    /** Timestamps of the last listing, so a vCard fetch does not query the provider for each one again. */
    private var listing: Map<String, String> = emptyMap()

    override fun listUids(max: Int): List<Pair<String, String>>? {
        return try {
            val out = ArrayList<Pair<String, String>>()
            val signal = CancellationSignal()
            cancel.guard({ signal.cancel() }) {
                resolver.query(
                    ContactsContract.Contacts.CONTENT_URI,
                    arrayOf(ContactsContract.Contacts.LOOKUP_KEY, ContactsContract.Contacts.CONTACT_LAST_UPDATED_TIMESTAMP),
                    null, null, "${ContactsContract.Contacts._ID} ASC", signal,
                )?.use { c ->
                    while (c.moveToNext() && out.size < max) {
                        cancel.check()
                        val key = c.getString(0) ?: continue
                        out += key to c.getLong(1).toString()
                    }
                } ?: return@guard null
                out
            }?.also { l -> listing = l.toMap() }
        } catch (_: SecurityException) {
            null
        } catch (_: android.os.OperationCanceledException) {
            throw QueryCancelled()
        }
    }

    override fun forget() {
        listing = emptyMap()
    }

    override fun vcard(uid: String): Pair<String, String>? = try {
        cancel.check()
        val ts = listing[uid] ?: resolver.query(
            Uri.withAppendedPath(ContactsContract.Contacts.CONTENT_LOOKUP_URI, Uri.encode(uid)),
            arrayOf(ContactsContract.Contacts.CONTACT_LAST_UPDATED_TIMESTAMP), null, null, null,
        )?.use { if (it.moveToFirst()) it.getLong(0).toString() else null }
        val uri = Uri.withAppendedPath(ContactsContract.Contacts.CONTENT_VCARD_URI, Uri.encode(uid))
        val text = if (ts == null) null else resolver.openAssetFileDescriptor(uri, "r")?.use { fd ->
            // InputStream.readNBytes needs API 33; minSdk is 26.
            fd.createInputStream().use { s ->
                val out = java.io.ByteArrayOutputStream()
                val chunk = ByteArray(8192)
                var left = MAX_RAW_VCARD
                while (left > 0) {
                    val n = s.read(chunk, 0, minOf(chunk.size, left))
                    if (n < 0) break
                    out.write(chunk, 0, n)
                    left -= n
                }
                String(out.toByteArray(), Charsets.UTF_8)
            }
        }
        if (text == null || ts == null) null else text to ts
    } catch (_: SecurityException) {
        null
    } catch (_: java.io.IOException) {
        null
    }

    private companion object {
        const val MAX_RAW_VCARD = 512 * 1024 // photos are stripped afterwards
    }
}

class AndroidCalendarPort(context: Context, private val cancel: QueryCancel = QueryCancel()) : CalendarPort {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    override fun calendars(): List<CalendarInfo> = try {
        val out = ArrayList<CalendarInfo>()
        resolver.query(
            CalendarContract.Calendars.CONTENT_URI,
            arrayOf(
                CalendarContract.Calendars._ID, CalendarContract.Calendars.CALENDAR_DISPLAY_NAME,
                CalendarContract.Calendars.ACCOUNT_NAME, CalendarContract.Calendars.ACCOUNT_TYPE,
                CalendarContract.Calendars.CALENDAR_COLOR, CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL,
            ),
            null, null, "${CalendarContract.Calendars._ID} ASC",
        )?.use { c ->
            while (c.moveToNext() && out.size < PhoneDataLimits.MAX_CALENDARS * 4) {
                val account = c.getString(2).orEmpty()
                val name = c.getString(1).orEmpty().ifBlank { account }
                out += CalendarInfo(
                    id = c.getLong(0).toString(), name = name, account = account.ifBlank { null },
                    accountType = c.getString(3)?.ifBlank { null },
                    color = if (c.isNull(4)) null else c.getInt(4),
                    writable = c.getInt(5) >= CalendarContract.Calendars.CAL_ACCESS_CONTRIBUTOR,
                )
            }
        }
        out
    } catch (_: SecurityException) {
        emptyList()
    }

    override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int): List<CalInstance>? =
        query(calendarIds, fromMs, toMs, max, descending = false)?.let(::withDetails)

    /**
     * The Instances table is ordered by start, so one query over a wide window returns the far past first. Two
     * bounded reads from the pivot, forward and backward, return the nearest [max] on each side instead, and a
     * cursor is never read past that (the provider can hold tens of thousands of rows for a long series).
     */
    override fun instancesAround(calendarIds: Set<String>, fromMs: Long, toMs: Long, pivotMs: Long, max: Int): List<CalInstance>? {
        val pivot = pivotMs.coerceIn(fromMs, toMs)
        val ahead = query(calendarIds, pivot, toMs, max, descending = false) ?: return null
        val behind = query(calendarIds, fromMs, pivot, max, descending = true) ?: return null
        return withDetails(ahead + behind)
    }

    private fun query(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int, descending: Boolean): List<CalInstance>? {
        val ids = calendarIds.filter { id -> id.all { it.isDigit() } && id.length in 1..18 }
        if (ids.isEmpty()) return emptyList()
        val placeholders = ids.joinToString(",") { "?" }
        return rows(fromMs, toMs, "${CalendarContract.Instances.CALENDAR_ID} IN ($placeholders)", ids.toTypedArray(), max, descending)
    }

    /** Streams the Instances cursor of a window; never reads past [max] rows, so a long series stays out of memory. */
    private fun rows(fromMs: Long, toMs: Long, selection: String, args: Array<String>, max: Int, descending: Boolean): List<CalInstance>? {
        val builder = CalendarContract.Instances.CONTENT_URI.buildUpon()
        ContentUris.appendId(builder, fromMs)
        ContentUris.appendId(builder, toMs)
        val signal = CancellationSignal()
        return try {
            cancel.guard({ signal.cancel() }) {
                val out = ArrayList<CalInstance>()
                resolver.query(
                    builder.build(),
                    arrayOf(
                        CalendarContract.Instances.EVENT_ID, CalendarContract.Instances.BEGIN, CalendarContract.Instances.END,
                        CalendarContract.Instances.TITLE, CalendarContract.Instances.ALL_DAY,
                        CalendarContract.Instances.EVENT_TIMEZONE, CalendarContract.Instances.EVENT_LOCATION,
                        CalendarContract.Instances.DESCRIPTION, CalendarContract.Instances.CALENDAR_ID,
                        CalendarContract.Events.RRULE, CalendarContract.Events.RDATE, CalendarContract.Events.ORIGINAL_ID,
                    ),
                    selection, args,
                    "${CalendarContract.Instances.BEGIN} ${if (descending) "DESC" else "ASC"}", signal,
                )?.use { c ->
                    while (c.moveToNext() && out.size < max) {
                        cancel.check()
                        val recurring = !c.getString(9).isNullOrBlank() || !c.getString(10).isNullOrBlank() || !c.isNull(11)
                        out += CalInstance(
                            eventId = c.getLong(0).toString(), begin = c.getLong(1), end = c.getLong(2),
                            title = c.getString(3), allDay = c.getInt(4) == 1, timeZone = c.getString(5),
                            location = c.getString(6), description = c.getString(7), calendarId = c.getLong(8).toString(),
                            recurring = recurring,
                        )
                    }
                } ?: return@guard null
                out
            }
        } catch (_: SecurityException) {
            null
        } catch (_: android.os.OperationCanceledException) {
            throw QueryCancelled()
        }
    }

    /**
     * The revision the sync would report for a one-off event right now, built by the same code that builds the sync
     * (so the two can never disagree), or null if the event or its instance cannot be read.
     */
    fun currentRevision(eventId: String): String? {
        val id = eventId.toLongOrNull() ?: return null
        val (start, end) = resolver.query(
            ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, id),
            arrayOf(CalendarContract.Events.DTSTART, CalendarContract.Events.DTEND), null, null, null,
        )?.use { if (it.moveToFirst()) it.getLong(0) to (if (it.isNull(1)) it.getLong(0) else it.getLong(1)) else null } ?: return null
        val one = rows(start, maxOf(end, start) + 1, "${CalendarContract.Instances.EVENT_ID} = ?", arrayOf(id.toString()), 1, false)
            ?.let(::withDetails)?.firstOrNull() ?: return null
        return PhoneDataProtocol.calendarEvent(one).getString("rev")
    }

    /** Adds reminders and attendees with one bounded query per [PhoneDataLimits.DETAIL_BATCH] distinct events. */
    private fun withDetails(list: List<CalInstance>): List<CalInstance> {
        val ids = list.map { it.eventId }.distinct()
        val reminders = HashMap<String, MutableList<Int>>()
        val counts = HashMap<String, Int>()
        val names = HashMap<String, MutableList<String>>()
        for (batch in ids.chunked(PhoneDataLimits.DETAIL_BATCH)) {
            cancel.check()
            val marks = batch.joinToString(",") { "?" }
            val args = batch.toTypedArray()
            try {
                resolver.query(
                    CalendarContract.Reminders.CONTENT_URI,
                    arrayOf(CalendarContract.Reminders.EVENT_ID, CalendarContract.Reminders.MINUTES, CalendarContract.Reminders.METHOD),
                    "${CalendarContract.Reminders.EVENT_ID} IN ($marks)", args, null,
                )?.use { c ->
                    while (c.moveToNext()) {
                        // -1 (MINUTES_DEFAULT) means "the calendar's default"; it carries no time of its own.
                        val m = c.getInt(1)
                        if (m >= 0 && c.getInt(2) != CalendarContract.Reminders.METHOD_EMAIL) {
                            reminders.getOrPut(c.getLong(0).toString()) { ArrayList() } += m
                        }
                    }
                }
                resolver.query(
                    CalendarContract.Attendees.CONTENT_URI,
                    arrayOf(CalendarContract.Attendees.EVENT_ID, CalendarContract.Attendees.ATTENDEE_NAME, CalendarContract.Attendees.ATTENDEE_EMAIL),
                    "${CalendarContract.Attendees.EVENT_ID} IN ($marks)", args, null,
                )?.use { c ->
                    while (c.moveToNext()) {
                        val id = c.getLong(0).toString()
                        counts[id] = (counts[id] ?: 0) + 1
                        val list2 = names.getOrPut(id) { ArrayList() }
                        val who = c.getString(1).orEmpty().ifBlank { c.getString(2).orEmpty() }
                        if (who.isNotBlank() && list2.size < PhoneDataLimits.MAX_ATTENDEE_NAMES) list2 += who
                    }
                }
            } catch (_: SecurityException) {
                // Details are optional; the events themselves were readable.
            }
        }
        return list.map {
            it.copy(
                reminders = reminders[it.eventId].orEmpty(),
                attendeeCount = counts[it.eventId] ?: 0,
                attendees = names[it.eventId].orEmpty(),
            )
        }
    }
}

/** Writes through the Calendar Provider as an ordinary app (not a sync adapter); the phone's own sync then uploads. */
class AndroidCalendarWriteStore(private val context: Context) : CalendarWriteStore {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    override fun calendar(id: String): CalendarAccess? {
        val cid = id.toLongOrNull() ?: return null
        return resolver.query(
            ContentUris.withAppendedId(CalendarContract.Calendars.CONTENT_URI, cid),
            arrayOf(CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL), null, null, null,
        )?.use { if (it.moveToFirst()) CalendarAccess(it.getInt(0) >= CalendarContract.Calendars.CAL_ACCESS_CONTRIBUTOR) else null }
    }

    private val reader by lazy { AndroidCalendarPort(context) }

    override fun event(id: String): ExistingEvent? {
        val eid = id.toLongOrNull() ?: return null
        val found = resolver.query(
            ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, eid),
            arrayOf(
                CalendarContract.Events.CALENDAR_ID, CalendarContract.Events.RRULE, CalendarContract.Events.RDATE,
                CalendarContract.Events.ORIGINAL_ID, CalendarContract.Events.DELETED,
                CalendarContract.Events.ORGANIZER, CalendarContract.Events.OWNER_ACCOUNT,
                CalendarContract.Events.HAS_ATTENDEE_DATA, CalendarContract.Events.GUESTS_CAN_MODIFY,
            ),
            null, null, null,
        )?.use {
            if (!it.moveToFirst() || it.getInt(4) != 0) null
            else ExistingEvent(
                calendarId = it.getLong(0).toString(),
                recurring = !it.getString(1).isNullOrBlank() || !it.getString(2).isNullOrBlank() || !it.isNull(3),
                guest = CalendarGuest.isGuest(it.getString(5), it.getString(6), it.getInt(7) == 1, it.getInt(8) == 1),
            )
        } ?: return null
        return found.copy(rev = reader.currentRevision(id))
    }

    private fun values(d: EventDraft): ContentValues = ContentValues().apply {
        put(CalendarContract.Events.TITLE, d.title)
        put(CalendarContract.Events.DTSTART, d.startMs)
        put(CalendarContract.Events.DTEND, d.endMs)
        put(CalendarContract.Events.ALL_DAY, if (d.allDay) 1 else 0)
        // Android's write rule: all-day events live in UTC; timed events carry the zone they were entered in.
        put(CalendarContract.Events.EVENT_TIMEZONE, if (d.allDay) "UTC" else d.tz)
        put(CalendarContract.Events.EVENT_LOCATION, d.location ?: "")
        put(CalendarContract.Events.DESCRIPTION, d.description ?: "")
        put(CalendarContract.Events.HAS_ALARM, if (d.reminderMin != null) 1 else 0)
    }

    private fun reminderValues(minutes: Int) = ContentValues().apply {
        put(CalendarContract.Reminders.MINUTES, minutes)
        put(CalendarContract.Reminders.METHOD, CalendarContract.Reminders.METHOD_ALERT)
    }

    private fun remindersOf(eventId: Long): List<ExistingReminder> {
        val out = ArrayList<ExistingReminder>()
        resolver.query(
            CalendarContract.Reminders.CONTENT_URI,
            arrayOf(CalendarContract.Reminders._ID, CalendarContract.Reminders.MINUTES, CalendarContract.Reminders.METHOD),
            "${CalendarContract.Reminders.EVENT_ID} = ?", arrayOf(eventId.toString()),
            "${CalendarContract.Reminders._ID} ASC",
        )?.use { c ->
            while (c.moveToNext()) {
                out += ExistingReminder(c.getLong(0), c.getInt(1), c.getInt(2) == CalendarContract.Reminders.METHOD_ALERT)
            }
        }
        return out
    }

    // The event and its reminder are written in one provider batch (CalendarProvider runs a batch in a single
    // transaction), so a failure leaves nothing behind and a retry with a new op id cannot duplicate the event.
    override fun insert(calendarId: String, draft: EventDraft): String? {
        val v = values(draft).apply { put(CalendarContract.Events.CALENDAR_ID, calendarId.toLong()) }
        val ops = ArrayList<ContentProviderOperation>()
        ops += ContentProviderOperation.newInsert(CalendarContract.Events.CONTENT_URI).withValues(v).build()
        if (draft.reminderMin != null) {
            ops += ContentProviderOperation.newInsert(CalendarContract.Reminders.CONTENT_URI)
                .withValues(reminderValues(draft.reminderMin))
                .withValueBackReference(CalendarContract.Reminders.EVENT_ID, 0)
                .build()
        }
        val uri = resolver.applyBatch(CalendarContract.AUTHORITY, ops).firstOrNull()?.uri ?: return null
        return ContentUris.parseId(uri).toString()
    }

    // Reminders are changed in place (see ReminderPlan): the old one is never deleted before the new one exists,
    // and reminders the editor does not show stay.
    override fun update(eventId: String, draft: EventDraft): Boolean {
        val id = eventId.toLongOrNull() ?: return false
        val existing = remindersOf(id)
        val plan = ReminderPlan.of(existing, draft.reminderMin)
        val v = values(draft).apply {
            put(CalendarContract.Events.HAS_ALARM, if (plan.hasAlarmAfter(existing, draft.reminderMin)) 1 else 0)
        }
        val eventUri = ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, id)
        val ops = ArrayList<ContentProviderOperation>()
        ops += ContentProviderOperation.newUpdate(eventUri).withValues(v).build()
        if (plan.updateId != null && plan.updateMinutes != null) {
            ops += ContentProviderOperation.newUpdate(ContentUris.withAppendedId(CalendarContract.Reminders.CONTENT_URI, plan.updateId))
                .withValues(reminderValues(plan.updateMinutes)).build()
        }
        if (plan.insert != null) {
            ops += ContentProviderOperation.newInsert(CalendarContract.Reminders.CONTENT_URI)
                .withValues(reminderValues(plan.insert).apply { put(CalendarContract.Reminders.EVENT_ID, id) })
                .build()
        }
        if (plan.deleteId != null) {
            ops += ContentProviderOperation.newDelete(ContentUris.withAppendedId(CalendarContract.Reminders.CONTENT_URI, plan.deleteId)).build()
        }
        val results = resolver.applyBatch(CalendarContract.AUTHORITY, ops)
        return (results.firstOrNull()?.count ?: 0) > 0
    }

    override fun delete(eventId: String): Boolean {
        val id = eventId.toLongOrNull() ?: return false
        return resolver.delete(ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, id), null, null) > 0
    }
}

class AndroidSmsPort(context: Context, private val cancel: QueryCancel = QueryCancel()) : SmsPort {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    private val columns = arrayOf(
        Telephony.Sms._ID, Telephony.Sms.THREAD_ID, Telephony.Sms.ADDRESS, Telephony.Sms.BODY,
        Telephony.Sms.DATE, Telephony.Sms.TYPE, Telephony.Sms.READ, Telephony.Sms.SUBSCRIPTION_ID,
    )

    private fun query(selection: String?, args: Array<String>?, limit: Int): List<SmsRow> = try {
        val q = Bundle().apply {
            if (selection != null) putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            if (args != null) putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args)
            putStringArray(ContentResolver.QUERY_ARG_SORT_COLUMNS, arrayOf(Telephony.Sms.DATE, Telephony.Sms._ID))
            putInt(ContentResolver.QUERY_ARG_SORT_DIRECTION, ContentResolver.QUERY_SORT_DIRECTION_DESCENDING)
            putInt(ContentResolver.QUERY_ARG_LIMIT, limit)
        }
        val signal = CancellationSignal()
        cancel.guard({ signal.cancel() }) {
            val out = ArrayList<SmsRow>()
            resolver.query(Telephony.Sms.CONTENT_URI, columns, q, signal)?.use { c ->
                while (c.moveToNext() && out.size < limit) {
                    cancel.check()
                    val type = c.getInt(5)
                    if (type !in 1..6) continue
                    out += SmsRow(
                        id = c.getLong(0), threadId = c.getLong(1), address = c.getString(2).orEmpty(),
                        body = c.getString(3).orEmpty(), date = c.getLong(4), type = type, read = c.getInt(6) != 0,
                        subId = c.getInt(7),
                    )
                }
            }
            out
        }
    } catch (_: SecurityException) {
        emptyList()
    } catch (_: android.os.OperationCanceledException) {
        throw QueryCancelled()
    }

    private val appContext = context.applicationContext

    @SuppressLint("MissingPermission")
    override fun subscriptions(): List<SimInfo> = try {
        if (Build.VERSION.SDK_INT < 22 ||
            appContext.checkSelfPermission(Manifest.permission.READ_PHONE_STATE) != PackageManager.PERMISSION_GRANTED
        ) emptyList()
        else appContext.getSystemService(android.telephony.SubscriptionManager::class.java)
            ?.activeSubscriptionInfoList.orEmpty()
            .map { SimInfo(it.subscriptionId, it.simSlotIndex + 1, (it.displayName ?: it.carrierName)?.toString().orEmpty()) }
    } catch (_: SecurityException) {
        emptyList()
    }

    override fun unreadCounts(): Map<Long, Int> = SmsMerge.unread(smsUnread(), mmsUnread())

    /** Unread picture messages per thread; any failure just means "none" so texts are never held back by it. */
    private fun mmsUnread(): Map<Long, Int> = try {
        val q = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SELECTION, "${Telephony.Mms.READ} = 0 AND ${Telephony.Mms.MESSAGE_BOX} = 1")
            putInt(ContentResolver.QUERY_ARG_LIMIT, 2000)
        }
        val out = HashMap<Long, Int>()
        resolver.query(Telephony.Mms.CONTENT_URI, arrayOf(Telephony.Mms.THREAD_ID), q, null)?.use { c ->
            var n = 0
            while (c.moveToNext() && n++ < 2000) out.merge(c.getLong(0), 1, Int::plus)
        }
        out
    } catch (_: RuntimeException) {
        emptyMap()
    }

    /** Picture messages, newest first. Only ids, thread, date, box, read mark and SIM; never a part or a caption. */
    private fun mmsQuery(selection: String?, args: Array<String>?, limit: Int): List<SmsRow> = try {
        val q = Bundle().apply {
            if (selection != null) putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            if (args != null) putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args)
            putStringArray(ContentResolver.QUERY_ARG_SORT_COLUMNS, arrayOf(Telephony.Mms.DATE, Telephony.Mms._ID))
            putInt(ContentResolver.QUERY_ARG_SORT_DIRECTION, ContentResolver.QUERY_SORT_DIRECTION_DESCENDING)
            putInt(ContentResolver.QUERY_ARG_LIMIT, limit)
        }
        val signal = CancellationSignal()
        cancel.guard({ signal.cancel() }) {
            val out = ArrayList<SmsRow>()
            val cols = arrayOf(
                Telephony.Mms._ID, Telephony.Mms.THREAD_ID, Telephony.Mms.DATE, Telephony.Mms.MESSAGE_BOX,
                Telephony.Mms.READ, Telephony.Mms.SUBSCRIPTION_ID,
            )
            resolver.query(Telephony.Mms.CONTENT_URI, cols, q, signal)?.use { c ->
                while (c.moveToNext() && out.size < limit) {
                    cancel.check()
                    val box = c.getInt(3)
                    if (box !in 1..4) continue
                    out += SmsRow(
                        id = c.getLong(0), threadId = c.getLong(1), address = "", body = "", date = MmsRows.dateMs(c.getLong(2)),
                        type = box, read = c.getInt(4) != 0, subId = c.getInt(5), mms = true,
                    )
                }
            }
            out
        }
    } catch (e: SecurityException) {
        GbLog.i("sms", "picture messages not read: ${e.javaClass.simpleName}")
        emptyList()
    } catch (_: android.os.OperationCanceledException) {
        throw QueryCancelled()
    } catch (e: RuntimeException) {
        // an OEM provider without one of the columns: texts still work, pictures are missing (the class only, never a row)
        GbLog.i("sms", "picture messages not read: ${e.javaClass.simpleName}")
        emptyList()
    }

    /** Fills in who an MMS is with (one small query per message shown). */
    private fun withAddress(r: SmsRow): SmsRow {
        if (!r.mms) return r
        return try {
            val uri = Telephony.Mms.CONTENT_URI.buildUpon().appendPath(r.id.toString()).appendPath("addr").build()
            val addrs = ArrayList<Pair<String, Int>>()
            resolver.query(uri, arrayOf("address", "type"), null, null, null)?.use { c ->
                while (c.moveToNext() && addrs.size < 16) addrs += (c.getString(0).orEmpty() to c.getInt(1))
            }
            r.copy(address = MmsRows.pickAddress(addrs, incoming = r.type == 1))
        } catch (_: RuntimeException) {
            r
        }
    }

    private fun smsUnread(): Map<Long, Int> = try {
        val q = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SELECTION, "${Telephony.Sms.READ} = 0 AND ${Telephony.Sms.TYPE} = 1")
            putInt(ContentResolver.QUERY_ARG_LIMIT, 5000)
        }
        val signal = CancellationSignal()
        cancel.guard({ signal.cancel() }) {
            val out = HashMap<Long, Int>()
            resolver.query(Telephony.Sms.CONTENT_URI, arrayOf(Telephony.Sms.THREAD_ID), q, signal)?.use { c ->
                var n = 0
                while (c.moveToNext() && n++ < 5000) out.merge(c.getLong(0), 1, Int::plus)
            }
            out
        }
    } catch (_: SecurityException) {
        emptyMap()
    } catch (_: android.os.OperationCanceledException) {
        throw QueryCancelled()
    }

    /**
     * Newest message of each thread. Pages through the provider newest-first by (date, id) instead of one fixed-size
     * read: a phone with 800 threads and 20 000 texts has recent bursts in a few chatty threads, and the newest 2000
     * rows alone would miss most of the quieter threads.
     */
    override fun latestPerThread(max: Int): List<SmsRow> {
        val texts = SmsPaging.latestPerThread(max) { date, id, limit ->
            query(
                "(${Telephony.Sms.DATE} < ? OR (${Telephony.Sms.DATE} = ? AND ${Telephony.Sms._ID} < ?))",
                arrayOf(date.toString(), date.toString(), id.toString()), limit,
            )
        }
        val pictures = mmsQuery(null, null, MMS_SCAN_ROWS)
        return MmsRows.fillAddress(SmsMerge.latest(texts, pictures, max).map(::withAddress), texts)
    }

    /** Distinct threads in the newest [PhoneDataLimits.SMS_SCAN_ROWS] rows; one narrow column, no bodies. */
    override fun threadCount(): Int? = try {
        val q = Bundle().apply { putInt(ContentResolver.QUERY_ARG_LIMIT, PhoneDataLimits.SMS_SCAN_ROWS) }
        val signal = CancellationSignal()
        cancel.guard({ signal.cancel() }) {
            val threads = HashSet<Long>()
            resolver.query(Telephony.Sms.CONTENT_URI, arrayOf(Telephony.Sms.THREAD_ID), q, signal)?.use { c ->
                var n = 0
                while (c.moveToNext() && n++ < PhoneDataLimits.SMS_SCAN_ROWS) threads += c.getLong(0)
            }
            threads.size
        }
    } catch (_: SecurityException) {
        null
    } catch (_: android.os.OperationCanceledException) {
        throw QueryCancelled()
    }

    override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int): List<SmsRow> {
        val texts =
            if (rangeStartMs == null) query("${Telephony.Sms.THREAD_ID} = ?", arrayOf(threadId.toString()), limit)
            else query("${Telephony.Sms.THREAD_ID} = ? AND ${Telephony.Sms.DATE} <= ?", arrayOf(threadId.toString(), rangeStartMs.toString()), limit)
        val pictures =
            if (rangeStartMs == null) mmsQuery("${Telephony.Mms.THREAD_ID} = ?", arrayOf(threadId.toString()), limit)
            else mmsQuery("${Telephony.Mms.THREAD_ID} = ? AND ${Telephony.Mms.DATE} <= ?", arrayOf(threadId.toString(), (rangeStartMs / 1000).toString()), limit)
        return MmsRows.fillAddress(SmsMerge.thread(texts, pictures, limit).map(::withAddress), texts)
    }

    private companion object {
        /** Newest picture messages looked at to find each thread's latest one. */
        const val MMS_SCAN_ROWS = 500
    }

    /** Newest inbox row from [address] with this [body], for linking a just-received broadcast to its provider row. */
    fun findIncoming(address: String, body: String): SmsRow? =
        query("${Telephony.Sms.TYPE} = 1 AND ${Telephony.Sms.ADDRESS} = ?", arrayOf(address), 5).firstOrNull { it.body == body }
}

/** An unguessable part of a status action name: below API 33 the receivers cannot be marked not-exported. */
internal fun smsActionToken(random: java.security.SecureRandom = java.security.SecureRandom()): String {
    val b = ByteArray(12)
    random.nextBytes(b)
    return b.joinToString("") { "%02x".format(it) }
}

/** `getSystemService(SmsManager::class.java)` exists from Android 12 (API 31); earlier releases return null. */
internal fun smsManagerIsSystemService(sdk: Int = Build.VERSION.SDK_INT): Boolean = sdk >= 31

/** `SmsManager` behind [SmsSender]. Requires SEND_SMS (a SecurityException is mapped by the engine). */
@SuppressLint("MissingPermission", "UnspecifiedRegisterReceiverFlag")
class AndroidSmsSender(context: Context) : SmsSender {
    private val app = context.applicationContext
    // Status callbacks run here, never on the main thread: the listener takes the engine lock, which the
    // worker holds during provider reads.
    private val callbacks = Handler(
        android.os.HandlerThread("geekbridge-sms-status").apply { start() }.looper,
    )
    private val requestCodes = java.security.SecureRandom()

    // SmsManager is a system service by class only from API 31; before that getSystemService returns null.
    @Suppress("DEPRECATION")
    private fun defaultSmsManager(): SmsManager =
        if (smsManagerIsSystemService()) app.getSystemService(SmsManager::class.java) else SmsManager.getDefault()

    override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {
        val manager = if (subId != null) SmsManager.getSmsManagerForSubscriptionId(subId)
        else defaultSmsManager()
        val parts = manager.divideMessage(text)
        val token = smsActionToken()
        val n = requestCodes.nextInt()
        val sentAction = "${app.packageName}.SMS_SENT.$token"
        val deliveredAction = "${app.packageName}.SMS_DELIVERED.$token"
        val tracker = SmsPartTracker(parts.size, listener)
        val receivers = ArrayList<BroadcastReceiver>(2)
        fun register(action: String, onResult: (Int) -> Boolean) {
            val r = object : BroadcastReceiver() {
                override fun onReceive(context: Context, intent: Intent) {
                    if (onResult(resultCode)) release()
                }
                fun release() = try { app.unregisterReceiver(this) } catch (_: IllegalArgumentException) { }
            }
            receivers += r
            val filter = IntentFilter(action)
            if (Build.VERSION.SDK_INT >= 33) app.registerReceiver(r, filter, null, callbacks, Context.RECEIVER_NOT_EXPORTED)
            else app.registerReceiver(r, filter, null, callbacks)
        }
        fun releaseAll() = receivers.forEach { try { app.unregisterReceiver(it) } catch (_: IllegalArgumentException) { } }
        fun intents(action: String) = ArrayList<PendingIntent>(parts.size).also { l ->
            repeat(parts.size) {
                l += PendingIntent.getBroadcast(
                    app, n, Intent(action).setPackage(app.packageName), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            }
        }
        try {
            register(sentAction) { tracker.onSent(it) }
            register(deliveredAction) { tracker.onDelivered(it) }
            // Never leave receivers behind if a report never comes (delivery reports are optional for carriers).
            callbacks.postDelayed({ releaseAll() }, 15 * 60_000L)
            manager.sendMultipartTextMessage(address, null, parts, intents(sentAction), intents(deliveredAction))
        } catch (t: Throwable) {
            // The send never started, so no report will come: do not leave the receivers registered until the timer.
            releaseAll()
            throw t
        }
        listener.onStatus("accepted")
    }
}

/**
 * "A computer sent too many texts, the rest were held back": a heads-up notification plus a toast, so the owner learns
 * of a runaway or hostile computer even when the screen was off. Needs no SMS right; the notification permission only
 * decides whether the banner shows (the toast always does). Names are bidi-isolated and clipped.
 */
class SmsLimitNotice(private val ctx: Context) {
    fun show(computer: String) {
        val name = Bidi.isolate(computer.ifBlank { ctx.getString(R.string.offer_a_computer) })
        val text = ctx.getString(R.string.sms_limit_text, name)
        Handler(android.os.Looper.getMainLooper()).post {
            android.widget.Toast.makeText(ctx, text, android.widget.Toast.LENGTH_LONG).show()
        }
        try {
            val nm = ctx.getSystemService(android.app.NotificationManager::class.java) ?: return
            nm.createNotificationChannel(
                android.app.NotificationChannel(CHANNEL, ctx.getString(R.string.channel_sms_limit), android.app.NotificationManager.IMPORTANCE_HIGH).apply {
                    description = ctx.getString(R.string.channel_sms_limit_desc)
                    setShowBadge(false)
                },
            )
            val open = PendingIntent.getActivity(
                ctx, 0, Intent(ctx, HomeActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            nm.notify(
                NOTICE_ID,
                android.app.Notification.Builder(ctx, CHANNEL)
                    .setSmallIcon(R.drawable.ic_stat_link)
                    .setContentTitle(ctx.getString(R.string.sms_limit_title))
                    .setContentText(text)
                    .setStyle(android.app.Notification.BigTextStyle().bigText(text))
                    .setContentIntent(open)
                    .setAutoCancel(true)
                    .setCategory(android.app.Notification.CATEGORY_STATUS)
                    .setTimeoutAfter(60 * 60_000L)
                    .build(),
            )
        } catch (_: SecurityException) {
            // Notifications not allowed: the toast above is all there is.
        }
    }

    private companion object {
        const val CHANNEL = "geekbridge_sms_limit"
        const val NOTICE_ID = 0x4000_0100
    }
}

/** Manifest receiver for incoming texts (needs RECEIVE_SMS; GeekBridge is not, and does not become, the default SMS app). */
class SmsReceivedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return
        val msgs = Telephony.Sms.Intents.getMessagesFromIntent(intent) ?: return
        if (msgs.isEmpty()) return
        val address = msgs[0].originatingAddress ?: return
        val body = msgs.joinToString("") { it.messageBody.orEmpty() }
        val pending = goAsync()
        // finish() may only run once, and a receiver held past ~10 s is an ANR: the provider polling below can queue
        // behind a long read on the shared worker, so a failsafe releases the broadcast after 8 s.
        val finished = java.util.concurrent.atomic.AtomicBoolean(false)
        val finish: () -> Unit = {
            if (finished.compareAndSet(false, true)) {
                try {
                    pending.finish()
                } catch (_: RuntimeException) {
                }
            }
        }
        Handler(Looper.getMainLooper()).postDelayed({ finish() }, 8_000L)
        PhoneDataRuntime.init(context)
        PhoneDataRuntime.onIncomingBroadcast(address, body, finish)
    }
}

@SuppressLint("StaticFieldLeak") // only the application context is held
object PhoneDataRuntime {
    private const val FILE = "geekbridge_phonedata"

    @Volatile private var app: Context? = null
    @Volatile private var prefs: SharedPreferences? = null
    @Volatile var settings: PhoneDataSettings = PhoneDataSettings()
        private set
    @Volatile private var engine: PhoneDataEngine? = null
    @Volatile private var declared = ActiveFeatures()
    @Volatile private var battery: BatteryReporter? = null
    @Volatile private var mirror: NotificationMirror? = null
    private var batteryReceiver: BroadcastReceiver? = null
    private val main = Handler(Looper.getMainLooper())
    private val core: CoreClient by lazy { CoreClient.native() }
    private var smsPort: AndroidSmsPort? = null
    private var waiter: Thread? = null
    private var observer: ContentObserver? = null
    private var linkRunning = false
    private var retryScheduled = false
    private val worker = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-data-send").apply { isDaemon = true } }

    @Synchronized
    fun init(ctx: Context) {
        if (app != null) return
        val a = ctx.applicationContext
        app = a
        val p = a.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        prefs = p
        settings = PhoneDataSettings(
            contacts = p.getBoolean("contacts", false),
            calendar = p.getBoolean("calendar", false),
            sms = p.getBoolean("sms", false),
            calendarIds = p.getStringSet("calendar_ids", emptySet()).orEmpty().toSet(),
            calendarEdit = p.getBoolean("calendar_edit", false),
            smsSpecialNumbers = p.getBoolean("sms_special", false),
            battery = p.getBoolean("battery", false),
            notifications = p.getBoolean("notifications", false),
            notificationApps = p.getStringSet("notification_apps", emptySet()).orEmpty().toSet(),
            photos = p.getBoolean("photos", false),
        )
        val mirrorOut = NotificationMirror(
            port = MirrorAndroid.Port(a),
            core = { core.dataAction(it) },
            enabled = { active().notifications && linkRunning },
            allowedApps = { settings.notificationApps },
            ownPackage = a.packageName,
        )
        mirror = mirrorOut
        val reporter = BatteryReporter(
            read = { currentBattery() },
            core = { core.dataAction(it) },
            enabled = { active().battery && linkRunning },
            later = { ms, action -> main.postDelayed({ worker.execute(action) }, ms) },
        )
        battery = reporter
        val cancel = QueryCancel()
        val sms = AndroidSmsPort(a, cancel)
        smsPort = sms
        engine = PhoneDataEngine(
            cancel = cancel,
            core = { core.dataAction(it) },
            features = { active() },
            calendarIds = { settings.calendarIds },
            contacts = AndroidContactsPort(a, cancel),
            calendar = AndroidCalendarPort(a, cancel),
            sms = sms,
            sender = AndroidSmsSender(a),
            calendarWriter = CalendarWriter(
                AndroidCalendarWriteStore(a),
                OpLedger(BackedBlob(StateMigrations.kv(p), "calendar_ops", BackedBlob::jsonArray).let { b ->
                    object : BlobStore {
                        override fun read() = b.read()
                        override fun write(text: String) = b.write(text)
                    }
                }),
            ) { settings.calendarIds },
            allowSpecialNumbers = { settings.smsSpecialNumbers },
            smsLimited = { computer -> SmsLimitNotice(a).show(computer) },
            battery = reporter,
            mirror = mirrorOut,
            photoPort = AndroidPhotoPort(a, main),
            smsLedger = RequestLedger(object : BlobStore {
                override fun read() = p.getString("sms_requests", "") ?: ""
                override fun write(text: String) {
                    p.edit().putString("sms_requests", text).apply()
                }
            }),
        )
    }

    fun permissions(ctx: Context): PhoneDataPermissions {
        fun has(p: String) = ctx.checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED
        return PhoneDataPermissions(
            readContacts = has(Manifest.permission.READ_CONTACTS),
            readCalendar = has(Manifest.permission.READ_CALENDAR),
            readSms = has(Manifest.permission.READ_SMS),
            sendSms = has(Manifest.permission.SEND_SMS),
            receiveSms = has(Manifest.permission.RECEIVE_SMS),
            writeCalendar = has(Manifest.permission.WRITE_CALENDAR),
            notificationAccess = MirrorAndroid.accessGranted(ctx),
            photoAccess = PhotoAccessRules.evaluate(android.os.Build.VERSION.SDK_INT) { has(it) },
        )
    }

    fun active(): ActiveFeatures {
        val a = app ?: return ActiveFeatures()
        return PhoneDataGate.active(settings, permissions(a))
    }

    /** Called when the transport is (re)started so the declared capabilities are what [active] says right now. */
    fun startFeatures(): ActiveFeatures = active().also {
        declared = it
        GbLog.i("capability", "declared at link start: contacts=${it.contacts} calendar=${it.calendar} calendar_edit=${it.calendarEdit} sms=${it.sms} battery=${it.battery} notifications=${it.notifications} photos=${it.photos}")
    }

    fun setContacts(on: Boolean) = update { it.copy(contacts = on) }
    fun setCalendar(on: Boolean) {
        update { it.copy(calendar = on) }
        if (!on) worker.execute { engine?.forgetCalendarWrites() }
    }
    fun setCalendarEdit(on: Boolean) {
        update { it.copy(calendarEdit = on) }
        if (!on) worker.execute { engine?.forgetCalendarWrites() }
    }
    fun setSms(on: Boolean) = update { it.copy(sms = on) }
    fun setBattery(on: Boolean) = update { it.copy(battery = on) }
    fun setNotifications(on: Boolean) = update { it.copy(notifications = on) }
    fun setPhotos(on: Boolean) = update { it.copy(photos = on) }
    fun setNotificationApps(apps: Set<String>) = update { it.copy(notificationApps = apps.take(MirrorRules.MAX_APPS).toSet()) }
    fun setSmsSpecialNumbers(on: Boolean) = update { it.copy(smsSpecialNumbers = on) }
    fun setCalendarIds(ids: Set<String>) {
        update { it.copy(calendarIds = ids.take(PhoneDataLimits.MAX_CALENDARS).toSet()) }
        worker.execute { engine?.reset() } // selection changed: the next sync is a full snapshot
    }

    private fun update(change: (PhoneDataSettings) -> PhoneDataSettings) {
        settings = change(settings)
        prefs?.edit()
            ?.putBoolean("contacts", settings.contacts)?.putBoolean("calendar", settings.calendar)
            ?.putBoolean("sms", settings.sms)?.putStringSet("calendar_ids", settings.calendarIds)?.putBoolean("calendar_edit", settings.calendarEdit)
            ?.putBoolean("sms_special", settings.smsSpecialNumbers)?.putBoolean("battery", settings.battery)
            ?.putBoolean("notifications", settings.notifications)?.putStringSet("notification_apps", settings.notificationApps)
            ?.putBoolean("photos", settings.photos)?.apply()
        resync()
    }

    /** A permission or setting changed. Main thread. */
    fun resync() {
        val a = app ?: return
        val now = active()
        worker.execute { core.dataAction(PhoneDataProtocol.configRequest(now)) }
        if (CapabilityRedeclare.onChange(declared, now, linkRunning) == CapabilityRedeclare.Action.None) return
        if (CapabilityRedeclare.onChange(declared, now, linkRunning) == CapabilityRedeclare.Action.Reconnect) {
            // Capabilities are declared in the identity packet at connect time: reconnect to change them.
            declared = now
            GbLog.i("capability", "changed (contacts=${now.contacts} calendar=${now.calendar} calendar_edit=${now.calendarEdit} sms=${now.sms} battery=${now.battery} notifications=${now.notifications} photos=${now.photos}): reconnecting to re-declare")
            val controller = LinkRuntime.controller()
            controller.stop(R.string.pd_reconnecting)
            restartAttempt(a, 0)
        } else {
            startObserving()
        }
    }

    private fun restartAttempt(a: Context, n: Int) {
        main.postDelayed({
            LinkService.sync(a)
            if (n < 4 && LinkRuntime.controller().phase == LinkPhase.Stopped) restartAttempt(a, n + 1)
        }, 800)
    }

    @Synchronized
    fun onLinkChanged(running: Boolean) {
        // Observers fire on every status refresh; only a real up/down transition starts a new session.
        if (running == linkRunning) return
        linkRunning = running
        val e = engine ?: return
        battery?.reset()
        mirror?.reset()
        if (!running) {
            stopObserving()
            e.cancelInFlight() // a long provider read has nowhere to send its answer any more
            worker.execute { e.reset() }
        } else {
            worker.execute { e.reset() }
            startObserving()
            // A permission answered while the link was still connecting was not in the identity packet it sent.
            if (CapabilityRedeclare.driftAtLinkUp(declared, active())) main.post { resync() }
        }
    }

    @Synchronized
    private fun startObserving() {
        val a = app ?: return
        val f = active()
        if ((!f.any && !DebugHooks.streaming) || !linkRunning) return
        val e = engine ?: return
        if (f.calendar && observer == null) {
            val o = object : ContentObserver(main) {
                override fun onChange(selfChange: Boolean) {
                    worker.execute {
                        if (!e.onCalendarChanged()) scheduleCalendarRetry(e)
                    }
                }
            }
            try {
                a.contentResolver.registerContentObserver(CalendarContract.CONTENT_URI, true, o)
                observer = o
            } catch (_: SecurityException) {
            }
        } else if (!f.calendar) {
            stopObserver()
        }
        // A picture message arrives without a broadcast this app may receive (that needs RECEIVE_MMS), so watch the
        // provider and re-send the conversation list shortly after it changes.
        if (f.sms && mmsObserver == null) {
            val o = object : ContentObserver(main) {
                override fun onChange(selfChange: Boolean) {
                    main.removeCallbacks(mmsRefresh)
                    main.postDelayed(mmsRefresh, MMS_REFRESH_DELAY_MS)
                }
            }
            try {
                a.contentResolver.registerContentObserver(Telephony.Mms.CONTENT_URI, true, o)
                mmsObserver = o
            } catch (_: SecurityException) {
            }
        } else if (!f.sms) {
            stopMmsObserver()
        }
        // Reading a conversation on the phone (or a new text) changes the SMS table: re-send the list with its unread
        // map so the computer's badge follows. f.sms means READ_SMS is granted. Same debounced refresh as pictures.
        if (f.sms && smsObserver == null) {
            val o = object : ContentObserver(main) {
                override fun onChange(selfChange: Boolean) {
                    main.removeCallbacks(mmsRefresh)
                    main.postDelayed(mmsRefresh, SMS_REFRESH_DELAY_MS)
                }
            }
            try {
                a.contentResolver.registerContentObserver(Telephony.Sms.CONTENT_URI, true, o)
                smsObserver = o
            } catch (_: SecurityException) {
            }
        } else if (!f.sms) {
            stopSmsObserver()
        }
        if (f.battery) startBatteryReceiver(a) else stopBatteryReceiver()
        if (waiter?.isAlive != true) {
            waiter = Thread({ waitLoop(e) }, "geekbridge-data-wait").apply { isDaemon = true; start() }
        }
    }

    /**
     * A change that came in inside the 30 s spacing is sent as soon as the spacing allows. The retry asks the engine how
     * long is left and goes again until nothing is owed: another push in between (an applied edit, a request) moves the
     * spacing, and a retry that simply gave up then left the change unsent until the next unrelated change.
     */
    private fun scheduleCalendarRetry(e: PhoneDataEngine) {
        val wait = e.calendarRetryInMs() ?: return
        synchronized(this) { if (retryScheduled) return; retryScheduled = true }
        main.postDelayed({
            synchronized(this) { retryScheduled = false }
            worker.execute {
                e.onCalendarChanged()
                scheduleCalendarRetry(e)
            }
        }, wait + 1000)
    }

    private var mmsObserver: ContentObserver? = null
    private val mmsRefresh = Runnable { engine?.let { e -> worker.execute { e.refreshConversations() } } }
    private const val MMS_REFRESH_DELAY_MS = 3_000L
    private var smsObserver: ContentObserver? = null
    private const val SMS_REFRESH_DELAY_MS = 1_500L

    private fun stopSmsObserver() {
        smsObserver?.let { try { app?.contentResolver?.unregisterContentObserver(it) } catch (_: RuntimeException) { } }
        smsObserver = null
    }

    private fun stopMmsObserver() {
        main.removeCallbacks(mmsRefresh)
        mmsObserver?.let { try { app?.contentResolver?.unregisterContentObserver(it) } catch (_: RuntimeException) { } }
        mmsObserver = null
    }

    private fun stopObserver() {
        observer?.let { try { app?.contentResolver?.unregisterContentObserver(it) } catch (_: RuntimeException) { } }
        observer = null
    }

    @Synchronized
    private fun stopObserving() {
        stopObserver()
        stopMmsObserver()
        stopSmsObserver()
        stopBatteryReceiver()
    }

    /** The sticky battery state, read without registering anything. Null when unknown. */
    private fun currentBattery(): BatteryReading? {
        val a = app ?: return null
        val i = a.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED)) ?: return null
        return BatteryRules.reading(
            i.getIntExtra(BatteryManager.EXTRA_LEVEL, -1),
            i.getIntExtra(BatteryManager.EXTRA_SCALE, -1),
            i.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0),
        )
    }

    /**
     * Listens to the system's battery broadcast only while the link runs and the feature is on. The broadcast is
     * sticky, so registering also delivers the current state once: that is the send at link start. No polling.
     */
    @Synchronized
    private fun startBatteryReceiver(a: Context) {
        if (batteryReceiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                worker.execute { battery?.onChanged() }
            }
        }
        batteryReceiver = r
        a.registerReceiver(r, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
    }

    @Synchronized
    private fun stopBatteryReceiver() {
        val r = batteryReceiver ?: return
        batteryReceiver = null
        try { app?.unregisterReceiver(r) } catch (_: RuntimeException) { }
    }

    /** A computer was allowed (or withdrawn) for the battery: tell the new one the current level at once. */
    fun onBatteryGrantsChanged() {
        worker.execute { battery?.sendNow() }
    }

    /** Notification access was granted or taken away (the listener connected or disconnected). */
    fun onListenerChanged() {
        main.post { resync() }
    }

    fun onMirrorPosted(n: MirrorInfo) {
        worker.execute { mirror?.onPosted(n) }
    }

    fun onMirrorRemoved(key: String) {
        worker.execute { mirror?.onRemoved(key) }
    }

    private fun waitLoop(e: PhoneDataEngine) {
        var since = 0L
        var odd = 0
        while (!Thread.currentThread().isInterrupted) {
            val o = core.dataAction(PhoneDataProtocol.waitRequest(since))
            if (o !is CoreOutcome.Ok) return // not running: restarted when the link comes back
            val r = PhoneDataProtocol.parseWait(o.result)
            if (r == null) {
                // An answer in a shape this build does not know: start over from the beginning of the core's
                // log (requests are idempotent) instead of ending the waiter until the next link change.
                if (++odd > 5) return
                since = 0L
                try { Thread.sleep(1000L * odd) } catch (_: InterruptedException) { return } // this thread's own, not the worker
                continue
            }
            odd = 0
            if (r.stopped) return
            since = PhoneDataProtocol.nextSince(since, r)
            if (r.events.isNotEmpty()) {
                try {
                    e.onEvents(r.events)
                } catch (_: RuntimeException) {
                    // A provider failure while serving one event must not end this daemon thread: an uncaught
                    // exception here would take the whole process down.
                }
            }
            if (!linkRunning || (!active().any && !DebugHooks.streaming)) return
        }
    }

    // Retries for an incoming text are scheduled, not slept: the shared worker also runs provider reads and sends.
    private val retries = java.util.concurrent.Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "geekbridge-data-retry").apply { isDaemon = true }
    }

    /**
     * Incoming text: poll Android's SMS provider (the default app writes the row) at 1.5 s, 3 s and 4.5 s without
     * holding the worker, then push it. If no row matches, the computer's conversation list is refreshed instead
     * so the text is not lost.
     */
    fun onIncomingBroadcast(address: String, body: String, done: () -> Unit) {
        val e = engine
        val port = smsPort
        if (e == null || port == null || !linkRunning || !active().sms) return done()
        fun attempt(n: Int) {
            worker.execute {
                var finished = false
                try {
                    val row = port.findIncoming(address, body)
                    if (row != null) {
                        e.onIncomingSms(row)
                        finished = true
                    } else if (n >= 3) {
                        e.refreshConversations()
                        finished = true
                    }
                } catch (_: RuntimeException) {
                    finished = true
                } finally {
                    if (finished) done() else retries.schedule({ attempt(n + 1) }, 1500, java.util.concurrent.TimeUnit.MILLISECONDS)
                }
            }
        }
        retries.schedule({ attempt(1) }, 1500, java.util.concurrent.TimeUnit.MILLISECONDS)
    }

    /** Calendars for the picker (needs READ_CALENDAR). Off the main thread. */
    fun calendars(): List<CalendarInfo> = app?.let { AndroidCalendarPort(it).calendars() }.orEmpty()
}

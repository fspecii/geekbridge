package dev.geekbridge.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
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
import android.os.Build
import android.os.Bundle
import android.os.Handler
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

class AndroidContactsPort(context: Context) : ContactsPort {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    override fun listUids(max: Int): List<Pair<String, String>>? {
        return try {
        val out = ArrayList<Pair<String, String>>()
        resolver.query(
            ContactsContract.Contacts.CONTENT_URI,
            arrayOf(ContactsContract.Contacts.LOOKUP_KEY, ContactsContract.Contacts.CONTACT_LAST_UPDATED_TIMESTAMP),
            null, null, "${ContactsContract.Contacts._ID} ASC",
        )?.use { c ->
            while (c.moveToNext() && out.size < max) {
                val key = c.getString(0) ?: continue
                out += key to c.getLong(1).toString()
            }
        } ?: return null
        out
    } catch (_: SecurityException) {
        null
    }
    }

    override fun vcard(uid: String): Pair<String, String>? = try {
        val ts = resolver.query(
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

class AndroidCalendarPort(context: Context) : CalendarPort {
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

    override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int): List<CalInstance>? {
        val ids = calendarIds.filter { id -> id.all { it.isDigit() } && id.length in 1..18 }
        if (ids.isEmpty()) return emptyList()
        val builder = CalendarContract.Instances.CONTENT_URI.buildUpon()
        ContentUris.appendId(builder, fromMs)
        ContentUris.appendId(builder, toMs)
        val placeholders = ids.joinToString(",") { "?" }
        return try {
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
                "${CalendarContract.Instances.CALENDAR_ID} IN ($placeholders)", ids.toTypedArray(),
                "${CalendarContract.Instances.BEGIN} ASC",
            )?.use { c ->
                while (c.moveToNext() && out.size < max) {
                    val recurring = !c.getString(9).isNullOrBlank() || !c.getString(10).isNullOrBlank() || !c.isNull(11)
                    out += CalInstance(
                        eventId = c.getLong(0).toString(), begin = c.getLong(1), end = c.getLong(2),
                        title = c.getString(3), allDay = c.getInt(4) == 1, timeZone = c.getString(5),
                        location = c.getString(6), description = c.getString(7), calendarId = c.getLong(8).toString(),
                        recurring = recurring,
                    )
                }
            } ?: return null
            withDetails(out)
        } catch (_: SecurityException) {
            null
        }
    }

    /** Adds reminders and attendees with one bounded query per [PhoneDataLimits.DETAIL_BATCH] distinct events. */
    private fun withDetails(list: List<CalInstance>): List<CalInstance> {
        val ids = list.map { it.eventId }.distinct()
        val reminders = HashMap<String, MutableList<Int>>()
        val counts = HashMap<String, Int>()
        val names = HashMap<String, MutableList<String>>()
        for (batch in ids.chunked(PhoneDataLimits.DETAIL_BATCH)) {
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
class AndroidCalendarWriteStore(context: Context) : CalendarWriteStore {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    override fun calendar(id: String): CalendarAccess? {
        val cid = id.toLongOrNull() ?: return null
        return resolver.query(
            ContentUris.withAppendedId(CalendarContract.Calendars.CONTENT_URI, cid),
            arrayOf(CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL), null, null, null,
        )?.use { if (it.moveToFirst()) CalendarAccess(it.getInt(0) >= CalendarContract.Calendars.CAL_ACCESS_CONTRIBUTOR) else null }
    }

    override fun event(id: String): ExistingEvent? {
        val eid = id.toLongOrNull() ?: return null
        return resolver.query(
            ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, eid),
            arrayOf(
                CalendarContract.Events.CALENDAR_ID, CalendarContract.Events.RRULE, CalendarContract.Events.RDATE,
                CalendarContract.Events.ORIGINAL_ID, CalendarContract.Events.DELETED,
            ),
            null, null, null,
        )?.use {
            if (!it.moveToFirst() || it.getInt(4) != 0) null
            else ExistingEvent(
                calendarId = it.getLong(0).toString(),
                recurring = !it.getString(1).isNullOrBlank() || !it.getString(2).isNullOrBlank() || !it.isNull(3),
            )
        }
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

    private fun setReminder(eventId: Long, minutes: Int?) {
        resolver.delete(CalendarContract.Reminders.CONTENT_URI, "${CalendarContract.Reminders.EVENT_ID} = ?", arrayOf(eventId.toString()))
        if (minutes != null) {
            resolver.insert(
                CalendarContract.Reminders.CONTENT_URI,
                ContentValues().apply {
                    put(CalendarContract.Reminders.EVENT_ID, eventId)
                    put(CalendarContract.Reminders.MINUTES, minutes)
                    put(CalendarContract.Reminders.METHOD, CalendarContract.Reminders.METHOD_ALERT)
                },
            )
        }
    }

    override fun insert(calendarId: String, draft: EventDraft): String? {
        val v = values(draft).apply { put(CalendarContract.Events.CALENDAR_ID, calendarId.toLong()) }
        val uri = resolver.insert(CalendarContract.Events.CONTENT_URI, v) ?: return null
        val id = ContentUris.parseId(uri)
        if (draft.reminderMin != null) setReminder(id, draft.reminderMin)
        return id.toString()
    }

    override fun update(eventId: String, draft: EventDraft): Boolean {
        val id = eventId.toLongOrNull() ?: return false
        val n = resolver.update(ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, id), values(draft), null, null)
        if (n > 0) setReminder(id, draft.reminderMin)
        return n > 0
    }

    override fun delete(eventId: String): Boolean {
        val id = eventId.toLongOrNull() ?: return false
        return resolver.delete(ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, id), null, null) > 0
    }
}

class AndroidSmsPort(context: Context) : SmsPort {
    private val resolver: ContentResolver = context.applicationContext.contentResolver

    private val columns = arrayOf(
        Telephony.Sms._ID, Telephony.Sms.THREAD_ID, Telephony.Sms.ADDRESS, Telephony.Sms.BODY,
        Telephony.Sms.DATE, Telephony.Sms.TYPE, Telephony.Sms.READ, Telephony.Sms.SUBSCRIPTION_ID,
    )

    private fun query(selection: String?, args: Array<String>?, limit: Int): List<SmsRow> = try {
        val q = Bundle().apply {
            if (selection != null) putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            if (args != null) putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args)
            putStringArray(ContentResolver.QUERY_ARG_SORT_COLUMNS, arrayOf(Telephony.Sms.DATE))
            putInt(ContentResolver.QUERY_ARG_SORT_DIRECTION, ContentResolver.QUERY_SORT_DIRECTION_DESCENDING)
            putInt(ContentResolver.QUERY_ARG_LIMIT, limit)
        }
        val out = ArrayList<SmsRow>()
        resolver.query(Telephony.Sms.CONTENT_URI, columns, q, null)?.use { c ->
            while (c.moveToNext()) {
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
    } catch (_: SecurityException) {
        emptyList()
    }

    override fun latestPerThread(max: Int): List<SmsRow> {
        val seen = HashSet<Long>()
        val out = ArrayList<SmsRow>()
        for (r in query(null, null, 2000)) {
            if (seen.add(r.threadId)) out += r
            if (out.size >= max) break
        }
        return out
    }

    override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int): List<SmsRow> =
        if (rangeStartMs == null) query("${Telephony.Sms.THREAD_ID} = ?", arrayOf(threadId.toString()), limit)
        else query("${Telephony.Sms.THREAD_ID} = ? AND ${Telephony.Sms.DATE} <= ?", arrayOf(threadId.toString(), rangeStartMs.toString()), limit)

    /** Newest inbox row from [address] with this [body], for linking a just-received broadcast to its provider row. */
    fun findIncoming(address: String, body: String): SmsRow? =
        query("${Telephony.Sms.TYPE} = 1 AND ${Telephony.Sms.ADDRESS} = ?", arrayOf(address), 5).firstOrNull { it.body == body }
}

/** `SmsManager` behind [SmsSender]. Requires SEND_SMS (a SecurityException is mapped by the engine). */
@SuppressLint("MissingPermission", "UnspecifiedRegisterReceiverFlag")
class AndroidSmsSender(context: Context) : SmsSender {
    private val app = context.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val counter = AtomicInteger()

    override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {
        val manager = if (subId != null) SmsManager.getSmsManagerForSubscriptionId(subId)
        else app.getSystemService(SmsManager::class.java)
        val parts = manager.divideMessage(text)
        val n = counter.incrementAndGet()
        val sentAction = "${app.packageName}.SMS_SENT.$n"
        val deliveredAction = "${app.packageName}.SMS_DELIVERED.$n"
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
            if (Build.VERSION.SDK_INT >= 33) app.registerReceiver(r, filter, Context.RECEIVER_NOT_EXPORTED)
            else app.registerReceiver(r, filter)
        }
        register(sentAction) { tracker.onSent(it) }
        register(deliveredAction) { tracker.onDelivered(it) }
        // Never leave receivers behind if a report never comes (delivery reports are optional for carriers).
        main.postDelayed({ receivers.forEach { try { app.unregisterReceiver(it) } catch (_: IllegalArgumentException) { } } }, 15 * 60_000L)
        fun intents(action: String) = ArrayList<PendingIntent>(parts.size).also { l ->
            repeat(parts.size) {
                l += PendingIntent.getBroadcast(
                    app, n, Intent(action).setPackage(app.packageName), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            }
        }
        manager.sendMultipartTextMessage(address, null, parts, intents(sentAction), intents(deliveredAction))
        listener.onStatus("accepted")
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
        PhoneDataRuntime.init(context)
        PhoneDataRuntime.onIncomingBroadcast(address, body) { pending.finish() }
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
        )
        val sms = AndroidSmsPort(a)
        smsPort = sms
        engine = PhoneDataEngine(
            core = { core.dataAction(it) },
            features = { active() },
            calendarIds = { settings.calendarIds },
            contacts = AndroidContactsPort(a),
            calendar = AndroidCalendarPort(a),
            sms = sms,
            sender = AndroidSmsSender(a),
            calendarWriter = CalendarWriter(
                AndroidCalendarWriteStore(a),
                OpLedger(object : BlobStore {
                    override fun read() = p.getString("calendar_ops", "") ?: ""
                    override fun write(text: String) {
                        p.edit().putString("calendar_ops", text).apply()
                    }
                }),
            ) { settings.calendarIds },
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
        )
    }

    fun active(): ActiveFeatures {
        val a = app ?: return ActiveFeatures()
        return PhoneDataGate.active(settings, permissions(a))
    }

    /** Called when the transport is (re)started so the declared capabilities are what [active] says right now. */
    fun startFeatures(): ActiveFeatures = active().also { declared = it }

    fun setContacts(on: Boolean) = update { it.copy(contacts = on) }
    fun setCalendar(on: Boolean) = update { it.copy(calendar = on) }
    fun setCalendarEdit(on: Boolean) = update { it.copy(calendarEdit = on) }
    fun setSms(on: Boolean) = update { it.copy(sms = on) }
    fun setCalendarIds(ids: Set<String>) {
        update { it.copy(calendarIds = ids.take(PhoneDataLimits.MAX_CALENDARS).toSet()) }
        worker.execute { engine?.reset() } // selection changed: the next sync is a full snapshot
    }

    private fun update(change: (PhoneDataSettings) -> PhoneDataSettings) {
        settings = change(settings)
        prefs?.edit()
            ?.putBoolean("contacts", settings.contacts)?.putBoolean("calendar", settings.calendar)
            ?.putBoolean("sms", settings.sms)?.putStringSet("calendar_ids", settings.calendarIds)?.putBoolean("calendar_edit", settings.calendarEdit)?.apply()
        resync()
    }

    /** A permission or setting changed. Main thread. */
    fun resync() {
        val a = app ?: return
        val now = active()
        worker.execute { core.dataAction(PhoneDataProtocol.configRequest(now)) }
        if (!linkRunning) return
        if (now != declared) {
            // Capabilities are declared in the identity packet at connect time: reconnect to change them.
            declared = now
            val controller = LinkRuntime.controller()
            controller.stop("Messages, contacts or calendar sharing changed; reconnecting.")
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
        if (!running) {
            stopObserving()
            worker.execute { e.reset() }
        } else {
            worker.execute { e.reset() }
            startObserving()
        }
    }

    @Synchronized
    private fun startObserving() {
        val a = app ?: return
        val f = active()
        if (!f.any || !linkRunning) return
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
        if (waiter?.isAlive != true) {
            waiter = Thread({ waitLoop(e) }, "geekbridge-data-wait").apply { isDaemon = true; start() }
        }
    }

    private fun scheduleCalendarRetry(e: PhoneDataEngine) {
        synchronized(this) { if (retryScheduled) return; retryScheduled = true }
        main.postDelayed({
            synchronized(this) { retryScheduled = false }
            worker.execute { e.onCalendarChanged() }
        }, PhoneDataLimits.CALENDAR_PUSH_SPACING_MS + 1000)
    }

    private fun stopObserver() {
        observer?.let { try { app?.contentResolver?.unregisterContentObserver(it) } catch (_: RuntimeException) { } }
        observer = null
    }

    @Synchronized
    private fun stopObserving() = stopObserver()

    private fun waitLoop(e: PhoneDataEngine) {
        var since = 0L
        while (!Thread.currentThread().isInterrupted) {
            val o = core.dataAction(PhoneDataProtocol.waitRequest(since))
            if (o !is CoreOutcome.Ok) return // not running: restarted when the link comes back
            val r = PhoneDataProtocol.parseWait(o.result) ?: return
            if (r.stopped) return
            since = maxOf(since, r.seq, r.events.maxOfOrNull { it.seq } ?: 0L)
            if (r.events.isNotEmpty()) e.onEvents(r.events)
            if (!linkRunning || !active().any) return
        }
    }

    /** Incoming text: wait for Android's SMS provider to store it (the default app writes it), then push it. */
    fun onIncomingBroadcast(address: String, body: String, done: () -> Unit) {
        val e = engine
        val port = smsPort
        if (e == null || port == null || !linkRunning || !active().sms) return done()
        worker.execute {
            try {
                repeat(3) {
                    Thread.sleep(1500)
                    val row = port.findIncoming(address, body)
                    if (row != null) {
                        e.onIncomingSms(row)
                        return@execute
                    }
                }
            } catch (_: InterruptedException) {
            } finally {
                done()
            }
        }
    }

    /** Calendars for the picker (needs READ_CALENDAR). Off the main thread. */
    fun calendars(): List<CalendarInfo> = app?.let { AndroidCalendarPort(it).calendars() }.orEmpty()
}

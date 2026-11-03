// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.annotation.SuppressLint
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.ContactsContract
import android.telecom.TelecomManager
import android.telephony.TelephonyManager
import java.util.concurrent.Executors

/** `TelecomManager` behind [TelecomPort]. Both calls are guarded by ANSWER_PHONE_CALLS (checked by the caller). */
class AndroidTelecom(context: Context) : TelecomPort {
    private val telecom: TelecomManager? = context.applicationContext.getSystemService(TelecomManager::class.java)

    @Suppress("DEPRECATION")
    @SuppressLint("MissingPermission") // CallRules.handle checks ANSWER_PHONE_CALLS first; SecurityException is mapped anyway
    override fun acceptRingingCall() {
        (telecom ?: throw IllegalStateException("No telecom service")).acceptRingingCall()
    }

    @SuppressLint("MissingPermission") // CallRules.handle checks CALL_PHONE first; SecurityException is mapped anyway
    override fun placeCall(number: String) {
        val t = telecom ?: throw IllegalStateException("No telecom service")
        // A tel: URI built from parts, so the number is data and never parsed as anything else.
        t.placeCall(Uri.fromParts("tel", number, null), android.os.Bundle())
    }

    @Suppress("DEPRECATION")
    @SuppressLint("MissingPermission", "NewApi") // reachable only when CallRules.canEnd (API 28+) allowed it
    override fun endCall(): Boolean {
        val t = telecom ?: throw IllegalStateException("No telecom service")
        return t.endCall()
    }
}

/**
 * The phone's own notice and question for computer-requested dials: a heads-up notification (the text carries the
 * number and computer, nothing else) plus a toast for the notice. Needs the notification permission to ask.
 */
class AndroidDialPrompts(private val ctx: Context) : DialPrompts {
    private val main = Handler(Looper.getMainLooper())

    private fun manager(): android.app.NotificationManager {
        val nm = ctx.getSystemService(android.app.NotificationManager::class.java)
        nm.createNotificationChannel(
            android.app.NotificationChannel(CHANNEL, ctx.getString(R.string.channel_dial), android.app.NotificationManager.IMPORTANCE_HIGH).apply {
                description = ctx.getString(R.string.channel_dial_desc)
                setShowBadge(false)
            },
        )
        return nm
    }

    private fun id(requestId: String) = 0x4000_0000 or (requestId.hashCode() and 0x3fff_ffff)

    override fun announce(text: String) {
        main.post { android.widget.Toast.makeText(ctx, text, android.widget.Toast.LENGTH_LONG).show() }
        try {
            val n = android.app.Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_link)
                .setContentTitle(ctx.getString(R.string.dial_notice_title))
                .setContentText(text)
                .setAutoCancel(true)
                .setCategory(android.app.Notification.CATEGORY_STATUS)
                .setTimeoutAfter(10 * 60_000L)
                .build()
            manager().notify(NOTICE_ID, n)
        } catch (_: SecurityException) {
            // Notifications not allowed: the toast above is all there is.
        }
    }

    override fun ask(requestId: String, text: String): Boolean {
        fun action(allow: Boolean, label: Int): android.app.Notification.Action {
            val i = Intent(ctx, DialDecisionReceiver::class.java)
                .setAction(if (allow) DialDecisionReceiver.ACTION_ALLOW else DialDecisionReceiver.ACTION_DENY)
                .putExtra(DialDecisionReceiver.EXTRA_REQUEST, requestId)
            val pi = android.app.PendingIntent.getBroadcast(
                ctx, id(requestId) + if (allow) 1 else 2, i,
                android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT,
            )
            return android.app.Notification.Action.Builder(null, ctx.getString(label), pi).build()
        }
        return try {
            val n = android.app.Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_link)
                .setContentTitle(ctx.getString(R.string.dial_confirm_title))
                .setContentText(text)
                .addAction(action(false, R.string.dial_deny))
                .addAction(action(true, R.string.dial_allow))
                .setCategory(android.app.Notification.CATEGORY_CALL)
                .setTimeoutAfter(45_000L)
                .build()
            manager().notify(id(requestId), n)
            true
        } catch (_: SecurityException) {
            false
        }
    }

    override fun withdraw(requestId: String) {
        try {
            manager().cancel(id(requestId))
        } catch (_: SecurityException) {
        }
    }

    private companion object {
        const val CHANNEL = "geekbridge_dial"
        const val NOTICE_ID = 0x4000_0000
    }
}

/** Looks a number up in this phone's contacts. Read on the phone only; the name goes to the paired computer. */
class AndroidContactLookup(context: Context) : ContactLookup {
    private val resolver = context.applicationContext.contentResolver

    override fun nameFor(number: String): String? {
        val uri = Uri.withAppendedPath(ContactsContract.PhoneLookup.CONTENT_FILTER_URI, Uri.encode(number))
        return try {
            resolver.query(uri, arrayOf(ContactsContract.PhoneLookup.DISPLAY_NAME), null, null, null)?.use { c ->
                if (c.moveToFirst()) c.getString(0) else null
            }
        } catch (e: SecurityException) {
            null
        }
    }
}

/** The active SIMs (slot, eSIM flag) and the default voice subscription. Needs READ_PHONE_STATE; empty without it. */
object SubscriptionReader {
    @SuppressLint("MissingPermission") // READ_PHONE_STATE is checked by the callers; a SecurityException gives an empty list
    fun active(ctx: Context): List<ActiveSim> {
        val sm = ctx.applicationContext.getSystemService(android.telephony.SubscriptionManager::class.java) ?: return emptyList()
        return try {
            sm.activeSubscriptionInfoList.orEmpty().map { ActiveSim(it.subscriptionId, it.simSlotIndex, Build.VERSION.SDK_INT >= 28 && it.isEmbedded) }
        } catch (_: SecurityException) {
            emptyList()
        } catch (_: RuntimeException) {
            emptyList()
        }
    }

    fun defaultVoiceId(): Int? = try {
        android.telephony.SubscriptionManager.getDefaultVoiceSubscriptionId()
    } catch (_: RuntimeException) {
        null
    }

    fun summary(ctx: Context): String = SimText.summary(active(ctx), defaultVoiceId())
}

/**
 * Every place Android reports a call state: the default subscription's callback, one callback per active SIM
 * (re-made when the SIM set changes), and the phone-state broadcast, which covers all SIMs. A listener on the default
 * subscription alone can miss a call that rings on the other SIM (dual SIM / eSIM).
 */
@Suppress("UseRequiresApi", "DEPRECATION")
class AndroidCallSources(
    context: Context,
    private val sdk: Int = Build.VERSION.SDK_INT,
    private val ledger: CallListenerLedger,
) : CallSourceRegistrar {
    private val app = context.applicationContext
    private val registrar = TelephonyRegistrar(app, sdk)

    @android.annotation.TargetApi(30)
    private fun addWithExecutor(
        sm: android.telephony.SubscriptionManager,
        executor: java.util.concurrent.Executor,
        l: android.telephony.SubscriptionManager.OnSubscriptionsChangedListener,
    ) = sm.addOnSubscriptionsChangedListener(executor, l)

    override fun register(
        executor: java.util.concurrent.Executor,
        onSource: (String, Int) -> Unit,
        onGone: (String) -> Unit,
    ): AutoCloseable {
        val lock = Any()
        var closed = false
        val subs = LinkedHashMap<Int, Pair<String, AutoCloseable>>()
        val base = registrar.telephony ?: throw IllegalStateException("No telephony service")

        val defaultHandle = registrar.registerOn(base, executor) { onSource("default", it) }

        fun closeQuietly(c: AutoCloseable) = try { c.close() } catch (_: RuntimeException) { }

        fun sync() {
            val sims = SubscriptionReader.active(app)
            synchronized(lock) {
                if (closed) return
                val want = sims.associateBy { it.subId }
                for ((id, entry) in subs.toList()) {
                    if (id in want) continue
                    closeQuietly(entry.second)
                    subs.remove(id)
                    ledger.sourceGone(entry.first)
                    onGone(entry.first)
                }
                for (sim in sims) {
                    if (sim.subId in subs) continue
                    try {
                        val tm = base.createForSubscriptionId(sim.subId)
                        subs[sim.subId] = sim.sourceName to registrar.registerOn(tm, executor) { onSource(sim.sourceName, it) }
                    } catch (e: RuntimeException) {
                        ledger.sourceFailed(sim.sourceName, e.javaClass.simpleName)
                    }
                }
                ledger.subscriptions(SimText.summary(sims, SubscriptionReader.defaultVoiceId()))
            }
        }

        var subListener: android.telephony.SubscriptionManager.OnSubscriptionsChangedListener? = null
        val sm = app.getSystemService(android.telephony.SubscriptionManager::class.java)
        try {
            if (sm != null) {
                if (sdk >= 30) {
                    val l = object : android.telephony.SubscriptionManager.OnSubscriptionsChangedListener() {
                        override fun onSubscriptionsChanged() = sync()
                    }
                    addWithExecutor(sm, executor, l)
                    subListener = l
                } else {
                    // Below 30 the listener is bound to the creating thread's Looper: the main thread, from onStart.
                    val l = object : android.telephony.SubscriptionManager.OnSubscriptionsChangedListener() {
                        override fun onSubscriptionsChanged() = sync()
                    }
                    sm.addOnSubscriptionsChangedListener(l)
                    subListener = l
                }
            }
        } catch (e: RuntimeException) {
            ledger.sourceFailed("subscriptions", e.javaClass.simpleName)
        }
        sync() // the listener also fires once on registration; this makes the first read certain

        var receiver: BroadcastReceiver? = null
        try {
            val r = object : BroadcastReceiver() {
                override fun onReceive(context: Context, intent: Intent) {
                    if (intent.action != TelephonyManager.ACTION_PHONE_STATE_CHANGED) return
                    val raw = SimText.fromBroadcast(intent.getStringExtra(TelephonyManager.EXTRA_STATE)) ?: return
                    onSource("broadcast", raw)
                }
            }
            val filter = IntentFilter(TelephonyManager.ACTION_PHONE_STATE_CHANGED)
            if (sdk >= 33) {
                app.registerReceiver(r, filter, Context.RECEIVER_NOT_EXPORTED)
            } else {
                // Exported below Android 13: accept only broadcasts from a sender holding the system-only permission.
                app.registerReceiver(r, filter, CallRules.PHONE_STATE_SENDER_PERMISSION, null)
            }
            receiver = r
        } catch (e: RuntimeException) {
            ledger.sourceFailed("broadcast", e.javaClass.simpleName)
        }

        return AutoCloseable {
            synchronized(lock) {
                closed = true
                for (entry in subs.values) closeQuietly(entry.second)
                subs.clear()
            }
            closeQuietly(defaultHandle)
            subListener?.let { l -> try { sm?.removeOnSubscriptionsChangedListener(l) } catch (_: RuntimeException) { } }
            receiver?.let { r -> try { app.unregisterReceiver(r) } catch (_: IllegalArgumentException) { } }
        }
    }
}

object CallPermissionsReader {
    fun read(ctx: Context): CallPermissions {
        fun has(p: String) = ctx.checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED
        return CallPermissions(
            readPhoneState = has(Manifest.permission.READ_PHONE_STATE),
            answerPhoneCalls = has(Manifest.permission.ANSWER_PHONE_CALLS),
            readCallLog = has(Manifest.permission.READ_CALL_LOG),
            readContacts = has(Manifest.permission.READ_CONTACTS),
            callPhone = has(Manifest.permission.CALL_PHONE),
        )
    }
}

/**
 * Process-wide wiring of call control: the user's opt-in, the [CallControlEngine], the coarse state monitor,
 * the incoming-number receiver (only with READ_CALL_LOG) and the one thread that waits for the computer's
 * requests (it parks inside the native core, no polling). Everything is idle while the setting is off or
 * the link is down.
 */
@SuppressLint("StaticFieldLeak") // only the application context is held
object CallControlRuntime {
    private const val FILE = "geekbridge_calls"

    @Volatile private var prefs: SharedPreferences? = null
    @Volatile private var app: Context? = null
    @Volatile var settings: CallLinkSettings = CallLinkSettings()
        private set
    @Volatile private var engine: CallControlEngine? = null
    private val main = Handler(Looper.getMainLooper())
    private val core: CoreClient by lazy { CoreClient.native() }
    private var monitor: CallStateMonitor? = null
    private var monitorExecutor: java.util.concurrent.ExecutorService? = null

    /** Debug aid: what the call-state listener did (registration, callbacks, reports). Codes and counts only. */
    val ledger = CallListenerLedger()
    private var receiver: BroadcastReceiver? = null
    private var waiter: Thread? = null
    private var linkRunning = false

    /** Native calls and telephony lookups, off the main thread, in order. */
    private val sender = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-calls-send").apply { isDaemon = true } }

    @Synchronized
    fun init(ctx: Context) {
        if (app != null) return
        val a = ctx.applicationContext
        app = a
        val p = a.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        prefs = p
        settings = CallLinkSettings(enabled = p.getBoolean("enabled", false), confirmDial = p.getBoolean("confirm_dial", false))
        engine = CallControlEngine(
            core = { core.transportAction(it) },
            telecom = AndroidTelecom(a),
            permissions = { CallPermissionsReader.read(a) },
            settings = { settings },
            lookup = AndroidContactLookup(a),
            sdk = Build.VERSION.SDK_INT,
            strings = ResStrings(a.resources),
            onReport = { ledger.reported(it) },
            prompts = AndroidDialPrompts(a),
        )
    }

    /** The user chose whether the phone asks before placing a call a computer requested. Main thread. */
    fun setConfirmDial(on: Boolean) {
        settings = settings.copy(confirmDial = on)
        prefs?.edit()?.putBoolean("confirm_dial", on)?.apply()
    }

    /** The owner answered the phone's "place this call?" notification. Any thread. */
    fun resolveDial(requestId: String, allow: Boolean) {
        val e = engine ?: return
        async { e.resolveDial(requestId, allow) }
    }

    private fun async(block: () -> Unit) {
        sender.execute {
            try {
                block()
            } catch (_: RuntimeException) {
                // A failed call report must never take the process down.
            }
        }
    }

    /** The user switched call control on or off. Main thread. */
    fun setEnabled(on: Boolean) {
        val e = engine ?: return
        settings = settings.copy(enabled = on)
        prefs?.edit()?.putBoolean("enabled", on)?.apply()
        app?.let { WidgetUpdater.refreshAll(it) }
        async { core.transportAction(CallsProtocol.configRequest(on)) }
        if (on) {
            if (linkRunning) startObserving(e)
        } else {
            stopObserving("calls_off")
            async { e.reset() }
        }
    }

    /** Called (main thread) whenever the transport state changes. */
    @Synchronized
    fun onLinkChanged(running: Boolean) {
        val e = engine ?: return
        val was = linkRunning
        linkRunning = running
        // The status observer fires every couple of seconds with the same phase. A repeat must not reset the engine:
        // that wiped the state of a ringing call and made every Answer fail with "no such call" (soak finding).
        if (running == was) {
            if (running && settings.enabled) {
                ensureWaiter(e)
                // A registration that failed or was lost is not final: try again, spaced, while it is wanted.
                if (monitor == null && ListenerRetry.due(ledger.lastAttemptAt(), System.currentTimeMillis())) ensureMonitor(e, "retry")
            }
            return
        }
        if (!running) {
            stopObserving("link_down")
            async { e.reset() }
        } else if (settings.enabled) {
            startObserving(e)
        }
    }

    /** A permission changed (setup screen came back to the foreground). Main thread. */
    fun onPermissionsChanged() {
        val e = engine ?: return
        if (!settings.enabled || !linkRunning) return
        stopObserving("permissions_changed")
        startObserving(e)
        async { e.permissionsChanged() }
    }

    @Synchronized
    private fun startObserving(e: CallControlEngine) {
        val a = app ?: return
        async { e.reset() }
        async { core.transportAction(CallsProtocol.configRequest(true)) } // the core forgets the opt-in when the process dies
        val perms = CallPermissionsReader.read(a)
        ensureMonitor(e, "link or setting")
        if (receiver == null && perms.readCallLog && perms.readPhoneState) registerNumberReceiver(a, e)
        ensureWaiter(e)
    }

    @Synchronized
    private fun ensureMonitor(e: CallControlEngine, why: String) {
        val a = app ?: return
        if (monitor != null) return
        if (!CallPermissionsReader.read(a).readPhoneState) {
            ledger.notRegistered("no_read_phone_state")
            return
        }
        val exec = CallStateExecutor.create()
        val m = CallStateMonitor(
            registrar = MergingCallRegistrar(AndroidCallSources(a, ledger = ledger)) { name, raw -> ledger.source(name, raw) },
            executor = exec,
            postToMain = { main.post(it) },
            onState = { s ->
                ledger.delivered(s)
                WidgetUpdater.onCall(s == CallState.Ringing)
                async { e.onState(s) }
            },
            onRawSeen = { ledger.raw(it) },
        )
        when (val r = m.start()) {
            is MonitorStart.Failed -> {
                exec.shutdownNow()
                ledger.notRegistered(r.code)
            }
            else -> {
                monitor = m
                monitorExecutor = exec
                ledger.registered(ListenerStrategy.forSdk(Build.VERSION.SDK_INT))
                GbLog.i("calls", "registration trigger: $why")
            }
        }
    }

    private fun ensureWaiter(e: CallControlEngine) {
        if (waiter?.isAlive != true) {
            waiter = Thread({ waitLoop(e) }, "geekbridge-calls-wait").apply { isDaemon = true; start() }
        }
    }

    @Synchronized
    private fun stopObserving(why: String) {
        if (monitor != null) ledger.notRegistered(why)
        monitor?.stop()
        monitor = null
        monitorExecutor?.shutdownNow() // one thread per registration, not one per link restart
        monitorExecutor = null
        val a = app
        receiver?.let { r -> try { a?.unregisterReceiver(r) } catch (_: IllegalArgumentException) { } }
        receiver = null
        // The wait loop ends by itself: switching the feature off wakes it, and a stopping core ends
        // it with `stopped`.
    }

    @Suppress("DEPRECATION") // EXTRA_INCOMING_NUMBER is deprecated but is the only number source without a default-dialer role
    private fun registerNumberReceiver(a: Context, e: CallControlEngine) {
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != TelephonyManager.ACTION_PHONE_STATE_CHANGED) return
                val number = intent.getStringExtra(TelephonyManager.EXTRA_INCOMING_NUMBER) ?: return
                async { e.onNumber(number) }
            }
        }
        val filter = IntentFilter(TelephonyManager.ACTION_PHONE_STATE_CHANGED)
        if (Build.VERSION.SDK_INT >= 33) {
            a.registerReceiver(r, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            // Exported below Android 13: accept only broadcasts from a sender holding the system-only permission.
            a.registerReceiver(r, filter, CallRules.PHONE_STATE_SENDER_PERMISSION, null)
        }
        receiver = r
    }

    private fun waitLoop(e: CallControlEngine) {
        var since = 0L
        while (!Thread.currentThread().isInterrupted) {
            val o = core.transportAction(CallsProtocol.waitRequest(since))
            if (o !is CoreOutcome.Ok) return // not running: restarted when the link comes back
            val r = CallsProtocol.parseWait(o.result) ?: return
            if (r.stopped) return
            since = maxOf(since, r.lastSeq)
            if (r.requests.isNotEmpty()) {
                try {
                    e.onRequests(r.requests)
                } catch (_: RuntimeException) {
                    // One bad request must not end the loop.
                }
            }
            if (!settings.enabled || !linkRunning) return
        }
    }
}

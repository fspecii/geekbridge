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
    private val app = context.applicationContext
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
        // placeCall needs no activity start, so it is allowed from the background service (Android 10+ restrictions
        // only apply to startActivity). With "ask every time" and several SIMs it would open the system's SIM picker
        // on a phone nobody is looking at, so the default voice subscription's account is passed when it can be told.
        val extras = android.os.Bundle()
        accountFor(t)?.let { extras.putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, it) }
        t.placeCall(Uri.fromParts("tel", number, null), extras)
    }

    @SuppressLint("MissingPermission") // READ_PHONE_STATE is a precondition of dialing (CallRules.canDial); failures mean "let Android choose"
    private fun accountFor(t: TelecomManager): android.telecom.PhoneAccountHandle? = try {
        val hasDefault = t.getDefaultOutgoingPhoneAccount("tel") != null
        if (hasDefault) {
            null
        } else {
            val tm = app.getSystemService(TelephonyManager::class.java)
            val accounts = t.callCapablePhoneAccounts.map { h ->
                h to (if (Build.VERSION.SDK_INT >= 30) tm?.createForPhoneAccountHandle(h)?.subscriptionId else null)
            }
            DialAccounts.choose(false, accounts, SubscriptionReader.defaultVoiceId())
        }
    } catch (_: RuntimeException) {
        null
    }

    @Suppress("DEPRECATION")
    @SuppressLint("MissingPermission", "NewApi") // reachable only when CallRules.canEnd (API 28+) allowed it
    override fun endCall(): Boolean {
        val t = telecom ?: throw IllegalStateException("No telecom service")
        return t.endCall()
    }

    override fun canPlayTones(): Boolean = InCallBridge.active() != null

    override fun playTone(tone: Char): Boolean = InCallBridge.play(tone)
}

/**
 * The live call as the in-call service sees it, for keypad tones. `TelecomManager` has no tone call; `Call.playDtmfTone`
 * exists only on the `Call` objects Android hands to the bound [GeekBridgeInCallService], and Android binds that only
 * for the app the user made the phone app. Nothing is kept but the `Call` handle; tones are never logged.
 */
object InCallBridge {
    private val main = Handler(Looper.getMainLooper())
    private val lock = Any()
    private val calls = LinkedHashSet<android.telecom.Call>()

    /** Told (off the main thread is not guaranteed) when a call appears or goes, so the report can change. */
    @Volatile var onChange: () -> Unit = {}

    /** How long a tone sounds. Long enough for a phone menu to hear it, short enough to type a PIN quickly. */
    private const val TONE_MS = 150L

    /** Silence between queued tones, so two equal digits are heard as two. */
    private const val GAP_MS = 60L

    fun added(c: android.telecom.Call) {
        synchronized(lock) { calls.add(c) }
        onChange()
    }

    /** A tracked call changed state (for example dialing -> active): the report may now differ. */
    fun changed() = onChange()

    fun removed(c: android.telecom.Call) {
        synchronized(lock) { calls.remove(c) }
        onChange()
    }

    fun clear() {
        synchronized(lock) { calls.clear() }
        onChange()
    }

    /** The one call tones go to: the active one, and only if it is the only one that is. */
    @Suppress("DEPRECATION") // Call.getState: still the plain answer on every API level
    fun active(): android.telecom.Call? = synchronized(lock) {
        calls.filter { it.state == android.telecom.Call.STATE_ACTIVE }.singleOrNull()
    }

    fun play(tone: Char): Boolean {
        if (!CallRules.validTone(tone)) return false
        val c = active() ?: return false
        // Queued: each tone starts after the previous one has stopped, so fast typing is played in order, not garbled.
        val now = android.os.SystemClock.uptimeMillis()
        val start = schedule.reserve(now)
        main.postAtTime({
            try {
                c.playDtmfTone(tone)
                main.postDelayed({
                    try {
                        c.stopDtmfTone()
                    } catch (_: RuntimeException) {
                    }
                }, TONE_MS)
            } catch (_: RuntimeException) {
            }
        }, start)
        return true
    }

    private val schedule = ToneSchedule(TONE_MS, GAP_MS)
}

/** See [InCallBridge]. Has no screen; only hands the calls it is given to the bridge. */
class GeekBridgeInCallService : android.telecom.InCallService() {
    // A call placed from the computer is added while still dialing and turns ACTIVE later; without a re-report the
    // phone would keep saying it cannot play tones for the whole call. Each call's callback is dropped with the call.
    private val watchers = HashMap<android.telecom.Call, android.telecom.Call.Callback>()

    override fun onCallAdded(call: android.telecom.Call) {
        val cb = object : android.telecom.Call.Callback() {
            override fun onStateChanged(c: android.telecom.Call, state: Int) = InCallBridge.changed()
        }
        watchers[call] = cb
        call.registerCallback(cb)
        InCallBridge.added(call)
    }

    override fun onCallRemoved(call: android.telecom.Call) {
        watchers.remove(call)?.let { call.unregisterCallback(it) }
        InCallBridge.removed(call)
    }

    override fun onDestroy() {
        for ((c, cb) in watchers) {
            try { c.unregisterCallback(cb) } catch (_: RuntimeException) { }
        }
        watchers.clear()
        InCallBridge.clear()
        super.onDestroy()
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
        // Without POST_NOTIFICATIONS (Android 13+) or with the channel switched off, notify() succeeds and shows
        // nothing: the owner could never answer. Say so now so the computer hears "declined" at once, not after 45 s.
        val nm = manager()
        if (!DialPromptRules.canShow(nm.areNotificationsEnabled(), nm.getNotificationChannel(CHANNEL)?.importance)) return false
        fun action(allow: Boolean, label: Int): android.app.Notification.Action {
            val i = Intent(ctx, DialDecisionReceiver::class.java)
                .setAction(if (allow) DialDecisionReceiver.ACTION_ALLOW else DialDecisionReceiver.ACTION_DENY)
                .putExtra(DialDecisionReceiver.EXTRA_REQUEST, requestId)
            val pi = android.app.PendingIntent.getBroadcast(
                ctx, id(requestId) + if (allow) 1 else 2, i,
                android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT,
            )
            val b = android.app.Notification.Action.Builder(null, ctx.getString(label), pi)
            // Allowing a call from the lock screen needs the owner's unlock (Android 12+); Deny never does.
            if (allow && Build.VERSION.SDK_INT >= 31) b.setAuthenticationRequired(true)
            return b.build()
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
        val base = registrar.telephony ?: throw IllegalStateException("No telephony service")
        val defaultHandle = registrar.registerOn(base, executor) { onSource("default", it) }
        // If anything after this point throws, the default callback must not stay registered with no one to close it.
        try {
            return registerRest(base, defaultHandle, executor, onSource, onGone)
        } catch (t: Throwable) {
            try { defaultHandle.close() } catch (_: RuntimeException) { }
            throw t
        }
    }

    private fun registerRest(
        base: TelephonyManager,
        defaultHandle: AutoCloseable,
        executor: java.util.concurrent.Executor,
        onSource: (String, Int) -> Unit,
        onGone: (String) -> Unit,
    ): AutoCloseable {
        val lock = Any()
        var closed = false
        val subs = LinkedHashMap<Int, Pair<String, AutoCloseable>>()
        val slots = HashMap<Int, Int>() // subId -> slot, to drop that slot's broadcast state with its SIM

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
                    slots.remove(id)?.let { onGone(SimText.broadcastSource(it)) }
                }
                for (sim in sims) {
                    if (sim.subId in subs) continue
                    try {
                        val tm = base.createForSubscriptionId(sim.subId)
                        slots[sim.subId] = sim.slot
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
                    onSource(SimText.broadcastSource(intent.getIntExtra("slot", -1)), raw)
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
    private var monitorStarting = false

    /** Debug aid: what the call-state listener did (registration, callbacks, reports). Codes and counts only. */
    val ledger = CallListenerLedger()
    private var receiver: BroadcastReceiver? = null
    private var waiter: Thread? = null
    private var linkRunning = false
    private val watch = PermissionWatch()

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
        InCallBridge.onChange = { engine?.let { e -> async { e.tonesChanged() } } }
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
                recheckPermissions()
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

    /**
     * A permission granted or revoked outside the setup screen (system Settings) is noticed here, from the places the
     * app comes back to or the link reports in: the listener, the number receiver and the capabilities are redone
     * at once instead of waiting for a restart. Main thread.
     */
    fun recheckPermissions() {
        val a = app ?: return
        if (!settings.enabled || !linkRunning) return
        if (watch.changed(CallPermissionsReader.read(a))) onPermissionsChanged()
    }

    @Synchronized
    private fun startObserving(e: CallControlEngine) {
        val a = app ?: return
        watch.reset()
        watch.changed(CallPermissionsReader.read(a)) // baseline for recheckPermissions
        async { e.reset() }
        async { core.transportAction(CallsProtocol.configRequest(true)) } // the core forgets the opt-in when the process dies
        val perms = CallPermissionsReader.read(a)
        ensureMonitor(e, "link or setting")
        if (receiver == null && perms.readCallLog && perms.readPhoneState) registerNumberReceiver(a, e)
        ensureWaiter(e)
    }

    /**
     * Registering telephony callbacks can block on the telephony service, so from API 30 (where listeners take an
     * executor) it runs on the worker thread, not the main thread. Below 30 the subscription listener is bound to the
     * creating thread's Looper, so it stays on the main thread there.
     */
    private fun ensureMonitor(e: CallControlEngine, why: String) {
        if (Build.VERSION.SDK_INT >= 30) async { ensureMonitorNow(e, why) } else ensureMonitorNow(e, why)
    }

    /**
     * The registration itself (which can block on the telephony service) runs outside the object's lock, so the main
     * thread's [onLinkChanged] never waits behind it; only the checks before and the publishing after hold the lock.
     */
    private fun ensureMonitorNow(e: CallControlEngine, why: String) {
        val a = app ?: return
        val go = synchronized(this) {
            when {
                // Queued earlier, run later: do nothing if the feature or the link went away in between.
                monitor != null || monitorStarting || !settings.enabled || !linkRunning -> false
                !CallPermissionsReader.read(a).readPhoneState -> {
                    ledger.notRegistered("no_read_phone_state")
                    false
                }
                else -> {
                    monitorStarting = true
                    true
                }
            }
        }
        if (!go) return
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
        val r = try {
            m.start()
        } catch (t: Throwable) {
            synchronized(this) { monitorStarting = false }
            exec.shutdownNow()
            throw t
        }
        synchronized(this) {
            monitorStarting = false
            when {
                r is MonitorStart.Failed -> {
                    exec.shutdownNow()
                    ledger.notRegistered(r.code)
                }
                !settings.enabled || !linkRunning -> {
                    // Turned off while registering: undo it.
                    m.stop()
                    exec.shutdownNow()
                    ledger.notRegistered("stopped_while_starting")
                }
                else -> {
                    monitor = m
                    monitorExecutor = exec
                    ledger.registered(ListenerStrategy.forSdk(Build.VERSION.SDK_INT))
                    GbLog.i("calls", "registration trigger: $why")
                }
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

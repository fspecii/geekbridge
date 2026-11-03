// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.content.SharedPreferences

/**
 * Small, non-sensitive app state kept across restarts: whether setup was finished, whether to stay connected
 * in the background, how many computers are paired (so the boot receiver and service can decide without
 * starting the native core) and which permissions were asked or worked before. Never stores contacts,
 * calendar, clipboard or message content. Initialise with [init] from any entry point (idempotent).
 */
object AppState {
    private const val FILE = "geekbridge_state"
    private const val MAX_NAMES = 8
    private const val NAME_CHARS = 48

    @Volatile private var prefs: SharedPreferences? = null
    @Volatile private var appContext: Context? = null

    fun init(ctx: Context) {
        if (prefs == null) synchronized(this) {
            if (prefs == null) {
                appContext = ctx.applicationContext
                prefs = ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
                // Call control reads its own opt-in here so StartOptions can negotiate it on every start path.
                CallControlRuntime.init(ctx.applicationContext)
                PhoneDataRuntime.init(ctx.applicationContext)
                ComputerGrantsRuntime.init(ctx.applicationContext)
            }
        }
    }

    private fun p(): SharedPreferences? = prefs

    var setupFinished: Boolean
        get() = p()?.getBoolean("setup_finished", false) ?: false
        set(v) { p()?.edit()?.putBoolean("setup_finished", v)?.apply() }

    /** Default on: a paired computer should stay reachable unless the user turns it off. */
    var keepConnected: Boolean
        get() = p()?.getBoolean("keep_connected", true) ?: true
        set(v) { p()?.edit()?.putBoolean("keep_connected", v)?.apply() }

    /**
     * Kill switch for binding the core's dials and announcements to the Wi-Fi address (default on, see
     * [WifiBindPolicy]). Only the debug build can change it (adb broadcast or the debug card); the core reads it from
     * the app on every service start.
     */
    var bindWifi: Boolean
        get() = p()?.getBoolean("bind_wifi", true) ?: true
        set(v) { p()?.edit()?.putBoolean("bind_wifi", v)?.apply() }

    @Volatile private var pairingUntil = 0L

    /** True while a pairing started in the last few minutes may keep the background link alive (not persisted). */
    fun pairingGrace(): Boolean = android.os.SystemClock.elapsedRealtime() < pairingUntil
    fun pairingGraceLeftMs(): Long = (pairingUntil - android.os.SystemClock.elapsedRealtime()).coerceAtLeast(0)
    fun startPairingGrace() { pairingUntil = android.os.SystemClock.elapsedRealtime() + ServicePolicy.PAIRING_GRACE_MS }
    fun endPairingGrace() { pairingUntil = 0 }

    /**
     * A pairing code is waiting for a decision (whichever screen shows it). The user usually looks at the other
     * computer to compare it, so the phone screen may lock or the app may be left before they answer: keep the link
     * alive for a few minutes through the background service.
     */
    fun pairingPromptSeen() {
        val fresh = !pairingGrace()
        startPairingGrace()
        if (fresh) appContext?.let { LinkService.sync(it) }
    }

    /** Should the foreground service hold the link right now? */
    fun wantService(): Boolean = ServicePolicy.shouldRun(pairedCount, keepConnected, pairingGrace(), sharing)

    /** A share to a computer is running: the foreground service holds the process until it is over, whatever "Stay connected" says. */
    @Volatile var sharing = false

    /** Why the foreground service could not start last time (shown in Diagnostics); null if it did. */
    @Volatile var serviceError: String? = null

    var oemAcknowledged: Boolean
        get() = p()?.getBoolean("oem_ack", false) ?: false
        set(v) { p()?.edit()?.putBoolean("oem_ack", v)?.apply() }

    val pairedCount: Int get() = p()?.getInt("paired_count", 0) ?: 0

    val pairedNames: List<String>
        get() = p()?.getString("paired_names", "")?.split('\n')?.filter { it.isNotEmpty() }.orEmpty()

    /** Paired computers as (device id, name), for the per-computer allow lists. */
    val pairedPeers: List<Pair<String, String>>
        get() = p()?.getString("paired_peers", "")?.split('\n')?.mapNotNull { l ->
            l.split('\t', limit = 2).takeIf { it.size == 2 && it[0].isNotEmpty() }?.let { it[0] to it[1] }
        }.orEmpty()

    /** Called with every readable status snapshot; an unreadable one ([readable] false) changes nothing. */
    fun recordPaired(paired: List<PairedPeer>, readable: Boolean) {
        if (!readable) return
        val names = paired.take(MAX_NAMES).map { it.name.ifEmpty { "Computer" }.replace('\n', ' ').take(NAME_CHARS) }
        ComputerGrantsRuntime.onPaired(paired)
        val before = pairedCount
        // The first successful pairing turns "Stay connected" on explicitly (the Home toggle shows it and can undo it).
        if (before == 0 && paired.isNotEmpty()) { keepConnected = true; endPairingGrace() }
        p()?.edit()?.putInt("paired_count", paired.size)?.putString("paired_names", names.joinToString("\n"))
            ?.putString("paired_peers", paired.take(MAX_NAMES).joinToString("\n") { it.deviceId + "\t" + it.name.ifEmpty { "Computer" }.replace(Regex("[\\n\\t]"), " ").take(NAME_CHARS) })?.apply()
        // Starting or stopping the background connection follows the paired count (event-driven, no polling).
        if ((before > 0) != (paired.isNotEmpty())) appContext?.let { LinkService.sync(it) }
    }

    fun wasAsked(permission: String): Boolean = p()?.getBoolean("asked:$permission", false) ?: false
    fun markAsked(permission: String) { p()?.edit()?.putBoolean("asked:$permission", true)?.apply() }

    fun wasDone(): Set<StepId> = StepId.entries.filter { p()?.getBoolean("done:$it", false) == true }.toSet()

    /** Remember which steps are Done now so a later regression is reported as a revocation. */
    fun rememberDone(steps: List<StepInfo>) {
        val e = p()?.edit() ?: return
        for (s in steps) if (s.status == StepStatus.Done) e.putBoolean("done:${s.id}", true)
        e.apply()
    }
}

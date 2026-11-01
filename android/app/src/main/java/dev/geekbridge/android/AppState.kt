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

    var oemAcknowledged: Boolean
        get() = p()?.getBoolean("oem_ack", false) ?: false
        set(v) { p()?.edit()?.putBoolean("oem_ack", v)?.apply() }

    val pairedCount: Int get() = p()?.getInt("paired_count", 0) ?: 0

    val pairedNames: List<String>
        get() = p()?.getString("paired_names", "")?.split('\n')?.filter { it.isNotEmpty() }.orEmpty()

    /** Called with every readable status snapshot; an unreadable one ([readable] false) changes nothing. */
    fun recordPaired(paired: List<PairedPeer>, readable: Boolean) {
        if (!readable) return
        val names = paired.take(MAX_NAMES).map { it.name.ifEmpty { "Computer" }.replace('\n', ' ').take(NAME_CHARS) }
        val before = pairedCount
        p()?.edit()?.putInt("paired_count", paired.size)?.putString("paired_names", names.joinToString("\n"))?.apply()
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

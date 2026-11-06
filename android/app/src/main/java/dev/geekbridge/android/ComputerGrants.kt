// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.Executors

/** The phone-side opt-ins that are granted per paired computer (the master switch stays global). */
enum class GrantFeature(val key: String) {
    Calls("calls"),
    Contacts("contacts"),
    Calendar("calendar"),
    CalendarEdit("calendar_edit"),
    Sms("sms"),
    /** Battery level and charging state (`kdeconnect.battery`). Off for every computer until allowed here. */
    Battery("battery"),
    /** Mirroring notifications of the chosen apps (`kdeconnect.notification*`). Off for every computer until allowed here. */
    Notifications("notifications"),
    /** Recent photos (`geekbridge.photos.*`): a short list and small previews on request. Off for every computer until allowed here. */
    Photos("photos"),
    /** EXPERIMENTAL call audio over Wi-Fi (debug builds only show it): listen to a call on this computer. */
    CallAudio("callaudio"),
}

/**
 * Which paired computer may use which opt-in: device id -> features. A newly paired computer has nothing until the
 * user allows it, so a computer paired later (or through a link someone sent) never inherits what another got.
 * The native core enforces it (`transport_permits`): requests from other computers are refused or dropped and
 * nothing is sent to them. Pure Kotlin, so it is tested without Android.
 */
data class ComputerGrants(val byDevice: Map<String, Set<GrantFeature>> = emptyMap(), val migrated: Boolean = false) {
    fun allows(device: String, f: GrantFeature) = f in byDevice[device].orEmpty()

    fun devicesFor(f: GrantFeature): Set<String> = byDevice.filterValues { f in it }.keys

    fun set(device: String, f: GrantFeature, on: Boolean): ComputerGrants {
        // Editing the calendar is never allowed to a computer that may not read it.
        val now = byDevice[device].orEmpty().let {
            if (on) it + f else if (f == GrantFeature.Calendar) it - f - GrantFeature.CalendarEdit else it - f
        }
        return copy(byDevice = if (now.isEmpty()) byDevice - device else byDevice + (device to now))
    }

    /** Everything for [f] withdrawn from all computers (the master switch went off). */
    fun clear(f: GrantFeature): ComputerGrants =
        byDevice.keys.fold(this) { g, d -> g.set(d, f, false) }

    /** Drops computers that are no longer paired, so a device id cannot come back with old rights. */
    fun keepOnly(paired: Set<String>) = copy(byDevice = byDevice.filterKeys { it in paired })

    /** The `permits` object of `transport_start` / `transport_permits`: every feature, empty = nobody. */
    fun permitsObject(): JSONObject = JSONObject().also { o ->
        for (f in GrantFeature.entries) {
            // Editing needs reading, whatever a stored or migrated list says (set() only enforces it when reading is withdrawn).
            val devices = if (f == GrantFeature.CalendarEdit) devicesFor(f) intersect devicesFor(GrantFeature.Calendar) else devicesFor(f)
            o.put(f.key, JSONArray(devices.sorted().take(MAX_PER_FEATURE)))
        }
    }

    fun permitsRequest(): String =
        JSONObject().put("version", CoreProtocol.VERSION).put("action", "transport_permits").put("permits", permitsObject()).toString()

    fun toJson(): String = JSONObject().put("migrated", migrated).put(
        "grants",
        JSONObject().also { o -> byDevice.forEach { (d, fs) -> o.put(d, JSONArray(fs.map { it.key })) } },
    ).toString()

    companion object {
        const val MAX_PER_FEATURE = 16

        fun fromJson(s: String?): ComputerGrants = try {
            val o = JSONObject(s ?: return ComputerGrants())
            val g = o.optJSONObject("grants") ?: JSONObject()
            val map = g.keys().asSequence().associateWith { d ->
                val a = g.optJSONArray(d) ?: JSONArray()
                (0 until a.length()).mapNotNull { i -> GrantFeature.entries.firstOrNull { it.key == a.optString(i) } }.toSet()
            }.filterValues { it.isNotEmpty() }
            ComputerGrants(map, o.optBoolean("migrated", false))
        } catch (_: Exception) {
            ComputerGrants() // unreadable = nothing allowed
        }

        /**
         * The old global switches become rights of the computers paired right now (they were already using them);
         * a computer paired afterwards gets nothing. Runs once, on the first readable list of paired computers.
         */
        fun migrate(old: ComputerGrants, legacy: Set<GrantFeature>, paired: Set<String>): ComputerGrants {
            if (old.migrated) return old
            var g = old.copy(migrated = true)
            for (d in paired) for (f in legacy) g = g.set(d, f, true)
            return g
        }
    }
}

/** Wording of the "who is allowed" line; pure so it is tested. */
object GrantText {
    /** "calls=1 sms=0 ..." (how many computers each feature is allowed for; no ids), for the debug log. */
    fun counts(g: ComputerGrants): String = GrantFeature.entries.joinToString(" ") { "${it.key}=${g.devicesFor(it).size}" }

    fun summary(allowedNames: List<String>, pairedCount: Int, s: Strings): String = when {
        pairedCount == 0 -> s.get(R.string.grant_none_paired)
        allowedNames.isEmpty() -> s.get(R.string.grant_none_allowed)
        else -> s.get(R.string.grant_allowed, allowedNames.joinToString(s.get(R.string.list_separator)))
    }
}

/**
 * What follows a change of the grants, in this order: the core gets the new permits, then a computer that was just
 * allowed the battery is sent the current state. The send goes through the core's permit check, so one made before the
 * core knew the new permit is refused as "not connected" and the computer, which already asked or has its consent on,
 * waits for a change that an idle phone never has.
 */
object GrantsPublish {
    fun run(g: ComputerGrants, batteryChanged: Boolean, applyPermits: (String) -> Unit, pushBattery: () -> Unit) {
        applyPermits(g.permitsRequest())
        if (batteryChanged) pushBattery()
    }
}

/** The grants the native start/permits requests are built from. Closed (nobody) until the runtime loads them. */
object GrantsHolder {
    @Volatile var current: ComputerGrants = ComputerGrants()
}

@SuppressLint("StaticFieldLeak") // only the application context is held
object ComputerGrantsRuntime {
    private const val FILE = "geekbridge_grants"

    @Volatile private var prefs: SharedPreferences? = null
    @Volatile private var blob: BackedBlob? = null
    private val core: CoreClient by lazy { CoreClient.native() }
    private val worker = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-grants").apply { isDaemon = true } }

    val grants: ComputerGrants get() = GrantsHolder.current

    @Synchronized
    fun init(ctx: Context) {
        if (prefs != null) return
        val p = ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        prefs = p
        blob = BackedBlob(StateMigrations.kv(p), "grants", BackedBlob::jsonObject)
        GrantsHolder.current = ComputerGrants.fromJson(blob?.read()?.ifEmpty { null })
        GbLog.i("grants", "loaded: ${GrantText.counts(GrantsHolder.current)}")
    }

    @Synchronized
    fun update(change: (ComputerGrants) -> ComputerGrants) {
        val g = change(GrantsHolder.current)
        if (g == GrantsHolder.current) return
        val before = GrantsHolder.current.devicesFor(GrantFeature.Battery)
        GrantsHolder.current = g
        blob?.write(g.toJson())
        val batteryChanged = g.devicesFor(GrantFeature.Battery) != before
        worker.execute {
            GrantsPublish.run(g, batteryChanged, { core.transportAction(it) }, { PhoneDataRuntime.onBatteryGrantsChanged() })
            GbLog.i("grants", "sent to the core: ${GrantText.counts(g)}")
        }
    }

    fun allow(device: String, f: GrantFeature, on: Boolean) = update { it.set(device, f, on) }

    /** Called with every readable list of paired computers: migrates once and forgets unpaired computers. */
    fun onPaired(paired: List<PairedPeer>) {
        val ids = paired.map { it.deviceId }.toSet()
        update { g ->
            val legacy = legacyFeatures()
            ComputerGrants.migrate(g, legacy, ids).keepOnly(ids)
        }
    }

    /** The features the user had switched on before grants existed (the global switches). */
    private fun legacyFeatures(): Set<GrantFeature> {
        val s = PhoneDataRuntime.settings
        return buildSet {
            if (s.contacts) add(GrantFeature.Contacts)
            if (s.calendar) add(GrantFeature.Calendar)
            if (s.calendarEdit) add(GrantFeature.CalendarEdit)
            if (s.sms) add(GrantFeature.Sms)
            if (CallControlRuntime.settings.enabled) add(GrantFeature.Calls)
        }
    }
}

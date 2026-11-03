// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.app.ActivityManager
import android.app.usage.UsageStatsManager
import android.content.Context
import android.content.pm.PackageManager
import android.content.pm.PermissionInfo
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import android.os.PowerManager
import java.text.SimpleDateFormat
import java.util.ArrayDeque
import java.util.Date
import java.util.Locale

/*
 * DEBUG BUILDS ONLY (src/debug). The redacted state dump behind `geekbridge-ctl phone-state` and the Diagnostics
 * "Debug state" button. It is built from structured facts only (versions, permission and switch states, counts,
 * reasons), never from message text, numbers, names, clipboard or addresses other than a paired computer's IP, and
 * the finished text passes [DebugRedactor] again.
 */

/** One Android permission as the dump shows it. */
data class PermState(val name: String, val granted: Boolean, val restricted: Boolean = false)

/** A paired computer. [alias] is "computer N (id prefix)": its name is never put in a dump. */
data class ComputerState(
    val alias: String,
    val connected: Boolean,
    val ip: String?,
    val grants: Set<GrantFeature>,
)

/** Everything the "why is this capability (not) offered" table depends on. */
data class CapabilityInputs(
    val messagesSwitch: Boolean,
    val contactsSwitch: Boolean,
    val calendarSwitch: Boolean,
    val calendarEditSwitch: Boolean,
    val callsSwitch: Boolean,
    val clipboardSwitch: Boolean,
    val debugStreaming: Boolean,
    val permissions: List<PermState>,
    val computers: List<ComputerState>,
) {
    fun perm(name: String): PermState? = permissions.firstOrNull { it.name == name }
}

/** One line of the capability table. [feature] is the wire/consent key (`sms`, `contacts`, ...). */
data class CapabilityLine(val feature: String, val offered: Boolean, val reason: String?) {
    fun text() = if (offered) "$feature: offered" else "$feature: not offered: $reason"
}

object CapabilityReasons {
    private const val RESTRICTED_HINT =
        "restricted setting: Settings, Apps, GeekBridge, three-dot menu, Allow restricted settings, then grant it"

    private fun missing(i: CapabilityInputs, names: List<String>): String? {
        val bad = names.mapNotNull { i.perm(it) }.filter { !it.granted }
        if (bad.isEmpty()) return null
        return bad.joinToString("; ") { p ->
            if (p.restricted) "${p.name} not granted ($RESTRICTED_HINT)" else "${p.name} not granted"
        }
    }

    private data class Spec(
        val key: String,
        val label: String,
        val switch: (CapabilityInputs) -> Boolean,
        val permissions: List<String>,
        val grant: GrantFeature?,
    )

    private val specs = listOf(
        Spec("sms", "Messages", { it.messagesSwitch }, listOf("READ_SMS", "SEND_SMS", "RECEIVE_SMS"), GrantFeature.Sms),
        Spec("contacts", "Contacts", { it.contactsSwitch }, listOf("READ_CONTACTS"), GrantFeature.Contacts),
        Spec("calendar", "Calendar", { it.calendarSwitch }, listOf("READ_CALENDAR"), GrantFeature.Calendar),
        Spec(
            "calendar_edit", "Calendar editing", { it.calendarEditSwitch && it.calendarSwitch },
            listOf("READ_CALENDAR", "WRITE_CALENDAR"), GrantFeature.CalendarEdit,
        ),
        Spec("calls", "Calls", { it.callsSwitch }, listOf("READ_PHONE_STATE"), GrantFeature.Calls),
        Spec("clipboard", "Clipboard", { it.clipboardSwitch }, emptyList(), null),
        Spec("share", "Share", { true }, emptyList(), null),
        Spec("debug", "Send debug log to computer", { it.debugStreaming }, emptyList(), null),
    )

    /** Does this phone declare [key] in its identity right now (switch on and Android permissions granted)? */
    fun declared(i: CapabilityInputs, key: String): Boolean {
        val s = specs.first { it.key == key }
        return reasonPhone(i, s) == null
    }

    private fun reasonPhone(i: CapabilityInputs, s: Spec): String? {
        if (s.key == "calendar_edit" && !i.calendarSwitch) {
            return "Calendar is not shared (editing needs the Calendar switch ON first)"
        }
        if (!s.switch(i)) {
            return when (s.key) {
                "debug" -> "'Send debug log to computer' is OFF in Diagnostics"
                "clipboard" -> "Clipboard sync is switched OFF in the app"
                else -> "${s.label} is switched OFF in the app (master switch)"
            }
        }
        return missing(i, s.permissions)
    }

    /** The phone-wide table: a capability is "offered" when declared and at least one paired computer may use it. */
    fun table(i: CapabilityInputs): List<CapabilityLine> = specs.map { s ->
        val phone = reasonPhone(i, s)
        when {
            phone != null -> CapabilityLine(s.key, false, phone)
            s.grant == null -> CapabilityLine(s.key, true, null)
            i.computers.isEmpty() -> CapabilityLine(s.key, false, "no computer is paired")
            i.computers.none { s.grant in it.grants } ->
                CapabilityLine(s.key, false, "per-computer ${s.label} switch is OFF (no paired computer is allowed)")
            else -> CapabilityLine(s.key, true, null)
        }
    }

    /** What one computer may use. */
    fun forComputer(i: CapabilityInputs, c: ComputerState): List<CapabilityLine> = specs.filter { it.grant != null }.map { s ->
        val phone = reasonPhone(i, s)
        when {
            phone != null -> CapabilityLine(s.key, false, phone)
            s.grant!! !in c.grants -> CapabilityLine(s.key, false, "per-computer ${s.label} switch is OFF")
            s.key == "calendar_edit" && GrantFeature.Calendar !in c.grants ->
                CapabilityLine(s.key, false, "per-computer Calendar switch is OFF (editing needs reading)")
            else -> CapabilityLine(s.key, true, null)
        }
    }
}

/** Last [capacity] timestamped entries, newest last. */
class EventLog(private val capacity: Int, private val clock: () -> Long = System::currentTimeMillis) {
    private val items = ArrayDeque<String>()
    private val fmt = SimpleDateFormat("MM-dd HH:mm:ss.SSS", Locale.US)

    @Synchronized fun add(text: String) {
        items.addLast("${fmt.format(Date(clock()))}  ${DebugRedactor.line(text)}")
        while (items.size > capacity) items.removeFirst()
    }

    @Synchronized fun all(): List<String> = items.toList()
}

/** Plain-data model of the dump, so rendering is a pure function. */
data class StateModel(
    val appVersion: String,
    val appBuild: Int,
    val commit: String,
    val buildType: String,
    val android: String,
    val sdk: Int,
    val device: String,
    val oem: String,
    val inputs: CapabilityInputs,
    val declared: List<String>,
    val battery: String,
    val standbyBucket: String,
    val dozing: Boolean,
    val powerSave: Boolean,
    val service: String,
    val network: String,
    val linkPhase: String,
    val linkHistory: List<String>,
    val exceptions: List<String>,
    val logLines: Int,
    val logBytes: Int,
    val streamingTo: Int,
    val linkHealth: List<String> = emptyList(),
)

/** The last time a link closed: why (the core's reason code), on which network, and how long the computer had been silent. */
data class DropNote(val atMs: Long, val code: String, val network: String, val quietMs: Long?)

/** The "link health" lines of the dump. Pure. Only structured facts, never an address. */
object LinkHealthText {
    fun render(last: DropNote?, drops: Int, nowMs: Long): List<String> = buildList {
        if (last == null) {
            add("no link has closed since the app started")
        } else {
            val ago = ((nowMs - last.atMs).coerceAtLeast(0)) / 1000
            add("last close: ${span(ago)} ago, reason ${last.code}, network then: ${last.network}")
            add("nothing heard from the computer for ${last.quietMs?.let { span(it / 1000) } ?: "an unknown time"} before it closed")
            add("closes since the app started: $drops")
        }
    }

    private fun span(s: Long) = if (s < 120) "${s} s" else "${s / 60} min"
}

object DebugStateText {
    fun render(m: StateModel): String = buildString {
        appendLine("GeekBridge phone state (debug build; redacted: no names, numbers, message or clipboard text)")
        appendLine("app: ${m.appVersion} (code ${m.appBuild}, ${m.buildType}) commit ${m.commit}")
        appendLine("android: ${m.android} (API ${m.sdk}); device: ${m.device}; vendor skin: ${m.oem}")
        appendLine()
        appendLine("== Permissions ==")
        for (p in m.inputs.permissions) {
            appendLine("${p.name}: " + if (p.granted) "granted" else if (p.restricted) "DENIED (restricted setting)" else "DENIED")
        }
        appendLine("battery optimisation: ${m.battery}")
        appendLine()
        appendLine("== Switches in the app ==")
        val i = m.inputs
        appendLine("Messages=${on(i.messagesSwitch)} Contacts=${on(i.contactsSwitch)} Calendar=${on(i.calendarSwitch)} " +
            "Calendar-editing=${on(i.calendarEditSwitch)} Calls=${on(i.callsSwitch)} Clipboard=${on(i.clipboardSwitch)} " +
            "Send-debug-log=${on(i.debugStreaming)}")
        appendLine()
        appendLine("== Paired computers (per-computer switches) ==")
        if (i.computers.isEmpty()) appendLine("none paired")
        for (c in i.computers) {
            val sw = GrantFeature.entries.joinToString(" ") { f -> "${f.key}=${on(f in c.grants)}" }
            appendLine("${c.alias}: ${if (c.connected) "connected" else "not connected"}${c.ip?.let { " ($it)" }.orEmpty()}; $sw")
            for (line in CapabilityReasons.forComputer(i, c)) appendLine("  ${line.text()}")
        }
        appendLine()
        appendLine("== Capabilities ==")
        appendLine("declared in this phone's identity: ${m.declared.ifEmpty { listOf("(none)") }.joinToString(", ")}")
        for (line in CapabilityReasons.table(i)) appendLine(line.text())
        appendLine()
        appendLine("== Service and network ==")
        appendLine("foreground service: ${m.service}")
        appendLine("link: ${m.linkPhase}")
        appendLine("network: ${m.network}")
        appendLine("doze: ${if (m.dozing) "IDLE (dozing)" else "not idle"}; power saver: ${on(m.powerSave)}; standby bucket: ${m.standbyBucket}")
        appendLine()
        appendLine("== Link health ==")
        m.linkHealth.ifEmpty { listOf("not tracked") }.forEach(::appendLine)
        appendLine()
        appendLine("== Debug log ==")
        appendLine("buffered: ${m.logLines} lines / ${m.logBytes} bytes; computers receiving it: ${m.streamingTo}")
        appendLine()
        appendLine("== Link transitions (last ${m.linkHistory.size} of 50) ==")
        if (m.linkHistory.isEmpty()) appendLine("none yet")
        m.linkHistory.forEach(::appendLine)
        appendLine()
        appendLine("== Exceptions caught (last ${m.exceptions.size} of 20) ==")
        if (m.exceptions.isEmpty()) appendLine("none")
        m.exceptions.forEach(::appendLine)
    }.let { DebugRedactor.text(it, m.inputs.computers.mapNotNull { c -> c.ip }.toSet()) }

    private fun on(b: Boolean) = if (b) "ON" else "OFF"
}

/** Collects the facts from the running app. */
object DebugStateCollector {
    private val PERMISSIONS = listOf(
        "READ_SMS" to Manifest.permission.READ_SMS,
        "SEND_SMS" to Manifest.permission.SEND_SMS,
        "RECEIVE_SMS" to Manifest.permission.RECEIVE_SMS,
        "READ_CONTACTS" to Manifest.permission.READ_CONTACTS,
        "READ_CALENDAR" to Manifest.permission.READ_CALENDAR,
        "WRITE_CALENDAR" to Manifest.permission.WRITE_CALENDAR,
        "READ_PHONE_STATE" to Manifest.permission.READ_PHONE_STATE,
        "ANSWER_PHONE_CALLS" to Manifest.permission.ANSWER_PHONE_CALLS,
        "POST_NOTIFICATIONS" to Manifest.permission.POST_NOTIFICATIONS,
    )

    fun permissions(ctx: Context): List<PermState> = PERMISSIONS.map { (name, perm) ->
        val granted = ctx.checkSelfPermission(perm) == PackageManager.PERMISSION_GRANTED
        PermState(name, granted, !granted && hardRestricted(ctx, perm))
    }

    /** A hard-restricted permission (SMS, call log) on Android 13+ needs "Allow restricted settings" when sideloaded. */
    private fun hardRestricted(ctx: Context, perm: String): Boolean {
        if (Build.VERSION.SDK_INT < 33) return false
        return try {
            (ctx.packageManager.getPermissionInfo(perm, 0).flags and PermissionInfo.FLAG_HARD_RESTRICTED) != 0
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }
    }

    fun inputs(ctx: Context, debugStreaming: Boolean): CapabilityInputs {
        val s = PhoneDataRuntime.settings
        val grants = ComputerGrantsRuntime.grants
        val snap = LinkRuntime.controller().snapshot
        val connected = snap?.connections.orEmpty().filter { it.trusted }
        val computers = AppState.pairedPeers.mapIndexed { n, (id, _) ->
            val link = connected.firstOrNull { it.peer.deviceId == id }
            ComputerState(
                alias = "computer ${n + 1} (${id.take(6)})",
                connected = link != null,
                ip = link?.peer?.ip,
                grants = GrantFeature.entries.filter { grants.allows(id, it) }.toSet(),
            )
        }
        return CapabilityInputs(
            messagesSwitch = s.sms, contactsSwitch = s.contacts, calendarSwitch = s.calendar,
            calendarEditSwitch = s.calendarEdit, callsSwitch = CallControlRuntime.settings.enabled,
            clipboardSwitch = ClipboardRuntime.settings.enabled, debugStreaming = debugStreaming,
            permissions = permissions(ctx), computers = computers,
        )
    }

    fun network(ctx: Context): String {
        val cm = ctx.getSystemService(ConnectivityManager::class.java) ?: return "unknown"
        val net = cm.activeNetwork ?: return "no active network"
        val caps = cm.getNetworkCapabilities(net) ?: return "active network, capabilities unknown"
        val transports = buildList {
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) add("wifi")
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) add("cellular")
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) add("ethernet")
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) add("vpn")
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_BLUETOOTH)) add("bluetooth")
        }.ifEmpty { listOf("other") }
        val validated = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        val metered = !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        return "${transports.joinToString("+")}, ${if (validated) "internet validated" else "no validated internet"}, ${if (metered) "metered" else "unmetered"}"
    }

    fun standbyBucket(ctx: Context): String {
        if (Build.VERSION.SDK_INT < 28) return "n/a (API < 28)"
        val usm = ctx.getSystemService(UsageStatsManager::class.java) ?: return "unknown"
        return when (val b = usm.appStandbyBucket) {
            5 -> "EXEMPTED"
            10 -> "ACTIVE"
            20 -> "WORKING_SET"
            30 -> "FREQUENT"
            40 -> "RARE"
            45 -> "RESTRICTED"
            50 -> "NEVER"
            else -> "bucket $b"
        }
    }

    fun service(ctx: Context): String {
        val running = LinkService.isRunning
        val err = AppState.serviceError?.let { "; last start failed: " + DiagnosticsReport.scrub(it) }.orEmpty()
        val am = ctx.getSystemService(ActivityManager::class.java)
        val restricted = Build.VERSION.SDK_INT >= 28 && am?.isBackgroundRestricted == true
        return (if (running) "running" else "not running") +
            "; Stay connected=${AppState.keepConnected}; wanted now=${AppState.wantService()}; " +
            "background restricted=$restricted$err"
    }

    fun model(
        ctx: Context,
        debugStreaming: Boolean,
        ring: LogRing,
        streamingTo: Int,
        linkHistory: List<String>,
        exceptions: List<String>,
        linkHealth: List<String> = emptyList(),
    ): StateModel {
        val inputs = inputs(ctx, debugStreaming)
        val pm = ctx.getSystemService(PowerManager::class.java)
        val c = LinkRuntime.controller()
        return StateModel(
            appVersion = BuildConfig.VERSION_NAME, appBuild = BuildConfig.VERSION_CODE,
            commit = BuildConfig.GIT_COMMIT, buildType = BuildConfig.BUILD_TYPE,
            android = Build.VERSION.RELEASE, sdk = Build.VERSION.SDK_INT,
            device = "${Build.MANUFACTURER} ${Build.MODEL}",
            oem = OemDetector.detect(Build.MANUFACTURER, Build.BRAND).label,
            inputs = inputs,
            declared = specKeys().filter { CapabilityReasons.declared(inputs, it) },
            battery = PowerChecks.batteryState(ctx).name,
            standbyBucket = standbyBucket(ctx),
            dozing = pm?.isDeviceIdleMode == true,
            powerSave = pm?.isPowerSaveMode == true,
            service = service(ctx),
            network = network(ctx),
            linkPhase = c.phase.name + (c.snapshot?.let { s -> ", ${s.connections.count { it.trusted }} trusted connection(s)" } ?: ""),
            linkHistory = linkHistory, exceptions = exceptions,
            logLines = ring.size(), logBytes = ring.byteSize(), streamingTo = streamingTo, linkHealth = linkHealth,
        )
    }

    private fun specKeys() = listOf("sms", "contacts", "calendar", "calendar_edit", "calls", "clipboard", "share", "debug")
}

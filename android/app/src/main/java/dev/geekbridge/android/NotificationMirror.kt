// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/**
 * What the mirror needs to know about one notification, copied out of the Android object so the rules can be tested
 * without Android. Never logged: [title] and [text] are private.
 */
data class MirrorInfo(
    val key: String,
    val pkg: String,
    val appName: String,
    val title: String,
    val text: String,
    val whenMs: Long,
    val ongoing: Boolean = false,
    val foregroundService: Boolean = false,
    val groupSummary: Boolean = false,
    val category: String? = null,
    val secret: Boolean = false,
    val clearable: Boolean = true,
    /** Importance below "default" (silent, minimised): listed on the computer but no banner. */
    val quiet: Boolean = false,
    /** Titles of the buttons that need no typed input (a reply button is never offered). */
    val actions: List<String> = emptyList(),
)

object MirrorRules {
    const val KIND = "kdeconnect.notification"
    const val REQUEST_KIND = "kdeconnect.notification.request"
    const val ACTION_KIND = "kdeconnect.notification.action"

    const val MAX_APP = 64
    const val MAX_TITLE = 160
    const val MAX_TEXT = 600
    const val MAX_ACTION = 48
    const val MAX_ACTIONS = 3
    /** Most notifications sent in answer to one "send what is showing" request. */
    const val MAX_BULK = 15
    /** Opaque ids remembered (oldest forgotten first). */
    const val MAX_IDS = 128
    const val MAX_APPS = 200

    /** Categories that are not conversations or alerts for a person: calls have their own feature. */
    private val skippedCategories = setOf("call", "progress", "service", "sys", "transport", "navigation", "status", "stopwatch", "alarm")

    /** Why a notification is not mirrored; null = it may be, when its app is allowed. */
    fun skipReason(n: MirrorInfo, ownPackage: String): String? = when {
        n.pkg == ownPackage -> "own"
        n.ongoing -> "ongoing"
        n.foregroundService -> "foreground_service"
        n.groupSummary -> "group_summary"
        n.secret -> "secret"
        n.category in skippedCategories -> "category"
        n.title.isBlank() && n.text.isBlank() -> "empty"
        else -> null
    }

    /** Mirrored only from an app the user picked: the default list is empty, so nothing leaves until a choice is made. */
    fun mirrors(n: MirrorInfo, allowedApps: Set<String>, ownPackage: String): Boolean =
        n.pkg in allowedApps && skipReason(n, ownPackage) == null

    fun clip(s: String, max: Int): String {
        val cleaned = buildString { for (c in s) append(if (Character.isISOControl(c)) ' ' else c) }.trim()
        if (cleaned.length <= max) return cleaned
        // Never cut between the two halves of an emoji: a lone surrogate is not valid text on the wire.
        val end = if (max > 0 && Character.isHighSurrogate(cleaned[max - 1])) max - 1 else max
        return cleaned.substring(0, end).trimEnd()
    }

    /** Body of a stock `kdeconnect.notification` post. No icon is sent: the computer never asks for one. */
    fun postBody(n: MirrorInfo, id: String, silent: Boolean): JSONObject {
        val title = clip(n.title, MAX_TITLE)
        val text = clip(n.text, MAX_TEXT)
        return JSONObject()
            .put("id", id)
            .put("appName", clip(n.appName, MAX_APP))
            .put("title", title)
            .put("text", text)
            .put("ticker", if (text.isEmpty()) title else "$title: $text")
            .put("time", n.whenMs.coerceAtLeast(0).toString())
            .put("isClearable", n.clearable)
            .put("silent", silent || n.quiet)
            .put("onlyOnce", false)
            .put("actions", JSONArray(n.actions.map { clip(it, MAX_ACTION) }.filter { it.isNotEmpty() }.take(MAX_ACTIONS)))
    }

    fun cancelBody(id: String): JSONObject = JSONObject().put("id", id).put("isCancel", true)

    /** What a computer asked for in a `.request` body. */
    sealed class Ask {
        object All : Ask()
        data class Dismiss(val id: String) : Ask()
    }

    private fun validId(s: String) = s.isNotEmpty() && s.length <= 128 && s.none { Character.isISOControl(it) }

    fun parseRequest(b: JSONObject): Ask? {
        if (b.has("cancel")) {
            val id = b.opt("cancel") as? String ?: return null
            return if (validId(id)) Ask.Dismiss(id) else null
        }
        return if (b.optBoolean("request", false)) Ask.All else null
    }

    /** (id, button title) of a `.action` body. */
    fun parseAction(b: JSONObject): Pair<String, String>? {
        val id = b.opt("key") as? String ?: return null
        val title = b.opt("action") as? String ?: return null
        return if (validId(id) && title.isNotEmpty() && title.length <= 200) id to title else null
    }

    fun signature(n: MirrorInfo): String = "${n.title}\u0000${n.text}\u0000${n.actions.joinToString("\u0001")}\u0000${n.clearable}"
}

/** Opaque ids for the computer: the Android key (package, id, tag) is never sent. Bounded, oldest forgotten first. */
class MirrorIds(private val max: Int = MirrorRules.MAX_IDS) {
    private val byKey = LinkedHashMap<String, String>()
    private val byId = HashMap<String, String>()
    private var counter = 0L

    @Synchronized
    fun idFor(key: String): String {
        byKey[key]?.let { return it }
        counter++
        val id = "gb$counter"
        byKey[key] = id
        byId[id] = key
        while (byKey.size > max) {
            val eldest = byKey.entries.first()
            byKey.remove(eldest.key)
            byId.remove(eldest.value)
        }
        return id
    }

    @Synchronized fun keyOf(id: String): String? = byId[id]

    @Synchronized fun idOfKey(key: String): String? = byKey[key]

    @Synchronized
    fun forget(key: String) {
        byKey.remove(key)?.let { byId.remove(it) }
    }

    @Synchronized
    fun clear() {
        byKey.clear()
        byId.clear()
    }
}

/** The Android side of the mirror: what is showing and how to act on it. Faked in tests. */
interface MirrorPort {
    fun active(): List<MirrorInfo>
    fun cancel(key: String): Boolean
    /** Press the button titled [title] of [key]; false if it has no such reply-less button. */
    fun press(key: String, title: String): Boolean
}

/**
 * Sends mirrored notifications to the computers allowed to have them (the native core enforces which and sends
 * nothing when none is connected) and carries out a computer's dismiss and button requests. Event driven only: the
 * listener's callbacks and the computer's requests. Pure Kotlin so it is tested without Android.
 *
 * A computer can act only on what was mirrored: ids it was given, from allowed apps, looked up here and re-checked
 * against what is showing. Titles and texts pass through here and are never logged.
 */
class NotificationMirror(
    private val port: MirrorPort,
    private val core: (String) -> CoreOutcome,
    private val enabled: () -> Boolean,
    private val allowedApps: () -> Set<String>,
    private val ownPackage: String,
    private val ids: MirrorIds = MirrorIds(),
) {
    /** Insertion ordered: the oldest entry is the one forgotten when the memory is full. */
    private val lastSent = LinkedHashMap<String, String>()

    @Synchronized
    fun reset() {
        lastSent.clear()
        ids.clear()
    }

    private fun eligible(n: MirrorInfo) = enabled() && MirrorRules.mirrors(n, allowedApps(), ownPackage)

    /** A notification was posted or updated. */
    @Synchronized
    fun onPosted(n: MirrorInfo) {
        if (!eligible(n)) return
        val sig = MirrorRules.signature(n)
        if (lastSent[n.key] == sig) return // the app re-posted the same thing (progress ticks, group churn)
        if (push(n, silent = false, connectionId = null)) lastSent[n.key] = sig
        while (lastSent.size > MirrorRules.MAX_IDS) lastSent.remove(lastSent.keys.first())
    }

    /** A notification went away on the phone (swiped, cancelled by its app, or by the computer's own dismiss). */
    @Synchronized
    fun onRemoved(key: String) {
        lastSent.remove(key)
        val id = ids.idOfKey(key) ?: return
        ids.forget(key)
        if (enabled()) core(PhoneDataProtocol.sendRequest(MirrorRules.KIND, MirrorRules.cancelBody(id)))
    }

    /** "Send what is showing": the newest few eligible ones, to that computer only, marked silent (no banners). */
    @Synchronized
    fun sendAll(connectionId: Long?) {
        if (!enabled()) return
        var sent = 0
        for (n in port.active().filter(::eligible).sortedByDescending { it.whenMs }) {
            if (sent >= MirrorRules.MAX_BULK) break
            if (!push(n, silent = true, connectionId = connectionId)) break
            sent++
        }
    }

    /** The computer dismissed one of the notifications it was given. */
    @Synchronized
    fun onDismiss(id: String) {
        if (!enabled()) return
        val key = ids.keyOf(id) ?: return
        val n = port.active().firstOrNull { it.key == key } ?: return
        if (!eligible(n) || !n.clearable) return
        port.cancel(key)
    }

    /** The computer pressed a button of one of the notifications it was given. */
    @Synchronized
    fun onAction(id: String, title: String) {
        if (!enabled()) return
        val key = ids.keyOf(id) ?: return
        val n = port.active().firstOrNull { it.key == key } ?: return
        // The computer only ever saw the clipped titles (and the first three), so that is what it can name.
        val offered = n.actions.map { MirrorRules.clip(it, MirrorRules.MAX_ACTION) }.filter { it.isNotEmpty() }.take(MirrorRules.MAX_ACTIONS)
        if (!eligible(n) || title !in offered) return
        port.press(key, title)
    }

    private fun push(n: MirrorInfo, silent: Boolean, connectionId: Long?): Boolean {
        val id = ids.idFor(n.key)
        val o = core(PhoneDataProtocol.sendRequest(MirrorRules.KIND, MirrorRules.postBody(n, id, silent), connectionId))
        // not_connected / data_disabled / rate_limited: nothing to tell anyone now; the computer asks again when it connects.
        return o is CoreOutcome.Ok
    }
}

/** What stands between the user and "the computer shows my phone notifications", in the order to fix it. */
enum class MirrorStep { Ready, TurnOn, GrantAccess, GrantAccessRestricted, ChooseApps, PairComputer, ChooseComputer }

object MirrorReadiness {
    /**
     * [restrictedLikely]: Android 13+ and not installed by a store, so the Notification access switch stays greyed
     * out until "Allow restricted settings" is chosen in the app's own info page (Samsung too).
     */
    fun evaluate(
        switchOn: Boolean,
        accessGranted: Boolean,
        restrictedLikely: Boolean,
        appCount: Int,
        pairedCount: Int,
        allowedCount: Int,
    ): MirrorStep = when {
        !switchOn -> MirrorStep.TurnOn
        !accessGranted -> if (restrictedLikely) MirrorStep.GrantAccessRestricted else MirrorStep.GrantAccess
        appCount == 0 -> MirrorStep.ChooseApps
        pairedCount == 0 -> MirrorStep.PairComputer
        allowedCount == 0 -> MirrorStep.ChooseComputer
        else -> MirrorStep.Ready
    }

    fun message(step: MirrorStep, s: Strings): String = s.get(
        when (step) {
            MirrorStep.Ready -> R.string.mirror_state_ready
            MirrorStep.TurnOn -> R.string.mirror_state_turn_on
            MirrorStep.GrantAccess -> R.string.mirror_state_access
            MirrorStep.GrantAccessRestricted -> R.string.mirror_state_restricted
            MirrorStep.ChooseApps -> R.string.mirror_state_apps
            MirrorStep.PairComputer -> R.string.mirror_state_pair
            MirrorStep.ChooseComputer -> R.string.mirror_state_choose
        },
    )
}

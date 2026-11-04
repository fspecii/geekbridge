// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject

/**
 * Kotlin side of `geekbridge.calls.*` (docs/android-calls.md): reports the phone's coarse call state to a
 * paired computer that the user allowed, and carries out that computer's request to answer, decline or end
 * a call. Pure: no Android types, so it runs in JVM tests. The Android binding is in `CallControlAndroid.kt`.
 *
 * What it can and cannot do, stated honestly:
 * - It only ever calls `TelecomManager.acceptRingingCall()` (API 26+) and `endCall()` (API 28+), both guarded
 *   by the `ANSWER_PHONE_CALLS` runtime permission. It never touches call audio: that stays on the phone's
 *   own Bluetooth hands-free connection.
 * - The caller's number needs `READ_CALL_LOG` (Android hands the number only to apps that hold it) and the
 *   name additionally `READ_CONTACTS`. Both are optional; without them the computer shows "Unknown caller" (it cannot tell a hidden number from a missing permission).
 * - Nothing is sent unless the user switched this on, and each report carries only what the permissions allow.
 */

/** Opt-in: off until the user turns it on in setup. Persisted by [CallControlRuntime]. */
data class CallLinkSettings(
    val enabled: Boolean = false,
    /**
     * Ask on the phone before it places a call a computer requested. Off by default: every dial is already
     * confirmed on the computer, limited to a few per 10 minutes per computer, and announced on the phone;
     * a second prompt for each call would defeat dialing from the PC. Turn it on to be the final gate.
     */
    val confirmDial: Boolean = false,
)

data class CallPermissions(
    val readPhoneState: Boolean,
    val answerPhoneCalls: Boolean,
    val readCallLog: Boolean,
    val readContacts: Boolean,
    /** CALL_PHONE: lets a paired computer ask this phone to place a call (TelecomManager.placeCall). */
    val callPhone: Boolean = false,
)

enum class CallAction(val wire: String) {
    Answer("answer"),
    Reject("reject"),
    Hangup("hangup"),
    Dial("dial");

    companion object {
        fun parse(s: String): CallAction? = entries.firstOrNull { it.wire == s }
    }
}

/** A request from a paired computer, as the native core queued it. */
data class CallRequest(
    val seq: Long,
    val connectionId: Long,
    val requestId: String,
    val action: CallAction,
    /** Only for [CallAction.Dial]: plain digits with an optional leading +. */
    val number: String? = null,
    /** Only for [CallAction.Dial]: the name the computer gave itself (untrusted text, cleaned before display). */
    val computer: String? = null,
) {
    override fun toString() = "CallRequest($seq, ${action.wire})" // never the number
}

data class CallWait(val requests: List<CallRequest>, val lastSeq: Long, val stopped: Boolean, val enabled: Boolean)

/** What one report says. Number and name are present only if the permissions allowed reading them. */
data class CallReport(
    val state: CallState,
    val number: String?,
    val name: String?,
    val canAnswer: Boolean,
    val canEnd: Boolean,
    val canDial: Boolean = false,
) {
    override fun toString() = "CallReport($state, answer=$canAnswer, end=$canEnd, dial=$canDial)" // never the party
}

/** Why the phone did not do a request. Fixed vocabulary shared with the computer (`LinkFailure`). */
enum class CallFailure(val wire: String) {
    PermissionMissing("permission_missing"),
    NoSuchCall("no_such_call"),
    Failed("failed"),
    RateLimited("rate_limited"),
    Declined("declined"),
}

object CallRules {
    const val MIN_SDK_ANSWER = 26
    const val MIN_SDK_END = 28
    const val MAX_FIELD_BYTES = 128

    /** Answering needs the permission, the call state, and API 26. */
    fun canAnswer(sdk: Int, p: CallPermissions) = sdk >= MIN_SDK_ANSWER && p.answerPhoneCalls && p.readPhoneState

    /** Declining and ending use `TelecomManager.endCall()`, which exists from API 28. */
    fun canEnd(sdk: Int, p: CallPermissions) = sdk >= MIN_SDK_END && p.answerPhoneCalls && p.readPhoneState

    /**
     * Placing a call needs CALL_PHONE (any API level that has TelecomManager.placeCall, i.e. 21+) and the call state,
     * because a dial is only placed from a phone known to be idle; without READ_PHONE_STATE every dial would fail.
     */
    fun canDial(p: CallPermissions) = p.callPhone && p.readPhoneState

    /** The only numbers this phone dials for a computer: digits, optional leading +. No USSD/MMI (`*`, `#`). */
    private val dialNumber = Regex("\\+?[0-9]{1,20}")

    fun dialable(raw: String?): Boolean = raw != null && dialNumber.matches(raw)

    /** A computer's self-chosen name, made safe to show: no control or bidi/invisible characters, short. */
    fun cleanComputer(raw: String?, strings: Strings): String {
        val s = buildString {
            for (c in raw.orEmpty()) {
                val t = Character.getType(c)
                val bad = Character.isISOControl(c) || t == Character.FORMAT.toInt() || t == Character.LINE_SEPARATOR.toInt() ||
                    t == Character.PARAGRAPH_SEPARATOR.toInt()
                append(if (bad) ' ' else c)
            }
        }.trim().replace(Regex("\\s+"), " ")
        return if (s.isEmpty()) strings.get(R.string.call_computer_fallback) else MirrorRules.clip(s, 40)
    }

    /**
     * What the owner reads when a computer asks the phone to dial. The number and the computer's self-chosen name are
     * isolated (see [Bidi]) so a right-to-left name cannot reorder the sentence or the digits around it.
     */
    fun dialNotice(number: String, computer: String?, strings: Strings): String =
        strings.get(R.string.call_dial_notice, Bidi.ltr(number), Bidi.isolate(cleanComputer(computer, strings)))

    /** The same checks [handle] makes before a dial, without placing it; null means it could be placed now. */
    fun dialPreflight(state: CallState?, p: CallPermissions, number: String?): CallFailure? = when {
        !canDial(p) -> CallFailure.PermissionMissing
        state != CallState.Idle -> CallFailure.NoSuchCall
        !dialable(number) -> CallFailure.Failed
        else -> null
    }

    private val phoneNumber = Regex("[+0-9*#().\\- ]{1,32}")

    /** A broadcast extra is only a phone number if it looks like one; anything else (text, markup) is ignored. */
    fun plausibleNumber(raw: String?): Boolean = raw != null && phoneNumber.matches(raw)

    /**
     * Below Android 13 a receiver cannot be marked not-exported, so any app could send a fake PHONE_STATE_CHANGED.
     * Only the system holds this permission, so requiring it on the receiver keeps other apps' broadcasts out.
     */
    const val PHONE_STATE_SENDER_PERMISSION = "android.permission.READ_PRIVILEGED_PHONE_STATE"

    /** A caller field that fits the wire limit and has no control characters; otherwise it is dropped. */
    fun cleanField(raw: String?): String? {
        val s = raw?.trim().orEmpty()
        if (s.isEmpty() || s.any { Character.isISOControl(it) }) return null
        return s.takeIf { Bounds.utf8Size(it) <= MAX_FIELD_BYTES }
    }

    fun report(state: CallState, number: String?, name: String?, sdk: Int, p: CallPermissions): CallReport {
        val party = state != CallState.Idle
        val n = if (party && p.readCallLog) cleanField(number) else null
        val nm = if (party && n != null && p.readContacts) cleanField(name) else null
        return CallReport(state, n, nm, canAnswer(sdk, p), canEnd(sdk, p), canDial(p))
    }

    /**
     * Decide and perform one request; null means done, otherwise the failure to report. Everything is
     * checked against the *current* state, so a request that raced a state change does nothing.
     */
    fun handle(
        action: CallAction,
        state: CallState?,
        sdk: Int,
        p: CallPermissions,
        telecom: TelecomPort,
        number: String? = null,
    ): CallFailure? {
        val allowed = when (action) {
            CallAction.Answer -> canAnswer(sdk, p)
            CallAction.Reject, CallAction.Hangup -> canEnd(sdk, p)
            CallAction.Dial -> canDial(p)
        }
        if (!allowed) return CallFailure.PermissionMissing
        val fits = when (action) {
            CallAction.Answer, CallAction.Reject -> state == CallState.Ringing
            CallAction.Hangup -> state == CallState.OffHook
            // Only from a known idle phone: never a second call on top of a ringing or live one.
            CallAction.Dial -> state == CallState.Idle
        }
        if (!fits) return CallFailure.NoSuchCall
        if (action == CallAction.Dial && !dialable(number)) return CallFailure.Failed
        return try {
            when (action) {
                CallAction.Dial -> {
                    telecom.placeCall(number!!)
                    null
                }
                CallAction.Answer -> {
                    telecom.acceptRingingCall()
                    null
                }
                CallAction.Reject, CallAction.Hangup -> if (telecom.endCall()) null else CallFailure.Failed
            }
        } catch (e: SecurityException) {
            CallFailure.PermissionMissing
        } catch (e: RuntimeException) {
            CallFailure.Failed
        }
    }
}

/** The only two telecom operations used. Implemented over `TelecomManager` on a device. */
interface TelecomPort {
    /** Throws [SecurityException] without ANSWER_PHONE_CALLS. Does nothing if no call is ringing. */
    fun acceptRingingCall()

    /** Declines a ringing call or ends the current one; false if Android did not end anything. */
    fun endCall(): Boolean

    /** Places a call to a plain number. Throws [SecurityException] without CALL_PHONE. */
    fun placeCall(number: String)
}

/**
 * How the owner is told about, and asked about, a dial a computer requested. Implemented with
 * notifications on a device. Calls may come from any thread.
 */
interface DialPrompts {
    /** "Dialing N requested by C": shown as the call is placed. */
    fun announce(text: String)

    /** Ask whether to place it; true if the question could be shown (otherwise the dial is declined). */
    fun ask(requestId: String, text: String): Boolean

    /** The question is no longer pending. */
    fun withdraw(requestId: String)
}

/** Notices a runtime permission granted or revoked while the app runs (Settings, another screen). */
class PermissionWatch {
    private var last: CallPermissions? = null

    /** True if [now] differs from the previous look; the first look after creation or [reset] is only a baseline. */
    @Synchronized fun changed(now: CallPermissions): Boolean {
        val before = last
        last = now
        return before != null && before != now
    }

    @Synchronized fun reset() {
        last = null
    }
}

/** Whether a dial question can reach the owner. Without POST_NOTIFICATIONS the system drops it silently. */
object DialPromptRules {
    private const val IMPORTANCE_NONE = 0

    fun canShow(appEnabled: Boolean, channelImportance: Int?): Boolean =
        appEnabled && (channelImportance == null || channelImportance != IMPORTANCE_NONE)
}

/** Which phone account (SIM) a requested dial uses. */
object DialAccounts {
    /**
     * Null means "pass nothing and let Android decide". The user's own default outgoing account always wins; with
     * "ask every time" the default voice subscription's account is used, or the only one there is. [accounts] pairs
     * an account with its subscription id.
     */
    fun <T> choose(hasDefault: Boolean, accounts: List<Pair<T, Int?>>, defaultVoiceSub: Int?): T? {
        if (hasDefault) return null
        if (defaultVoiceSub != null && defaultVoiceSub >= 0) {
            accounts.firstOrNull { it.second == defaultVoiceSub }?.let { return it.first }
        }
        return accounts.singleOrNull()?.first
    }
}

/** Resolves a caller's number to a contact name on this phone (needs READ_CONTACTS), or null. */
fun interface ContactLookup {
    fun nameFor(number: String): String?
}

object CallsProtocol {
    const val MAX_WAIT_MS = 50_000L
    private val idPattern = Regex("[A-Za-z0-9_-]{1,64}")

    private fun base(action: String) = JSONObject().put("version", CoreProtocol.VERSION).put("action", action)

    /** The `calls` object of `transport_start`: its presence makes the app declare the packet types. */
    fun startObject(s: CallLinkSettings): JSONObject = JSONObject().put("enabled", s.enabled)

    fun configRequest(enabled: Boolean): String = base("transport_calls_config").put("enabled", enabled).toString()

    fun waitRequest(since: Long, timeoutMs: Long = MAX_WAIT_MS): String =
        base("transport_calls_wait").put("since", since).put("timeout_ms", timeoutMs.coerceIn(0, MAX_WAIT_MS)).toString()

    fun reportRequest(r: CallReport): String {
        val o = base("transport_calls_report").put("state", wire(r.state))
        if (r.number != null) o.put("number", r.number)
        if (r.name != null) o.put("name", r.name)
        return o.put("can_answer", r.canAnswer).put("can_end", r.canEnd).put("can_dial", r.canDial).toString()
    }

    fun resultRequest(connectionId: Long, requestId: String, failure: CallFailure?): String {
        val o = base("transport_calls_result").put("connection_id", connectionId).put("request_id", requestId)
        if (failure != null) o.put("failure", failure.wire)
        return o.toString()
    }

    fun wire(s: CallState): String = when (s) {
        CallState.Idle -> "idle"
        CallState.Ringing -> "ringing"
        CallState.OffHook -> "offhook"
    }

    /** Strictly parses a `transport_calls_wait` result; null if it is not what the core documents. */
    fun parseWait(result: Any?): CallWait? {
        val o = result as? JSONObject ?: return null
        val arr = o.optJSONArray("events") ?: return null
        if (arr.length() > 16) return null
        val out = ArrayList<CallRequest>(arr.length())
        for (i in 0 until arr.length()) {
            val e = arr.optJSONObject(i) ?: continue
            if (e.optString("kind") != "request") continue
            val seq = (e.opt("seq") as? Number)?.toLong() ?: continue
            val conn = (e.opt("connection_id") as? Number)?.toLong() ?: continue
            val id = e.optString("request_id", "")
            val action = CallAction.parse(e.optString("action", "")) ?: continue
            if (!idPattern.matches(id)) continue
            val number = e.optString("number", "").takeIf { it.isNotEmpty() }
            if (action == CallAction.Dial && !CallRules.dialable(number)) continue
            val computer = if (action == CallAction.Dial) e.optString("computer", "").takeIf { it.isNotEmpty() } else null
            out += CallRequest(seq, conn, id, action, if (action == CallAction.Dial) number else null, computer)
        }
        return CallWait(out, o.optLong("last_seq", 0), o.optBoolean("stopped", false), o.optBoolean("enabled", false))
    }
}

/**
 * Glue between telephony, the native core and [CallRules]. All methods are synchronized and must run off
 * the main thread (they call the core).
 */
class CallControlEngine(
    private val core: (String) -> CoreOutcome,
    private val telecom: TelecomPort,
    private val permissions: () -> CallPermissions,
    private val settings: () -> CallLinkSettings,
    private val lookup: ContactLookup,
    private val sdk: Int,
    private val strings: Strings,
    private val clock: () -> Long = System::currentTimeMillis,
    private val onReport: (String) -> Unit = {},
    private val prompts: DialPrompts = object : DialPrompts {
        override fun announce(text: String) {}
        override fun ask(requestId: String, text: String) = false
        override fun withdraw(requestId: String) {}
    },
) {
    private class PendingDial(val request: CallRequest, val atMs: Long)

    private val pendingDials = LinkedHashMap<String, PendingDial>()
    private var state: CallState? = null
    private var number: String? = null
    private var name: String? = null
    private var lastSent: CallReport? = null

    /**
     * Android sends the number in a broadcast and the state through a separate callback; they race, so the number
     * can arrive first. It is kept briefly and used if a ringing state follows at once.
     */
    private var earlyNumber: String? = null
    private var earlyAtMs = 0L

    /** A call was already in progress when this one began ringing (call waiting). */
    private var ringingOverCall = false

    /** The coarse state changed. [number] is whatever Android supplied, if anything. */
    @Synchronized
    fun onState(newState: CallState, number: String? = null) {
        val before = state
        state = newState
        when {
            newState == CallState.Idle -> {
                this.number = null
                name = null
                earlyNumber = null
                ringingOverCall = false
            }
            newState == CallState.Ringing -> {
                ringingOverCall = before == CallState.OffHook
                val n = number ?: earlyNumber?.takeIf { clock() - earlyAtMs <= EARLY_NUMBER_MS }
                earlyNumber = null
                // A second call must not be shown with the first caller's number.
                if (before != CallState.Ringing && n == null) {
                    this.number = null
                    name = null
                }
                if (n != null) setNumber(n)
            }
            newState == CallState.OffHook -> {
                // Ringing ended in a call. If that ringing was a waiting call we cannot tell whether it was answered or
                // declined, so which party is on the line is unknown: say nothing rather than the wrong number.
                if (before == CallState.Ringing && ringingOverCall) {
                    this.number = null
                    name = null
                }
                ringingOverCall = false
                if (number != null) setNumber(number)
            }
        }
        send()
    }

    /** The incoming number arrived after the state (Android delivers them separately). */
    @Synchronized
    fun onNumber(number: String) {
        if (!CallRules.plausibleNumber(number)) return
        if (state == null || state == CallState.Idle) {
            earlyNumber = number
            earlyAtMs = clock()
            return
        }
        setNumber(number)
        send()
    }

    /** A permission was granted or revoked: report the new capabilities. */
    @Synchronized
    fun permissionsChanged() {
        lastSent = null
        send()
    }

    /** Forget everything (link stopped, feature turned off). */
    @Synchronized
    fun reset() {
        state = null
        number = null
        name = null
        lastSent = null
        earlyNumber = null
        ringingOverCall = false
        for (p in pendingDials.values) prompts.withdraw(p.request.requestId)
        pendingDials.clear()
    }

    @Synchronized
    fun onRequests(requests: List<CallRequest>) {
        if (!settings().enabled) return
        expirePending()
        for (r in requests) {
            if (r.action == CallAction.Dial) {
                dial(r)
                continue
            }
            val failure = CallRules.handle(r.action, state, sdk, permissions(), telecom, r.number)
            core(CallsProtocol.resultRequest(r.connectionId, r.requestId, failure))
        }
    }

    private fun answer(r: CallRequest, failure: CallFailure?) {
        core(CallsProtocol.resultRequest(r.connectionId, r.requestId, failure))
    }

    /** A computer asked the phone to dial: refuse what cannot work, then ask or announce, then place it. */
    private fun dial(r: CallRequest) {
        val number = r.number
        val early = CallRules.dialPreflight(state, permissions(), number)
        if (early != null || number == null) return answer(r, early ?: CallFailure.Failed)
        if (!settings().confirmDial) return place(r, number)
        if (pendingDials.size >= MAX_PENDING_DIALS) return answer(r, CallFailure.Declined)
        val asked = prompts.ask(r.requestId, CallRules.dialNotice(number, r.computer, strings))
        if (!asked) return answer(r, CallFailure.Declined)
        pendingDials[r.requestId] = PendingDial(r, clock())
    }

    private fun place(r: CallRequest, number: String) {
        // Re-checked against the state as it is now; announced only when the phone is really about to dial.
        if (CallRules.dialPreflight(state, permissions(), number) == null) {
            prompts.announce(CallRules.dialNotice(number, r.computer, strings))
        }
        answer(r, CallRules.handle(CallAction.Dial, state, sdk, permissions(), telecom, number))
    }

    /** The owner tapped Dial or Deny on the phone's question. Unknown or expired ids do nothing. */
    @Synchronized
    fun resolveDial(requestId: String, allow: Boolean) {
        expirePending()
        val p = pendingDials.remove(requestId) ?: return
        prompts.withdraw(requestId)
        val number = p.request.number
        if (!allow || number == null || !settings().enabled) return answer(p.request, CallFailure.Declined)
        place(p.request, number)
    }

    private fun expirePending() {
        val now = clock()
        val it = pendingDials.values.iterator()
        while (it.hasNext()) {
            val p = it.next()
            if (now - p.atMs < DIAL_CONFIRM_MS) continue
            it.remove()
            prompts.withdraw(p.request.requestId)
            answer(p.request, CallFailure.Declined)
        }
    }

    private fun setNumber(n: String) {
        if (n == number) return
        number = n
        name = if (permissions().readContacts) {
            try {
                lookup.nameFor(n)
            } catch (e: RuntimeException) {
                null
            }
        } else {
            null
        }
    }

    private fun outcomeCode(o: CoreOutcome): String = when (o) {
        is CoreOutcome.Ok -> "sent" + ((o.result as? org.json.JSONObject)?.let { " queued=${it.optInt("queued", -1)}" } ?: "")
        is CoreOutcome.Rejected -> "refused ${o.code}"
        is CoreOutcome.Unavailable -> "core unavailable"
        is CoreOutcome.Malformed -> "malformed reply"
    }

    private fun send() {
        val s = state ?: return
        if (!settings().enabled) return onReport("skipped_calls_off")
        val report = CallRules.report(s, number, name, sdk, permissions())
        if (report == lastSent) return onReport("unchanged")
        val outcome = core(CallsProtocol.reportRequest(report))
        onReport("${CallsProtocol.wire(report.state)} -> ${outcomeCode(outcome)}")
        // Only remember what the core accepted; a not-yet-running core is retried on the next change.
        if (outcome is CoreOutcome.Ok) lastSent = report
    }

    private companion object {
        const val EARLY_NUMBER_MS = 3_000L
        const val MAX_PENDING_DIALS = 3
        const val DIAL_CONFIRM_MS = 45_000L
    }
}

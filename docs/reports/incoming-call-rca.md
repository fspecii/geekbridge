# Incoming call from an iPhone never reaches the PC (Fold5, Android 17): root-cause analysis

Date: 2026-11-04. Device: Samsung SM-F946B (Fold5), Android 17 / API 37, One UI, debug APK code 10099
(`v010-rc1-125-gb9b67bdae`). PC: `geekbridge-service` from `target/release`, state dir `target/realdevice-state`.
Scope: analysis and a proposed fix only. No code was changed, and the live service and phone were only read
(`status`, `events`, `calls`, `phone-log`, `phone-state`).

## Summary

The link, the PC consent, the per-computer grant, capability negotiation, the PC call hub and Android's
READ_PHONE_STATE all work. The PC has a call report from this phone (`ready to answer`). That report can only
have come from a `TelephonyCallback` delivery after the last link restart (01:14:38), so the callback was
registered and fired once, for its initial IDLE. The phone process was alive while both calls rang. So the
`RINGING` change was either never delivered to the app's `CallStateListener`, or was dropped silently between the
callback and the wire. The call path has no log lines at all, so the current logs cannot tell these apart.

Most likely cause: the only state source is the per-registration `TelephonyCallback` on the default
`TelephonyManager`. On this dual-SIM Samsung on API 37 it did not report the ringing call. The likely reasons are
subscription routing, or the record being dropped by `TelephonyRegistry`. It is not something the PC did.

The fix below changes two things. It adds Telecom's `ACTION_PHONE_STATE_CHANGED` broadcast as a second state
source. That broadcast covers all SIMs, does not depend on the callback record, and the app already registers it
when READ_CALL_LOG is granted. It also adds one log line at each step that could drop the state, so the next real
call shows which step failed.

## What the evidence shows

| Fact | Source | What it proves |
| --- | --- | --- |
| `source: Phone link (SM-F946B): ready to answer; to dial from here, allow ...` | `geekbridge-ctl calls` (01:20) | The PC hub has a `LinkCallReport` from this phone, with `canAnswer=true` and `canDial=false`. So `calls_link` registered the link (both sides declared the `geekbridge.calls.*` types), Telephony consent is on, and the phone sent at least one `geekbridge.calls.state`. |
| `telephony: enabled`. Events since the 01:15:22 PC restart are only `connected`, `ping_received` and 2x `consent_changed battery` | `status`, `events 0` | The report was not dropped because consent was off. Consent never changed after the link came up. |
| Phone dump: `READ_PHONE_STATE: granted`, `Calls=ON`, `computer 1 ... calls=ON`, `calls: offered`, `foreground service: running`, `standby bucket: EXEMPTED`, `doze: not idle`, `background restricted=false`, exceptions `none` | `geekbridge-ctl phone-state` | Phone-side gates pass. The app is not cached, restricted or dozing. |
| Last link restart on the phone: `Stopped -> Starting -> Running` at 01:14:38.79. After that the link stays `Running` (the 01:15:20 PC restart is `link lost (service running, phase=Running)`) | phone log | `CallControlRuntime.startObserving` last ran at 01:14:38. That restart rebuilt the native `Core` (`calls.last = None`). The engine was reset too (`lastSent = null`). So the report the PC holds was sent after 01:14:38, by the callback's initial delivery. |
| No `SetupActivity` permission result after 01:14:38. `SetupActivity` detached at 01:14:43 | phone log | `onPermissionsChanged()` did not run again. The 01:14:38 registration is the live one. |
| Display ON at 01:16:16 and 01:17:49, OFF at 01:16:27 and 01:18:20. Background GC at 01:16:18. Phone ping at 01:18:07. Main-thread `onDisplayChanged` lines at 01:17:49 and 01:18:20 | live `phone-log`, `status.last_ping_unix_ms` | The two "screen on" moments are the two incoming calls. The main thread and the Rust runtime were running while each call rang. The app was not frozen. |
| The saved `phonelog.txt` ends at 01:16:27. The live log has lines up to 01:18:20 | `/home/ambsd/.cache/gb-tmp/phonelog.txt` vs live | The log streamer did not stop. The saved file was a snapshot. |
| No `GB` line about calls anywhere | phone log, plus `grep GbLog CallControl*.kt CallState.kt TelephonyRegistrar.kt` (no hits) | **This proves nothing.** The call pipeline never logs: not when the monitor starts or fails, not on callbacks, not on reports. "No call-state lines" would look the same even if everything worked. |

The PC also needs no Bluetooth for this. A ringing link report becomes `CallEvent::CallUpdated{Incoming}` with or
without a number (`calls_link.rs` `LinkCallReport::apply`).

## The pipeline, and where a state can disappear silently

```
TelephonyManager (default sub) --registerTelephonyCallback--> TelephonyRegistrar.registerCallback (cb)
  -> CallStateExecutor thread -> CallStateMonitor (generation guard) -> main.post
  -> CallControlRuntime onState: WidgetUpdater.onCall + async{ engine.onState }      [single "calls-send" thread]
  -> CallControlEngine.send: needs settings.enabled, dedup vs lastSent
  -> core "transport_calls_report" -> Core::calls_report: needs hub.enabled
  -> permitted_ctls("calls", STATE): conn Trusted && permits.allows("calls", peer) && peer_accepts has STATE
  -> Ctl::Calls -> run_trusted -> TLS -> PC gate (negotiated) -> CallHub::on_packet (needs consent) -> view
```

Silent drop points, none of which logs: `MonitorStart.Failed` (swallowed in `startObserving`), the
`RuntimeException` catch in `async`, an `outcome` that is not `Ok` in `send()`, and `queued: 0` from
`calls_report`. Every gate after the callback was shown to pass on this link: the greeting replay in
`run_trusted`, the PC's `negotiated` list, and the hub's `report`. The one gate not covered by that replay is
`peer_accepts`, and the PC's TLS identity carries `LOCAL_INCOMING`, which includes `geekbridge.calls.state`.

## Ranked hypotheses

### 1. The default-subscription `TelephonyCallback` does not see this call (dual SIM / eSIM routing). Likely.

Code: `TelephonyRegistrar.registerCallback` calls `registerTelephonyCallback` on
`context.getSystemService(TelephonyManager)`, which is the default subscription. It does not use
`createForSubscriptionId`, and it registers nothing per subscription. The API 31+ `CallStateListener` docs say the
callback "applies to `SubscriptionManager#getDefaultSubscriptionId()`" when the manager was not created for a
specific subscription. API 31 also deprecated `TelephonyManager.getCallState()` in favour of the per-subscription
`getCallStateForSubscription()` and, for all calls, `TelecomManager.getCallState()`. AOSP's `TelephonyRegistry` also
feeds Telecom's all-subscriptions state to default-subscription records (`notifyCallStateForAllSubs`). Whether a
default record gets the aggregate state or only the default SIM's state is a platform detail. Samsung's telephony is
a fork, and this cannot be decided from our code.

The Fold5 supports physical SIM + eSIM. If the iPhone called the line that is not the default voice/data
subscription, or "calls: always ask" leaves the default as the data SIM, the callback never fires. Its initial
IDLE still arrives, which matches the evidence exactly.

Why it fits: registration succeeds, the initial state arrives, and the process is alive, yet no change is
delivered. The API 36 emulator e2e (`scripts/e2e/call-ui.sh`, single SIM) passes the same code.

Confirm or refute with what exists now:
- Owner, phone: Settings > Connections > SIM manager. How many SIMs are on, which one is "Calls" and which is
  "Mobile data", and which number did the iPhone dial? Then call the *other* line, or switch the preferred calls
  SIM, and watch `geekbridge-ctl calls` while it rings. Ringing that shows on one line only confirms it.
- If adb is available on the PC (it is not on `PATH` here): `adb shell dumpsys telephony.registry`. Look at
  `mDefaultSubId`, `mCallState` per phone during the ring, and the record for `dev.geekbridge.android` (its
  `subId` and events). The local log there shows `notifyCallState`/`notifyCallStateForAllSubs` lines with subId and
  state. Our record on the default sub while the ringing phone index is the other one confirms it.
  `adb shell dumpsys isub` lists the active subscriptions.
- After the fix below: a `calls: state broadcast: ringing` line with no `calls: state callback: ringing` line
  confirms it.

### 2. Our callback record was removed or never got the change (platform or Samsung delivery). Possible.

`TelephonyRegistry` removes a record when a delivery throws `RemoteException` (`mRemoveList`). One failed oneway
delivery, for example while Samsung's freezer ("BBA", `setIsFg isFg = false; delayValue 3999ms` at 01:15:33)
throttled the process, would end all later deliveries for the life of the registration. The app does not
re-register until the next link restart. Android 17's own delivery changes for non-default-dialer apps could do the
same. **Nothing in this analysis confirms such a change.** I know of none, and I did not verify one, so treat it
as an open possibility.

What argues against it: an FGS-holding app in the `EXEMPTED` bucket should not be frozen, and the main thread ran
at 01:16:16 and 01:17:49.

Confirm or refute: `adb shell dumpsys telephony.registry`. Is a `dev.geekbridge.android` record with
`CALL_STATE_CHANGED` still present after the first missed call, and is there a `remove` entry in its local log?
`adb shell appops get dev.geekbridge.android READ_PHONE_STATE` should say `allow`. Without adb, the Diagnostics
screen test below helps: a foreground registration that works points at the background or registration
lifecycle.

### 3. The state reached the app but was dropped between callback and wire. Unlikely, but unprovable without logs.

Candidates: the swallowed `RuntimeException` in `CallControlRuntime.async`; `CoreOutcome` not `Ok` in
`CallControlEngine.send()`, for example `calls_disabled`; `queued: 0` from `Core::calls_report`.

Code review finds no path here that drops a RINGING when an IDLE got through on the same objects. Same engine,
same settings, same hub flag, same permits. The `peer_accepts` gate differs from the replay path, but the PC's TLS
identity declares `geekbridge.calls.state`. The single `geekbridge-calls-send` thread only runs short tasks. The
long-poll `transport_calls_wait` runs on its own thread and releases the hub mutex while it waits.

Confirm or refute: the logging in the fix (`report: queued=N` or `report refused: CODE`).

### Refuted by the evidence

| Hypothesis | Why it is refuted |
| --- | --- |
| Listener lost across link restarts | The last restart at 01:14:38 re-registered, and its initial IDLE reached the core (the PC holds a report). No restart or permission change came after it. `onLinkChanged` ignores repeated phases. |
| `TelephonyCallback` collected by GC (`IPhoneStateListenerStub` keeps only a `WeakReference`) | `cb` is strongly reachable through the `AutoCloseable` lambda in `CallStateMonitor.handle` and the static `CallControlRuntime.monitor`. (`MainActivity` keeps its own.) |
| Executor or threading | `CallStateExecutor` is one daemon thread with a queue of 8. The main thread was dispatching at both ring times. Minor unrelated finding: a new executor is created at every `startObserving` and never shut down, which leaks one idle thread per link restart. |
| Process frozen by the app freezer | FGS running, `EXEMPTED`, not dozing, a GC at 01:16:18, a ping at 01:18:07, main-thread lines at 01:17:49 and 01:18:20. |
| Log streamer stops when the process is cached | The live log continues past the saved snapshot (to 01:18:20). The real gap is that the call path never logs. |
| Permission: READ_BASIC_PHONE_STATE needed on new API levels | The registration needs READ_PHONE_STATE on API 31+ (`CallStateListener` requires it), which is granted. The registration did not throw and delivered IDLE. READ_BASIC_PHONE_STATE dates from API 33, not 31, and is not what call-state callbacks need. |
| Per-computer grant `calls` or PC Telephony consent | The grant is ON, `calls: offered`, consent is on and never changed, and the hub holds the report (a disabled link would print "allow Calls for this phone..."). |
| State dropped while consent was off, then never re-sent | Consent was on throughout this link. `CallHub::set_enabled` also asks the phone for a `report` when consent comes back. |
| Capability names stripped from the plaintext identity (0e3f40157) | Peers use the identity *inside* TLS (`bridge-kde` `link.rs`, `LinkInfo.peer = secure`). The PC's negotiated list shows `geekbridge.calls.state`/`result`. |
| VoLTE/IMS call behaves differently | IMS calls go through the same phone and Telecom notifications as CS calls. It can only matter combined with hypothesis 1 (which subscription the IMS call is on). |

## Tests the owner can run now, without a new build

1. **SIM layout** (hypothesis 1). Note the SIM manager settings, call each line from the iPhone, and watch
   `geekbridge-ctl calls` (or the Calls window) while it rings.
2. **Foreground registration** (hypothesis 1 vs 2). Open GeekBridge > Diagnostics (`MainActivity`). Its "Calls"
   row has its own `CallStateMonitor`, on the same default-subscription `TelephonyCallback`. Keep the phone
   unlocked on that screen so the incoming call shows as a heads-up and the activity stays resumed, then call.
   - The row shows "Ringing": Android delivers to a foreground registration. Suspect hypothesis 2 (the background
     service's record).
   - The row stays idle: the default-subscription callback does not see this call at all. That is hypothesis 1.
3. **adb, if available**: `dumpsys telephony.registry`, `dumpsys isub`, and `appops get ... READ_PHONE_STATE`
   during and after a ring, as described above.

## Proposed minimal fix (not applied)

There are two parts, both in `CallControlAndroid.kt` plus one pure helper in `CallState.kt`:

1. **Add a state source that covers every SIM.** Register the existing `ACTION_PHONE_STATE_CHANGED` receiver
   whenever READ_PHONE_STATE is granted, not only with READ_CALL_LOG, and read its `EXTRA_STATE`. Telecom sends
   this broadcast (`PhoneStateBroadcaster`) for the device's overall call state across all subscriptions and phone
   accounts. Per-subscription changes go out under a different, hidden action. The broadcast does not depend on
   the callback record. KDE Connect relies on the same broadcast. The receiver stays dynamic and not exported
   (API 33+), or guarded by the system-only sender permission below API 33, exactly as today. The number extra is
   still used only when Android supplies it (READ_CALL_LOG) and passes `plausibleNumber`.

   The callback stays, because the broadcast does not give the current state at registration. Once a broadcast
   has been seen, callback changes are ignored. On a dual-SIM phone the two sources could otherwise disagree,
   for example a default-SIM IDLE while the other SIM's call is still OFFHOOK.
2. **Log each step that can drop the state**, coarse state only, in debug builds only (`GbLog` is dead code in
   release). Log the monitor start result, each state with its source, and the outcome of each report. The next
   real call then shows exactly where it stops.

```diff
--- a/android/app/src/main/java/dev/geekbridge/android/CallState.kt
+++ b/android/app/src/main/java/dev/geekbridge/android/CallState.kt
@@ object CallStateMapping {
     fun fromRaw(raw: Int): CallState? = when (raw) {
         RAW_IDLE -> CallState.Idle
         RAW_RINGING -> CallState.Ringing
         RAW_OFFHOOK -> CallState.OffHook
         else -> null
     }
+
+    // Values of TelephonyManager.EXTRA_STATE_IDLE / _RINGING / _OFFHOOK in ACTION_PHONE_STATE_CHANGED.
+    const val EXTRA_IDLE = "IDLE"
+    const val EXTRA_RINGING = "RINGING"
+    const val EXTRA_OFFHOOK = "OFFHOOK"
+
+    /** The `state` extra of Telecom's PHONE_STATE broadcast; anything else is ignored rather than guessed. */
+    fun fromExtra(extra: String?): CallState? = when (extra) {
+        EXTRA_IDLE -> CallState.Idle
+        EXTRA_RINGING -> CallState.Ringing
+        EXTRA_OFFHOOK -> CallState.OffHook
+        else -> null
+    }
 }
--- a/android/app/src/main/java/dev/geekbridge/android/CallControlAndroid.kt
+++ b/android/app/src/main/java/dev/geekbridge/android/CallControlAndroid.kt
@@ object CallControlRuntime {
     private var monitor: CallStateMonitor? = null
     private var receiver: BroadcastReceiver? = null
+    /** Telecom's broadcast (all SIMs) has been heard since observing started: from then on it alone drives the state. */
+    @Volatile private var broadcastSeen = false
     private var waiter: Thread? = null
@@ fun init(ctx: Context) {
         engine = CallControlEngine(
-            core = { core.transportAction(it) },
+            core = { req -> core.transportAction(req).also { o -> if (REPORT in req) logReport(o) } },
             telecom = AndroidTelecom(a),
@@
+    private const val REPORT = "\"transport_calls_report\""
+
+    /** Outcome only: the request carries the caller's number and is never logged. */
+    private fun logReport(o: CoreOutcome) = GbLog.i(
+        "calls",
+        when (o) {
+            is CoreOutcome.Ok -> "report: queued=${(o.result as? org.json.JSONObject)?.optInt("queued", -1)}"
+            is CoreOutcome.Rejected -> "report refused: ${o.code}"
+            else -> "report failed: ${o.javaClass.simpleName}"
+        },
+    )
+
+    /** Main thread. [source] is "callback" (TelephonyCallback, default SIM) or "broadcast" (Telecom, every SIM). */
+    private fun onObserved(e: CallControlEngine, s: CallState, source: String) {
+        if (source == "broadcast") broadcastSeen = true else if (broadcastSeen) return
+        GbLog.i("calls", "state $source: ${CallsProtocol.wire(s)}")
+        WidgetUpdater.onCall(s == CallState.Ringing)
+        async { e.onState(s) }
+    }
@@ private fun startObserving(e: CallControlEngine) {
         val perms = CallPermissionsReader.read(a)
         if (monitor == null && perms.readPhoneState) {
             val m = CallStateMonitor(
                 registrar = TelephonyRegistrar(a),
                 executor = CallStateExecutor.create(),
                 postToMain = { main.post(it) },
-                onState = { s -> WidgetUpdater.onCall(s == CallState.Ringing); async { e.onState(s) } },
+                onState = { s -> onObserved(e, s, "callback") },
             )
-            if (m.start() !is MonitorStart.Failed) monitor = m
+            val started = m.start()
+            GbLog.i("calls", "state callback: " + when (started) {
+                is MonitorStart.Failed -> "FAILED (${started.detail ?: "refused"})"
+                else -> started.javaClass.simpleName
+            })
+            if (started !is MonitorStart.Failed) monitor = m
         }
-        if (receiver == null && perms.readCallLog && perms.readPhoneState) registerNumberReceiver(a, e)
+        // Telecom's PHONE_STATE broadcast: the device-wide state across every SIM (and the number with READ_CALL_LOG).
+        if (receiver == null && perms.readPhoneState) registerPhoneStateReceiver(a, e)
         ensureWaiter(e)
     }
@@ private fun stopObserving() {
         monitor?.stop()
         monitor = null
+        broadcastSeen = false
         val a = app
@@
-    @Suppress("DEPRECATION") // EXTRA_INCOMING_NUMBER is deprecated but is the only number source without a default-dialer role
-    private fun registerNumberReceiver(a: Context, e: CallControlEngine) {
+    @Suppress("DEPRECATION") // EXTRA_INCOMING_NUMBER is deprecated but is the only number source without a default-dialer role
+    private fun registerPhoneStateReceiver(a: Context, e: CallControlEngine) {
         val r = object : BroadcastReceiver() {
             override fun onReceive(context: Context, intent: Intent) {
                 if (intent.action != TelephonyManager.ACTION_PHONE_STATE_CHANGED) return
+                CallStateMapping.fromExtra(intent.getStringExtra(TelephonyManager.EXTRA_STATE))?.let { onObserved(e, it, "broadcast") }
                 val number = intent.getStringExtra(TelephonyManager.EXTRA_INCOMING_NUMBER) ?: return
                 async { e.onNumber(number) }
             }
         }
```

Why this is safe:
- No new permission and no manifest change. The receiver, its export flags and the sender-permission guard below
  API 33 are unchanged. It only registers in more cases.
- `CallControlEngine` already copes with duplicate states. A second RINGING keeps the number. A second OFFHOOK or
  IDLE is a no-op, and `send()` drops a report equal to `lastSent`. With READ_CALL_LOG, Telecom sends the
  broadcast twice (with and without the number). That is harmless for the same reason. State and number go
  through the same single-thread `sender` in arrival order, so `onNumber` after a ringing `onState` attaches the
  number as it does today.
- The callback still gives the state at registration, which the broadcast cannot. If a device never sends the
  broadcast to a dynamic receiver, nothing changes from today.
- Log lines are coarse state words, counts and error codes. No number and no name. `GbLog` does nothing in
  release builds.

Tests to add with the fix (JVM, next to `CallStateMappingTest`):
- `fromExtra` maps "IDLE", "RINGING" and "OFFHOOK", ignores null, "" and other strings, and the constants equal
  `TelephonyManager.EXTRA_STATE_*` (inlined from android.jar, like `rawConstantsMatchPlatform`).
- In `CallControlTest`, a fake-core engine: Ringing from two sources, then a number, sends exactly
  [ringing, ringing+number]. An OFFHOOK then IDLE pair from the second source sends each once.

Verification once built: `adb logcat -s GB` or `geekbridge-ctl phone-log ID -f` while calling each SIM. Expect
`calls: state callback: Started` at link start, then for each call
`calls: state broadcast: ringing` -> `calls: report: queued=1`, and the call ringing in `geekbridge-ctl calls`.

If test 1 shows the callback alone works on one SIM only, hypothesis 1 is confirmed. The broadcast is then the
fix, and a later clean-up can drop the callback in favour of `TelecomManager.getCallState()` (API 31+, READ_PHONE_STATE)
for the initial state. If the broadcast is also silent, the problem is below the app (Samsung or Android 17
delivery to this package). The logs then show that, and the next step is `dumpsys telephony.registry` on the device.

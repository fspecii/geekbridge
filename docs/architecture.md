# GeekBridge: source analysis and proposed Rust architecture

Research date: 2026-11-01; status note 2026-11-02. This began as a design and is now largely implemented: see the
[README](../README.md) for the current feature matrix and what has and has not been verified. The text below keeps the
original research reasoning; sections under "Work still required" and "Product correction" are updated. Real-phone
interoperability and call audio are still unverified.

## Recommendation

Build a local-first Rust daemon with a desktop interface inspired by macOS Continuity: a small menu-bar/tray presence, a device sidebar, unified conversations, contact search, a day agenda, and an incoming-call panel. Use platform-native window behavior and shortcuts on Windows, Linux, and macOS. Android remains the cellular endpoint.

Start with a Rust implementation of the KDE Connect LAN protocol against the existing Android app. This gets a real interoperability target before building a companion app. Keep protocol adaptation separate from the application domain. Add a Kotlin Android companion with a shared Rust library when calendar support, richer capability reporting, or app-specific behavior requires it. A wholly Rust Android app would still require Android framework integration; a thin Kotlin shell makes permissions, providers, lifecycle, and services practical.

Do not depend on any vendor's private phone-companion protocol. GeekBridge uses public, documented interfaces only (the KDE Connect protocol, Android platform APIs, and the public Bluetooth hands-free APIs of each desktop OS).

## Interoperability target

The reference for the wire protocol is the KDE Connect project, whose sources are public:

- Desktop: https://invent.kde.org/network/kdeconnect-kde
- Android: https://invent.kde.org/network/kdeconnect-android

GeekBridge does not vendor or translate that code; it is an independent implementation of the observed protocol
(see "Licensing boundary" below and [kdeconnect-interop.md](kdeconnect-interop.md)).

## Findings from KDE Connect source

Paths below are relative to the appropriate cloned repository and describe the pinned snapshot.

### Transport, discovery, and pairing

Android `backends/lan/LanLinkProvider.kt` uses UDP discovery port 1716 and TCP ports 1716–1764. The same backend also includes `MdnsDiscovery.kt`; do not assume discovery is only a broadcast. `NetworkPacket.kt` serializes JSON packets followed by a newline, with envelope fields and optional payload-transfer metadata. `LanLink.kt` handles separate payload transfers.

The LAN handshake is not simply “open TLS and send JSON.” The TCP initiator takes the TLS server role; the TCP acceptor takes the TLS client role. This inversion also applies to payload sockets; follow the upstream state machine exactly. Protocol 8 first exchanges a bounded plaintext connection packet containing the target device ID and protocol version, then re-exchanges full identities inside TLS. Validate IDs, certificate CN, and negotiated protocol consistency. Accept both string and numeric representations of targetProtocolVersion where stock Android uses them. Trusted peers require stored certificates, and known protocol downgrades are rejected. `helpers/security/SslHelper.kt` currently requests TLS 1.2. A compatibility implementation needs to match those details rather than impose a TLS-1.3-only transport.

Read and reproduce the pairing/trust behavior from the complete backend and pairing code before accepting application packets. Discovery identity is untrusted. Unknown peers must not access contacts, SMS, clipboard, or commands. Bound line length, packet depth, connection count, payload size, and handshake time. Test certificate replacement, revocation, identity mismatch, downgrade attempts, and malformed packets.

### Clipboard

Android `plugins/clipboard/ClipboardPlugin.kt` uses `kdeconnect.clipboard` and `kdeconnect.clipboard.connect`; reconnect synchronization carries a timestamp. `ClipboardListener.kt`, `ClipboardFloatingActivity.kt`, and `ClipboardTileService.kt` show why mobile clipboard is a platform problem: modern Android restricts background reads. The current source includes a privileged READ_LOGS workaround and a focused activity, alongside manual sharing behavior.

For GeekBridge, support explicit “Send clipboard” and foreground reads first. Do not claim that an ordinary foreground service grants background clipboard access. Offer automatic modes only where actual platform capability permits them; do not silently require ADB, root, an IME replacement, or accessibility access. Desktop clipboard handling needs loop suppression, MIME limits, sensitive-item exclusion where metadata exists, expiry, and per-device opt-in. Never persist clipboard history by default.

### SMS

Android `plugins/sms/SMSPlugin.kt` implements message packets, conversation requests, per-conversation requests, and attachment request/transfer packets. Its sent message packet version is 2. Required permissions include SEND_SMS, READ_SMS, and READ_PHONE_STATE. `SmsMmsUtils.kt` is important for provider queries and send behavior. Desktop references are `plugins/sms/` and the SMS application sources.

Start with text SMS, paginated history, selected SIM/subscription, and explicit send status. Distinguish accepted-by-phone, sent-by-modem, and delivery receipt; do not label a queued request delivered. Treat MMS and attachments as a later feature with separate size, carrier, and provider tests. Do not advertise RCS interoperability: it is not a generic capability established by this source analysis.

### Calls

Android `plugins/telephony/TelephonyPlugin.kt` emits ringing/talking/missed-call events and handles `kdeconnect.telephony.request_mute`. The desktop `plugins/telephony/` consumes related events. This supplies call awareness and ringer control, not a demonstrated cellular-audio relay or full answer/hang-up API.

Separate call events, call control, and bidirectional call audio into independently advertised capabilities. Launching the phone dialer is a valid early feature; it must not look like a desktop voice call. Android call control may require a dialer role or Bluetooth telephony integration depending on the operation.

### Contacts and agenda

Android `plugins/contacts/ContactsPlugin.kt` requests READ_CONTACTS and exchanges UID/timestamp lists and vCards through `kdeconnect.contacts.*` packets. It is a useful read-only phone-address-book synchronization baseline, not a complete bidirectional contacts system.

“Agenda” can mean address book or calendar, so the design covers both. The inspected desktop plugin list has contacts, SMS, clipboard, and telephony, but no calendar plugin; the Android manifest contains no READ_CALENDAR/WRITE_CALENDAR declarations. Calendar therefore needs an extension and companion support rather than an assumption of stock interoperability.

## Android constraints verified against official documentation

Android 10+ permits clipboard reads when the app has focus or is the default input method. See [Android privacy changes](https://developer.android.com/about/versions/10/privacy/changes#clipboard-data). Focus, background operation, and phone-to-desktop direction must be tested separately from desktop-to-phone writes.

Google Play restricts SMS and call-log permissions. Default-handler rules have exceptions, including cross-device SMS/call synchronization. GeekBridge should investigate that exception and submit the required declaration; approval is not established by this research. Sideloading does not bypass Android runtime permissions. See [permission policy](https://support.google.com/googleplay/android-developer/answer/10208820).

The [Calendar Provider](https://developer.android.com/identity/providers/calendar-provider) supports events, attendees, reminders, and calendars, with read/write permissions for direct access. Start read-only, select calendars explicitly, and preserve time zones, recurrence, exceptions, and all-day semantics. Calendar access through intents can avoid permissions for individual user-confirmed actions but cannot replace background synchronization.

## Calling: lessons and unresolved work

Borrow the frictionless pairing, permission guidance, reconnect behaviour, device status, conversation interface and visible failure recovery that good phone-companion apps show. Avoid assuming an ordinary third-party Android app has the same OEM privileges.

Full desktop calling is the largest feasibility risk. The phone acts as a Bluetooth audio gateway and the computer needs the hands-free role, including call signaling and duplex SCO/eSCO audio. A generic network audio library cannot obtain protected cellular audio from an ordinary Android app.

Linux is the first candidate for an audio spike: [PipeWire documents HFP hands-free roles](https://pipewire.pages.freedesktop.org/pipewire/page_man_pipewire-props_7.html), but role presence alone does not demonstrate complete call control. Validate BlueZ, PipeWire, and any telephony backend together on hardware.

Windows calling is OS-managed HFP reached through the public `Windows.ApplicationModel.Calls` phone-line APIs; the PC is the hands-free unit and the phone is the audio gateway, so no app-level SCO or codec work is needed. [AudioPlaybackConnection](https://learn.microsoft.com/en-us/windows/apps/develop/media-playback/enable-remote-audio-playback) provides A2DP playback only; it is not duplex calling. macOS needs a hardware spike of the IOBluetooth hands-free APIs.

Do not choose `btleplug` for call audio: its [documented scope is BLE](https://docs.rs/btleplug/latest/btleplug/), not Bluetooth Classic HFP/SCO.

### Emulated call pipeline and its limits (development only)

The Linux call stack can be exercised without a phone or Bluetooth controller: an Android emulator supplies a simulated cellular modem, `geekbridge-phone-sim` (crate `bridge-phone-sim`) turns it into the `org.pipewire.Telephony` D-Bus API on a private bus, and the unmodified `bridge-calls` PipeWire backend and `Session` consume it; `geekbridge-call-e2e` runs scenarios S1-S8 (S8 drives the service call hub, i.e. answer / decline / hang up from the desktop; see [call-control.md](call-control.md)). Run it with `scripts/emulate-call.sh fake` (CI-safe, no emulator) or `scripts/emulate-call.sh emulator`. Full description, scenario table, commands and troubleshooting: [emulated-calls.md](emulated-calls.md); modem behaviour: see that file; the Android app's matching call-state display: [android-calls.md](android-calls.md).

Honest limits: this verifies signalling and the desktop state machine only. It does not exercise Bluetooth pairing, HFP AT commands, SCO audio, codecs, real phones or carriers, or the real PipeWire/WirePlumber/BlueZ implementation, and it carries no audio. It is not evidence for the calling-feasibility gate (step 4 below); a real cellular call with desktop microphone and speakers on each OS is still required, and no call has been placed with GeekBridge.

## Proposed code organization

These are proposed modules, not files already implemented:

- `bridge-core`: device identity, capability model, typed events, feature authorization, operation results.
- `bridge-transport-kde`: bounded packet codecs, LAN discovery, exact KDE handshake, certificates, pairing, payload streams.
- `bridge-sync`: per-device cursors, entity versions, tombstones, reconnect reconciliation, bounded queues, operation deduplication.
- `bridge-store`: SQLite for contacts/messages/calendar, migrations, retention settings, and secret references. Keep credentials in OS-protected storage; protect sensitive cached data at rest and define deletion behavior.
- `bridge-platform`: clipboard, notifications, tray, autostart, secure storage, and OS permission adapters.
- `bridge-calls`: separate OS-specific HFP/control/audio adapters, with explicit unsupported states.
- `bridge-daemon`: lifecycle and authenticated local IPC; keeps connectivity independent of an open window.
- `bridge-ui`: macOS-inspired desktop interface. Evaluate a Rust UI toolkit such as Slint for a Rust-first UI, or Tauri if web UI is acceptable. Neither choice removes native adapters.
- `android/`: Kotlin permissions, content providers, notifications, lifecycle/services, and a shared Rust core through JNI or generated bindings.

Candidate Rust libraries include Tokio, Serde, rustls, SQLite bindings, and OS-specific bindings. Verify current versions, platform coverage, and licenses during implementation; this document does not pin a dependency stack. Especially verify rustls configuration against the actual KDE certificate and TLS behavior.

Use the existing KDE envelope only for interoperability. Custom calendar operations need an explicitly negotiated extension namespace and companion implementation. Keep an independent versioned internal event model so UI and storage do not depend on raw protocol JSON.

## State and security decisions

Pair only after explicit user confirmation, bind trust to the actual peer certificate, and show the verification value derived from the two certificate SPKIs and pairing-request timestamp, following the pinned protocol analysis. Never trust a displayed device name. Revoke trust on both ends where possible and erase local cached data on explicit request.

Per-device permissions independently gate SMS reads, SMS sends, contacts, calendar, and clipboard. Authorize inbound operations in the daemon rather than relying on hidden UI controls. Disable remote command execution in the initial feature set.

Queueing data reads is safe; offline SMS sends and call actions need deliberate semantics. Use operation IDs and persist acknowledgements for the custom protocol. Stock KDE compatibility must not assume deduplication support it does not have. After a disconnect during send, report uncertain status and reconcile instead of blindly retransmitting a potentially billable SMS.

For new calendar synchronization use per-source IDs, revision tokens, tombstones, and a selected source of truth; do not resolve edits only by wall-clock timestamps. Never write imported phone contacts/events into the desktop's native databases without explicit user choice. Read-only in-app views are the initial scope.

Linux adapters must distinguish X11 from Wayland. Wayland clipboard access varies by compositor and available protocols/portals; report unsupported automatic observation honestly. macOS and Windows adapters need corresponding OS clipboard and notification integration. Logs should contain diagnostic IDs and states, not message bodies, phone numbers, clipboard content, or contact data.

## Delivery sequence and acceptance gates

1. **Interoperability spike.** A Rust CLI discovers and pairs with stock KDE Connect Android on a real device, reconnects after restart, handles certificate rejection/revocation, and exchanges ping packets. Gate: successful hardware interoperability and negative trust tests.
2. **Useful desktop MVP.** Add text clipboard with honest Android manual mode, contacts, SMS conversation history and send status, and call event notifications. Add tray UI and device permissions. Gate: real-device send/receive, reconnection without duplicate sends, Unicode/long SMS and multi-SIM behavior.
3. **Custom Android companion and agenda.** Introduce the shared core and versioned extension; implement read-only calendar selection and incremental refresh. Gate: permission denial/revocation, doze/reboot behavior, time-zone changes, recurring/all-day events, and deletion propagation.
4. **Calling feasibility, started early in parallel with product planning.** Prove dial/answer/end and duplex audio independently on each OS. Gate: a real cellular call with desktop microphone and speakers, reconnect, Bluetooth loss, and audio-device switching. Unsupported platforms keep event-only capabilities until proven.
5. **Product finish.** Polish the Continuity-inspired UI, notifications and shortcuts; package and sign per OS; establish update, cache retention, and permission-recovery flows. Gate: clean-machine installation and an OS/device regression suite.

Do not promise a release date for complete cross-platform audio before step 4. The initial shippable bridge can deliver contacts, SMS, useful clipboard actions, agenda through the companion, and call awareness while that feasibility work proceeds.

## Licensing boundary

The inspected KDE code carries file-level SPDX licenses, including GPL alternatives; desktop dependencies and individual files vary. Cloning for research does not select GeekBridge's license. Translating or copying implementation code into Rust is not automatically independent. Preserve provenance and decide licensing before incorporating upstream code. A separately authored compatible protocol implementation should document its sources and undergo a license review before distribution. No Microsoft binary or private implementation belongs in GeekBridge's shipped code.

## Work still required

The initial research did not include an Android build, live pairing, or an audio test (an Android build and an emulator-only call-signalling check exist; see above and [builds.md](builds.md); live pairing and audio tests are still outstanding). Implementation has since progressed well beyond this: the apps, the authenticated service, TLS pairing (including QR), sharing, clipboard sync, SMS, contacts, calendar, call control, the update flow and the Android companion exist and are tested against scripted peers and an emulator. Real-phone interoperability and Bluetooth call audio remain unverified hardware investigations.

## Product correction: independent applications

Calls, Messages, Contacts, Calendar (executable `geekbridge-agenda`), and Clipboard ship as separate executables and OS application entries, each with its own window and process. A shared background service owns the phone connection. There is no all-in-one application shell or cross-feature navigation. Contacts and Calendar were originally one "Agenda" app and are now separate.

The first implementation introduces the Cargo workspace, typed discovery data, passive bounded KDE identity discovery, device expiry, and a bounded read-only loopback discovery endpoint. This endpoint exposes only untrusted LAN announcements and must not be extended to phone content or commands without authenticated local IPC. TLS, pairing, and phone feature access remain subsequent work.


## Open-source audit decisions

Implement Windows controls independently against public WinRT APIs, with OS-managed HFP audio. Use the existing PipeWire telephony adapter for Linux controls, then add explicit session-scoped audio routing and echo cancellation. macOS continues to target IOBluetooth HandsFree plus native audio routing, with hardware feasibility still unproven. Shared Rust/mobile binding examples inform lifecycle and build structure; they do not establish cellular-calling support.

# Parity gaps against Microsoft's phone companion, round 2

Date: 2026-11-03. Scope: user-visible features and behaviours of Microsoft's Phone Link / Link to Windows compared with
GeekBridge as of 0.1.0. Source of the Microsoft side is our own private static-analysis notes (names and behaviours
only, never code or strings); this document restates conclusions in our own words and cites nothing proprietary.
Phone Link evidence is mostly inferred from component names and manifests, so "they do X" below means "it appears to".

Legend: **Have** = shipped and tested, **Partial**, **Gap**, **By design** (we choose not to match).
Priority: P1 high value and feasible soon, P2 worthwhile, P3 later or optional.

## Comparison

| Area | Phone Link (appears to) | GeekBridge now | Verdict |
| --- | --- | --- | --- |
| Calls: answer, hang up, mute, dial from the PC | Uses the phone as a Bluetooth hands-free audio gateway; PC does the heavy work | Same model, desktop side owns the audio path; dial from PC, answer and end are in the call path audit | Have |
| Calls: robust retry when the Bluetooth headset profile hangs | Timeout and retry around profile-proxy acquisition | Covered on the desktop side; phone-side helper only reports state | Partial (P3) |
| SMS threads, send, delivery state | Observes the SMS store, sends through the platform API, echoes sent/delivered per message | Threads, send with honest states, SIM choice, segment counter | Have |
| MMS | Shows attachments | Placeholder row "open it on the phone"; no attachment bytes requested | Gap (P2) |
| Reply from a notification | Inline reply on mirrored chat notifications | Desktop banner for a new text has a Reply action that opens the thread | Have (text only through the Messages window) |
| Notification banner behaviour for texts | One entry per conversation | Previously: one banner per burst for the whole phone, so a second sender within 2 s was silently lost; banners stacked | Fixed in this round (see below) |
| Notification mirroring of other apps, dismiss both ways | Listener service in its own process mirrors every app, actions, dismiss syncs back | Not claimed. No listener, no packet kinds, consent exists but no adapter | Gap (P1 for value, large) |
| Read state | Mark read syncs | Local to the computer only; stock protocol cannot mark read on the phone | By design (limit of the protocol) |
| Photos and recent media on the PC | Content-change jobs and media permissions, gallery view | Files can be sent either way; no gallery or "recent photos" feed | Gap (P2) |
| Battery level and charging state of the phone | Shown on the PC | The phone sends nothing; the interop capture pins that no battery packets exist | Done: see docs/reports/battery-status-feature.md |
| Connection status vocabulary (asleep, battery-restricted, Wi-Fi changed, PC off) | Same words on both sides | Home shows connected/offline and a battery-restriction warning; no per-cause "why disconnected" on the desktop | Partial (P2) |
| Hotspot, do-not-disturb, ring-mode toggles from the PC | Hotspot handoff and DND as feature-level permission requests | None | Gap (P3; DND needs a special Android access grant, hotspot needs privileged APIs on recent Android) |
| Pairing | Scan a QR on the phone, then confirm | QR scan plus verification code with both fingerprints shown | Have |
| Onboarding permission order | Permissions asked in the moment of use, with a "why" screen and a settings redirect when permanently denied; the PC can request a feature permission | Wizard orders pair, notifications, contacts, calendar, phone state, SMS, battery with an overview; settings redirect exists | Have for order; PC-initiated permission request is a Gap (P3) |
| Battery-exemption prompt timing | Asked after first activation with a plain reason | Final wizard step with detection and revoke warning | Have |
| OEM autostart guidance | Not visible in their app | Test plan lists per-vendor steps; no in-app guided screen | Partial (P2) |
| Reconnect and keep-alive | Foreground service of the connected-device type, boot and update restarts, push wake-up through vendor cloud, Bluetooth side channel wake | Foreground service, boot and update restarts, bounded back-off redial, mDNS re-announce | Have; push and Bluetooth wake By design (no cloud) |
| Wake a sleeping phone from the PC | Via Bluetooth side channel | None; relies on the phone announcing itself | Gap (P3, opt-in only) |
| Clipboard | Request/response with on, off, status, reject versus fail | Shipped with consent and history | Have |
| Share sheet and file transfer | Direct-share target per PC, progress, cancel | Shipped | Have |
| Updates | Store-delivered plus a coupling check | Own updater with signature checks | Have |

## Prioritised gap list

1. **Notification mirroring with dismiss (P1, large).** Biggest visible hole. Needs a notification-listener service on
   the phone, an allow-listed packet pair, a per-app filter (default deny), a consent that already exists
   (`Feature::Notifications`), and dismiss in both directions keyed by an opaque id. Privacy review first: bodies may
   hold codes and secrets, so hidden-by-default like texts. Too large for this round; first step is a design note.
2. **Phone battery and charging state (P1, medium).** Smallest self-contained telemetry. Add a `Feature::Battery`
   consent, declare the stock battery packet in both directions, send on change plus a low threshold event, store
   in memory only, show on Home and in the tray. It touches the capability tables and the interop tests that pin the
   negotiated lists, so it needs its own change with those tests updated deliberately.
3. **Per-sender text banners that do not hide each other (P1, small). Done.** Details below.
4. **Why-disconnected status (P2, small).** Name the cause on the desktop (phone silent, battery restricted by the
   phone's own report, different network, computer offline) using one vocabulary on both sides.
5. **MMS attachments (P2, medium).** Request and show small images in a thread, with size caps and no auto-download
   over limits.
6. **Recent photos feed (P2, medium).** Read-only, user-selected folder or the media permission, thumbnails on demand,
   never a background upload.
7. **In-app OEM guidance (P2, small).** A "fix disconnects" screen driven by a table of vendor intents, shown only when
   the intent resolves; link to a community page.
8. **PC-initiated feature permission request (P3).** Desktop asks, phone answers granted, denied or permanently denied
   with a deep link. Nice for the QR flow; not needed while onboarding is phone-led.
9. **Hotspot, do-not-disturb, ring mode (P3).** Constrained by what Android lets a normal app do; do-not-disturb
   needs a special access grant, hotspot control is not available to normal apps. Treat as likely won't-do.
10. **Bluetooth wake of a sleeping phone (P3, opt-in).**

## Implemented in this round

Both changes are on the desktop notification path for incoming texts (`crates/bridge-service/src/phone_data.rs`).

* **A second sender is no longer swallowed.** The old rule allowed one banner per two seconds for the whole phone and
  kept only the first sender, so two different people texting at once produced one banner. Now the quiet period is per
  sender, up to three distinct senders are announced per packet, one banner per sender, and a bounded table
  (64 senders) remembers when each last fired. A flood from one sender is still limited to one banner per gap.
  Test: `a_second_sender_in_the_same_burst_is_still_announced`.
* **A new text replaces the sender's previous banner.** The banner now carries the instant-message category and a
  replace key derived from the digits of the number, so servers that honour it update one entry per conversation
  instead of stacking a pile. Banners without a number (no reply possible) are unchanged. Options stay ahead of the
  argument separator so sender text can never be parsed as an option. Test:
  `banner_replaces_the_previous_one_from_the_same_sender`.

## Not done, and why

Battery status and notification mirroring were the two obvious candidates for this round, but both need a new consent,
new capability entries on both ends, Kotlin changes and a rewrite of the pinned negotiation tests. That is more than a
small change and would collide with work other agents have in progress, so they are ranked above and left for a
planned change with a protocol note first.

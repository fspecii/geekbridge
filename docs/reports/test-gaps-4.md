# Test gaps 4: features added on 2026-11-03

Scope: PC dial (`call_cli`, `call_control`), `doctor`, the "why is the phone not connected" mapping, phone battery,
notification mirroring (Kotlin and desktop, caps and TTL), `codepages::drop_idle`, `Response::from_json`, the screen-off
heartbeat cadence, and the announce pause with newcomer wake. Tests only; the three bugs below were found by a failing
test first.

`cargo llvm-cov` is not installed here (`cargo: no such command: llvm-cov`, only the `llvm-cov` binary exists), so gaps were
found by reading each file for branches that no test reached, not by line counts. `docs/coverage.md` has the command for a
machine that has the tool; re-measure these files there.

## Bugs found (each failed first, then fixed in the smallest way)

| # | Where | Bug | Fix |
| - | - | - | - |
| 1 | `MirrorRules.clip` (NotificationMirror.kt) | Cutting at the cap could split an emoji, leaving a lone surrogate in the app name, title, text or button label sent to the computer. | Drop a trailing high surrogate after the cut. |
| 2 | `NotificationMirror.lastSent` | Held in a `HashMap` but trimmed with `keys.first()`, which is an arbitrary key, not the oldest: a just-sent notification could be forgotten and sent again. | `LinkedHashMap`, so the oldest goes first as the comment promises. |
| 3 | `BatteryRules.reading` | `(level * 100 / scale).toInt()` overflowed for an absurd extra (`Int.MAX_VALUE` over scale 1 gave 0 % and "low"). | Clamp as a `Long` before narrowing. |
| 4 | `NotificationMirror.onAction` / `MirrorAndroid.press` | The computer only ever sees the clipped (48 character, trimmed, at most three) button titles, but the phone compared what the computer sent against the raw titles, so a long or padded title never pressed anything. | Compare against the clipped titles on both sides. (Another change had the same fix in the working tree earlier and was gone when I checked; the test pins it either way.) |

## Added tests

Rust (about 130 new tests):

- `doctor.rs`: every ufw state (on and readable, on and unreadable, off, none) with and without firewalld, per-protocol
  messages, port-range edges (`1717:1764`, `1716:1763`, reversed, garbage, missing value after `--dport`, other chain), a
  check that `firewall_blocks` and a failing check always agree, Wayland / X11 / runtime dir / Hyprland variants,
  the non-Linux path (`session_check_on(.., false)` never warns) plus the real `session_check` per platform, reconnecting
  versus reason versus connected phones, stale reasons, clipboard and calls levels, scrubbed service errors, check order
  and the service-down short circuit, `render` counts, all `hint_for_error` arms.
- `why_disconnected.rs` and `why.rs`: pairwise precedence of every cause, zero retry cap, each trust and timeout keyword in
  any case and for the right link end only, firewall rewrite of exactly four codes, `age_words` boundaries, `one_line`
  for every code, old and future status JSON, sightings cap, `last_end` with other events, loopback / link-local /
  unspecified addresses, /24 matching on several interfaces.
- `call_control.rs`: dialing over the link and through hands-free (number sent once, consent, phone permission, forged or
  foreign source, live call, two phones, pending cap of four, dead link, full queue, refusal notice, host failure),
  the 1.5 s grace for a ring with no caller, announce once and again for a new call, a call that ends during the grace,
  banner button presses and stale presses. Plus `call_cli.rs` (formatting, masking, usage) and an end-to-end ctl test in
  `tests/calls_api.rs` for `calls`, `--reveal`, answer / reject / hangup / mute / audio refusals and the phone's notice.
- `battery_link.rs` (every level -5..105, threshold values, which field is blamed, build/parse round trip) and
  `tests/battery_api.rs` (consent, request on enable and on relink, every field through the status, withdrawal erases,
  malformed is an offence and never replaces a good reading, unknown level is neither).
- `notification_link.rs` (id and text caps in characters, button caps, time forms, ticker fallback, flags, strict
  requests) and `tests/mirror_caps.rs` (TTL boundary, update restarts the clock, per-phone cap with update not evicting,
  one phone cannot evict another, device table full then freed by clear / remove / expiry, newest-first view, name
  cleaning, banner burst then one per gap, no banking over a long idle, backwards clock, button-key parsing, a 6-thread
  stress run).
- `crates/bridge-core/tests/drop_idle.rs`: `drop_idle` called hundreds of times while four threads compute, hash, parse JSON
  and sort and check their results, from six threads at once, while threads start and finish and loopback TCP runs, and
  right before panics unwind. None crashed, with the current tree (which reads `smaps`).
- `ipc.rs`: `from_json` against the derived parser for all 17 `Response` variants (a real sample of each, checked to parse
  first): about 2,000 one-place edits (remove, unknown member, every JSON type, doubled arrays, depth 3), every body under
  every tag, 70 hand-made tag and number shapes (duplicate `kind`, escapes, BOM, NUL, invalid UTF-8, lone surrogates), every
  truncation and 300 seeded byte corruptions per variant. Both parsers agree everywhere.
- `heartbeat.rs`: simulated timer loop for 10 and 30 minutes with the screen on, off, and toggled (20 s / 60 s cadence,
  next-tick wake-up, worst idle gap inside the silence limit), boundaries, a peer that pinged before going idle is never cut
  off mid-gap, gap window edges, bursts, prefix-only capability matching.
- `tests/transport.rs` (Android crate): announcements stay silent while every paired computer is linked, a linked computer's
  own announcement does not wake them, an unpaired one is answered within two seconds, a burst of ten newcomers produces one
  deferred answer and no storm (the 5 s `WAKE_GAP`). Takes about 9 s.

Kotlin (JVM, 26 new tests): `NotificationMirrorTest` (button titles, surrogate cut, clip boundaries, time and ticker,
request and action parsing edges, bulk stops at the first refusal, off switch, reset forgets ids, id reuse, repost memory,
skip-reason order) and `BatteryReportTest` (exact 60 s gap and remainder wait, early trailing send reschedules, switched off
meanwhile, revert sends nothing, failed broadcast not remembered, one retry for rate limits only, replies do not touch the
broadcast memory, reading edges, body fields).

## Observations, not changed

- A phone that is screen-off from the moment the link comes up pings every 60 s, outside the 10 to 50 s window that marks a
  peer as heartbeating, so the computer never applies the 70 s silence rule to it and relies on TCP keepalive. This is
  fail-safe (never a false cut-off) and is pinned by `a_peer_seen_only_pinging_a_minute_apart_is_never_cut_off`.
- The Android `org.json` `optBoolean("request")` accepts the string `"true"`, so a malformed request body still means
  "send everything". Harmless; the test no longer asserts otherwise.
- The real engine tick (`HEARTBEAT_EVERY`) and `SCREEN_OFF` flag are exercised through the same `Heartbeat` calls, not in a
  60 s end-to-end run; the simulation drives the identical `ping_due_idle` loop.

## Verification

See the commit; commands: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, and `./gradlew :app:testDebugUnitTest` (via `scripts/android-env.sh`).

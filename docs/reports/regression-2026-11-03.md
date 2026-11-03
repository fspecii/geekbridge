# Regression run, 2026-11-03 (emulator / e2e suites)

Built from `origin/main` 1a16464b6 in a clean worktree (own cargo target, debug APK built from it), fresh AVD clones of `gb36x`
(and `gbhome` for the clipboard comparison), own ports and Xvfb displays, all clones, emulators, Xvfb and services stopped or
deleted afterwards. The live service (1716/17381) and the 8000 server were not touched. Free disk was 126 GB before the run.

## Results

| Suite | Result | Notes |
|---|---|---|
| `e2e-android.sh` (all stages, gb36x clone) | PASS: env, battery, pair, reconnect x3, clip_d2p, clip_p2d x3, file_d2p, share_text, file_p2d, update | First attempt failed from `battery` on (System UI ANR at host load 30-77); rerun at load 13 passed |
| `e2e-android.sh` full sequence on a `gbhome` clone | PASS, same stages | Load about 2 |
| `e2e/call-ui.sh` | 12/12 PASS on the last full run | Earlier attempts hit other agents' services on the default ports; see "Flakiness" |
| `e2e/pair-background.sh` | PASS: prehome, pair, home, swipe, doze | |
| `e2e/update-install.sh` desktop | all PASS | |
| `e2e/update-install.sh android` (`E2E_UPD_REBUILD=1`) | 30 PASS, 1 SKIP (`android-skip-version`, by design) | needs `jniLibs` copied into a clean worktree (gitignored) |
| `scale-e2e.sh` short mode (300 contacts, 1000 events, 40 threads) | env, seed, battery, pair, measure PASS | contacts full sync 2.4 s, calendar first 0.95 s, SMS first 0.48 s, phone PSS 64 to 77 MB, desktop RSS 8.6 to 14.4 MB. The first attempt failed in `battery`: "System UI isn't responding" while update-install ran in parallel; rerun alone passed |
| Android unit tests / lint after the fix | 499 tests, 0 failures; lint clean | |

## Clipboard phone-to-desktop: image quirk or link bug?

Not an image quirk. The same stage subset (`--only env,battery,pair,clip_d2p,clip_p2d`) fails on both a `gbhome` and a `gb36x`
clone, and the full default sequence passes on both. The subset skips `reconnect_after_restart`, which is what restarts the
phone's link from Home with Clipboard sync on. The earlier "fails on gbhome, passes on gb36x" was most likely which link the
phone happened to have when the stage ran (and load), not the image.

Root cause, a real (small) app bug: capabilities are declared in the identity packet when the link starts. If the user switches
Clipboard sync ON while a link that started with it OFF is up, the desktop never learns (`negotiated` stays
`ping, share.request`), so pulls are refused ("no connected phone has Clipboard switched on", or "app too old"), and the phone's
clipboard packets are counted as rejected on the desktop. Phone logcat shows `capability: clipboard: offered` but no
re-declaration. Contacts/calendar/SMS already reconnect on such a change (`CapabilityRedeclare`); clipboard did not.

Fix: `ClipboardRuntime` remembers whether the running link declared clipboard (`startSettings()`), and `update()` reconnects
when it is switched on while a link without it is up (`CapabilityRedeclare.clipboardNeedsReconnect`, unit test
`clipboardSwitchedOnOnALinkThatDidNotDeclareItReconnects`; new string `clip_reconnecting`). Verified on an emulator: before the
fix the subset failed clip_d2p/clip_p2d/window/locked; after it, with the toggle followed by the normal redial, they pass
without an app restart.

Separately observed (not fixed): about 4 to 60 s after such a toggle the link closed (`peer_closed` / `conn_aborted`) and the
phone's redials failed with `tls_failed` for a few attempts; cause not isolated (it is also seen once per desktop disconnect in
passing runs, then recovers). Logs: `~/.cache/gb-regress3/logs/` (`e2e-home3.logcat`, `drive.logcat`, `e2e-sub36x.logcat`).

## Flakiness seen (harness, not product, as far as determined)

- System UI ANR on fresh clones at host load above 25 (three occurrences); every one passed on rerun at lower concurrency.
- `call-ui` `callui_pair` failed 2 of 8 attempts in different ways (Continue not advancing; link not connected after the
  forced app restart in `write_calls_prefs`); `callui_popup_ring` once ("desktop never saw the call ringing"). Another agent's
  call-ui/pair-background/e2e runs were using the default ports and AVD names at the same time, which also caused one full
  failure; own ports were used for the final run. Worth a look if it recurs on a quiet host.
- `e2e-android` clip_d2p checks `negotiated` before it turns the phone setting on, so it depends on an earlier stage having
  restarted the link; running it alone fails. Left as is.

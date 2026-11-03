# Share to computer: emulator end-to-end suite (2026-11-04)

New suite `scripts/e2e/share.sh` (own AVD `gbshare`, console port 5676, adb server 16190, Xvfb :96, private desktop service on
control port 17911, own HOME, `xdg-open` / `notify-send` logging shims; never the live service or ports 1716, 17381, 8000).
It covers the AirDrop-like flows in both directions with a real "other app": `scripts/e2e/share-sender/` is a test-only APK
(javac + aapt2 + d8 + apksigner from the project SDK, `build.sh`) with a content provider that serves generated files (any
name or size, throttled, slow to open, no name or size column, grants revoked a moment after the first open) and a
trampoline activity that starts ShareActivity with EXTRA_STREAM / ClipData and a read grant, as Gallery, Browser and Files do.
Added to `.github/workflows/emulator-e2e.yml` as `share`, **experimental (non-blocking)**; the sender APK is built in the
build job and packed in the artifact.

Last full local run: 28 rows, all PASS (`dist/android/logs/e2e-share-results.tsv`). The emulator itself aborted twice on this
shared host (SIGABRT in the emulator process, not the app) and those runs were repeated; no test was weakened to pass.

## What is covered

| Step | Result |
|---|---|
| ACTION_SEND text (unicode, Arabic, CJK, emoji) | whole text arrives as a text transfer; clipboard untouched |
| ACTION_SEND link | link transfer; **not opened** (no xdg-open), clipboard untouched, notified |
| ACTION_SEND image/jpeg, MediaStore EXTRA_STREAM (`am start -a SEND ... --eu`, through the chooser) | sha256 equal |
| Provider URI (ClipData + read grant); provider without size / name | delivered; no size is refused in words |
| ACTION_SEND_MULTIPLE, 3 files | all three, sha256 equal |
| Names: unicode, RTL override, 300 characters, `../../etc/passwd`, `...`, reserved characters, Arabic, `CON.txt` | all saved inside the save folder with safe names, extension kept |
| Duplicates (5 in one batch, then 1 more) | `same.bin`, `same (1).bin` ... six files, nothing overwritten |
| Android 14 grant revoked 150 ms after the first open | all three files complete |
| 200 MiB | sha256 equal; phone PSS 53 MB at rest, peak 54 MB (+1 MB, limit 40) |
| Leave the Share screen mid-transfer, `am kill` (also with "Stay connected" off) | completes |
| A second share while one runs | joins it, all delivered |
| Cancel from the phone / from the computer, mid-transfer | cancelled on both sides in words, no partial file, next share works |
| Computer off, then back | screen says "not found on this network"; delivered on its own when the computer returns |
| Computer vanishes mid-transfer | failure in words; retry works |
| Computer with receive off | nothing stored; file shows "waiting for the computer (it may need Share files > Allow ...)"; text says it only shows up if allowed |
| Computer to phone: Accept (sha256), same name twice (unique name, first intact), Decline, computer cancels mid-transfer, link (Open, never automatic), text (Copy) | pass |

## Real bugs found and fixed (test first)

1. **Third file of a batch expired** (desktop). With auto-accept on, only 2 transfers per phone run; the rest stayed "offered" and
   expired after 120 s even though the first two had finished (`multiple`, `names`, `duplicates` failed: a 50-photo share lost
   all but two). Finished transfers now start the waiting auto-accepted offers (`waiting_for_a_slot`, unit test).
2. **The share session ended while the file was moving** (phone). The core answers "started" before the next poll lists the
   transfer, so `endIfDone` saw nothing open: the screen showed only "Done" (no Cancel, no Hide), the hold on the link was
   released. Fixed with `ShareSessionLogic.itemOpen` (unit test).
3. **A share dies when the screen is left and "Stay connected" is off** (phone). No foreground service held the process, so
   Android could kill it (`am kill`: transfer Failed). A running share now keeps the service up (`ServicePolicy.shouldRun(sharing)`).
4. **Android 14 grant lifetime**: files are opened one after another; leaving the screen (Hide/Back) before the last was opened
   failed it with SecurityException. The screen now goes to the back and finishes when every file is open (`EarlyOpen.isSettled`,
   60 s cap).
5. **A second share while one runs was dropped silently** (`sessionActive` returned early; the second screen showed the first
   share). It now joins the running share.
6. **Raw codes on the Share screen**: `failed (io_error)`, `failed (link_lost)`. Words now (`ShareText.failure`, new codes
   link_lost, io_error, port_rejected, no_port, busy). A text/link used to read "queued" forever; it now says it was sent and only
   shows up if the computer allows it.
7. **RTL override in names** (phone): `NameRules.sanitize` and the Share screen kept U+202E and other bidi controls
   (the Rust core already replaced them).
8. **Share screen redrawn every poll** (twice a second while sending) rebuilt identical views: TalkBack loses its place and
   uiautomator never sees an idle screen. It now redraws only when what it shows changed.

## Not fixed / limits

* **No refusal message exists in the protocol.** A computer with receive off (or a user who taps Decline) simply never connects
  to the phone's payload port; the phone shows the "waiting ... Allow this phone" hint and then times out. For text and links there
  is no acknowledgement at all, so the phone can only say it handed the text over. A real "refused" answer needs a new
  GeekBridge-only packet (stock KDE Connect has none); left for a decision.
* The progress bar changes the Share screen every poll while a transfer runs; the suite therefore uses 256 KB/s transfers when
  it has to tap during one.
* The sender app's provider is not a real FileProvider (it streams through a pipe, like Photos or Drive; the 200 MiB case streams
  from a pipe, not from a file).
* The emulator crashes about one run in three on this host; a retry passes.

## Verification

`gradle testDebugUnitTest lintDebug` exit 0 (new tests: bidi names, failure wording, session logic, early-open, service policy);
`cargo test --release -p bridge-service` exit 0 (561 tests, new `offers_over_the_limit_wait_for_a_slot...`), `cargo clippy ... -D warnings` and
`cargo fmt --check` clean.

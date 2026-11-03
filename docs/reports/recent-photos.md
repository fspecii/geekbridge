# Recent photos feed (parity gap 6) and MMS attachments (gap 5)

`docs/reports/parity-gaps-2.md` listed two P2 gaps: **5, MMS attachments** and **6, a recent-photos feed**. This change does
the careful design pass for both (`docs/design/recent-photos.md`), implements the smaller one, **recent photos**, end to
end, and leaves MMS designed but unbuilt.

## Decision

Recent photos is the smaller change because it reuses what exists (consent and per-computer grants, the phone data relay,
the in-memory store pattern of notification mirroring, the share channel for big bytes, the Share window) and adds one packet
family, one Android media-store reader and one tab. MMS needs the same privacy shape but also a messages-model change (previews
inside a thread, part lookup in the telephony provider, carrier variation) that cannot be verified without a SIM; the design
note records how it would reuse the thumbnail packet and the "expected file" mechanism built here.

## What was built

* **Packets (GeekBridge only):** `geekbridge.photos.request` (`list`, `thumb`, `get`), `.list`, `.thumb`. A stock KDE Connect peer
  never declares them, so it is never offered them. Thumbnails are small base64 JPEGs inside the packet (at most 24 KiB,
  256 px); a full picture is an ordinary `kdeconnect.share.request` file offer, accepted without the general receive
  permission only because the user just asked for that very photo.
* **Consent:** new `Feature::Photos` (off by default, per phone) with `geekbridge-ctl consent ID photos on` and an *Allow photos
  from PHONE* button in the window; on the phone a switch (off), Android photo access (Android 14 partial access supported)
  and the per-computer grant `GrantFeature.Photos`.
* **Desktop service:** `crates/bridge-service/src/photos.rs` (bounded memory store: asked-for-only lists and thumbnails, 24
  photos per phone, 8 phones, 2 MiB of thumbnails, 30 min, erased on consent withdrawal, link loss and unpair), daemon
  requests `Photos`, `PhotosRefresh`, `PhotoThumb`, `PhotoFetch`, and the expected-file mechanism in `share.rs` (one-shot,
  same phone, same sanitised name, 2 minutes; Copy mode lands in a private scratch folder emptied after 10 minutes).
* **CLI:** `geekbridge-ctl photos [DEVICE] [--json]`, `photos refresh|save|copy`.
* **Window:** `crates/bridge-apps/src/photos_pane.rs`, a *Phone photos* tab in the Share window (tab strip only while a connected
  phone offers photos): grid of previews decoded off the frame thread, selection, Save, Copy to clipboard (`wl-copy`/`xclip`,
  PowerShell and `osascript` untested), live progress line, privacy line, *Stop showing photos*.
* **Phone core (Rust):** `Feature::Photos` in the data relay (declared only when on; permit key `photos`).
* **Android (Kotlin):** `RecentPhotos.kt` (rules, ids, answers), `RecentPhotosAndroid.kt` (media store reader, thumbnails,
  hand-off of the full file to the share coordinator), settings switch with readiness text, manifest permissions
  (`READ_MEDIA_IMAGES`, `READ_MEDIA_VISUAL_USER_SELECTED`, `READ_EXTERNAL_STORAGE` up to API 32), strings.
* **Simulated phone:** `bridge-phone-sim` serves a made-up photo library (eight flat-colour thumbnails in
  `crates/bridge-phone-sim/fixtures/photos`).

## Tests

* Core: `photos_link` unit tests (round trips, strictness, caps, bidi stripping, thumbnail exactly at the cap).
* Fuzz: new target `photos_wire` (`crates/bridge-fuzz`, `fuzz/fuzz_targets/photos_wire.rs`, documented in `docs/fuzzing.md`) with
  oracles for list size, unique valid ids, clean names, thumbnail bytes and pixels, request ranges; it runs in the
  deterministic 2000-input test of every target.
* Service: `photos` store unit tests; `gate` unit test; `tests/photos_api.rs` end to end over pinned TLS (nothing asked or
  kept before consent, unasked list and thumbnail ignored and not punished, one list and one thumbnail per photo and never a
  full picture, refresh coalescing, erase on withdrawal, a full picture accepted only for a listed photo asked for and once,
  receive permission stays off, Copy lands in a 0700 scratch folder and not the save folder, withdrawn request forgotten).
* Negotiation: `android_negotiation.rs` (the real stock Android identity is never offered or routed a photo packet),
  `stock_peer.rs` (a stock phone with every consent on is sent no photo packet), `control_api.rs` consent table.
* Phone core: `data_transport.rs` (declared only when on; per-computer permits; requests become events; replies go to one
  connection; stock desktop gets no photo packet), `data.rs` unit test.
* Phone simulator: `data_link.rs` (scripted phone serves the library, nothing before asked).
* Window: `photos_pane_tests.rs` (decoder limits including a header that claims a huge picture, clipboard commands, every state at
  three sizes, one request per click, allow is one explicit button, an allowed phone is asked once) and AccessKit tests (every
  control named, photo cells named "Photo NAME, TIME" and "preview loading", polite live region).
* i18n: 29 `photos.*` strings in the shared catalogue (`crates/bridge-i18n/i18n/en.properties`), covered by the catalogue
  checks (every key used, no literals outside the catalogue).
* Kotlin: `RecentPhotosTest` (permission mapping per Android version incl. partial access, gating, strict parsing, list shape,
  opaque ids, only listed ids, no full file without `get`, thumbnail bounds, id bounds, engine routing to one connection).
* Golden screens: `share-photos`, `share-photos-detail`, `share-photos-off` (light and dark, standard and minimum) and the four
  existing `share` goldens, which now show the tab strip because the scripted phone declares photos. Reviewed visually.

## Results

Final run on a quiet machine, from a clean worktree of `origin/main` plus this change:

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace --no-fail-fast` | 1860 passed; one pinned count (`Response` variants 17 to 19 in `ipc.rs`) failed first, was updated, and `bridge-core` was rerun green (283 passed) |
| `./gradlew --offline :app:testDebugUnitTest :app:lintDebug` | exit 0 (566 unit tests, 0 failures) |
| `scripts/golden-desktop.sh --check` (all screens) | 96 screens, 0 failing |
| `bridge-fuzz` deterministic run (includes `photos_wire`) | passed (part of the workspace test run) |

CI on the pushed commits (`check.yml`, run 41675110777 for tip `41cc886f7`, which contains this change): Android unit tests and lint,
Linux Rust tests, macOS Rust tests, the Windows and macOS type-check (clippy denied), cargo-deny, the performance budget and coverage
all green; the Windows job passed formatting, clippy, build, tests and both smoke tests, and was cancelled only at its last step
("Build desktop executables") because a newer push superseded the run. Three things surfaced on the way and were fixed in follow-up
commits: the `geekbridge-ctl` size budget (1.318 MB against 1.3 MB; raised to 1.35 MB in `scripts/perf-check.sh` and
`docs/performance.md`, the new request and response types and the `photos` command being the cause), and two failures that were not
from this change but blocked the Windows jobs (`chunks_exact(2)` in `clipboard_win.rs`, a `persist` test helper that only a unix test
uses, and the soak smaps test that reads `/proc`, now Linux-only).

The golden script's default work folder (`~/.cache/gb-golden`) is shared by anyone running it on this machine and is wiped at start;
the run above used a private `RUN` folder. A concurrent run by someone else produces spurious "service not started" screens.

## Not done, and honest limits

* **Not run on a real phone.** The media-store reader, `loadThumbnail`, the Android 14 permission dialog and the hand-off to the
  share coordinator are checked by unit tests of their rules and by review only. First real-device step: switch the feature on,
  grant *Select photos*, open the tab, Save and Copy one picture.
* If the phone is already sending another share when a Save arrives, the share coordinator declines to start a second session and
  the computer's request lapses after two minutes (shown as "the phone did not send it in time"). A queue would fix it.
* Copy to clipboard on Windows and macOS is built but never run. HEIC and other types cannot be copied (Save works).
* Only images from the media store index, 24 at a time, no albums, no videos, no search.
* MMS attachments (gap 5) are not built.
* Store distribution of the photo permission needs the store's photo-access declaration.

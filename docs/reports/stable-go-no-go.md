# v0.1.0 go / no-go audit

Audit date: 2026-11-06, third refresh against `origin/main` 70848f1b2 (last code head; 803dda57d on top of it changed only the morning checklist header). Earlier the same day: 7cbe7301c and 0af48d0ae; before that 2026-11-05 at 84d361ecf / code head 3af033de4.
Method: `gh run list` / `gh run view` / `gh pr list` against `fspecii/geekbridge`, the CI log of the `perf` step, `scripts/perf-check.sh`,
`scripts/check-doc-links.sh`, `scripts/gen-whats-new.sh --check`, and reading the reports named below. Nothing was tagged, released,
published or merged, and no Dependabot PR was touched. Anything not checkable from here is marked **UNVERIFIED**. Every row says where
it was verified: **hardware** (a real phone or PC), **emulator/sandbox** (Android emulator, stand-in phone, private HOME and ports),
**CI runners** (GitHub's Windows, macOS and Linux machines) or **reading** (code and report review only).

## Verdict: NO-GO today. After the owner items it is a GO with known, written-down gaps.

Not releasable right now. No agent-side blocker is known; what is left is owner-only, plus three open engineering items (below):

1. **The Windows/macOS red is fixed.** The `private_file.rs` test compile error (from 8560244b2) was fixed in c054ea31c. On the
   current head 7cbe7301c `Cross-platform Rust checks` (run 42165093297) and `Docs links` (42165093288) are green, and
   `Dev build (rolling)` (42165093196) built all four desktop targets and the debug APK. The newest code head 70848f1b2 is green too:
   `Cross-platform Rust checks` (42217241705, all eight jobs including Windows, macOS and the performance budget), `Docs links` (42217243416) and
   `Dev build (rolling)` (42217241685). Heads 9cbab9f9b and 225e15415 had their cross-platform runs cancelled by the next push.
2. **Every owner-only item is still open** (release key, real-device session, security contact, GitHub secrets, decisions). The
   code was never run on a real call, a real photo, a real notification, real Bluetooth audio, Windows or macOS hardware, or the
   real GitHub release endpoint. `Emulator end-to-end` is green on all 11 jobs on 84392e594 (scheduled run 42207429247); no run exists on the
   four Android-touching commits after it (Quick Settings tile, setup Back, Direct Share, call state on allow), so the current head is not covered.

The code side is otherwise the strongest it has been. Since the last audit: self-update on Linux
([design](../design/desktop-self-update.md), [security-review-9](security-review-9.md), nine defects fixed), the Android update chip,
crash and power-loss safety ([crash-safety](crash-safety.md), 1352 checks, six defects fixed), an accessibility audit, a tested upgrade
path from rc.1 ([upgrade-path](upgrade-path.md), 182 checks, no migration bug), [soak-4](soak-4.md) (67 min, PASS, no leak trend), message
store memory halved (193 B per message), a rehearsed release dry run, Emulator end-to-end green on all 11 jobs, a platform-support table
and README screenshots. None of that changes what is verified on hardware: nothing new was.

## Evidence since the last refresh (7cbe7301c to 70848f1b2)

Checked against `git log 7cbe7301c..70848f1b2`, `gh run`, `scripts/perf-check.sh` output in the CI log and the reports named. None of it was run
on a real phone or PC.

* **Idle memory** (b512861c1, 9cbab9f9b, 84392e594; [performance](../performance.md)): the service's `drop_idle` skipped a binary with dirty page cache,
  and CI links the binary seconds before the run, so nothing was dropped there. Fixed, plus read-only file mappings, a 4 s beat and `malloc_trim`.
  CI-measured settled idle RSS **8 036 kB to 2 624 kB** (84392e594) and **2 784 kB** in the perf step of run 42217241705 (70848f1b2; budget 8 500, peak sample 3 104 kB).
  9cbab9f9b then fixed a real **data-loss race** in that code: a library unloaded between reading the memory map and `madvise` could have had its reused
  address range zeroed; `drop_idle` now advises only inside the loader's read-only segments, under `dl_iterate_phdr`'s lock, and a new dlopen/dlclose test
  fails on the old code. Cost of a drop: about 60 minor faults, 0.2-0.3 ms on the next request. Sandbox and CI only.
* **Size**: `bridge-core` and cold dependencies at `opt-level = "z"` (8a0702ddf, 3d688b77c), base64 0.23 everywhere (9062cb270), per-subcommand `--help` for
  every `geekbridge-ctl` command (504db34d3). Budgets now **service 7 560 000 B and ctl 1 745 000 B** (67962490f, 3d688b77c). CI measured on 70848f1b2:
  service 7 531 216 B (28.8 kB headroom), ctl 1 721 544 B (23.5 kB). The row 9 figures below are updated; the older 7 580 000 B text is superseded.
* **Security review 11** ([security-review-11](security-review-11.md), b7c45bc76): updater proxy and connect race, BlueZ guard, redial wake-ups, history queue,
  Android sort, uninstaller. Nothing Medium or higher; two Low defects fixed (a forgotten phone stayed in `share.json.bak`; every unauthenticated discovery
  announcement woke the redial pass). Residuals listed in the report (a proxy setting the updater cannot use means a direct connection;
  `write_atomic_plain` follows a planted temp link).
* **Dependabot triage 4** ([dependabot-triage-4](dependabot-triage-4.md), 94f306568): advice unchanged: merge #1 (actions group, 5 workflow files, CI green), close #2, #4,
  #5, #6, #8, #9, defer #7. zbus 6 does not compile (25 blocking sites), zbus_xml 6 alone costs +16.9 kB of service, eframe 0.37 breaks `bridge-apps`. Nothing
  was merged, closed or commented on; **the owner's go is still required**. Its quoted budgets (7 560 000 / 1 790 000) predate the ctl change above.
* **Test and platform fixes**: Windows parity (6e75883f8: unix-only mirror test, refused connect reads as no answer, wider updater timing margins); the Wi-Fi call
  echo-guard test no longer races the wall clock (0be79a629) and covers the playout-to-microphone path in two small functions (24809bc50); the Windows accessibility
  focus-ring test no longer races the window's workers, and the update status worker asks at once (cd69b9b0e).
* **Android** (emulator only): `Computer link` Quick Settings tile and a `Tap to reconnect` notice when Android refuses a background restart (a64530973);
  one `Send to <computer>` Share-sheet row per linked computer (225e15415); setup Back from a step opened from Home, a widget or an overview row returns there (3e980419b);
  a battery level pushed before the core knew the permit (f64dac41c); a computer allowed on the phone while a call is up is told the call state at once, and
  `call-ui` checks the phone accepts the computer before it rings (70848f1b2). The 28 Android goldens that changed on purpose were regenerated (2aac58d27), desktop goldens
  and README screenshots refreshed (4f44eeab4, 4bc29e24f).
* **E2E flake causes** ([e2e-status](e2e-status.md), run history for 2026-11-06): adb offline after boot (`adb_install_retry`), photos `dev_listed` and `stays_empty` exit-status bugs
  in the scripts, the battery push race (a product bug), the first-run wizard covering the screen after a force-stop, link drops. Runs: 42174932893 (749747ffd) green,
  42180468999, 42194747065 and 42198230749 failed (causes above), **42207429247 (84392e594) green on all 11 jobs**. One green run after a string of reds is evidence, not proof.
* **Docs audit and first-run walkthroughs** (2eaadbc47, [android-first-run-walkthrough](android-first-run-walkthrough.md), 3e980419b): install and update wording matches what ships; the
  Android walkthrough on a fresh emulator found the setup Back problem above. Morning checklist round 13 is built at 70848f1b2 (versionCode 20464, same signer, sandbox smoke test).
* **BlueZ guard, stated honestly**: an earlier sandboxed test changed the **real adapter's DiscoverableTimeout to 180 s** (the guard in 4dbcc7829 now stops that, `call setup` never
  touches the system adapter from a sandbox or test). The checklist step `bluetoothctl show` makes the owner check and reset it; I have not shown that the adapter was restored.

### Earlier in this refresh cycle (7cbe7301c and before)

Changes on `main` since 0af48d0ae (from `git log`; none of them was run on a real phone or PC):

* **Do Not Disturb deadlock** (3ac5f4960): saving the privacy switch "Respect Do Not Disturb" between the keep-warm thread's look and its
  wait locked the counter twice and hung that thread and every later privacy save. Guard released before the count is read again; the test
  saves from inside the look. Found while doing the idle-wakeup work below.
* **Idle wakeups** (77f4ad8bd, 26dee3203, [performance](../performance.md) "Idle wakeups round"): whole process 2.9/s down to 0.22/s with no phone
  and 2.6-3.1/s down to 0.35-0.40/s with a stand-in phone paired (measured in the sandbox, table in the doc); `scripts/perf-check.sh` ceiling
  4-5/s down to 1/s. The CI budget stays 6/s, the local one 3/s.
* **Messages**: a8d98b443 fixed the phone reading SMS/MMS oldest first (two sort columns shared one direction: lists showed the oldest text of each
  thread and history paging returned nothing new) and the desktop's history requests are capped at 16 out (the phone drops beyond 64 queued; a
  burst of 100 lost 17 threads). 39bcb6126 tests that a refresh per thread brings all 2,000 texts of 100 threads x 20. Messages now load
  fully in these tests and in the emulator measurement ([android-perf](../android-perf.md)); not seen on a real phone.
* **Updater** ([updater-real-network](updater-real-network.md), 149b92589, f439a267e, d8010715d): an ignored real-GitHub smoke test; a wrong
  clock makes TLS errors say so (clock-skew message); happy-eyeballs connect (interleaved families, 250 ms stagger); `HTTPS_PROXY`/`ALL_PROXY` through
  CONNECT with `NO_PROXY` (host:port, wildcard, IPv6 entries); Android no longer shows "null" for empty JSON fields in release notes. TLS and the allow-list
  are unchanged. Windows test timing fixes in 68142649f, aa608c384, a481478cb. No stable release exists, so the update check against a real
  release is still unverified.
* **First-run walkthrough 2** ([first-run-walkthrough-2](first-run-walkthrough-2.md), c44ba02ae): `install.sh --uninstall` and `--purge-data`
  leftovers fixed, call setup restores Bluetooth settings when it cannot go visible, unpair refreshes the consent backup, first-run hints
  say to start the service.
* **Call setup BlueZ guard** (4dbcc7829): `call setup` never touches the system-bus Bluetooth adapter from a sandbox or test
  (`GEEKBRIDGE_NO_SYSTEM_BLUETOOTH`, a private state dir or control port) and a drop guard restores the adapter's modes on every exit; fake-BlueZ tests.
  An earlier sandbox test had changed the real adapter's DiscoverableTimeout (to 180 s); the checklist now has a step to check it.
* **Service size budget** (7cbe7301c, since lowered again to 7 560 000 B, see above): 7 560 000 to 7 580 000 B. CI measured 7 567 184 B (12.8 kB headroom) against 7 534 536 B at 149b92589: the
  update client's connect race and proxy tunnel plus the BlueZ guard, no new dependency. Android (95e095a33): APK 7.13 / 3.56 MB, dex 0.59 MB inside the budgets.
* **Dev release not published**: the rolling dev prerelease job is skipped on 7cbe7301c. Publishing needs the `DEV_KEYSTORE_B64` secret (and `DEV_SIGNER_SHA256`) and
  `DEV_AUTOPUBLISH`, none set: an owner step, now explained in [release-owner-steps](../release-owner-steps.md) (O3).
* **Emulator end-to-end flakes**: runs 42155454627 (149b92589) and 42159647633 (68142649f) failed only in `features`, `photos` and `share` (the
  first run) and `share` and `features` (the second). 9b0f5d9ea changes the scripts: data that never arrives on an up link gets one afresh start
  of the phone app, a lost Cancel tap is repeated, the grant-revoke window widens once. Whether that fixed them: partly; see the e2e bullet above (7cbe7301c and 749747ffd green, later reds had other causes, 84392e594 green). `gh issue list` shows no GitHub issues, open or closed. [e2e-status](e2e-status.md) now carries the run history.

* **Latest CI** (`gh run list`): head 70848f1b2 green (`Cross-platform Rust checks` 42217241705, `Docs links`, `Dev build (rolling)`); 803dda57d `Docs links` green. Earlier head 7cbe7301c: `Cross-platform Rust checks`, `Docs links` and `Dev build (rolling)` green (publish step skipped).
  Earlier: green on 7b35acff9, cc06731fa, 33363446a and d38ab3092; red on ee0b38b4c until c054ea31c; some runs on intermediate heads were cancelled by the next push.
* **Emulator end-to-end**: run 42133897536 on 7b35acff9 is green on all 11 jobs (build, `e2e-android`, `features`, `call-ui`,
  `battery-optimisation`, `update-install`, `pair-background`, `photos`, `share`, `sms`, `contacts-calendar-cases`, `call-audio-wifi`), after
  the `DebugNetHold` link-drop fix ([e2e-link-drops](e2e-link-drops.md)). The run before it (667f0a5aba) failed. It has not run on a head
  after 7b35acff9 (later commits changed service code: crash safety, contacts retry), so it is one head behind the code.
* **Service size** (superseded by the 7 580 000 B budget above; the numbers below are from run 42141263495 on d38ab3092): `geekbridge-service` **7 531 808 B against the 7 560 000 B
  budget (28 kB headroom then)**, `geekbridge-ctl` 1 768 104 B against 1 790 000 B (22 kB). The previous audit quoted 7 500 000 / 1 650 000;
  those were raised in `scripts/perf-check.sh` for the Messages data work and the self-update. The same step: idle RSS 8 268 kB
  (CI budget 8 500, strict 6 000 measured locally), 7 threads (10), 21 fds (24), 1 wakeup/s (6), startup 13 ms. Headroom is thin: the next feature will
  need either a trim or another raise, and every dependency bump is a risk.
* **Memory**: message store 193.5 B per stored message (was 406.7), 20 000 messages (the per-phone cap) 3.9 MB
  ([performance](../performance.md), asserted at 220 B by `message_memory.rs`). Soak-4: RSS 7.4 / 9.4 / 10.9 MB, anonymous memory flat at
  3.0-3.3 MB after minute 25 (soak-3 crept to 6.4 MB), 176 rings, 0 stuck, 0 stalls. Stand-in phone, private ports, 67 min; no 4 h run on
  this head.

## Dry-run rehearsal (2026-11-06, emulator/sandbox, throwaway key)

Full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` (with fmt, clippy and tests), run in a
scratch `git clone` of `origin/main` 19881b568 with a THROWAWAY keystore and pin created by `scripts/make-keystore.sh` in a
scratch `HOME` (no real key was read or written; nothing was tagged, pushed or published; `gh` was the printing stub).

Passed: keystore creation and the printed pin; the pin check; fmt, clippy and the whole workspace test suite; the Linux build and
tarball; installer tarball and `.deb` (version, nine binaries); the Arch package; the CycloneDX SBOM (1.5, version, listed in
`SHA256SUMS`); both Android APKs (zipalign, v1+v2+v3 signing, signer equals the pin, `versionCode` 10099 / `versionName` 0.1.0,
minSdk 26); `SHA256SUMS` matches; release notes generated with the signer line; the tag and publish commands printed only.
The real desktop updater (`crates/bridge-update/tests/release_assets.rs`, new) accepted the produced assets from a mock release:
every `SHA256SUMS` line equals the file digest, the Linux archive is chosen over the installer tarball, `.deb`, Arch package, APKs
and SBOM, the API `digest` cross-check agrees, the download re-hashes, rc.1 is offered the release and 0.1.0 is not offered itself.
The Android updater compares the downloaded APK's signer with the installed app's: both produced APKs carry the throwaway signer
that equals the pin, so a phone with this key installed accepts them (the `SignerCheck` code itself was not run: no emulator here).

Found and fixed (same day):

* `check-release-isolation.sh` was not called by anything. `release-local.sh` now runs it on both signed APKs (and needs `unzip`, `strings`).
* A release-notes file still holding the `[OWNER: ...]` placeholder was published verbatim. `release-local.sh` now refuses (a warning in a dry run).
* A long `GB_RELEASE_DIR` makes the D-Bus based tests fail ("unexpected dbus-daemon address line", unix socket path limit); the script now warns.
* `release-owner-steps.md` named line 141 for the notes placeholder; it is line 147.
* On 19881b568 the `i18n_catalogue` test failed (a literal in `tray/model.rs`); already fixed upstream in 7b35acff9.

Observed (later addressed in d38ab3092, tests now run one service at a time): `calls_api::ctl_calls_trace_shows_the_states_without_numbers_prints_json_and_clears` failed once in the
full-workspace run (the trace still held its events after `--clear`) and passed 5 of 5 alone and on the rerun: a flake under load, cause not found.
Manual steps that remain: the owner's real key, the `[OWNER]` notes placeholder, the date under `## [0.1.0]`, and a fresh clone needs `.tools`
(`scripts/bootstrap-android.sh`) before any Android step. A dry run in a dirty tree reports it as a warning, as designed.


## What still blocks v0.1.0

### Needs the owner

Copy-paste steps for every item, with what to send back: [release-owner-steps](../release-owner-steps.md). Estimated effort is the
owner's hands-on time.

| # | Item | State (re-checked 2026-11-06) | Effort |
|---|---|---|---|
| O1 | **Release signing key and signer pin** (`scripts/make-keystore.sh`; two offline backups) | `~/secrets` and `~/.config/geekbridge/android-signer-sha256` do not exist; `release-local.sh v0.1.0` refuses without them. The rehearsal used a throwaway key only. | 20 min |
| O2 | **`SECURITY.md` contact** and the `[OWNER: ...]` line in [release-notes-v0.1.0](../release-notes-v0.1.0.md) | Still placeholders (`SECURITY.md` line 8 comment; notes line 147). `release-local.sh` now refuses to publish notes that still hold the placeholder. Private vulnerability reporting is on, so a route exists. | 10 min |
| O3 | **GitHub environment, secrets, variables** | `dev-signing` exists (2026-11-05) with no branch policy; 0 repository variables, 0 secrets. `DEV_SIGNER_SHA256` and the keystore secrets are missing, so CI cannot sign a dev APK and the rolling dev release stays skipped. Not needed for a local `release-local.sh` release; needed for the dev channel (`DEV_KEYSTORE_B64`, `DEV_SIGNER_SHA256`, then `DEV_AUTOPUBLISH`; as of 7cbe7301c the dev prerelease is not published). Decide whether branch protection should require the Rust jobs. | 20 min |
| O4 | **Dependabot go** | 8 open PRs (#1, #2, #4-#9), 0 security alerts. [dependabot-triage-4](dependabot-triage-4.md): merge #1 after a green run, close #2, #4, #5, #6, #8, #9, defer #7. Not a release blocker; no PR was touched. | 5 min after the go |
| O5 | **Real-device session** ([morning-test-checklist](morning-test-checklist.md), about 90 min, Fold5 SM-F946B, an iPhone to call it, a Bluetooth controller in the PC) | Not run. The checklist build is round 13 at 70848f1b2 (versionCode 20464, same signer); rebuild if more code lands first. This is the only way to learn whether calls, the in-call card, Bluetooth audio, photos, notifications and the update chip work on a real phone. Order for call audio: [call-audio-decision](call-audio-decision.md). | 2 h incl. setup |
| O6 | **Risk acceptance and stance decisions** ([release-owner-steps](../release-owner-steps.md) section 6) | Open: the residual risks of security reviews 5-9 (including that desktop archives are verified by SHA-256 only, no signature); **unsigned Windows and macOS, no prebuilt binaries, CI-tested only**; SBOM as an asset; Android size budgets (the release APK is within the raised budgets by 0.15 MB). | 30 min |
| O7 | **Name, trademark, support channel** | Unticked in [release-checklist](../release-checklist.md). | 30 min plus any trademark search |
| O8 | **Release date**, then tag and publish | CHANGELOG `## [0.1.0]` still says 2026-11-03; set when tagging. `release-local.sh v0.1.0 --tag --push-tag --publish --notes-file ...` is the owner's command. Then check the update from an rc.1 install against the real GitHub. | 30 min |

### Agent can do

1. ~~Fix the red Windows/macOS compile~~ done in c054ea31c; green on 7cbe7301c and 70848f1b2.
2. **Settled head**: run `Emulator end-to-end` on the current head (the last run, 42207429247, was on 84392e594, before four Android commits). The morning-checklist build already matches 70848f1b2.
3. A full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` once the real key exists (the 2026-11-06
   rehearsal used a throwaway key at 19881b568).
4. Service size: 28.8 kB of headroom (budget 7 560 000 B), ctl 23.5 kB (1 745 000 B). Decide before the next feature whether to trim (dropping `x86_64` from the universal APK is the cheap Android trim) or raise again.
4a. **Open engineering items** (not owner-only): (i) **soak-5 verdict pending**: there is no soak-5 report in the tree, and the idle-memory and `drop_idle` changes are newer than soak-4, so the soak
   evidence in row 8 predates them; (ii) **dex budget CI check pending**: no workflow step checks the dex size (only `scripts/android-perf.sh` reports it; the last figure, 0.59 MB against 0.6, is from 95e095a33);
   (iii) **Wi-Fi call audio is unproven on a real phone** ([report](call-audio-wifi-realphone.md)); it stays experimental, debug builds only.
5. Optional: a 4 h soak on the head (soak-4 was 67 min); real-service self-update test in a sandbox prefix against a mock release
   (unit and kill tests exist; no run against the real release endpoint is possible before a stable exists).
6. Small review items still open: "this replaces your link" line on the QR confirm page (S7-14), call card answers on click only,
   desktop first-run firewall card placement and a "Check for problems" button ([first-run-review](first-run-review.md) items 4-6), the
   [release-scripts-review](release-scripts-review.md) maintainer list, Android learning of an update only from Home or Diagnostics when
   no check ran (A1 is partly met by the update chip, which shows on Home after a check, not push).

## Changes made in this refresh

* A00. Third refresh (70848f1b2): verdict, evidence section, rows 1-3, 8, 9, 17, O4, O5 and the agent list updated; the open engineering items added; CHANGELOG and release notes gained the user-facing entries (see below).
* A0. Second refresh (7cbe7301c): verdict, evidence, rows 1, 2, 3, 9, 10, 11, 16 and 17 and the agent list updated; the morning checklist header says
  "rebuild needed" and gains the `bluetoothctl show` step; CHANGELOG and release notes gained the entries named under "Evidence".
* A. This file re-verified: CI (`gh`), CI-measured service and ctl size, the reports named above, the GitHub environment and PR list.
* B. `CHANGELOG.md`: the self-update entry moved from `[Unreleased]` into `[0.1.0]` ("Since the first release candidate", `[Unreleased]` is an
  empty heading again so `scripts/release.sh` works) together with the Android update chip, crash and power-loss safety, the accessibility
  fixes, and the upgrade-path check (stated as a test result, not a feature). Self-update is described as Linux per-user only and
  not signed. The older "never installs by itself" line is corrected.
* C. `docs/release-notes-v0.1.0.md`: the Updater highlight no longer says the desktop user unpacks and copies files on every desktop; new
  highlights for Install update (Linux per-user), the update chip, crash safety and accessibility; limitations name what is unverified
  (no TalkBack, Orca, NVDA session, no real power cut, no real update from GitHub).
* D. `gen-whats-new.sh --check` and `check-doc-links.sh` pass.

## Table

GREEN = evidence seen, AMBER = acceptable with a stated gap, RED = blocks, OPEN-OWNER = only the owner can do it.
"Verified on" says whether the evidence is hardware, emulator/sandbox, CI runners or reading only.

| # | Item | Status | Verified on | Evidence |
|---|---|---|---|---|
| 1 | CI green on a settled head (Linux) | AMBER | CI runners | 70848f1b2: `Cross-platform Rust checks` (Linux, Windows, macOS, perf budget, supply chain, Android unit tests; run 42217241705), `Docs links` and `Dev build (rolling)` green; 7cbe7301c green too. Heads 9cbab9f9b and 225e15415 were cancelled by the next push. |
| 2 | CI green on Windows and macOS | AMBER | CI runners | Red on ee0b38b4c and 0af48d0ae (`replace` not in scope in a test of `private_file.rs`); fixed in c054ea31c. Green on 7cbe7301c (run 42165093297) and 70848f1b2 (42217241705), including the Windows updater test timing fixes (68142649f, aa608c384, a481478cb). Nobody has run either OS on hardware. |
| 3 | Emulator end-to-end | AMBER (not a release gate) | emulator | Run 42133897536 on 7b35acff9: all 11 jobs green (including `call-ui`, `update-install`, `photos`, `sms`, `contacts-calendar-cases`, `call-audio-wifi`) after the DebugNetHold link-drop fix. Two heads earlier it was red. Latest run 42207429247 on 84392e594: all 11 jobs green. Not run on the current head (four Android commits since). The reds of 2026-11-06 had causes found (adb offline after boot, script exit-status bugs, the battery push race, wizard after force-stop, link drops: [e2e-status](e2e-status.md)); one green run since is not proof the flakes are gone. |
| 4 | Security reviews | AMBER | reading and sandbox tests | Reviews 1, 2, 5-9, blackbox and followups-security-5 written with fixes and tests. New: [security-review-9](security-review-9.md) (self-update: archive path, symlinks, bombs, `current` swap, restart rules): nine weaknesses fixed with tests, two accepted (a staged binary is run once for `--version` after the hash matched; a user who hand-mixes versions gets a mislabelled "previous"). The design states that the desktop archive has no signature: someone who can edit the GitHub release can ship code (non-goal, owner risk O6). Earlier residuals unchanged (reviews 5/6, 7, 8). Wi-Fi call audio is debug-only and excluded from release artifacts (`check-release-isolation.sh`). |
| 5 | Code review | GREEN / AMBER | reading | code-review-4, call-chain, call-features, android-release-audit and the new crash-safety work (fix list D1-D6 with tests). No review covers the range after 3af033de4 end to end. |
| 6 | Crash and power-loss safety | GREEN / AMBER | sandbox | [crash-safety](crash-safety.md): 1352 checks, 0 failed over 20 kill rounds of each scenario (state writes, failpoint between temp and rename, 64 MiB transfer, message sync, installer, ring, disk full, read-only folder, clock steps); six defects fixed with tests. **No real power cut** (page cache not droppable here); the guarantee rests on the write ordering. A full save folder is only covered by older tests. Not run on the Android side. |
| 7 | Upgrade path | GREEN / AMBER | emulator and sandbox | [upgrade-path](upgrade-path.md): state written by the real v0.1.0-rc.1 and a round 9 build opened by the current service and the current phone app: pairings, consents, privacy, update state, identity kept, byte-identical files, rollback to the old build works, 17 damaged/newer file variants, 182 checks, no migration bug; runs in CI (13 s). Not covered: a real phone and package installer (Samsung screens, Play Protect). rc.1 on Android still cannot update in place (debug-signed): uninstall first, as the notes say. |
| 8 | Soak | AMBER | sandbox (stand-in phone) | **soak-5 verdict pending (no report yet).** [soak-4](soak-4.md), older than the idle-memory changes: 67 min, 176 rings, 0 stuck, 0 stalls over 1 s, RSS flat, anonymous memory flat at 3 MB, 0 panics: PASS, no service code changed. Two hard caps in the verdict script were corrected (state size, banner count), which is why the first verdict was red. Earlier 4 h soak failed on control-API stalls, fixed and re-run at 2 h. No 4 h run on the head; no real phone. |
| 9 | Size and perf budgets | AMBER | CI runners (Linux) | See "Evidence since the last refresh": CI on 70848f1b2: service 7 531 216 B / 7 560 000 B (28.8 kB headroom), ctl 1 721 544 B / 1 745 000 B (23.5 kB), idle RSS 2 784 kB (was 8 268), strict local budgets and CI slack as listed in `scripts/perf-check.sh` (RSS CI 8 500; 8 threads, CI 10; 20 fds, CI 24; 3 wakeups/s, CI 6; measured 0.2-0.4/s after 77f4ad8bd, was 2.9; startup 300 ms, CI 1 000). [performance](../performance.md): 200 call cycles with no growth, 193 B per message, 2 000 contacts and 2 600 texts hold 11-13.7 MB. Android release APK (android-release-audit): universal 7.10 MB (raised budget 7.25), arm64 3.55 MB (3.6), dex 0.58 MB (0.6): budgets were raised, not met. Android sizes and cold start not re-measured on the head (emulator); no CI step checks the dex budget yet. |
| 10 | Updater and self-update | GREEN / AMBER | reading, sandbox, emulator | Check, verify, download: as before ([updater-e2e-3](updater-e2e-3.md), [updater-review](updater-review.md); `update-install` emulator job green). New: Linux per-user **Install update** and `geekbridge-ctl update install|rollback|restart` with staging, strict tar/gzip readers, atomic `current` swap, version check within 20 s and automatic way back, no restart during a call; the kill-at-random-moment test passes. Windows, macOS, package installs and the dev channel stay download only. The release asset names and checksums were accepted by the real updater code from a mock release ([rehearsal](#dry-run-rehearsal-2026-11-06-emulatorsandbox-throwaway-key)). **UNVERIFIED**: anything against real GitHub (no stable exists) and the in-place upgrade from rc.1; the Android update chip is unit-tested and built, and has not been seen on a phone. |
| 11 | Dev channel | AMBER | CI runners | `Dev build (rolling)` green on 7cbe7301c (four desktop targets plus the debug APK), publishing skipped: `dev-signing` has no policy and no secrets, `DEV_KEYSTORE_B64` and `DEV_AUTOPUBLISH` are unset (O3, owner step); no dev release exists. |
| 12 | Packaging / fresh install | GREEN | sandbox | [fresh-install-audit](fresh-install-audit.md), [install-upgrade-sandbox](install-upgrade-sandbox.md), the rehearsal's tarball, `.deb` and Arch package checks. |
| 13 | Cross-platform parity | AMBER | CI runners, reading | [cross-platform-parity-3](cross-platform-parity-3.md) and the README platform-support table (checked against the Android and desktop sources in 1ae3b5029). No Windows or macOS hardware, and row 2 is green on CI only. |
| 14 | Accessibility | AMBER | headless tests, reading | [accessibility-audit](accessibility-audit.md): contrast computed from theme tokens, every tab stop has a focus ring and a name in 12 call-window and card states, 12 pt floor now scans all sources; six defects fixed (card re-announced every second, no focus ring or 11.5 pt on "Decline with message", `*`/`#` names, Android disclosure state, TalkBack focus lost on rebuild). **No TalkBack, Orca or NVDA session was run** and the emulator was not used for this audit. |
| 15 | Call audio decision | AMBER / OPEN-OWNER | reading | [call-audio-decision](call-audio-decision.md): on a stock Fold5 there is no app-level way to carry call audio to the PC without Bluetooth; Bluetooth hands-free first, one ten-second Wi-Fi probe in the same session. Nothing here was run against a real phone or Bluetooth. |
| 16 | First-run experience | AMBER | emulator, reading | [first-run-review](first-run-review.md): the three top items (one-page "Answer calls", scan as the only visible way to link, Back cancels an attempt) are done. Items 4-6 (firewall card placement, "Check for problems" button, repeated "No computer linked yet") remain. Android goldens `setup-pair*` and `setup-answer*` were stale when written (emulator would not stay online on that host). |
| 17 | Real-device morning checklist | OPEN-OWNER | **hardware: none** | [morning-test-checklist](morning-test-checklist.md) round 13 built at 70848f1b2 (versionCode 20464, sandbox smoke test); includes the `bluetoothctl show` step (the real adapter's DiscoverableTimeout was once changed to 180 s by a sandbox test). Not recorded as run. One phone has ever been paired (SM-F946B, pairing and link only). Everything else rated by emulator or stand-in phone: calls and the in-call card, photos, notifications, privacy switches, QR scan with the in-app camera, goodbye, Bluetooth and Wi-Fi call audio, the update chip. |
| 18 | Documentation | GREEN | reading | `check-doc-links.sh` passes on this tree; release notes carry one `[OWNER]` placeholder; README screenshots (light-theme images from the goldens), platform table, FAQ and release notes agree on what is unverified; `docs/privacy.md` lists every release permission (guarded by a test). |
| 19 | CHANGELOG and release notes match | GREEN (this refresh) | reading | See change B and C above; `gen-whats-new.sh --check` passes. Date still 2026-11-03 (O8). |
| 20 | Licence, notices | GREEN | CI runners | GPL-3.0-or-later; supply-chain job green on d38ab3092 and ee0b38b4c. |
| 21 | SBOM | GREEN | sandbox | CycloneDX 1.5 produced and listed in `SHA256SUMS` by the rehearsal (owner decides whether it ships). |
| 22 | Release script plumbing | GREEN | sandbox | Full dry-run rehearsal with a throwaway key passed and found five script problems, fixed the same day (see above). The flake in `calls_api` noted in the rehearsal was addressed in d38ab3092 (tests run one service at a time); not re-observed. Needs a repeat with the real key. |
| 23 | Repository visibility, private reporting | GREEN | gh | Public; private vulnerability reporting enabled; 0 Dependabot alerts. `SECURITY.md` contact still a placeholder (O2). |
| 24 | Windows / macOS stance, name, support channel | OPEN-OWNER | n/a | Unticked in [release-checklist](../release-checklist.md) (O6, O7). |

## Minimum path to GO, in order

Owner effort in brackets (hands-on time).

1. **Agent, first, about one CI cycle:** (Windows/macOS compile fixed in c054ea31c) stop pushing, wait for
   `Cross-platform Rust checks` green on all three OSes plus `Emulator end-to-end` on that same head (70848f1b2 has the first; the second is missing); the morning-checklist build (round 13) is already at 70848f1b2.
2. **Owner (20 min):** create the key and pin with `scripts/make-keystore.sh`, back it up twice offline.
3. **Owner (10 min):** fill the `SECURITY.md` contact and the release-notes placeholder.
4. **Owner (about 2 h):** the real-device session from the checklist on that build: pair, one answered and one declined call with a real
   caller, a photo, a notification, Bluetooth hands-free (the update chip shows only when a newer stable exists, so it cannot be seen before publishing). Record the outcome as a report. Anything that fails is
   either fixed or moved into the notes' limitations; the wording already says "unverified on real hardware" for the rest.
5. **Owner (30 min):** decide the risks and stance (O6, O7): accept the residuals, confirm unsigned Windows/macOS "build from source",
   name and support channel.
6. **Agent (30 min):** full `release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` with the real key; fix the date
   under `## [0.1.0]` and the notes.
7. **Owner (30 min):** `scripts/release-local.sh v0.1.0 --tag --push-tag --publish --notes-file docs/release-notes-v0.1.0.md`, then an
   update check from an rc.1 install.

Optional, not blocking: the GitHub environment (O3) and the Dependabot go (O4) can follow the release. The owner total is about
4 to 5 hours in one or two sittings; the real-device session is the only part that cannot be shortened. If the owner skips step 4, the
release is still defensible only as a "first version, verified on one phone for pairing, everything else emulator-only", which is what
the notes say today, but it should then be labelled in the release as such and the calls claims kept in "not yet verified on hardware".

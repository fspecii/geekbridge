# Ship runbook: branch `hard` (based on 6e61ae7bd)

Written while Bash was dead (EDQUOT on /tmp/claude-1000), from the reports only. Nothing here was run. The file lists in
section 2 are derived from the reports' edited-files sections and from the task brief, so confirm each one with
`git status` before you stage it. Anything git shows that is not in a group goes into "soak/lint fixes" or gets asked about.

Hard rules:
- Never stage `scripts/e2e/wayland.sh`. It is a user edit in the main checkout.
- No Claude or AI attribution in commit messages (project memory). Ignore any default trailer.
- Do not touch the live service, Hyprland, real Bluetooth, or ports 1716, 17381, 8000.
- Confirm with the owner before any force-push or visibility change.

## 1. First minutes once a shell works

```sh
cd /home/ambsd/.cache/gb-wt-hardening
echo ok; df -h /tmp /home | tail -3                      # EDQUOT check
du -sh /tmp/claude-1000 2>/dev/null                      # if full: ask the user before deleting anything
export CLAUDE_CODE_TMPDIR=$HOME/.cache/claude-tmp; mkdir -p "$CLAUDE_CODE_TMPDIR"   # for the next session
git branch --show-current                                # expect: hard
git status --short | tee "$HOME/.cache/hard-status.txt"
git status --short | grep -v '^??'                       # modified, by path
git status --short | grep '^??'                          # untracked (new files: reports, tests, modules)
git diff --stat | tail -40
git fetch origin && git log --oneline HEAD..origin/main | wc -l
git merge-base HEAD origin/main                          # expect 6e61ae7bd or later
git diff --name-only | grep -x 'scripts/e2e/wayland.sh' && echo "DO NOT STAGE wayland.sh"
```

Sort the status output by area:

```sh
git status --short | awk '{print $2}' | sed -E \
 -e 's#^crates/bridge-service/src/(call_|banner_pool|desktop_notify).*#A-calls-hub#' \
 -e 's#^crates/bridge-apps/src/(call_feed|call_overlay|prefill|call_ui|apps/calls).*#B-apps-card#' \
 -e 's#^crates/bridge-update/.*|^android/.*/Update.*|^android/.*/SignerCheck.*|^docs/updates.md#C-update#' \
 -e 's#^android/.*/(CallControl|CallState|BootReceiver|PhoneDataAndroid).*#D-android-call#' \
 -e 's#^\.github/.*|^scripts/tests/workflow.*|^docs/release-owner.*#G-workflows#' | sort | uniq -c | sort -rn | head -60
```

If `git status` is clean for areas that should have edits, the work is in another branch or worktree. Run
`git worktree list; git branch -a | head -40` before assuming anything is lost.

## 2. Commit groups (in this order, one commit each)

Use `git add -- <paths>` only. Never `git add -A`. After each group: `git diff --cached --stat`, then commit.
Commit messages are plain, no trailer.

1. **Service calls hub** (`calls: banner pool, card launch, history forgetting, trace, send_link guards`)
   - `crates/bridge-service/src/call_control.rs`
   - `crates/bridge-service/src/call_history.rs` (`forget_source`, source key in `Done`/`Open`)
   - `crates/bridge-service/src/call_trace.rs` (control-char strip, MAC strip, response size)
   - `crates/bridge-service/src/banner_pool.rs`
   - `crates/bridge-service/src/call_overlay.rs` (service Supervisor, card launch for a live call)
   - `crates/bridge-service/src/daemon.rs` (only the `Calls` in `blocking`, `set_consent` forgetting)
   - `crates/bridge-service/src/call_cli.rs` (tone pacing and retry on Busy, full number in the prompt)
   - tests under `crates/bridge-service/tests/` that touch calls
2. **Apps card** (`call card: survive a service blip, prefill env, reap launched windows`)
   - `crates/bridge-apps/src/call_feed.rs`
   - `crates/bridge-apps/src/call_overlay.rs`
   - `crates/bridge-apps/src/prefill.rs` (`env_remove` for the other prefill variables, detached reaper)
   - `crates/bridge-apps/src/call_ui.rs` and `apps/calls.rs` only if modified
3. **Update crate, Android update classes, docs** (`updater: persist the rate-limit pause, overall download deadline, attempt gate`)
   - `crates/bridge-update/src/lib.rs` (D2, D3 and the test `a_rate_limit_pause_is_persisted_and_a_wild_one_is_ignored`)
   - `android/app/src/main/java/dev/geekbridge/android/UpdateChecker.kt`, `UpdateSection.kt`, `UpdateModel.kt`
   - `android/app/src/test/java/dev/geekbridge/android/UpdateModelTest.kt`
   - `docs/updates.md`
   - `docs/reports/updater-review.md`
4. **Android call control** (`android: tone queue and limiter, boot receiver off the main thread, SMS receiver failsafe`)
   - `android/.../CallControl.kt` (RateLimited for tones)
   - `android/.../CallControlAndroid.kt` (tone queue, `Call.Callback` re-report on ACTIVE)
   - `android/.../CallState.kt`
   - `android/.../BootReceiver.kt`
   - `android/.../PhoneDataAndroid.kt` (confirmed edits: `finish()` once via `AtomicBoolean` plus 8 s failsafe, `try/catch` around `onEvents`, `@SuppressLint` moved to the class)
   - Android unit tests for the above
5. **Keypad CLI plus host deadline** (`keypad: pace and retry in the CLI, drop expired host commands`)
   - `crates/bridge-service/src/call_cli.rs` if not already in group 1 (move it here if you prefer a clean split)
   - `crates/bridge-service/src/call_control.rs` hunks for `HostCommand` deadline. If these hunks share a file with group 1, use `git add -p`, or fold groups 1 and 5 into one commit.
6. **Messages fixes** (`messages: group reply blocker, unread count, load-older, capped thread re-mark`)
   - `crates/bridge-apps/src/apps/messages.rs`
   - `crates/bridge-service/src/phone_data.rs`
   - `crates/bridge-kde/src/sms.rs`
   - `crates/bridge-apps/src/recipient_pick.rs`, `crates/bridge-core/src/messages.rs` only if modified
7. **Perf wins** (`perf: cheaper store refresh, token cache, clipboard truncate, packet line reader, fewer clones`)
   - `crates/bridge-kde/src/packet.rs` (shrink-to, 16 KiB chunk, no body clone)
   - `crates/bridge-service/src/clipboard.rs` (`truncate`, one `list-types`)
   - `crates/bridge-apps/src/connection.rs` (token cache)
   - `crates/bridge-kde/src/store.rs` (stat check before re-read)
   - `scripts/perf-check.sh` and any perf test, only if modified
8. **Wi-Fi procedure, helper, setup, playback** (`wifi audio: safer test tones, setup status without pairing, playback backlog, corrected procedure`)
   - `crates/bridge-service/src/callwifi.rs`, `callwifi_setup.rs`
   - `crates/bridge-callwifi/src/*` if modified
   - `android/.../callaudio/CallAudioHelper.kt`, `UplinkTest.kt`, `CallWifiRuntime.kt` if modified
   - `scripts/callaudio-enable.sh`
   - `docs/reports/call-audio-wifi-realphone.md` (corrected steps and the cleanup block from `wifi-realphone-prereview.md`)
9. **Workflows plus owner docs** (`ci: separate publish concurrency, require signer pin, workflow secrets test`)
   - `.github/workflows/dev-release.yml`
   - `.github/workflows/release.yml`
   - `.github/workflows/check.yml`
   - `scripts/tests/workflow-secrets-test.sh`
   - `scripts/release-dev.sh` if modified
   - `docs/release-owner-steps.md`
10. **Docs and checklist** (`docs: call features, reports, release checklist`)
    - `docs/reports/*.md` (all the review reports, including this runbook)
    - `docs/call-control.md`, `docs/release-checklist.md`, `docs/morning-test-checklist` variants
    - `CHANGELOG.md` if touched
11. **Soak and lint fixes** (`tests: soak_real clippy, onboarding autostart test, tray wording`)
    - `crates/**/tests/soak_real*.rs` and whatever the clippy run flags
    - the tray wording and onboarding test files (see section 5)
    - leftovers from `git status` that fit nowhere else

After all groups: `git status --short` must show only `scripts/e2e/wayland.sh` (if the worktree even has it) and nothing else.

## 3. Rebase onto origin: conflict hotspots

```sh
git fetch origin
git rebase origin/main        # or: git rebase --onto origin/main 6e61ae7bd hard
```

Origin has features that this branch never saw: decline-with-message (6e9c971cb range), Do Not Disturb (95e96b0d0),
dialer autocomplete (de262ec23), the call trace, keypad, Recents, and the first-run wording.

**Rule: take origin's structure, then re-apply this branch's fix on top.** In practice, per conflict:
`git checkout --theirs` is wrong during a rebase (the sides are swapped). Use `git checkout --ours -- <file>` to get origin's
version during a rebase (`ours` = the branch being rebased onto), then re-apply the hunk by hand. Do not blindly merge both.

Hotspots, in order of risk:

| File | Why | What to re-apply |
| --- | --- | --- |
| `crates/bridge-service/src/call_control.rs` | Decline-with-message, DND, trace and keypad all touched `act_inner`, `announce_new_calls`, `NotifySendCalls`, `send_link` | `if !phone.enabled { return Err(NotConnected) }` in `send_link`; `forget_source` calls in `set_enabled(false)` and `tick()` clear path; `tone_count` reset; banner name-only when `CallerNumber` is off; `declined` marked on success |
| `crates/bridge-service/src/call_history.rs` | Recents and missed banner on origin | `source` key in `Done`/`Open`, `forget_source` |
| `crates/bridge-service/src/daemon.rs` | New requests (DND, decline reply, suggestions) | `Calls` in `blocking`, consent revoke forgetting |
| `crates/bridge-apps/src/call_overlay.rs`, `call_feed.rs` | DND and decline button on the card | the `None` arm of `Card::update` returns `Keep` while a call is live (30 s grace) |
| `crates/bridge-apps/src/call_ui.rs`, `apps/calls.rs`, `prefill.rs` | Autocomplete, keypad, Recents | prefill env_remove, reaper; keep origin's suggestion list |
| `android/.../CallControl.kt`, `CallControlAndroid.kt` | Keypad on origin | tone queue, limiter, `Call.Callback` |
| `crates/bridge-service/src/phone_data.rs`, `apps/messages.rs` | Decline reply uses the Messages send path; autocomplete reads contacts | messages fixes |
| `.github/workflows/*.yml`, `docs/*.md`, `CHANGELOG.md` | Everyone edits them | keep both lists, re-run workflow-secrets-test |
| First-run wording files (tray, onboarding) | Origin renamed strings | adopt origin's wording, fix this branch's tests to match |
| golden images under `tests/golden*` | Binary | never merge; regenerate (section 6) |

During the rebase: `git status` after each stop, `cargo check --workspace --all-targets` after each hotspot, `git add` and
`git rebase --continue`. On a binary golden conflict take origin's file and regenerate later.
Before the review checklists that were never run on origin's code, do the checklist in
`call-features-review.md` ("Checklist for them") for decline-with-message, DND and autocomplete, and the checklist in
`decline-reply-review.md` (one send per ring keyed by ring number, shared SMS limiter and consent, number never on argv or in the trace).

## 4. Verify (run from the worktree, one at a time, with timeouts)

```sh
cargo fmt --all -- --check
timeout 1500 cargo clippy --workspace --all-targets -- -D warnings
timeout 2400 cargo test --workspace -- --test-threads=4
timeout 600 cargo test -p bridge-update
( cd android && timeout 1800 ./gradlew --no-daemon :app:testDebugUnitTest :app:lintDebug )
scripts/perf-check.sh
cargo build --release && scripts/check-release-isolation.sh      # needs a release build
scripts/check-doc-links.sh
scripts/tests/workflow-secrets-test.sh
```

Use `ANDROID_ADB_SERVER_PORT=5038` if anything touches adb, and do not run e2e scripts that drive Hyprland or Bluetooth.
Any test that needs the live service must be skipped or pointed at a private state dir.

## 5. Known red tests: look at these first

1. **Tray wording.** A test expects the old string; new copy is "No phone paired yet" versus the linked-state wording.
   Decide which side is right from origin's first-run wording, then change the test, not the product text.
2. **Onboarding autostart test.** Probably touches the real autostart path or the new first-run flow. Make it use a temp
   `XDG_CONFIG_HOME`; do not let it write to the real `~/.config/autostart`.
3. **`soak_real` clippy.** `-D warnings` over all targets includes the soak test crate. Fix the lints in the test code;
   do not add a blanket `#![allow]`.

Then anything new that comes from the rebase (call_control tests that touch history, trace redaction tests).

## 6. Golden refresh

```sh
ONLY='^settings-privacy$' scripts/golden-desktop.sh --update      # only the one that changed with the new wording
git diff --stat -- '*golden*'                                     # confirm only the expected images moved
scripts/golden-android.sh --update                                # only on a quiet machine, nothing else running
```

Look at each updated image (Read tool) before committing. Commit the images in their own commit
(`goldens: refresh after first-run wording`).

## 7. Push and CI watch

```sh
git fetch origin && git rebase origin/main
git push origin hard                       # new branch, no force
gh pr create --base main --head hard --title "Hardening: calls, updater, messages, perf, CI" --body-file docs/reports/ship-runbook.md   # only if the owner wants a PR; otherwise push to main as usual
gh run list --branch hard --limit 5
gh run watch "$(gh run list --branch hard --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
gh run view <id> --log-failed | tail -120
```

(No attribution trailer in the PR body either. Edit the body first if you reuse this file.)

Per red job:

| Red | Action |
| --- | --- |
| `Rust / ubuntu-latest` | Reproduce locally with the exact step. Fix, amend into the right group with `git commit --fixup`, push again. |
| `Rust / windows-latest` or `macos-latest` | Usually timing or path separators. Check the existing "macOS runner timing" fixes (keypad test) before changing product code. Rerun once; a second red is real. |
| `Performance budget` | Run `scripts/perf-check.sh` on a quiet machine; compare to the budget file. Do not raise a budget without the owner. |
| `Type-check Windows and macOS targets` | `cargo check --target x86_64-pc-windows-gnu` / `x86_64-apple-darwin` locally; usually a missing `cfg` on a Linux-only call (`readiness::detect`, `notify-send`). |
| gradle / lint | Re-run `:app:lintDebug`; fix new lint (exported component, missing permission annotations). |
| workflow-secrets-test | Fix the workflow, not the test, unless the test pins an old name. |
| Flaky | One rerun with `gh run rerun <id> --failed`. Never merge on "flaky" without a second green. |

---

# Backlog: top 3 unfixed findings per report

Reports are static reviews. "Unfixed" means not marked fixed in the report; check `git log` before starting.

**call-features-review.md**
1. HIGH-1: the in-call card closes and the timer restarts when one poll fails (`Card::update` `None` arm; return `Keep` with a 30 s grace).
2. HIGH-2: Recents outlive consent revocation and unpair (`History::forget_source`, called from `set_enabled(false)`, unpair, `set_consent`).
3. MED-3: ring and missed banners put the caller's number on the `notify-send` command line and ignore the "caller number" privacy choice.
(Next: MED-4 `Calls` blocking an async worker; MED-6 "declined" recorded before the phone confirms.)

**android-code-review.md**
1. `CallControlRuntime.ensureMonitor` and the first `sync()` run binder calls on the main thread (ANR risk).
2. `AndroidCallSources.register` can leak `defaultHandle` on a non-`RuntimeException`.
3. Waiter threads rely on `CoreClient` catching every throwable (`LinkageError`, `OutOfMemoryError`); also `BootReceiver` disk work on main.

**updater-review.md**
1. D1: no desktop self-update (nothing installs; Windows cannot overwrite the running exe). Needs design.
2. A1: Android learns of an update only when "Diagnostics & about" is opened; add a Home chip and a periodic check.
3. P2: dev manifest not bound to assets (add `apkSha256` and require equality with `SHA256SUMS`). Also T1 bundled root store, L2 fail-open `SignerCheck.identity`.

**keypad-recents-review.md**
1. Finding 1: no re-report when the call becomes ACTIVE, so `canTones` can stay false for the whole call (`Call.Callback`).
2. Finding 2: `geekbridge-ctl call tone` with more than about 20 keys fails halfway (pace 260 ms, retry on Busy).
3. Finding 3: timed-out HFP tone commands still run later (deadline on `HostCommand`). Also Recents leak after consent-off, and a stale-cleared ring recorded as Missed twice.

**messages-code-review-2.md**
1. H1: a reply in a group conversation goes privately to one participant (add a blocker when `addresses.len() > 1`).
2. H2: unread count and tray badge go stale after reading on the phone (`parse_unread` returns `Option`, `unread()` logic, observe `Telephony.Sms`).
3. H3: "Load older" can get stuck at "exhausted" from unrelated packets. Also M3 contact merging by name or 7-digit tail can send to the wrong John.

**perf-static-review.md**
1. Unchanged Messages poll does full work every 1.5 s (generation counter, `Unchanged` response, cached `name_index`).
2. Messages window deep-clones the whole view several times per frame (`Arc<FeedState>`).
3. Every app process polls status 2 to 3 times per second with disk reads each time (token cache, stat before re-read; partly in the perf group, verify). Nothing was measured; profile first.

**wifi-realphone-prereview.md**
1. Step K cannot show helper survival with Wireless debugging off; use the corrected K, and make `setup --status/--stop` say "no phone connected" instead of starting the pairing walk.
2. Test tones are loud and `CallAudioHelper.preferredTx` returns the injector even when `setPreferredDevice` fails (return null; amplitude 0.1 with a ramp).
3. No limiter or fade-in on the PC output path (`--volume 0.5`, 100 ms fade-in), and the playback pipe backlog defeats the echo guard timing.

**decline-reply-review.md**
1. The review did not cover the feature (it was not in the checked tree). Re-run it on origin's diff (6e9c971cb range) with the checklist in the report.
2. One send per ring: the double-click dedupe returns `Ok`, so a naive "decline then SMS if Ok" would send twice; key by ring number under the hub lock.
3. Report a failed text separately from a successful decline; share the SMS consent and the 10/min limiter with the normal send path; preset index resolved server-side, no free text from the wire.

**call-ux-gap-analysis.md**
1. Gap 7: hold, swap and call waiting (needs a multi-call model; the link reports one call only).
2. Gap 9: persistent phone-side call history (READ_CALL_LOG over the link, consent, size budget).
3. Gap 10: switch audio back to the phone from the card (`DisconnectAudio` exists). Also gap 12 (tray missed-call badge) and 11 (automatic Bluetooth pairing, needs real hardware).

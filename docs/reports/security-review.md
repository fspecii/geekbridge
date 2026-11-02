# Security review: updater, release pipeline, Android update/share, KDE share plugin

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Scope: `crates/bridge-update`, `.github/workflows/release.yml`, Android `UpdateChecker` / `UpdateFileProvider` /
`UpdateSection` / manifest, `ShareActivity` and friends, `crates/bridge-kde` `share.rs` + `payload.rs`.
Review only; the other agents' files were not edited. Nothing was committed.

## Fixed in this pass (bridge-update, release.yml; `cargo test -p bridge-update` passes, 29 tests)

| # | What | Where |
|---|------|-------|
| F1 | A 403 counted as a rate limit whenever `x-ratelimit-reset` was present, and GitHub sends that header on every response. A plain 403 caused a 1 min to 24 h back-off. Now only `Retry-After` or `x-ratelimit-remaining: 0` count. | `bridge-update/src/http.rs` `Head::summary` (test `plain_403_is_not_a_rate_limit_...`) |
| F2 | The GitHub allow-list accepted any port (`https://github.com:8443`). It now requires 443. | `http.rs` `Policy::allows` (extended policy test) |
| F3 | A tag such as `v1.2.3-rc.1` on a release not flagged as prerelease would have been offered. `is_prerelease()` versions are now treated as "no release". | `lib.rs` `fetch_latest` (test `prerelease_tag_is_never_offered_...`) |
| F4 | When the asset was found but `SHA256SUMS` was missing, unreadable or non-200, the answer was still stored with its ETag. Every later check got a 304, so the checksum was never retried until the release JSON changed. The ETag is no longer stored for an offer without a checksum. | `lib.rs` `fetch_latest` (test `a_release_without_checksum_is_not_pinned_...`) |
| F5 | The workflow silently published a debug-signed APK for stable tags. That APK cannot be updated over, or replaced by, the real key. Stable tags now fail without the keystore secrets, and the signer is verified (v2/v3) and pinned to `ANDROID_SIGNER_SHA256`; the app also checks the signer before Install. Prereleases still fall back. | `release.yml` Android job |
| F6 | `gh release create` published the release first and attached assets afterwards. `releases/latest` could briefly show a release with no `SHA256SUMS` or APK. It now creates a draft with `--verify-tag`, then un-drafts. A completeness check on the five artifacts runs before `SHA256SUMS` is written. | `release.yml` publish job |

## Remediation status (second pass)

| # | Status | Notes |
|---|--------|-------|
| 1 | Fixed (earlier) + regression test | `share.rs` `temp_path_survives_multibyte_at_truncation_boundary` (`"a".repeat(63)+"é"`) |
| 2 | Fixed | temp name is `.gb-<pid>-<counter>-<nanos>-<short>.part`; test `temp_paths_differ_for_names_sharing_a_long_prefix` |
| 3 | Fixed | `ShareIntentLogic.isBlockedAuthority` (own package, contacts, calendar, call_log, sms, mms) applied in `ShareActivity.parse`; the confirmation lists resolved display names (first 5). Tests `ShareAuthorityTest`. The `getLaunchedFromUid` grant check was not added (needs API 34+ handling). |
| 4 | Fixed | `accept_peer` drops connections from any IP other than the paired peer (IPv4-mapped normalised) before spawning a handshake; default handshake timeout 10 s to 4 s. Test `only_the_paired_host_gets_a_handshake_slot`. Binding to the link interface address was not done. |
| 5 | Fixed | default download root is per-user state dir (`$XDG_STATE_HOME`/`~/.local/state`, `%LOCALAPPDATA%`, macOS Application Support) `/geekbridge/updates`, created 0700; `geekbridge-update-*` dirs older than 1 h swept in `Updater::new`; `Updater::verified_installer()` re-hashes before use. Tests `sweep_removes_only_old_update_dirs`, `verified_installer_rehashes_before_use`. Not done: Windows DACL; wiring `verified_installer` into a future installer launcher (nothing opens the file today; UI only reveals it). |
| 6 | Fixed | exact hosts (`github.com`, `api.github.com`, `objects.`/`release-assets.`/`github-releases.githubusercontent.com`), port 443 on Android too. Tests in `http.rs` and `UpdateModelTest`. Not done: requiring the first hop to be github.com/api.github.com. |
| 7 | Mostly fixed | all `uses:` pinned by SHA (verified via `gh api`; `dtolnay/rust-toolchain` pins the `stable` branch head and sets `toolchain: stable`); Gradle test/build step has no secrets, a separate signing step holds them, keystore written with `umask 077` and removed in an `if: always()` step; version job requires the tag commit to be an ancestor of `origin/main`. Skipped (needs a repo owner decision): tag-protection rule or `production` environment with reviewers, Dependabot config for actions, and a detached desktop signature (item 4 below). |
| 4 (orig.) | Open | needs a signing-key decision |
| 8-17 | Open | not in scope of this pass |

Not verified: the workflow itself was not run on GitHub (YAML parses). `actions/checkout@v7` exists and resolved to the SHA above.

## Call control from the computer (added after this review; not part of its scope)

New surface, listed so the next review does not miss it. Details: [call-control.md](../call-control.md).

- **Authority.** A paired computer can make the phone answer, decline or end a call. Gates: per-device `Telephony` consent on
  the computer (off by default), the phone's opt-in switch (off by default), the `ANSWER_PHONE_CALLS` runtime permission, and
  a call-state precondition on both ends. A malicious paired peer that the user already allowed "Calls" for can hang up or
  answer calls (answering routes no audio to it); it cannot dial.
- **Privacy.** Caller number/name cross the link only with `READ_CALL_LOG` / `READ_CONTACTS` and the opt-in. They live in memory
  on both sides, never in logs (`Debug` is redacted), and the control channel that serves them (`Request::Calls`) is the
  existing token-authenticated loopback port. `geekbridge-ctl calls` masks numbers unless `--reveal`. The Linux
  notification shows the caller label on the desktop's notification daemon (any local notification listener can read it).
- **Input handling.** `geekbridge.calls.*` bodies are strictly parsed and bounded (128-byte fields, control characters
  rejected, 64-character request ids); malformed or unsolicited packets count against the link's misbehaviour budget; a
  payload on these packets is refused; pending requests are capped (4 per phone, 20 s TTL).
- **Stale decisions.** `CallHub::act` only runs a target that is in the *current* view, so a click on a call that just ended
  or changed cannot hit another call.
- **Open:** no replay protection beyond request-id matching and the call-state precondition; the macOS helper path trusts
  the helper binary found next to the service; `READ_CALL_LOG` is Play-restricted (sideload distribution only).

## Findings by priority (status as of 2026-11-02; Low items are backlog, some already covered by the remediation table)

### High and Medium (summarised)

Details of the original write-ups are kept out of the public tree; the remediation table above is the authoritative status.

| # | Severity | Area | Status |
|---|----------|------|--------|
| 1 | High | Filename handling in the file-receive path (truncation at a character boundary) | Fixed, regression test |
| 2 | Medium | Temporary-file naming for concurrent receives | Fixed, regression test |
| 3 | Medium | Android share sheet accepting content from other apps | Fixed (blocked authorities, names shown in the confirmation); a caller-identity grant check is not added |
| 4 | Medium | Release integrity is "same trust root" (checksums are published with the artifacts, no detached publisher signature for desktop) | **OPEN**: needs a signing-key decision. The UI says "checksum verified", not "verified publisher". Tag protection and a protected release environment are recommended repository settings |
| 5 | Medium | CI action pinning | Fixed (actions pinned by commit SHA) |
| 6 | Medium | CI secret scoping | Fixed (secrets only in the signing step) |
| 7 | Medium | Availability of the file-payload listener against LAN hosts | Fixed (source filtering, shorter pre-handshake timeout); binding to the link interface is not done |
| 8 | Medium | Downloaded desktop installer handling | Fixed (per-user directory, sweep, re-hash before use); a Windows ACL is not done |

### Low
9. **Host allow-list is broader than needed.** `http.rs:106`. `*.githubusercontent.com` includes `raw.`, `gist.`, `user-images.` etc.,
   which serve user-controlled content. A release asset URL from the API may legitimately redirect only to
   `objects.githubusercontent.com` / `release-assets.githubusercontent.com` / `github-releases.githubusercontent.com`.
   Use exact hosts and require the first hop to be `github.com` / `api.github.com`. Same in Android `UpdateModel.kt:133`
   (which also does not check the port; `URI.host` with `:8443` passes). Since SHA256SUMS comes from the same release this
   is defence in depth.

10. **`Url::parse` accepts a backslash and non-ASCII bytes in the path/authority** (`http.rs:36-55`; only bytes <= 0x20 and 0x7f are
    rejected). These land unescaped in the request line. They do not enable host confusion, because the host must exactly
    match the allow-list after `@` rejection, but they are not valid HTTP. Reject `\` and bytes >= 0x80, or percent-encode.

11. **DNS resolution ignores the deadline.** `http.rs:267` `to_socket_addrs()` is blocking and unbounded, so a stalled resolver
    can hang the check thread past `check_timeout` (and keep `checking = true`, so later manual checks return `Busy`).
    Resolve on a helper thread with a timeout, or accept the limitation and document it.

12. **Loopback test hook is compiled into production.** `lib.rs` `Updater::new` and `http.rs` `Policy::loopback_origin`.
    Only `Config::github` is used by the service, so it is not reachable today. If `base_url` ever becomes configurable
    (env var or config file), an `http://127.0.0.1:port` base lets a local process serve a release, its sums and its installer,
    all consistent. Gate it behind `#[cfg(test)]` or a `testing` feature. The Android `allowLoopback` constructor flag is defaulted off
    and only used in tests: fine.

13. **Update state is trusted on load.** `lib.rs` `load_persisted` / `persist`. `update-state.json` (cached release,
    asset URL, sha256, ETag) is read without validation and written non-atomically through a predictable `update-state.tmp`
    (it follows symlinks in a shared dir). `begin_download` re-checks name and size but not that `browser_download_url` still passes
    `url_allowed`. It is covered by the policy in `Client::get`, which re-checks every hop, but validate on load too, and write the
    state with the project's `atomic_write_private`. Result: a same-user attacker cannot do more than they already can.

14. **Bidi and format characters are not stripped.** `share.rs` `sanitize_filename` (`:182-215`) and `strip_controls`, update `clean_notes`,
    Android `NameRules` / `cleanNotes`. `is_control()` covers only Cc. A peer name like `report‮txt.exe` renders as
    `reportexe.txt`. Strip or replace U+202A-202E, U+2066-2069, U+200B-200F, U+FEFF. Also lone `~`-style names and `NUL` with trailing
    `.` are fine.

15. **Received-URL handling is correct but worth keeping tight.** `share.rs` `is_openable_url` and `ShareNotifications.kt:99` both require
    `http(s)` and the notification needs a tap. Keep it that way. A desktop auto-open path must also go through `is_openable_url`.

16. **No free-space or aggregate limit** on receive (`share.rs:~296`; 8 GiB per file, no concurrent-transfer or disk check
    here). Enforce free space and a global in-flight cap at the service layer (`bridge-service/src/share.rs`). This was not reviewed in depth.

17. **Rate-limit error reporting from the Android client.** `UpdateChecker.kt:50` maps every 403 to "rate limiting". Cosmetic.
    The same ETag-pinning problem as F4 exists on the Android side, but there the cache stores the JSON body and the sums are re-fetched on every
    evaluation, so it is not affected.

### Checked and fine (no action)
- TLS: rustls with the `ring` provider, Mozilla roots from `webpki-roots`, TLS 1.2/1.3, hostname verified through `ServerName`. Android uses `HttpsURLConnection` with
  the system trust store. No `networkSecurityConfig` and `targetSdk 36`, so cleartext is blocked by default for the HTTP stack.
  The Rust KDE link uses its own sockets and pinned certificates.
- Redirects: manual, at most 5 hops, and every hop is re-checked against the policy (https + allow-listed host, no userinfo, scheme downgrades refused). Request
  headers are only `Accept` / `If-None-Match`, with no credentials to leak across hosts. Relative `//host` redirects are rejected.
- Size and time bounds: head 32 KiB, 100 headers, API 1 MiB, sums 64 KiB, asset 512 MiB (declared and actual), chunked decoding bounded,
  overall deadline plus per-read timeout, `Content-Encoding` must be identity.
- Download: filename comes from the validated name list (`safe_file_name`) and is never taken from the response; `create_new` in a fresh random 0700 directory; SHA-256 is
  computed while writing; size and hash must both match before the file is reported; the file and dir are deleted on any failure; no hash means no download.
- Version logic: strictly newer only (no downgrade), semver with prerelease ordering, build metadata ignored, leading zeros rejected, lengths bounded.
  `releases/latest` excludes drafts and prereleases, and the code also checks the flags (now also the tag, F3).
- Workflow: top-level `contents: read`, `write` only on publish; triggers on tags only (no `pull_request_target`, no untrusted PR input); `GITHUB_REF_NAME` is
  used via the shell environment, not interpolated into scripts, and the tag is validated by regex and must equal the Cargo version; the Gradle download is
  checked against a pinned SHA-256; `--locked` builds.
- Android update path: the FileProvider is `exported=false`, serves one fixed path read-only (`mode == "r"`), ignores the path component for file selection (no traversal) and
  has no insert/update/delete; the intent grants `FLAG_GRANT_READ_URI_PERMISSION` only for the installer; the APK is re-hashed immediately before handing it over
  (`UpdateSection.kt:198`); the user must grant "install unknown apps" and confirm; the `versionCode` must increase; the OS enforces package name and signing key continuity
  (so the debug-key fallback, F5, was the only continuity risk). Components: only `HomeActivity` and `ShareActivity` are exported; the receivers and the service are not;
  all PendingIntents are `FLAG_IMMUTABLE` with explicit components or fixed data; `allowBackup=false` and the extraction rules exclude everything.
- Share plugin: filenames are reduced to one component (`/` and `\` stripped, controls and Windows-reserved characters replaced, reserved device names prefixed, leading `.` prefixed, length bounded) and
  never joined as paths; `reserve_unique` uses `O_EXCL`, so existing files and symlinks are never overwritten or followed; data goes to a temp file and is renamed on success; the
  payload port is limited to 1739-1764; the peer certificate is pinned on both roles; the byte cap, idle timeout, cancel token and truncation check are present; text 48 KiB, URL 4 KiB; file size is
  checked before connecting.

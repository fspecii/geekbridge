# Security review: updater, release pipeline, Android update/share, KDE share plugin

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
| F5 | The workflow silently published a debug-signed APK for stable tags. That APK cannot be updated over, or replaced by, the real key. Stable tags now fail without the keystore secrets. Prereleases still fall back. | `release.yml` Android job |
| F6 | `gh release create` published the release first and attached assets afterwards. `releases/latest` could briefly show a release with no `SHA256SUMS` or APK. It now creates a draft with `--verify-tag`, then un-drafts. A completeness check on the five artifacts runs before `SHA256SUMS` is written. | `release.yml` publish job |

## Open findings (not fixed), by priority

### High
1. **Remote panic in the receive path: `String::truncate` on a non-char boundary.**
   `crates/bridge-kde/src/share.rs:268` `short.truncate(64)` panics when byte 64 falls inside a multi-byte character.
   The `while !is_char_boundary` loop after it never runs. A paired peer can send a filename of 63 ASCII bytes plus `é`
   (and similar names). The receive task panics, the transfer dies and the reserved file may be left behind.
   Fix: truncate at a char boundary first, e.g. `let mut n = 64.min(short.len()); while !short.is_char_boundary(n) { n -= 1; } short.truncate(n);`.
   Add a test with `"a".repeat(63) + "é"`.

### Medium
2. **Temp-file name collisions for long names.** `share.rs:262-273`. The temp name is `.gb-<first 64 bytes of the final name>.part`.
   Two concurrent receives whose reserved names share a 64-byte prefix map to the same temp path. The second
   `remove_file(&tmp)` deletes the first transfer's temp file. The first `rename(tmp, reserved)` can then move the
   second transfer's bytes into the first file's name, so contents are cross-wired. Fix: put a random or transfer-id
   token in the temp name (`.gb-<id>-<rand>.part`) and do not derive it from the peer name.

3. **Share-sheet confused deputy.** `ShareActivity.kt:78-86` and `AndroidManifest.xml` (ShareActivity is exported, as it must be).
   Any app can launch it with `EXTRA_STREAM` set to any `content://` URI. The activity reads the URI with this app's own
   identity. That covers this app's own provider (`dev.geekbridge.android.updates`) and any provider this app has
   permissions for (`READ_CONTACTS` / `READ_CALENDAR`: `content://com.android.contacts/...`, `content://com.android.calendar/...`).
   The user taps a computer to confirm, but the screen shows only "N file(s)", not what they are. A malicious app can
   therefore get contacts or calendar data sent to the user's own paired computer. The sink is trusted, so the impact is
   limited, but it still bypasses the permission-grant model. Fix, any of:
   - reject URIs whose authority is this app's own, or `com.android.contacts`, `com.android.calendar`, `call_log`, `sms`, `mms`;
   - require a real grant: `checkUriPermission(uri, -1, -1, MODE_FLAG_GRANT_READ)` against the calling UID (`getLaunchedFromUid()`), i.e. only accept URIs the sender could itself read;
   - show the resolved display names in the confirmation.
   Also consider `android:permission` / `CATEGORY_DEFAULT` only, which is already the case. There are no `file://` URIs, which is good.

4. **Release integrity is "same-trust-root".** `release.yml` publish job. `SHA256SUMS` is uploaded next to the artifacts, from the
   same job, and fetched from the same release. It protects against corruption and a mismatched CDN, not against a
   compromised repo, token or release. The Rust and Android updaters never run the installer, which limits the damage
   (the Android OS enforces signing continuity). For desktop, add a detached signature (minisign or Sigstore/cosign) and pin
   the public key in the binary. Until then the UI should say "checksum verified" and not "verified publisher".
   Related: anyone with push access to a `v*` tag can release. Add a tag-protection rule or a `production` environment
   with required reviewers, and have the version job check that the tag commit is reachable from `main`
   (`git merge-base --is-ancestor`).

5. **Actions are not pinned by commit SHA.** `release.yml:23,49,50,86-93,115,150` use `@v7`, `@v4`, `@v3`, `@stable`
   (`dtolnay/rust-toolchain@stable` is a moving branch; `android-actions/setup-android@v3` is third-party). The publish job has
   `contents: write`, and the Android build job holds the signing key. Pin every `uses:` to a full SHA with a version comment and
   let Dependabot bump them. Verify that `actions/checkout@v7` is the intended major version.

6. **Signing secrets are exposed to the whole Gradle step.** `release.yml:~125-140`. The `env:` block with the keystore password
   applies to `gradle :app:testDebugUnitTest :app:assembleDebug`, which runs build scripts, test code and Gradle plugins. Only the
   `apksigner` invocation needs the secrets. Split the step so tests and the debug build run without secrets. Write the
   keystore with `umask 077` and delete it in an `if: always()` step (it stays in `$RUNNER_TEMP`, which is ephemeral on
   hosted runners but not on self-hosted ones). Note also that `assembleRelease` is unsigned unless `signingConfig` is empty,
   so `apksigner` is the only signer. Good.

7. **Payload listener slot can be starved from the LAN.** `payload.rs:290-312` (`accept_peer`), binding `0.0.0.0` / `::`
   (`payload.rs:~190`). Pinned-certificate TLS stops strangers from receiving data. But 8 half-open connections that never finish
   the handshake keep all `MAX_PENDING_HANDSHAKES` slots busy for the 10 s handshake timeout, and further connections, including
   the real peer's, are dropped (`payload.rs:304`). A LAN attacker who repeats this can block every outgoing
   file for the 120 s accept window. Mitigations: drop connections whose source IP is not `ctx.peer_ip` before spawning a handshake;
   bind to the interface address of the link instead of unspecified; use a shorter pre-handshake timeout.

8. **Downloaded installer is a path, not a handle; no re-verification before use (desktop).** `bridge-update/src/lib.rs:581-640`.
   The file is hashed while written, then made read-only (`set_readonly` is 0444 on Unix, which the owner can undo) and left in
   `$TMPDIR/geekbridge-update-<rand>/`. The directory is created 0700 with `create`, so another local user cannot swap it, and
   the same user can do anything anyway. Residual risk: `$TMPDIR` is honoured and the file is created with the default umask.
   The path is shown to the user, who may copy or move it, and anything that opens it later (UI "reveal", a future installer
   launcher) trusts that it is unchanged. Fix: when any code starts to execute or open the file, re-hash it
   (as `UpdateSection.kt:198` already does on Android) or open it once and keep the fd. Prefer a per-user data dir
   (`$XDG_STATE_HOME/geekbridge/updates`, `%LOCALAPPDATA%`) over `temp_dir()`, and on Windows apply a restrictive DACL.
   Also: old `geekbridge-update-*` dirs (up to 512 MiB each) are never cleaned up after a crash or at next start; sweep them on startup.

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

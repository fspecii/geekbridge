# Updater against real GitHub (read-only smoke test)

Date 2026-11-06, head 850262c1c plus the fixes listed below. Nothing was published, tagged or changed
on `fspecii/geekbridge`; nothing was installed. About 40 MB were downloaded in total (the 3.3 MB APK three times, the 29 MB Linux archive once complete and once cut by a deadline), into
temporary directories that were deleted. The checks ran unauthenticated (60 requests/hour per IP).

What exists upstream today: one release, `v0.1.0-rc.1` (pre-release, 7 assets, every asset has an API
`digest`, `SHA256SUMS` present). There is no stable release and no rolling `dev` release.

How it was run:

* Library path: `GEEKBRIDGE_REALNET=1 cargo test -p bridge-update --lib realnet -- --ignored --nocapture`
  (`crates/bridge-update/src/realnet.rs`, ignored by default, so CI never touches the network).
* CLI path: a sandbox `geekbridge-service` (private HOME/XDG dirs, private `--state-dir`, ports
  17731-17734, `--no-announce --no-tray --no-calling-probe`) and `geekbridge-ctl --state-dir ... --control-port 17732
  update status|check|download|channel|install --dry-run`. The ports 1716, 17380, 17381 and 8000 were not touched.
* Offline, proxy and clock cases: the same test binary under `unshare -rn` (no network), with
  `HTTPS_PROXY=http://127.0.0.1:9`, and with an `LD_PRELOAD` shim that adds an offset to `CLOCK_REALTIME`.

## Verified against real GitHub

| Behaviour | Result |
| --- | --- |
| Request accepted with our `User-Agent: geekbridge-updater/<ver>` and `Accept: application/vnd.github+json`, `X-GitHub-Api-Version` | 200. Without a User-Agent GitHub answers 403 (curl check), so the header is required, not decoration. |
| Stable channel, only a pre-release published | `releases/latest` is 404, then the release list probe (200) gives "no stable release published - you are up to date". 2 requests per check. Not offered. |
| Dev channel, no `dev` release | `releases/tags/dev` 404, list probe 200, up to date. 2 requests. The "dev" offer path itself is mock-only: there is no real dev release to read. |
| `releases/tags/v0.1.0-rc.1` | 200, weak ETag `W/"..."`, `prerelease:true`, `draft:false`, 7 assets, parses (`0.1.0-rc.1`). |
| `If-None-Match` | 304 with an empty body. |
| 304 vs rate limit | An unauthenticated 304 **does count** (remaining 49 to 48 by curl, and 1 per conditional request in the test). GitHub only exempts 304s for authenticated callers. The ETag saves bandwidth, not quota, for the anonymous user. |
| Cost of a daily check | 2 of 60 per hour for the stable and the dev channel while no release is published (a 404 carries no ETag, so it is never conditional). Fine. |
| SHA256SUMS discovery | `github.com/.../SHA256SUMS` fetched, entry for `geekbridge-linux-x86_64.tar.gz` equals the API `digest` (`d7cca998...`). Platform selection picked `geekbridge-linux-x86_64.tar.gz`, not the longer `...-installer.tar.gz`. |
| Redirects | `github.com/.../download/...` answers 302 to `release-assets.githubusercontent.com` (pre-signed, Azure blob), which is on the allow list; download works. |
| Download | 3.3 MB APK and the 29 MB archive: size and SHA-256 match, file in a private 0700 directory, made read-only. |
| Wrong expected digest | `HashMismatch`, directory removed. |
| Declared size above the cap (`size - 1`) | `TooLarge` from Content-Length before the body is read, directory removed. |
| Declared size above the real size | `SizeMismatch`, directory removed. |
| 400 ms deadline in the middle of the 29 MB download | `Timeout`, nothing left behind. |
| Partial/resumable | There is no resume (no `Range`): a failed download is deleted and a retry starts at byte 0. By design (hash is computed while writing). |
| Sandbox ctl | `update status` before any check: "unknown (no check has completed)". `check`: up to date; a second `check` within 30 s did not hit the network (cooldown). `update download`: refused, "no newer release is known". `update install --dry-run` on a non-install copy: "Download only". `channel dev` / `channel stable` switch and drop the other channel's cache. |
| Offline (`unshare -rn`) | Clean, instant: "Couldn't reach api.github.com (Network is unreachable (os error 101)). Check your internet connection." State stays "not checked", not "up to date". |
| Dead `HTTPS_PROXY`, `ALL_PROXY` | **Ignored**: the check succeeds directly. See "Proxies". |
| Clock +10 minutes | Works (certificates are valid for months). |
| Clock +2 years / -2 years | TLS refused: certificate expired / not valid yet. Before this change the message was rustls' raw `invalid peer certificate: certificate expired: verification time ...`. Now: "The secure connection was refused because this computer's date or time looks wrong. Check the clock." |

## Defects fixed

1. TLS failure from a wrong clock now says so (`crates/bridge-update/src/http.rs`, unit test
   `certificate_time_errors_point_at_the_clock`).
2. `geekbridge-ctl update status` on the dev channel said "no stable release published"; it now says "no dev build published".
3. Android: `UpdateModel.parseRelease` used `optString`, which on Android turns JSON `null` into the text
   "null". GitHub sends `"body": null` for a release without a description, so the update card would have
   shown the notes "null" (also for null `html_url`, `digest`, asset name and the dev manifest fields). A `text()`
   helper returns "" for missing and null. Test `tagSpellingsAndApiShapeVariants` fails without the fix
   (verified) and passes with it.

## Android side (read, then unit tested with fake responses)

`UpdateChecker.kt` mirrors the desktop: `HttpURLConnection`, https only (http only to loopback in tests), manual redirects with the
host allow list re-checked on each hop, `User-Agent`, `Accept`, `X-GitHub-Api-Version`, `If-None-Match`, identity encoding, 1 MiB API cap,
64 KiB SHA256SUMS cap, deadline per read and overall, APK deleted on any mismatch, no Authorization header ever. Not run against real
GitHub (no device in this session); the Android system trust store applies there, and the platform proxy settings are honoured by `HttpURLConnection`.

New fake-response cases (`UpdateModelTest.tagSpellingsAndApiShapeVariants`): tags `v0.2.0`, `0.2.0`, `V0.2.0`, ` v0.2.0 `, `v0.2.0+build.7`
are offered; `v0.2.0-rc.1`, `0.2.0-rc.1`, `v0.1.0-rc.1` are not; `release-0.2.0`, `v0.2`, `0.2.0.1`, `latest` fail with a message;
`assets` empty, missing or null gives "no package" instead of a crash; `draft:true` and `prerelease:true` are both "up to date";
null `body`/`html_url`/`digest`/name. The existing suites already covered oversize, redirects to untrusted hosts, digest disagreement, 304, offline and truncation.

## Not exercised against real GitHub (mock only)

* Rate-limit exhaustion (403/429 with `X-RateLimit-Remaining: 0` or `Retry-After`): would burn the owner's IP quota; covered by `mock_server.rs` and
  the `http.rs` unit test. The persisted back-off is `Retry-After` or reset time, 60 s to 24 h.
* The stable offer/download path of the updater itself (needs a non-pre-release); the same functions (`attach_installer`,
  `download_verified`) were driven with the real rc.1 data.
* The dev-channel offer (no dev release exists).
* IPv6-only networks, from code reading only (`netns` has no upstream connectivity): the resolver result is tried in order, one
  address at a time, each with the 15 s connect timeout, inside the 30 s check deadline. A host with IPv6 addresses that black-holes (no RST/ICMP)
  waits up to 15 s before the IPv4 address is tried; IPv6-only works because the first address is used. No happy-eyeballs; left as is.

## Proxies and trust (documented behaviour, not changed)

* `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` are **not read**. The client connects directly. On a network that only allows egress through a proxy,
  the check fails with the "Couldn't reach api.github.com ... Check your internet connection." message. Updates are optional (`update disable`, or download the release by hand).
  Adding `CONNECT` support is a small change but it would need a policy for credentials and a size budget decision, so it was left out.
* TLS roots: `webpki-roots` (the Mozilla bundle compiled in), not the system store and not `rustls-native-certs`. Consequences: no dependence on the machine's CA package, but a
  TLS-inspecting corporate proxy or a locally trusted private CA is **not** trusted (fails with `invalid peer certificate: UnknownIssuer`). That is consistent with the
  allow-list design. The bundle ages with the build: a build older than a root rotation would need an update, which the updater cannot fetch; that is why the downloads page is the fallback.
* Time: rustls validates certificates with the system clock; a skew of minutes is harmless, years is not (now explained).

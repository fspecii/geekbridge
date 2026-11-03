# Supply chain: dependencies, licences, audits

Snapshot of the first-release audit (2026-11-02) and how to repeat it. GeekBridge itself is GPL-3.0-or-later: [licence-decision.md](licence-decision.md). Full third-party list and licence texts: [../THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Running the audits

The SBOM release asset (`scripts/gen-sbom.sh`) and the reproducibility audit are in [reproducible-builds.md](reproducible-builds.md).

```sh
# 1. Licence notices (deterministic; commit the result; CI-friendly)
scripts/gen-notices.sh            # rewrite THIRD_PARTY_NOTICES.md
scripts/gen-notices.sh --check    # exit 1 if it is stale

# 2. Licences, advisories, bans, sources with the repository policy (deny.toml)
cargo install cargo-deny --locked      # once
cargo deny check

# 3. RustSec advisories only
cargo install cargo-audit --locked     # once
cargo audit

# 4. What is in the tree
cargo metadata --locked --format-version 1 | python3 -I -c 'import json,sys; m=json.load(sys.stdin); print(len(m["packages"]))'
cargo tree --locked -d                 # duplicate versions
cargo tree --locked --target all -i <crate>   # who pulls a crate in
```

Without those tools, the advisory check can be reproduced from the advisory database: `git clone --depth 1 https://github.com/rustsec/advisory-db`, then for each `crates/<name>/RUSTSEC-*.md` whose crate and version are in `Cargo.lock` and not covered by `patched`/`unaffected`, report it. The yanked check is one request per crate to the sparse index (`https://index.crates.io/<path>`, field `yanked`). Both were done this way on 2026-11-02 and the results are below.

The Android side has no Cargo: `cd android && gradle :app:dependencies --configuration releaseRuntimeClasspath` (see "Android"; use `./gradlew` instead of `gradle` if you have not run `scripts/bootstrap-android.sh`).

## Rust: findings (462 packages, 11 of them ours, 451 third-party)

- **Sources:** every dependency comes from crates.io. No git or path dependencies besides our own workspace crates. `deny.toml` enforces this.
- **Licences:** all permissive (counts: MIT/Apache-2.0 variants ~380, Unicode-3.0 18, Zlib 4+, ISC 3, BSL-1.0 2, Unlicense 4+, BSD 3, CC0 1, CDLA-Permissive-2.0 1, plus composites). No GPL/AGPL/LGPL/SSPL/proprietary crate. Two crates (`r-efi` 5.3.0 and 6.0.0, UEFI-only, not linked into anything we ship) offer `LGPL-2.1-or-later` only as an alternative to MIT/Apache-2.0. Attention items:
  - Fonts: egui's `default_fonts` feature is off, so the Ubuntu Font Licence font is no longer embedded; `third_party/fonts` carries Noto Sans (subset, OFL-1.1), Noto Emoji (OFL-1.1), Hack (MIT/Bitstream Vera) and emoji-icon-font (MIT). `scripts/gen-notices.sh` fails if a crate's licence expression has no GPL-3.0-or-later-compatible alternative.
  - `webpki-roots` (CDLA-Permissive-2.0) embeds the Mozilla CA bundle; `ring` has its own ISC/OpenSSL-style text (`Apache-2.0 AND ISC`); `clipboard-win`, `error-code` BSL-1.0 (Windows only).
  - The workspace crates are `GPL-3.0-or-later` and `publish = false`.
- **Advisories** (advisory-db commit of 2026-10-30): **no vulnerabilities.** Four "unmaintained" notices that `cargo-deny` reports (a fifth, `paste` 1.0.15 RUSTSEC-2024-0436, is a compile-time macro only and is not surfaced because cargo-deny limits unmaintained notices to direct dependencies), all transitive, none with a patched version: `zvariant` 5.15.1 (RUSTSEC-2026-0370), `zbus_names` 4.3.5 (-0371), `zvariant_derive` 5.15.1 (-0372) via `zbus` 5 (Linux call integration); `ttf-parser` 0.25.1 (RUSTSEC-2026-0192) via egui 0.31 `ab_glyph`. They are listed with reasons in `deny.toml` so the CI job stays quiet; re-evaluate on the next zbus/egui upgrade.
- **Yanked:** `clipboard-win` 5.4.1 (Windows-only, via `arboard` ← `egui-winit`) was yanked. Fixed in this audit with `cargo update -p clipboard-win --precise 5.4.2` (lock file only; 5.4.3 also exists). Everything else (449 of 451 verified, `cc` 1.10.0 re-checked by hand) is un-yanked.
- **Unused dependencies** (grep of every crate's sources, tests and build scripts for each declared dependency): `serde_json` in `bridge-apps` and `serde` in `bridge-phone-sim` were provably unused and were removed (build, clippy `-D warnings` and tests unaffected; the one failing test, `bridge-phone-sim` `data_link::consent_gates_every_feature_and_data_flows_end_to_end` ("timed out waiting for conversations"), belongs to the in-progress Messages work and does not depend on either crate). No others found. Re-run the same check with `cargo machete` if you install it (it can report false positives for macro-only crates).
- **Duplicate versions (30 crates on the Linux target, ~40 across all targets).** Almost all come from `winit` 0.30 / `egui` 0.31 (older `smithay-client-toolkit` 0.19 + `calloop` 0.13 + `rustix` 0.38 + `thiserror` 1 + `bitflags` 1) living next to newer stacks (`zbus`, `rustls`/`rcgen`, `png`): `thiserror` 1/2, `rustix` 0.38/1, `bitflags` 1/2, `syn` 2/3 (build time), `base64` 0.22 (ours) vs 0.23 (`pem`), `getrandom` 0.2/0.3/0.4 (`ring` pins 0.2), `miniz_oxide` 0.8/0.9, `hashbrown` 0.15/0.17, `windows-*` several (Windows only). Our own crates add no duplicates except `base64` 0.22 vs `pem`'s 0.23; bumping `bridge-kde` to base64 0.23 would remove it (small API change, not done here). The size cost is in the egui desktop binaries only; the Android library and the headless service do not link egui. The real fix is upgrading `eframe`/`egui` when the 0.32+ line is adopted; no action required now.
- **Build scripts and proc macros:** 79 packages have `build.rs`, 38 are proc macros. Authors are the usual ecosystem maintainers: `dtolnay` (serde, quote, proc-macro2, thiserror, paste…), `microsoft` (windows-rs family), `rust-lang`, `rust-windowing`/`smithay`/`gfx-rs`/`madsmtm` (winit, wayland, wgpu, objc2), `jni-rs`, `bytecodealliance`, `briansmith` (`ring`), `rustls`, `servo`, `tokio-rs`, `z-galaxy` (zbus), `unicode-org` (ICU data). No unknown individual stood out. The less common ones are all long-established crates (`orlp` foldhash, `Amanieu` parking_lot, `Frommi` miniz_oxide, `SSheldon` objc/block, `psychon` x11rb, `jhpratt` time). Build scripts run arbitrary code at compile time, so build in CI or a clean environment and use `cargo build --locked`. There is no vendoring; the lock file's checksums pin the exact source.
- **Android native library:** `bridge-android` builds with the `android` profile and links only the subset needed by the bridge core (no egui, no zbus). It is covered by the same list.

## Android

- **Runtime dependencies:** `releaseRuntimeClasspath` is only `org.jetbrains.kotlin:kotlin-stdlib:2.2.21` (Apache-2.0) and its `org.jetbrains:annotations:13.0` (Apache-2.0), added implicitly by the Kotlin plugin and shrunk by R8. No AndroidX, no Material, no Play Services, no image/font/network libraries. The UI uses platform widgets and code-drawn views, and system fonts only (`Typeface.create("sans-serif-medium", …)`, monospace). `android.useAndroidX=false` in `gradle.properties` enforces this.
- **Build plugins:** Android Gradle Plugin 8.13.2 (Apache-2.0; Android SDK components fall under the Android SDK licence the CI accepts via `sdkmanager --licenses`), Kotlin Gradle plugin 2.2.21 (Apache-2.0). NDK r30 (Apache-2.0 with LLVM exceptions) is used to build the native library. None of these ends up in the APK.
- **Test dependencies (not shipped):** `junit:junit:4.13.2` (EPL-1.0; pulls `hamcrest-core` 1.3, BSD-3-Clause) and `com.vaadin.external.google:android-json:0.0.20131108.vaadin1` (Apache-2.0). `android.jar`'s `org.json` is a stub that throws on the JVM, so unit tests need a real implementation; `android-json` is a repackaging of AOSP's own `org.json` (the code the app uses on a phone), so tests exercise the same behaviour. It replaces `org.json:json`, whose "JSON License" ("shall be used for Good, not Evil") most open-source policies treat as non-free. `test*` configurations only: nothing here reaches the APK, and the app's runtime classpath is unchanged.
- **Gradle distribution pinning:** `android/gradlew` (documented path) downloads Gradle 8.13 from `services.gradle.org` and refuses it unless it matches `distributionSha256Sum` in `android/gradle/wrapper/gradle-wrapper.properties` (`20f1b1176237254a6fc204d8434196fa11a4cfb387567519c61556e8710aed78`, from `gradle-8.13-bin.zip.sha256`). The committed binary `android/gradle/wrapper/gradle-wrapper.jar` has SHA-256 `81a82aaea5abcc8ff68b3dfcb58b3c3c429378efd98e7433460610fecd7ae45f`, which matches Gradle's published wrapper checksum for 8.13 (<https://gradle.org/release-checksums/>, also `services.gradle.org/distributions/gradle-8.13-wrapper.jar.sha256`). Check it with `sha256sum android/gradle/wrapper/gradle-wrapper.jar`; a PR that changes the jar must say why and carry the new published checksum. CI and the release workflow use `./gradlew`. The offline project-local flow (`source scripts/android-env.sh`, then `gradle --offline ...` with the Gradle from `scripts/bootstrap-android.sh`, pinned by the same SHA-256) still works and is what the maintainer scripts use.
- **Dependency verification:** `android/gradle/verification-metadata.xml` holds a SHA-256 for every artefact (jars, AARs, POMs, Gradle modules) from Google Maven, Maven Central and the Gradle Plugin Portal that the build resolves; Gradle fails the build on any mismatch or unlisted artefact. It was generated with network by
  ```sh
  cd android
  ./gradlew --write-verification-metadata sha256 help testDebugUnitTest lintDebug assembleDebug assembleRelease
  ```
  (hashes are trust-on-first-use, not PGP signatures; `verify-signatures` is off because many AndroidX/Google artefacts are not signed with keys published on a keyserver). When you add or bump a dependency, re-run that command, review the diff of the XML, and commit it with the change. Add other tasks to the command if you start using configurations it did not touch (for example `lintRelease`).

## Third-party assets

- `assets/icon.svg`: a green rounded square with a white "G" set in a generic system font stack, authored in this repository (single initial commit `bae186b`). No external artwork. It names no font file, so nothing is embedded.
- Android icons (`android/app/src/main/res/drawable/*.xml`) are hand-written vector paths (stroke-based), not Material Symbols or another icon set; the launcher foreground is likewise our own. The dot-matrix UI (Nothing-OS-inspired) is drawn in code (`DotComponents.kt`, `DotCalendar.kt`, desktop `icons.rs`/`call_ui.rs`); no Nothing font (Ndot) or other font file is bundled.
- Desktop fonts: OFL/MIT fonts bundled under `third_party/fonts` (see its README and the notices).
- Screenshots in `docs/img/*.png` are captures of GeekBridge itself on an emulator/desktop; they contain only our UI and synthetic data.
- Upstream KDE Connect sources and any other third-party material used during development are kept outside the repository (git-ignored, never tracked). A line-level scan of 94k distinctive lines found only language/API boilerplate matches.

## cargo-deny triage (2026-11-02, cargo-deny 0.20.2, advisory-db of that day)

`cargo deny check` now reports `advisories ok, bans ok, licenses ok, sources ok`.

- **Licences:** the earlier failures were only the workspace crates. They are now `GPL-3.0-or-later` (`license.workspace = true`, `publish = false`), and `[licenses.private] ignore = true` skips them in either state, so GPL is **not** on the allow list and a third-party GPL crate would still fail. `r-efi` is clarified to `MIT OR Apache-2.0`. CC0-1.0 and MPL-2.0 are not present and not allowed.
- **Bans:** `wildcards = "deny"` (path dependencies of `publish = false` crates are exempt); duplicate versions stay `warn`. **Sources:** crates.io only.
- **Advisories, no fixed version reachable, ignored in `deny.toml` with reason and review date (2027-02-01):**
  - `RUSTSEC-2026-0194` and `RUSTSEC-2026-0195` (`quick-xml` 0.30: quadratic attribute check, unbounded `NsReader` namespaces). Path: `eframe` 0.31 -> `egui-winit` -> `accesskit_winit` -> `accesskit_unix` -> `atspi` 0.22 -> `zbus-lockstep` -> `zbus_xml` 4.0. `zbus_xml` 4 pins `quick-xml` 0.30; the fix (>= 0.41) needs a newer accesskit/atspi line, i.e. an egui upgrade (`cargo update` cannot do it). Reachability: Linux desktop UI accessibility only, and the XML is D-Bus introspection data from the user's own session bus, never network input. Severity: DoS (CPU/memory) by a hostile local bus peer.
  - `RUSTSEC-2024-0436` (`paste` unmaintained) via `accesskit_windows`: compile-time macro, no runtime code.
  - The existing zbus/ttf-parser "unmaintained" ignores are unchanged.
  Remove these ignores when egui is upgraded.
- **CI:** the `supply-chain` job blocks on bans, licences and sources and on the notices check; the advisories step is report-only (`continue-on-error` on that step), because new advisories appear independent of our changes and cargo-deny cannot separate direct from transitive crates.

Re-run: `cargo install --locked cargo-deny --version 0.20.2` (or the release binary from GitHub), then `cargo deny --locked check`; the advisory DB is fetched automatically (needs network).

## CI notes (earlier proposal, partly done)

1. A scheduled workflow (weekly `cron`) running `cargo deny check advisories` and opening an issue on failure.
2. A step `scripts/gen-notices.sh --check` so `THIRD_PARTY_NOTICES.md` cannot go stale.
3. Dependabot/Renovate for Cargo and the GitHub Actions SHAs (all actions are already pinned to commits).
4. Commit `android/gradle/verification-metadata.xml` and run Gradle with `--dependency-verification strict`.
5. At release time, build from a tag with `--locked`, publish SHA-256 sums, and (optionally) generate an SBOM with `cargo cyclonedx`.

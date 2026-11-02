# Supply-chain audit 2 (2026-11-03)

Follow-up to the first-release audit in [../supply-chain.md](../supply-chain.md). Tools: `cargo-deny`, `cargo-audit`,
`cargo-udeps` and `cargo-machete` are not installed, so the checks were reproduced by hand (method as in supply-chain.md).

## Advisories (advisory-db clone of 2026-11-02, matched against Cargo.lock)
No new vulnerability. Everything found is already in `deny.toml` or is informational only:
- `quick-xml` 0.30.0 RUSTSEC-2026-0194/-0195 (accessibility D-Bus XML, needs egui upgrade), `zvariant`/`zvariant_derive`/`zbus_names`
  (-0370/-0371/-0372), `ttf-parser` (-0192), `paste` (RUSTSEC-2024-0436): all already ignored with reasons.
- `ring` 0.17.14 RUSTSEC-2025-0007 ("unmaintained") was withdrawn upstream on 2025-02-22; nothing to do.
- `xml-rs` 0.8.29 RUSTSEC-2022-0048 (unmaintained, new in this list): build-dependency only, via `gl_generator` for glutin's EGL
  bindings (eframe). Never in a shipped binary, informational, and not surfaced by cargo-deny (not a direct dependency). No action
  until egui is upgraded.

## Yanked
Checked all 482 registry packages against the crates.io sparse index. One hit: `async-executor` 1.14.0 (via
`accesskit_unix`, eframe). Fixed: `cargo update -p async-executor --precise 1.14.1` (lock file only, patch release);
`THIRD_PARTY_NOTICES.md` regenerated.

## Licences
Unchanged from audit 1: all third-party crates are permissive (MIT/Apache/BSD/ISC/Zlib/Unicode/Unlicense/0BSD/BSL/CDLA/OFL), none is
GPL-incompatible, no (A/L)GPL crates, `deny.toml` allow list has no copyleft. `scripts/gen-notices.sh --check` passes after
regeneration. Android: runtime dependencies are none beyond the platform; only test-scope `junit` 4.13.2 (EPL-1.0, never
packaged) and AOSP `android-json` (Apache-2.0, test only). Build plugins AGP 8.13.2 and Kotlin 2.2.21; `verification-metadata.xml`
pins checksums. No Gradle change needed; newer majors (AGP 9, etc.) are left for the owner's Dependabot decision.

## Unused dependencies
Manual scan of every declared dependency against each crate's sources: none unused (`zbus_xml`, `blocking`, `async-io`,
`futures-lite` are all referenced). Nothing removed.

## Duplicates (`cargo tree -d`)
Same picture as audit 1 (about 30 on Linux): winit/egui 0.31 era (`smithay-client-toolkit`, `calloop`, `rustix` 0.38,
`thiserror` 1, `bitflags` 1, `zbus` 4 via accesskit/atspi, `quick-xml` 0.30) versus the newer stacks. All are transitive of
`eframe`; they only go away with an egui upgrade (a major bump, not done). The one duplicate we own is `base64` 0.22 vs
`pem`'s 0.23: bumping `bridge-kde` is a 0.x breaking bump, left alone per the no-major-bumps rule.

## Release profile
`[profile.release]`: opt-level "s", fat LTO, codegen-units 1, strip symbols; `[profile.android]`: opt-level "z", same. Service
binary 5.67 MB, ctl 1.27 MB (measured in docs/performance.md). `panic = "unwind"` is deliberate (tokio isolates task panics, JNI
boundary catches them), so `panic = "abort"` would trade robustness for roughly 5-10 percent size and is not recommended.
No change.

## Applied
- `Cargo.lock`: async-executor 1.14.0 -> 1.14.1 (yanked fix); `THIRD_PARTY_NOTICES.md` regenerated.
- No Dependabot PR touched; no major bumps.

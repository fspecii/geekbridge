# Licence decision (owner's choice, not made yet)

The repository intentionally has **no `LICENSE` file** and no `license` field in any `Cargo.toml`. Until one is added, the code is "all rights reserved" by default: nobody may legally copy, fork or redistribute it. This page lays out what the audit found and what each option implies so the choice can be made once, before the first release. It is an engineering summary, not legal advice.

Audit date: 2026-11-02. Method and tooling: [supply-chain.md](supply-chain.md).

## What constrains the choice

### 1. Our dependencies do not constrain it

All 451 third-party Rust crates in `Cargo.lock` carry permissive licences (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, Unlicense, 0BSD, CC0, BSL-1.0, CDLA-Permissive-2.0). The only copyleft string anywhere is `r-efi` (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`, UEFI-only), where we take MIT/Apache-2.0. The Android app has no third-party runtime code except the Kotlin standard library (Apache-2.0). So **any** licence below is compatible with every dependency, including a closed-source one. Two details:

- Apache-2.0 is compatible with GPL-3.0 but **not** with GPL-2.0-only. If you choose GPL, choose "3.0 or later" (or AGPL-3.0), not "2.0 only".
- The desktop apps embed egui's default fonts. `Ubuntu-Light` is under the Ubuntu Font Licence 1.0, which the FSF lists as free but GPL-incompatible (naming and modification clauses). Fonts are data files shipped unmodified, usually treated as a separate work in an aggregate, but if you pick a GPL-family licence either get a legal view or drop the `default_fonts` feature of `eframe` and ship only OFL/MIT fonts (Hack is MIT-style, Noto Emoji is OFL).

### 2. KDE Connect is GPL; we implement the protocol, we do not use its code

GeekBridge speaks the KDE Connect protocol (`crates/bridge-kde`) so a stock KDE Connect phone or desktop can talk to it. The reference implementations (`kdeconnect-kde`, `kdeconnect-android`) are licensed per file, mostly `GPL-2.0-only OR GPL-3.0-only OR LicenseRef-KDE-Accepted-GPL`, with some LGPL/MIT/BSD/CC0 files.

- Our implementation was written from our own protocol notes (`docs/research/kde-protocol.md`: observed behaviour, wire field names, constants, state sequences, independently computed test vectors). Port numbers, JSON field names and state names are the protocol, not the expression of GPL code, so an independent implementation does not inherit the GPL. Many permissive implementations of the same protocol exist.
- The audit checked this rather than assuming it: `references/` and `downloads/` are git-ignored and **zero** files from them are tracked. A script compared about 94,000 distinctive lines (55+ characters) from every reference clone against every tracked source file; the only matches are unavoidable boilerplate (`use std::…`, `fn fmt(&self, f: &mut Formatter)`, rustls trait signatures, Android permission lines and XML namespaces, `onRequestPermissionsResult` overrides). No SPDX/copyright headers from other projects exist in tracked code. Test certificates from the KDE test sources are not committed (`pairing.rs` reads them from a path in an environment variable).
- **What this implies:** you are free to choose a permissive licence, and you are also free to choose GPL. What you may *not* do under a permissive licence is later paste or mechanically translate KDE (or Sefirah/TwinDesk, both GPL-3.0) code into the tree. If you want that freedom in the future, pick GPL-3.0-or-later now. `docs/architecture.md` and `kde-protocol.md` already say "do not translate implementation bodies"; keep that rule whichever licence you pick, and keep the provenance notes.
- The names "KDE Connect" and "Phone Link" are other parties' marks. Use them nominatively ("compatible with KDE Connect") and never in the product name or icon. Microsoft Phone Link artefacts in `downloads/` are analysis input only and must never be distributed or shipped.

## Options

| Option | For | Against | Consequences |
|---|---|---|---|
| **A. MIT OR Apache-2.0 (dual)** | Rust ecosystem norm, matches almost every dependency, minimal friction for contributors and packagers (distros, Homebrew, winget, F-Droid all fine). Apache-2.0 gives an explicit patent grant; MIT keeps GPL-2 compatibility for downstream. | Anyone may fork it into a closed product. Cannot ever absorb GPL code (KDE Connect, TwinDesk, Sefirah). | Add `LICENSE-MIT` and `LICENSE-APACHE`, `license = "MIT OR Apache-2.0"` in `[workspace.package]` plus `license.workspace = true` in each crate, README line. Keep `THIRD_PARTY_NOTICES.md` in packages. |
| **B. GPL-3.0-or-later** | Same family as the ecosystem we interoperate with (KDE Connect, GSConnect, Valent are GPL), so KDE-derived code could be incorporated later and KDE developers can contribute freely. Keeps forks open. | Closed redistribution and some stores are off the table (GPL-3 conflicts with Apple App Store terms; a future iOS/macOS App Store build would need a separate grant). Some companies will not touch it. The Ubuntu font question above needs resolving. | Add `LICENSE` (GPL-3.0 text), per-file notice recommended, `license = "GPL-3.0-or-later"`. Contributors' patches are then GPL; relicensing later needs everyone's consent. |
| **C. AGPL-3.0-or-later** | As B plus network-service clause. | Little real benefit here (the service is local); most packagers and enterprises avoid it. | Same mechanics as B, stricter. Not recommended unless you plan a hosted component. |
| **D. Apache-2.0 only** | Patent grant, simple, widely accepted. | Not compatible with GPL-2.0-only consumers; otherwise like A. | One `LICENSE` file. |
| **E. MPL-2.0** | File-level copyleft: improvements to our files stay open, but can be combined with anything. | Less familiar, GPL-2/3 compatible only via its secondary-licence clause. | One `LICENSE` file plus header notes. |
| **F. Proprietary / source-available** | Full control; dependency licences allow it (notices only). | Cannot use any GPL-derived code; hostile to distro packaging and community trust for a tool that touches SMS, calls and clipboard. | Needs EULA, notices and a way to ship `THIRD_PARTY_NOTICES.md` inside each installer. |

## Recommendation

If the goal is adoption and the freedom to stay independent of the KDE code, choose **A (MIT OR Apache-2.0)**: it matches the dependency tree, nothing in the audit blocks it, and the independent-implementation record above is clean. If the goal is to live inside the KDE/GNOME ecosystem and reserve the right to reuse their code, choose **B (GPL-3.0-or-later)** and settle the font question first. Whichever you pick, do it before accepting outside contributions: relicensing afterwards requires every contributor's consent (a DCO sign-off policy and, for option B or later relicensing, possibly a CLA).

## After deciding (checklist for whoever wires it up)

1. Add the licence file(s) at the repo root; set `license` in `[workspace.package]`, inherit it in each crate (`license.workspace = true`; today none of the 11 workspace crates declare one, which makes `cargo deny check licenses` report them as unlicensed).
2. Mention the licence in `README.md`, the Android About/diagnostics screen, and the `.desktop`/MSIX/Info.plist metadata where a field exists.
3. Ship `THIRD_PARTY_NOTICES.md` inside every distributed artefact: Linux package (`scripts/package-linux.sh`), Windows installer/MSIX, macOS app bundle, and the APK (an About screen link or an asset). Regenerate with `scripts/gen-notices.sh` after every `Cargo.lock` change; CI can run `scripts/gen-notices.sh --check`.
4. Tick the licence items in `docs/release-checklist.md`.

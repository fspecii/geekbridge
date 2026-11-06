# GeekBridge documentation

Start with the [project README](../README.md). Troubleshooting is a section of the [user guide](user-guide.md).
Documents under [reports/](reports/) are dated snapshots, not living documentation; each says so in its first lines.

## Using GeekBridge

- [FAQ](FAQ.md): the questions people ask first
- [User guide](user-guide.md): install, pair, daily use, symptom-keyed troubleshooting
- [Real-device quickstart](real-device-quickstart.md): for testers with a source checkout: drive a USB-connected phone with `scripts/realdevice.sh`
- [Omarchy / Hyprland setup](omarchy.md): hotkeys, window rules, notifications; also GNOME, KDE and sway
- [Pairing](pairing.md): QR and code pairing, discovery, what is trusted
- [Sharing](sharing.md), [clipboard sync](clipboard-sync.md), [call control](call-control.md), [phone data: contacts, calendar, SMS](phone-data-link.md): feature behaviour and consents
- [Call audio options](call-audio-options.md): how call audio can reach the computer
- [Android setup and battery](android-power.md): setup wizard, background connection, power use
- [Accessibility](accessibility.md): keyboard, screen reader and contrast support
- [Updates](updates.md): the optional update check and how updates are verified
- [Packaging for Linux](packaging-linux.md) and [local release builds](release-local.md): building installable packages

## Contributing and architecture

- [Contributor onboarding](contributor-onboarding.md) and [CONTRIBUTING](../CONTRIBUTING.md)
- [Architecture](architecture.md) and [builds](builds.md): crates, data flow, how to build every target
- [Service integration](service-integration.md): daemon, control API, client wiring
- [State formats](state-formats.md): on-disk files and their versioning
- [Privacy review](privacy.md): every kind of phone data, where it is kept, what leaves the LAN, how to erase it
- [KDE Connect interoperability](kdeconnect-interop.md): protocol coverage and what stock peers do
- [Running next to stock KDE Connect](coexistence.md): shared ports, discovery, QR/IP pairing, clipboard double sync
- Android: [app](android-app.md), [native library](android-native.md), [contract](android-contract.md), [transport](android-transport.md), [runtime notes](android-runtime.md), [call state](android-calls.md), [performance](android-perf.md)
- [Desktop design](design-desktop.md): the Nothing x macOS/iOS visual language
- [Performance](performance.md): budgets and how to measure them
- [Golden images](golden-images.md): visual regression tests for the desktop and Android UI, and how to update them
- [Adding a language](translations.md): the short hands-on contributor guide (worked example `eo`), with [Translating](translating.md) as the full rules: file formats, pseudo-locales, glossary, tone
- [Disk hygiene](disk-hygiene.md): `make disk` / `make clean-dev`, scratch roots, free-space pre-flight
- [Test coverage](coverage.md): measured line/function/region coverage, targets and how to regenerate
- [Fuzzing](fuzzing.md), [supply chain](supply-chain.md), [SBOM and reproducible builds](reproducible-builds.md), [CI dry run](ci-dry-run.md), [emulator end-to-end suites in CI](emulator-ci.md)
- [Emulated calls](emulated-calls.md) and [updater end to end](updater-e2e.md): development test setups
- [What works on which system](platform-support.md): feature by Linux / Windows / macOS / Android
- [Platform status](platform-status.md): what is verified on which OS

## Maintainers: releasing

- [Release checklist](release-checklist.md), [licence decision](licence-decision.md), [v0.1.0 release notes (draft)](release-notes-v0.1.0.md), [v0.1.0 go/no-go](reports/stable-go-no-go.md)
- [Real-device test plan](real-device-test-plan.md): the ordered session checklist for one phone and one PC, with the report and link-watch kit
- [Real-device clipboard and calls script](real-device-clipboard-calls.md): step-by-step for the Galaxy Z Fold5 + Hyprland, with `geekbridge-ctl` proofs

## Reports (snapshots)

Findings from reviews, audits and long runs. Open items are kept visible; later status is in git history and the
[CHANGELOG](../CHANGELOG.md).

- Security: [security review](reports/security-review.md), [second security review](reports/security-review-2.md); to report a vulnerability see [SECURITY](../SECURITY.md)
- Code correctness: [code review](reports/code-review.md), [code review 2](reports/code-review-2.md)
- Testing: [emulator end-to-end findings](reports/e2e-findings.md), [soak report](reports/soak-report.md), [scale report](reports/scale-report.md), [chaos findings](reports/chaos-findings.md), [release-candidate report](reports/release-candidate-report.md)
- Audits: [pre-flight upgrade of the paired phone and the PC service](reports/preflight-upgrade.md), [upgrade path: old state read by the new build and back](reports/upgrade-path.md), [production readiness](reports/production-readiness.md), [first-run audit](reports/first-run-audit.md), [desktop UI review](reports/desktop-ui-review.md), [Android device risks](reports/android-device-risks.md)
- Features (design notes and what was checked): [recent photos](design/recent-photos.md) ([report](reports/recent-photos.md)), [notification mirroring](reports/notification-mirroring.md), [battery status](reports/battery-status-feature.md), [why disconnected](reports/why-disconnected.md), [goodbye and listening-port hint](reports/goodbye-and-port-hint.md), [desktop onboarding](reports/desktop-onboarding.md), [redial collision](reports/redial-collision.md), [link stability review 2](reports/link-stability-review-2.md), [emulator features end to end](reports/emulator-features-e2e.md), [Windows and macOS doctor parity](reports/windows-macos-doctor-parity.md)
- Performance and long runs: [size and memory round 3](reports/size-memory-round-3.md), [Android ICF](reports/android-icf-validation.md), [4-hour soak](reports/soak-4h-2026-11-03.md), [regression run 2026-11-03](reports/regression-2026-11-03.md)
- Docs audits: [docs audit 4](reports/docs-audit-4.md)

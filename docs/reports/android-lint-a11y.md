# Android lint, accessibility and updater checks

Run on 2026-11-02 with the project toolchain (`source scripts/android-env.sh`, `gradle --offline lintDebug testDebugUnitTest`). Disk: 141 GB free.

## Lint (debug)
Zero errors. 22 warnings, none accessibility-related except one:
- `AccessibilityFocus` in `A11yLabels.restoreFocus`: this puts TalkBack focus back on the node the user already had after a screen rebuild. It is intentional, so it is now suppressed with that reason beside it.
- Remaining warnings (NestedWeights, StaticFieldLeak, PluralsCandidate, UseCompoundDrawables, ExportedReceiver, InlinedApi, TypographyDashes, ApplySharedPref, ObsoleteSdkInt, UnusedResources, AGP version) are style or maintenance notes and were left alone.

## Accessibility review
- Touch targets: the shared tokens (`Tok.TARGET = 48`) and the interactive controls in `Ui.kt` and `DotComponents.kt` use 48dp minimums. The only sub-48dp element is the 40dp striped info row in `Ui.row`, which is not interactive.
- Content descriptions: the dot-matrix views are described through `A11yLabels` (spoken text, never dots), and these labels are unit-tested.
- Contrast: new `ThemeContrastTest` computes WCAG ratios from `colors.xml` for light and dark. Text, muted, warn, bad and ok colours on surface, card, stripe and fill, plus on-accent, on-hero and hero-muted, all reach at least 4.5:1. The red accent reaches at least 3:1 on cards. No colour changes were needed.
- TalkBack order on the pairing and computer list: the existing code restores focus by child path and uses single focus stops per tile. I found no ordering defect in static review, but I did not run TalkBack on a device or emulator. A real-device TalkBack pass is still open.

## UpdateChecker
Already covered by existing tests: 404 and plain 403 (NoPublicRelease), rate limit (403 with X-RateLimit-Remaining 0, and 429), SHA-256 mismatch (file deleted), missing checksum, oversize, redirects, ETag. Added `prereleaseIsIgnored_andDowngradeIsNotOffered`: a prerelease tag or flag is never offered, an older release is up to date, and a newer semver whose versionCode is not above the installed one is not offered.

## Result
`lintDebug` and `testDebugUnitTest` pass (484 tests). One intermediate run failed `DebugDiagnosticsTest` while another change to that file was in progress; it passed on rerun. No emulator was started.

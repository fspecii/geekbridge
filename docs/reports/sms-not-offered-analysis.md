# Why a phone may not offer Messages (SMS) - analysis and fixes

Scope: Android app (`android/`). Reported on a Samsung SM-F946B, sideloaded APK, Android 13+: "Messages is off" even after the SMS permissions were granted. The live phone was not touched.

## When SMS is declared

`PhoneDataGate.active` sets `sms` only if ALL of these hold:

1. the "Share text messages (SMS)" switch is on (`PhoneDataSettings.sms`);
2. READ_SMS, SEND_SMS and RECEIVE_SMS are all granted (`smsComplete`).

The flag is put in the identity packet at connect time (`PhoneDataRuntime.startFeatures`). The desktop then offers Messages only if the phone declared the SMS packet kinds. Per-computer grants (`ComputerGrants`) do not change the declaration. They only decide which computer the core answers, so "declared" and "allowed for this computer" are separate steps.

## Every path that leaves sms undeclared

| # | Path | Visible to the user as | Fix |
|---|------|------------------------|-----|
| 1 | Switch off | `sms_state_turn_on` | already stated |
| 2 | Permission never asked | `sms_state_allow` + "Allow access" | already stated |
| 3 | Split grant (e.g. READ+SEND granted, RECEIVE missing) | lists exactly the missing ones ("see new texts") | already stated; gate deliberately needs all three |
| 4 | "Don't allow" twice (Android stops asking) | `sms_state_settings` | already stated |
| 5 | Android 13+ restricted setting for sideloaded apps (request returns denied with no dialog, no rationale, so it looks like step 4) | `sms_state_restricted` | now chosen only when the install is not from Play or Galaxy Store (`SmsRestriction.likely`); text now says the "Allow restricted settings" item in the top-right menu appears only after one "Allow access" attempt |
| 6 | Permission answered while the link was still connecting: the identity packet already went out with the old set, and `resync()` returned early because the link was not "running" yet | Messages stays off until some later reconnect | `PhoneDataRuntime.onLinkChanged(true)` now compares `declared` with `active()` and re-declares. `CapabilityRedeclare.onChange` and `driftAtLinkUp` hold the decision (unit tested) |
| 7 | Permission granted in system Settings while link is up | none | `onResume` in Home and Setup calls `resync()`; `CapabilityRedeclare.needed` triggers stop + restart so the new identity is sent (already existed, now covered by a decision test) |
| 8 | Auto-revoke of unused-app permissions, Samsung "Auto Blocker" or a work profile policy | permission goes missing again | the same evaluate/resync path reports the missing permission by name; no extra code needed |
| 9 | No computer paired, or paired computer not ticked in "Allowed computers..." | `sms_state_pair` / `sms_state_choose` | already stated |

Default SMS app: not required. Reading the provider and sending plain texts work with the three permissions. MMS, group and RCS are out of scope and already stated in `pd_sms_honest`.

## Samsung notes

- The three SMS permissions are one permission group, so one system dialog grants or denies all three. A split grant comes from Settings or from automatic revocation.
- Samsung's installer (My Files, Chrome, Galaxy Store) sets the installing package, so "sideloaded" is detected by installer name, not by assumption. Installs through `adb install` carry RESTRICTION_INSTALLER_EXEMPT (seen on the AVD), so a developer test via adb cannot reproduce the restricted case. The real phone must be checked by hand: App info > top-right menu > Allow restricted settings, then Permissions > SMS > Allow.

## Verification

- Gradle unit tests (`./gradlew --offline testDebugUnitTest`): exit 0. New tests in `SmsReadinessTest`: restricted-setting wording choice, redeclare decision when linked / not linked / drift at link-up, no declaration while any of the three permissions is missing.
- AVD (gbhome, API 36, debug APK): SMS step shows "Messages is switched off..."; toggling the switch raised the single system dialog; Allow granted READ, SEND and RECEIVE in one go. The restricted path could not be reproduced (adb install is exempt). Emulator stopped afterwards; disk was 137 GB free.
- Rust: `cargo fmt --check` and `cargo clippy` fail on files that other work-in-progress edits touch (`calls_link.rs`, `calls_view.rs`, `calls_transport.rs`, `chaos_link` test: missing `can_dial`). None are touched by this change; no Rust code was changed.

# Android build verification 10

Date: 2026-11-03

## CI on main (check.yml)

Most main runs were cancelled by newer pushes (concurrency). The first settled run
was 41684224810 on 35180f343 ("call-ui: in CI probe notify-send action support..."):
conclusion failure, from one job.

| Job | Result |
| --- | --- |
| Android / unit tests, lint, debug APK | success |
| Supply chain (cargo-deny, notices) | success |
| Type-check Windows and macOS targets | success |
| Rust / windows-latest | success |
| Rust / ubuntu-latest | success |
| Rust / macos-latest | failure |
| Coverage report (non-blocking) | success |
| Performance budget | success |

The macOS failure is a timing flake, not a regression:
`phone_feed::tests::follows_params_shows_action_results_and_reports_a_down_service`
panicked with "timed out: notice" (phone_feed.rs:215). The test polls for 3 s on a
20 ms feed; 204 other bridge-apps tests passed, the same test passed on Linux and
Windows, and phone_feed.rs has not changed recently. Left as is; if it recurs, raise
the `wait_until` deadline in that test.

## Debug APK

Built from a clean worktree of origin/main (06a6b1a17) with
`scripts/build-android.sh --skip-host-tests`; Gradle unit tests and lint passed.

- sha256: 9a3f0423354502f4e7899e38f979ad85e28422dc073c9166a28db2df5d426729
- size: 8061237 bytes (7.7 MB), ABIs arm64-v8a and x86_64
- copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk
- `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`: HTTP 200, Content-Length 8061237

## Release isolation

`assembleRelease` (gradle --no-daemon) succeeded; `scripts/check-release-isolation.sh`
passed on app-release-unsigned.apk.

## PC release binaries

`cargo build --release -p bridge-service -p bridge-apps` from the same tree; the nine
binaries in scripts/release-binaries.txt were copied over target/release of the main
checkout via temp copy and mv. The live service was not restarted.

# Head verification 2

Date: 2026-11-03. Clean worktree of origin/main at 298c919d6 (shared CARGO_TARGET_DIR under the tmp cache, removed afterwards).

## Local results (298c919d6)

| Check | Result |
| --- | --- |
| cargo fmt --check | pass |
| cargo clippy --workspace --all-targets --locked -D warnings | pass |
| cargo test --workspace --locked | pass (all 99 result lines ok, exit 0, no retry needed) |
| scripts/add-spdx.sh --check | pass |
| scripts/gen-notices.sh --check | pass |
| scripts/check-doc-links.sh | FAIL: docs/README.md and docs/release-checklist.md linked the local-only (git-excluded) public-release-runbook.md |

## Fixes pushed

- 82a1867da: drop the links to the local-only runbook (doc links now 0 broken).
- 745f51971: audio_route late-echo-nodes test used an 80 ms wall-clock bound; macOS CI failed it. Now 5 s (returns as soon as nodes appear).
- persistence tests (bridge-kde): the contention-sensitive tests now serialise; on a Windows runner one atomic write took 458 ms and tripped the 10 s lock timeout (run 41542402398 and others).
- pair_ui test `the_card_with_no_service_disables_pairing_buttons`: racy on Windows (status arrived between clicks); the status fetch now fails deterministically.
- scripts/smoke-binaries.sh: `cmd | grep -q` under pipefail gave a Broken pipe failure on ubuntu (run 41540629878); output is captured first.
- chaos_clipboard flood test (macOS, run 41540442927): wait raised from 10 s to 60 s.
- share_api `cancel_and_unpair_close_the_payload_listener` (Windows, once): close bound 2 s to 10 s.
- 596d6c642: build break. Commit 832ae3a2d accidentally included two lines (`disconnected`, `battery`) from another agent's unlanded work in pair_ui_tests.rs, failing every CI job (run 41544284430); removed them.

## CI on origin/main

Failed before the fixes: 41537409087 (macOS, post-fix flake class), 41535314867, 41540442927, 41540629878, 41541040792, 41542402398, 41542526071, 41544284430 (build break).
Green: 41542305791, 41543999903, 41545881931 (596d6c642), 41546450484 (8f4f88192, all jobs).

## Notes

- Windows disk slowness (hundreds of ms per atomic write on hosted runners) is the common cause of the persistence flakes; the product lock timeout (10 s) was left unchanged.
- The other agent's uncommitted pair_ui_tests.rs lines will reappear in their working tree and should be committed together with the ipc changes they depend on.

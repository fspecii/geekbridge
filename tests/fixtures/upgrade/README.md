State directories written by real OLD builds of the desktop service, used by `scripts/e2e/upgrade-path.sh`
(`docs/reports/upgrade-path.md`, `docs/state-formats.md`). Each is what `--state-dir` held after a stand-in phone
paired over loopback, three consents were given, six text messages were pushed, and the switches the build had were
changed. Nothing here belongs to a real person or device.

| Directory | Built from | Notable content |
|---|---|---|
| `v0.1.0-rc.1/` | tag `v0.1.0-rc.1` | no `privacy` file (the `sms-notify-content` marker is the only switch), no `call-replies`, `update-state.json` without `backoff_until`, `last_attempt_unix` or `last_install`, mode 0644 (it was written by `PlainIo`) |
| `37af23dba/` | commit `37af23dba` (round 9) | `privacy` with `respect_dnd`, `call-replies` (two lines), `update-state.json` with `backoff_until` and `last_attempt_unix` set to null |

`identity.pem` holds a **throwaway** key made for these fixtures; it protects nothing. Secret scanners may flag it.
The control token and lock files are left out (the service makes them). Git does not keep file modes: the script
`chmod 600`s its copy first, as the service insists on owner-only files.

Refresh (needs the old builds, see the script header):

    scripts/e2e/upgrade-path.sh --bin-dir DIR --phone-example target/release/examples/upgrade-phone \
        --old-bin rc1=OLD_RC1_BIN_DIR --old-bin r9=OLD_R9_BIN_DIR --capture /scratch/captured

then copy `/scratch/captured/rc1` and `/scratch/captured/r9` over the directories above. Never change a fixture to make
a failing run pass: a failure means a state file written by a shipped version no longer loads.

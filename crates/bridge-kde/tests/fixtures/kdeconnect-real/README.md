# Packets captured from a real KDE Connect desktop daemon

Captured 2026-11-02 from **kdeconnectd 26.08.2** (Arch Linux package `kdeconnect` 26.08.2-1, Qt 6.12)
talking to GeekBridge over loopback / a dummy private address inside a throw-away network namespace,
by `scripts/interop-kdeconnect.sh`; exported by `scripts/interop-kdeconnect-export.py`.

* `daemon-*.json`: one packet line each, exactly the bytes the daemon wrote (after TLS decryption),
  including the trailing LF.
* `geekbridge-*.json`: lines GeekBridge wrote that the daemon accepted (the daemon acted on them or,
  for `geekbridge-ext-*`, logged `discarding unsupported packet` and kept the link).
* `daemon.crt.pem`, `geekbridge.crt.pem`: the two **public** certificates of that run. Both
  identities are random, single-use and exist nowhere else. No private key is stored.
* `manifest.json`: file list plus the verification keys the daemon displayed over D-Bus
  (`verificationKey`) for the two pairings, which `tests/kdeconnect_real.rs` reproduces.

Sanitising: the only change to any line is the machine hostname in `deviceName`, replaced by
`interop-daemon`. Nothing else is edited, so byte-for-byte re-serialisation can be asserted.

Regenerate: see `docs/kdeconnect-interop.md` ("Reproducing"). A regenerated set has new device ids
and certificates; the tests derive everything from the files, so they keep passing.

# Packets captured from the REAL stock KDE Connect Android app

Captured 2026-11-02 from **KDE Connect 1.37.0 (versionCode 13700)**, F-Droid build of
`org.kde.kdeconnect_tp` (APK SHA-256 `a51c6e85585748b44b42fb409229effb333f7758e5e7cbfff2f480a62530dd01`,
signer certificate SHA-256 `79b50031486746dffbc746f48fb0eb62fc7bb0ae7b976e7d6507b2c078840d92`, both equal to
the published F-Droid index), running in an Android 36 emulator, talking to `geekbridge-service` on the
host (see `docs/kdeconnect-interop.md`, "Stock Android app").

* `android-*.json`: one packet line each, exactly what the app wrote (after TLS decryption), with the
  trailing LF. Key order is the app's own (`id`, `type`, `body`), not alphabetical, so these are checked
  semantically, not byte-for-byte.
* `geekbridge-*.json`: lines GeekBridge wrote that the app acted on (the phone stored the SMS, answered
  the contacts request, ...).
* `android-udp-identity.json`: the UDP identity datagram the app sends (manual "Add devices by IP").

Everything is synthetic: the emulator's device id and name (`emulator`), a made-up contact and SMS.
Nothing is edited. No certificate or key is stored.

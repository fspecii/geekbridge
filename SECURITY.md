# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue:

- Use GitHub's **Report a vulnerability** (Security tab, private advisory) on this repository.
- If that is unavailable, contact the maintainer at the address on their GitHub profile. <!-- OWNER: add a monitored security contact; see docs/release-checklist.md -->

Include the affected version (`geekbridge-ctl diagnose` and the Android **Copy report** contain no personal data and
are safe to attach), the platform, and steps to reproduce. Do not include pairing keys, the control token file or
real personal data.

You can expect an acknowledgement within 7 days and a status update within 30 days. Fixes ship in a new release;
the advisory credits the reporter unless they prefer not to be named.

## Supported versions

Only the latest released version receives fixes while the project is pre-1.0.

## Scope and design notes

- Phone and computer authenticate with pinned TLS certificates after a code comparison on both screens; discovery
  alone never grants trust.
- QR / link pairing pins the computer's certificate before anything secret is sent, uses a 128-bit token that works
  once for two minutes, and still needs an explicit tap on the phone (see [docs/pairing.md](docs/pairing.md)).
- The service now listens for phones (TCP 1716-1764) and announces itself on the LAN by default so pairing is easy; both
  can be switched off (`--no-listen-inbound`, `--no-announce`). An unpaired connection can only pair or be closed.
- The local control API is bound to loopback (default port 17381; the read-only discovery API is 17380) and needs a per-user
  token stored with owner-only permissions.
- Clipboard, file sharing, contacts, calendar, SMS, battery level, phone notifications (only from apps the user picks) and
  call control including dialing are each off until the user opts in per phone, on both the phone and the computer;
  nothing is read or sent in the background. Phone content is kept in memory only. Dialing from the computer is also
  rate limited on the phone (3 per 10 minutes) with a visible notice and an optional on-phone confirmation.
- Updates are only offered with a published SHA-256, verified before the file is shown; nothing installs itself. The
  update check to GitHub is the only internet traffic and can be turned off (`geekbridge-ctl update disable`).
- Windows and macOS builds are unsigned and have not been run on real systems yet.

The internal review is in [docs/reports/security-review.md](docs/reports/security-review.md).

In scope: pairing and TLS handling, the control API, file/clipboard handling, update verification, packaging
scripts. Out of scope: attacks that need root/administrator or physical access to an unlocked device, and denial of
service by a device already on your network.

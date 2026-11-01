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
- The local control API is bound to loopback and needs a per-user token stored with owner-only permissions.
- Clipboard and file sharing are off until the user opts in per phone; nothing is read or sent in the background.
- Updates are only offered with a published SHA-256, verified before the file is shown; nothing installs itself.
- Windows and macOS builds are unsigned and have not been run on real systems yet.

The internal review is in [docs/security-review.md](docs/security-review.md).

In scope: pairing and TLS handling, the control API, file/clipboard handling, update verification, packaging
scripts. Out of scope: attacks that need root/administrator or physical access to an unlocked device, and denial of
service by a device already on your network.

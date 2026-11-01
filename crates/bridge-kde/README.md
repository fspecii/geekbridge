# bridge-kde

KDE Connect protocol 8 LAN transport (TCP + TLS) with explicit, certificate-pinned pairing, plus the
`geekbridge-pair` developer CLI. See `docs/kdeconnect-interop.md` for
the interoperability results and what has not been tested on real hardware.

Provenance: the code is an independent implementation written from observed protocol behaviour. Only
wire-visible facts (port numbers, field names, state names, the verification-key recipe) are used; no
upstream implementation body or test source is copied. Test fixtures under `tests/fixtures/` are packets
captured from stock KDE Connect builds (see the README there). Licence: GPL-3.0-or-later.

#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Guards the emulator e2e scripts against the mistake that made the phone declare no Messages: the phone offers a
# feature only with the master switch (geekbridge_phonedata.xml) AND the per-computer grant (geekbridge_grants.xml).
# A script that turns a switch on by writing the preference file must write the grants file too.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
fail=0
for f in scripts/e2e-phonedata.sh scripts/e2e/*.sh scripts/*.sh; do
  [[ -f "$f" ]] || continue
  if grep -q 'shared_prefs/geekbridge_phonedata.xml' "$f" && grep -Eq 'name="(sms|contacts|calendar)" value="(true|\$)' "$f"; then
    if ! grep -q 'geekbridge_grants.xml' "$f"; then
      echo "FAIL: $f switches a phone-data feature on but never writes geekbridge_grants.xml" >&2
      fail=1
    fi
  fi
done
for f in scripts/e2e/sms.sh scripts/e2e-phonedata.sh; do
  bash -n "$f" || { echo "FAIL: $f does not parse" >&2; fail=1; }
done
# The SMS e2e must be wired into the nightly workflow, non-blocking.
grep -Eq "name: sms, run: 'scripts/e2e/sms.sh'.*experimental: true" .github/workflows/emulator-e2e.yml \
  || { echo "FAIL: the sms suite is not in .github/workflows/emulator-e2e.yml as a non-blocking suite" >&2; fail=1; }
((fail)) || echo "e2e grants test: ok"
exit "$fail"

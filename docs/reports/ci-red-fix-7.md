# CI red fix 7

Run 41766609345 (b5633caee) failed three jobs.

* Rust / windows-latest and Type-check Windows and macOS: `unused variable: file` in the
  `choice_is_validated_saved_private_and_reloaded` test of `crates/bridge-service/src/call_audio.rs`
  (only used inside a `#[cfg(unix)]` block). Moved the binding into that block.
* Performance budget: service 6 138 064 B (budget 6.1 MB), ctl 1 450 704 B (budget 1.4 MB). A local release build
  gives 6 128 408 B and 1 450 272 B. Unstripped ctl symbols show no accidentally linked heavy dependency (no image, rqrr,
  opus or camera code); growth is serde code for the new wire types (call-audio `Setup`, pairing, Calls window) and
  `call_cli`. Legitimate feature growth, so budgets are raised to service 6.2 MB and ctl 1.5 MB, documented in
  `docs/performance.md` and `scripts/perf-check.sh`.

## Follow-up: idle RSS over budget after 4c196e52b

The next head (call-audio routing on the computer's headset, 4c196e52b) kept the size checks green (that commit raised
them to 6.7 MB and 1.55 MB itself, merged with this fix) but the performance job then failed on idle RSS: 9692 kB on
CI against 8000 kB (7720 kB before, reproduced locally: 9280 vs 7720). Cause: the service now links libpulse, which pulls
16 more shared objects into the process at start (`ldd` 5 -> 21). Not a leak or a new thread. RSS budgets raised to
9500 kB (local) and 10500 kB (CI) with the explanation in `docs/performance.md`; the proper fix (lazy `dlopen` of libpulse)
is left to the call-audio work.

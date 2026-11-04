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

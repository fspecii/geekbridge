# Shared i18n catalogue (2026-11-03)

Closes item 2 of `docs/reports/i18n-sweep-2.md`: desktop banner and tray text was English literals in `bridge-service`.

## What changed

* New crate `crates/bridge-i18n` with no dependencies. `i18n.rs` moved there unchanged (`git mv`, history kept) as
  `src/lib.rs`, and `i18n/en.properties` moved with it. `bridge-apps` re-exports it (`pub use bridge_i18n as i18n;`), so
  every `crate::i18n::...` and `bridge_apps::i18n::...` path still works. `bridge-service` depends on it directly.
* Tests always see English: `current()` answers English under `cfg(test)` of the crate itself and under the new
  `force-english` feature, which the dev-dependencies of `bridge-apps` and `bridge-service` switch on (dev-only, so no
  release binary is affected).
* New keys `banner.*` (23) and `tray.*` (22) in `en.properties`; the service code in `os_notify.rs`, `desktop_notify.rs`,
  `mirror.rs` and `tray/model.rs` uses `tr`, `tr_args`, `tr_n`. Phone-chosen values are still passed through
  `isolate()` before they fill a placeholder, so a translator can reorder the sentence and the names stay intact. "Received
  text" / "Received a link", "Now: x" / "In n min: x" and the connected/paused lines are whole sentences, not joined pieces.
* Banner button labels (`Open`, `Show in folder`, `Update`, `Dismiss`) use the same catalogue.
* Isolation tests that were missing: `mirror::tests::notify_send_body_isolates_title_and_text_in_order` (freedesktop
  body) and `os_notify::tests::mirrored_body_keeps_title_and_text_in_order_with_each_isolated` (Windows/macOS body, now
  built by the small `mirror_body` function so it can be tested). Both use an Arabic title and a right-to-left override
  in the text.
* Literal guard (`crates/bridge-apps/tests/i18n_catalogue.rs`) now also scans `bridge-service/src` for the key-usage,
  concatenation and unused-key checks, and holds `os_notify.rs`, `desktop_notify.rs`, `mirror.rs`, `tray/model.rs` to
  zero visible literals (no allowlist entry). Technical strings carry `// i18n-ok`. The apps allowlist did not grow.
* Docs: `docs/translating.md`, `docs/translations.md`, `CONTRIBUTING.md` point at the new paths.

## Size

`bridge-i18n` has no dependencies. The service links the catalogue (about 17 KB of English text and the loader; release build: service 5 862 600 B (budget 6 000 000), ctl 1 250 512 B (budget 1 300 000), ctl contains no catalogue text);
`geekbridge-ctl` does not call it, so the linker drops it (it uses only `notify_text`). Budgets are checked by the
`Performance budget` CI job (service 6.0 MB, ctl 1.3 MB in `scripts/perf-check.sh`).

## Not done

* `bridge_core::why_disconnected::one_line` and the battery text in the tray status line come from `bridge-core` and are
  still English; moving them needs `bridge-core` to depend on `bridge-i18n` (tiny, no dependencies), left for a follow-up.
* The Windows tray tooltip joiner (`GeekBridge - title: body`) and the `geekbridge-ctl --notify` clipboard line are not moved.
* No translation exists yet (`BUNDLED` is empty); nothing visible changes in English.

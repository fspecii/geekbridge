# Phone data at realistic scale

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

A real phone holds thousands of contacts, years of calendar events (series with hundreds of instances) and tens of
thousands of texts. This report covers what was measured at that size, what broke, what was fixed, and the caps that
are now enforced and shown to the user. Re-run it with the commands in "Repeating this".

## Test data

| | Emulator (real providers, real link) | Rust / JVM generators |
| --- | --- | --- |
| Contacts | 5,000, names with accents / CJK / emoji, 1-3 numbers, e-mail, JPEG photo on every 5th | 5,000 (`GEEKBRIDGE_SCALE=1`), 500 by default; vCards with stray photos |
| Calendar | 8 calendars of 3 accounts, 20,026 events: 6 daily + 20 weekly series over 5 years, all-day, multi-day, six time zones | up to 20,000 provider rows, 1,000 per sync |
| SMS | 1,640 texts over 800 threads | 20,000 texts over 800 threads (skewed, a few chatty threads), 12 threads x 2,500 for the caps |

Android 36 only lets the default SMS app write the SMS provider (`content insert` returns silently without writing), so
the emulator holds 1,640 texts sent through the modem (`adb emu sms send`, about 7 per second); the 20,000-text load runs
through `PhoneData` (desktop) and `SmsPaging` / `PhoneDataEngine` (phone logic) in tests instead. Contacts and
calendar are written by the debug-only `ScaleSeedReceiver`.

## Numbers

Emulator: x86_64, API 36, software GPU (so UI timings there are not meaningful; frame costs below come from
headless egui runs). Desktop service: release build on a 24-core host.

| Measure | Result |
| --- | --- |
| Contacts: first usable list | 0.25 s (first 25-vCard batch) |
| Contacts: all 5,000 | 22.7 s (200 round trips of 25), phone CPU 3.8 s, about 3.0 MB on the link (photos stripped on the phone) |
| Calendar: first events | 0.5 s; full sync 1,000 events (the cap), 254 KB packet, phone CPU 0.4 s |
| Calendar: one event changes | 710 B delta (vs 254 KB full) in the JVM test; on the emulator the change reaches the desktop at the next push (at most one per 30 s) |
| SMS: first conversations | 0.25 s; 300 threads listed in 3 packets, 100 shown, "of 827" said so |
| Phone memory (PSS) | 58-64 MB idle, 66-71 MB peak during contacts / calendar sync |
| Desktop service RSS | 15 MB idle, 28 MB with 5,000 contacts + 1,000 events + 300 threads |
| Desktop hub work (release, `cargo test --test scale`) | contacts sync ingest 35 ms, view 4.5 ms, search 3.5 ms; calendar apply 4.6 ms, view 0.2 ms; conversation list 1 ms, 2,000-message thread 0.6 ms, search 0.9 ms; 30,000-message flood ingest 137 ms |
| UI frame (headless, release) | Contacts, 1,000 rows: 0.23 ms (0.96 ms when every row is laid out); thread with 2,000 messages: 0.14 ms (6.4 ms without virtualising) |

Emulator battery and jank figures are in `dist/android/logs/scale-measure.txt` and are only indicative (the
emulator reports the foreground service's wall time as power, and software rendering janks any app).

## Defects found and fixed

1. **Contacts sync stalled after about 550 contacts.** The phone's data-send limiter allowed a burst of 20 and 2 per
   second; a 5,000-contact sync is 200 answers, the first refused one was dropped, and the desktop waited forever.
   Now: 8 per second sustained, the phone retries a refused answer for about 3 s before backing off, and the desktop
   asks again for a vCard batch nobody answered in 15 s.
2. **Calendar kept the wrong events.** The provider returns instances ordered by start, so a long window with more
   than the cap handed back the far past and the "nearest first" cut never saw today. The phone now reads forward and
   backward from now (nearest first on each side), one cursor each, stops at the cap, and flags truncation.
   The window is the last 12 months to the next 24 months.
3. **Conversation list missed threads.** The newest 2,000 rows were all it looked at, so a few chatty threads hid the
   rest. The phone now pages newest-first by (date, id) until it has 300 threads or has scanned 20,000 rows.
4. **Only the newest 100 messages of a thread could ever be seen**, however much history was loaded. The thread view now
   takes a `message_limit` (up to 2,000, cut to a 150 KB byte budget so a reply still fits one response); "Load older
   messages" shows what is already held before asking the phone.
5. **Sorting.** Contacts sorted by lower-cased code point: "Émile" after "Zoë", "Ñandú" after "z", and every CJK name
   its own rail letter. `bridge_core::collate` folds Latin accents and ligatures, groups symbols / digits first and other
   scripts last (code point order; reading-based CJK order needs a dictionary the app does not ship) and drives both
   the order and the rail (`A`-`Z`, `#`, `…`). Search ignores accents too ("emile" finds "Émile").
6. **UI work per frame.** The Contacts list laid out every row each frame and the thread laid out every bubble; both now
   lay out only what is on screen (fixed row heights; measured bubble heights, cached).
7. **Silent truncation.** Nothing said when data was cut. Now: contacts "Showing N of M. Search to narrow down." (existing)
   plus "The phone shares at most 5,000 contacts"; calendar "Showing the events nearest today (up to 12 months back and 24
   months ahead)"; messages "Showing the newest 100 of 827 conversations" and "Showing the newest N of M messages loaded".
8. **Provider reads could not be cancelled.** Every read now has a `CancellationSignal`, and dropping the link cancels the
   read in flight. All reads already ran off the main thread (the link waiter / `geekbridge-data-send` workers);
   `QueryCancel` is shared by the three ports.
9. **One provider query per vCard.** The timestamp is now taken from the listing instead of queried again per contact.
10. **Per-poll cost of the conversation list** sorted every thread to find its last message; `Conversation::newest`
    is O(n) without allocating.

Already correct and now covered by tests: duplicate instances (one row per calendar / event / start), recurring
series (the phone expands them, the desktop never expands), per-thread and per-phone caps, packet limits.

## Documented caps

Per paired phone, enforced on the phone and again on the desktop:

| What | Cap | Shown when cut |
| --- | --- | --- |
| Contacts | 5,000 listed, 25 vCards per request, 64 KiB per vCard (photos stripped); view 300 (up to 1,000) rows within 90 KiB | "Showing N of M", "at most 5,000" |
| Calendars | 32 | |
| Events | window -12 / +24 months, 1,000 per sync nearest today, 500 `deleted` per delta; 400 shown per view within 110 KiB | "Showing the events nearest today ..." |
| Conversations | 300 held, 100 shown | "Showing the newest 100 of 827 conversations" |
| Messages | 2,000 per conversation, 20,000 per phone (oldest dropped, biggest thread first), 50 per history page, 100 per packet, 2,000 shown (default 100) within 150 KiB | "Showing the latest N messages ... Older ones stay on the phone", "Showing the newest N of M messages loaded" |
| Packets | 900 KiB phone to desktop, 256 KiB per IPC response | |
| Link rate | burst 20, 8 per second | |

## Cancelling

Leaving a window stops its polling (the feed thread is dropped with the window), so no new request is made. There is
no wire message to cancel a read already running on the phone; a read ends by itself within its caps (the longest are
the contacts listing and the calendar window, a few hundred ms on the emulator) and is cancelled at once when the link
drops. A newer contacts listing supersedes any vCard batches still queued on the desktop.

## Repeating this

```
cargo test -p bridge-service --test scale --release -- --nocapture                      # small sizes
GEEKBRIDGE_SCALE=1 cargo test -p bridge-service --test scale --release -- --nocapture   # 5,000 / 1,000 / 20,000, with budgets
GEEKBRIDGE_SCALE=1 cargo test -p bridge-apps --release frame_cost -- --nocapture        # UI frame cost
(cd android && gradle testDebugUnitTest --tests 'dev.geekbridge.android.ScaleTest')    # query / paging logic
scripts/scale-e2e.sh                                                                    # emulator: seed, pair, measure
```

`scripts/scale-e2e.sh` needs its own AVD (`gb36z`, a copy of any `gb36*`), uses console port 5700 and adb server port
16150, and writes `dist/android/logs/scale-results.tsv` and `scale-measure.txt`.

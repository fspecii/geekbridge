# GeekBridge static performance / memory review

Scope: read-only review of the main checkout working tree (`/home/ambsd/.t3/projects/geekbridge`), hot paths of bridge-service, bridge-kde, bridge-core (state files) and bridge-apps.

## Method and limits

- Bash failed in this session with EDQUOT on `/tmp/claude-1000`, and no grep/glob was available. Every file was opened by path with Read.
- **Nothing was measured.** Every number below is an estimate from the code, labelled as such. Please confirm the top items with a profile before changing code.
- Files read in full or in the relevant part:
  - bridge-kde: `payload.rs`, `packet.rs`, `link.rs`, `store.rs`.
  - bridge-service: `daemon.rs`, `phone_data.rs`, `lib.rs`, `main.rs`, `privacy.rs`, `private.rs`, `coexist.rs`, `lan_select.rs` (system_interfaces), `clipboard.rs` (backend, service core and receive paths), `share.rs` (header only).
  - bridge-apps: `apps/messages.rs`, `phone_feed.rs`, `photos_pane.rs`, `shell.rs`, `pace.rs`, `poller.rs`, `connection.rs` (head), `memtrim.rs`, `script_fonts.rs`, `localtime.rs` (relevant parts), `theme.rs` (head).
- **Not examined** (no findings, not cleared): `reconnect.rs`, `mirror.rs`, service `photos.rs`, `callwifi.rs`, `call_control.rs`, `call_audio*`, `bridge_core::statefile`, `bridge_core::heartbeat`, the `share.rs` receive/send task bodies, `contacts.rs`, `calendar.rs`, `calls.rs`, `home_app.rs`, `bridge-core` JSON parsers for the phone-data packets (`sms::parse_messages`, `phone_link`).

## Already good (do not "fix")

- Payload sockets (`bridge-kde/src/payload.rs`): fixed 64 KiB chunk (`CHUNK`, line 37), one buffer per transfer, per-chunk idle timeout, hard byte cap. No per-packet allocation. The only per-transfer cost is building a rustls config (lines 291 and 378), which is small.
- The service runs on 2 tokio workers and a single malloc arena (`main.rs`). The apps trim malloc at frames 3 and 90 and when focus is lost (`memtrim.rs`), and cap the egui atlas width at 2048 px.
- No GUI worker calls `request_repaint` unless its data changed. `Poller`, `Feed`, `Worker` and the photos worker all compare before `wake()`. All poll waits go through `pace::scaled`, which multiplies by 4 when unfocused and by 2 when no phone is paired. I found no idle repaint loop in Messages.
- The bubble list is virtualised (`thread_bubbles`, `messages.rs:1627`). Heights are cached, and `request_repaint` is only called when a measured height changes.
- Service-side bounds all exist: `EVENT_CAP` 256, `MAX_THREADS` 300, `MAX_MESSAGES_PER_PHONE` 20 000 and `MAX_MESSAGES_PER_THREAD` 2000, `MAX_PENDING_VCARDS`, `SEND_HISTORY` 200, `notified_at` 64, `SourceLimiter` 256 sources, `MAX_LINKS` 16. App-side: drafts 50, `sent_replies` 16, photo textures pruned to the live set, `handled` 64.

## Ranked findings

Rank is by expected effect on idle RSS/CPU or throughput, highest first.

### 1. Unchanged Messages poll does the full work every 1.5 s (service CPU, client CPU and RSS churn)

- Where: `phone_data.rs:1597-1757` (`messages_view_window`) served from `daemon.rs:1138`. The client polls from `phone_feed.rs:228-244` with a 1500 ms interval (6 s when unfocused).
- Scenario: Messages window open and idle, one phone with several thousand contacts and a few thousand messages.
- Cost per poll, all under the single `PhoneData` std `Mutex` (`lock()` at line 1613), even when nothing changed:
  - `name_index(d)` (line 1887, called at 1633): rebuilds a `HashMap<String,String>` over every contact phone number. Each `suffix()` allocates two `String`s, so roughly 2N+ allocations for N phone numbers.
  - `Self::devices(&g)` (1457): `contacts.live().count()` and `events.live().count()` walk both stores.
  - `t.unread()` (line 207, called at 1686): scans every message of every thread.
  - `conv.ordered()` for the selected thread.
  - With a search term (1649-1659): `ordered()` plus `to_lowercase()` of every body in every thread, up to 20k messages.
  - Then serialise up to about 256 KiB of JSON (messages budget 150 KiB). The client parses it and the feed worker does a deep `*g != next` compare (`phone_feed.rs:121`).
- Why it matters beyond CPU:
  - The long `std::Mutex` hold blocks `on_packet`, `caps`, `send_stock`, `unread_total` and the tray poke path. These run on the async link tasks, and with only 2 workers a 5-30 ms hold stalls link reads. This is an estimate, not a measurement.
  - It creates steady allocation churn in the service, which the single-arena setup turns into RSS retention.
- Minimal fix, in order of value:
  1. Add a `u64 generation` to `Dev`, bumped on every mutation (`on_messages`, `mark_read`, `set_enabled`, send ledger changes, `link_up`/`link_down`).
  2. Let the client send `since_generation`. The server answers a tiny `Unchanged` response, skipping the work and the JSON. The `Feed` worker then keeps its old `Arc` view.
  3. Cache `name_index` per device in `Dev` (`Option<Arc<HashMap>>`), invalidated in `on_vcards`, `on_uids`, `clear(CONTACTS)` and when the consent changes.
  4. Cache `Thread::unread` and update it in `merge`/`mark_read`.
- Same pattern, smaller: `agenda_view_range` (`phone_data.rs:1498`) is polled every 2 s by the Agenda and Contacts feeds (`phone_feed.rs:17`, `POLL`).

### 2. Messages window deep-clones the whole view four or more times per frame (app CPU and transient RSS)

- Where:
  - `Feed::state()` clones the `FeedState`, including the `MessagesView`, under the lock (`phone_feed.rs:160-162`).
  - `Messages::view()` (`messages.rs:404-407`) then clones `state.view` again.
  - `view()` is called from: `show` (1837), `sidebar` (350), `compose` (1091 and 1134), `device` (1321), plus `Feed::state()` directly in `conversations_empty` (835), `candidates_now` (926) and `contact_list` (949).
- Cost: on every redrawn frame (hover, scroll, typing, so not idle) each call copies up to 100 conversations, 20 sends and up to 2000 message bodies (a few hundred KiB). Estimate: more than 1 MB of alloc/copy/free per frame. This is the main source of transient RSS spikes in the 66 MB / 75 MB budget discussion and of glibc arena fragmentation.
- Minimal fix: store `Arc<FeedState<V>>` (or `Arc<V>` for the view) in the shared mutex. `state()` returns an Arc clone (a refcount bump) and `Messages::view()` returns the Arc. All callers only read. The worker already builds a fresh state each publish.
- Related, smaller (see 10): `bubble()` does `m.body.clone()` per visible bubble per frame (`messages.rs:1757`).

### 3. Every app process polls status roughly 2-3 times per second, and each poll hits disk twice (service CPU, wakeups)

- Per app process there are three polling threads:
  - discovery, 2 s (`poller.rs:7`);
  - link-control `Status`, 1 s (`connection.rs:209`), plus `Events` when the sequence moved;
  - phone feed, 1.5-2 s.
- Each request opens a TCP connection. `Config::token()` re-reads the token file with `read_private_file` on every request (`connection.rs:99-116`).
- Server side, `Daemon::status()` (`daemon.rs:723`) calls `store.list()`. `FileTrustStore::refresh` (`store.rs:223-232`) opens and reads `trusted_peers.json` every call, under the store mutex, and only then compares bytes. `store.get()` does the same (`store.rs:314`).
- `why_disconnected` (`daemon.rs:791-844`) calls `lan_select::system_interfaces()`, i.e. `getifaddrs` (a netlink dump), on every status call while any paired phone is not connected.
- With five apps focused that is about 5 status/s, or about 10 file reads/s, plus getifaddrs when the phone is offline. They are x4 slower when unfocused.
- Minimal fix:
  - Cache the token in `Config` (`OnceLock<Token>`, re-read only after an auth failure).
  - In `FileTrustStore::refresh`, check `(mtime, len, inode)` via `metadata()` first and skip the read when unchanged, or cache for about 1 s.
  - Cache `system_interfaces()` for about 5 s.
  - Better: add a cheap `Request::Pulse` returning only `event_seq` plus a generation, and fetch the full status only when it moves. Alternatively make link-control 2 s when focused.

### 4. Large packet lines pin memory per link for the life of the connection (service RSS)

- Where: `LineReader` in `bridge-kde/src/packet.rs:177-245`.
- `buf` grows by doubling to the longest line seen (the daemon allows 2 MiB, `daemon.rs:73`) and is never shrunk (`drain` keeps capacity). Estimate: up to about 2-4 MiB retained per connected phone after one big conversation list or vCard batch.
- `from_line` builds a full `serde_json::Value` tree (`packet.rs:66`), typically several times the line size. The phone-data parsers then walk the `Map` again. A 1 MiB line therefore costs several MiB transient, then fragmentation.
- `to_line` deep-clones the body (`body.clone()`, `packet.rs:50`) for every send. Replace it with a borrowed `#[derive(Serialize)] struct Wire<'a>`.
- The read chunk is 8 KiB (`packet.rs:232`), so there are 128+ reads per MiB; a 16 KiB chunk matches the TLS record size.
- Minimal fix:
  - When `buf` is empty after a line and `capacity() > 64 KiB`, call `buf.shrink_to(16 KiB)`.
  - Use 16 KiB chunks.
  - Serialise `Packet` without cloning.
  - Consider a periodic `malloc_trim` in the service after a line over 256 KiB (the service trims only at startup; I did not check `bridge_service::memtrim` beyond its name).

### 5. Clipboard tool runner: process plus extra thread per call, byte-wise copy, 30 s safety poll (service wakeups and spikes)

- Where: `clipboard.rs:367-435` (`run_tool`), `64` (`EVENT_SAFETY_POLL`), `562-576` (`sensitivity`).
- Each tool call spawns a process and also an OS thread just to read stdout (402-406). It then busy-waits `try_wait` with 5 ms sleeps (422).
- On Wayland with a clipboard-enabled phone, the safety poll runs about every 30 s and each observation costs `wl-paste --type text` plus a second spawn for `sensitivity()` (`--list-types`). The two spawns plus two threads every 30 s are a periodic wakeup, not an idle-zero. Estimate: a few ms of CPU per poll.
- `bytes.into_iter().take(limit).collect()` (line 432) copies byte by byte. For an image of up to `max_image_bytes` it is slow, doubles the buffer, and spikes RSS.
- Minimal fix:
  - Replace that line with `bytes.truncate(limit)`.
  - Run one `list-types` per observation and reuse it for sensitivity, HTML and image checks.
  - Raise the safety poll to 120 s, or make it a no-op when `wl-paste --watch` is alive.
  - Read stdout on the calling thread using `read_to_end` after a `poll`/timeout, instead of a thread per call.
- Also at `daemon.rs:3092`, the image receive buffer is `Vec::new()` and grows by doubling. Use `Vec::with_capacity(ticket.size as usize)`; it is bounded by `ticket.max`.

### 6. Daemon timers that wake an otherwise idle service

All numbers are wakeups per second at idle with one paired phone connected.

- `reconnect_tick`, every 2 s (`lib.rs:590`, body `daemon.rs:1499-1586`):
  - Takes `discovery.read().await` three times and clones the device list each time.
  - Does `store.get()` (a file read, see 3) for each announcing paired device on every tick.
  - Fix: compute the "paired and not linked" set first and return early when empty. Take one read of the discovery state. Better: an event-driven wake (a `Notify` fired on discovery change or link loss) with a 10 s fallback.
- `tick_alerts` every 30 s via `spawn_blocking` (`lib.rs:602-608`). Tokio's blocking pool threads exit after about 10 s idle, so each tick likely creates and destroys an OS thread. The function only takes a std mutex for microseconds. Call it directly on the async task.
- `coexist::watch` every 20 s (`coexist.rs:24-53`) builds a brand-new D-Bus session connection (connect, auth, Hello, `NameHasOwner`) each time. If the zbus build in use spawns a per-connection executor thread, that is a thread plus socket per probe (not verified). Fix: keep one connection and subscribe to `NameOwnerChanged`, or probe every 60-120 s.
- `expire_devices` every 5 s takes the write lock (`lib.rs:215`). Cheap, but it could be lazy (expire on read).
- `Privacy::forced` (`privacy.rs:50-88`) does `Path::exists()` and a `read_to_string` on every `item()`/`on()`/`view()`. `Daemon::privacy_view` (`daemon.rs:438`) calls `apply_privacy` (2 `on()`) plus `view()` (3 items): about 5 stats and 1 file read per `PrivacyStatus` request. I did not confirm that the per-notification or per-SMS paths also hit it. Fix: cache the resolved value for about 2 s, or watch the directory.

### 7. Per-packet classify chain in the link loop (service CPU, matters for call-audio frame rates)

- Where: `trusted_loop`, `daemon.rs:2463-2557`.
- `gate::classify_incoming(&packet.kind, &negotiated_in)` is evaluated up to 9 times sequentially per packet (calls, phone data, battery, callwifi, photos, notification, debug, then `on_packet`). I did not read `gate::classify_incoming`; if it scans the negotiated `Vec<String>`, that is 8-9 string scans per packet.
- Fix: compute `let class = classify_incoming(..)` once and `match class { ... }`.
- `route_clipboard` and `share::is_share_packet` run first for every packet as well.

### 8. Phone-data ingest clones every message (service CPU and transient RSS during syncs)

- Where: `phone_data.rs:641-642`. `m.body.clone()` and `m.clone()` run for every parsed message, even when `merge` returns no change. `fresh.push(m)` keeps a copy for correlation. `total` is recomputed over all threads on every packet (613).
- Fix: clone the body only when `incoming && unread && recent && sms_listed`. Pass `&m` to `merge` if the signature allows. Compute `total` incrementally.
- Contacts: `self.listing` (uid to revision, line 524) is kept forever after sync, about 5000 x 2 strings per phone. It is only needed while vCard batches are in flight. Free it when `pending` and `inflight` are empty (`next_vcard_batch`, line 477) and call `shrink_to_fit`. Estimate: 0.5-1 MB per phone.

### 9. Photos pane keeps decoded RGBA after the GPU texture exists (Share window RSS)

- Where: `photos_pane.rs:46-53`, `350`, `726-739`.
- `Model.thumbs` holds `Arc<Decoded>` (up to 256x256x4 = 256 KiB each, 24 items, about 6 MB). `texture_for` uploads it but the CPU copy stays in the model until the item leaves the list. `ColorImage::from_rgba_unmultiplied` also makes a transient copy.
- Fix: after upload, replace the entry with just `(w, h)`, or hold textures in the worker's published model only until first upload.
- Also, the worker polls `Request::Photos` every 0.5 s (focused) or 2 s x scaled even while the tab is closed (`photos_pane.rs:435-440`). Gate on `active || !loaded`, and lengthen to 10 s when inactive.

### 10. Per-frame work in the Messages window that scales with thread length (app CPU, only on redrawn frames)

- `script_fonts::ensure` (`messages.rs:1844-1860`) walks all conversation text and up to 2000 message bodies every frame, with a `Vec` alloc and range searches (`script_fonts.rs:285-296`). Run it only when the feed state changes. A generation counter from fix 2 gives this for free.
- `thread_bubbles` (`messages.rs:1643-1649`) calls `local_day()` for every message, every frame. That is one `localtime_r` (global tz lock) per message, up to 2000 per frame, and a `days` `Vec`. On Windows it is `GetDynamicTimeZoneInformation` per message (`localtime.rs:530-552`). Fix: compute days once per feed generation (store a parallel `Vec<Date>` in the app), or only for `[first..=last]`.
- `bubble` (`messages.rs:1757`, 1774): `m.body.clone()` and `clock_label_local` (a `format!` plus `localtime_r`) per visible bubble per frame. Cache the clock string by uid.
- `heights.len() > 8 * MAX_MESSAGES_LIMIT` clears the whole map (`messages.rs:1691`), which causes a one-frame re-measure storm plus a repaint. Evict instead.

### 11. Payload throughput (low confidence: write/read target not read)

- The 64 KiB chunk with a per-chunk `timeout()` is fine for LAN. If `share.rs` passes `tokio::fs::File` as `src`/`out`, every 64 KiB chunk is one `spawn_blocking` hop. I did not read the `share.rs` task bodies.
- If that is the case, raise `CHUNK` to 128-256 KiB or wrap in a `BufReader`/`BufWriter` with a larger buffer; otherwise leave it. Verify with a 500 MB loopback transfer before changing.
- Receive writes call `flush()` only at the end (`payload.rs:488`), which is good. Confirm there is no per-chunk `sync_data` in the share receive path.

## Memory-budget view of Messages (66 MB RSS vs 75 MB budget)

Headroom is about 9 MB. In order of how much each would give back:

1. The Arc-shared feed state (2) removes more than 1 MB of per-frame transient allocation and the fragmentation it causes.
2. Unchanged-poll elision (1) removes the repeated 100-250 KiB JSON decode and the matching service-side churn. The window then does no allocation at idle.
3. Calling `memtrim::trim` once after a large view arrives while the window is unfocused is cheap (today it runs only at frames 3 and 90 and on focus loss).
4. If the Share window is also counted, free the RGBA copies (9).

## Suggested order of work

1. Item 2 (small, mechanical, immediate RSS/CPU win in the GUI).
2. Item 1 generation/`Unchanged` plus the name-index cache.
3. Item 3 token cache plus trust-store stat check.
4. Items 5 and 6 (idle wakeups, spawn_blocking tick, coexist probe).
5. Items 4, 7, 8, 10 (each independent, low risk).
6. Item 9 and then 11 only after measuring.

Before any of these, take a baseline: `perf stat -p <service pid>` for 60 s idle with Messages open, and the app's `memstats` build, so each change can be confirmed.

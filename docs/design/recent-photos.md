# Recent photos on the computer (and why MMS attachments come second)

Design and feasibility note for gaps 5 (MMS attachments) and 6 (recent-photos feed) of
`docs/reports/parity-gaps-2.md`. It is written in our own words from our own code and from public Android
documentation; no proprietary material was used. Gap 6 is implemented (see [Status](#status)); gap 5 is designed here
and left for a follow-up. The build and test report is `docs/reports/recent-photos.md`.

## What the user gets

On the computer, in the Share window, a **Phone photos** tab shows the newest photos of a paired phone as a grid of
small previews. Pressing a preview shows its name, size and time and two buttons: **Save** (the full picture goes to the
save folder) and **Copy to clipboard**. Nothing is shown, listed or uploaded until the user has turned the feature on
**on the phone**, allowed **that computer** on the phone, and allowed it **on the computer** with an explicit button.

## Privacy

Photos are about the most private thing a phone holds, so the feature is built so that "off" means nothing leaves and
"on" still means nothing leaves until a person asks.

| Rule | How it is enforced |
| --- | --- |
| Off by default, on both ends | Phone: the switch *Show recent photos on the computer* is off; the app declares the packets in its identity only while the switch is on **and** Android gave photo access. Computer: `Feature::Photos` consent is off per phone; `geekbridge-ctl consent ID photos on` or the *Allow photos from PHONE* button. |
| Per computer, not global | Phone: the per-computer grant `GrantFeature.Photos` (`ComputerGrants`); the native core drops requests from, and sends nothing to, a computer that is not on the list (`transport_permits`, key `photos`). A computer paired later gets nothing. |
| Asked for, never pushed | The phone sends a list only as the answer to `list`, a thumbnail only to `thumb`, a file only to `get`. The computer accepts a list only within 30 s of asking, a thumbnail only for an id it asked for, and a file only for a photo it asked for (see below). There is no background upload, no sync, no schedule, no observer on the media store. |
| Previews only until asked | The list carries names, sizes and times (no pixels); thumbnails are at most 24 KiB and 256 px. The full file moves only on **Save** / **Copy**. |
| The computer cannot ask for arbitrary files | Ids are opaque (`p1`, `p2`, ...), made up by the phone per list and never reused. The phone honours only an id it handed out in the last ~four lists and re-checks that the picture still exists and is still visible to the app. No path, content URI or media id crosses the link. |
| Memory only on the computer | The list and thumbnails live in memory: 24 photos per phone, 8 phones, 2 MiB of thumbnails in all, erased when consent is withdrawn, the link ends, the phone is unpaired or after 30 minutes without a refresh. Nothing is written to disk except a picture the user chose to **Save**. A picture chosen for **Copy** goes to a private (0700) scratch folder inside the save folder that is emptied after ten minutes and at start. |
| Nothing logged | File names and image bytes are never logged on either side. |
| No cloud, no account | The only transport is the existing paired, pinned-TLS link on the local network. |

### Android permissions

* **Android 14+ (API 34):** request `READ_MEDIA_IMAGES` **and** `READ_MEDIA_VISUAL_USER_SELECTED` in one request. The
  system shows its own chooser: *Select photos* (partial access, only the pictures the user picks are visible to the app)
  or *Allow all*. The app works with either. With partial access the list says `partial: true`, the computer says so
  ("the phone shares only the photos you picked"), and the phone offers *Choose which photos GeekBridge may see* to
  re-open the chooser.
* **Android 13 (API 33):** `READ_MEDIA_IMAGES`.
* **Android 8 to 12 (API 26 to 32):** `READ_EXTERNAL_STORAGE` (declared with `maxSdkVersion="32"`).
* Never `READ_MEDIA_VIDEO`, `READ_MEDIA_AUDIO`, `ACCESS_MEDIA_LOCATION`, `MANAGE_EXTERNAL_STORAGE` or "all files access".
  Pictures are read through the media store (`MediaStore.Images`), the only route that honours partial access.
* Location metadata is not requested; thumbnails are re-encoded JPEGs and carry no EXIF.
* A store build that ships this permission needs the store's photo-permission declaration, which this note does not
  cover; the sideloaded and F-Droid style builds need nothing more.

## Packet shapes (GeekBridge only)

None of these names exists in stock KDE Connect, so a stock peer never declares them, is never offered them and never
sees them: `gate::negotiated_*` intersects the lists, and the packets are consent-gated like every feature packet.
`tests/android_negotiation.rs` and `tests/stock_peer.rs` pin this.

| Packet | Direction | Body |
| --- | --- | --- |
| `geekbridge.photos.request` | computer to phone | `{"op":"list","limit":1..24}`, `{"op":"thumb","id":"p3"}` or `{"op":"get","id":"p3"}` |
| `geekbridge.photos.list` | phone to computer | `{"items":[{"id","name","taken":ms,"size":bytes,"w","h"}],"partial":bool}` (at most 24, ids unique) |
| `geekbridge.photos.thumb` | phone to computer | `{"id","w","h","jpeg":"<base64>"}` (at most 24 KiB decoded, 256 px, starts like a JPEG) or `{"id","gone":true}` |

* The phone declares `geekbridge.photos.request` incoming and `.list` / `.thumb` outgoing. The computer declares the
  reverse. A phone that did not declare them is never sent a request.
* Thumbnails ride inside the packet (a small base64 JPEG), not on a payload socket: 24 of them is about 130 KB, and one
  packet per picture keeps every packet far under the 1 MiB relay limit and lets the rate limiter pace them (the phone's
  relay allows 8 packets a second). The data relay deliberately refuses payload-bearing packets, which is also why a
  thumbnail packet may not carry a payload.
* **Full pictures use the existing share channel.** `get` makes the phone start an ordinary file send of that one picture
  (`kdeconnect.share.request` with a payload on a pinned-TLS socket, 8 GiB limit, never overwrites). The computer would
  normally refuse a file from a phone that has no *receive* permission; for this one it records "I asked
  `<phone>` for `<name>`" and accepts the one offer from that phone with that (sanitised) name within two minutes, once,
  without the general *receive* permission and without a prompt, because the click on Save or Copy was the consent. An
  unasked offer, a second offer of the same name, or an offer after consent was withdrawn is refused as before.
  The general receive permission stays off.
* Parsers: `bridge_core::photos_link` (Rust, fuzzed by `photos_wire`) and `RecentPhotos.kt` (Kotlin). Both are strict:
  a list over 24, a repeated id, a name that is not plain text, a thumbnail that is too big or not a JPEG, or an absurd
  dimension is an error. Names from the phone lose control and bidirectional-override characters.

## Bandwidth and memory

| | Budget |
| --- | --- |
| Link | One list (about 3 KB) and up to 24 thumbnails of at most 24 KiB each per refresh; refreshes are limited to one per 3 s per phone and only happen when the user opens the tab or presses Refresh. |
| Phone | No cache and no thread: each answer is computed on request from the media store (`loadThumbnail` on Android 10+, a sampled decode before), encoded at falling JPEG quality until it fits 24 KiB. At most 96 ids kept. A bitmap is recycled right after encoding. |
| Computer service | At most 2 MiB of thumbnails across all phones plus 24 list entries per phone; all of it erased on consent withdrawal or link loss. |
| Computer window | Previews are decoded on the window's worker thread, never in a frame (at most 256 px, header checked before decoding), and become at most 24 textures of at most 160 x 160 pixels (about 1.5 MB); textures are dropped when a photo leaves the list. |
| Full picture | Not fetched until Save or Copy; then one transfer under the share limits (3 at once, 2 per phone). |

## Computer UI sketch

```
+------------------------------------------+
| ( Send and receive )( Phone photos )     |
|                                          |
| RECENT PHOTOS                  [Refresh] |
| Pixel shares only the photos you picked. |
| +------+ +------+ +------+ +------+      |
| | img  | | img  | | img  | | img  |      |
| +------+ +------+ +------+ +------+      |
| +------+ +------+                        |
| ...                                      |
| +--------------------------------------+ |
| | IMG_2041.jpg                         | |
| | 2.3 MB · 4032 × 3024 · 2026-06-15    | |
| | [ Save ] [ Copy to clipboard ]       | |
| | Saved to ~/Downloads/GeekBridge/...  | |
| +--------------------------------------+ |
| Only previews are sent to this list...   |
| [ Stop showing photos ]                  |
+------------------------------------------+
```

* The tab exists only while a connected phone declares the photos packets. When photos are not yet allowed for that
  phone the tab shows what will happen and one button, *Allow photos from Pixel*; nothing is requested before the click.
* Each cell is one control named "Photo IMG_2041.jpg, 2026-06-15 09:30" (or "..., preview loading"); Tab and Enter work.
  Fetch progress is a polite live region.
* **Copy to clipboard** needs a JPEG, PNG, GIF or WebP name and a clipboard tool (`wl-copy`, then `xclip`, on Linux;
  PowerShell / `osascript` elsewhere, which are not exercised). Otherwise the line says what was tried and **Save** works.
* Golden screens: `share-photos`, `share-photos-detail`, `share-photos-off` (light and dark, standard and minimum size).
* CLI: `geekbridge-ctl photos [DEVICE] [--json]`, `photos refresh|save|copy ...`.

## Feasibility: why photos first, and what MMS needs

Photos are the smaller change because every moving part already exists: a consent and per-computer grant mechanism
(`Feature`, `GrantFeature`, permits), a request/answer relay between the link and Kotlin, bounded in-memory stores on the
computer (the `mirror` pattern), a payload channel for large bytes (the share manager), and a window to host the grid. What
is new is one packet family, one Android media-store reader and one tab.

**MMS attachments (gap 5)** are feasible but larger, for these reasons (all from our own code and public documentation):

* The part bytes live in the telephony provider's MMS part table (`content://mms/part`, columns such as content type and
  the part id; the bytes are read through a per-part URI). Reading needs `READ_SMS`, which the app already asks for under
  the SMS switch, and works only on a sideloaded build, as the SMS feature does.
* Our SMS rows already carry `attachments: [{part_id, mime_type, unique_identifier}]` and show a placeholder
  ("Picture message: open it on the phone"). Fetching a part would be a new request/answer pair (a part id plus its
  unique identifier in, a small preview packet and a payload for the full part out) keyed to a conversation row.
* The extra work is the messages model, not the link: previews inside a thread, size caps per part, carrier quirks
  (parts without a content type, SMIL parts, group MMS with several addresses), and a test matrix of real carriers that this
  repository cannot run without a SIM. The privacy rules above carry over unchanged (no auto-download, thumbnail first,
  lazy full fetch, memory only), and so does the packet design: a `geekbridge.sms.attachment.*` family, GeekBridge-only,
  consent-gated by the existing `Sms` feature.
* Recommended next step: reuse `photos_link`'s thumbnail packet and the share-channel "expected file" mechanism, add the
  part lookup on the phone, and show previews in the Messages window behind a "Load pictures" button per thread.

## Status

Implemented: everything in this note for recent photos (Rust core, service, control API, CLI, window tab, simulated phone,
Android feature with permission handling), with the tests listed in `docs/reports/recent-photos.md`. Not done: MMS
attachments; videos; folders other than the media store's image index; more than 24 photos; an Android-side picker for
which albums are visible (Android 14 partial access is the user's control for now); real-device verification (see the
report).

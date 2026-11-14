# History responsiveness — 2026-11-14

Runtime: Rust 1.98.1, gtk-rs 0.11.5 / GTK 4.22.4, libadwaita 1.9.3.
A private Xvfb/DBus/keyring fixture uses Cairo, 900×520, 720 archived messages,
140 unread, and a 350-item Markdown list in every ninth message. After opening
the latest page it sends upward scroll bursts for three seconds, samples the
main loop every 4 ms, and checks the retained window against the current screen
plus two screens on each side. The archive keeps all 720 messages.

The comparison used the same final sources and fixture; only the long-list
virtualization block was temporarily removed for the baseline and then restored.

| Case | Worst main-loop gap | Retained message rows |
| --- | ---: | ---: |
| Eager list items | 225.24 ms | 5 |
| Recycled list items | 137.16 ms | 21 |

These are single-run measurements, not a general hardware guarantee. A normal
short-text burst previously measured 12.40 ms. The reported complete freeze was
not reproduced in this fixture; it now guards the expensive long-list path and
the 400 ms responsiveness limit. Production GPU/live-account acceptance remains
separate from this Cairo fixture.

Changes retained: lists with more than 16 items recycle rows through the existing
native block scroller. One huge list can no longer bypass the article's top-level
block limit. Sticker decoder disposal sends process reaping to a worker instead
of waiting on GTK. Focus/send read-state archive updates also run on workers.

Additional native production checks cover focused older-history arrival/read
receipts, typing subtitle transitions, icon lookup/preferences/node selection,
whole fitting URLs, compact file rows, and actual asynchronous inline-emoji
texture loading plus renderer disposal. The earlier inline visual probe kept an
Rc renderer in the test itself and therefore could not catch production's
premature renderer disposal; the new test uses the production widget-only API.

A link-only preview regression reproduced 26 px of unused trailing space beyond
the preview's outer edge (48 px if counting only its content). The bubble clamp
now uses the actual allocated preview edge; its content width is 366 px in the
same fixture, matching the card rather than its 392 px width request. Ordinary
bubble padding is unchanged. The regression runs in the native GTK fixture.

Full native smoke also reproduced a 65 px gap after a photo at a narrower
allocation. Aspect images and the bubble clamp now report no fixed vertical
minimum without a width and use the same width cap for height measurement and
allocation. The unchanged 30 px adjacency assertion passes at 1920/960/460 px.
Opening the requested media sidebar after the first conversation paint reduced
the same startup fixture's maximum heartbeat gap from 448 ms to 193 ms; the
complete multi-step native smoke measured 288 ms. Its 4 ms heartbeat is enabled
only by `--smoke-test` and asserts the 400 ms limit.

Reply quotes fill the available card width and elide at a whole-word boundary
with `...`, falling back to a mid-word cut only when that word exceeds the line.
A stable source layout keeps truncation from shrinking and re-expanding the
parent bubble. Preview shaping is bounded to 4096 Unicode scalars. The native
regression exercises the production reply row, ordinary words and one over-wide
word rather than estimating width from a character count.

Like mobile ChatMessageCell's `AndroidUtilities.replaceNewLines`, desktop reply
previews fold source line breaks into spaces. The message itself and stored
reply target/fallback stay intact. The native fixture includes the screenshot's
line break after the parenthetical phrase and verifies it does not force a
short line in the card.

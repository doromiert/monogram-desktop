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

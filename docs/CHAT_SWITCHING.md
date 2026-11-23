# Chat switching and context menus

Chat switches retain up to three inactive timelines, each capped at 72 message
rows. Only completed timelines at the newest local edge are retained. Their
widgets are detached while inactive; returning attaches the same rows immediately
and refreshes them from local history. Older views are evicted, and account
activation releases the entire cache. This is a UI cache, not another history
store or a replacement for the bounded active history window.

Page Up and Page Down scroll the current conversation by a native GTK page while
the Monogram window has focus, including from the composer and sidebar controls.
They retain keyboard focus and the draft/caret. Modified shortcuts and keys in
dialogs, popovers or media/story viewers keep their normal behavior. Paging uses
the existing history-window loading and retention rules.

Uncached timelines still build in short batches to keep input responsive. The
list stays transparent while being populated and becomes visible after final
allocation and scroll placement, before paint. The existing history spinner
provides feedback during that work. Intermediate batches no longer look like
scrolling through old messages.

Chat and folder context menus use `GMenu`, a popup-local action group, and
`GtkPopoverMenu`, following Nautilus's native menu pattern. The menu has a 200px
minimum width, native row spacing and start-aligned text. Cleanup is deferred
until after action dispatch because native popovers close before activating an
item. Remaining custom media/picker menu buttons also use compact, start-aligned
rows.

Reference: [GNOME Nautilus files-view context-menu implementation](https://github.com/GNOME/nautilus/blob/main/src/nautilus-files-view.c)
(GPL-2.0-or-later; the Rust integration uses the same standard GTK APIs).

## Verification

Private X11/D-Bus/keyring fixtures, Rust 1.98.1, gtk4-rs 0.11.5, GTK 4.22.4,
libadwaita-rs 0.9.2 and libadwaita 1.9.3:

- Original warm chat switch: 299ms, different widgets; cold render exposed 100
  partial-viewport observations in the polling fixture.
- Cached warm switch: 29–44ms, identical widgets reused. Cold switch: 288–291ms,
  loading feedback after 9ms, zero partial-viewport observations.
- Cache eviction, three-view/72-row bounds and account isolation checked.
- A 720-message/140-unread fixture retained 22 rows and stayed responsive during
  rapid scrolling; maximum observed GTK heartbeat gap was 147ms.
- Physical Alt-arrow keys update both folder state and the selected toggle.
- Native menu rows measured 162px by 32px, aligned to the start; physical Return
  dispatches the intended action and then cleans up the popup.

These timings describe controlled local fixtures, not a network benchmark.

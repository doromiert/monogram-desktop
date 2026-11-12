# Desktop GIF grid

The GIF page ports Galeria's actual `layout.RowLayout` into Rust and follows
`timeline.Mosaic` / `timeline_cells.Cell` for viewport controls. It uses justified
rows, a 154 px target height, a 3 px gap, and Galeria's 64 px minimum tile size;
extreme aspect ratios crop through `GtkPicture`'s Cover fit. The last unfinished
row keeps its natural width. A binary row lookup determines the visible band.
Only controls around the viewport exist, with at most two new controls or 3 ms
of control construction per frame. Network/cache access and image decoding use
the existing worker-backed, account-scoped GIF library and sandboxed decoder.

The native favorite button overlays each tile instead of Galeria's selection
check. Favorite changes persist in the account library; a failed change restores
the previous toggle and shows an error. Search pages deduplicate IDs and fetch
the next provider offset near the end, including an underfilled viewport.
Concurrent requests, exhausted results, stale queries and errors are guarded;
errors pause automatic paging until an explicit retry. The keyless state keeps
saved GIFs searchable and exposes Add key. There is no GIF settings gear. Preferences → General → GIF search retains key
replacement/removal, bound to the original account and session generation.

## Reference sources

User-requested source: [Galeria](https://github.com/zenos-n/galeria), local
`/Users/doromiert/Projects/gallery/`, at source commit
[`325d5e31b084718e164531f375d756ea50778996`](https://github.com/zenos-n/galeria/commit/325d5e31b084718e164531f375d756ea50778996). Snapshot SHA-256:

- `layout.py`: `c8025346c170273af1846af0e9a8d12263150ae62008d3e6b0dfa471891ef232`
- `timeline.py`: `02e48d60f1ef3f8383c8f29fc9420c81d4a93935244b49df2b2e0c40fbfdd81b`

The adapted grid module retains Galeria's NAPL 1.1 license in
[LICENSES/NAPL-1.1.txt](../LICENSES/NAPL-1.1.txt), included in the installed package.
Mobile counterpart inspected: `EmojiView.GifLayoutManager` and
`GifSearchAdapter` (native variable-aspect grid, near-end paging, cancellation),
and `MonogramGiphy` (account-local favorites/recents and direct opt-in search).
The existing mobile screenshot `.state/settings-renders/gifs.png` confirms the
account-specific keyless state and offline saved-GIF behavior. This desktop presentation follows the user's explicit Galeria layout request.

Real-provider acceptance with a configured GIPHY account remains a separate
parity gap. Local geometry/native checks do not establish provider acceptance.

## Verification

The native isolated GTK/Secret Service scenario exercises 2,000 local item
records, bounded viewport controls, native favorite persistence, duplicate
append, automatic near-end request initiation, keyless failure pausing, three
successful cached provider-page appends, exhaustion and stale-query rejection. First tile controls and scroll feedback each appear in under 400 ms
in that scenario. The geometry check covers narrow and wide windows and extreme
aspect ratios. This does not prove a successful live GIPHY paging roundtrip or
all application startup/history responsiveness.

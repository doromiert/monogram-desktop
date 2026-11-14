# Monogram desktop tasks

## In progress

- [x] Fix sticker panel resource exhaustion; bound video playback and keep GTK responsive.
- [x] Finish metadata-first sticker/custom emoji sync; show download circles until media arrives.
- [x] Sync favorites, recents, and their order for sticker packs, custom emojis, and standard emojis.
- [x] Fix inline custom emoji sizing inside message text.
- [x] Finish inline editing: transparent text background, maximum bubble width, emoji suggestions, Up Arrow for the last sent message, Enter to save, Shift+Enter for a newline.
- [x] Keep currency amounts from accidentally triggering inline LaTeX, consistently with mobile.

- [x] Use right-click context menus for stickers/custom emojis, and Alt+left-click to toggle Favorites without sending.

## Backlog

- [ ] Hold a sticker/emoji still to enlarge it; after enlargement, moving across items previews the item under the cursor. Keep mobile's equivalent preview browsing behavior. Before preview opens, hold-and-move in Favorites reorders items.
- [ ] Add full sticker pack editing, modeled on mobile, in a centered desktop dialog.
- [ ] Allow Favorites and Recent sticker/emoji sections to collapse.
- [ ] Animate sent stickers from their panel tile into the final chat bubble; show a spinner in place of delivery checkmarks during upload, rather than a separate upload card.
- [ ] Add configurable double-click actions using mobile's Double-tap actions settings as the reference, including incoming/outgoing previews and Quick Reactions. Default incoming to Reply and outgoing to Edit.
- [ ] Animate incoming messages from below the input box on the left.

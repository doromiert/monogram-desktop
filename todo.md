# Monogram desktop tasks

## Completed fixes

- [x] Fix sticker panel resource exhaustion; bound video playback and keep GTK responsive.
- [x] Finish metadata-first sticker/custom emoji sync; show download circles until media arrives.
- [x] Sync favorites, recents, and their order for sticker packs, custom emojis, and standard emojis.
- [x] Fix inline custom emoji sizing inside message text.
- [x] Finish inline editing: transparent text background, maximum bubble width, emoji suggestions, Up Arrow for the last sent message, Enter to save, Shift+Enter for a newline.
- [x] Keep currency amounts from accidentally triggering inline LaTeX, consistently with mobile.

- [x] Use right-click context menus for stickers/custom emojis, and Alt+left-click to toggle Favorites without sending.

## Completed desktop follow-up

- [x] Hold a sticker/emoji still to enlarge it; after enlargement, moving across items previews the item under the cursor. Keep mobile's equivalent preview browsing behavior. Before preview opens, hold-and-move in Favorites reorders items.
- [x] Add full sticker pack editing, modeled on mobile, in a centered desktop dialog.
- [x] Allow Favorites and Recent sticker/emoji sections to collapse.
- [x] Animate sent stickers from their panel tile into the final chat bubble; show a spinner in place of delivery checkmarks during upload, rather than a separate upload card.
- [x] Add configurable double-click actions using mobile's Double-tap actions settings as the reference, including incoming/outgoing previews and Quick Reactions. Default incoming to Reply and outgoing to Edit.
- [x] Animate incoming messages from below the input box on the left.

- [x] Remove the duplicate outer background from sticker/custom emoji context menus.

## Current follow-up

- [x] Replace pack editor list with mobile-style virtual thumbnails and native pack/item menus; verify keyboard actions, resizing, loading/error states and screenshots.
- [x] Remove preview card padding and display artwork directly.
- [x] Add drag-to-reorder pack-bar items.
- [x] Track actual picker edits with durable revisions; imports and exports retain versions, latest user change wins across clients.

- [x] Fix multi-session MUC joins that discarded repeated matching presence items; restore the visible room after reconnect and request history after confirmation.

## Reported regressions — active

- [x] Restore encrypted MUC sending and make room fingerprint auto-trust on by default on both clients.
- [x] Preserve viewport/scroll anchoring when deleting messages, including the bottom and a deleted anchor.
- [x] Automatically download visible media in joined rooms; preserve cancellation, file limits and verification.
- [x] Remove Developer Tools from the hamburger menu.
- [x] Implement live Stories using mobile protocol/source rather than local-only previews.
- [x] Show the Monero panel tab after configuring a wallet.
- [x] Fix actual pointer dragging to reorder packs on the category bar.
- [x] Make Manage Packs show/reorder all packs and add an editor action to each pack heading.
- [x] Fix pack editor header color, disabled actions, sticker dragging and duplicate hover highlight.
- [x] Check native controls/accessibility and sub400ms feedback on the affected flows.

- [x] Show sticker/custom-emoji reply thumbnails and readable labels in replies and chat sidebar instead of WebP filenames.

- [x] Add local contact nickname/avatar/notes and suggest-avatar controls in Conversation Info; sync contact overrides with mobile.
- [x] Mark focused bottom-of-chat MUC messages read, including after dialogs close.

## Motion photos

- [x] Preserve JPEG still and embedded MP4 through the existing XMPP encrypted file path on both clients.
- [x] Desktop: still-first inline/viewer playback, keyboard-accessible play/stop, one-shot playback returning to still.
- [x] Verify malformed containers, persistence/retry, native playback/lifecycle and UI responsiveness; build fresh mobile APK (0.2.219 published; physical phone check pending offline ADB).

## Stories and clipboard input

- [x] Close Stories from any background area or Escape, including while the reply field has focus.
- [x] Paste clipboard images/files from anywhere in the focused chat window with Ctrl+V or Shift+Insert; preserve ordinary text paste.
- [ ] Verify native pointer/keyboard behavior and package the desktop update.

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
- [x] Verify native pointer/keyboard behavior and package the desktop update (Stories6.86s, clipboard1.37s; packaged smoke110ms feedback/246ms maximum heartbeat gap).

## Stories transition

- [x] Add compositor scale/slide and fade on story entry/exit, honoring reduced motion and interrupted dismissal.
- [x] Verify intermediate animation frames and native Stories keyboard/background controls.

## Message clamp width

- [x] Use the full existing chat clamp for message maximum width, fit short bubbles to content, and remove the message-body natural-width character cap.
- [x] Verify long text/links, incoming/outgoing alignment, editing and narrow-window sizing; update the desktop package (native0.49s, full packaged smoke115ms feedback/248ms maximum heartbeat gap).

## Corrected clamp percentage and URL wrapping

- [x] Cap bubbles at 40% of the clamp maximum (432px), shrinking to available width on narrow windows.
- [x] Keep fitting URLs whole; prefer `/`, `&`, `?` breaks otherwise, preserving text/copy/style and safe oversized-segment fallback.
- [x] Verify raw/rich links, selection, formatting and full app layout; update package (native geometry/selection/formatting passed; final packaged smoke 254ms maximum heartbeat gap; ZenPkgs d57d97c9 installed and running).

## Wrapped link bubble fitting

- [x] Fit wrapped URL bubbles to painted text/preview edges while preserving standard insets and the 432px cap.
- [x] Verify fitting stays stable across raw/rich links, preview cards and narrow resizing; publish/install the desktop package (native 0.49s/0.95s, full source smoke 267ms and packaged smoke 256ms maximum heartbeat; ZenPkgs b97744a9 installed/running).

## Empty folders and file transfers

- [x] Add native folder/search empty states and keep the full-height slide when switching to/from empty folders (native test 1.70s).
- [x] Add desktop automatic downloads for all file types with an adjustable 15 MB default limit and verify header/body enforcement (4 transport tests plus native persisted settings checks).
- [ ] Finish diagnosing the intermittent remote/network cause of initial transfer stalls. Mobile 0.2.221 installed/published: body/response recovery plus accepted-object confirmation; user confirms quicker recovery but brief stalls remain. 21 transport + 6 picker tests pass; no further phone ADB after departure. Desktop real-world stall is not reproduced.
- [x] Deliver fresh desktop package and mobile APK: 0.2.221 installed on phone before departure and published; desktop ZenPkgs 8a52b8f4 installed/running, packaged smoke 255ms maximum heartbeat. Recovery additionally verified against real Android API36 HTTPS using a private emulator; underlying remote/network stall remains open.

## Media with replies

- [x] Media-only messages with a reply must keep thin padding on media-touching edges and standard reply padding around the reply, with or without reactions (reference: /tmp/codex-clipboard-O9Sz5j.png).

## Reply compatibility

- [x] Strip Monocles legacy reply fallback quotes from the displayed body when rendering the native reply preview, matching mobile (reference: /tmp/codex-clipboard-VaTA0D.png).

## Desktop sidebar follow-ups

- [ ] Make the media sidebar scroll viewport and its edge shadow span the full sidebar width; retain content padding inside it (reference: /tmp/codex-clipboard-7816YI.png).
- [ ] Make sticker-pack drag reordering show continuously interpolated real-time positions like folder reordering, without inconsistent snapping (references: /tmp/codex-clipboard-geVbGd.png, /tmp/codex-clipboard-gNurVs.png).
- [x] Remove icons from the media sidebar tab switcher; use text-only labels (reference: /tmp/codex-clipboard-l6MfI7.png).
- [x] Restore the correct Monero wallet sidebar with the 3D card, locating/reusing the existing implementation or version history; keep wallet settings in Preferences (reference: /tmp/codex-clipboard-8gw0a6.png).
- [ ] Verify pack deletion/import and horizontal pack-bar order update promptly on paired clients; distinguish persisted sync state from stale visible picker state.

## Current picker/wallet/sync fixes

- [x] Animate pack selector icons using the existing visibility-aware decoders.
- [x] Restore the wallet card sidebar and text-only Monero toggle; retain real wallet metadata without demo funds.
- [x] Verify live phone sticker favorites/recents reach desktop: after restoring TCP/UDP45950 and restarting the updated desktop, phone revision1794843209272 imported10 favorite stickers and3 recent stickers. No further phone control was used.
- [ ] Activate the already-configured Monogram system firewall module so TCP/UDP45950 survives reboot; the booted generation predates it.
- [x] Review PR2 head32c1b77 first: GTK-thread cache I/O/PNG work remains a freeze blocker; no merge.

- [ ] Port Android's fast post-body acknowledgement recovery to desktop uploads: one-second first ACK window, one fresh slot, five-second last ACK window, final stored-object confirmation; desktop currently waits30seconds per ACK.

## Pack file activation

- [x] Open `.pack`, `.nzsp`, and `.xmsp` chat attachments in the existing pack dialog on desktop and the sticker sheet on mobile.
- [x] Register native file-manager associations, including desktop launch arguments and Android content-URI permissions.
- [x] Preview before installation; keep Add Pack explicit and validate archives off the UI thread with cancellation and account fencing.
- [x] Verify cold library startup, installation, malformed input, single-click download/open, and continued GTK responsiveness.
- [x] Move PR #2 raster-preview cache reads, PNG encoding, writes and pruning off GTK before merging.

## Current invitation, profile and media follow-up

- [x] Reconcile accepted group invitations on both clients from confirmed server bookmarks/joins; refresh open invitation views.
- [x] Add clickable native profile cards to chat/sidebar avatars and names using real XMPP profile/presence data.
- [x] Add inline audio-file and voice-message playback with download, seeking, duration and cancellation.
- [x] Bound pack category strip height and restore full-width sidebar scroll shadows while keeping inner row padding.
- [x] Check profile keyboard access, narrow cards, loading/error feedback, account fencing and native screenshots.

## Oversized animated pack icon regression

- [x] Bound animated pack icon measurement independently of source media dimensions; the previous 64px fixture missed a 512px video expanding the bar to 518px.
- [x] Verify realistic animated icons and native responsiveness: 12×512px WebM packs keep the strip at 30px and icons at 24px; 48-video grid/hover test maximum UI gap57ms.
- [x] Publish and update the installed ZenOS package with the corrected icon measurement: source6887b50, ZenPkgs3b26c59c; packaged smoke119ms initial feedback /338ms maximum heartbeat.

- [x] Restore the pack category row’s previous 10px horizontal inset on the row itself, keeping the sidebar scroll viewport and shadow full width.

## Forwarded attachments and media replies

- [x] Fold the mobile Legacy OMEMO forwarded-attachment marker into its adjacent attachment, preserving both wire messages/IDs and checking author, key, protocol and timing. Recover existing history and retain linked deletion behavior.
- [x] Give media reply cards the normal bubble inset while keeping media edges thin; native geometry/screenshot verified with and without reactions (1.13s), 290 default tests plus4 targeted compatibility checks pass.
- [x] Publish and install the updated desktop package: sourceb29c234 / ZenPkgs59d3ae09; final packaged smoke109ms feedback /342ms maximum heartbeat.

- [x] Keep long quoted media bubbles fitted to the actual media width: native regression reproduced428px content beside320px media, then passed at320px with13px/10px reply insets and2px media edges (1.04s).

- [x] Increase the reply-to-media gap to10px to match the vertical reply inset; keep the media-to-reactions spacing unchanged.

- [x] Show pending photo/video/GIF/sticker uploads as native media previews with the shared download-style progress ring, cancel/retry controls and overlaid sending status; native phases/cancel/retry/caption/video checks pass with26ms feedback/91ms maxUI gap.

- [x] Fix long attachment names overflowing reply cards beside thumbnails: reproduce36px row allocation for50px text, replace the horizontal Box with a native Grid, and verify text/image reply width and painted bounds under fatal GTK critical checks.

- [x] Publish/install the combined media upload previews, reply-thumbnail layout and10px reply-to-media gap update: source7387e3c / ZenPkgs320697f1 installed/running; final packaged smoke109ms feedback/343ms maximum heartbeat. File replies keep the paperclip and name on one ellipsized line while text captions retain the Grid sizing fix.

- [x] Keep the paperclip and attachment filename together on one ellipsized reply line while retaining the Grid fix for two-line caption previews; native filename/caption geometry and full-width text quote checks pass.

- [x] Restrict automatic downloads to media; ordinary files choose a native save destination before transfer, with cancellation/account fences and pack opening retained. Native picker/cancel/background export passes1.11s;291 default tests pass (94ignored).
- [x] Publish/install desktop manual-document save behavior: source656bf13 / ZenPkgsf8a7340b, packaged smoke147ms feedback/369ms maximum heartbeat. Mobile0.2.227/74449 published froma6bccbc34 with3API36 tests, matching signer/all8native libraries and ELF/ZIP16KiB checks; hosted digest verified.

- [x] Integrate merged PR3 audio helper and preserve manual-document save behavior plus automatic media policy for both audio paths.321 default tests pass128ignored;12 real sandbox +4 playback +6 audio UI checks pass, including25s playback and bounded preparation. Existing audio/profile/pack-strip and native save-picker checks pass.
- [x] Publish/install merged PR3 (850786b) plus audio-policy integration270c11b: ZenPkgs97eb01b7, packaged audio container/PCM and helper-confinement checks pass; native package smoke109ms feedback/339ms maximum heartbeat. Exact installed app verified with2.3ms Ping and no GTK criticals; live loading feedback205ms/frame gap369ms.

## Current encrypted sending and mentions

- [x] Match mobile automatic MUC encryption: reuse established sessions and cached discovery, skip temporarily unavailable recipients while retaining encrypted delivery to at least one external recipient. Preserve strict trust mode and explicit blocks.
- [x] Add MUC @nickname suggestions to composer and message editor using the existing emoji completion popover; Tab accepts, arrows choose, Escape dismisses, Enter retains its send/save behavior.
- [x] Review native popover reuse, accessible plain-text nickname labels, room scoping and bounded synchronous filtering in source. Build passed; user requested no self-testing.
- [ ] User acceptance of @nickname popup behavior; the last three user-sent messages were observed as sent with Legacy encryption after the automatic recipient-policy fix.

## Current desktop reports (2026-11-20)

- [x] Composer: grow by full display lines plus top/bottom padding, cap at five lines, and scroll minimally to the caret instead of centering after a newline.
- [x] Animated Noto emoji: stop playback at the rest marker without entering the outro.
- [x] Profile popouts: use the same presence, local nickname and avatar as the sidebar and refresh coherently.
- [x] Add Alt+Left/Right visible-folder navigation and Alt+Up/Down chat navigation in folder order, including shortcut help.

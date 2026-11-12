# Desktop interaction design

Follow the [GNOME HIG](https://developer.gnome.org/hig/) and use the platform's
widgets, metrics, window controls, fonts, contrast and focus handling.

Reference implementation: [Nautilus window Blueprint](https://gitlab.gnome.org/GNOME/nautilus/-/blob/20fc816d6f1e148e1ba7639865a893a39540f281/src/resources/ui/nautilus-window.blp),
revision `20fc816d6f1e148e1ba7639865a893a39540f281`. Its outer
`Adw.OverlaySplitView`, with separate `Adw.ToolbarView` and header bars on either
side, is the structural reference. No Nautilus source code or assets were copied.

## Structure

- **Left navigation sidebar:** app header, requests, search,
  folder filters, story avatars and native list rows. The primary menu leads to
  Contacts, Wallet, Stories, Spaces, Profile and Preferences. No bottom tab bar.
- **Conversation:** its own native header, timeline, ordinary message bubbles,
  quoted replies, encryption marker, polls, artwork, file and location previews,
  and a fixed composer. Real text selection and keyboard focus remain native.
- **Right utility pane:** emoji/stickers/GIFs/Monero or conversation information. It extends to the top edge, with its own native toolbar/header, following the user’s Galeria/Gnosis reference.
  F9 toggles it. At narrow widths the pane overlays rather than squeezing every
  column into a tiny fixed area.
- **Secondary tasks:** `Adw.Dialog` with `Adw.HeaderBar`/`Adw.ToolbarView`;
  Preferences uses searchable `Adw.PreferencesDialog` pages, groups and rows.
  Context actions use native popovers and text labels.
- **File selection:** portal-aware `Gtk.FileDialog`, not a custom gallery or file
  browser. The Nix wrapper sets `GTK_USE_PORTAL=1` for this process only. A usable
  host file-chooser portal is required; headless native checks do not certify
  every desktop's portal configuration.

Below 760sp of available navigation/content width the conversation sidebar becomes an overlay; a header button appears only in this collapsed mode.
The right utility pane independently becomes an overlay below 1240sp.
The app respects window scaling, text metrics and the desktop color scheme. The
light/dark preview switches affect only this app's `Adw.StyleManager`.

## Desktop adaptations

Use pointer/context menus and keyboard shortcuts instead of requiring long
presses or swipe gestures. Provide toolbar/menu entry points for every operation
that was a mobile attachment sheet or bottom-tab destination. Editor/selection
menus must remain anchored to the selected content. Native portals own system
file access. A later camera/call implementation must likewise respect desktop
permissions and device selection rather than copying Android permission flows.

The prototype currently leaves selected files in the composer for review; this is
a desktop interaction proposal, not a claim that the Android auto-send picker
flow is already reproduced. Final send/caption behavior needs desktop acceptance
before connecting a transport. No real send is possible in this milestone.

## Deliberate scope limits

The GIF palette uses bundled animations and map previews use a placeholder. The
wallet uses the source Android card meshes and GLSL materials, including the
Monero model shaders. The native Gtk.GLArea adapter preserves the source camera,
lighting, engraving, QR insert and flecks, while text/address/balances are fictional.
The article editor currently shows Markdown text. Media-viewer controls currently
exercise fixture selection, not a full video pipeline. These remain parity work,
not features removed from the desktop product.

Search follows NautilusToolbar’s title-widget Gtk.Stack/search-container pattern: the
search mode replaces the header title rather than inserting another toolbar row.
Reference: https://gitlab.gnome.org/GNOME/nautilus/-/blob/20fc816d6f1e148e1ba7639865a893a39540f281/src/resources/ui/nautilus-toolbar.blp

Stories follow Android DialogStoriesCell and StoryViewer: horizontal avatar strip
inside the scrolling chat list, unread/seen rings, collapsed header avatars, direct
overlay viewer, timed segments, hold/pause, next/previous and private local replies.
My Story opens the photo portal when no own story exists. Posting creates only an
in-memory preview. Expiration, archives, profile persistence, video and XEP-0501
transport remain backend milestones.
Reference: https://github.com/doromiert/monogram/blob/be6876c045163e32fe07a84416621770b0964b30/TMessagesProj/src/main/java/org/telegram/ui/Stories/DialogStoriesCell.java

Folder type/exclusion/override precedence follows Android MessagesController.DialogFilter
and FilterCreateActivity. The prototype evaluates rules against changing fixture
state, rather than snapshotting a folder's current member IDs.

The focused chat window routes Enter/Keypad Enter and Escape through the
composer even when a non-editor control has focus. Shift+Enter retains its
newline behavior; dialogs, popovers and other text inputs keep their native
keys. A short body fills the bubble width supplied by replies/reactions so
its timestamp aligns to the right. Plain bubbles tighten to rendered line
extents while preserving wider media/reply/reaction children. Reply previews
mark truncation with an ellipsis and keep whole words unless a word itself
exceeds the preview budget.

Native GTK acceptance uses the production widgets in an isolated chat: reply-
and reaction-stretched timestamps reach the right edge; the reported wrapped
body leaves under 4 px of unused line width. Real header-focused Enter/Escape,
Shift+Enter and dialog key isolation pass. The reply truncation unit check
covers complete text, whole words and an oversized Unicode word.

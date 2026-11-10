# Desktop interaction design

Follow the [GNOME HIG](https://developer.gnome.org/hig/) and use the platform's
widgets, metrics, window controls, fonts, contrast and focus handling.

Reference implementation: [Nautilus window Blueprint](https://gitlab.gnome.org/GNOME/nautilus/-/blob/20fc816d6f1e148e1ba7639865a893a39540f281/src/resources/ui/nautilus-window.blp),
revision `20fc816d6f1e148e1ba7639865a893a39540f281`. Its outer
`Adw.OverlaySplitView`, with separate `Adw.ToolbarView` and header bars on either
side, is the structural reference. No Nautilus source code or assets were copied.

## Structure

- **Left navigation sidebar:** app header, requests, new conversation, search,
  folder filters, native list rows, account footer. The primary menu leads to
  Contacts, Wallet, Stories, Spaces, Profile and Preferences. No bottom tab bar.
- **Conversation:** its own native header, timeline, ordinary message bubbles,
  quoted replies, encryption marker, polls, artwork, file and location previews,
  and a fixed composer. Real text selection and keyboard focus remain native.
- **Right utility pane:** emoji/stickers/GIFs or conversation information. It extends to the top edge, with its own native toolbar/header, following the user’s Galeria/Gnosis reference.
  F9 toggles it. At narrow widths the pane overlays rather than squeezing every
  column into a tiny fixed area.
- **Secondary tasks:** `Adw.Dialog` with `Adw.HeaderBar`/`Adw.ToolbarView`;
  Preferences uses searchable `Adw.PreferencesDialog` pages, groups and rows.
  Context actions use native popovers and text labels.
- **File selection:** portal-aware `Gtk.FileDialog`, not a custom gallery or file
  browser. The Nix wrapper sets `GTK_USE_PORTAL=1` for this process only. A usable
  host file-chooser portal is required; headless native checks do not certify
  every desktop's portal configuration.

Below 860sp the conversation sidebar becomes an overlay; an always-available
header button exposes it. Utility-pane collapse also applies at that breakpoint.
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
wallet uses the original 3D Monero mesh with a projected prototype card; exact
production shader/material matching remains outstanding.
The article editor currently shows Markdown text. Media-viewer controls currently
exercise fixture selection, not a full video pipeline. These remain parity work,
not features removed from the desktop product.

# Monogram Desktop

A native **GTK 4 / libadwaita** reimplementation of Monogram for Linux.
This first milestone is an interactive, **backend-free UI prototype** with
fictional in-memory conversations. It never signs in, contacts an XMPP server,
loads real encryption keys, places calls or opens a real wallet.

## Run with Nix

```sh
nix run
```

Open with the media utility pane visible:

```sh
nix run . -- --show-media
```

Development and verification:

```sh
nix develop
PYTHONPATH=src python -m monogram_desktop
nix flake check
nix build
```

The flake pins GTK, libadwaita, Python and PyGObject. Runtime UI dependencies are
packaged with the application. No global Python installation or system activation
is needed. The flake exports x86_64-linux and aarch64-linux; the current native
checks were run on x86_64-linux.

## Try it

- Select chats; Ctrl+F replaces the header with search and shows message/chat results.
- Filter All / Unread / Groups. Right click folders to edit chat-type/exclusion
  rules and explicit exceptions. Drag horizontally into gaps or empty space to
  reorder folders, or use their Move Left/Right menu actions.
- Compose locally, reply, edit, copy, forward and inspect message details. Right
  click a message for its mobile-style reaction/action menu. Ctrl-click and
  Shift-click select messages; the action bar copies, forwards or deletes them.
  Enter sends; Shift+Enter adds a line.
- Press **F9** for emoji, stickers, GIFs and Monero controls in a **right-hand utility pane**.
  Emoji insert into the composer; sticker/GIF tiles create local fixture messages.
  The grids show six emoji/four stickers per row; GIF tiles use bundled
  animated artwork. No remote GIF search runs.
- Choose **Attach → Photo or Video / File** to use `Gtk.FileDialog`. The packaged
  app requests the desktop file portal; selected filenames appear as removable
  attachment chips. No selected file contents are uploaded or copied to a cache.
- Open the Requests button beside the main menu, or tap story avatars above the chat list.
- Explore Contacts, Saved Messages, Wallet, Spaces and preferences from
  the main menu. Profile, security, device review, presence access, room creation,
  account/proxy setup and developer tools have local form previews.
- Switch the light/dark/system style in Preferences without changing the desktop's
  setting. The chat uses system accent colors; orange is reserved for Monero cards.
- Story avatars live above the chat list and open a timed overlay viewer. Hold
  to pause, use Space or the pause button, and use arrows/taps to navigate.
  My Story opens the photo portal and posts only an in-memory preview.
- Wallet balances and transactions are fictional. Create/remove/switch preview
  wallets and explore send/request review. There is no signing, seed generation,
  currency endpoint, blockchain scanning or payment backend.

## Feature parity contract

**100% Monogram feature parity is the product goal, not the current prototype's
completion claim.** The backend-free milestone establishes the desktop layout
and interaction vocabulary. See [FEATURE_PARITY.md](docs/FEATURE_PARITY.md) for
coverage, desktop adaptations and outstanding work, and
[ANDROID_SOURCE_INDEX.md](docs/ANDROID_SOURCE_INDEX.md) for the pinned Android
feature-note inventory. Existing backend behavior must not be silently dropped
when integration starts.

[DESIGN.md](docs/DESIGN.md) records the GNOME HIG and Nautilus source references.
This is real GTK: no WebView, browser shell, Android emulator, Telegram account
or embedded backend is involved. Backend selection remains a separate decision.

The prototype code is GPL-2.0-or-later. The imported Monero rendering components
are GPL-3.0-or-later; the combined build is distributed under GPL-3.0-or-later.
See [LICENSE](LICENSE), [GPL-3.0](LICENSES/GPL-3.0-or-later.txt), and
[asset notices](src/monogram_desktop/assets/NOTICE.md).

See [UI_REVIEW.md](docs/UI_REVIEW.md) for review corrections, verification and prototype limits.

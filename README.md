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

- Select chats, search messages and filter All / Unread / Groups.
- Compose locally, reply, edit, copy, forward and inspect message details. Right
  click a message for its standard popover menu. Enter sends; Shift+Enter adds a line.
- Press **F9** for emoji, stickers and GIF tiles in a **right-hand utility pane**.
  Emoji insert into the composer; sticker/GIF tiles create local fixture messages.
  The GIF tiles use bundled animated artwork; no remote GIF search runs.
- Choose **Attach → Photo or Video / File** to use `Gtk.FileDialog`. The packaged
  app requests the desktop file portal; selected filenames appear as removable
  attachment chips. No selected file contents are uploaded or copied to a cache.
- Open the prominent Requests button beside New Conversation.
- Explore Contacts, Saved Messages, Wallet, Stories, Spaces and preferences from
  the main menu. Profile, security, device review, presence access, room creation,
  account/proxy setup and developer tools have local form previews.
- Switch the light/dark/system style in Preferences without changing the desktop's
  setting. The chat uses system accent colors; orange is reserved for Monero cards.
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

GPL-2.0-or-later. See [LICENSE](LICENSE). The prototype's icon and landscape
illustration are original vector assets included under the same license.

See [UI_REVIEW.md](docs/UI_REVIEW.md) for review corrections and the outstanding exact wallet-shader port.

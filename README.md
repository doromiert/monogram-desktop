# Monogram Desktop

A native **Rust / GTK 4 / libadwaita** reimplementation of Monogram for Linux.
The app now includes **initial live XMPP direct-text messaging** and a separate
offline demo with fictional conversations. Live mode supports verified TLS login,
roster/presence, text, replies, carbons, archive history, receipts and local drafts.
**OMEMO 2 direct messages are available through explicit setup and fingerprint review.**
Live direct chats also support structured polls, corrections/reactions/retractions,
attributed text forwarding, and generic file uploads/downloads with encrypted
metadata and native progress controls. Rich media playback, calls, groups and
wallets remain offline demo surfaces. See the [parity ledger](docs/FEATURE_PARITY.md).

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
cargo run
cargo test
nix flake check
nix build
```

The flake pins Rust, GTK and libadwaita; Cargo.lock pins Rust dependencies.
Application logic and rendering run in a native Rust executable. A Python
standard-library script in tests/ launches disposable Prosody verification fixtures.
No system activation is needed. The flake exports x86_64-linux and aarch64-linux; the current native
checks were run on x86_64-linux.

## Try it

- On first launch, choose **Sign In** to connect an XMPP account. You can also add an account from **Main Menu → Accounts → Add Account**.
  Use New Conversation to enter another person's bare address. Passwords stay in
  memory; reconnect after restart requires entering them again. Accounts keep
  separate local history/drafts. Open Offline Demo returns to the fictional data.
  See [LIVE_XMPP.md](docs/LIVE_XMPP.md) for behavior, storage and current limits.
- Select chats; Ctrl+F replaces the header with search and shows message/chat results.
- Filter All / Unread / Groups. Right click folders to edit chat-type/exclusion
  rules and explicit exceptions. Drag horizontally into gaps or empty space to
  reorder folders, or use their Move Left/Right menu actions. Folder switches
  animate horizontally. Vertical wheel input scrolls both folder and story strips
  horizontally; horizontal trackpad input continues to work.
- Compose, reply, copy, forward and inspect message details. Right
  click a message for its mobile-style reaction/action menu. Ctrl-click and
  Shift-click select messages; the action bar copies, forwards or deletes them.
  Enter sends; Shift+Enter adds a line.
- Pin/archive/mark unread from a chat's menu; open Archived Chats to unarchive.
  Live text drafts and reply targets survive switching chats and restarting.
- Open **Conversation Security** to set the sending policy and review Legacy
  OMEMO and OMEMO 2 fingerprints separately. Automatic checks OMEMO 2, then
  Legacy OMEMO, and uses plaintext only when both are definitively unavailable.
  Incoming ciphertext uses its own protocol regardless of the sending setting.
  See [desktop encryption](docs/OMEMO2.md) and [Legacy OMEMO](docs/LEGACY_OMEMO.md).
- The controls below include the separate offline demo. Live stories, groups,
  calls and wallet operations remain outstanding.
- Press **F9** for emoji, stickers, GIFs and Monero controls in a **right-hand utility pane**.
  Emoji insert into the composer; sticker/GIF tiles create local fixture messages.
  The grids show six emoji/four stickers per row; GIF tiles use bundled
  animated artwork. No remote GIF search runs.
- Choose **Attach → Photo or Video / File** to use `Gtk.FileDialog`. The packaged
  app requests the desktop file portal; selected filenames appear as removable
  attachment chips. No selected file contents are uploaded or copied to a cache.
- Choose **Conversation Info → Shared Media** for a three-column sidebar grid;
  selecting a thumbnail opens the expanded viewer with its return animation.
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
completion claim.** The offline milestone established the desktop layout; the
XMPP backend adds real text messaging and explicit OMEMO 2 text/replies. See [FEATURE_PARITY.md](docs/FEATURE_PARITY.md) for
coverage, desktop adaptations and outstanding work, and
[ANDROID_SOURCE_INDEX.md](docs/ANDROID_SOURCE_INDEX.md) for the pinned Android
feature-note inventory. Existing backend behavior must not be silently dropped
when integration starts.

[DESIGN.md](docs/DESIGN.md) records the GNOME HIG and Nautilus source references.
This is real GTK: no WebView, browser shell, Android emulator, Telegram account
or embedded Android backend is involved. The desktop transport uses tokio-xmpp for verified TLS, SASL and XML/stanza handling.

The prototype code is GPL-2.0-or-later. The imported Monero rendering components
are GPL-3.0-or-later; libomemo-c is GPL-3.0-only. The combined build uses GPL-3.0.
The OMEMO 2 backend also reuses mobile's GPL-3.0-or-later native adapter/provider;
see [native component notices](native/NOTICE.md) and its [integration status](docs/OMEMO2.md).
See [LICENSE](LICENSE), [GPL-3.0](LICENSES/GPL-3.0-or-later.txt), and
[asset notices](assets/NOTICE.md).

See [UI_REVIEW.md](docs/UI_REVIEW.md) for review corrections, verification and prototype limits.

# Monogram Desktop

A native **Rust / GTK 4 / libadwaita** reimplementation of Monogram for Linux.
Live mode supports TLS sign-in, saved credentials, direct and bookmarked MUC
conversations, Legacy OMEMO and OMEMO 2, history, replies, message actions and
cached media playback. An optional offline demo uses fictional conversations.
Full mobile parity remains unfinished; see the [parity ledger](docs/FEATURE_PARITY.md).

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

Nix builds compiled dependencies separately with Crane. App-only source changes
reuse those artifacts; dependency manifests, Cargo.lock and vendored XMPP changes
invalidate them. The initial cache fill still compiles dependencies. `nix build`
builds the application; `nix flake check` opts into the verification suite.

The flake pins Rust, GTK and libadwaita; Cargo.lock pins Rust dependencies.
Application logic and rendering run in a native Rust executable. A Python
standard-library script in tests/ launches disposable Prosody verification fixtures.
No system activation is needed. The flake exports x86_64-linux and aarch64-linux; the current native
checks were run on x86_64-linux.

## Arch Linux

The root `PKGBUILD` builds the Git version, including the supplied temporary icon,
pinned encryption backend, sandboxed sticker renderer and JLaTeXMath helper:

```sh
git clone https://github.com/doromiert/monogram-desktop.git
cd monogram-desktop
makepkg -si
```

Run `makepkg` as your normal user on an up-to-date Arch installation with
`base-devel` installed. Cargo downloads locked dependencies during preparation;
compilation uses `--frozen`. A Secret Service provider is needed for saved
credentials and the OMEMO vault. KeePassXC is optional; see [KeePass setup](docs/KEEPASS.md).
The recipe’s metadata, checksums and native helper builds were verified on
x86_64 Linux; a complete Arch clean-chroot build and aarch64 run remain unverified.

## Try it

- On first launch, choose **Sign In** to connect an XMPP account. You can also add an account from **Main Menu → Accounts → Add Account**.
  Use New Conversation to enter another person's bare address. Saved passwords use Secret Service for automatic reconnect. Accounts keep
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

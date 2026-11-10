# Desktop prototype review pass

Acceptance references are the user's 21-item review and follow-up screenshots.

The conversation title is left aligned at native type/avatar sizes inside stock
Adwaita chrome. Header search follows Nautilus's title-widget Gtk.Stack flow;
search, emoji and call shortcut buttons were removed from the conversation header.
Preferences lives in the main menu. The left Monero shortcut and account footer
are removed. Chat/folder selection uses neutral gray. Requests use centered
native actions and a small badge. Folder editing uses grouped preference rows
with avatar/check controls and a header Save action. Folder rules follow mobile chat-type, read/muted/archived exclusions and explicit
always/never exceptions; folders reorder horizontally through gaps/free space.
Folder and chat context menus, inline message editing and native Adw.ShortcutsDialog are present.

The composer has an optically centered external paper plane, symmetric 8px outer
padding and an 8px gap. The central header and timeline share a background.
Message author, timestamp and encryption indicator share a centered top row;
metadata is aligned right. Replies are readable, clickable scroll targets.
Reactions toggle, photos keep their aspect/content height, and poll options use
left-aligned labels with right-aligned counts.

The full-height outer right pane includes its own top bar and continuous border.
Its close control uses the native sidebar icon. Emoji/Stickers/GIFs/Monero tabs
live in that top bar; the windowed layout collapses sidebars independently.
Emoji/sticker grids use six/four columns; their native buttons own the single
hover/focus highlight. GIF previews fill native grid tiles and are local artwork. Monero's sidebar card is constrained to
a medium centered preview containing only name, amount and fiat amount, separate from the full Wallet page.

The wallet imports the original Android card/edge and Monero meshes and original
GLSL materials. Gtk.GLArea hosts the source camera, lighting, engraving, insert,
flecks and Monero shader; front text is a projected face texture. Rendering uses
physical device pixels, up-to-4x MSAA, text textures matched to the actual projected device-pixel footprint and
512px mipmapped logo textures, with
overscan checked at all supported tilt extremes. Separate native accessible
precision/QR controls follow the projected card. QR opens a scannable *preview*
payload; no real receiving wallet/address exists. Wallet switching uses animated
left/right transitions and reuses card widgets. Cairo/Skia font and antialiasing
can differ; text, numbers and addresses remain fictional.

The media viewer uses a translucent black overlay, edge navigation, a thumbnail
fly-in and reverse fly-out. It has no Info button or header divider. Images expand
to their allocation; fullscreen removes margins and fits the display. Other
fixture media without a corresponding chat thumbnail close with a fade.

Stories follow the Android chat-list flow instead of a separate page: avatar strip,
unread/seen rings, collapsed header avatars, direct timed overlay viewer,
pause/hold/next/previous, reactions and local private replies. My Story invokes
the photo portal and creates an in-memory story preview. Real expiration, archive,
profile saving, video and XEP-0501 transport remain future backend work.

Verification uses model/amount/projection tests plus native GTK/OpenGL smoke checks
under isolated Xvfb with fatal GTK criticals. Checks include native dialogs,
folder controls, amount insertion, photo bounds, metadata centering, original GL
shader rendering, story navigation and media close-animation completion. Approved
captures contain only the prototype's fictional data; no phone/desktop capture.
All transport, keys, funds and persistent data remain outside this milestone.

Multi-message selection uses native Gtk.ListBox rows, Ctrl/Shift-click, a Select
menu action and a bulk action bar. The message popover follows mobile's reaction
strip, conditional text actions and equally wide reply/delete/copy/forward strip.
The two sidebars use independently scoped breakpoint bins and explicit minimum
sizes. Closed utility content is hidden after the native close animation to avoid
libadwaita 1.9's hidden-sidebar allocation issue when snapshotting an old layout.
Pointer checks verify horizontal dragging (including vertical pointer movement),
empty-space and between-folder drops, Ctrl/Shift selection and popup actions.

## Rust replacement and follow-up fixes

The native desktop is implemented in Rust. Python application modules are removed;
Git commit 0ae33bdf452323ec0ce546887c74595318198f55 retains the previous prototype.
The new app retains the existing SQLite schema, account ownership and privacy defaults.

A real wide-window photo gap was traced to height-for-width measurement against
the full row width despite a 420px thumbnail. Measurement now caps the photo's
width before calculating its height. Native tests assert compact photo rows and
inter-message spacing at 1920, 960 and 460px widths.

Shared Media opens a three-column square-thumbnail grid in the full-height utility
sidebar; only thumbnail activation opens the expanded viewer. The viewer uses the
selected conversation's media. Vertical mouse-wheel input scrolls folder and
story strips horizontally, with ordinary horizontal input preserved. Folder
changes animate left/right using Gtk.Stack's native transition.

Rust model/protocol/storage/amount/projection checks, fatal-critical GTK smoke,
real disposable Prosody/TLS exchange and native composer restoration checks are
part of the Nix package checks. Full Android feature parity remains open.

# Feature parity ledger

Target: all shipped Monogram behavior, with desktop-specific presentation.
Android baseline: `be6876c045163e32fe07a84416621770b0964b30` / 0.2.195.

**Live direct-text XMPP and explicit OMEMO 2 text/replies are implemented; full parity is open.**
The desktop implementation is Rust with GTK 4/libadwaita; its Python predecessor
is replaced, preserving the SQLite schema and account-specific data.
See [LIVE_XMPP.md](LIVE_XMPP.md) for verified behavior and security limits.
A form, switch or fixture is not a working server feature. "Preview" below means
an in-memory interaction or surface; "planned" means it is still absent.

| Feature family | Current prototype | Required for product parity |
| --- | --- | --- |
| Account onboarding, multiple accounts, reconnect and proxy | Verified-TLS login, cancellable session, automatic socket reconnect, saved account profiles with one active connection, generation-fenced callbacks | Simultaneous connections, keyring credentials, proxy and certificate-review UI, broader provider testing |
| Chat list, search, folders, unread/archived/saved chats | Live roster/chats, loaded-history search, durable local archive/pin/unread markers and folder rules/order, XMPP self-chat | Full database/server search, pin ordering, tags, account-wide archive synchronization |
| Text, composer, drafts | Live text, durable history, debounced text/reply drafts, explicit retry and receipt state | Durable automatic outbox/reconciliation, attachment/formatting drafts, `/me` actions |
| Message actions | Live reply/copy/text-forward/details, local delete/pin, selection and batch actions; edits/reactions remain demo-only | Live corrections/reactions/retractions, full backend permissions, replay/save/repeat |
| Rich formatting, articles, tables, spoilers | Markdown text editor / article fixture | Native rich rendering, tables, edit roundtrip, safe links and code-point ranges, partial/whole spoilers |
| Replies and quoted content | Live stable-ID references and Unicode-code-point fallback quoting/removal; sender-scoped loaded-history resolution | Complete alias recovery across unloaded history, MAM reply lookup, room author checks |
| Polls and reactions | Interactive fixture poll controls/reaction toggles; local poll form | Exact public reaction poll grammar, durable ballots, revotes, author closure and timestamps |
| Standard/custom emoji, stickers and GIFs | Standard emoji in live text; separate offline animated GIF/sticker fixtures | Live transfers/decoders, search/providers, packs/favorites/recents and full icon-pack replacement |
| Files and media | Portal selection → removable filenames; artwork/file fixtures | Encrypted streaming transfers, real progress/resume/cancel, thumbnails, captions, cache lifecycle and account fences |
| Viewer and playback | Three-column shared-media sidebar grid, thumbnail-triggered overlay, fly-in/fly-out and fullscreen fit | Correct media switching, bounded 11-item window, video/audio/voice/round-video, zoom, download and editor behavior |
| HTML widgets | Upstream sandbox source audited; no renderer/runtime integrated | Resolve upstream licensing, GTK host and compatible XMPP proposal; enforce accepted **16 KiB compressed** budget plus independent expansion/memory/execution bounds on both clients |
| Media authoring | Planned | Crop/trim/markup, compression, albums, camera capture and desktop permission/device flows |
| Locations and previews | Location card/form preview | Real safe map rendering, provider privacy, coordinates, live/location selection and valid fallback removal |
| OMEMO legacy and Monocles PQ in DMs/MUCs | Legacy/PQ remain unavailable; unsupported encrypted payloads show an unavailable placeholder, never a decrypted fallback | Exact separate key stores, authenticated content, trust/block, key change prompts and compatible wire protocols |
| OMEMO 2 direct messages | [Live explicit OMEMO 2](OMEMO2.md): native setup/review, durable identity/keyring, encrypted text/replies, live/carbon/MAM handling, journal/history/ack integration and trust retry | Automatic version/trust policies, event/cached review lifecycle, complete background retry, rich content, restore/migration/revocation and remaining mobile policies |
| Device and contact access control | Exact OMEMO 2 fingerprint review/deny and public key PEP setup; other access forms remain previews | Device revocation, legacy/PQ trust, presence authorization and ACL grant/revoke |
| Groups, MUCs, MIX and Spaces | Group creation and spaces/channel form/list previews | Real membership, roles/configuration, presence, bookmarks, pagination, history and transport-specific crypto |
| Invitations and contact requests | Live presence-subscription requests with explicit authorization; room invitations remain demo-only | Durable requests, room invitation permissions and complete account fencing/lifecycle |
| Presence and typing | Roster multi-resource status and live direct typing with expiry/privacy control; clears on disconnect | Group states, full carbons/delay/archive rejection and broader interoperability |
| Direct and MUJI group calls | Call-controls dialog preview | Real media transport, incoming calls, desktop device/portal permissions, mute/camera/speaker, lifecycle and group membership |
| Contacts and profiles | Contact list, editable profile preview | Real roster/local overrides, vCard fields, avatar publication/suggestions, colors, notes and blocking |
| Stories and persistent profile posts | Horizontal chat-list avatar strip and timed native overlay viewer | Correct PEP/Atom access, expiry, persistent saves, edits/retractions, media and profile editing |
| Notifications | Basic live desktop notifications, opt-in message previews, local chat mute and notification preferences | Actions/attention, mention matching, timed-mute expiry and background behavior |
| Read state and archives | Local unread state, paged direct MAM retrieval and account/peer/direction-scoped dedup | Private read synchronization, public read-receipt consent, account-wide recovery, room readers |
| Wallet overview / multiple wallets | Source 3D meshes/GLSL rendered in Rust, precision/QR controls, local add/remove/animated switch and activity fixtures | Production 3D card, secure keyring/SDK integration, unlock/lock, durable wallet slots and removal semantics |
| Wallet recovery and scan | Planned | Polyseed/default recovery, 2×8 words UI, restore/import, node configuration, parallel bounded scanning, checkpoints and useful progress |
| Send/request and fiat pricing | Local amount/currency/note/request-card forms | Exact atomic amounts, online fiat list/formatting/rates, address reservation, fees, confirmation and transaction history |
| Payment/claim links | No real link parsing or wallet material | Compatible formats, protected bearer secrets, external restore path, multi-wallet deposits, readiness/claimed status and independent verification |
| Background deposits / fee graph | Planned | Explicit fee approval/ASAP, configured-node history, job cancellation/uncertain broadcast safety and desktop background lifecycle |
| Preferences and appearance | Native live settings for style/emoji, typing, receipts and notification privacy; unsupported switches remain demo-only | Complete settings, themes/icons/layout, accessibility, translation and media/power behavior |
| Privacy/storage/history | Encrypted OMEMO 2 state; plaintext local chat/draft history; other switches remain previews | Honest cache/history clearing, retention, local encryption and backups as actually supported by Android |
| Developer/diagnostics/crashes | Discovery/ad-hoc form previews and message-detail dialog | Real disco/commands, sanitized diagnostics, crash handling, version updates and protocol test vectors |
| Desktop integration | Native window/menu/clipboard, portal-aware chooser, Nix package | Desktop sharing, drag/drop, search, notifications, MPRIS/media keys, accessibility/keyboard acceptance and distribution updates |

## Acceptance rule

An entry becomes complete only after its UI, backend behavior, protocol/security
invariants, persistence/lifecycle and desktop interactions are verified. Fixtures
and screenshots alone do not complete a row. Existing Android bugs or dormant
Telegram actions are not goals to copy; use the implemented Monogram behavior and
its documented limitations as the reference. Codecs explicitly marked unshipped
(such as collaborative checklists) are not falsely counted as shipped parity.

The accompanying [Android source index](ANDROID_SOURCE_INDEX.md) preserves every
feature document considered, including smaller behavior customizations. It is a
reference inventory; the table above groups them for implementation. Keep both
updated rather than dropping features because their mobile presentation differs.

## Next milestones

1. Broaden the initial direct-text backend's interoperability, history/outbox and
   account lifecycle coverage; close explicit gaps listed in LIVE_XMPP.md.
2. Extend the explicit OMEMO 2 direct-text path to remaining mobile encryption
   policies, legacy/PQ, groups and media. Encrypted sends never fall back to plaintext.
3. Implement media, groups/calls/profile and wallet systems with their safety
   invariants, then close each remaining ledger row with evidence.

Live account connections happen only through explicit sign-in. Never connect
fictional demo controls to a real transport or wallet to simulate completeness.
No credentials, real account data or wallet material are bundled with the app.

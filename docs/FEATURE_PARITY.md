# Feature parity ledger

Target: all shipped Monogram behavior, with desktop-specific presentation.
Android baseline: `be6876c045163e32fe07a84416621770b0964b30` / 0.2.195.

**Live direct text, Legacy OMEMO and OMEMO 2 are implemented. Full parity remains open.**
The desktop implementation is Rust with GTK 4/libadwaita; its Python predecessor
is replaced, preserving the SQLite schema and account-specific data.
See [LIVE_XMPP.md](LIVE_XMPP.md) for verified behavior and security limits.
A form, switch or fixture is not a working server feature. "Preview" below means
an in-memory interaction or surface; "planned" means it is still absent.

| Feature family | Current prototype | Required for product parity |
| --- | --- | --- |
| Account onboarding, multiple accounts, reconnect and proxy | Native first-run XMPP sign-in, verified TLS, cancellable session, automatic socket reconnect and keyring-backed sign-in, saved account profiles with one active connection, generation-fenced callbacks | In-app registration and invitations, simultaneous connections, proxy and certificate-review UI, broader provider testing |
| Chat list, search, folders, unread/archived/saved chats | Live roster/chats sorted by recency below pinned chats, local archive search, durable local archive/pin/unread markers and folder rules/order, XMPP self-chat | Server search, more local search results/filters, pin ordering, tags, account-wide archive synchronization |
| Text, composer, drafts | Live text, sender clustering, inline message status, retained media rows, reply cards, durable history, debounced text/reply drafts, explicit retry and receipt state | Durable automatic outbox/reconciliation, attachment/formatting drafts, `/me` actions |
| Message actions | Live reply/copy/[attributed text/poll forwarding and Hide Sender](FORWARDING.md)/details, local delete/pin, selection/batch actions and [direct corrections/reactions/retractions](MESSAGE_ACTIONS.md), including explicit OMEMO 2 | Room/attachment/rich forwarding actions, full backend permissions, complete reaction UI and wider interoperability |
| Rich formatting, articles, tables, spoilers | Markdown text editor / article fixture | Native rich rendering, tables, edit roundtrip, safe links and code-point ranges, partial/whole spoilers |
| Replies and quoted content | Live stable-ID references and Unicode-code-point fallback quoting/removal; sender-scoped loaded-history resolution | Complete alias recovery across unloaded history, MAM reply lookup, room author checks |
| Polls and reactions | [Structured direct polls](STRUCTURED_POLLS.md): native composer/results, durable ballots, revotes/withdrawal, author closure/cutoff, bodyless and OMEMO 2; historical canonical mobile body cards/votes/closure/attributed fresh forwarding; mobile structured send and durable metadata | Mobile fallback UI toggle, room backend, uncertain-send reconciliation and broader interoperability |
| Standard/custom emoji, stickers and GIFs | Standard emoji in live text, sandboxed animated image reception and borderless Legacy/structured sticker hints; offline picker fixtures | Sticker/GIF search/providers, packs/favorites/recents and full icon-pack replacement |
| Files and media | [Live direct SFS/OOB reception and authenticated HTTPS downloads](FILE_TRANSFERS.md), automatic visible image downloads up to 10 MiB for trusted chats, inline animated images and sticker hints, account-scoped cache and plaintext/OMEMO 2 captions; XEP-0363 uploads with immutable snapshots, bounded queue, Cancel/Retry and interrupted-draft recovery | Voice thumbnails, complete content-type upgrades, adjustable download policy, durable jobs, complete cache lifecycle, caption edits/forwarding and room transport |
| Viewer and playback | Live square shared-photo tiles in a three-by-three first page, sandboxed inline photos, thumbnail-triggered overlay, fly-in/fly-out, fullscreen fit with idle control hiding and gradients, inline GIF/video playback and a 21-item window (selected photo plus up to 10 on each side) | Complete video/audio/voice/round-video controls, zoom and editor behavior |
| HTML widgets | Upstream sandbox source audited; no renderer/runtime integrated | Resolve upstream licensing, GTK host and compatible XMPP proposal; enforce accepted **16 KiB compressed** budget plus independent expansion/memory/execution bounds on both clients |
| Media authoring | Planned | Crop/trim/markup, compression, albums, camera capture and desktop permission/device flows |
| Locations and previews | Location card/form preview | Real safe map rendering, provider privacy, coordinates, live/location selection and valid fallback removal |
| Legacy OMEMO and Monocles PQ in DMs/MUCs | [Live Legacy OMEMO direct text](LEGACY_OMEMO.md), separate durable identity/ratchet/trust, live/carbon/MAM receive and per-contact opt-in blind trust | Legacy attachments, rich actions, room encryption, PQ, receive-only recovery and independent-client coverage |
| OMEMO 2 direct messages | [Live OMEMO 2](OMEMO2.md): independent incoming decryption, sending selector and both-version fingerprints, automatic 24-hour selection, durable identity/keyring, encrypted text/replies, live/carbon/MAM and journal recovery | PEP event/cached review lifecycle, complete background retry, rich content, restore/migration/revocation and remaining mobile policies |
| Device and contact access control | Both standard OMEMO fingerprint reviews, per-contact opt-in blind trust and public key PEP setup | Device revocation, PQ trust, presence authorization and ACL grant/revoke |
| Groups, MUCs, MIX and Spaces | Live bookmarked MUC discovery, autojoin, join presence, plain text and room MAM paging, plus group creation and spaces/channel form/list previews | Bookmark editing, full membership and roles, room encryption, live MIX and Spaces |
| Invitations and contact requests | Live presence-subscription requests with explicit authorization; room invitations remain demo-only | Durable requests, room invitation permissions and complete account fencing/lifecycle |
| Presence and typing | Roster multi-resource status and live direct typing with expiry/privacy control; clears on disconnect | Group states, full carbons/delay/archive rejection and broader interoperability |
| Direct and MUJI group calls | Call-controls dialog preview | Real media transport, incoming calls, desktop device/portal permissions, mute/camera/speaker, lifecycle and group membership |
| Contacts and profiles | Live roster and verified direct-chat PEP/vCard avatars with a bounded account disk cache and offline restoration, editable profile preview | Avatar publication, local overrides, vCard fields, avatar publication/suggestions, colors, notes and blocking |
| Stories and persistent profile posts | Horizontal chat-list avatar strip and timed native overlay viewer | Correct PEP/Atom access, expiry, persistent saves, edits/retractions, media and profile editing |
| Notifications | Basic live desktop notifications, opt-in message previews, local chat mute and notification preferences | Actions/attention, mention matching, timed-mute expiry and background behavior |
| Read state and archives | Local unread state, retained message widgets, roughly three screen heights cached on each side of the active viewport, bidirectional local paging, reply jumps and paged direct MAM with account/peer/direction-scoped dedup | Private read synchronization, public read-receipt consent, account-wide recovery, room readers |
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
2. Extend the standard OMEMO direct-text paths to remaining mobile encryption
   policies, legacy/PQ, groups and media. Encrypted sends never fall back to plaintext.
3. Implement media, groups/calls/profile and wallet systems with their safety
   invariants, then close each remaining ledger row with evidence.

Saved accounts reconnect through the system keyring. Never connect
fictional demo controls to a real transport or wallet to simulate completeness.
No credentials, real account data or wallet material are bundled with the app.

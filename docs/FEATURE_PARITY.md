# Feature parity ledger

Target: all shipped Monogram behavior, with desktop-specific presentation.
Android baseline: `ec4a3fa3e9f7167e7e4976427dee290d3bbc3e02` / 0.2.197.

**Live direct text, Legacy OMEMO and OMEMO 2 are implemented. Full parity remains open.**
The desktop implementation is Rust with GTK 4/libadwaita; its Python predecessor
is replaced, preserving the SQLite schema and account-specific data.
See [LIVE_XMPP.md](LIVE_XMPP.md) for verified behavior and security limits.
A form, switch or fixture is not a working server feature. "Preview" below means
an in-memory interaction or surface; "planned" means it is still absent.

| Feature family | Current prototype | Required for product parity |
| --- | --- | --- |
| Account onboarding, multiple accounts, reconnect and proxy | Native first-run XMPP sign-in, verified TLS, cancellable session, automatic socket reconnect and keyring-backed sign-in, saved account profiles with one active connection, generation-fenced callbacks | In-app registration and invitations, simultaneous connections, proxy and certificate-review UI, broader provider testing |
| Chat list, search, folders, unread/archived/saved chats | Live roster/chats sorted by recency below pinned chats, local archive search, durable local archive/pin/unread markers and folder rules/order, XMPP self-chat; restored room join failures stay in room status with stale-operation fencing, and failed background bookmark loads retain cached choices | Server search, more local search results/filters, pin ordering, tags, account-wide archive synchronization |
| Text, composer, drafts | Live text, sender clustering, inline message status, retained media rows, reply cards, durable history, debounced text/reply drafts, pasted/picked media staged inside the filled composer as removable cards with filename overlays until Send, native shaped-line sizing with capped scrolling and Escape clearing text while retaining attachments, centered full-opacity XEP-0245 `/me` action pills preserving original bodies, optimistic durable text/media queue before encryption checks with offline/reconnect flushing, explicit retry and receipt state | Ambiguous-send reconciliation, attachment/formatting drafts |
| Message actions | Live reply/copy/[attributed text/poll forwarding and Hide Sender](FORWARDING.md)/details, local delete/pin, selection/batch actions and [direct corrections/reactions/retractions](MESSAGE_ACTIONS.md), including explicit OMEMO 2; pinned-first compact reactions, continuous searchable emoji sections, reversible expansion from the compact pill, and wrapped timestamps sharing reaction rows | Room/attachment/rich forwarding actions, full backend permissions and wider interoperability |
| Rich formatting, articles, tables, spoilers | Markdown text editor / article fixture; selectable native HTTP/HTTPS/bare URL links, including inline emoji and cached-preview rows; bounded opt-in HTTPS preview fetching, account cache and retained title/description/thumbnail cards, verified with a real public fetch | Native rich rendering, tables, edit roundtrip, complete formatting/code entity ranges, partial/whole spoilers; mobile empty-OG fallback fix |
| Replies and quoted content | Live stable-ID references and Unicode-code-point fallback quoting/removal; sender-scoped loaded-history resolution | Complete alias recovery across unloaded history, MAM reply lookup, room author checks |
| Polls and reactions | [Structured direct polls](STRUCTURED_POLLS.md): native composer/results, durable ballots, revotes/withdrawal, author closure/cutoff, bodyless and OMEMO 2; historical canonical mobile body cards/votes/closure/attributed fresh forwarding; mobile structured send and durable metadata | Mobile fallback UI toggle, room backend, uncertain-send reconciliation and broader interoperability |
| Standard/custom emoji, stickers and GIFs | Native colon completion and Ctrl+; chooser, searchable mobile category artwork, frequency-ranked built-in recents and shared persistent favorites/pins, custom packs in Emoji and separate sticker packs, local/XMSP/Telegram pack import and native Android XMSP export verified through SAF into the desktop library, persistent native desktop favorite drag/Alt+Arrow reordering with mixed built-in/custom emoji rows and independent recents, custom inline artwork with Unicode fallback, sticker/GIF favorites/recents, explicit-key GIPHY search with a keyless Add key action, [Galeria-derived justified GIF grid](GIF_GRID.md) with viewport controls, overlay favorites and automatic offset paging, bounded looping raster/TGS playback | Mobile favorite drag reordering, mobile pack-export process-death restoration/large-copy lifecycle, real-provider acceptance with configured GIPHY/Telegram credentials, complete pack management UI, WebM sticker coverage and full icon-pack replacement |
| Files and media | [Live direct/room SFS/OOB reception and authenticated HTTPS downloads](FILE_TRANSFERS.md), automatic visible image downloads up to 10 MiB for trusted chats, inline animated images and sticker hints, account-scoped originals and derived thumbnail cache, prepared-video reuse, sized progress/cancel placeholders and plaintext/OMEMO 2 captions plus durable Legacy caption companions displayed with their media; direct and room Legacy/plain XEP-0363 uploads with immutable snapshots, bounded queue, Cancel/Retry and interrupted-draft recovery | Transient GTK sizing warning during compact sticker loading, voice thumbnails, complete content-type upgrades, adjustable download policy, durable jobs, complete cache lifecycle, caption edits/forwarding, albums and PQ room transport |
| Viewer and playback | Live square three-column shared-media grid with automatic nine-item paging, underfill loading, exhaustion/retry feedback and conversation-scoped refresh, sandboxed inline photos, thumbnail-triggered overlay, fly-in/fly-out, fullscreen fit with idle control hiding and gradients, shared image/GIF/video focus with Galeria’s actual Clapper player/SimpleControls and arc transitions, pointer-anchored wheel/pinch zoom and bounded drag panning, always-looping GIF/animated-sticker playback, verified circular round-video previews with inline audible play/pause, sticker-free media navigation and a 21-item window (selected photo plus up to 10 on each side) | Complete playback preferences, audio/voice controls, round-video radial seeking and remaining-time labels, and editor behavior |
| HTML widgets | Upstream sandbox source audited; no renderer/runtime integrated | Resolve upstream licensing, GTK host and compatible XMPP proposal; enforce accepted **16 KiB compressed** budget plus independent expansion/memory/execution bounds on both clients |
| Media authoring | Planned | Crop/trim/markup, compression, albums, camera capture and desktop permission/device flows |
| Locations and previews | Location card/form preview | Real safe map rendering, provider privacy, coordinates, live/location selection and valid fallback removal |
| Legacy OMEMO in DMs/MUCs | [Live Legacy OMEMO direct and room text/replies/attachments](LEGACY_OMEMO.md), authenticated same-author cross-device direct XEP-0424 retractions and consented public XEP-0444 reactions, complete member/own-device targets, separate durable identity/ratchet/trust, live/carbon/MAM receive and per-contact/per-room opt-in blind trust including conversation-scoped own devices | Rich actions, embedded-thumbnail receive, broader recovery and independent mobile room coverage |
| OMEMO2 / Monocles PQ | Sending choice OMEMO2 uses the pinned Monocles hybrid PQ backend with separate encrypted identity/ratchet/trust and durable inbox/outbox; Legacy, Classic and PQ incoming decryption and fingerprint reviews stay independent of sending choice. Actual mobile Java/JNI bidirectional/restart/tamper checks, isolated TLS actor/PEP send/reply and live provider bundle publication verified; Classic [receive compatibility](OMEMO2.md) retained | PQ room runtime acceptance, complete background retry and rich content, restore/migration/revocation and remaining mobile policies |
| Device and contact access control | Separate Legacy, Classic and full hybrid PQ fingerprint reviews, per-contact opt-in authenticated blind trust, own-device review plus conversation-scoped automatic PQ own-device approval on both clients when blind trust is enabled (explicit blocks preserved), and public key PEP setup; live RFC-6121 directional contact request/allow/accept/deny/cancel/revoke with account-local durable auto-approval, server-confirmed status, metadata-preserving revocation and reactive native controls | Matching mobile contextual own-device blind trust in Legacy/Classic, live Legacy/PQ room trust acceptance, device revocation and broader trust lifecycle acceptance |
| Groups, MUCs, MIX and Spaces | Live bookmarked MUC discovery, autojoin, join presence, plain/Legacy-encrypted text, replies and attachments, room MAM paging, complete offline member discovery, fresh before/after encryption checks and revision-fenced real-JID contexts, real private-group creation/service discovery and explicit room join/leave, durable nickname/autojoin choices, private XEP-0402 bookmark writes and separate keyring room passwords; native public-room browser using service discovery, RSM paging/unpaged fallback, cumulative failures and explicit Join; spaces/channel form/list previews | Room details, full membership and role editing, room rich content/PQ encryption, live MIX and Spaces |
| Invitations and contact requests | Durable account-scoped contact requests with paged review, acknowledged accept-and-request-back responses, one-way permission controls and server withdrawal/authorization reconciliation; mediated/direct room invitation inbox, encrypted invitation passwords, confirmed join/local dismissal, revision guards and durable keyring cleanup; empty Requests and invitations controls hidden on both clients; created groups grant selected members and send mediated invitations with per-recipient failures | Cross-device dismissal and full invitation management |
| Presence and typing | Roster-authorized multi-resource status with unknown/unshared presence hidden, stale startup status cleared, and live direct typing with expiry/privacy control; clears on disconnect | Group states, full carbons/delay/archive rejection and broader interoperability |
| Direct and MUJI group calls | Call-controls dialog preview | Real media transport, incoming calls, desktop device/portal permissions, mute/camera/speaker, lifecycle and group membership |
| Contacts and profiles | Live roster and verified direct-chat PEP/vCard avatars with a bounded account disk cache and offline restoration, conversation info following chat/account selection, [server-enforced contact blocking](BLOCKING.md) with direct profile/list actions, cross-resource pushes and acknowledged offline cache, editable profile preview | Avatar publication, local overrides, vCard fields, avatar publication/suggestions, colors and notes |
| Stories and persistent profile posts | Horizontal chat-list avatar strip and timed native overlay viewer | Correct PEP/Atom access, expiry, persistent saves, edits/retractions, media and profile editing |
| Notifications | Basic live desktop notifications, opt-in message previews, local chat mute and notification preferences | Actions/attention, mention matching, timed-mute expiry and background behavior |
| Read state and archives | Visible-at-bottom local unread clearing, authenticated own-carbon read synchronization, default-off public XEP-0333 receipts with durable opt-in and monotonic read status, retained message widgets, bounded adaptive paging, roughly three screen heights cached on each side, reply jumps and paged direct MAM | Broader cross-client read interoperability, account-wide recovery and room readers |
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

Emoji/Stickers use a full-height native virtual section list: combined favorites/recents, installed packs, then built-in categories only for Emoji. Eight-column square emoji cells have equal horizontal/vertical gaps; emoji-only text messages use transparent bubbles, matching mobile TYPE_EMOJIS. Sticker search remains visible and picker panels share padding. Emoji/sticker category controls have matching native positions, insets and spacing; media tabs slide horizontally according to tab order. Loading GIF tiles have square corners. Square mobile-icon selectors scroll to sections; Settings stays fixed outside the horizontal strip and uses one popover surface. Pack previews decode on workers; GIF selection captures the active chat at the click and rejects completion after a chat/account change. Chat-switch/history-scroll responsiveness now uses worker-side caption/link preparation, bounded backend-event draining and at most two new native rows per frame, with pruning/read-state guarded until the batch finishes. The native paging/reply/incoming-update scenario passes; real-workload latency acceptance remains open. Managed joining of the reported room still times out although an isolated raw discovery/join succeeds.

## Next milestones

1. Broaden the initial direct-text backend's interoperability, history/outbox and
   account lifecycle coverage; close explicit gaps listed in LIVE_XMPP.md.
2. Extend the standard OMEMO direct-text paths to remaining mobile encryption
   policies, PQ group acceptance and rich media. Encrypted sends never fall back to plaintext.
3. Implement media, groups/calls/profile and wallet systems with their safety
   invariants, then close each remaining ledger row with evidence.

Saved accounts reconnect through the system keyring. Never connect
fictional demo controls to a real transport or wallet to simulate completeness.
No credentials, real account data or wallet material are bundled with the app.

Current concrete user-reported regressions and pending requests are tracked in
[REPORTED_ISSUES.txt](REPORTED_ISSUES.txt). Desktop favorite reordering passes actual
pointer drags in all three production views plus database reload; mobile
reordering remains open. This implementation is awaiting the next desktop package.

PQ recipient selection now follows mobile `XmppPqOmemo.targets`: explicit blocks
are checked before bundle retrieval; absent bundles are skipped unless a pinned
session remains usable. A required contact must retain at least one usable
device. This also applies to encrypted reaction payloads and own-device copies.
Native hybrid-crypto checks cover stale own/contact entries, existing sessions,
conversation blind trust and preserved blocks. Live user send acceptance remains
open; no real contact was messaged for verification.

The unrestricted battery-usage reminder is Android-only at the user’s explicit
request. Desktop has no corresponding Android power-exemption prompt.

Both clients render exact leading `/you ` and `/nar ` as action messages beside
`/me `, preserving their original bodies and ordinary encryption/persistence.
`/you ` displays literal `you`; `/nar ` displays only its suffix. These are local
presentation extensions, documented in [the proposal](PROPOSED_ACTION_COMMANDS.md).

2026-11-12 delivery: desktop8cdd88f adds actual mobile-style round-video seeking
and mute, shared quick-reaction button motion, the five-screen history cache,
and bounded transfer reconnection. Mobile0.2.202 has the same GET/immutable-wire
PUT recovery and retains its signing identity/native backend. Real-service cold
transfer checks pass on desktop and Android15 emulator; full parity and the
remaining user issues in REPORTED_ISSUES.txt remain open.

Desktop reaction follow-up `42a2a67` preserves the same actual quick buttons
through expansion, adds circular highlights and compact padding, and keeps the
popup top fixed until a progressive screen-edge clamp is needed. The virtual
viewport realizes 48 of 1,910 emoji buttons; native cold menu / first expansion
frame measure 144 / 50 ms. Reusing message rows and restoring logical focus
prevents the actual pointer add/remove scroll jump. Pointer, keyboard, wheel,
near-bottom and image-only inset checks pass. These local acceptance results do
not establish full client parity or live-contact reaction delivery.

Desktop follow-up `6a80eef` updates reaction chips in place, preserving image,
video/player and download widgets and avoiding reaction-triggered history loading.
Chip enter/exit ports mobile's 250 ms scale/opacity transition and exact Bézier;
Telegram document-driven TGS bursts remain a gap. Compact quick reactions have
6 px equal highlight insets and full pill ends. Primary dragging preserves text
selection inside a single message, then clears it and enters full-message brush
selection when crossing another row. Highlights join squarely; selection close
precedes the count. Actual native pointer, keyboard, remote-update, rapid-toggle,
scroll, reduced-motion and selection handoff checks pass in private fixtures.

Desktop selection follow-up `6b13456` adds whole-local-day selection without
mounting its unloaded rows, optional own-only deletion requests in mixed batches,
and 8 px highlight overhang while preserving bubble geometry. Confirmed local
scrubbing and durable retraction recording share one transaction; retry preserves
wire identity, encryption mode and sender fingerprint. Unsupported/unsent messages
remain local-only. Native 5,000-message select/delete acceptance retains 92 rows
and reports 10 ms feedback / 17 ms maximum frame gap. Server-only day history and
unsupported room retractions remain outside this behavior.

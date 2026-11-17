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
| LAN device sync | [QR and typed TOTP pairing, TLS 1.3/SPAKE2, encrypted portable records, mesh forwarding, account/credential, pack, folder, fingerprint and wallet-capsule adapters](DEVICE_SYNC.md) on both clients | Cancellation/progress and legacy-folder export covered by production UI/core/JNI tests; physical phone/LAN acceptance; desktop wallet capsules are securely stored/relayed but its pre-existing production wallet engine is still absent |
| KeePass credentials | [Android Autofill / KeePassDX and explicit encrypted KeePassXC fill/save](KEEPASS.md), per-account associations in the existing keyring, immediate waiting feedback and cancellation | Real KeePassDX provider acceptance on a phone; live KeePassXC GUI acceptance blocked by fixture startup crash |
| Text, composer, drafts | Live text, sender clustering, inline message status, retained media rows, reply cards, durable history, debounced text/reply/formatting drafts, custom composer layout with a Nautilus-style focus outline, [composer-to-bubble send transition](SEND_ANIMATION.md) with reduced-motion support, pasted/picked media staged inside the filled composer as removable cards with filename overlays until Send, native shaped-line sizing with capped scrolling and Escape clearing text while retaining attachments, centered full-opacity XEP-0245 `/me` action pills preserving original bodies, optimistic durable text/media queue before encryption checks with offline/reconnect flushing, explicit retry and receipt state | Ambiguous-send reconciliation, attachment drafts |
| Motion photos | [JPEG Motion Photo](MOTION_PHOTOS.md) still + embedded MP4 sent together through SFS/OMEMO; mobile native live-photo projection; desktop still-first click playback and original-still restoration | Real phone UI acceptance; HEIC/AVIF motion containers and separate Apple library pairs |
| Message actions | Live reply/copy/[attributed text/poll forwarding and Hide Sender](FORWARDING.md)/details, local delete/pin, selection/batch actions and [direct corrections/reactions/retractions](MESSAGE_ACTIONS.md), including explicit OMEMO 2; pinned-first compact reactions, continuous searchable emoji sections, reversible expansion from the compact pill, and timestamps sharing every nonempty reaction row | Room/attachment/rich forwarding actions, full backend permissions and wider interoperability |
| Rich formatting, articles, tables, spoilers | [Mobile-source formatting](FORMATTING.md): native selectable styled ranges, bounded Markdown blocks/tables/lists/details/footnotes, sandboxed native MicroTeX, whole/partial/media spoilers, composer formatting, saved formatting drafts, corrections, forwarding and captions; authenticated modern metadata and restricted Legacy hints | Real-contact formatted-message acceptance; Telegram-only embedded article media/widgets and custom-emoji article composition are outside mobile’s text-only Markdown transport |
| Replies and quoted content | Live stable-ID references and Unicode-code-point fallback quoting/removal, full-width two-line quote cards with word-boundary ellipses (over-wide words can split); sender-scoped loaded-history resolution and mobile-style 36 px cropped image thumbnails from authenticated media, including archived originals outside the viewport | Complete alias recovery across unloaded history, MAM reply lookup, room author checks |
| Polls and reactions | [Structured direct polls](STRUCTURED_POLLS.md): native composer/results, durable ballots, revotes/withdrawal, author closure/cutoff, bodyless and OMEMO 2; historical canonical mobile body cards/votes/closure/attributed fresh forwarding; mobile structured send and durable metadata | Mobile fallback UI toggle, room backend, uncertain-send reconciliation and broader interoperability |
| Standard/custom emoji, stickers and GIFs | Tab-only colon completion (Basic; Preferences → Behavior also offers Discord-style: two characters, Enter accepts, cursor before the last letter bypasses) and Ctrl+; chooser, searchable mobile category artwork, frequency-ranked built-in recents and shared persistent favorites/pins, custom packs in Emoji and separate sticker packs, local/XMSP/Telegram pack import and native Android XMSP export verified through SAF into the desktop library, persistent native desktop favorite drag/Alt+Arrow reordering with mixed built-in/custom emoji rows and independent recents, custom inline artwork with Unicode fallback, sticker/GIF favorites/recents, explicit-key GIPHY search with a keyless Add key action, [Galeria-derived justified GIF grid](GIF_GRID.md) with viewport controls, overlay favorites and automatic offset paging, bounded looping raster/TGS playback Preferences → Behavior → Stickers: the Stickers tab stays open after sending by default; switching it off closes the pane after a sent sticker unless Shift is held while clicking. | Mobile favorite drag reordering, mobile pack-export process-death restoration/large-copy lifecycle, real-provider acceptance with configured GIPHY/Telegram credentials, complete pack management UI, WebM sticker coverage and full icon-pack replacement |
| Files and media | [Live direct/room SFS/OOB reception and authenticated HTTPS downloads](FILE_TRANSFERS.md), automatic visible media downloads with a configurable 15 MiB default for trusted chats, inline animated images and sticker hints, account-scoped originals and derived thumbnail cache, prepared-video reuse, sized progress/cancel placeholders and plaintext/OMEMO 2 captions plus durable Legacy caption companions displayed with their media; direct and room Legacy/plain XEP-0363 uploads with immutable snapshots, bounded queue, Cancel/Retry and interrupted-draft recovery | Transient GTK sizing warning during compact sticker loading, voice thumbnails, complete content-type upgrades, adjustable download policy, durable jobs, complete cache lifecycle, caption edits/forwarding, albums and PQ room transport |
| Viewer and playback | Live square three-column shared-media grid with automatic nine-item paging, underfill loading, exhaustion/retry feedback and conversation-scoped refresh, sandboxed inline photos, thumbnail-triggered overlay, fly-in/fly-out, fullscreen fit with idle control hiding and gradients, shared image/GIF/video focus with Galeria’s actual Clapper player/SimpleControls and arc transitions, pointer-anchored wheel/pinch zoom and bounded drag panning, always-looping GIF/animated-sticker playback, verified circular round-video previews with inline audible play/pause, sticker-free media navigation and a 21-item window (selected photo plus up to 10 on each side) Playback refuses files that do not start like a binary media container (a manifest or playlist saved as `video/*` is not played), and GStreamer network sources and adaptive demuxers (HLS, DASH, Smooth Streaming) are disabled, so a media file can never make the client contact an address written inside it. | Complete playback preferences, audio/voice controls, round-video radial seeking and remaining-time labels, and editor behavior |
| HTML widgets | Upstream sandbox source audited; no renderer/runtime integrated | Resolve upstream licensing, GTK host and compatible XMPP proposal; enforce accepted **16 KiB compressed** budget plus independent expansion/memory/execution bounds on both clients |
| Client-side commands | Desktop-only `/neofetch` (not in mobile): typed in the composer and sent with Enter, it is replaced by a generated code block (model or motherboard, processor, memory, total disk, uptime, distribution, Monogram version and git commit) left in the composer to edit and send; the command itself is never sent. Never includes the user or host name; read from /proc and /sys without subprocesses. The commit is embedded at build time (`MONOGRAM_GIT_COMMIT` for builds without git) | More commands, command hints in the composer, an ASCII logo, auto-send option |
| Animated single emoji | [Noto animations](ANIMATED_EMOJI.md): a message that is exactly one bundled emoji plays its Lottie animation once in view and on click, rendered in the confined helper with emoji-only limits; curated 250-emoji pack (about 2.1 MB), optional full-set pack placeholder, reduced-motion respected | Skin-tone variants; a setting to turn it off separately from big emoji |
| Media authoring | [Plan model and limits](MEDIA_AUTHORING.md): validated crop/rotate/trim/scale plans with hard source, output and job-size bounds; GStreamer backend running in a bubblewrap-confined helper with a minimal plugin allowlist, header-bounded JPEG/PNG/WebP crop, rotate and scale with EXIF orientation, verified output and cancellation; Edit Picture dialog (crop with handles, rotate, crop shapes, numeric and keyboard crop) from the composer's attachment cards | Markup, video trim/compression with progress and cancel, albums, camera capture through the desktop portal |
| Locations and previews | Location card/form preview | Real safe map rendering, provider privacy, coordinates, live/location selection and valid fallback removal |
| Link previews | HTTPS-only text cards fetched through DNS-pinned, public-address-only requests, with `og:image` thumbnails decoded by the sandboxed image loader. Remote `og:video` is never downloaded or decoded, because that would run GStreamer on a stranger's bytes inside the main process; WebM sticker posters use a libvpx-only decoder allowlist | Mobile shows video thumbnails for pages and links that only offer `og:video`; desktop intentionally shows the text card without one until video decoding is sandboxed |
| Legacy OMEMO in DMs/MUCs | [Live Legacy OMEMO direct and room text/replies/attachments](LEGACY_OMEMO.md), authenticated same-author cross-device direct XEP-0424 retractions and consented public XEP-0444 reactions, complete member/own-device targets, separate durable identity/ratchet/trust, live/carbon/MAM receive and default-on contact and room fingerprint trust including conversation-scoped own devices | Rich actions, embedded-thumbnail receive, broader recovery and independent mobile room coverage |
| OMEMO2 / Monocles PQ | Sending choice OMEMO2 uses the pinned Monocles hybrid PQ backend with separate encrypted identity/ratchet/trust and durable inbox/outbox; Legacy, Classic and PQ incoming decryption and fingerprint reviews stay independent of sending choice. Actual mobile Java/JNI bidirectional/restart/tamper checks, isolated TLS actor/PEP send/reply and live provider bundle publication verified; Classic [receive compatibility](OMEMO2.md) retained | PQ room runtime acceptance, complete background retry and rich content, restore/migration/revocation and remaining mobile policies |
| Device and contact access control | Separate Legacy, Classic and full hybrid PQ fingerprint reviews, default-on contact/room authenticated blind trust, own-device review plus conversation-scoped automatic PQ own-device approval on both clients when blind trust is enabled (explicit blocks preserved), and public key PEP setup; live RFC-6121 directional contact request/allow/accept/deny/cancel/revoke with account-local durable auto-approval, server-confirmed status, metadata-preserving revocation and reactive native controls | Matching mobile contextual own-device blind trust in Legacy/Classic, live Legacy/PQ room trust acceptance, device revocation and broader trust lifecycle acceptance |
| Groups, MUCs, MIX and Spaces | Participants panel (header menu): virtualised Owners/Administrators/Participants sections with counts, live presence from occupant presence, search and invite by address; Live bookmarked MUC discovery, autojoin, join presence, plain/Legacy-encrypted text, replies and attachments, room MAM paging, complete offline member discovery, fresh before/after encryption checks and revision-fenced real-JID contexts, real private-group creation/service discovery and explicit room join/leave, durable nickname/autojoin choices, private XEP-0402 bookmark writes and separate keyring room passwords; native public-room browser using service discovery, RSM paging/unpaged fallback, cumulative failures and explicit Join; spaces/channel form/list previews | Room details, offline-member and role editing in the participants panel, room rich content/PQ encryption, live MIX and Spaces |
| Invitations and contact requests | Durable account-scoped contact requests with paged review, acknowledged accept-and-request-back responses, one-way permission controls and server withdrawal/authorization reconciliation; mediated/direct room invitation inbox, encrypted invitation passwords, confirmed join/local dismissal, revision guards and durable keyring cleanup; empty Requests and invitations controls hidden on both clients; created groups grant selected members and send mediated invitations with per-recipient failures | Cross-device dismissal and full invitation management |
| Presence and typing | Roster-authorized multi-resource status with unknown/unshared presence hidden, stale startup status cleared, and live direct typing with expiry/privacy control; clears on disconnect | Group states, full carbons/delay/archive rejection and broader interoperability |
| Direct and MUJI group calls | Call-controls dialog preview | Real media transport, incoming calls, desktop device/portal permissions, mute/camera/speaker, lifecycle and group membership |
| Contacts and profiles | Live roster and verified direct-chat PEP/vCard avatars with a bounded account disk cache and offline restoration, conversation info following chat/account selection, [server-enforced contact blocking](BLOCKING.md) with direct profile/list actions, cross-resource pushes and acknowledged offline cache, editable profile preview, account-scoped local nickname/notes/avatar overrides with actual-edit revisions across encrypted device sync, and normal-policy image suggestions | Own-avatar publication/accepting suggestions, vCard fields and colors |
| Stories and persistent profile posts | [Live XEP-0501/Atom stories](STORIES.md), confirmed contact-only PEP access, 24-hour expiry, cached metadata/seen state, recoverable progress/cancel/retry upload, image/animation/video viewing, stable-owner ordinary chat replies, own caption edits/retractions and local own history | Real-contact/server publication acceptance, permanent remote profile saves, viewer receipts, reactions and advanced editing |
| Notifications | Basic live desktop notifications, opt-in message previews, local chat mute and notification preferences | Actions/attention, mention matching, timed-mute expiry and background behavior |
| Read state and archives | Focused-chat local unread clearing (including older-history views), immediate send acknowledgement, authenticated own-carbon read synchronization, default-on direct public XEP-0333 receipts with durable opt-out and monotonic read status, retained message widgets, bounded adaptive paging, current screen plus two screens cached on each side, reply jumps and paged direct MAM | Broader cross-client read interoperability, account-wide recovery and room readers |
| Wallet overview / multiple wallets | Source 3D meshes/GLSL rendered in Rust, precision/QR controls, local add/remove/animated switch and activity fixtures | Production 3D card, secure keyring/SDK integration, unlock/lock, durable wallet slots and removal semantics |
| Wallet recovery and scan | Planned | Polyseed/default recovery, 2×8 words UI, restore/import, node configuration, parallel bounded scanning, checkpoints and useful progress |
| Send/request and fiat pricing | Local amount/currency/note/request-card forms | Exact atomic amounts, online fiat list/formatting/rates, address reservation, fees, confirmation and transaction history |
| Payment/claim links | No real link parsing or wallet material | Compatible formats, protected bearer secrets, external restore path, multi-wallet deposits, readiness/claimed status and independent verification |
| Background deposits / fee graph | Planned | Explicit fee approval/ASAP, configured-node history, job cancellation/uncertain broadcast safety and desktop background lifecycle |
| Preferences and appearance | Native live settings for style/emoji, typing, receipts and notification privacy; unsupported switches remain demo-only | Complete settings, themes/icons/layout, accessibility, translation and media/power behavior |
| Privacy/storage/history | Encrypted OMEMO 2 state; plaintext local chat/draft history; other switches remain previews | Honest cache/history clearing, retention, local encryption and backups as actually supported by Android |
| Developer/diagnostics/crashes | Discovery/ad-hoc form previews and message-detail dialog | Real disco/commands, sanitized diagnostics, crash handling, version updates and protocol test vectors |
| Desktop integration | Native window/menu/clipboard, About dialog, pressed reply/reaction feedback, smooth minimal reply navigation, portal-aware chooser, Nix package | Desktop sharing, drag/drop, search, notifications, MPRIS/media keys, accessibility/keyboard acceptance and distribution updates |

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

Emoji/Stickers use a full-height native virtual section list: combined favorites/recents, installed packs, then built-in categories only for Emoji. Eight-column square emoji cells have equal horizontal/vertical gaps; emoji-only text messages use transparent bubbles, matching mobile TYPE_EMOJIS. Sticker search remains visible and picker panels share padding. Emoji/sticker category controls have matching native positions, insets and spacing; media tabs slide horizontally according to tab order. Loading GIF tiles have square corners. Pack category icons use a fixed 24px layout independent of raster/video dimensions, including 512px animated icons; the populated strip remains 30px tall. Square mobile-icon selectors scroll to sections; Settings stays fixed outside the horizontal strip and uses one popover surface. Pack previews decode on workers; GIF selection captures the active chat at the click and rejects completion after a chat/account change. Chat-switch/history-scroll responsiveness now uses worker-side caption/link preparation, bounded backend-event draining and at most two new native rows per frame, with pruning/read-state guarded until the batch finishes. The native paging/reply/incoming-update scenario passes; real-workload latency acceptance remains open. Managed joining now accepts matching multi-resource MUC user items: the reported room confirms membership in 1.14 seconds in the production join path. Conflicting identities/permissions remain rejected. The visible room restores after reconnect and bookmark loading even with autojoin disabled; room history waits for confirmed self presence. Offline native coverage checks the join-to-history transition and immediate feedback.

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

Media-tab follow-up `73f4cf7` uses libadwaita’s native round mode control and
exposes Emoji, Stickers and GIFs only. The unavailable Monero tab/page is removed;
desktop still has no real wallet connection/settings, so demo wallet names never
expose it. Native pointer and keyboard captures verify the pill control.

Picker follow-up adds shared Favorites/Recent category navigation, Alt-click
favorite toggles, edge-to-edge scroll viewports with inner content padding, and
live favorite drag positions for Emoji/Stickers/GIFs. All can be reordered,
hidden and restored through folder controls. Reaction expansion interpolates
fixed pixel corners over 200ms. Stable Unicode emoji IDs resolve presentation
variants in both reaction and main-sidebar favorites while keeping existing
stored preferences compatible; emoji glyphs explicitly use Noto Color Emoji.
The favorite preview wrapper follows hidden content so it leaves no blank space.
Category highlights follow the first visible section; category clicks align its
heading to the top, including repeated clicks on the current category. Scroll
tracking retains weak references to bound list items and preserves virtualization.

Reaction morph corners use float GSK rounded clipping to preserve frame delivery
without restyling the virtual emoji subtree. Popup dismissal restores prior text
ranges after GTK focus restoration, preserving both unselected and partially
selected source messages. Native frame-budget and focus-restoration regression
checks pass; no protocol behavior changes.

Expand/collapse uses one persistent button/image at a fixed hit target. Quick
reactions retain their parent when possible and receive allocation immediately
when undocked, preventing missing-row frames at animation handoff.

Quick-reaction headings stay in the expanded list while only the shared emoji
row moves. Toolbar controls reveal after the row clears them. Expansion keeps the
existing popup origin and uses the native vertical slide constraint for the
minimum upward movement when space runs out, preserving horizontal position.
Quick glyphs interpolate from compact19px to the regular24px grid size on the
same transition progress, preserving the existing button and label widgets.

The Nix desktop package carries a private GTK4.22.4 clip-state correction for
missing top-edge frames during GPU reaction expansion. Three native before/after
recordings reproduce the failure and verify its removal; software/layout-only
fixtures do not establish GPU pixel correctness. Reaction UI and protocol behavior
are unchanged. System GTK remains unchanged; broader parity is still open.

## 2026-11-14 regression update

Android 0.2.205 and the matching desktop source add one-time, account-scoped
migration enabling read receipts, typing, embeds (including encrypted chats),
and direct-contact blind trust. Subsequent user opt-outs persist. Explicitly
blocked fingerprints are still rejected; room blind trust remains separate.
Desktop group read-receipt transmission remains an existing parity gap.

Device Sync is a native Preferences page with activatable action rows, and
accepts the desktop built-in All folder. Android camera/gallery pairing closes
the scanner before showing its result and requests credential authorization
when required. Invitation creation releases the storage lock before acquiring
the pairing lock. APK native-core bytes and signing certificate are verified.
Sticker exports use `.nzsp` and a dedicated MIME type; legacy `.xmsp` imports
remain supported. Monero settings offer the mobile node catalogue and separate
custom-node credentials, with a bundled symbolic icon. Wallet spending and the
other existing wallet-engine gaps above are unchanged.

Desktop custom emoji rows now own their asynchronous renderer, which holds a
weak widget reference and is released on row disposal. A production-path native
test decodes actual artwork, paints five slots, and checks disposal. Direct
Legacy inline artwork uses the negotiated [local proposal](PROPOSED_XEP_LEGACY_INLINE.md);
modern OMEMO formats stay compatible. Old received Legacy fallback-only messages
cannot recover artwork that was never transmitted. Mobile custom emoji have a
name editor independent of fallback emoji, with native name-based suggestions
and pack keywords. Native desktop picker name search remains available.

Native checks cover window/app icons, inline sync settings, node selection,
whole-link wrapping and compact document rows. Large Markdown lists recycle
native rows, and sticker worker reaping no longer blocks GTK during eviction.
See [PERF.md](../PERF.md) for the repeated upward-scroll workload and its limits.
Phone camera/credential UI and user-workload acceptance still require a phone;
no attached Android device was available during these checks.

Custom emoji additionally emit/read XEP-0514 spans with hash-verified inline XEP-0447/BoB media, preserving the previous authenticated Legacy envelope grammar. Remote-only emoji sources and empty insertion spans remain unsupported; see [inline emoji](INLINE_EMOJI.md). Mobile picker glyphs are padded to the supplied Telegram reference without shrinking grid hit targets.

2026-11-15 upload recovery: desktop and mobile0.2.215 use one fresh validated
XEP-0363 slot after transport ambiguity or HTTP409, retaining immutable wire
bytes and selecting the replacement GET URL. PUT acknowledgement keeps its
normal30s timeout; only downloads retain the short header reconnect policy.
Local Android-core and desktop TLS regressions pass. The user confirmed that
mobile0.2.215 successfully uploads the reported MP4; a temporary progress stall
around 6.8 MiB remains to be investigated.

Original-quality mobile video selection now preserves the source dimensions, matching desktop's existing original-file path. A non-aligned 854×480 input stays 854×480 instead of silently resizing to 852×480 and forcing a full transcode. Explicit lower-quality selections still resize; mobile sizing regressions and an installed-app reproduction cover the distinction.


MUC send preparation accepts promotions/demotions among member/admin/owner between live presence and a fresh complete affiliation list. It still requires the joined send role, private nonanonymous room discovery, all three authenticated lists, own membership, every visible occupant's identity in those lists, and matching membership revision. The reported room now installs its complete 18-member recipient set in 1.33 seconds, without sending a chat message. Incomplete, unidentified, revoked and stale recipient contexts remain blocked; there is no plaintext fallback for an encrypted room. A room the server reports as not members-only and non-anonymous is read from its disco features alone, before any member list is requested. If that room has **never** used encryption (no sticky `encrypted_history`, no encrypted setting, no own encrypted message in memory or the database) it is sent plain with the unencrypted-conversation notice. If it **has** used encryption, the send is refused with an explicit message and nothing is sent, because a server or room admin could otherwise switch a room's flags to downgrade it; the only way to unencrypted is choosing Unencrypted in Conversation Security. A cached plain choice is not trusted for such a room either, and timeouts and refused requests never downgrade a room.

Automatic room fingerprint trust now defaults on in desktop and mobile0.2.217, with an independent account-scoped upgrade for existing rooms. Subsequent opt-outs and blocked fingerprints remain authoritative. Closing Preferences, Shortcuts or another tracked modal resumes focused-chat read tracking; closed dialogs cannot leave a visible group's unread badge stuck.

2026-11-15 desktop follow-up: deletion preserves bottom-following and the first
surviving viewport anchor. Joined rooms allow automatic visible media downloads,
reevaluated as membership changes, with existing10MiB/type/integrity/cancellation
checks. Sticker replies now use the same verified image thumbnail path as mobile
and sticker previews use a readable label rather than a generatedWebP filename.
Native deletion anchor tests and download-policy/reply tests cover these paths.
The unused DeveloperTools preview entry was removed from the hamburger menu.


Contact override sync uses per-account/per-contact portable records with actual user-edit revisions, exact-version acknowledgements and explicit null-avatar clears. Nicknames, notes and PNG bytes cross encrypted Device Sync; local paths and remote roster/profile writes do not. Two Android adapter checks, desktop private-store coverage, 19 shared-core checks and a production JNI paired-TLS contact transfer/version fixture pass. Mobile0.2.218 requires the compatible desktop contact-record update.

Desktop native follow-up verification: ManagePacks lists every pack with install,
edit and order controls; each picker section has its own edit button. Connected
packs offer explicit local editing instead of disabled actions. Native pointer
tests cover category-bar drag, manager drag, sticker drag/rename and700-item
resource bounds (maxGTKgap82ms); high-contrast run passes. Editor toolbar uses
flat native styling and only one hover background. The text-only Monero tab opens
the existing interactive 3D wallet card with real synced wallet names and receiving
addresses; configuration remains in Preferences. Unavailable balances are explicit,
and account switches remove the old card.
This does not add desktop wallet spending. Local contact UI and paired transfer
are documented in [LOCAL_CONTACTS.md](LOCAL_CONTACTS.md). Allreportedfollow-up
flows have controls and sub400ms feedback checks; actual user-account Stories
publication and manualphone↔desktop contact editing were not performed.

Both clients keep ordinary files manual: selecting Download opens the native save
picker before transfer; cancelling it starts no download. Photos, videos, audio and
stickers retain automatic download policies. Desktop has a native Preferences switch
and adjustable per-file media limit (default 15 MB); mobile retains its network/type
policy UI. Explicit sticker-pack downloads still open the existing pack preview.
Legacy Monocles reply fallback quotes may include a blank separator; desktop now
accepts that separator like mobile while rejecting nonquote lines inside the range.

Mobile 0.2.221 adds bounded first-attempt body/acknowledgement recovery and
accepted-object confirmation over HTTPS. The intermittent real-network initial
stall remains unresolved; desktop real-world reproduction and matching recovery
behavior require further evidence. Track this explicitly rather than claiming
that faster retries establish cross-client root-cause parity.


Desktop pack-selector artwork now uses looping raster/TGS/WebM playback rather
than first-frame textures, with offscreen/unmapped playback paused. The native
WebM pack-bar test verifies a live MediaStream and a 48-sticker grid remains
responsive (maximum heartbeat gap41ms); offscreen TGS reopen coverage passes.
The native sync adapter imports mobile-format sticker favorites/recents by
stable UUID and preserves them in the desktop library. Physical phone↔PC
favorites/recents transfer is now verified: phone revision1794843209272 imported
10 favorite stickers and3 recent stickers into the running packaged desktop.
The booted firewall omitted45950TCP/UDP; restoring those rules unblocked the
transfer. The permanent Monogram system-module selection still requires system
activation to survive reboot. No phone control was used after the user asked
to stop. Desktop wallet scanning/spending remains absent.


Android's upload acknowledgement recovery now waits one second after the complete
wire body is flushed (including closing the output stream), then immediately
requests a fresh single-use slot for one replay of the same ciphertext. The last
acknowledgement is bounded to five seconds; final size/header checks can confirm
either stored object, and failures/certificate/cancellation guards remain. The
26 core transfer checks pass, including the one-second retry, stream-close wait,
final deadline, immutable bytes and original-object fallback. Physical AndroidAPI37 acceptance passed: the first upload lost its ACK;
the fresh-slot retry returned successfully (396ms response phase), with no
subsequent upload/send failure in the captured interval. Desktop already uses
fresh slots but still has a30-second
acknowledgement window and lacks this final stored-object fallback; that narrower
recovery behavior remains a desktop parity task.

### Pack files (desktop and Android)

Both clients open `.pack` as an alias for the existing XMSP v1 portable archive; `.nzsp` and `.xmsp` remain supported. Opening a downloaded chat attachment or choosing Monogram from the file manager validates/copies the archive on a worker, then opens the existing pack UI with an explicit **Add Pack** action. Previewed packs are initially uninstalled; malformed archives preserve the library, and cancellation/account replacement fences callbacks. This does not add a new wire format. Desktop registers a shared-mime-info type and `%F` activation in Nix, Arch and AppImage packaging. Android accepts dedicated pack MIME types, extension-bearing URIs, and generic ZIP/binary content URIs after checking the provider's display name off-thread. Android requires sign-in and prompts to reopen the file afterward; desktop retains queued files through sign-in.

Verification: 33 Android pack/archive tests (including actual Android intent-filter matching), desktop file classification, and an isolated native GTK cold-library/file-preview/Add Pack/invalid-input check pass. The native flow gives immediate feedback below 400 ms and completes in 1.02 seconds, including a single click that restores a cached chat attachment and opens its pack after download. PR #2's raster cache file I/O and PNG encoding/write/pruning now run on workers; 287 default desktop tests pass. Physical Android file-manager tapping remains untested in this delivery.

### Profile cards, audio playback, invitations and pack sidebar follow-up

Desktop chat-header profiles, message authors/avatars, conversation-info avatars, chat-list avatars and participant rows open a native profile card. It shows cached names/avatars/presence immediately, fetches XEP-0054 vCard fields through the existing authenticated IQ path, and provides Message/local-details actions. Anonymous room participants use their full room/nickname address; no real identity is inferred. Your Profile now opens the same card. Fields are bounded plain text; external PHOTO URLs are never followed, callbacks are account-fenced, and public details have a bounded five-minute session cache. Mobile retains its native full profile screen and persistent profile cache; desktop's full public-field cache is session-only.

Audio and voice attachments use on-demand native playback with play/pause, seeking, elapsed/remaining time, volume, download progress/cancel and Save As. MP3, Opus/Ogg and the existing binary media containers are supported subject to installed codecs. No automatic playback occurs after automatic downloads. File authentication/first plugin initialization run on workers, cancellation propagates to verification, and another audio playback pauses the previous one. Audio-only streams prepare without waiting for a video frame; text playlists remain rejected and network-source blocking remains in place.

Confirmed server bookmarks with autojoin enabled resolve the matching saved group invitations on both clients, without LAN sync or fingerprint-trust changes. Desktop refreshes open request lists and closes obsolete join dialogs. Failed/cached bookmark loads never dismiss pending invitations. Legacy servers without bookmark notifications converge on the next successful refresh/reconnect.

The sticker category strip explicitly declines vertical expansion. The scrolling surface spans the sidebar, with the previous horizontal padding moved into its rows. Native tests cover invite/account/dialog handling, clickable profile cards and screenshots, compact bar/full-width root, authenticated cached audio UI and real MP3/Opus play/pause/seek, and playlist refusal. Android has a selective/idempotent invitation regression and a fresh 0.2.225 APK. Physical phone interaction was not used.

References: [XEP-0054](https://xmpp.org/extensions/xep-0054.html), [XEP-0402](https://xmpp.org/extensions/xep-0402.html).

Mobile0.2.226 bounds public PNG photos to32KiB and checks complete UTF-8 vCard writes against60KiB before sending; local-only photos keep their separate allowance. Personal/group photo uploads and accepted suggestions share the limit. Acknowledged writes retain cached PEP nicknames without a redundant fetch. Native Android graphics/migration and protocol guards pass14tests. See [mobile profile-photo publication](https://github.com/doromiert/monogram/blob/c1e8ff308/docs/PROFILE_PHOTOS.md). Desktop own-avatar publication/accepting suggestions remains an explicit gap in the table above; physical affected-server acceptance is pending.

Desktop now recognizes mobile’s separate Legacy OMEMO forwarded-attachment placeholder as attachment attribution, including existing history, while retaining original wire rows/IDs and linked outgoing deletion. Media reply cards match ordinary reply insets (13px sides/10px top); attachment-touching bubble edges remain2px, with or without reactions.

Media bubble measurement follows the tile’s natural width plus the bubble’s own insets, so long reply quotes/captions wrap instead of creating blank space beside the media. The size follows changing aspect-ratio metadata; editing retains its full-width behavior. Native long-quote regression covers incoming/outgoing media with reactions.

The matching manual-document/save-picker behavior ships in [Android0.2.227](https://github.com/doromiert/monogram/releases/tag/v0.2.227). Desktop native chooser/cancel/background export checks and Android API36 policy/intent/export tests pass; physical Android provider acceptance remains untested.

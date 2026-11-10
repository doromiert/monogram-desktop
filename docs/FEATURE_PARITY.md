# Feature parity ledger

Target: all shipped Monogram behavior, with desktop-specific presentation.
Android baseline: `be6876c045163e32fe07a84416621770b0964b30` / 0.2.195.

**No protocol or financial feature is implemented by this offline prototype.**
A form, switch or fixture is not a working server feature. "Preview" below means
an in-memory interaction or surface; "planned" means it is still absent.

| Feature family | Current prototype | Required for product parity |
| --- | --- | --- |
| Account onboarding, multiple accounts, reconnect and proxy | Account/proxy form preview; fictional account footer | Cancellable login, multi-account ownership, real reconnect/backoff and proxy handling |
| Chat list, search, folders, unread/archived/saved chats | Fictional chat switching, search, All/Unread/Groups and Saved Messages | Durable search/history, editable folders/archive, mute/pin state and tags |
| Text, composer, drafts | Local send, multiline editor and text selection | Durable drafts/outbox, retries, receipts, history and `/me` system-style messages |
| Message actions | Local reply/edit/delete/copy/forward/details | All backend permissions, author/alias resolution, replay, pin/save/repeat, selection and batch actions |
| Rich formatting, articles, tables, spoilers | Markdown text editor / article fixture | Native rich rendering, tables, edit roundtrip, safe links and code-point ranges, partial/whole spoilers |
| Replies and quoted content | Quoted-reply fixture and local reply state | Correct stable IDs, fallback slicing, MAM aliases and room author checks |
| Polls and reactions | Interactive fixture poll controls/reaction toggles; local poll form | Exact public reaction poll grammar, durable ballots, revotes, author closure and timestamps |
| Standard/custom emoji, stickers and GIFs | Right pane, emoji insertion, local sticker/static GIF tiles | Asset decoders, animation, search/provider configuration, packs/favorites/recents and full icon-pack replacement |
| Files and media | Portal selection → removable filenames; artwork/file fixtures | Encrypted streaming transfers, real progress/resume/cancel, thumbnails, captions, cache lifecycle and account fences |
| Viewer and playback | Native fixture dialog and navigation controls | Correct media switching, bounded 11-item window, video/audio/voice/round-video, zoom, download and editor behavior |
| Media authoring | Planned | Crop/trim/markup, compression, albums, camera capture and desktop permission/device flows |
| Locations and previews | Location card/form preview | Real safe map rendering, provider privacy, coordinates, live/location selection and valid fallback removal |
| OMEMO legacy and Monocles PQ in DMs/MUCs | Mode picker and fictional security/device-review dialogs | Exact separate key stores, authenticated content, trust/block, key change prompts and compatible wire protocols |
| Device and contact access control | Local forms and example fingerprint review | Real device revocation, presence authorization, ACL grant/revoke and safe public PEP setup |
| Groups, MUCs, MIX and Spaces | Group creation and spaces/channel form/list previews | Real membership, roles/configuration, presence, bookmarks, pagination, history and transport-specific crypto |
| Invitations and contact requests | Prominent header button and accept/decline previews | Real durable requests, badge counts, invitation permissions and account fencing |
| Presence and typing | Fictional subtitles | Replace subtitle while typing; handle carbons, disconnect and archived-state rejection |
| Direct and MUJI group calls | Call-controls dialog preview | Real media transport, incoming calls, desktop device/portal permissions, mute/camera/speaker, lifecycle and group membership |
| Contacts and profiles | Contact list, editable profile preview | Real roster/local overrides, vCard fields, avatar publication/suggestions, colors, notes and blocking |
| Stories and persistent profile posts | Fixture list/viewer | Correct PEP/Atom access, expiry, persistent saves, edits/retractions, media and profile editing |
| Notifications | All/Mentions/Muted and preview switches | Real desktop notifications/actions, privacy, attention, mention matching and background behavior |
| Read state and archives | Planned | Private own-account read synchronization, public receipt consent, MAM/replay, unread counts and room reader identities |
| Wallet overview / multiple wallets | Orange card, precision switch, local add/remove/switch and activity fixtures | Production 3D card, secure keyring/SDK integration, unlock/lock, durable wallet slots and removal semantics |
| Wallet recovery and scan | Planned | Polyseed/default recovery, 2×8 words UI, restore/import, node configuration, parallel bounded scanning, checkpoints and useful progress |
| Send/request and fiat pricing | Local amount/currency/note/request-card forms | Exact atomic amounts, online fiat list/formatting/rates, address reservation, fees, confirmation and transaction history |
| Payment/claim links | No real link parsing or wallet material | Compatible formats, protected bearer secrets, external restore path, multi-wallet deposits, readiness/claimed status and independent verification |
| Background deposits / fee graph | Planned | Explicit fee approval/ASAP, configured-node history, job cancellation/uncertain broadcast safety and desktop background lifecycle |
| Preferences and appearance | Searchable native pages; light/dark/system, call visibility and large emoji controls | Complete settings, themes/icons/layout, font/accessibility, translation, media/power controls and persistence |
| Privacy/storage/history | Switch previews | Honest cache/history clearing, retention, local encryption and backups as actually supported by Android |
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

## Next milestones after UI acceptance

1. Refine desktop navigation and all preview interactions with user feedback.
2. Choose backend integration architecture and establish protocol/model contracts.
3. Implement ordinary messaging, account lifecycle, persistence and encryption
   with real interoperability tests before enabling real-account use.
4. Implement media, groups/calls/profile and wallet systems with their safety
   invariants, then close each remaining ledger row with evidence.

Do not connect the prototype to a real account or wallet simply to make a demo
control appear functional. There are no backend endpoints or credentials here.

# Live direct-message actions

Native inline editing, replacement-set reactions and best-effort retraction are
connected to live direct chats, in plaintext or explicit OMEMO 2. Mobile reference:
`be6876c045163e32fe07a84416621770b0964b30` / 0.2.195,
`MessageCorrection`, `MessageReactions`, `MessageRetraction`, `MonogramStore`,
`MonogramReactions` and `MonogramRetractions`.

## Behavior

Edit an eligible own text message from its context menu. The editor replaces the
bubble content and preserves the composer/reply draft. Each correction gets a
new event ID and references the original client stanza ID, including repeated
edits. The bubble shows “edited”; Message Details retains original/edit text.
Failed/uncertain changes roll back their projection; reopening Edit recovers the
latest failed correction's text. Account/chat/session changes fence callbacks.

Reaction chips toggle the caller's emoji. The separate context-menu reaction
strip keeps the action list narrower. Expanded reactions use one native virtual
section list for Favorites, Recent and the built-in emoji categories. Category
buttons jump within this list; search filters it. Expansion/collapse slide and
interpolate the whole quick-actions/picker bounds over 350 ms, following mobile
`SelectAnimatedEmojiDialog` and `CustomEmojiReactionsWindow`. The Back button
and Escape return to quick reactions. Reaction failure messages never suggest
recovering an edit draft. Each outgoing event replaces the complete
own set (maximum 32); an empty set removes it. Incoming sets use the same Unicode
emoji table and variation-selector normalization as mobile. Counts aggregate one
set per bare author, independently of message direction. Resolved control events do not create unread chat messages. An unresolved
correction remains an ordinary visible fallback until its target is available.

Delete retains the local option. Eligible own messages also offer Request
Retraction, with an explicit statement that remote deletion/copies cannot be
guaranteed. A pending local request does not erase content until stream send is
confirmed. Applied retractions remove the row from history and clear original,
resolved-edit and reply/reaction payloads for that message; identity/reference
metadata remain for deduplication. This is not forensic secure deletion of SQLite
pages, backups or the remote server.

## Protocol and persistence

- [XEP-0308](https://xmpp.org/extensions/xep-0308.html): original client stanza
  references for direct corrections; ambiguous originals decline the edit.
- [XEP-0444](https://xmpp.org/extensions/xep-0444.html): complete replacement sets,
  origin/client references and accepted aliases of resolved corrections.
- [XEP-0424](https://xmpp.org/extensions/xep-0424.html): current `:1` retractions,
  with incoming legacy `:0` fastening/origin references. Fallback text is never
  rendered as a new retraction message.

Original payloads and control events are stored separately in account/peer scoped
SQLite tables. Reconciliation handles events before originals, late aliases,
replay and later-discovered ID collisions. Client IDs reused across resources
remain distinct originals; ambiguous corrections revert to the immutable base.
Unresolved corrections remain ordinary visible fallback messages; reactions and
retractions stay hidden. Locally deleted aliases remain tombstones.

Corrections cannot lower the original or a prior edit's encryption level.
A protected reaction set cannot be replaced by later plaintext from that actor.
Retracting an encrypted original requires its exact protocol and sender key;
outgoing requests from another device are blocked. OMEMO 2 controls live inside
the authenticated SCE payload, never in the exposed outer fallback. Journal
acceptance follows durable control storage/reconciliation. Pending changes after
a restart/disconnect become uncertain; automatic blind retransmission is absent.

## Verification and limits

Rust tests cover parsing/fallback grammar, outgoing roundtrips, Unicode, delayed
originals, replay, ID collision rollback, aliases, encryption floors/fingerprints,
failed sends, interrupted sends, unread flags, retained drafts and payload removal.
The disposable Prosody fixture drives real GTK inline Save, the context reaction
strip, reactions and retraction/reload in both plaintext and OMEMO 2 chats.
No real accounts, contacts or login keyring are used.

This implements direct text actions. Room permissions/actor identities, caption
edits with unchanged attachment identity, full reaction/poll UI, event rejection
UI, server moderation/tombstones and complete provider/mobile-app interoperability
remain open. Full desktop/mobile parity is not claimed by this milestone.

See [forwarding](FORWARDING.md) for native attributed text/poll copies, Hide Sender,
standalone XEP-0297 quote reception and the remaining attachment/rich-content gaps.

Received file metadata is retained and scrubbed on retraction. Caption corrections
remain unresolved rather than replacing an attachment with unverified text. Active
file transfer/cache lifecycle is documented in [file transfers](FILE_TRANSFERS.md).

Native verification: actual context-menu expansion/collapse had 19/20 distinct
frame heights (248 -> 426 -> 248 px); 257 continuous rows, Flags category jump
and search/clear passed. Native desktop capture: `.state/reaction-picker-expanded-native.png`.
Mobile source was inspected and its actual installed 0.2.198 picker was captured
on an owned offline tablet fixture at `.state/mobile-reaction-reference.png`
in the mobile repository. No transport or real-account sends were configured.

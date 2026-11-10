# Structured direct polls

Desktop now uses the unsubmitted Structured Reaction Polls ProtoXEP 0.0.2 from
`../mobile/xeps/xep-reaction-polls.xml`. `urn:xmpp:polls:0` is a proposed namespace,
not an assigned XSF registration. Questions, descriptions, ordered option labels,
emoji mappings, mode and closure come from the explicit XML. Body text never
supplies authoritative options. A poll can omit both body and fallback entirely.

## Desktop behavior

The native composer supports 2–32 options, an optional description, single or
multiple selections, and a readable-fallback switch. Public votes use XEP-0444
complete replacement sets, retaining unrelated reactions. Single-choice sets
containing several options contribute no ballot; a set without option emoji
withdraws the vote. Counts, chosen state and public voter lists derive from the
persisted event journal. Option reaction chips are hidden on the poll card while
other reactions remain visible.

Only an author-scoped XEP-0308 correction containing the complete unchanged
question, description, ordered options/emoji and mode can close a poll. Results
freeze at the first accepted closure timestamp. Later reactions cannot extend the
cutoff by repeating closure. Timestamps are local accepted ordering, not a server
enforced deadline or proof of when someone voted.

Original definitions and ballots survive late arrival, aliases, archive replay,
restart and failures through the same message-action store. An unresolved poll
correction is readable fallback text, never an independent voting card. Encrypted
originals reject plaintext ballots. Once an own reaction set was protected, later
outgoing replacements stay protected even if the original message was plaintext.
Forwarding creates a new open definition/identity without copied ballots. Copy,
search, chat previews, notifications and reply snippets use the actual definition.

The optional poll fallback and reply fallback have independent Unicode code-point
ranges. Accompanying text is retained. OMEMO 2 authenticates the definition,
optional fallback and ballot/closure metadata inside SCE; there are no exposed
question/choice hints and no plaintext fallback for an encrypted send.

## Shared validation and interoperability

Both codecs enforce 4096 Unicode scalar values for question/description, 1024 for
labels, 2–32 unique option emoji, the XEP-0444 128 UTF-16-unit emoji bound, and a
65536-byte serialized poll element. These are independent bounds. Duplicate
questions/mappings, nested fields, invalid modes/booleans, malformed XML text and
mixed controls decline a structured card. An invalid poll does not hide readable
body text; an invalid bodyless poll produces an unsupported-content message.

`tests/mobile-reference/poll-vectors.json` comes from freshly compiled mobile
`PollDefinition`, `Omemo2Payload` and the shared emoji vocabulary. Rust verifies
Java wire values, fallback behavior and actual HKDF/AES-CBC/HMAC/SCE payloads.
This exercise found and fixed a mobile encoder bug: Smack exposes typed body and
subject values in its extension list too, so copying both emitted duplicate text
children. Typed text is now included only once per language. Both decoders
coalesce identical historical text children, while rejecting conflicting text or
attributes. Generated encrypted historical vectors exercise that compatibility.

Packaged native tests operate the real GTK poll form, option buttons, voter list
and Close Poll menu in plaintext and OMEMO 2 against fictional loopback Prosody
accounts. They check bodyless delivery, both voters, preserved composer drafts,
closure cutoff and reload. Rust store checks cover revote/withdrawal, invalid
single ballots, foreign/changed closures and bodyless ID collisions.

## Mobile progress and remaining parity

The same codec and vocabulary are included in mobile. Direct and room models keep
validated poll metadata through crypto/copy/archive paths; database schema 36
retains it independently of body text and migrates 34/35 data. Native receive
cards, custom emoji mappings, vote conversion, counts/voters, authenticated closure
handling and first-closure cutoff are connected. Poll/reply fallback offsets and
formatting are rebased independently. Malformed metadata cannot fall back to
legacy body guessing. Deletion clears structured payloads.

Mobile creation now derives XML directly from the native composer fields; author
closure, fresh forwarding and encoded native outbox/scheduled metadata are
connected. Scheduled retries/repeats retain the definition, with the existing
account/encryption fences. Encoding avoids native local-parameter delimiters in
user text. The mobile composer uses a readable fallback; its shared send API also
supports bodyless sends. Both clients advertise the proposed namespace.

Java-generated builder vectors additionally exercise actual mobile outgoing
bodyless content and independent poll/reply fallbacks; Rust verifies both wire and
protected payloads and normalizes the resulting message/reply. Local mobile-core
server tests exercise plaintext/OMEMO 2 direct sends and plaintext room sends,
votes and complete closure. Native checks cover outbox serialization, forwarding
and scheduled retries/repeats. Separate native-backend checks verify protected PQ
bodyless direct/MUC polls, closure and absence of exposed poll/fallback XML.

A native mobile fallback toggle, desktop room
transport, complete legacy/PQ encryption and physical mobile/desktop UI
exchange remain open. These checks do not establish exactly once delivery after
an uncertain send, full poll parity or full client parity.

## Historical mobile body polls

Desktop recognizes the separate historical canonical body convention from mobile
`MonogramPolls.java`. It matches the fixed 32 emoji mappings, indentation, exact
footer and UTF-16 limits, including the 40000-unit body limit. These compatibility
limits do not constrain new structured polls. Existing cached body-only messages
are recognized on read; their original bodies remain intact for copy and legacy
closure. No database schema rewrite is required.

An explicit poll element, even malformed, blocks legacy body guessing. Attachment
bodies, unavailable ciphertext and retracted content are also excluded. Message
projections retain the source format and decline cross-format corrections. For
both formats, only an author-scoped complete unchanged definition can close the
poll; the first accepted closure fixes the vote cutoff. Legacy closure retains
the historical body convention, including inside OMEMO 2. A legacy poll too close
to the old size limit to encode closure is refused with an explanation. Forwarding
creates a fresh structured open poll without repeating the old body or ballots.

Twenty fixtures exported by the actual mobile legacy codec check valid/malformed
bodies, all mappings, multiline text, UTF-16 boundaries and the maximum body size.
Native plaintext/OMEMO 2 checks operate the card, voting/voters, closure, reload and
fresh forwarding. Empty voter dialogs also open correctly. Historical polls with
a canonical forwarding-text prefix are recognized after removing the original
claim; [forwarding](FORWARDING.md) preserves that claim and creates a fresh poll.
This does not establish complete historical media forwarding or room parity.

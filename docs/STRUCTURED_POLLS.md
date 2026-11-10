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

**Mobile authoring, author update sending, forwarding and scheduled/outbox metadata
migration remain unfinished.** The current mobile composer still produces the
historical body convention. It does not advertise structured poll support, and
structured update sends fail explicitly instead of emitting an incompatible body
correction. Desktop historical body-only polls remain readable text. Desktop room
transport, complete legacy/PQ encryption and physical mobile/desktop UI exchange
also remain open. This milestone does not establish full poll or client parity.

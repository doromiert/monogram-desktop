# Legacy OMEMO direct and room messages

The legacy option uses the same `eu.siacs.conversations.axolotl` protocol as
Monogram Android’s Smack backend. It is the legacy profile from
[XEP-0384 0.3.0](https://xmpp.org/extensions/attic/xep-0384-0.3.0.html).
libomemo-c supplies the upstream Signal v3 ratchet. Legacy identities, sessions,
trust decisions and Secret Service keys are separate from OMEMO 2.

**Conversation Security → Sending Encryption** controls outgoing packets.
Automatic runs local encryption preparation with OMEMO 2 first and Legacy
OMEMO second. The checks discard their ciphertext. A failed connection, timeout
or storage operation does not select plaintext. The per-conversation result
expires after 24 hours or when its trust or sending policy changes.

Each protocol has its own fingerprint review. Opt-in blind trust applies to
both standard versions for the selected contact. It keeps explicit blocks and
never approves other devices on your own account. Compare and approve those
fingerprints on each device to read encrypted copies of sent messages.

Legacy protects the message body. Quoted reply text and forwarding fallback
text can travel inside that body. Outer reply, poll and action metadata do not
become authenticated content. Legacy files encrypt bytes with XEP-0454 and authenticate only the file URI
plus an optional reply prefix. Captions follow as separate encrypted messages
after successful media submission; pending companions survive failures/restarts.
Explicit local companions render as a caption on their media and are included
when deleting that media. Incoming nearby text is never guessed or hidden.
Direct XEP-0424 retractions follow mobile’s encrypted-body/outer-metadata format,
including archived retractions. They require the original protocol and author plus an authenticated sending
identity; another authenticated device on the same account can retract them.
Legacy reactions use public XEP-0444 metadata after explicit per-chat consent,
matching mobile. Other rich actions and room retractions remain unavailable. Direct live, own-device carbons and MAM ciphertext route
according to the received namespace. Incoming ratchets, decoded bodies and key
transport acknowledgements are committed to a separate authenticated journal
before visible history is accepted. Missing or untrusted device keys can leave
messages unavailable until review. A newly created device cannot read old
ciphertext that contains no key for it.

Verification includes independent Java Smack/Signal exchange in both directions,
local authenticated payload and journal regressions, and disposable Prosody over
TLS. The live fixture selects Legacy automatically when OMEMO 2 is untrusted,
then sends and decrypts a direct message and deduplicates its MAM copy.

Nonanonymous, members-only MUCs now support encrypted text and replies using
Android’s Legacy wire format. Automatic selects Legacy here; classical OMEMO 2
room sending remains unavailable in both clients. Complete member/admin/owner
lists include offline members and own other devices, are fetched before and
after encryption, and are fenced against room/session changes before send.
Live sender identities are captured from room presence; archived identities
require the server’s historical real-JID metadata. Room contexts survive journal
recovery and trust retries. Fingerprint review shows member devices. An isolated
TLS Prosody check verified encrypted delivery and MAM with an offline member;
independent mobile room interoperability and richer room content remain open.

Legacy upload requests retain the sanitized filename/MIME used by Android;
OMEMO 2 retains opaque upload metadata. Outer SFS/OOB descriptions remain
untrusted and cannot supply Legacy URLs, keys, sizes or hashes. A small isolated
TLS/HTTPS check verified direct and room upload/download bytes and caption order.

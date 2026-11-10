# Legacy OMEMO direct messages

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
become authenticated content. Legacy file sending and protected rich actions
remain unavailable. Direct live, own-device carbons and MAM ciphertext route
according to the received namespace. Incoming ratchets, decoded bodies and key
transport acknowledgements are committed to a separate authenticated journal
before visible history is accepted. Missing or untrusted device keys can leave
messages unavailable until review. A newly created device cannot read old
ciphertext that contains no key for it.

Verification includes independent Java Smack/Signal exchange in both directions,
local authenticated payload and journal regressions, and disposable Prosody over
TLS. The live fixture selects Legacy automatically when OMEMO 2 is untrusted,
then sends and decrypts a direct message and deduplicates its MAM copy.

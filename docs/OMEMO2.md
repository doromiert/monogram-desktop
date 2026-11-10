# Live OMEMO 2 direct messages

Desktop now connects the Rust OMEMO 2 backend to explicit account setup, native
fingerprint review, per-conversation encrypted sending and authenticated receiving.
This is direct text/reply support; full mobile encryption parity remains open in
[FEATURE_PARITY.md](FEATURE_PARITY.md).

## Use and persistence

Connect an account, open **Conversation Security**, and choose **Enable**. This
creates a separate OMEMO 2 identity, stores the master key in Secret Service and
publishes the public device/bundle through PEP. Review the full fingerprints of
contacts and your other devices, compare them through a trusted channel, and
choose Trust Shown or Deny Shown. Enable **Encrypt Messages** for that conversation.
Other clients must implement `urn:xmpp:omemo:2`; legacy/PQ selection is not yet
available. Encryption never silently falls back to plaintext.

Setup is explicit and survives restarting/reconnecting. A per-account file lock
serializes identity initialization. A durable device manifest and an independent
keyring fingerprint binding detect missing databases, missing keys, incomplete
initialization and changed identities. Recovery preserves the original device;
errors require intervention instead of silently rotating it. Another running
session cannot acquire the same local identity. Removing an account's history
retains its crypto identity/trust for safe reconnection; server devices are not
revoked by local account removal.

Crypto lives under `XDG_STATE_HOME/monogram-desktop/crypto/<account-hash>/` in an
AES-256-GCM record database with account/device/kind/peer/key-ID authenticated
scope. Directories/databases use 0700/0600 permissions. The encryption master key
and identity binding live in Secret Service, with no plaintext file fallback.
**Chat history and drafts remain plaintext locally.** Saved unavailable ciphertext
can be retried after trust approval without displaying an outer fallback body.

## Protocol and lifecycle

The reference is mobile `be6876c045163e32fe07a84416621770b0964b30`, specifically
`Omemo2{Device,Bundle,Payload,IncomingJournal}.java`, `XmppOmemo2.java`,
`omemo2_jni.cpp`, and `crypto_provider.c`. The wire profile follows
[XEP-0384 0.9.1](https://xmpp.org/extensions/xep-0384.html) and
[XEP-0420](https://xmpp.org/extensions/xep-0420.html). The linked libomemo-c source
is identical to mobile's pin; see [native notices](../native/NOTICE.md).

- Key lifecycle, unsigned 32-bit remote prekey IDs, signature/point validation,
  signed-key retention and exact-fingerprint trust use the mobile policy.
  Changed/stale approvals fail atomically. Denial never removes previously
  decrypted history. Legacy/PQ identities, ratchets and trust are not shared.
- Public-key setup uses bounded, account-scoped IQs with sender checks, deadlines
  and disconnect cancellation. Failed own-list reads abort publication; bounded
  read/merge/verify retries preserve other clients' entries/metadata. Public access
  is verified, and access repair changes only its configuration field. Sends fetch
  current device lists/bundles and include every nonblocked target and eligible
  own-device copy. Unreviewed valid identities block sending.
  Only invalid/missing auxiliary public bundles may be skipped; own-device
  storage or network failures still block the send.
- A dedicated cancellable crypto worker keeps IQ waits off GTK and the transport
  loop. A bounded incoming frame queue persists overflow ciphertext for later
  retry; it does not send acknowledgements for dropped/deferred work. Callback
  generation/account fences prevent stale sessions from changing another account.
- The HKDF/AES-CBC/HMAC payload and SCE routing affixes are authenticated. Decrypted
  XML has independent byte/depth/node bounds and must be one complete document.
  Plaintext fallback bodies cannot become decrypted content. Ratchet/ciphertext
  transactions roll back on authentication or persistence failure.
- Live, own-carbon and query-bound MAM frames share decryption. Sender bundles are
  refreshed before incoming prekey messages. Stable encrypted digest aliases and
  account/peer/direction-scoped IDs deduplicate replay, including deleted rows.
- Ratchet changes, consumed prekeys, plaintext, original archive/time/receipt
  context and heartbeat acknowledgements commit in one encrypted journal
  transaction. GTK accepts the token only after durable history/alias persistence.
  Heartbeats and prekey replenishment follow acceptance. Pending content/acks
  survive restart; capacity/storage errors do not advance the ratchet. Recovered
  messages replace unavailable rows without duplicating unread state or replacing
  authenticated text with a replayed placeholder.
- Uncertain socket delivery is labelled accordingly and retry requires the existing
  duplicate-delivery confirmation. There is no automatic plaintext retransmission.

## Verification and remaining work

The package runs Rust tests, the unchanged mobile Java/JNI ratchet comparison,
fixed payload vectors from freshly compiled mobile Java, and real GNOME Secret
Service tests on disposable private D-Bus/keyring/state directories. Lifecycle
checks cover process locking, reconnect identity preservation, missing/deleted
stores and initialization recovery. No real account or user keyring is tested.

Disposable Prosody checks cover public PEP setup, preserved auxiliary devices,
actual encrypted wire text/replies, own-device copies and heartbeat exchange.
The integrated service checks unreviewed-send rejection, saved-ciphertext retry
following approval, crash recovery, preserved trust/identity, MAM digest aliases
and denial. Native GTK checks activate the real setup/review/encryption controls,
send from the composer, receive a protected reply, persist/reconnect and send again.
Native controls also verify protected inline corrections, reaction replacement
sets and original-device retraction/reload; see [message actions](MESSAGE_ACTIONS.md).
History failure/replay checks exercise the real receive/persistence path. Prior
native layout/composer and transport/TLS rejection checks remain required.

Remaining encryption parity includes automatic version selection and default-off
automatic-trust policy, PEP watch/invalidation lifecycle and richer cached review,
receive-only restore/migration and explicit revocation/reset, complete rich-content
and formatting recovery, encrypted attachments, legacy/PQ and group encryption.
Ciphertext retry currently processes a bounded 128-row batch after setup or trust
changes; complete background pagination/retry remains open. Provider/federation
coverage, a mobile app UI exchange and an independent security audit are not
established by these local tests. Unsupported encrypted rich content remains
unavailable/pending rather than being presented as plaintext.

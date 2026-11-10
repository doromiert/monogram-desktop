# OMEMO 2 backend milestone

The Rust OMEMO 2 backend is implemented and tested independently of the live
composer. **The installed app still sends plaintext direct messages.** This
milestone does not enable, advertise or generate keys for real accounts.
Full encryption parity remains open in [FEATURE_PARITY.md](FEATURE_PARITY.md).

The reference is mobile `be6876c045163e32fe07a84416621770b0964b30`, specifically
`Omemo2{Device,Bundle,Payload,IncomingJournal}.java`, `XmppOmemo2.java`,
`omemo2_jni.cpp`, and `crypto_provider.c`. The wire profile follows
[XEP-0384 0.9.1](https://xmpp.org/extensions/xep-0384.html) and
[XEP-0420](https://xmpp.org/extensions/xep-0420.html).

## Implemented backend behavior

- The exact mobile libomemo-c source handles sessions and the Double Ratchet.
  Rust owns storage, explicit trust, payload authentication, bounds and rollback.
  No legacy/PQ identities, sessions or trust records are shared.
- AES-256-GCM protects account/device/kind/peer/key-ID-scoped SQLite records.
  Private directories/databases use modes 0700/0600. Ratchet changes commit before
  ciphertext leaves the operation. Failed persistence/authentication rolls back
  session changes and consumed prekeys.
- Local key creation, prekey replenishment, signed-key rotation/retention and
  incomplete-state checks preserve existing identities. Remote unsigned 32-bit
  prekey/signed-key IDs are accepted; device IDs remain positive 31-bit values.
  Bundles validate point/subgroup/low-order keys and Ed25519 signatures.
- Exact observed fingerprints need explicit approval. Changed/stale approvals
  fail atomically; rejection history survives key changes. Changed pins clear
  their associated ratchet sessions. There is no implicit trust policy.
- The mobile HKDF/AES-CBC/truncated-HMAC payload profile and SCE routing affixes
  are authenticated. Unsupported content, wrong routing, unsafe XML, oversized
  payloads, invalid timestamps and malformed envelope structures fail closed.
  No outer fallback body becomes decrypted content.
- A bounded encrypted journal commits decrypted content, consumed prekeys,
  ratchets and heartbeat acknowledgements together. History acceptance and
  acknowledgement delivery are separate phases. Pending content/acks survive
  database reopening. Capacity exhaustion rejects the operation without advancing
  the ratchet; stale tokens cannot delete reused slots.
- Secret Service stores per-account/device master keys using encrypted D-Bus
  sessions. Missing/locked/invalid/duplicate secrets fail; no plaintext key file
  fallback exists. This boundary is not yet invoked by normal application login.
- PEP discovery/publication uses bounded account-scoped IQ requests, exact reply
  senders, deadlines, cancellation and disconnect handling. Own-list read failure
  prevents publication. Read/merge/reread preserves other clients' device entries
  and unknown metadata and retries races at most three times. Access repair only
  submits the access-model field and verifies the resulting public configuration.
  Publish-option conflict repair retains the mobile policy.

## Verification

The Nix package runs ordinary Rust tests, the unchanged mobile Java/JNI ratchet
comparison, fixed payload vectors from freshly compiled mobile Java, and a real
GNOME Secret Service on a disposable private D-Bus/keyring. No user keyring or
real account is touched.

The disposable Prosody fixture publishes and discovers devices/bundles, preserves
an existing auxiliary device and its metadata, sends a body-less encrypted stanza
over verified TLS, decrypts text/replies and the own-device copy, and exchanges the
prekey heartbeat. The prior direct-text/reply/receipt/carbon/MAM/reconnect and TLS
rejection checks remain required. Unit checks separately exercise failed own-list
reads, publication races, public-access repair, wrong keys, restart recovery,
capacity/storage/authentication failures, unsigned IDs, replay and stale trust.

## Required before enabling real-account encryption

Connect the backend to explicit account initialization and persistent device
selection, serialize keyring/identity creation, and handle absent/deleted/incomplete
stores without silently rotating identities. Add the native trust/version controls
and manual encrypted-send selection. Discovery must retain partial fingerprints
for review, refresh sender bundles for incoming prekey messages, preserve explicit
blocks, implement mobile's default-off automatic-trust option, and account-fence
every callback.

Route encrypted live/carbon/MAM messages through authenticated normalization,
durable history and the journal acceptance/ack phases. Persist canonical encrypted
aliases, replay metadata and unavailable ciphertext for later trust/retry; never
lose content if the history write fails. Send must include every nonblocked target
and eligible own-device copy and must never fall back to plaintext on failure.
Implement PEP invalidation/watch lifecycle, optional known-formatting recovery,
receive-only restoration, and deliberate logout/reset/revocation semantics.

Legacy/PQ, group crypto, key backup/migration, encrypted attachments and the other
mobile encryption features remain separate required parity work. The live test
does not establish mobile app UI exchange, a federated/provider matrix, complete
SCE rich-content validation, or a security audit.

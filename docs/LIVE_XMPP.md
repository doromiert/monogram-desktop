# Initial live XMPP milestone

The desktop now has a real direct-text path, alongside a separate offline demo.
This is the first backend milestone, not full Monogram parity.

## Use

Open **Main Menu → Accounts → Add Account**, enter a bare XMPP address and
password, and choose Connect. DNS service discovery is the default. The optional
server override changes the endpoint, while TLS identity verification still uses
the XMPP address's service domain. STARTTLS and direct TLS are available; no
unencrypted transport or certificate-verification bypass is exposed.

Use **New Conversation** to open a bare XMPP address. Opening it sends no message
and does not authorize presence. Text, Unicode/emoji, multiline messages,
stable-ID replies, plain-text forwarding and the account's Saved Messages
self-chat use the same transport. Send failures keep the compose draft or expose
Retry on the stored message. Retrying uncertain delivery requires confirmation
because another client/server might not deduplicate it.

Accounts are saved separately. One account can be connected at a time; switching
or disconnecting cancels the old session, fences queued callbacks and clears its
password from the transport on teardown. Passwords are session-only and must be
entered again after restart. Opening the app restores cached local data without
automatically connecting. The offline demo never borrows a live account's history,
drafts, authorization requests or transport.

## Implemented behavior

- Rust runs tokio-xmpp 6.0.0 TLS/SASL/XML on a cancellable Tokio thread.
  The connection adapter keeps handshake failures observable and bounds
  connection attempts and backoff. GTK/model/database changes stay on the main thread. Login can be cancelled by Disconnect or switching
  accounts; socket loss reconnects with backoff. Initial handshake attempts have
  a deadline, and authentication/certificate failures require user intervention.
- Roster names and multi-resource presence update chats. Incoming subscription
  requests have explicit Accept/Decline controls. Starting a direct conversation
  does not silently subscribe or grant presence access.
- Direct text uses stanza/origin IDs. Trusted own-account carbons and query-bound
  MAM results share the incoming normalization path. Wrappers from foreign
  senders are rejected. Deduplication is scoped by account, conversation and
  direction; only the account's server-assigned stanza IDs are accepted.
- Server history is fetched in pages of 50. The initial local window loads at
  most 200 messages per conversation; older local pages load 100 at a time.
  History preserves server timestamps. Reconnection syncs the latest page for
  the selected conversation. This is not an exhaustive account-wide MAM sync.
- Outgoing delivery receipts are requested. The visible Delivered state means
  receipt by another client, never that a person read the message. Receipt sender
  and message ID must match the stored conversation. Sending receipts is opt-in
  and follows successful local persistence; archive/carbon replay never generates
  receipt acknowledgements.
- Per-account preferences control typing, delivery receipts, notifications and
  notification previews. Notification bodies hide message content by default.
  Local archive, pin, unread markers, folder rules/order, drafts/reply targets and
  text history survive restart in SQLite. Drafts debounce for 750 ms and flush on
  navigation/close. Draft attachment recovery is not implemented.
- Local deletion retains ID tombstones so fetching the server archive does not
  immediately recreate deleted rows. It does not retract remote messages.

## Security and current limits

**Direct text/replies can use explicitly enabled OMEMO 2.** Open Conversation
Security to initialize this account, compare/review device fingerprints and enable
Encrypt Messages for the conversation. The selected mode persists and encrypted
sends never fall back to plaintext. Unreviewed/missing/blocked devices or failed
key publication stop sending. Secret Service protects the local crypto master key.
See [OMEMO2.md](OMEMO2.md) for protocol, lifecycle, tests and remaining limitations.

Conversations without that selection remain plaintext over verified TLS and are
labelled accordingly. Unsupported or untrusted encrypted payloads remain
unavailable; their outer fallback bodies never appear as decrypted content.

Live mode implements [direct polls](STRUCTURED_POLLS.md) and
[attributed text/poll forwarding](FORWARDING.md), including explicit OMEMO 2.
Received files use [real manual downloads](FILE_TRANSFERS.md); upload/attach and
fictional media, article, wallet, story, group and call actions remain disabled.
Their offline demo surfaces remain available through Open Offline Demo.
MUC/MIX, uploads/complete media transfers, legacy/PQ, complete OMEMO 2 policies, stream resumption,
proxy configuration, certificate exceptions, keyring-backed credentials and
simultaneous connected accounts remain outstanding. Unsupported discovery/archive
features report their absence without fabricating successful results.

The database is `XDG_STATE_HOME/monogram-desktop/history.sqlite3` (normally
`~/.local/state/monogram-desktop/history.sqlite3`), with mode 0600 inside a private
directory. It contains plaintext local message/draft content and account endpoint
details. Attachment URLs/file keys belong to that plaintext message content;
downloaded attachments are also plaintext in an account-scoped cache. Login
passwords and OMEMO identity/ratchet keys are not stored in this history database. Removing an account deletes its
local rows and leaves the server untouched; this is not a forensic secure erase.
Storage-at-rest encryption is a later milestone.

The [OMEMO 2 integration](OMEMO2.md) stores crypto records separately under
`crypto/<account-hash>/`. Removing an account's chat data retains that identity
and trust for safe reconnection. Chat/draft storage remains plaintext.

## Verification

`nix build` / `nix flake check` run model checks, native GTK checks, and an isolated
loopback-only Prosody 13.0.6 fixture with a disposable trusted TLS certificate.
The fixture exercises real login/roster/text/replies/receipts/carbons/MAM/typing,
socket reconnect, invalid-password and certificate/hostname rejection, refusal to
authenticate without TLS, and thread cancellation. A second check drives the real
native Rust window's composer against a fixture peer, then verifies restoration/MAM dedup,
draft+reply restoration, account isolation and stale-callback rejection.
No real accounts or contacts are used. Public-server/provider interoperability,
production notification behavior and end-to-end encryption are not established by
these checks.

References: [tokio-xmpp](https://docs.rs/tokio-xmpp/6.0.0/tokio_xmpp/),
[RFC 6120](https://www.rfc-editor.org/rfc/rfc6120),
[Carbons](https://xmpp.org/extensions/xep-0280.html),
[MAM](https://xmpp.org/extensions/xep-0313.html),
[stable IDs](https://xmpp.org/extensions/xep-0359.html),
[delivery receipts](https://xmpp.org/extensions/xep-0184.html),
[replies](https://xmpp.org/extensions/xep-0461.html).
The Android behavior reference remains the pinned source index; this code does
not embed Android, Telegram services or the Android JVM adapter.

Live [direct-message actions](MESSAGE_ACTIONS.md) now include inline corrections,
reaction replacement sets and best-effort retractions, with durable event replay.

[Structured direct polls](STRUCTURED_POLLS.md) now use explicit XML definitions,
public replacement-set ballots and author closure, with optional body fallbacks.

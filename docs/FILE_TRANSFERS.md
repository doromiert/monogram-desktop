# Live attachment reception and downloads

Desktop advertises SFS support, receives direct file messages in plaintext or
explicit OMEMO 2 and offers
manual Download, Cancel, Retry and native Save As. The reference is mobile's
`FileLink`, `FileShare`, `FileTransfer` and account-owned transfer registry.
Uploads, rich media viewing and complete transfer/settings parity remain open.

## Wire profile

The existing [XEP-0447](https://xmpp.org/extensions/xep-0447.html) SFS description
carries file metadata, one HTTP URL source and optional SHA-256, SHA3-256 and
BLAKE2b-256 hashes. The mobile-compatible
[XEP-0454](https://xmpp.org/extensions/xep-0454.html) AES-GCM URL profile carries a
12- or 16-byte nonce and 32-byte key in its secret fragment. The actual request
always uses HTTPS with that fragment removed. Plain SFS/OOB links cannot become
encrypted attachments, nor can an encrypted link become a plaintext attachment.

OMEMO 2 file descriptions and keys come exclusively from authenticated SCE
content. Exposed outer extensions cannot introduce a private download URL. Legacy
OMEMO and PQ transport are still unavailable on desktop; an unsupported ciphertext
never becomes a downloadable outer fallback. Plain OOB compatibility and supported
protected body-only file links remain readable. Malformed descriptions stay text;
attachment-looking text alone does not trigger a network request.

Only validated SFS/OOB URL fallback spans and valid reply quote spans disappear
from captions. Ranges count Unicode code points. The canonical forwarding claim
is interpreted after caption extraction. Poll/file combinations and ambiguous or
unsupported sources are not interpreted as downloadable files.

The backend can serialize a fresh typed description and independent file/reply
fallbacks; it rejects file-key/envelope mismatches. Live upload/attach controls
remain disabled until provider discovery, immutable inputs and fresh uploads are
implemented. Forwarding a received attachment is still refused rather than
reusing its original key-bearing URL. Caption corrections remain unsupported
until unchanged attachment identity is verified.

Peer capabilities are queried at a full resource JID. The generic IQ path accepts
canonical entity, resource and service addresses, and matches replies to the exact
requested sender; another resource cannot satisfy a pending request.

## Transfer and cache lifecycle

Downloads run outside GTK, with at most four concurrent workers, bounded chunks,
15-second connect and 30-second read timeouts. They accept HTTPS-only redirects
(up to five), send no referrer, request identity encoding and reject unsupported
content encoding, invalid lengths, truncation and oversized data. The hard
plaintext bound is mobile's 4000 MiB, with a separate 16-byte GCM tag allowance.
No URL, query token or file key is included in error text or attachment Debug output.

A private partial file may contain unverified plaintext while streaming. It is
not exported or treated as a completed download. GCM authentication, advertised
size and every supported advertised hash must pass before atomic cache commit.
Normal cancellation/failure removes the partial. Abrupt process termination can
leave private partial files; account removal clears the account cache. Plain HTTPS
files without an advertised hash have transport/length checks, not an independent
cryptographic content checksum.

The cache uses opaque names derived from URL/key plus expected size/hashes,
under a separate opaque account directory. Files are mode 0600 and account
directories 0700. Cache reuse and export recheck size/hashes. After restart, an
explicit Download first checks the existing cache and avoids fetching valid bytes
again. Range resume and automatic job restoration are absent; interrupted jobs
restart from the beginning, matching mobile's current transfer backend.

Leaving a chat preserves its job. Cancellation/retry uses a distinct worker and
partial file, so late cleanup cannot overwrite the replacement job's progress.
Account switches and window close cancel jobs; account removal also deletes its
cache. Cache creation/commit are fenced against concurrent account removal.
Save As uses the native file chooser and asynchronous destination copy, with
account/message checks before export. Cached attachments remain plaintext;
received URLs/file keys are part of the plaintext local history. OMEMO identity
and ratchet keys continue using their existing protected keyring-backed store.
Local message deletion/retraction does not promise forensic removal of copies,
exports or cached media; cache management/retention UI remains unfinished.

## Verification and remaining work

The mobile test-only `GenerateFileVectors` exports 32 actual wire/model cases and
four actual `FileTransfer` cipher cases, including both nonce lengths, empty and
binary payloads and all three supported hashes. Rust checks received/protected
models, outgoing roundtrips, Unicode captions/replies, forwarding prefixes, typed
persistence, secret-URL rejection and exposed-metadata isolation.

A disposable HTTPS fixture checks tag/hash/size failures, truncated/oversized
responses, unsafe redirects, untrusted certificates, cache corruption/reuse and
partial cleanup. The private GTK/XMPP fixture drives Download/Cancel/Retry,
live SFS discovery, leave-chat continuation, native Save As/export, preserved drafts, reload and account
cache removal in plaintext and OMEMO 2. No real account or host clipboard is used.

Generic file cards currently represent photos, video, audio, stickers and GIFs as
files. Live thumbnails/viewer/playback, content-type upgrades, provider uploads,
media authoring, albums, automatic-download preferences, durable transfers,
complete cache management, caption edits, fresh media forwarding, room transport
and physical mobile/desktop application exchange remain open. These receive and
download paths do not establish complete file/media or client parity.

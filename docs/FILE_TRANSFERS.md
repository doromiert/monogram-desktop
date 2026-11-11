# Live attachment reception and downloads

Desktop advertises SFS support, receives direct/room file messages in plaintext,
Legacy OMEMO and direct OMEMO 2, and offers
manual Download, Cancel, Retry and native Save As. The reference is mobile's
`FileLink`, `FileShare`, `FileTransfer` and account-owned transfer registry.
Direct and room uploads now use XEP-0363 service discovery and slot requests. Live photos have inline previews, a shared media grid and an expanded viewer.
Other rich media and complete transfer/settings parity remain open.

Inline media preserves aspect ratio within 420 × 360 pixels; stickers stay
borderless within 256 × 256 pixels. Known metadata sizes the progress placeholder
before decode. Retained rows keep following account-owned download/decode jobs
across reconnects and map/unmap cycles.

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
OMEMO accepts only authenticated URI-body files; PQ transport remains unavailable.
An unsupported ciphertext
never becomes a downloadable outer fallback. Plain OOB compatibility and supported
protected body-only file links remain readable. Malformed descriptions stay text;
attachment-looking text alone does not trigger a network request.

Only validated SFS/OOB URL fallback spans and valid reply quote spans disappear
from captions. Ranges count Unicode code points. The canonical forwarding claim
is interpreted after caption extraction. Poll/file combinations and ambiguous or
unsupported sources are not interpreted as downloadable files.

The backend can serialize a fresh typed description and independent file/reply
fallbacks; it rejects file-key/envelope mismatches. Live upload/attach controls
use provider discovery, immutable inputs and fresh uploads. Forwarding a received attachment is still refused rather than
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

## Upload selection, sending and retry

The native chooser retains GIO files, including portal/GVfs sources without a
local path. Sending creates durable local pending messages and clears the composer
only after persistence. The first selected file carries the caption; selections
currently send as independent files rather than a media album.

[XEP-0363](https://xmpp.org/extensions/xep-0363.html) discovery queries the account
server and its advertised services. Provider limits include the encrypted 16-byte
tag and are checked before copying a known-size source. Unknown-size sources are
bounded while copying, and the actual snapshot size is checked before slot use.
The worker copies to a private, synced, immutable account-owned snapshot. Retry
reuses that snapshot, protecting against later changes to the original file.

Uploads use at most four workers with 64 total queued files. Plaintext bytes stream
in 64 KiB chunks; encrypted uploads use a fresh 12-byte nonce and 32-byte AES-GCM
key for each attempt. Modern encrypted slots expose only a random `.bin` name and
`application/octet-stream`; actual filename, MIME, size and plaintext SHA-256 are
inside the protected SFS description. PUT credentials are restricted to the
XEP's Authorization/Cookie/Expires headers. CR/LF is rejected, redirects are
refused, and only HTTP 200/201/204 after complete body generation permits sending.

A pending placeholder is never an immutable wire-message base and cannot be
submitted by the XMPP or crypto send path. After a successful upload, owner,
session generation, encryption mode and local fingerprint must still match;
otherwise the pending message fails and requires explicit retry. The crypto
worker rechecks the selected fingerprint immediately before encryption, covering
identity changes queued ahead of a send while GTK status is stale. Final attachment
metadata is persisted before sending its stanza. The snapshot then moves into the
verified download cache. Cancellation, closing the window or changing account
stops workers. Restart recovers pending messages as interrupted, without automatic
retransmission. Retrying a pending upload requests a fresh slot and encryption key.
Retrying an already uploaded message under changed encryption requires selecting the file
again. Forwarding to a different recipient still requires its own verified fresh
upload flow and remains unavailable.

Pending source URIs, filenames, captions and final file URLs/keys are plaintext
local history. Source URIs and bearer credentials are excluded from Debug/error
output. Snapshots and abandoned private partials are removed with the account
cache; complete retention/cleanup controls and process-kill resume remain open.

## Photo previews and shared media

Once an image is downloaded, desktop verifies its open file descriptor and gives
that descriptor to libglycin in a bubblewrap sandbox. GTK only receives a decoded
texture. Save As also keeps the verified descriptor through destination selection
and asynchronous copy. Local MIME and dimensions do not change the original wire
metadata. Decode workers and the texture cache have separate bounded queues and
budgets for thumbnails and viewer images.

Conversation Info → Shared Media opens a three-by-three photo grid. Further pages
use a cursor ordered by timestamp and ID. Choosing a tile opens the viewer. It
shows the selected image immediately and looks up up to ten older and ten newer
images outside GTK. It prepares neighbor thumbnails from cached files and can
download the selected image.

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

Generic file cards still represent video, audio and unsupported image types.
Animated playback, full content-type upgrades,
media authoring, albums, automatic-download preferences, automatic transfer resume,
complete cache management, caption edits, fresh media forwarding, room transport
and physical mobile/desktop application exchange remain open. These receive and
download paths do not establish complete file/media or client parity.

Legacy uploads retain original sanitized filename/MIME while encrypting bytes.
Their message body contains only the AES-GCM URI and optional reply prefix; no
SFS/OOB key material is emitted outside encryption. A durable companion caption
is released once the parent is durably sent, preserving its Legacy mode and
identity. Interrupted claims become uncertain instead of being sent twice.
Deleting a pending parent also removes its unsent companion. Cancelled uploads
retain the caption for retry. Upload retries preserve their captured encryption
mode; a new mode requires reattaching the file.

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

Round-video messages use mobile's `round_video_*.mp4` filename marker only after
verified local playback confirms a video track, positive duration, and square
intrinsic dimensions up to 1280 pixels. Generic attachment MIME does not prevent
recognition. Their pixels are clipped to a circle and captionless bubbles stay
transparent. Visible muted previews loop; tapping starts audible inline playback
or pauses it, preserving an explicit pause during viewport updates. Starting
another round video returns the previous one to muted preview behavior. Radial
seeking, remaining-time labels and the mobile round-video editor remain open.

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

Uploads use at most four workers with 64 total queued files. File work uses
64 KiB chunks; encrypted upload jobs use a fresh 12-byte nonce and 32-byte AES-GCM
key. Before requesting a slot, the worker validates and encrypts the immutable
input into a private wire snapshot. The snapshot is unlinked before networking
and kept open only for that job. This uses up to one additional file's worth of
temporary disk space while keeping memory bounded. Modern encrypted slots expose only a random `.bin` name and
`application/octet-stream`; actual filename, MIME, size and plaintext SHA-256 are
inside the protected SFS description. PUT credentials are restricted to the
XEP's Authorization/Cookie/Expires headers. CR/LF is rejected, redirects are
refused, and only HTTP 200/201/204 after complete body generation permits sending.

After the body is consumed, a three-second response-header wait may reconnect
once; the final request retains the normal 30-second read timeout. Transport
failures may also trigger that one recovery. The repeated PUT
uses the same slot, headers and byte-identical wire snapshot, following
[RFC 9110 section 9.2.2](https://www.rfc-editor.org/rfc/rfc9110.html#name-idempotent-methods).
It never re-encrypts changed plaintext under the original nonce. HTTP rejections
are not retried. The UI shows confirmation and reconnection states; cancellation
and the existing owner/generation checks still fence publication. A failed
recovery returns an error and leaves explicit Retry available. That recovery is
bounded connection handling, not a correction to the external server.

The local TLS fixture confirms identical encrypted bytes and allowed headers on
two connections to the same slot, one recovery at most, no replay after HTTP
403 or cancellation, and rejection of a changed plaintext snapshot. Three cold
production uploads of synthetic 8 KiB files completed in 3.86–4.10 seconds without
manual restart; each resulting attachment passed authenticated decryption and
SHA-256 verification on download. No contact message was sent.

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

Media presentation follows mobile `ChatMessageCell` separation of photo bounds
and caption bounds: the media retains its 2 px inset, while captions and reactions
receive their own text insets. Long captions wrap at the media width rather than
widening it. Native actual-widget checks cover short/long captions and reactions.

Download failures now retain safe timeout/connect categories, following mobile
`MonogramController.downloadFailure`; private file URLs and AES-GCM fragments
are excluded from logs. The reported 18:11 video reproduced a 30 s wait before
HTTP headers twice, then completed verified decryption on retry in 0.81 s. This
is evidence of intermittent response timeout; it does not identify the remote
or network cause. A local TLS fixture verifies the timeout label and no cache
commit before headers.

While a media request is waiting for its first byte, a known advertised size
does not make its progress determinate: the ring keeps rotating at the frame
clock rate until bytes arrive. The reported 19:37 WebP sticker timed out before
HTTP headers; the user confirmed immediate success with a fresh retry. The
underlying intermittent network/server cause has not been established.

Downloads recover up to two response-header stalls automatically: after three
seconds without headers, cancel that GET future and start a new GET using the
same TLS configuration. The UI reports reconnecting, and existing cancellation, redirect,
size, ciphertext authentication and hash checks still apply. The final request
keeps the normal timeout. Upload recovery is described above and only repeats
byte-identical wire snapshots.

The actual affected encrypted media passed five cold `Downloads::start` runs
with full decryption/authentication/cache commit: 404 ms without recovery, then
3352/3294/3295/3607 ms with one recovery each. The local TLS regression withholds
the first response and verifies the second connection returns authenticated
bytes. Header timeout, corruption, unsafe redirect and cancellation checks pass.
The upload service still intermittently stalls before headers in independent
Android HTTP/1.1 requests; this recovery does not establish a server-side fix.

A larger affected video also passed five cold starts with full authenticated
1,407,353-byte plaintext results: 1740/3701/3821/3892/4235 ms. The TLS regression
withholds the first two responses and asserts exactly three GET requests and
one verified final cache entry. Reconnect attempts never restart body reads
that are already advancing normally.

Rejected diagnosis: forced HTTP/1.1 initially appeared to help an interleaved
probe, but sequential HTTP/1.1 requests also stalled after TLS on desktop and
Android. No protocol downgrade is retained. A per-socket 1200-byte MSS probe
also failed to help; no network-interface or route settings were changed.

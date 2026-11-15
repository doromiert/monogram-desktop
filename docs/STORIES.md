# Live stories

Desktop follows mobile `XmppStories.java` and `docs/STORIES.md` using experimental
[XEP-0501](https://xmpp.org/extensions/xep-0501.html), XEP-0472 Atom entries and the
`urn:xmpp:pubsub-social-feed:stories:0` PEP node. Each item has one HTTPS image or
video enclosure, a bounded caption and a publication timestamp. Both clients
hide active stories after 24 hours. Story media is not OMEMO encrypted.

Publication creates/configures the node for presence access, checks the server's
read-back configuration, then includes an atomic presence-access publish option.
A server that cannot confirm this policy does not receive a story publication.
The editor offers subscribed-contact visibility. Public/custom audiences are
not presented. Caption edits keep the original ID and timestamp; own-story
retraction removes the server item and the local history entry.

The live strip loads an account-scoped metadata/read cache immediately and
refreshes the account plus up to 50 known contacts. Authenticated direct PEP
notifications trigger a peer refresh. Metadata, URLs, author identities, item
counts, MIME types and timestamps are bounded and checked before display.
Uploads, HTTPS downloads, hashing, decoder initialization and preview/image
work run outside GTK. Upload progress updates every 80 ms; interrupted drafts
retain their source and uploaded URL for retry. Closing the posting window
cancels pending publication. Images, animated GIF/WebP and video use the
existing bounded native media infrastructure. Navigation/closing fences stale
worker results, and unopened/loading media does not advance the story timer.

Viewed entries retain a local seen flag. Opening a story suspends read marking
for the underlying chat, and closing it resumes focused-chat read marking.
Replies are ordinary quoted-text chat messages addressed to the stable owner
JID and use that conversation's sending policy. Own history retains up to 1000
metadata entries, including expired stories; own uploads retain their local
snapshot. Remote media still depends on its upload host.

Verification includes mobile-shaped literal Atom input, expiry, malformed media,
authenticated notifications, account-isolated cache/read/history behavior, and
an IQ fixture that refuses publication when presence access was not confirmed.
Native private-display fixtures verify immediate posting feedback, prepared-URL
publication acknowledgement/cache, cancellation/recoverable drafts, off-thread
image decoding, video playback and seen state. No story was published to real
contacts during validation. Real contact/server publication acceptance remains
unverified.

Permanent remote profile-post saves, viewer receipts, story reactions and
advanced story/media editing remain outside this desktop baseline. The native
preview's local heart control is hidden for live stories.

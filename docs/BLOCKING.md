# Contact blocking

Desktop implements [XEP-0191](https://xmpp.org/extensions/xep-0191.html) using the
mobile `XmppBlocking`, `MonogramBlocking`, controller and Profile/Privacy UI as
references. Both use the account's server list; no private blocking protocol or
local-only success fallback is used.

Conversation Info provides Block/Unblock for other direct contacts. Privacy's
Blocked Contacts page provides search, Block Contact and exact-rule Unblock.
Domain/resource rules remain distinguishable. Confirmation names the target,
retains history, and rejects actions after an account switch. Unsupported,
unknown, loading and cached states are distinct.

The session discovers support before fetching its list. Commands omit `to` as
required by XEP-0191. Pushes accept only the authenticated account's bare sender
or no sender, validate the destination/payload, acknowledge valid updates and
replay updates received while a list query is pending. Disconnects invalidate
old replies. Lists are bounded to 10,000 entries and a 2 MiB cache.

Acknowledged snapshots are saved off the UI thread. Cache writes are serialized,
a newer session invalidates older writers, and removal of an account prevents
late writes from recreating its cache. Cached rules are visibly stale offline.
Known blocked sends keep composer drafts/files; queued rows become failed and
require explicit Retry after unblocking. Active uploads are cancelled and the
final plain/encrypted transport also checks rules. Blocking does not erase
history or silently resend old failures when unblocked.

Verification: parser/authentication/replay/disconnect/cache tests; native draft
and retry gate; private verified-TLS Prosody block/list/push/cache, server message
rejection, unblock and restored delivery with a second account resource. Native
list/profile/chooser/status/account checks include 10,000 rules (27 ms rendering,
299 ms search, 48 ms maximum heartbeat gap). Actual mobile tablet screenshots
were inspected. Native destructive buttons were not pressed against a real
account; actual mutation/delivery testing used disposable accounts only.

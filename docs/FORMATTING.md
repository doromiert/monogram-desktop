# Mobile formatting on desktop

Desktop follows the mobile source profile; original message bodies remain intact
in storage and on the wire. GTK renders selectable native text and blocks, without
a webview or HTML execution.

Supported formatting includes bold, italic, strike, inline/fenced code and quote
ranges; headings, bullet/ordered/task lists, horizontal rules, tables with column
alignment, links, footnotes, details sections, underline, highlight, subscript,
superscript, and inline/display math. Top-level ordered list source numbers are
preserved. Quotes require an explicit `> ` on each line; greentext and bare URL
punctuation stay literal. Markdown image syntax displays alt text, matching mobile.

Whole-message/media spoilers use XEP-0382. Partial spoilers use mobile's existing
`urn:monogram:spoiler:0` extension inside XEP-0394 spans; Markdown additionally
accepts `<tg-spoiler>`. Hidden content stays hidden in previews, accessibility and
copy until an explicit reveal. Custom emoji and math inside spoilers share that
state. Media and caption reveal together. Hidden URLs never trigger previews.

The native multiline composer exposes formatting in its selection context menu,
with Ctrl+B/I/U/K shortcuts. Formatting survives drafts, direct/room sends,
corrections, text forwarding, caption projection and Legacy caption companions.
The custom composer wraps a native `GtkTextView` for multiline input and tagged
ranges, with its original layout and a Nautilus-style two-pixel focus outline.
It lives in normal content rather than an AdwToolbarView bottom bar: the latter installs a GtkWindowHandle and
turns text drags into window moves.

Native typed text with no explicit entity ranges carries mobile's `unstyled`
hint. This bypasses XEP-0393 marker inference and permits mobile's CommonMark
fallback: `*text*` / `_text_` are italic, `**text**` / `__text__` bold and
`~~text~~` strike; a single `~` stays literal. Unhinted incoming wire text still follows XEP-0393, where
`*text*` means bold. Explicit entity ranges and Markdown hints keep precedence.

Wire formats are XEP-0393/0394, XEP-0372 mention references and XEP-0481's empty
Markdown content selector. Modern encrypted metadata is inside authenticated SCE;
forged outer ranges/mentions/content cannot restyle decrypted text. Legacy OMEMO
retains readable body directives and accepts only outer spoiler hiding hints and
a validated empty Markdown selector. It does not trust outer styling ranges.
No new wire namespace is introduced; the partial-spoiler proposal is documented
in mobile `protocols/NON_STANDARD_XMPP.md` and `docs/SPOILERS.md`.

Markdown input is bounded to 32768 UTF-16 units, parser depth to 24 and blocks to
2048. Long articles use a native virtual list; parsing occurs with message
preparation on workers. Math uses pinned JLaTeXMath 1.0.7 in a networkless sandbox,
two concurrent workers, size/time/memory bounds, immediate loading feedback and
a bounded texture cache. Unsupported formulas remain readable as source.

Verification: 29 outputs generated from actual mobile MessageMarkup; Unicode,
protected metadata and reply/media projection tests; Markdown profile tests; a
private native GTK fixture checks pointer text selection, spoiler reveal, draft
restoration, loaded math and bounded long-article realization with feedback under
400ms. It sends no messages to real contacts. Real-contact acceptance remains
unverified. Telegram article embedded media/widgets and custom-emoji article
composition are excluded by mobile's own text-only Markdown transport.

References: [XEP-0393](https://xmpp.org/extensions/xep-0393.html),
[XEP-0394](https://xmpp.org/extensions/xep-0394.html),
[XEP-0382](https://xmpp.org/extensions/xep-0382.html),
[XEP-0481](https://xmpp.org/extensions/xep-0481.html).

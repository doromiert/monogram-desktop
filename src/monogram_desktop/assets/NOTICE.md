The `monero.bin` mesh is copied unchanged from the user's saved Monogram model:
TMessagesProj/src/main/assets/models/monero/extruded.bin at Android commit
4736f7eddb423192e5d68435879f0b0a88371168.
SHA-256: b9d8fcfa7b9824f9ab8496f5fd6dc713bf4ae560b3592e631b12e22bc4e32dea.
The model was exported from the user's edited design/monero-logo.blend. Original
logo reference: https://commons.wikimedia.org/wiki/File:Monero-Logo.svg.
The assets/wallet directory imports the original card meshes and GLSL files from
Monogram Android commit be6876c045163e32fe07a84416621770b0964b30:
https://github.com/doromiert/monogram/tree/be6876c045163e32fe07a84416621770b0964b30/TMessagesProj/src/main/assets
These preserve the source Telegram/Monogram attribution and GPL terms. The
Monero renderer source is explicitly GPL-3.0-or-later. See LICENSES for that
license; the combined application is distributed under GPL-3.0-or-later.
The new GTK adapter ports the source WalletCardGLRenderer camera/matrices,
WalletCardFlecks seeded placement and MoneroRenderer projection/lighting. GLSL
files remain unchanged on disk; the adapter replaces only the GLES version
preamble for desktop GL 3.3. qr.svg preserves the original wallet_card_qr.xml path,
with its Android color replaced by the shader's green glyph mask.
Cairo replaces Android Canvas for dynamic fictional front text and masks; this
can produce small font/antialiasing differences. GPU material equations, card
geometry and Monero mesh are reused, rather than approximated by Cairo.

preview-*.gif are original generated local animation fixtures for this prototype,
provided with the project's license. They do not contact a GIF provider.

The XEP-0461/0428 reply fallback validation in xmpp.py follows Monogram's
ReplyReference.java (GPL-3.0-or-later), preserving Unicode code-point slicing and
the rule that malformed fallback ranges must not hide message text:
https://github.com/doromiert/monogram/blob/be6876c045163e32fe07a84416621770b0964b30/xmpp-core/src/main/java/com/negzero/monogram/xmpp/ReplyReference.java

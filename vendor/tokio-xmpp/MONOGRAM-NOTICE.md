# Monogram's bounded XML token adjustment

Source: crates.io `tokio-xmpp` **6.0.0**, upstream
`https://gitlab.com/xmpp-rs/xmpp-rs`, commit
`1710a6f99edd18cd926a2dc5d14ec532a2a6ad72` (`tokio-xmpp/`).
Published crate SHA-256:
`8921a165ab1eefd447cb1661423c582ac77b9c6960e123a71c8ad9cebc40b026`.
Original copyright notices and MPL-2.0 terms remain in effect;
see `LICENSE-MPL-2.0.txt` (canonical SPDX license text).

The only implementation change is in `src/xmlstream/common.rs`: initial and
reset parsers use a **32,768-byte** XML token limit so a mobile-compatible
4,096 UTF-16-unit invitation password fits with standard XML escaping on the
wire. All other upstream restricted-XML defaults remain enabled. Monogram retains its independent
application stanza/payload bounds; this is not an unlimited XML parser.

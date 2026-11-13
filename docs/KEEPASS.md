# KeePass credentials

KeePass fills sign-in credentials. It does not replace Monogram’s encryption
identity store or migrate existing OMEMO keys.

## Android / KeePassDX

Choose KeePassDX as Android’s preferred Autofill service in its **Form filling**
settings. Monogram’s XMPP address and password fields support Android Autofill
(API 26+), including inline keyboard suggestions. The **Password manager** action
requests suggestions or opens Android settings when no provider is enabled.
Android 6/7 can use KeePassDX’s Magikeyboard.

Use the full XMPP address as the entry’s username. KeePassDX can remember the app
association (`com.negzero.monogram`, or `com.negzero.monogram.beta` for debug
builds). Successful authentication commits the form so the provider can offer to
save/update it. Closing an unsuccessful sign-in cancels that Autofill session.
Chat, wallet and unrelated rich editors remain excluded from Autofill.

## Linux / KeePassXC

Install KeePassXC including `keepassxc-proxy`, open its database and enable
**Browser Integration**. In Monogram’s **Sign In** dialog:

1. Enter the full XMPP address.
2. Choose **Fill from KeePassXC** and approve the connection in KeePassXC. Give it
   a recognizable name, such as Monogram.
3. Approve access to the matching entry, then press **Connect** yourself.

For `alice@example.org`, use `alice@example.org` as username and
`xmpp://example.org` as URL. A username of `alice` is also accepted for this
queried domain. If multiple entries match, Monogram asks you to disambiguate
in KeePassXC instead of choosing a password arbitrarily. Expired entries are
excluded. **Save to KeePassXC** explicitly requests creation or an update to the
single matching entry; accept any confirmation shown by KeePassXC.

The connection uses KeePassXC’s Curve25519/XSalsa20-Poly1305 native messaging
protocol. Requests run outside GTK’s main thread; waiting/unlock/approval status
appears immediately. Response nonces, authenticated ciphertext, database identity,
action and account matches are checked. Frames are bounded to 1 MiB, waits to
90 seconds, and editing/closing the form cancels pending work. Association keys
are isolated per account in the existing Secret Service keyring. The disconnect
button removes that local association; revoke its key in KeePassXC to remove
permission from the database too. Existing reconnect credentials and OMEMO vault
keys remain in their current stores.

## Verification

Android SDK 36 / Robolectric 4.16.1: login-only Autofill opt-in and provider text
insertion passed. Fresh 0.2.203 arm64 APK retains the previous signing certificate
and all seven native libraries.

Desktop: `crypto_box` 0.9.1 interoperates with an independent PyNaCl/libsodium
native-messaging fixture for authorization, fill, save and subsequent reconnection.
The native GTK fill test completed in 81 ms with a largest main-loop gap of
11 ms. `tests/keepassxc_fixture.sh <unit-test-binary>` runs these tests under a
disposable display, with PyNaCl and a private Secret Service.

A live upstream KeePassXC GUI acceptance test is still unverified: the available
cached KeePassXC 2.7.12 GUI segfaulted during browser-enabled startup in the private
fixture, before Monogram connected. No personal database or manager configuration
was used. KeePassDX’s real provider prompts still need acceptance on a phone with
KeePassDX configured.

References: [KeePassDX Autofill](https://github.com/Kunzisoft/KeePassDX/wiki/AutoFill),
[Android Autofill](https://developer.android.com/identity/autofill/autofill-optimize),
[KeePassXC native protocol](https://github.com/keepassxreboot/keepassxc-browser/blob/develop/keepassxc-protocol.md).

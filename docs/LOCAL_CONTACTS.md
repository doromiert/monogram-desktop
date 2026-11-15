# Local contact details

Conversation Info offers **Local Details** for a contact's private nickname,
picture and notes, plus **Suggest Picture…**. Local changes do not rewrite the
roster or the contact's published profile. Clearing the overrides restores the
remote name and picture. Names update chat headings, the sidebar and search.

Suggestions use the same ordinary image message and “Suggested profile photo”
caption as mobile, with the conversation's usual encryption and send policy.
The recipient decides whether to use the picture.

Device Sync carries a versioned, account-scoped `contact` record containing the
peer, nickname, notes and optional PNG bytes. Local paths never cross the wire.
Actual edit revisions survive export; stale imports and acknowledgements cannot
replace a newer edit. Empty values remain versioned so clearing a field syncs.
See [the LAN sync profile](xeps/monogram-lan-sync.md).

Selected pictures are decoded on a worker and reduced to 256 pixels before
private PNG storage. Input is bounded to 32 MiB and 40 megapixels; synced PNGs
retain the 1 MiB limit. Decoded local avatars are bounded to 128 contacts,
including the active chat. Account/generation and request checks reject stale
presentation work. Picture controls show preparation feedback and dialogs clean
up temporary files when closed.

Checks cover private/account-scoped storage, edit/import revisions, paired native
JNI transfer, Android PNG export/import without path leakage, and the native
desktop editor, picture conversion, nickname/notes save and reset. No real contact
profile or suggestion message was changed for testing. Manual phone↔desktop
contact editing remains unverified.

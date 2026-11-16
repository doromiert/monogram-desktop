# Sandboxed helpers: which thread starts them

Every sandboxed helper is started with `bwrap --die-with-parent` (`src/confined.rs`, and the
launchers in `lottie.rs`, `math_ui.rs` and `media_author_run.rs`). Linux implements that as
`PR_SET_PDEATHSIG`, and the kernel counts the parent as the **thread** that created the child, not
the process. When that thread exits, the child receives SIGKILL.

## The bug

`gio::spawn_blocking` runs on GLib's worker pool, and GLib retires a pool thread after about 15
seconds without work. A helper started in a job that returns straight away, and that is then used
for longer, is killed when its starting thread retires.

Audio playback hit this: every track stopped between roughly 15 and 30 seconds, with no error. The
feeder read end-of-file, the player treated it as the end of the song, and the helper's exit status
was 9 (SIGKILL). It is fixed in `audio_preview_run.rs` by starting the helper from a thread that
lives as long as the process (`on_the_spawner_thread`). The regression test is
`audio_player::tests::a_track_keeps_playing_past_the_worker_pool_idle_timeout`, which fails without
the fix.

## Rule

Start a helper that lives longer than the call that starts it from a long-lived thread (the main
thread, a dedicated thread, or `on_the_spawner_thread`), never from a `spawn_blocking` job that
then returns the child. A helper that is started and waited for inside one call is fine.

## Still to check

These start a long-lived renderer inside `spawn_blocking` and keep it across frames, so they should
have the same problem. Not reproduced yet:

- `src/inline_emoji_ui.rs` (`Decoder::open` in `spawn_blocking`, frames pulled on a timer)
- `src/image_loader.rs` (animated stickers, decoder kept between frames)

Expected symptom: a looping sticker or custom emoji freezes or disappears about 15 seconds after it
started, because the next `frame()` call fails. `animated_emoji_ui.rs` is probably safe: its
animations are at most 6.11 s and the decoder is dropped at the end.

Suggested fix: start every sandboxed child from one shared long-lived thread in
`confined::sandbox_command`'s spawn step, and add a test that plays a looping sticker for over 20
seconds.

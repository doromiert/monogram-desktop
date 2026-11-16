# Audio preview

An audio attachment is shown as a player: a round play button, a waveform and the duration. It
plays `.ogg`, `.oga` and `.opus` (Ogg with Vorbis or Opus), `.mp3`, `.wav`, `.m4a` and `.webm`
(audio only). Anything else keeps the ordinary file presentation.

Receiving a message can prepare its audio without a click, which is the shape of a "zero-click"
attack, so the design assumes every file is hostile and keeps all parsing and decoding out of the
GTK process.

## Layers

1. **Download policy.** Audio is fetched under the same rules as other media: automatically only
   where `automatic_media_allowed` says so (the user's own, encrypted, contacts, joined rooms),
   never above 64 MiB, otherwise on a click. Preparation starts only after the download, and only
   while the bubble is on screen.
2. **Strict check** (`src/audio_check.rs`, pure Rust, no decoding). The first 64 KiB are read and
   the file is refused unless it is exactly one of the accepted shapes:
   - **Ogg:** every stream header is Opus (version 1, one or two channels, mapping family 0) or
     Vorbis (version 0, one or two channels, 8 to 96 kHz, framing bit set). One stream only: FLAC,
     Speex, Theora and any second stream are refused, as is a page that is not a beginning of
     stream, a nonzero first granule position, or a bad page version.
   - **MP3:** Layer III only (Layer I and II, free-format, reserved versions and rates are
     refused). The first frame must start exactly where the audio does and the next must follow
     where its length says, so junk in front and random bytes that look like a sync word do not
     pass. An ID3v2 tag must have a valid version, flags and syncsafe size, and cannot fill the
     file.
   - **WAV:** plain PCM or IEEE float only (ADPCM, MP3-in-WAV and the rest are refused), one or
     two channels, 8 to 96 kHz, 8/16/24/32 bit, block align and byte rate consistent with the
     rest, and no chunk larger than what remains of the file (the classic huge-allocation trick).
   - **M4A:** the first box is a small `ftyp` with an audio brand; every box size must fit the
     file, 64-bit sizes must not overflow, and no box is smaller than its own header.
   - **WebM:** an EBML header of sane size and limits, `DocType` `webm` (NUL padding allowed;
     Matroska proper is refused because it can carry video and attachments), then a Segment.
   - **Everywhere:** non-empty and under 64 MiB, and a file name whose extension names a different
     container than the bytes is refused. The checks never index out of range: a test corrupts
     every byte of every valid file and tries every prefix, and another tries every possible EBML
     length prefix (a crafted one found a real shift-overflow panic during development).
3. **Sandboxed helper** (`src/bin/monogram-audio-preview.rs`). Started under bubblewrap with
   `--unshare-all` (no network), all capabilities dropped, a cleared environment, no audio
   device, and only the GStreamer plugin files for these formats mounted (the FFmpeg plugin only
   if the small MP3 and AAC decoders are missing). The file is passed as an open descriptor and
   bubblewrap copies it in, so the bytes the helper sees are exactly the bytes that were opened
   and checked. Inside, it applies `RLIMIT_AS`, `RLIMIT_CPU`, a small `RLIMIT_FSIZE`,
   `no_new_privs`, the shared seccomp deny-list (`helper_hardening`, proven by `selftest`) and a
   wall-clock alarm for probes. It repeats the strict check on what it received, then builds an
   explicit pipeline per container (no `decodebin`, no pipeline strings): Opus or Vorbis only, one
   audio stream only, no video or extra tracks (a second stream or an unknown codec is refused),
   at most two channels and 8 to 96 kHz as the decoder reports them, and at most 30 minutes
   counted from what was actually decoded, not from any header. Output is always raw S16LE,
   48 kHz, stereo.
4. **The reply is untrusted too.** The main process reads at most 8 KiB, parses it with unknown
   fields refused, and checks the container, duration, rate, channels and that the waveform has
   exactly 64 bars.
5. **Playback** (`src/audio_player.rs`). The only thing the GTK process gives GStreamer is that
   PCM, through `appsrc ! audioconvert ! audioresample ! <sink>`: no demuxer, decoder or URI. Seeking
   restarts the helper at the new position rather than seeking inside a container. Dropping the
   player ends the helper.

## Residual risk

A bug in a decoder (libvorbis, libopus, mpg123, faad, a demuxer) is contained by the sandbox
but is not prevented by it; escaping it would take a second, kernel-level bug. The deny-list
filter is not a complete list. The strict check reads only the start of the file, so what lies
deeper is judged by the helper's decoder.

## Interface

Play and pause on the button, click the waveform to seek, Left and Right on the focused waveform
move five seconds and Space toggles. The button is libadwaita's circular suggested-action
(accent colour) with an accessible name that follows its state (download, cancel, preparing,
play, pause, try again). A file that is refused or fails shows a short reason under the waveform
and on the button instead of a player. Only one audio plays at a time.

## Tests

- `cargo test audio_check`: every container, every refusal, and the exhaustive no-panic checks.
- `cargo build --bins`, then `cargo test -- --ignored audio_preview_run`: all six containers
  through the real sandbox, PCM format and seeking, video, FLAC and surround refused, crafted
  files, a 69-minute file with an honest header refused after decoding, cancellation, the filter
  self-test, and a mutation run that corrupts every fixture 14 ways (84 variants) and requires a
  result or a refusal each time.
- `cargo test -- --ignored --test-threads=1 audio_player`: play, progress, pause, resume, seek,
  stop during start (silent `fakesink` in place of a sound card).
- `tools/headless-gtk-test.sh --bin monogram-desktop -- --ignored the_bubble_prepares`: the
  bubble end to end, and `an_audio_message_in_the_timeline`: the player appears for audio
  messages and not for other files.

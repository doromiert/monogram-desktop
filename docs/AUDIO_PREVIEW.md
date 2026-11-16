# Audio preview

An audio attachment is shown as a player: a round play button, a waveform and the duration. It
plays `.ogg`, `.oga` and `.opus` (Ogg with Vorbis or Opus), `.mp3`, `.wav`, `.m4a` and `.webm`
(audio only). Anything else keeps the ordinary file presentation.

Receiving a message can prepare its audio without a click, which is the shape of a "zero-click"
attack, so the design assumes every file is hostile and keeps all parsing and decoding out of the
GTK process.

## Layers

1. **Decoding waits for a click, except for trusted senders.** Audio is fetched under the same
   rules as other media: automatically only where `automatic_media_allowed` says so (the user's
   own, encrypted, contacts, joined rooms), never above 64 MiB, otherwise on a click. A finished
   download only makes the file *available*: the bubble shows a play button and the file size,
   and nothing parses, checks or decodes the file, in this process or in a helper, until the user
   presses play. That one press prepares and plays. The exception is the user's own messages and
   contacts in a direct chat (`prepare_without_a_click`): their audio is prepared as soon as it
   is downloaded, so the length and waveform are ready, but it never starts playing by itself.
   Room participants and strangers, even ones whose messages arrive encrypted, always need the
   click, so receiving a message from them cannot start decoding. Tests cover the rule for every
   kind of sender, a stranger's finished download (no helper starts, nothing is opened, one press
   prepares and plays), and a contact's (prepared, not played).
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
   Two properties of the launcher (`src/confined.rs`) matter as much as the flags:
   - **No inherited descriptors.** bubblewrap does not close the descriptors it inherits: a file
     the app held open without close-on-exec (a socket, a database, a handle a library opened
     carelessly) was readable inside the sandbox, which a test with a deliberately leaked
     descriptor reproduced. The launcher now marks every descriptor above stdio close-on-exec in
     the forked child, and re-enables only the input. (It marks them rather than closing them, so
     a failed exec is still reported as an error.) The helper's `selftest` prints what it
     inherited and a sandbox test requires "none".
   - **One open file description per launch.** A duplicated descriptor shares its read offset with
     the original, so helpers started together (rapid seeking) each received part of the file and
     some got nothing. Each launch reopens the verified file through `/proc/self/fd` instead.
   At most two files are prepared at a time: a flood of audio messages scrolling into view
   queues, because each preparation is a helper process and a memory copy of the file, and the
   work runs on the shared I/O thread pool (the wait happens before a pool thread is taken).
4. **The reply is untrusted too.** The main process reads at most 8 KiB, parses it with unknown
   fields refused, and checks the container, duration, rate, channels and that the waveform has
   exactly 64 bars.
5. **Playback** (`src/audio_player.rs`). The only thing the GTK process gives GStreamer is that
   PCM, through `appsrc ! audioconvert ! audioresample ! <sink>`: no demuxer, decoder or URI. Seeking
   restarts the helper at the new position rather than seeking inside a container. Dropping the
   player ends the helper.

## FFmpeg

MP3 and AAC (M4A) have a small GStreamer decoder (mpg123, faad) and an FFmpeg one (gst-libav).
The helper uses FFmpeg only if the system already has it installed and the small decoder is
missing; nothing is bundled for audio. If a format's decoder is not installed at all, the bubble
says so ("Can't play this: it needs the GStreamer mpg123 plugin or FFmpeg (gst-libav), which is
not installed") instead of reporting a corrupt file: the app checks the plugin directories before
starting anything, and the helper reports a missing demuxer, parser or decoder with its own exit
code (8). The packages that already ship `gst-libav` for video playback (Arch, Nix, the AppImage)
are unchanged.

## Residual risk

A bug in a decoder (libvorbis, libopus, mpg123, faad, a demuxer) is contained by the sandbox
but is not prevented by it; escaping it would take a second, kernel-level bug. The deny-list
filter is not a complete list. The strict check reads only the start of the file, so what lies
deeper is judged by the helper's decoder.

## Interface

Press play (it downloads first if needed, then prepares and plays); pause on the same button,
click the waveform to seek, Left and Right on the focused waveform
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

The helper must be started from a thread that lives as long as the process, or it is killed
after about 15 seconds; see [SANDBOXED_HELPERS.md](SANDBOXED_HELPERS.md).

## The other audio player

Upstream also added `audio_ui.rs`, which plays audio through the media player (decoded in the app
process, on request). The sandboxed preview in this document is asked first and takes every `audio/*`
type and its own extensions. `audio_ui.rs` only receives what the preview does not claim: a FLAC or
raw AAC file that arrives without an audio type. Such a file is therefore not strictly checked or
sandboxed. Removing that fallback would make "unsupported" the answer for them too.


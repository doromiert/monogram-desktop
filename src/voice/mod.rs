//! Voice messages: record from the microphone and show the recording as it happens.
//!
//! * [`meter`]: loudness, timer text and the bar history.
//! * [`recorder`]: the GStreamer pipeline that writes an Ogg Opus file.
//! * [`wave`]: the waveform widget.
//!
//! See `docs/VOICE.md`.
pub mod meter;
pub mod recorder;
pub mod wave;

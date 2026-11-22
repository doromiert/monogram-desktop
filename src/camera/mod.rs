//! Camera access: ask the desktop portal, then stream from PipeWire.
//!
//! * [`portal`]: asks the person for permission and returns the PipeWire connection.
//! * [`stream`]: turns that connection into bounded RGBA pictures.
//! * [`frame`]: one picture, size-checked, for the screen or as a JPEG.
//!
//! See `docs/CAMERA.md`.
pub mod frame;
pub mod portal;
pub mod stream;
#[cfg(test)]
pub(crate) mod fake;

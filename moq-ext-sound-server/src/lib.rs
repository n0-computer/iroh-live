//! Turns on moq-audio's PipeWire and PulseAudio backends on Linux.
//!
//! This crate has no code. Depending on it enables the `pipewire` and
//! `pulseaudio` features of `moq-audio` on Linux, and nothing on other
//! platforms, where moq-audio's PulseAudio code does not compile. Crates use it
//! to offer a `sound-server` feature that `--all-features` can turn on
//! anywhere.

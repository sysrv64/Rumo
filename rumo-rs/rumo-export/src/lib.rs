// SPDX-License-Identifier: Apache-2.0

//! Headless MP4 export pipeline: frame source -> RGBA -> NV12 -> H.264 -> MP4.
//!
//! Sound sources are mixed into one PCM stream, encoded to AAC and written as a
//! second track of the same MP4 (docs/11 §11.5); see [`audio`].
//!
//! The timing, NV12 conversion, frame-source and audio layers are platform-free
//! and run on host builds. The encoder/muxer backend lives behind
//! `#[cfg(target_os = "android")]`; on host it is a stub returning
//! [`ExportError::UnsupportedPlatform`], so `cargo test` works everywhere.

pub mod audio;
pub mod backend;
pub mod error;
pub mod exporter;
pub mod frames;
pub mod jni;
pub mod nv12;
pub mod render_source;
pub mod timing;

#[cfg(target_os = "android")]
mod android;

pub use audio::{
    AUDIO_BITRATE, AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioMixer, AudioSource, MIME_AAC,
};
pub use backend::{PlatformBackend, VideoBackend};
pub use error::{ExportError, Result};
pub use exporter::{Exporter, ExporterConfig};
pub use frames::{FrameSource, RgbaFrames};
pub use render_source::{Layer, LayerFrameSource};

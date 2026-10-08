// SPDX-License-Identifier: Apache-2.0

use std::fmt;

/// Errors from the headless export pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// The current build target has no video encoder backend (host builds).
    UnsupportedPlatform,
    /// Configuration is unusable (zero/odd dimensions, non-positive fps, ...).
    InvalidConfig(&'static str),
    /// The RGBA frame handed to [`crate::Exporter::write_frame`] has the wrong length.
    InvalidFrameLen { expected: usize, got: usize },
    /// Presentation timestamps must be non-decreasing.
    NonMonotonicPts { previous: i64, got: i64 },
    /// MediaCodec operation failed.
    Codec(String),
    /// An audio source could not be mixed or encoded.
    Audio(String),
    /// MediaMuxer operation failed.
    Muxer(String),
    /// File / fd handling failed.
    Io(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::UnsupportedPlatform => {
                write!(f, "video export is only supported on Android")
            }
            ExportError::InvalidConfig(msg) => write!(f, "invalid export config: {msg}"),
            ExportError::InvalidFrameLen { expected, got } => {
                write!(f, "frame length {got} does not match expected {expected}")
            }
            ExportError::NonMonotonicPts { previous, got } => {
                write!(f, "non-monotonic pts: {got} after {previous}")
            }
            ExportError::Codec(msg) => write!(f, "media codec error: {msg}"),
            ExportError::Audio(msg) => write!(f, "audio export error: {msg}"),
            ExportError::Muxer(msg) => write!(f, "media muxer error: {msg}"),
            ExportError::Io(msg) => write!(f, "io error: {msg}"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<rumo_media::mixer::MixerError> for ExportError {
    fn from(err: rumo_media::mixer::MixerError) -> Self {
        ExportError::Audio(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ExportError>;

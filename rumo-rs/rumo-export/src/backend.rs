// SPDX-License-Identifier: Apache-2.0

//! Platform abstraction over the video encoder + container muxer.

use crate::audio::AudioSource;
use crate::error::{ExportError, Result};
use crate::exporter::ExporterConfig;

/// A configured NV12-in / MP4-out encoder.
///
/// Implemented by the Android MediaCodec/MediaMuxer backend on device and
/// by [`StubBackend`] everywhere else. The exporter feeds it NV12 frames
/// in presentation order and closes it once all frames (and the
/// end-of-stream marker) have been drained.
///
/// Audio arrives through [`VideoBackend::add_audio_source`] rather than a
/// second trait: there is exactly one container to write into, so the audio
/// track belongs to whoever owns the muxer. The method is part of the trait
/// (with a required impl) so the host stub can be *honest* about it —
/// returning [`ExportError::UnsupportedPlatform`] like every other call —
/// instead of the trait quietly not describing what the pipeline does.
pub trait VideoBackend: Sized {
    /// Create and start a backend for `config` (open output, configure and
    /// start the codec, prepare the muxer).
    fn open(config: &ExporterConfig) -> Result<Self>;

    /// Mix one audio source into the export's single audio track, encoding
    /// whatever became mixable. Returns `false` when the source contributes no
    /// frames — an audio layer over a silent file, or a clip shorter than one
    /// output frame — which is a valid project state, not a failure.
    fn add_audio_source(&mut self, source: AudioSource) -> Result<bool>;

    /// Encode one tightly-packed NV12 frame at `pts_us`.
    fn queue_nv12(&mut self, nv12: &[u8], pts_us: i64) -> Result<()>;

    /// Signal end-of-stream, drain remaining output, stop the codec and
    /// finalize the container.
    fn finish(&mut self) -> Result<()>;
}

/// Host fallback: every operation reports [`ExportError::UnsupportedPlatform`].
#[derive(Debug, Default)]
pub struct StubBackend;

impl VideoBackend for StubBackend {
    fn open(_config: &ExporterConfig) -> Result<Self> {
        Err(ExportError::UnsupportedPlatform)
    }

    fn add_audio_source(&mut self, _source: AudioSource) -> Result<bool> {
        Err(ExportError::UnsupportedPlatform)
    }

    fn queue_nv12(&mut self, _nv12: &[u8], _pts_us: i64) -> Result<()> {
        Err(ExportError::UnsupportedPlatform)
    }

    fn finish(&mut self) -> Result<()> {
        Err(ExportError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "android")]
pub type PlatformBackend = crate::android::AndroidBackend;

#[cfg(not(target_os = "android"))]
pub type PlatformBackend = StubBackend;

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use crate::exporter::Exporter;

    #[test]
    fn stub_reports_unsupported_platform() {
        let config = ExporterConfig::new("out.mp4", 64, 64, 30.0, 1_000_000);
        assert!(matches!(
            Exporter::new(config),
            Err(ExportError::UnsupportedPlatform)
        ));
    }

    #[test]
    fn stub_audio_is_unsupported_too() {
        let mut stub = StubBackend;
        let source = AudioSource {
            samples: vec![0.0; 4],
            sample_rate: 48_000,
            channels: 1,
            start_ms: 0,
            duration_ms: 0,
            gain: 1.0,
        };
        // No encoder means no muxer, so audio cannot be mixed either: the stub
        // must say so rather than pretend to accept and drop the source.
        assert_eq!(
            stub.add_audio_source(source),
            Err(ExportError::UnsupportedPlatform)
        );
    }
}

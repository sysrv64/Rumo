// SPDX-License-Identifier: Apache-2.0

//! The frame-order and timing half of the export pipeline, platform-free.

use crate::audio::AudioSource;
use crate::backend::{PlatformBackend, VideoBackend};
use crate::error::{ExportError, Result};
use crate::nv12;
use crate::timing;

/// Everything the encoder needs to open an MP4/H.264 stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ExporterConfig {
    pub width: u32,
    pub height: u32,
    /// Frames per second; used for the codec hint and auto timestamps.
    pub fps: f64,
    pub bitrate: u32,
    /// Seconds between forced I-frames (MediaCodec `i-frame-interval`).
    pub i_frame_interval_secs: f64,
    pub output_path: String,
}

impl ExporterConfig {
    /// Default config with a 2-second I-frame interval.
    pub fn new(
        output_path: impl Into<String>,
        width: u32,
        height: u32,
        fps: f64,
        bitrate: u32,
    ) -> Self {
        Self {
            width,
            height,
            fps,
            bitrate,
            i_frame_interval_secs: 2.0,
            output_path: output_path.into(),
        }
    }

    /// RGBA byte length of one frame at this resolution.
    pub fn rgba_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Packed-ARGB pixel count of one frame at this resolution — one Java
    /// `int` per pixel, which is four bytes per pixel like [`Self::rgba_len`].
    pub fn argb_len(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Reject configurations the pipeline cannot honor.
    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(ExportError::InvalidConfig("width and height must be > 0"));
        }
        if self.width % 2 != 0 || self.height % 2 != 0 {
            return Err(ExportError::InvalidConfig(
                "width and height must be even for NV12 4:2:0",
            ));
        }
        if !(self.fps > 0.0) || !self.fps.is_finite() {
            return Err(ExportError::InvalidConfig(
                "fps must be positive and finite",
            ));
        }
        if self.bitrate == 0 {
            return Err(ExportError::InvalidConfig("bitrate must be > 0"));
        }
        if !(self.i_frame_interval_secs > 0.0) || !self.i_frame_interval_secs.is_finite() {
            return Err(ExportError::InvalidConfig(
                "i_frame_interval_secs must be positive and finite",
            ));
        }
        if self.output_path.is_empty() {
            return Err(ExportError::InvalidConfig("output_path must not be empty"));
        }
        Ok(())
    }
}

/// Drives a [`VideoBackend`], owning frame ordering and timestamp policy.
///
/// Use [`Exporter::new`] on device. Tests and alternative backends use
/// [`Exporter::new_with_backend`].
pub struct Exporter<B: VideoBackend> {
    config: ExporterConfig,
    backend: B,
    last_pts_us: Option<i64>,
    frame_count: u64,
}

impl Exporter<PlatformBackend> {
    /// Open the platform backend (Android: MediaCodec + MediaMuxer).
    /// On host builds the backend is a stub and this returns
    /// [`ExportError::UnsupportedPlatform`].
    pub fn new(config: ExporterConfig) -> Result<Self> {
        config.validate()?;
        let backend = PlatformBackend::open(&config)?;
        Self::new_with_backend(config, backend)
    }
}

impl<B: VideoBackend> Exporter<B> {
    /// Build an exporter around an already-open `backend`.
    pub fn new_with_backend(config: ExporterConfig, backend: B) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            backend,
            last_pts_us: None,
            frame_count: 0,
        })
    }

    pub fn config(&self) -> &ExporterConfig {
        &self.config
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend itself, for the audio path: mixing happens in the backend
    /// because it owns the muxer the AAC track has to be added to.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Mix one audio source into the export's single audio track.
    ///
    /// Returns `false` when the source contributes no frames. Sources may be
    /// added at any time before [`Exporter::finish`]; the encoder runs as each
    /// one arrives, so the mix is never held as PCM in full.
    pub fn add_audio_source(&mut self, source: AudioSource) -> Result<bool> {
        self.backend.add_audio_source(source)
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn last_pts_us(&self) -> Option<i64> {
        self.last_pts_us
    }

    /// Timestamp the next frame would get from [`Self::write_frame_auto`].
    pub fn next_pts_us(&self) -> i64 {
        timing::pts_for_frame(self.frame_count, self.config.fps)
    }

    /// Reject a frame of the wrong size or a timestamp that goes backwards,
    /// shared by both input layouts.
    fn check_frame(&self, got: usize, expected: usize, pts_us: i64) -> Result<()> {
        if got != expected {
            return Err(ExportError::InvalidFrameLen { expected, got });
        }
        if let Some(previous) = self.last_pts_us {
            if pts_us < previous {
                return Err(ExportError::NonMonotonicPts {
                    previous,
                    got: pts_us,
                });
            }
        }
        Ok(())
    }

    /// Encode one frame that is **already** NV12 — the layout the encoder takes.
    ///
    /// This is the GPU path's entry point (docs/12 §12.3): `shader_nv12.wgsl`
    /// packed the frame on the device, so there is nothing to convert here and
    /// nothing to copy — the backend takes the bytes into the codec's input
    /// buffer synchronously, and the caller's buffer is free the moment this
    /// returns.
    ///
    /// A frame of the wrong length is rejected rather than queued: a short frame
    /// would leave the encoder reading whatever followed it in memory.
    pub fn write_frame_nv12(&mut self, nv12: &[u8], pts_us: i64) -> Result<()> {
        let expected = nv12::nv12_len(self.config.width, self.config.height).unwrap_or(0);
        self.check_frame(nv12.len(), expected, pts_us)?;
        self.queue_nv12(nv12, pts_us)
    }

    /// Hand one NV12 frame to the encoder and commit the frame counter.
    fn queue_nv12(&mut self, nv12: &[u8], pts_us: i64) -> Result<()> {
        self.backend.queue_nv12(nv12, pts_us)?;
        self.last_pts_us = Some(pts_us);
        self.frame_count += 1;
        Ok(())
    }

    /// Signal end-of-stream, drain and finalize. Consumes the exporter.
    pub fn finish(mut self) -> Result<()> {
        self.backend.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockBackend {
        frames: Vec<(i64, usize)>,
        /// The NV12 bytes as handed to the backend, so a test can compare the
        /// encoded frames instead of only their sizes.
        bytes: Vec<Vec<u8>>,
        audio: usize,
    }

    impl VideoBackend for MockBackend {
        fn open(_config: &ExporterConfig) -> Result<Self> {
            Ok(Self::default())
        }

        fn add_audio_source(&mut self, _source: AudioSource) -> Result<bool> {
            self.audio += 1;
            Ok(true)
        }

        fn queue_nv12(&mut self, nv12: &[u8], pts_us: i64) -> Result<()> {
            self.frames.push((pts_us, nv12.len()));
            self.bytes.push(nv12.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
    }

    /// One 2x2 NV12 frame filled with `fill`.
    ///
    /// There is no RGBA or ARGB form here any more: §12.4 deleted the CPU
    /// conversion, so the exporter only ever sees a frame that is already in the
    /// layout the encoder takes.
    fn nv12_frame(fill: u8) -> Vec<u8> {
        vec![fill; nv12::nv12_len(2, 2).unwrap()]
    }

    fn config() -> ExporterConfig {
        ExporterConfig::new("unused.mp4", 2, 2, 30.0, 1_000_000)
    }

    fn exporter() -> Exporter<MockBackend> {
        Exporter::new_with_backend(config(), MockBackend::default()).unwrap()
    }

    #[test]
    fn next_pts_follow_fps_and_are_monotonic() {
        let mut ex = exporter();
        let frame = nv12_frame(0);
        let mut pts = Vec::new();
        for _ in 0..4 {
            let next = ex.next_pts_us();
            ex.write_frame_nv12(&frame, next).unwrap();
            pts.push(next);
        }
        assert_eq!(pts, vec![0, 33_333, 66_667, 100_000]);
        assert!(timing::is_strictly_increasing(&pts));
        assert_eq!(ex.frame_count(), 4);
    }

    #[test]
    fn queue_receives_nv12_of_expected_size() {
        let mut ex = exporter();
        ex.write_frame_nv12(&nv12_frame(0), 0).unwrap();
        let (_, nv12_len) = ex.backend().frames[0];
        assert_eq!(nv12_len, nv12::nv12_len(2, 2).unwrap());
    }

    /// Two different frames must reach the backend as two different frames: a
    /// caller's buffer is free the moment the call returns, so nothing may be
    /// retained and re-sent.
    #[test]
    fn consecutive_frames_do_not_share_stale_bytes() {
        let mut ex = exporter();
        ex.write_frame_nv12(&nv12_frame(0x11), 0).unwrap();
        ex.write_frame_nv12(&nv12_frame(0x22), 33_333).unwrap();
        assert_ne!(ex.backend().bytes[0], ex.backend().bytes[1]);
        assert_eq!(ex.frame_count(), 2);
    }

    #[test]
    fn equal_timestamps_are_allowed_and_later_ones_are_not() {
        let mut ex = exporter();
        let frame = nv12_frame(0);
        ex.write_frame_nv12(&frame, 100).unwrap();
        // Equal timestamps are allowed (non-decreasing).
        ex.write_frame_nv12(&frame, 100).unwrap();
        assert!(matches!(
            ex.write_frame_nv12(&frame, 50),
            Err(ExportError::NonMonotonicPts {
                previous: 100,
                got: 50
            })
        ));
        assert_eq!(ex.frame_count(), 2);
    }

    #[test]
    fn wrong_and_invalid_inputs_are_rejected() {
        let mut ex = exporter();
        assert_eq!(
            ex.write_frame_nv12(&[0u8; 3], 0),
            Err(ExportError::InvalidFrameLen {
                expected: 6,
                got: 3
            })
        );
        let bad = ExporterConfig {
            width: 3,
            ..config()
        };
        assert!(matches!(
            Exporter::<MockBackend>::new_with_backend(bad, MockBackend::default()),
            Err(ExportError::InvalidConfig(_))
        ));
    }

    /// The GPU path hands over a frame that is already NV12: nothing is
    /// converted, and the bytes reach the encoder unchanged.
    #[test]
    fn nv12_frames_reach_the_backend_unchanged() {
        let frame = vec![7u8; nv12::nv12_len(2, 2).unwrap()];
        let mut ex = exporter();
        ex.write_frame_nv12(&frame, 0).unwrap();
        assert_eq!(ex.backend().bytes[0], frame);
        assert_eq!(ex.frame_count(), 1);
        assert_eq!(ex.last_pts_us(), Some(0));
    }

    /// A frame that is not exactly one NV12 frame is a caller bug: a short one
    /// would leave the encoder reading past it, so it never reaches the backend.
    #[test]
    fn nv12_frames_reject_a_wrong_length_and_a_pts_regression() {
        let mut ex = exporter();
        assert!(matches!(
            ex.write_frame_nv12(&[0u8; 5], 0),
            Err(ExportError::InvalidFrameLen {
                expected: 6,
                got: 5
            })
        ));
        assert_eq!(ex.frame_count(), 0, "a rejected frame must not count");
        let frame = vec![0u8; nv12::nv12_len(2, 2).unwrap()];
        ex.write_frame_nv12(&frame, 100).unwrap();
        assert!(matches!(
            ex.write_frame_nv12(&frame, 50),
            Err(ExportError::NonMonotonicPts {
                previous: 100,
                got: 50
            })
        ));
        assert_eq!(ex.frame_count(), 1);
    }

    #[test]
    fn finish_consumes_and_closes_backend() {
        let mut ex = exporter();
        ex.write_frame_nv12(&nv12_frame(0), 0).unwrap();
        ex.finish().unwrap();
    }

    #[test]
    fn audio_sources_reach_the_backend() {
        let mut ex = exporter();
        let source = AudioSource {
            samples: vec![0.0; 8],
            sample_rate: 48_000,
            channels: 1,
            start_ms: 250,
            duration_ms: 0,
            gain: 0.5,
        };
        assert!(ex.add_audio_source(source.clone()).unwrap());
        assert_eq!(ex.backend().audio, 1);
        // Video frames are unaffected by the audio path.
        ex.write_frame_nv12(&nv12_frame(0), 0).unwrap();
        assert_eq!(ex.backend().frames.len(), 1);
        assert_eq!(ex.backend().audio, 1);
    }
}

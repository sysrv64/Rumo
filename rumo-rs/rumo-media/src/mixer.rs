// SPDX-License-Identifier: Apache-2.0

//! Mixing several sound sources into one PCM stream.
//!
//! Every source of a project — an `AUDIO` layer, the sound of a `MEDIA` clip —
//! is resampled and channel-mapped to a single output format, placed at its own
//! `start_ms`, scaled by its own `gain`, and summed. The result is what the MP4
//! exporter encodes into one AAC track (docs/11 §11.5), so nothing here needs a
//! platform: it is plain `f32` arithmetic over [`crate::audio::DecodedAudio`] and
//! is covered by host tests.
//!
//! Two decisions worth naming, because they are what the export contract rests
//! on:
//!
//! * Sources are summed **unweighted**, in the order they were added, and only
//!   clamped afterwards. A weighted sum would move the mix when a layer is
//!   added, so the export of an unchanged project would change audibly.
//! * Offsets are applied at *mix* time: the output frame index is the timeline
//!   position, and a source's `start_ms` decides only which output frame its
//!   sample 0 lands on. Presentation timestamps are therefore monotonic per
//!   track for free.

use std::fmt;

use crate::audio::DecodedAudio;
use crate::audio::convert_channels;
use crate::resample::resample_linear;

/// Output sample rate of the exported AAC track, in Hz.
///
/// 48 kHz is what essentially every Android capture and playback path runs at,
/// so no source is resampled upward in practice and AAC's own delay stays the
/// one constant the muxer has to live with.
pub const DEFAULT_OUTPUT_RATE: u32 = 48_000;

/// Output channel count of the exported AAC track.
///
/// Stereo even when the project holds a single mono source: players do not have
/// to upmix, and `convert_channels` replicates mono rather than dropping a side.
pub const DEFAULT_OUTPUT_CHANNELS: u16 = 2;

/// What the mixer could not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixerError {
    /// The mixer's own output format is unusable (zero rate or channels).
    InvalidOutput(&'static str),
    /// A source cannot be mixed: no samples, no rate, no channels, or a
    /// non-finite gain.
    InvalidSource(&'static str),
}

impl fmt::Display for MixerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MixerError::InvalidOutput(msg) => write!(f, "invalid mixer output: {msg}"),
            MixerError::InvalidSource(msg) => write!(f, "invalid audio source: {msg}"),
        }
    }
}

impl std::error::Error for MixerError {}

/// One sound source on the timeline.
///
/// Interleaved `f32` PCM plus everything the mix needs: where the source starts,
/// how long it plays, and how loud it is.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSource {
    /// Interleaved `f32` PCM in `-1.0..=1.0`, `frames * channels` long.
    pub samples: Vec<f32>,
    /// Sample rate of `samples`, in Hz.
    pub sample_rate: u32,
    /// Channel count of `samples`.
    pub channels: u16,
    /// Where the source starts in the project, in milliseconds.
    pub start_ms: i64,
    /// How long the source plays, in milliseconds; `0` means "to the end of
    /// the samples".
    pub duration_ms: i64,
    /// Linear gain; `1.0` leaves the samples untouched.
    pub gain: f32,
}

impl AudioSource {
    /// A source from already decoded PCM, placed at `start_ms` and limited to
    /// `duration_ms` (`0` = whole track).
    pub fn new(audio: DecodedAudio, start_ms: i64, duration_ms: i64, gain: f32) -> Self {
        Self {
            samples: audio.samples,
            sample_rate: audio.sample_rate,
            channels: audio.channels,
            start_ms,
            duration_ms,
            gain,
        }
    }
}

/// A source already mapped onto the output format.
#[derive(Debug, Clone)]
struct Prepared {
    /// Interleaved `f32` at the output rate and channel count, gain applied.
    samples: Vec<f32>,
    /// Output frame the source starts on.
    start_frame: usize,
}

/// Output frame that contains `start_ms`.
///
/// Floor, so a source never starts a fraction of a frame early; with the
/// default 48 kHz that is a 20.8 µs quantization, far below what a muxed
/// AAC track can show anyway.
fn start_frame(start_ms: i64, rate: u32) -> usize {
    if start_ms <= 0 {
        return 0;
    }
    let frames = start_ms as u128 * rate as u128 / 1000;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Frames kept from a source of `frames` frames, honouring `duration_ms`.
fn trimmed_frames(frames: usize, duration_ms: i64, rate: u32) -> usize {
    if duration_ms <= 0 {
        return frames;
    }
    let wanted = duration_ms as u128 * rate as u128 / 1000;
    usize::try_from(wanted).unwrap_or(usize::MAX).min(frames)
}

/// Sums audio sources into one stream at a fixed output format.
///
/// Sources are added whole; nothing is decoded or played here. The output
/// length is the longest source end, so a source that starts late still leaves
/// the earlier part of the mix silent rather than shifting it.
#[derive(Debug, Clone)]
pub struct AudioMixer {
    out_rate: u32,
    out_channels: u16,
    sources: Vec<Prepared>,
    total_frames: usize,
    cursor: usize,
}

impl AudioMixer {
    /// An empty mixer producing `out_rate` / `out_channels` PCM.
    pub fn new(out_rate: u32, out_channels: u16) -> Result<Self, MixerError> {
        if out_rate == 0 {
            return Err(MixerError::InvalidOutput("sample rate must be > 0"));
        }
        if out_channels == 0 {
            return Err(MixerError::InvalidOutput("channel count must be > 0"));
        }
        Ok(Self {
            out_rate,
            out_channels,
            sources: Vec::new(),
            total_frames: 0,
            cursor: 0,
        })
    }

    /// A mixer with the exported-audio defaults.
    pub fn with_defaults() -> Result<Self, MixerError> {
        Self::new(DEFAULT_OUTPUT_RATE, DEFAULT_OUTPUT_CHANNELS)
    }

    /// Output sample rate in Hz.
    pub fn output_rate(&self) -> u32 {
        self.out_rate
    }

    /// Output channel count.
    pub fn output_channels(&self) -> u16 {
        self.out_channels
    }

    /// Number of sources added so far.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Total output frames the mix produces, i.e. the end of the last source.
    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    /// Total output frames already returned by [`AudioMixer::render_chunk`].
    pub fn rendered_frames(&self) -> usize {
        self.cursor
    }

    /// Length of the mix in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.total_frames as f64 / f64::from(self.out_rate)
    }

    /// `true` once every frame of the mix has been rendered.
    pub fn is_finished(&self) -> bool {
        self.cursor >= self.total_frames
    }

    /// Add a source, extending the mix if it reaches past the current end.
    ///
    /// The source is resampled to the output rate and channel-mapped once, here,
    /// so rendering is a plain sum afterwards.
    pub fn add_source(&mut self, source: AudioSource) -> Result<(), MixerError> {
        if source.sample_rate == 0 {
            return Err(MixerError::InvalidSource("sample rate must be > 0"));
        }
        if source.channels == 0 {
            return Err(MixerError::InvalidSource("channel count must be > 0"));
        }
        if !source.gain.is_finite() {
            return Err(MixerError::InvalidSource("gain must be finite"));
        }
        if source.samples.is_empty() {
            return Err(MixerError::InvalidSource("no samples"));
        }

        let resampled = if source.sample_rate == self.out_rate {
            source.samples
        } else {
            resample_linear(
                &source.samples,
                source.sample_rate,
                source.channels,
                self.out_rate,
            )
        };
        let mut samples = if source.channels == self.out_channels {
            resampled
        } else {
            convert_channels(&resampled, source.channels, self.out_channels)
        };

        let frames = samples.len() / self.out_channels as usize;
        let keep = trimmed_frames(frames, source.duration_ms, self.out_rate);
        samples.truncate(keep * self.out_channels as usize);
        if source.gain != 1.0 {
            for sample in samples.iter_mut() {
                *sample *= source.gain;
            }
        }

        // A source shorter than one output frame has no place on this timeline:
        // keeping it would extend the mix by its whole offset in silence.
        if keep == 0 {
            return Ok(());
        }
        let start_frame = start_frame(source.start_ms, self.out_rate);
        self.total_frames = self.total_frames.max(start_frame.saturating_add(keep));
        self.sources.push(Prepared {
            samples,
            start_frame,
        });
        Ok(())
    }

    /// Mix `frames` output frames starting at `start_frame`, without touching
    /// the render cursor.
    ///
    /// The sum is clamped to `-1.0..=1.0`: the AAC encoder takes 16-bit input,
    /// and an out-of-range `f32` would wrap around there into a loud click
    /// instead of the loudness loss clipping gives.
    pub fn mix_range(&self, start_frame: usize, frames: usize) -> Vec<f32> {
        let ch = self.out_channels as usize;
        let mut out = vec![0.0f32; frames.saturating_mul(ch)];
        if out.is_empty() {
            return out;
        }
        let range_end = start_frame.saturating_add(frames);
        for source in &self.sources {
            let source_end = source.start_frame.saturating_add(source.samples.len() / ch);
            let begin = start_frame.max(source.start_frame);
            let end = range_end.min(source_end);
            if begin >= end {
                continue;
            }
            let dst = (begin - start_frame) * ch;
            let src = (begin - source.start_frame) * ch;
            let count = (end - begin) * ch;
            for i in 0..count {
                out[dst + i] += source.samples[src + i];
            }
        }
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        out
    }

    /// Render the next chunk into `out` and advance the cursor.
    ///
    /// `out` holds interleaved `f32`; a trailing partial frame is left
    /// untouched. Returns the frames written, `0` at the end of the mix, so the
    /// caller can size the next chunk from the answer.
    pub fn render_chunk(&mut self, out: &mut [f32]) -> usize {
        let ch = self.out_channels as usize;
        if out.is_empty() || ch == 0 || self.is_finished() {
            return 0;
        }
        let capacity = out.len() / ch;
        let frames = capacity.min(self.total_frames - self.cursor);
        out[..frames * ch].copy_from_slice(&self.mix_range(self.cursor, frames));
        self.cursor += frames;
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mono source of `frames` frames at `rate`, all set to `value`.
    fn mono(rate: u32, frames: usize, value: f32) -> AudioSource {
        AudioSource {
            samples: vec![value; frames],
            sample_rate: rate,
            channels: 1,
            start_ms: 0,
            duration_ms: 0,
            gain: 1.0,
        }
    }

    /// Mono mixer at 1000 Hz: one output frame is one millisecond, so the
    /// expected values can be written down without arithmetic.
    fn mixer_1k() -> AudioMixer {
        AudioMixer::new(1000, 1).expect("valid output format")
    }

    #[test]
    fn empty_mixer_is_silent_and_finished() {
        let mut mixer = mixer_1k();
        assert_eq!(mixer.total_frames(), 0);
        assert!(mixer.is_finished());
        assert_eq!(mixer.source_count(), 0);
        assert_eq!(mixer.duration_seconds(), 0.0);
        let mut out = [1.0f32; 4];
        assert_eq!(mixer.render_chunk(&mut out), 0);
        assert_eq!(out, [1.0; 4], "nothing may be written past the end");
    }

    #[test]
    fn non_overlapping_sources_land_at_their_offsets() {
        let mut mixer = mixer_1k();
        let mut first = mono(1000, 2, 1.0);
        first.start_ms = 0;
        mixer.add_source(first).expect("add first");
        let mut second = mono(1000, 2, 1.0);
        second.start_ms = 5;
        mixer.add_source(second).expect("add second");

        assert_eq!(mixer.source_count(), 2);
        assert_eq!(mixer.total_frames(), 7, "last source ends at 7 ms");
        assert_eq!(
            mixer.mix_range(0, 7),
            vec![1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0]
        );
    }

    #[test]
    fn negative_offset_is_clamped_to_zero() {
        let mut mixer = mixer_1k();
        let mut source = mono(1000, 2, 0.5);
        source.start_ms = -500;
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.total_frames(), 2);
        assert_eq!(mixer.mix_range(0, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn gain_scales_a_source() {
        let mut mixer = mixer_1k();
        let mut source = mono(1000, 2, 1.0);
        source.gain = 0.25;
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.mix_range(0, 2), vec![0.25, 0.25]);
    }

    #[test]
    fn overlapping_sources_sum() {
        let mut mixer = mixer_1k();
        mixer.add_source(mono(1000, 3, 0.4)).expect("add first");
        let mut second = mono(1000, 3, 0.3);
        second.start_ms = 1;
        mixer.add_source(second).expect("add second");

        assert_eq!(mixer.total_frames(), 4);
        let mixed = mixer.mix_range(0, 4);
        assert!((mixed[0] - 0.4).abs() < 1e-6);
        assert!(
            (mixed[1] - 0.7).abs() < 1e-6,
            "overlap must sum, got {mixed:?}"
        );
        assert!(
            (mixed[2] - 0.7).abs() < 1e-6,
            "overlap must sum, got {mixed:?}"
        );
        assert!((mixed[3] - 0.3).abs() < 1e-6, "only the second source here");
    }

    #[test]
    fn sum_is_clamped_instead_of_wrapping() {
        let mut mixer = mixer_1k();
        mixer.add_source(mono(1000, 2, 0.8)).expect("add loud");
        mixer.add_source(mono(1000, 2, 0.9)).expect("add louder");

        let loud = mixer.mix_range(0, 2);
        assert_eq!(loud, vec![1.0, 1.0], "1.7 must clip to 1.0");

        let mut mixer = mixer_1k();
        mixer.add_source(mono(1000, 2, -0.8)).expect("add quiet");
        mixer.add_source(mono(1000, 2, -0.9)).expect("add quieter");
        let quiet = mixer.mix_range(0, 2);
        assert_eq!(quiet, vec![-1.0, -1.0], "-1.7 must clip to -1.0");
    }

    #[test]
    fn source_is_resampled_and_replicated_to_stereo() {
        // 2 mono frames at 1000 Hz -> 4 frames at 2000 Hz, then duplicated per
        // channel: [0.0, 0.5, 1.0, 1.0] by linear interpolation.
        let mut mixer = AudioMixer::new(2000, 2).expect("valid output format");
        let mut source = mono(1000, 2, 0.0);
        source.samples = vec![0.0, 1.0];
        mixer.add_source(source).expect("add");

        assert_eq!(mixer.output_rate(), 2000);
        assert_eq!(mixer.output_channels(), 2);
        assert_eq!(mixer.total_frames(), 4);
        let mixed = mixer.mix_range(0, 4);
        let expected = [0.0f32, 0.0, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0];
        for (got, want) in mixed.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    #[test]
    fn stereo_source_is_averaged_to_mono() {
        let mut mixer = mixer_1k();
        let mut source = mono(1000, 2, 0.0);
        source.channels = 2;
        source.samples = vec![0.0, 0.2, 0.4, 0.6]; // frames (0.0,0.2), (0.4,0.6)
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.total_frames(), 2);
        let mixed = mixer.mix_range(0, 2);
        assert!((mixed[0] - 0.1).abs() < 1e-6);
        assert!((mixed[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn duration_trims_the_tail() {
        let mut mixer = mixer_1k();
        // Values stay inside -1..=1: the mix clamps its output, and a sample
        // outside the range would come back clamped instead of as written.
        let mut source = mono(1000, 5, 0.1);
        source.samples = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        source.duration_ms = 2;
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.total_frames(), 2, "the window is two frames");
        // `mix_range` returns the frames it was asked for, padding past the end
        // of the mix with silence — the same shape the sibling test relies on,
        // and what a chunked renderer needs to write fixed-size buffers.
        assert_eq!(
            mixer.mix_range(0, 5),
            vec![0.1, 0.2, 0.0, 0.0, 0.0],
            "only the window is audible"
        );
        assert_eq!(mixer.mix_range(0, 2), vec![0.1, 0.2]);
    }

    #[test]
    fn duration_beyond_the_source_keeps_all_of_it() {
        let mut mixer = mixer_1k();
        let mut source = mono(1000, 3, 0.5);
        source.duration_ms = 60_000;
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.total_frames(), 3);
    }

    #[test]
    fn render_chunk_walks_the_mix_exactly_once() {
        let mut mixer = mixer_1k();
        let mut first = mono(1000, 4, 1.0);
        first.start_ms = 2;
        mixer.add_source(first).expect("add first");

        let mut rendered: Vec<f32> = Vec::new();
        // An odd buffer length leaves a trailing partial frame untouched.
        let mut buf = vec![-1.0f32; 3];
        loop {
            let frames = mixer.render_chunk(&mut buf);
            if frames == 0 {
                assert_eq!(buf, [-1.0; 3], "a finished mix writes nothing");
                break;
            }
            rendered.extend_from_slice(&buf[..frames]);
            buf.iter_mut().for_each(|s| *s = -1.0);
        }
        assert_eq!(rendered, vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(mixer.rendered_frames(), 6);
        assert_eq!(mixer.rendered_frames(), mixer.total_frames());
        assert!(mixer.is_finished());
    }

    #[test]
    fn mix_range_is_repeatable_and_cursor_free() {
        let mut mixer = mixer_1k();
        mixer.add_source(mono(1000, 4, 0.5)).expect("add");
        let mut buf = [0.0f32; 2];
        assert_eq!(mixer.render_chunk(&mut buf), 2);
        assert_eq!(buf, [0.5, 0.5]);
        // The same range must give the same samples, whatever the cursor did.
        assert_eq!(mixer.mix_range(0, 2), vec![0.5, 0.5]);
        assert_eq!(mixer.rendered_frames(), 2);
    }

    #[test]
    fn rejects_unusable_output_and_sources() {
        assert!(matches!(
            AudioMixer::new(0, 2),
            Err(MixerError::InvalidOutput(_))
        ));
        assert!(matches!(
            AudioMixer::new(48_000, 0),
            Err(MixerError::InvalidOutput(_))
        ));
        assert!(AudioMixer::with_defaults().is_ok());

        let mut mixer = mixer_1k();
        let mut no_rate = mono(1000, 1, 0.0);
        no_rate.sample_rate = 0;
        assert!(matches!(
            mixer.add_source(no_rate),
            Err(MixerError::InvalidSource(_))
        ));

        let mut no_channels = mono(1000, 1, 0.0);
        no_channels.channels = 0;
        assert!(matches!(
            mixer.add_source(no_channels),
            Err(MixerError::InvalidSource(_))
        ));

        let mut silent = mono(1000, 1, 0.0);
        silent.samples.clear();
        assert!(matches!(
            mixer.add_source(silent),
            Err(MixerError::InvalidSource(_))
        ));

        let mut nan_gain = mono(1000, 1, 0.0);
        nan_gain.gain = f32::NAN;
        assert!(matches!(
            mixer.add_source(nan_gain),
            Err(MixerError::InvalidSource(_))
        ));

        assert_eq!(mixer.source_count(), 0, "a rejected source is not mixed");
        assert_eq!(mixer.total_frames(), 0);
    }

    #[test]
    fn source_from_decoded_audio_carries_the_stream_parameters() {
        // 800 frames at 8 kHz is 100 ms, i.e. 100 frames at the 1 kHz output.
        // 8:1 decimation means output frame `n` comes from input frame `8n`, so
        // the value is held in blocks of eight for the result to be readable.
        let mut samples = vec![0.0f32; 800];
        samples[0..8].fill(0.1);
        samples[8..16].fill(0.2);
        let decoded = DecodedAudio {
            samples,
            sample_rate: 8_000,
            channels: 1,
        };
        assert_eq!(decoded.frames(), 800);
        let source = AudioSource::new(decoded, 250, 0, 2.0);
        assert_eq!(source.sample_rate, 8_000);
        assert_eq!(source.channels, 1);
        assert_eq!(source.start_ms, 250);
        assert_eq!(source.duration_ms, 0);
        assert_eq!(source.gain, 2.0);

        let mut mixer = AudioMixer::new(1000, 1).expect("valid output format");
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.total_frames(), 350, "250 ms offset + 100 frames");
        let tail = mixer.mix_range(250, 2);
        assert!((tail[0] - 0.2).abs() < 1e-6, "got {tail:?}");
        assert!((tail[1] - 0.4).abs() < 1e-6, "got {tail:?}");
        assert_eq!(
            mixer.mix_range(0, 250),
            vec![0.0; 250],
            "nothing before the offset"
        );
    }

    #[test]
    fn source_shorter_than_one_output_frame_is_dropped() {
        // Two samples at 8 kHz are 0.25 ms, which is less than one 1 kHz
        // output frame: it must not become a second of silence at its offset.
        let mut mixer = mixer_1k();
        let mut source = mono(8000, 2, 0.5);
        source.start_ms = 1000;
        mixer.add_source(source).expect("add");
        assert_eq!(mixer.source_count(), 0);
        assert_eq!(mixer.total_frames(), 0);
    }

    #[test]
    fn errors_are_printable_and_are_std_errors() {
        for err in [
            MixerError::InvalidOutput("x"),
            MixerError::InvalidSource("x"),
        ] {
            let _: &dyn std::error::Error = &err;
            assert!(!err.to_string().is_empty());
            assert!(!format!("{err:?}").is_empty());
        }
    }
}

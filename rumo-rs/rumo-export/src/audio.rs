// SPDX-License-Identifier: Apache-2.0

//! Audio half of the MP4 export: constants and the platform-free conversions
//! between the mixed `f32` stream and the AAC encoder's input.
//!
//! Everything here runs on the host and is unit-tested there; the `AMediaCodec`
//! side lives in [`crate::android`].

pub use rumo_media::mixer::{AudioMixer, AudioSource, MixerError};

/// MIME type of the exported audio track: AAC-LC in an MP4 (LATM) container.
pub const MIME_AAC: &str = "audio/mp4a-latm";

/// Sample rate of the exported AAC track, in Hz.
///
/// 48 kHz is what Android capture and playback paths use, so sources are almost
/// never resampled and the encoder's own delay stays the only constant the
/// muxer has to carry.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Channel count of the exported AAC track.
pub const AUDIO_CHANNELS: u16 = 2;

/// Bitrate of the exported AAC track, in bits per second.
///
/// 128 kbps is the usual stereo-AAC default (≈174 kbps per channel-pair for a
/// transparent encode, 96 kbps being audibly lossier). Constant rather than
/// configurable: the editor has no audio-quality control, and a value that
/// silently follows the video bitrate would make the audio the first thing to
/// degrade on a low-bitrate export.
pub const AUDIO_BITRATE: u32 = 128_000;

/// Presentation timestamp of the output frame `frame`, in microseconds.
///
/// Derived from the frame index instead of accumulated, so a chunked mix cannot
/// drift: the encoder is fed one continuous stream whose position *is* the
/// timeline position. Monotonic by construction (docs/11 §11.5).
pub fn frames_to_pts_us(frame: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    let micros = frame as u128 * 1_000_000 / sample_rate as u128;
    i64::try_from(micros).unwrap_or(i64::MAX)
}

/// Mixed `f32` samples as the 16-bit signed PCM a `MediaCodec` encoder takes.
///
/// Clamping happens *here*, before the cast: `f32 as i16` saturates in Rust, but
/// only for values that reached the cast, and a `NaN` would become `0` while a
/// value past the range would depend on the cast's behaviour rather than on the
/// mixer's contract. A loud sum must clip, not wrap into a click.
pub fn to_i16_pcm(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&sample| {
            let clamped = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            (clamped * 32_767.0).round() as i16
        })
        .collect()
}

/// An empty mixer at the exported audio format.
///
/// A separate constructor rather than `AudioMixer::new` at each call site so the
/// rate/channel constants are read in exactly one place. The format is a
/// constant, so this only fails if a constant is zero.
pub fn new_mixer() -> std::result::Result<AudioMixer, MixerError> {
    AudioMixer::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rumo_media::audio::DecodedAudio;

    #[test]
    fn mixer_uses_the_documented_export_format() {
        let mixer = new_mixer().expect("valid output format");
        assert_eq!(mixer.output_rate(), AUDIO_SAMPLE_RATE);
        assert_eq!(mixer.output_channels(), AUDIO_CHANNELS);
        assert_eq!(AUDIO_SAMPLE_RATE, 48_000);
        assert_eq!(AUDIO_CHANNELS, 2);
        assert_eq!(AUDIO_BITRATE, 128_000);
        assert_eq!(MIME_AAC, "audio/mp4a-latm");
    }

    #[test]
    fn pts_follows_the_frame_index_without_drift() {
        // 48 kHz: one frame is 1000/48 us, which is not an integer, so this
        // checks the division rather than a hand-computed constant.
        assert_eq!(frames_to_pts_us(0, 48_000), 0);
        assert_eq!(frames_to_pts_us(48_000, 48_000), 1_000_000);
        assert_eq!(frames_to_pts_us(96_000, 48_000), 2_000_000);
        let mut previous = 0;
        for frame in 0..200_000 {
            let pts = frames_to_pts_us(frame, AUDIO_SAMPLE_RATE);
            assert!(pts >= previous, "pts must not go backwards at {frame}");
            previous = pts;
        }
        assert_eq!(frames_to_pts_us(100, 0), 0, "a zero rate has no timeline");
    }

    #[test]
    fn pcm_conversion_clips_instead_of_wrapping() {
        assert_eq!(to_i16_pcm(&[0.0]), vec![0]);
        assert_eq!(to_i16_pcm(&[1.0]), vec![32_767]);
        assert_eq!(to_i16_pcm(&[-1.0]), vec![-32_767]);
        // Out of range on both sides: the extremes are what a loud sum reaches.
        assert_eq!(to_i16_pcm(&[4.0, -4.0]), vec![32_767, -32_767]);
        assert_eq!(
            to_i16_pcm(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
            vec![0, 0, 0]
        );
        assert_eq!(to_i16_pcm(&[0.5, -0.5]), vec![16_384, -16_384]);
        assert!(to_i16_pcm(&[]).is_empty());
    }

    #[test]
    fn decoded_source_feeds_the_mixer() {
        let decoded = DecodedAudio {
            samples: vec![0.5; 96_000],
            sample_rate: AUDIO_SAMPLE_RATE,
            channels: AUDIO_CHANNELS,
        };
        let mut mixer = new_mixer().expect("valid output format");
        mixer
            .add_source(AudioSource::new(decoded, 1000, 0, 1.0))
            .expect("add source");
        assert_eq!(mixer.source_count(), 1);
        assert_eq!(
            mixer.total_frames(),
            96_000,
            "1 s of audio plus a 1000 ms offset at 48 kHz"
        );
    }
}

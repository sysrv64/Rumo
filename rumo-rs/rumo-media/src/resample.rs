// SPDX-License-Identifier: Apache-2.0

//! Sample-rate conversion by simple linear interpolation.
//!
//! Pure Rust, zero dependencies, no C. Audio is interleaved `f32`; the
//! channel count is carried through untouched. This is a playback-grade
//! resampler (fast, allocation-light), not a mastering SRC.

/// Output frame count for `in_frames` frames resampled
/// `in_rate` → `out_rate`, rounded to the nearest frame.
fn out_frame_count(in_frames: usize, in_rate: u32, out_rate: u32) -> usize {
    if in_rate == 0 || in_frames == 0 {
        return 0;
    }
    let numerator = in_frames as u128 * out_rate as u128;
    ((numerator + u128::from(in_rate) / 2) / u128::from(in_rate)) as usize
}

/// Resample interleaved `f32` audio from `in_rate` to `out_rate` with
/// linear interpolation.
///
/// Returns an empty vector for a zero rate, zero channels, or empty
/// input. When `in_rate == out_rate` the input is copied verbatim.
/// The last output frame never reads past the last input frame: the
/// interpolation endpoint is clamped to the final frame.
pub fn resample_linear(input: &[f32], in_rate: u32, channels: u16, out_rate: u32) -> Vec<f32> {
    let ch = channels as usize;
    if ch == 0 || in_rate == 0 || out_rate == 0 || input.is_empty() {
        return Vec::new();
    }
    let in_frames = input.len() / ch;
    if in_frames == 0 {
        return Vec::new();
    }
    if in_rate == out_rate {
        return input.to_vec();
    }

    let out_frames = out_frame_count(in_frames, in_rate, out_rate);
    let ratio = f64::from(in_rate) / f64::from(out_rate);
    let mut out = Vec::with_capacity(out_frames * ch);
    let last_base = (in_frames - 1) * ch;

    for j in 0..out_frames {
        let pos = j as f64 * ratio;
        let i0 = pos.floor() as usize;
        if i0 >= in_frames {
            out.extend_from_slice(&input[last_base..last_base + ch]);
            continue;
        }
        let frac = (pos - i0 as f64) as f32;
        let a = i0 * ch;
        let b = if i0 + 1 < in_frames { a + ch } else { a };
        for c in 0..ch {
            let s0 = input[a + c];
            let s1 = input[b + c];
            out.push(s0 + (s1 - s0) * frac);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rate_copies_input() {
        let input = [0.0f32, 0.25, -0.5, 1.0];
        assert_eq!(resample_linear(&input, 48_000, 1, 48_000), input.to_vec());
        assert_eq!(resample_linear(&input, 44_100, 2, 44_100), input.to_vec());
    }

    #[test]
    fn upsample_doubles_frame_count() {
        let input = vec![0.0f32; 8000]; // 8000 frames mono
        let out = resample_linear(&input, 8000, 1, 16_000);
        assert_eq!(out.len(), 16_000);
        let stereo = vec![0.0f32; 2 * 4000];
        let out = resample_linear(&stereo, 8000, 2, 16_000);
        assert_eq!(out.len(), 2 * 8000);
    }

    #[test]
    fn downsample_frame_count() {
        let input = vec![0.0f32; 48_000];
        let out = resample_linear(&input, 48_000, 1, 44_100);
        assert_eq!(out.len(), 44_100);
    }

    #[test]
    fn constant_signal_is_preserved() {
        let input = vec![0.25f32; 1000];
        let out = resample_linear(&input, 1000, 1, 2205);
        assert!(!out.is_empty());
        assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn ramp_interpolates_and_clamps_at_boundary() {
        // 2 mono frames at 2 Hz -> 4 frames at 4 Hz.
        // positions 0.0, 0.5, 1.0, 1.5 -> [0.0, 0.5, 1.0, 1.0(clamped)]
        let out = resample_linear(&[0.0, 1.0], 2, 1, 4);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
        assert!(
            (out[3] - 1.0).abs() < 1e-6,
            "boundary must clamp, got {}",
            out[3]
        );
    }

    #[test]
    fn stereo_channels_stay_in_lockstep() {
        // frame0 = (0.0, 10.0), frame1 = (2.0, 12.0)
        let input = [0.0f32, 10.0, 2.0, 12.0];
        let out = resample_linear(&input, 2, 2, 4);
        assert_eq!(out.len(), 8);
        // First and last frames are exact.
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 10.0).abs() < 1e-6);
        assert!((out[6] - 2.0).abs() < 1e-6);
        assert!((out[7] - 12.0).abs() < 1e-6);
        // Midpoint frames interpolate within each channel.
        assert!((out[2] - 1.0).abs() < 1e-6);
        assert!((out[3] - 11.0).abs() < 1e-6);
    }

    #[test]
    fn guards_empty_and_zero_rate() {
        assert!(resample_linear(&[], 8000, 1, 16_000).is_empty());
        assert!(resample_linear(&[0.0], 0, 1, 16_000).is_empty());
        assert!(resample_linear(&[0.0], 8000, 1, 0).is_empty());
        assert!(resample_linear(&[0.0], 8000, 0, 16_000).is_empty());
    }

    #[test]
    fn frame_count_helper_rounds() {
        assert_eq!(out_frame_count(8000, 8000, 16_000), 16_000);
        assert_eq!(out_frame_count(48_000, 48_000, 44_100), 44_100);
        assert_eq!(out_frame_count(1, 3, 2), 1); // 0.67 -> 1
        assert_eq!(out_frame_count(0, 8000, 16_000), 0);
    }
}

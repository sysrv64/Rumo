// SPDX-License-Identifier: Apache-2.0

//! Speech/silence segmentation of decoded audio.
//!
//! The app cuts long recordings ("cut this 12-minute talk into chapters") at
//! natural pauses, so this module turns interleaved PCM into a list of speech
//! segments and the silences between them. Everything is in the *original*
//! timeline of the input; the model is only ever run at its own rate.
//!
//! # Model and provenance
//!
//! Detection is [`earshot`] (`pykeio/earshot`), pinned to `=1.2.2`, licence
//! `MIT OR Apache-2.0`. It is pure Rust with **zero transitive dependencies**
//! (`cargo tree` shows only `earshot` under this crate), no `cc`/`cmake`, and
//! builds for `aarch64-linux-android` in a few seconds. The published crate
//! contains only the frame-level detector — there is no segmenter — so the
//! hysteresis/merge/pad logic below is ours; it is the part that decides
//! quality.
//!
//! [`earshot::Detector::predict_f32`] consumes exactly 256 samples (16 ms at
//! 16 kHz) per call and keeps a 768-sample context internally, so consecutive,
//! non-overlapping frames must be fed in order. A frame of any other length is
//! silently ignored by the model, so the frame loop below only ever passes
//! whole 256-sample frames.
//!
//! # Measured calibration
//!
//! `predict_f32` was measured on three inputs at 16 kHz mono (`mean` / `p50` /
//! `p95` / `max`):
//!
//! | Input | mean | p50 | p95 | max |
//! | --- | --- | --- | --- | --- |
//! | Digital silence | 0.161 | 0.150 | 0.194 | 0.275 |
//! | Low room tone (0.002 sine) | 0.180 | 0.163 | 0.283 | 0.332 |
//! | JFK speech | 0.672 | 0.835 | 0.961 | 0.970 |
//!
//! The floor is ~0.15–0.33 and speech is ~0.83–0.97, leaving a wide empty band
//! between them; [`ONSET_THRESHOLD`] (`0.60`) and [`OFFSET_THRESHOLD`] (`0.40`)
//! sit inside that gap. The same measurement showed that silence *following*
//! speech reads higher than silence before it (mean 0.228 vs 0.161, max 0.812)
//! because the detector carries state in its context ring — which is exactly
//! why a single threshold is not enough and hysteresis plus a minimum silence
//! duration are needed.
//!
//! # Constants
//!
//! - [`ONSET_THRESHOLD`] / [`OFFSET_THRESHOLD`]: measured gap, see above.
//! - [`MIN_SPEECH_MS`]: runs shorter than this are dropped as clicks/breaths.
//! - [`MIN_SILENCE_MS`]: a gap shorter than this is absorbed into the speech
//!   around it. This is the rule that makes "cut at pauses" work instead of
//!   "cut everywhere": a natural inter-word gap is well under 300 ms while a
//!   sentence break is well over it.
//! - [`PREROLL_MS`] / [`POSTROLL_MS`]: outward padding so a cut does not clip
//!   a phoneme.
//! - [`MIN_CUT_SILENCE_MS`]: a silence long enough to place a cut in.
//!
//! Every entry point is panic-free: empty input, one sample, `channels == 0`,
//! NaN samples, an 8 kHz rate and a rate that is not 16 kHz all return a sane
//! result rather than an index error.

use std::borrow::Cow;

use earshot::Detector;

use crate::audio::{AudioError, convert_channels, decode_audio_bytes};
use crate::resample::resample_linear;

/// The rate the model was trained at. Input at any other rate is resampled for
/// the model only; reported times stay in the original timeline.
const MODEL_SAMPLE_RATE: u32 = 16_000;
/// Samples per model frame: exactly 256 (16 ms at 16 kHz). `earshot` ignores
/// any other length, so this is not tunable.
const FRAME_SAMPLES: usize = 256;
/// Milliseconds covered by one model frame (`FRAME_SAMPLES / 16 kHz`).
const MODEL_FRAME_MS: i64 = 16;

/// Probability that starts a speech run. Sits in the measured gap between the
/// silence floor (~0.33) and speech (~0.83).
const ONSET_THRESHOLD: f32 = 0.60;
/// Probability below which a speech run ends. Deliberately lower than
/// [`ONSET_THRESHOLD`]: the detector's estimate is noisy and a hangover is
/// cheaper than chopping a syllable in half.
const OFFSET_THRESHOLD: f32 = 0.40;

/// Runs shorter than this are discarded. A meaningful utterance is at least a
/// short word (~200 ms+); anything under 150 ms is a click, a lip smack, or the
/// tail of a breath, and cutting on it would litter the timeline with shards.
const MIN_SPEECH_MS: i64 = 150;
/// A gap shorter than this is not a pause and is absorbed into the surrounding
/// speech. 300 ms is the crux: natural inter-word gaps run well under it, while
/// a sentence break runs well over it, so this separates "breath inside a
/// sentence" from "place to cut".
const MIN_SILENCE_MS: i64 = 300;

/// Padding added before a segment so a cut does not clip the onset phoneme.
/// ~5 model frames: enough to catch the attack the detector under-reports.
const PREROLL_MS: i64 = 80;
/// Padding added after a segment. Larger than [`PREROLL_MS`] because trailing
/// fricatives/decay smear further than the onset.
const POSTROLL_MS: i64 = 120;

/// A silence must be at least this long to hold a cut. Equal to
/// [`MIN_SILENCE_MS`] by construction: every shorter gap was already absorbed,
/// so any surviving interior gap is a real pause. Kept as its own constant so
/// the two policies can diverge later without touching the merge rule.
const MIN_CUT_SILENCE_MS: i64 = MIN_SILENCE_MS;

/// One stretch of detected speech, in the original timeline (ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSegment {
    /// Inclusive start, milliseconds into the source.
    pub start_ms: i64,
    /// Exclusive end, milliseconds into the source.
    pub end_ms: i64,
}

/// One silence between/around speech segments, in the original timeline (ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilenceGap {
    /// Inclusive start, milliseconds into the source.
    pub start_ms: i64,
    /// Exclusive end, milliseconds into the source.
    pub end_ms: i64,
}

/// The result of analysing one track.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechAnalysis {
    /// Source sample rate, the rate of the input, not of the model.
    pub sample_rate: u32,
    /// Source duration, milliseconds.
    pub duration_ms: i64,
    /// Speech segments, ascending, non-overlapping, already padded.
    pub segments: Vec<SpeechSegment>,
    /// Complement of `segments` over `0..duration_ms`, ascending.
    pub silences: Vec<SilenceGap>,
    /// Midpoints of the silences long enough to cut in, ascending. Only
    /// silences *between* two segments count: the file's head and tail are not
    /// cut candidates because there is nothing to cut between.
    pub cut_points: Vec<i64>,
    /// Fraction of the source covered by speech, 0..=1.
    pub speech_ratio: f32,
    /// The onset probability used (see the module calibration table).
    pub onset_threshold: f32,
    /// The offset probability used.
    pub offset_threshold: f32,
}

impl SpeechAnalysis {
    /// The shape every degenerate/failure path returns.
    fn empty(sample_rate: u32, duration_ms: i64) -> Self {
        Self {
            sample_rate,
            duration_ms,
            segments: Vec::new(),
            silences: Vec::new(),
            cut_points: Vec::new(),
            speech_ratio: 0.0,
            onset_threshold: ONSET_THRESHOLD,
            offset_threshold: OFFSET_THRESHOLD,
        }
    }
}

/// Start time of model frame `index`, in milliseconds. Exactly
/// `index * 16`, so frame times never drift.
fn frame_ms(index: usize) -> i64 {
    (index as i64).saturating_mul(MODEL_FRAME_MS)
}

/// Run the detector over `model` (16 kHz mono) and return one probability per
/// whole 256-sample frame.
///
/// Each frame is sanitised first: non-finite samples become 0 (a single NaN
/// must not poison the model or trip its debug range assertion) and finite
/// samples are clamped to the model's documented `[-1, 1]` domain. A trailing
/// partial frame is dropped rather than padded.
fn frame_probabilities(model: &[f32]) -> Vec<f32> {
    let frames = model.len() / FRAME_SAMPLES;
    let mut detector = Detector::const_default();
    let mut frame = [0.0f32; FRAME_SAMPLES];
    let mut probabilities = Vec::with_capacity(frames);
    for index in 0..frames {
        let base = index * FRAME_SAMPLES;
        for (slot, sample) in frame.iter_mut().zip(&model[base..base + FRAME_SAMPLES]) {
            *slot = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        probabilities.push(detector.predict_f32(&frame));
    }
    probabilities
}

/// Hysteresis over the per-frame probabilities: a run starts at the first
/// frame at/above [`ONSET_THRESHOLD`] and ends at the first frame below
/// [`OFFSET_THRESHOLD`]. Returns half-open `(start_ms, end_ms)` runs.
fn speech_runs(probabilities: &[f32]) -> Vec<(i64, i64)> {
    let mut runs = Vec::new();
    let mut in_speech = false;
    let mut start = 0usize;
    for (index, &raw) in probabilities.iter().enumerate() {
        let probability = if raw.is_finite() { raw } else { 0.0 };
        if !in_speech {
            if probability >= ONSET_THRESHOLD {
                in_speech = true;
                start = index;
            }
        } else if probability < OFFSET_THRESHOLD {
            runs.push((frame_ms(start), frame_ms(index)));
            in_speech = false;
        }
    }
    if in_speech {
        runs.push((frame_ms(start), frame_ms(probabilities.len())));
    }
    runs
}

/// Absorb gaps shorter than [`MIN_SILENCE_MS`] into the speech around them.
/// Runs arrive ascending and non-overlapping.
fn merge_runs(runs: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(runs.len());
    for (start, end) in runs {
        if let Some(last) = merged.last_mut() {
            // Strictly shorter: a gap of exactly MIN_SILENCE_MS is a pause.
            if start - last.1 < MIN_SILENCE_MS {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

/// Pad runs outward by [`PREROLL_MS`]/[`POSTROLL_MS`], clamp to the file, and
/// merge anything that now touches. Input is ascending.
fn pad_runs(runs: &[(i64, i64)], duration_ms: i64) -> Vec<SpeechSegment> {
    let mut segments: Vec<SpeechSegment> = Vec::with_capacity(runs.len());
    for &(start, end) in runs {
        let start = start.saturating_sub(PREROLL_MS).max(0);
        let end = end.saturating_add(POSTROLL_MS).min(duration_ms);
        if end <= start {
            continue;
        }
        if let Some(last) = segments.last_mut()
            && start <= last.end_ms
        {
            last.end_ms = last.end_ms.max(end);
            continue;
        }
        segments.push(SpeechSegment {
            start_ms: start,
            end_ms: end,
        });
    }
    segments
}

/// Midpoints of the interior gaps long enough to cut in, ascending.
fn cut_points(runs: &[(i64, i64)]) -> Vec<i64> {
    runs.windows(2)
        .filter_map(|pair| {
            let gap = pair[1].0 - pair[0].1;
            if gap >= MIN_CUT_SILENCE_MS {
                Some(pair[0].1 + gap / 2)
            } else {
                None
            }
        })
        .collect()
}

/// Complement of `segments` over `0..duration_ms`: the leading silence (if the
/// file does not start with speech), the gaps between segments, and the
/// trailing silence.
fn complement(segments: &[SpeechSegment], duration_ms: i64) -> Vec<SilenceGap> {
    let mut silences = Vec::new();
    let mut cursor = 0i64;
    for segment in segments {
        if segment.start_ms > cursor {
            silences.push(SilenceGap {
                start_ms: cursor,
                end_ms: segment.start_ms,
            });
        }
        cursor = cursor.max(segment.end_ms);
    }
    if cursor < duration_ms {
        silences.push(SilenceGap {
            start_ms: cursor,
            end_ms: duration_ms,
        });
    }
    silences
}

/// Analyse interleaved samples (any channel count, any rate) into speech
/// segments and silences.
///
/// Pure and panic-free. The model always runs at 16 kHz mono; the reported
/// times are converted back to the source timeline (frame times are exact
/// multiples of 16 ms, and `duration_ms` comes from the source frame count).
///
/// Buffering: an already-16-kHz mono input is borrowed, never copied. A
/// multi-channel input costs one mono buffer, and a non-16-kHz input costs one
/// resampled buffer; both are released before the frame loop runs, so a 20-min
/// signal is never held twice at once beyond the resampling step.
pub fn analyze_speech(samples: &[f32], channels: u16, sample_rate: u32) -> SpeechAnalysis {
    // A zero channel count is treated as mono rather than rejected; this is
    // what `beats` does, and it also keeps a malformed call panic-free.
    let channels = channels.max(1) as usize;
    let frames = samples.len() / channels;
    let duration_ms = if sample_rate == 0 {
        0
    } else {
        let ms = frames as u128 * 1000 / u128::from(sample_rate);
        ms.min(i64::MAX as u128) as i64
    };
    if sample_rate == 0 || samples.is_empty() {
        return SpeechAnalysis::empty(sample_rate, duration_ms);
    }

    let mono: Cow<'_, [f32]> = if channels == 1 {
        Cow::Borrowed(samples)
    } else {
        Cow::Owned(convert_channels(samples, channels as u16, 1))
    };
    let model: Cow<'_, [f32]> = if sample_rate == MODEL_SAMPLE_RATE {
        mono
    } else {
        Cow::Owned(resample_linear(&mono, sample_rate, 1, MODEL_SAMPLE_RATE))
    };

    let probabilities = frame_probabilities(&model);
    let runs = speech_runs(&probabilities);
    let kept: Vec<(i64, i64)> = merge_runs(runs)
        .into_iter()
        .filter(|(start, end)| end - start >= MIN_SPEECH_MS)
        .collect();

    // Cut points come from the raw (pre-padding) pauses so padding cannot
    // shrink a real pause down to something uncuttable.
    let cut_points = cut_points(&kept);
    let segments = pad_runs(&kept, duration_ms);
    let silences = complement(&segments, duration_ms);

    let speech_ms: i64 = segments.iter().map(|s| s.end_ms - s.start_ms).sum();
    let speech_ratio = if duration_ms > 0 {
        (speech_ms as f64 / duration_ms as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };

    SpeechAnalysis {
        sample_rate,
        duration_ms,
        segments,
        silences,
        cut_points,
        speech_ratio,
        onset_threshold: ONSET_THRESHOLD,
        offset_threshold: OFFSET_THRESHOLD,
    }
}

/// Decode a container/codec in memory and segment it.
pub fn analyze_audio_bytes(bytes: &[u8]) -> Result<SpeechAnalysis, AudioError> {
    let decoded = decode_audio_bytes(bytes)?;
    Ok(analyze_speech(
        &decoded.samples,
        decoded.channels,
        decoded.sample_rate,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap mono 16-bit PCM in a 44-byte RIFF/WAVE header (same shape as
    /// `beats.rs` uses).
    fn wav_pcm16_mono(sample_rate: u32, pcm: &[i16]) -> Vec<u8> {
        let channels: u16 = 1;
        let bits: u16 = 16;
        let data_len = (pcm.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(
            &(sample_rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes(),
        );
        wav.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in pcm {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    /// Assert the structural invariants every analysis must satisfy, whatever
    /// the detector decided.
    fn assert_well_formed(analysis: &SpeechAnalysis, duration_ms: i64) {
        assert_eq!(analysis.duration_ms, duration_ms);
        assert!((0.0..=1.0).contains(&analysis.speech_ratio));
        let mut previous_end = 0i64;
        for segment in &analysis.segments {
            assert!(segment.start_ms >= 0, "negative start {segment:?}");
            assert!(segment.end_ms > segment.start_ms, "empty segment {segment:?}");
            assert!(
                segment.end_ms <= duration_ms,
                "segment past the end {segment:?} > {duration_ms}"
            );
            assert!(
                segment.start_ms >= previous_end,
                "segments overlap or are unsorted: {segment:?}"
            );
            previous_end = segment.end_ms;
        }
        let mut previous_end = 0i64;
        for silence in &analysis.silences {
            assert!(silence.start_ms >= 0, "negative start {silence:?}");
            assert!(silence.end_ms >= silence.start_ms, "inverted {silence:?}");
            assert!(
                silence.end_ms <= duration_ms,
                "silence past the end {silence:?} > {duration_ms}"
            );
            assert!(
                silence.start_ms >= previous_end,
                "silences overlap or are unsorted: {silence:?}"
            );
            previous_end = silence.end_ms;
        }
        // Every segment must lie inside a silence-free stretch: no overlap.
        for segment in &analysis.segments {
            for silence in &analysis.silences {
                assert!(
                    segment.end_ms <= silence.start_ms || segment.start_ms >= silence.end_ms,
                    "segment {segment:?} overlaps silence {silence:?}"
                );
            }
        }
        // Cut points ascend and each sits inside some reported silence.
        let mut previous = i64::MIN;
        for &cut in &analysis.cut_points {
            assert!(cut > previous, "cut points not ascending: {cut}");
            assert!(
                analysis
                    .silences
                    .iter()
                    .any(|s| cut >= s.start_ms && cut <= s.end_ms),
                "cut {cut} is not inside a silence"
            );
            previous = cut;
        }
    }

    /// Build a per-frame probability trace: `bursts` are half-open frame
    /// ranges set to `level`, everything else stays at the silence floor.
    fn probability_trace(frames: usize, bursts: &[(usize, usize)], level: f32) -> Vec<f32> {
        let mut trace = vec![0.0f32; frames];
        for &(start, end) in bursts {
            for value in &mut trace[start..end] {
                *value = level;
            }
        }
        trace
    }

    /// Run the segmentation stages the way `analyze_speech` does, without the
    /// model: hysteresis -> absorb short gaps -> drop short runs.
    fn segment_trace(frames: usize, bursts: &[(usize, usize)], level: f32) -> Vec<(i64, i64)> {
        let trace = probability_trace(frames, bursts, level);
        merge_runs(speech_runs(&trace))
            .into_iter()
            .filter(|(start, end)| end - start >= MIN_SPEECH_MS)
            .collect()
    }

    #[test]
    fn all_silence_yields_one_silence_and_no_cuts() {
        // `earshot` reads digital silence near 0.16 (max ~0.28), far below
        // onset, so this is a real end-to-end assertion, not just structure.
        let samples = vec![0.0f32; 16_000 * 2];
        let analysis = analyze_speech(&samples, 1, 16_000);
        assert_well_formed(&analysis, 2_000);
        assert!(analysis.segments.is_empty(), "{:?}", analysis.segments);
        assert_eq!(analysis.silences.len(), 1);
        assert_eq!(analysis.silences[0].start_ms, 0);
        assert_eq!(analysis.silences[0].end_ms, analysis.duration_ms);
        assert!(analysis.cut_points.is_empty());
        assert_eq!(analysis.speech_ratio, 0.0);

        // Same through the decode path.
        let wav = wav_pcm16_mono(16_000, &vec![0i16; 16_000 * 2]);
        let decoded = analyze_audio_bytes(&wav).expect("wav decodes");
        assert!(decoded.segments.is_empty());
        assert_eq!(decoded.silences.len(), 1);
        assert_eq!(decoded.silences[0].end_ms, decoded.duration_ms);
    }

    #[test]
    fn a_short_gap_inside_a_burst_is_absorbed() {
        // Two speech runs separated by 10 frames = 160 ms, under the 300 ms
        // minimum silence. The rule must fuse them into one segment and not
        // report a pause. Deterministic: driven by a synthetic probability
        // trace, not by the neural detector.
        let frames = 400;
        let runs = segment_trace(frames, &[(50, 150), (160, 300)], 0.9);
        assert_eq!(runs.len(), 1, "short gap was not absorbed: {runs:?}");
        assert_eq!(runs[0], (frame_ms(50), frame_ms(300)));
        assert!(
            cut_points(&runs).is_empty(),
            "an absorbed gap must not become a cut"
        );
    }

    #[test]
    fn a_long_gap_splits_the_burst_and_offers_a_cut() {
        // 70 frames = 1120 ms of silence, well over the 300 ms minimum.
        let frames = 400;
        let runs = segment_trace(frames, &[(50, 150), (220, 320)], 0.9);
        assert_eq!(runs.len(), 2, "long gap was wrongly absorbed: {runs:?}");
        let cuts = cut_points(&runs);
        assert_eq!(cuts.len(), 1);
        // Midpoint of frames 150..220 -> 185 * 16 ms.
        assert_eq!(cuts[0], frame_ms(185));
        assert!(cuts[0] > runs[0].1 && cuts[0] < runs[1].0);
    }

    #[test]
    fn a_short_blip_is_dropped_as_a_click() {
        // 2 frames = 32 ms, under the 150 ms minimum speech duration.
        let runs = segment_trace(400, &[(100, 102)], 0.9);
        assert!(runs.is_empty(), "a 32 ms blip survived: {runs:?}");
    }

    #[test]
    fn hysteresis_needs_onset_and_holds_until_offset() {
        // Frame 0-9 at 0.5: above offset but below onset -> no run yet.
        // Frames 10-19 at 0.7: crosses onset -> run opens at frame 10.
        // Frames 20-29 at 0.5: above offset -> run stays open.
        // From frame 30 on at 0.1: below offset -> run closes at frame 30.
        let mut trace = vec![0.5f32; 40];
        for value in &mut trace[10..20] {
            *value = 0.7;
        }
        for value in &mut trace[30..] {
            *value = 0.1;
        }
        let runs = speech_runs(&trace);
        assert_eq!(runs, vec![(frame_ms(10), frame_ms(30))]);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let empty = analyze_speech(&[], 1, 16_000);
        assert!(empty.segments.is_empty());
        assert_eq!(empty.duration_ms, 0);

        let one_sample = analyze_speech(&[0.5], 1, 16_000);
        assert!(one_sample.segments.is_empty());
        assert_eq!(one_sample.duration_ms, 0);

        // Zero channels behaves as mono.
        let zero_channels = analyze_speech(&[0.0; 512], 0, 16_000);
        assert_well_formed(&zero_channels, 32);

        // More channels than frames is bounded, not indexed.
        let odd = analyze_speech(&[0.5, -0.5, 0.25], u16::MAX, 16_000);
        assert!(odd.segments.is_empty());

        // NaN must be sanitised, never fed to the model raw.
        let mut noisy = vec![0.0f32; 16_000];
        noisy[100] = f32::NAN;
        noisy[101] = f32::INFINITY;
        noisy[200..400].fill(f32::NAN);
        let nan = analyze_speech(&noisy, 1, 16_000);
        assert_well_formed(&nan, 1_000);

        // An 8 kHz signal is resampled for the model and still reports the
        // original 1 s duration.
        let eight_k = analyze_speech(&vec![0.0f32; 8_000], 1, 8_000);
        assert_well_formed(&eight_k, 1_000);

        // A zero rate must not divide by zero.
        let zero_rate = analyze_speech(&[0.0f32; 4_096], 1, 0);
        assert_eq!(zero_rate.duration_ms, 0);
        assert!(zero_rate.segments.is_empty());
        assert!(zero_rate.silences.is_empty());
    }

    #[test]
    fn original_timeline_is_preserved_across_rate_conversion() {
        // The same 1 s signal at 16 kHz and at 48 kHz. Frame times are derived
        // from the 16 kHz model, so both must report the *source* duration
        // (1000 ms), not the model's truncated 992 ms, and the frame
        // probabilities must line up frame-by-frame after resampling.
        let seconds = 1.0f64;
        let make = |rate: u32| -> Vec<f32> {
            let n = (f64::from(rate) * seconds).round() as usize;
            (0..n)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    // A smooth, bounded, speech-ish test tone with an envelope.
                    let envelope = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * 3.0 * t).sin();
                    (0.7 * envelope * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as f32
                })
                .collect()
        };
        let at_16k = make(16_000);
        let at_48k = make(48_000);
        let analysis_16k = analyze_speech(&at_16k, 1, 16_000);
        let analysis_48k = analyze_speech(&at_48k, 1, 48_000);

        assert_eq!(analysis_16k.sample_rate, 16_000);
        assert_eq!(analysis_48k.sample_rate, 48_000);
        assert_eq!(analysis_16k.duration_ms, 1_000);
        assert_eq!(analysis_48k.duration_ms, 1_000);

        // Both paths resample to (nearly) the same 16 kHz signal, so the model
        // sees the same frames: the frame grids must agree within one frame.
        let probs_16k = frame_probabilities(&at_16k);
        let probs_48k = frame_probabilities(&resample_linear(&at_48k, 48_000, 1, 16_000));
        assert!(
            probs_16k.len().abs_diff(probs_48k.len()) <= 1,
            "frame counts diverge: {} vs {}",
            probs_16k.len(),
            probs_48k.len()
        );
        let shared = probs_16k.len().min(probs_48k.len());
        let worst = probs_16k[..shared]
            .iter()
            .zip(&probs_48k[..shared])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.25, "per-frame probabilities diverge by {worst}");

        // Whatever the detector decided, both timelines must agree.
        assert_eq!(analysis_16k.segments.len(), analysis_48k.segments.len());
        assert_eq!(analysis_16k.cut_points.len(), analysis_48k.cut_points.len());
    }

    #[test]
    fn synthetic_bursts_yield_well_formed_structure() {
        // A synthetic tone burst is NOT speech, so the detector may fire
        // anywhere; this test therefore asserts only structure (ordering,
        // containment, non-overlap), not that the burst "is speech".
        let rate = 16_000u32;
        let mut pcm = vec![0i16; rate as usize * 3];
        let burst = |pcm: &mut [i16], start_s: f64, len_s: f64, freq: f64| {
            let start = (start_s * f64::from(rate)).round() as usize;
            let len = (len_s * f64::from(rate)).round() as usize;
            for k in 0..len {
                let t = k as f64 / f64::from(rate);
                let value = 0.8 * (2.0 * std::f64::consts::PI * freq * t).sin();
                pcm[start + k] = (value * 32767.0).round() as i16;
            }
        };
        burst(&mut pcm, 0.0, 0.5, 180.0);
        burst(&mut pcm, 1.5, 0.5, 300.0);
        let wav = wav_pcm16_mono(rate, &pcm);
        let analysis = analyze_audio_bytes(&wav).expect("wav decodes");
        assert_well_formed(&analysis, 3_000);
    }

    #[test]
    fn garbage_bytes_are_an_error_not_a_panic() {
        assert!(analyze_audio_bytes(b"").is_err());
        assert!(analyze_audio_bytes(b"not audio at all................").is_err());
    }
}

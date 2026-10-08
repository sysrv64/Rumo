// SPDX-License-Identifier: Apache-2.0

//! Beat/onset analysis of decoded audio.
//!
//! The app needs to place cuts and elements on the beat, so this module turns
//! interleaved PCM into a list of beat times with a loudness for each, plus a
//! tempo estimate. It is pure Rust arithmetic: no FFI, no new dependencies.
//!
//! # Provenance
//!
//! The *detection* pipeline is ported from the HyperFrames project
//! (`packages/core/src/beats/beatDetection.ts`): the framing constants
//! (`WINDOW_SIZE = 1024`, `HOP_SIZE = 512`), the half-wave rectified onset
//! envelope, the `0.25` onset threshold with a ±3 frame local maximum and a
//! `120 ms` minimum gap, and the ±50 ms RMS window used for beat strength all
//! come from there.
//!
//! The *tempo estimator* is ours, not HyperFrames'. HyperFrames gets its BPM
//! from the npm `bpm-detective` package, which is not available here (and is
//! not pure Rust); we autocorrelate the onset envelope instead. See
//! [`estimate_tempo`] for the details, including how confidence is scored.
//!
//! Everything is written to be panic-free: empty input, silence, a single
//! sample, a nonsense channel count and NaN samples all yield an empty analysis
//! rather than an index error.

use crate::audio::{AudioError, decode_audio_bytes};

/// Analysis window in samples. Ported from HyperFrames.
const WINDOW_SIZE: usize = 1024;
/// Window hop in samples (50 % overlap). Ported from HyperFrames.
const HOP_SIZE: usize = 512;
/// A frame is a candidate beat at this fraction of the envelope peak.
/// Ported from HyperFrames.
const ONSET_THRESHOLD: f32 = 0.25;
/// Candidate beats must be the maximum within this many frames either side.
/// Ported from HyperFrames (whose own picker uses a wider 20-frame local mean).
const LOCAL_WINDOW: usize = 3;
/// Minimum spacing between two beats, in milliseconds. Ported from HyperFrames.
const MIN_GAP_MS: i64 = 120;
/// Half-width of the RMS window used to score a beat's loudness, in seconds
/// (±50 ms). Ported from HyperFrames.
const STRENGTH_WINDOW_S: f32 = 0.05;

/// How sure the tempo estimate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatConfidence {
    High,
    Low,
    Uncertain,
}

impl BeatConfidence {
    /// The lowercase word used in the JSON answer to Kotlin.
    pub fn as_str(self) -> &'static str {
        match self {
            BeatConfidence::High => "high",
            BeatConfidence::Low => "low",
            BeatConfidence::Uncertain => "uncertain",
        }
    }
}

/// One detected beat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    /// Milliseconds into the track.
    pub time_ms: i64,
    /// Local loudness at the beat, 0..1, normalised by the track's peak.
    pub strength: f32,
}

/// The result of analysing one track.
#[derive(Debug, Clone, PartialEq)]
pub struct BeatAnalysis {
    /// Estimated tempo, or `None` when there were too few beats to tell.
    pub bpm: Option<f32>,
    pub confidence: BeatConfidence,
    pub sample_rate: u32,
    pub duration_ms: i64,
    pub beats: Vec<Beat>,
    /// Peak RMS over the whole track, the normaliser for `strength`.
    pub peak: f32,
}

impl BeatAnalysis {
    /// An analysis with no beats, the shape every failure path returns.
    fn empty(sample_rate: u32, duration_ms: i64) -> Self {
        Self {
            bpm: None,
            confidence: BeatConfidence::Uncertain,
            sample_rate,
            duration_ms,
            beats: Vec::new(),
            peak: 0.0,
        }
    }
}

/// Square of one sample, with non-finite values treated as zero so a single
/// NaN cannot poison every envelope downstream.
fn square(sample: f32) -> f64 {
    if sample.is_finite() {
        f64::from(sample) * f64::from(sample)
    } else {
        0.0
    }
}

/// Average the channels down to mono; a zero channel count is treated as mono.
///
/// Partial trailing frames (a byte stream that is not a whole number of frames)
/// are dropped rather than indexed past the end.
fn downmix(samples: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    if channels == 1 {
        return samples.to_vec();
    }
    let frames = samples.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let base = frame * channels;
        let sum: f32 = samples[base..base + channels].iter().sum();
        mono.push(sum / channels as f32);
    }
    mono
}

/// Number of whole analysis frames in `len` samples, matching HyperFrames'
/// `for (i = 0; i <= len - WINDOW_SIZE; i += HOP_SIZE)` loop.
fn frame_count(len: usize) -> usize {
    if len < WINDOW_SIZE {
        0
    } else {
        (len - WINDOW_SIZE) / HOP_SIZE + 1
    }
}

/// Start sample of frame `index`.
fn frame_start(index: usize) -> usize {
    index * HOP_SIZE
}

/// Time of frame `index` in whole milliseconds (`index * HOP_SIZE` samples).
///
/// Guards the float-to-int cast: a non-finite or negative time becomes 0 rather
/// than a saturating `as` conversion.
fn frame_time_ms(index: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    let time = (index as f64) * HOP_SIZE as f64 * 1000.0 / f64::from(sample_rate);
    if time.is_finite() && time > 0.0 {
        time.round() as i64
    } else {
        0
    }
}

/// RMS amplitude of each hop-aligned window (step 3: the energy envelope).
fn rms_envelope(mono: &[f32]) -> Vec<f32> {
    let frames = frame_count(mono.len());
    let mut envelope = Vec::with_capacity(frames);
    for index in 0..frames {
        let base = frame_start(index);
        let window = &mono[base..base + WINDOW_SIZE];
        let sum: f64 = window.iter().map(|s| square(*s)).sum();
        envelope.push((sum / WINDOW_SIZE as f64).sqrt() as f32);
    }
    envelope
}

/// Half-wave rectified difference between consecutive frames (step 4),
/// normalised by its own peak. A flat envelope stays all zeros, which is what
/// makes silence produce no beats.
fn onset_envelope(rms: &[f32]) -> Vec<f32> {
    let mut onset = vec![0.0f32; rms.len()];
    for index in 1..rms.len() {
        let delta = rms[index] - rms[index - 1];
        if delta.is_finite() && delta > 0.0 {
            onset[index] = delta;
        }
    }
    let peak = onset.iter().copied().fold(0.0f32, f32::max);
    if peak > 0.0 {
        for value in &mut onset {
            *value /= peak;
        }
    }
    onset
}

/// Peak picking (step 5): a frame is a beat when it clears
/// [`ONSET_THRESHOLD`] of the envelope peak, is the strict maximum within
/// ±[`LOCAL_WINDOW`] frames, and clears [`MIN_GAP_MS`] since the previous beat.
fn pick_beat_frames(onset: &[f32], sample_rate: u32) -> Vec<usize> {
    let peak = onset.iter().copied().fold(0.0f32, f32::max);
    // A flat envelope (silence, or a single-sample track) has no onsets: bail
    // before the `>=` comparison would accept every zero frame.
    if peak <= 0.0 || sample_rate == 0 || onset.is_empty() {
        return Vec::new();
    }
    let threshold = peak * ONSET_THRESHOLD;
    let last_index = onset.len() - 1;
    let mut beats = Vec::new();
    let mut last_time_ms: Option<i64> = None;
    for (index, &value) in onset.iter().enumerate() {
        if value < threshold {
            continue;
        }
        let low = index.saturating_sub(LOCAL_WINDOW);
        let high = (index + LOCAL_WINDOW).min(last_index);
        if onset[low..=high].iter().any(|neighbour| *neighbour > value) {
            continue;
        }
        let time_ms = frame_time_ms(index, sample_rate);
        if let Some(previous) = last_time_ms
            && time_ms - previous < MIN_GAP_MS
        {
            continue;
        }
        beats.push(index);
        last_time_ms = Some(time_ms);
    }
    beats
}

/// RMS over the ±[`STRENGTH_WINDOW_S`] window centred on `time_ms`, clamped to
/// the signal. Mirrors HyperFrames' `computeRmsAt`.
fn rms_at(mono: &[f32], sample_rate: u32, time_ms: i64) -> f32 {
    if mono.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let half = (f64::from(sample_rate) * f64::from(STRENGTH_WINDOW_S)) as usize;
    let centre = (time_ms.max(0) as f64) * f64::from(sample_rate) / 1000.0;
    if !centre.is_finite() {
        return 0.0;
    }
    let centre = centre as usize;
    let start = centre.saturating_sub(half);
    let end = centre.saturating_add(half).min(mono.len());
    if end <= start {
        return 0.0;
    }
    let sum: f64 = mono[start..end].iter().map(|s| square(*s)).sum();
    (sum / (end - start) as f64).sqrt() as f32
}

/// Greatest ±[`STRENGTH_WINDOW_S`] RMS anywhere in the signal (step 6's
/// normaliser). A sliding window keeps this O(n) time and O(1) memory instead
/// of scoring a prefix-sum table the size of the track.
fn peak_window_rms(mono: &[f32], sample_rate: u32) -> f32 {
    if mono.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let half = (f64::from(sample_rate) * f64::from(STRENGTH_WINDOW_S)) as usize;
    let len = mono.len();
    let mut start = 0usize;
    let mut end = half.min(len);
    let mut sum: f64 = mono[..end].iter().map(|s| square(*s)).sum();
    let mut best = if end > 0 { sum / end as f64 } else { 0.0 };
    for centre in 1..len {
        let new_start = centre.saturating_sub(half);
        let new_end = centre.saturating_add(half).min(len);
        while start < new_start {
            sum -= square(mono[start]);
            start += 1;
        }
        while end < new_end {
            sum += square(mono[end]);
            end += 1;
        }
        if end > start {
            // Guard the running sum: float cancellation could take it slightly
            // negative, and a negative mean square would poison the sqrt.
            let mean = if sum > 0.0 {
                sum / (end - start) as f64
            } else {
                0.0
            };
            if mean > best {
                best = mean;
            }
        }
    }
    best.max(0.0).sqrt() as f32
}

/// A beat's loudness: the local RMS over the track's peak RMS, clamped to 0..1
/// (step 6). Mirrors HyperFrames' `strengthAtTime`.
fn strength_at(mono: &[f32], sample_rate: u32, time_ms: i64, peak: f32) -> f32 {
    if !peak.is_finite() || peak <= 0.0 {
        return 0.0;
    }
    let rms = rms_at(mono, sample_rate, time_ms);
    if !rms.is_finite() {
        return 0.0;
    }
    (rms / peak).clamp(0.0, 1.0)
}

/// Normalised autocorrelation of `series` at `lag`:
/// `Σ x[i]x[i+lag] / sqrt(Σ x[i]² Σ x[i+lag]²)`.
///
/// Normalising (rather than summing raw products) stops the estimator from
/// preferring the shortest lag simply because it has the most overlapping
/// terms; ties are broken towards the shorter lag by the caller, which keeps
/// the estimate on the fundamental rather than double the period.
fn normalized_autocorr(series: &[f32], lag: usize) -> f64 {
    if lag == 0 || lag >= series.len() {
        return 0.0;
    }
    let left = &series[..series.len() - lag];
    let right = &series[lag..];
    let mut numerator = 0.0f64;
    let mut energy_left = 0.0f64;
    let mut energy_right = 0.0f64;
    for (a, b) in left.iter().zip(right.iter()) {
        let a = f64::from(*a);
        let b = f64::from(*b);
        numerator += a * b;
        energy_left += a * a;
        energy_right += b * b;
    }
    let denominator = (energy_left * energy_right).sqrt();
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

/// Tempo from the mean interval between consecutive beats whose spacing is
/// within ±25 % of `lag_ms`. `None` when no interval qualifies.
fn refine_from_intervals(lag_ms: f64, beat_frames: &[usize], sample_rate: u32) -> Option<f32> {
    if !lag_ms.is_finite() || lag_ms <= 0.0 {
        return None;
    }
    let tolerance = lag_ms * 0.25;
    let mut total = 0.0f64;
    let mut count = 0usize;
    for pair in beat_frames.windows(2) {
        let interval =
            (frame_time_ms(pair[1], sample_rate) - frame_time_ms(pair[0], sample_rate)) as f64;
        if (interval - lag_ms).abs() <= tolerance {
            total += interval;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    let mean = total / count as f64;
    if mean > 0.0 {
        Some((60_000.0 / mean) as f32)
    } else {
        None
    }
}

/// Tempo estimate and its confidence (step 7).
///
/// **This estimator is ours, not HyperFrames'.** HyperFrames takes its BPM from
/// the `bpm-detective` npm package, which is neither available nor pure Rust
/// here. We autocorrelate the onset envelope instead: correlate over the lags
/// that correspond to 60..200 BPM at the frame rate, take the best lag, then
/// refine it with the mean interval of the detected beats whose spacing is
/// within ±25 % of that lag (which removes the frame quantisation error when
/// the period is not a whole number of hops).
///
/// Confidence compares the winning correlation with the mean of the other lags
/// in range: `High` above 1.6×, `Low` above 1.15×, `Uncertain` below. Fewer
/// than four beats is always `None`/`Uncertain`.
fn estimate_tempo(
    onset: &[f32],
    beat_frames: &[usize],
    sample_rate: u32,
) -> (Option<f32>, BeatConfidence) {
    if beat_frames.len() < 4 || sample_rate == 0 {
        return (None, BeatConfidence::Uncertain);
    }

    let frame_rate = f64::from(sample_rate) / HOP_SIZE as f64;
    let min_lag = (frame_rate * 60.0 / 200.0).ceil().max(1.0) as usize;
    let max_lag = (frame_rate * 60.0 / 60.0).floor() as usize;

    let mut correlations: Vec<(usize, f64)> = Vec::new();
    for lag in min_lag..=max_lag.max(min_lag) {
        if lag < onset.len() {
            correlations.push((lag, normalized_autocorr(onset, lag)));
        }
    }

    // Too short to autocorrelate: fall back to the observed beat spacing.
    if correlations.is_empty() {
        let mean_interval = beat_frames
            .windows(2)
            .map(|pair| {
                (frame_time_ms(pair[1], sample_rate) - frame_time_ms(pair[0], sample_rate)) as f64
            })
            .sum::<f64>()
            / (beat_frames.len() - 1) as f64;
        let bpm = if mean_interval > 0.0 {
            Some((60_000.0 / mean_interval) as f32)
        } else {
            None
        };
        return (bpm, BeatConfidence::Uncertain);
    }

    // Strict `>` keeps the shortest lag on a tie, which is the fundamental
    // period rather than its double (half the tempo).
    let mut best = correlations[0];
    for &candidate in &correlations[1..] {
        if candidate.1 > best.1 {
            best = candidate;
        }
    }
    let (best_lag, best_score) = best;

    let others: Vec<f64> = correlations
        .iter()
        .filter(|(lag, _)| *lag != best_lag)
        .map(|(_, score)| *score)
        .collect();
    let mean_other = if others.is_empty() {
        0.0
    } else {
        others.iter().sum::<f64>() / others.len() as f64
    };
    let confidence = if best_score > 1.6 * mean_other {
        BeatConfidence::High
    } else if best_score > 1.15 * mean_other {
        BeatConfidence::Low
    } else {
        BeatConfidence::Uncertain
    };

    let lag_ms = best_lag as f64 * HOP_SIZE as f64 * 1000.0 / f64::from(sample_rate);
    let bpm = refine_from_intervals(lag_ms, beat_frames, sample_rate)
        .map(f64::from)
        .or_else(|| {
            if lag_ms > 0.0 {
                Some(60_000.0 / lag_ms)
            } else {
                None
            }
        })
        .map(|bpm| ((bpm * 10.0).round() / 10.0) as f32);

    (bpm, confidence)
}

/// Analyse interleaved samples (any channel count) at `sample_rate`.
///
/// Never panics: an empty signal, silence, a sub-window signal or a nonsense
/// channel count all return an [`BeatAnalysis::empty`]-shaped result.
pub fn analyze_beats(samples: &[f32], channels: u16, sample_rate: u32) -> BeatAnalysis {
    let mono = downmix(samples, channels);
    let duration_ms = if sample_rate == 0 {
        0
    } else {
        let ms = mono.len() as u128 * 1000 / u128::from(sample_rate);
        ms.min(i64::MAX as u128) as i64
    };
    let mut analysis = BeatAnalysis::empty(sample_rate, duration_ms);
    // Fewer than one window of samples: nothing to frame, so nothing to detect.
    if sample_rate == 0 || mono.len() < WINDOW_SIZE {
        return analysis;
    }

    let rms = rms_envelope(&mono);
    let onset = onset_envelope(&rms);
    let beat_frames = pick_beat_frames(&onset, sample_rate);

    let peak = peak_window_rms(&mono, sample_rate);
    analysis.peak = if peak.is_finite() { peak } else { 0.0 };
    analysis.beats = beat_frames
        .iter()
        .map(|&index| {
            let time_ms = frame_time_ms(index, sample_rate);
            Beat {
                time_ms,
                strength: strength_at(&mono, sample_rate, time_ms, analysis.peak),
            }
        })
        .collect();

    if beat_frames.len() >= 4 {
        let (bpm, confidence) = estimate_tempo(&onset, &beat_frames, sample_rate);
        analysis.bpm = bpm.filter(|bpm| bpm.is_finite());
        analysis.confidence = if analysis.bpm.is_some() {
            confidence
        } else {
            BeatConfidence::Uncertain
        };
    }
    analysis
}

/// Decode a container/codec in memory and analyse it.
pub fn analyze_audio_bytes(bytes: &[u8]) -> Result<BeatAnalysis, AudioError> {
    let decoded = decode_audio_bytes(bytes)?;
    Ok(analyze_beats(
        &decoded.samples,
        decoded.channels,
        decoded.sample_rate,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode_audio_bytes;

    const SAMPLE_RATE: u32 = 44_100;

    /// Wrap mono 16-bit PCM in a 44-byte RIFF/WAVE header.
    fn wav_pcm16_mono(sample_rate: u32, pcm: &[i16]) -> Vec<u8> {
        let channels: u16 = 1;
        let bits: u16 = 16;
        let data_len = (pcm.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
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

    /// A click track: exponentially decaying 1 kHz bursts of silence, one every
    /// `60/bpm` seconds, starting at 0.5 s. Returns the WAV bytes, the click
    /// times in milliseconds, and the click period in milliseconds.
    fn click_track(bpm: f64, seconds: f64) -> (Vec<u8>, Vec<i64>, i64) {
        let total = (f64::from(SAMPLE_RATE) * seconds).round() as usize;
        let mut pcm = vec![0i16; total];
        let period_s = 60.0 / bpm;
        let burst_len = (f64::from(SAMPLE_RATE) * 0.04).round() as usize; // 40 ms
        let tau = f64::from(SAMPLE_RATE) * 0.012; // 12 ms decay
        let frequency = 1000.0f64;
        let mut click_ms = Vec::new();
        let mut time = 0.5f64;
        while time + 0.1 < seconds {
            let start = (time * f64::from(SAMPLE_RATE)).round() as usize;
            for k in 0..burst_len {
                let index = start + k;
                if index >= total {
                    break;
                }
                let envelope = (-(k as f64) / tau).exp();
                let phase =
                    2.0 * std::f64::consts::PI * frequency * k as f64 / f64::from(SAMPLE_RATE);
                let value = 0.9 * envelope * phase.sin();
                pcm[index] = (value * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
            }
            click_ms.push((time * 1000.0).round() as i64);
            time += period_s;
        }
        (
            wav_pcm16_mono(SAMPLE_RATE, &pcm),
            click_ms,
            (period_s * 1000.0).round() as i64,
        )
    }

    /// Mean of `values`, or 0 for an empty slice.
    fn mean(values: &[f32]) -> f32 {
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f32>() / values.len() as f32
        }
    }

    fn decoded_mono(wav: &[u8]) -> (Vec<f32>, u32) {
        let decoded = decode_audio_bytes(wav).expect("generated wav must decode");
        (
            downmix(&decoded.samples, decoded.channels),
            decoded.sample_rate,
        )
    }

    #[test]
    fn click_track_120bpm() {
        let (wav, click_ms, _period) = click_track(120.0, 30.0);
        let analysis = analyze_audio_bytes(&wav).expect("wav decodes");
        let bpm = analysis.bpm.expect("120 BPM track must yield a tempo");
        assert!((bpm - 120.0).abs() < 2.0, "bpm {bpm}");
        assert!(
            analysis.beats.len() >= 50,
            "only {} beats detected",
            analysis.beats.len()
        );

        // Every beat must sit on a click, within ±30 ms.
        for beat in &analysis.beats {
            let nearest = click_ms
                .iter()
                .map(|click| (beat.time_ms - click).abs())
                .min()
                .expect("track has clicks");
            assert!(
                nearest <= 30,
                "beat at {} ms is {nearest} ms off",
                beat.time_ms
            );
        }

        // Beats must be louder than positions halfway between clicks: sound the
        // same number of midpoints (skipping any that would land on a click or
        // past the end) and compare the means.
        let (mono, sample_rate) = decoded_mono(&wav);
        let peak = peak_window_rms(&mono, sample_rate);
        let midpoint_strengths: Vec<f32> = click_ms
            .iter()
            .map(|click| click + (click_ms[1] - click_ms[0]) / 2)
            .filter(|time| *time < analysis.duration_ms - 100)
            .map(|time| strength_at(&mono, sample_rate, time, peak))
            .collect();
        let beat_strengths: Vec<f32> = analysis.beats.iter().map(|beat| beat.strength).collect();
        assert!(
            mean(&beat_strengths) > mean(&midpoint_strengths),
            "beat mean {} not above midpoint mean {}",
            mean(&beat_strengths),
            mean(&midpoint_strengths)
        );
    }

    #[test]
    fn click_track_90bpm() {
        let (wav, _click_ms, _period) = click_track(90.0, 30.0);
        let analysis = analyze_audio_bytes(&wav).expect("wav decodes");
        let bpm = analysis.bpm.expect("90 BPM track must yield a tempo");
        assert!((bpm - 90.0).abs() < 3.0, "bpm {bpm}");
    }

    #[test]
    fn five_second_excerpt_still_finds_the_tempo() {
        let (wav, _click_ms, _period) = click_track(120.0, 5.0);
        let analysis = analyze_audio_bytes(&wav).expect("wav decodes");
        let bpm = analysis.bpm.expect("5 s is enough for a tempo");
        assert!((bpm - 120.0).abs() < 3.0, "bpm {bpm}");
        assert!(analysis.beats.len() >= 6, "beats {}", analysis.beats.len());
    }

    #[test]
    fn silence_has_no_beats() {
        let analysis = analyze_beats(&vec![0.0f32; SAMPLE_RATE as usize * 2], 1, SAMPLE_RATE);
        assert!(analysis.beats.is_empty(), "silence produced beats");
        assert_eq!(analysis.bpm, None);
        assert_eq!(analysis.confidence, BeatConfidence::Uncertain);
    }

    #[test]
    fn empty_and_tiny_inputs_do_not_panic() {
        let empty = analyze_beats(&[], 2, SAMPLE_RATE);
        assert!(empty.beats.is_empty());
        assert_eq!(empty.bpm, None);

        let one_sample = analyze_beats(&[0.5], 1, SAMPLE_RATE);
        assert!(one_sample.beats.is_empty());

        // A nonsense channel count (more channels than frames) is bounded, not
        // indexed.
        let odd = analyze_beats(&[0.5, -0.5, 0.25], u16::MAX, SAMPLE_RATE);
        assert!(odd.beats.is_empty());

        // A zero sample rate must not divide by zero.
        let zero_rate = analyze_beats(&[0.0f32; 4096], 1, 0);
        assert!(zero_rate.beats.is_empty());
        assert_eq!(zero_rate.duration_ms, 0);
    }

    #[test]
    fn deterministic_noise_does_not_panic() {
        // Small LCG (Numerical Recipes constants); no new dependency.
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let samples: Vec<f32> = (0..8000).map(|_| next()).collect();
        let analysis = analyze_beats(&samples, 1, 8000);
        // Noise may or may not look periodic; it must only return sane values.
        if let Some(bpm) = analysis.bpm {
            assert!(bpm.is_finite() && bpm > 0.0, "bpm {bpm}");
        }
        for beat in &analysis.beats {
            assert!((0.0..=1.0).contains(&beat.strength));
        }
    }

    #[test]
    fn garbage_bytes_are_an_error_not_a_panic() {
        assert!(analyze_audio_bytes(b"").is_err());
        assert!(analyze_audio_bytes(b"not audio at all................").is_err());
    }
}

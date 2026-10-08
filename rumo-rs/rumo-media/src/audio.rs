// SPDX-License-Identifier: Apache-2.0

//! Audio decoding and playback.
//!
//! Decoding uses [`symphonia`] (pure Rust) to turn a container/codec into
//! interleaved `f32` PCM. Playback uses [`cpal`], which on Android drives
//! the low-latency AAudio/NDK backend (pure Rust FFI, no vendored C++).
//!
//! The player never blocks the calling thread: a dedicated control thread
//! owns the output stream and applies `play`/`pause`/`seek` commands sent
//! over a channel. Position is taken from the count of frames the device
//! has actually rendered, not from wall-clock time.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::resample::resample_linear;

/// Anything that can stop audio from being decoded or played.
#[derive(Debug)]
pub enum AudioError {
    /// The stream is not audio, or no decoder is registered for its codec.
    Unsupported(String),
    /// The container/codec could not be decoded.
    Decode(String),
    /// The stream decoded to zero frames.
    Empty,
    /// No output device, or the device rejected the stream configuration.
    Device(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::Unsupported(m) => write!(f, "unsupported audio: {m}"),
            AudioError::Decode(m) => write!(f, "audio decode failed: {m}"),
            AudioError::Empty => write!(f, "audio stream decoded to zero frames"),
            AudioError::Device(m) => write!(f, "audio output error: {m}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Decoded interleaved PCM plus its stream parameters.
///
/// `samples` holds `frames * channels` `f32` samples in frame-major
/// order: `[frame0_ch0, frame0_ch1, frame1_ch0, ...]`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    /// Interleaved `f32` PCM in `-1.0..=1.0`.
    pub samples: Vec<f32>,
    /// Source sample rate in Hz.
    pub sample_rate: u32,
    /// Source channel count.
    pub channels: u16,
}

impl DecodedAudio {
    /// Number of whole frames (`samples / channels`).
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    /// Duration in seconds, derived from the decoded frame count.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / f64::from(self.sample_rate)
        }
    }
}

/// Decode a whole media source into interleaved `f32` PCM.
fn decode_source(source: Box<dyn MediaSource>) -> Result<DecodedAudio, AudioError> {
    let stream = MediaSourceStream::new(source, Default::default());
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| AudioError::Decode(format!("probe: {e}")))?;

    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| AudioError::Unsupported("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .cloned()
        .ok_or_else(|| AudioError::Unsupported("no audio codec parameters".into()))?;

    let registered = symphonia::default::get_codecs()
        .get_audio_decoder(params.codec)
        .ok_or_else(|| {
            AudioError::Unsupported(format!("no decoder for codec {:?}", params.codec))
        })?;
    let mut decoder = (registered.factory)(&params, &AudioDecoderOptions::default())
        .map_err(|e| AudioError::Decode(format!("decoder init: {e}")))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut spec: Option<(u32, u16)> = None;

    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) => return Err(AudioError::Decode(format!("next_packet: {e}"))),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let source_spec = decoded.spec();
                let rate = source_spec.rate();
                let channels = source_spec.channels().count() as u16;
                if rate == 0 || channels == 0 {
                    continue;
                }
                spec.get_or_insert((rate, channels));
                decoded.copy_to_vec_interleaved::<f32>(&mut scratch);
                samples.extend_from_slice(&scratch);
            }
            // A single bad packet is not fatal; skip it and continue.
            Err(SymphoniaError::DecodeError(_))
            | Err(SymphoniaError::IoError(_))
            | Err(SymphoniaError::ResetRequired) => continue,
            Err(e) => return Err(AudioError::Decode(format!("decode: {e}"))),
        }
    }

    let (sample_rate, channels) = spec.ok_or(AudioError::Empty)?;
    if samples.is_empty() {
        return Err(AudioError::Empty);
    }
    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
    })
}

/// Decode an audio file at `path` (e.g. `/proc/self/fd/N`) to interleaved PCM.
pub fn decode_audio_file(path: &str) -> Result<DecodedAudio, AudioError> {
    let file =
        std::fs::File::open(path).map_err(|e| AudioError::Decode(format!("open {path}: {e}")))?;
    decode_source(Box::new(file))
}

/// Decode in-memory audio `bytes` to interleaved PCM.
pub fn decode_audio_bytes(bytes: &[u8]) -> Result<DecodedAudio, AudioError> {
    decode_source(Box::new(std::io::Cursor::new(bytes.to_vec())))
}

/// Map interleaved audio from `in_channels` to `out_channels`.
///
/// Equal counts copy. Mono is replicated to every output channel; a
/// multi-channel input is averaged down to mono; otherwise the first
/// `out_channels` are copied and any missing channel is silenced.
pub fn convert_channels(input: &[f32], in_channels: u16, out_channels: u16) -> Vec<f32> {
    let ic = in_channels as usize;
    let oc = out_channels as usize;
    if ic == 0 || oc == 0 || input.is_empty() {
        return Vec::new();
    }
    if ic == oc {
        return input.to_vec();
    }
    let frames = input.len() / ic;
    let mut out = Vec::with_capacity(frames * oc);
    if ic == 1 {
        for &s in &input[..frames] {
            for _ in 0..oc {
                out.push(s);
            }
        }
    } else if oc == 1 {
        for frame in 0..frames {
            let base = frame * ic;
            let sum: f32 = input[base..base + ic].iter().sum();
            out.push(sum / ic as f32);
        }
    } else {
        for frame in 0..frames {
            let base = frame * ic;
            for c in 0..oc {
                out.push(if c < ic { input[base + c] } else { 0.0 });
            }
        }
    }
    out
}

/// Playback state shared between the control thread, the cpal callback,
/// and reader methods on the handle.
///
/// `cursor_frames` counts output frames actually written to the device,
/// so `cursor_frames / out_rate` is the audio-clock position.
struct PlayerShared {
    buffer: Box<[f32]>,
    channels: u16,
    out_rate: u32,
    cursor_frames: AtomicU64,
    playing: AtomicBool,
    finished: AtomicBool,
}

impl PlayerShared {
    fn new(buffer: Vec<f32>, out_rate: u32, channels: u16) -> Self {
        Self {
            buffer: buffer.into_boxed_slice(),
            channels,
            out_rate,
            cursor_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }

    fn total_frames(&self) -> u64 {
        let ch = self.channels as usize;
        self.buffer.len().checked_div(ch).unwrap_or(0) as u64
    }

    fn play(&self) {
        self.finished.store(false, Ordering::Release);
        self.playing.store(true, Ordering::Release);
    }

    fn pause(&self) {
        self.playing.store(false, Ordering::Release);
    }

    fn seek_seconds(&self, seconds: f64) {
        let total = self.total_frames();
        let frame = if seconds.is_finite() && seconds > 0.0 {
            (seconds * f64::from(self.out_rate)).round()
        } else {
            0.0
        };
        let frame = if frame.is_finite() && frame > 0.0 {
            (frame as u64).min(total)
        } else {
            0
        };
        self.cursor_frames.store(frame, Ordering::Release);
        self.finished.store(frame >= total, Ordering::Release);
    }

    fn position_seconds(&self) -> f64 {
        if self.out_rate == 0 {
            return 0.0;
        }
        self.cursor_frames.load(Ordering::Acquire) as f64 / f64::from(self.out_rate)
    }

    fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Write up to `out.len() / channels` frames into `out` starting at
    /// the cursor. Returns the number of frames written. This is the
    /// exact body of the device callback; driving it in a loop is the
    /// mock clock used by the unit tests.
    fn render_into(&self, out: &mut [f32]) -> usize {
        let ch = self.channels as usize;
        if ch == 0 || out.is_empty() {
            return 0;
        }
        if !self.playing.load(Ordering::Acquire) {
            return 0;
        }
        let out_frames = out.len() / ch;
        if out_frames == 0 {
            return 0;
        }
        let cursor = self.cursor_frames.load(Ordering::Acquire);
        let total = self.total_frames();
        if cursor >= total {
            self.playing.store(false, Ordering::Release);
            self.finished.store(true, Ordering::Release);
            return 0;
        }
        let available = (total - cursor) as usize;
        let take = out_frames.min(available);
        let start = cursor as usize * ch;
        let end = start + take * ch;
        out[..take * ch].copy_from_slice(&self.buffer[start..end]);

        let new_cursor = cursor + take as u64;
        self.cursor_frames.store(new_cursor, Ordering::Release);
        if new_cursor >= total {
            self.playing.store(false, Ordering::Release);
            self.finished.store(true, Ordering::Release);
        }
        take
    }
}

/// Commands accepted by the control thread.
enum Command {
    Play,
    Pause,
    Seek(f64),
    Shutdown,
}

/// A handle to one playing (or paused) audio stream.
///
/// Dropping the handle stops playback and joins the control thread.
pub struct AudioPlayer {
    shared: Arc<PlayerShared>,
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
    duration_seconds: f64,
    out_rate: u32,
    out_channels: u16,
}

impl AudioPlayer {
    /// Decode-free constructor from interleaved PCM at `sample_rate`
    /// with `channels`. Returns once the output device is open; the
    /// stream starts paused, call [`AudioPlayer::play`].
    pub fn open_pcm(
        samples: Vec<f32>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, AudioError> {
        Self::open(DecodedAudio {
            samples,
            sample_rate,
            channels,
        })
    }

    /// Open the default output device and prepare `audio` for playback.
    ///
    /// The whole file is resampled/channel-mapped once at open time to
    /// the device's native rate and channel count. Playback starts
    /// paused.
    pub fn open(audio: DecodedAudio) -> Result<Self, AudioError> {
        if audio.sample_rate == 0 || audio.channels == 0 || audio.samples.is_empty() {
            return Err(AudioError::Empty);
        }
        let duration_seconds = audio.duration_seconds();

        let (ready_tx, ready_rx) = mpsc::channel::<Result<Arc<PlayerShared>, AudioError>>();
        let (commands, command_rx) = mpsc::channel::<Command>();

        let thread = thread::Builder::new()
            .name("rumo-audio".to_string())
            .spawn(move || match build_output(audio) {
                Ok((shared, stream)) => {
                    let _ = ready_tx.send(Ok(shared.clone()));
                    while let Ok(command) = command_rx.recv() {
                        match command {
                            Command::Play => shared.play(),
                            Command::Pause => shared.pause(),
                            Command::Seek(seconds) => shared.seek_seconds(seconds),
                            Command::Shutdown => break,
                        }
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| AudioError::Device(format!("spawn control thread: {e}")))?;

        let shared = match ready_rx.recv() {
            Ok(Ok(shared)) => shared,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(AudioError::Device("audio control thread died".into()));
            }
        };

        Ok(Self {
            out_rate: shared.out_rate,
            out_channels: shared.channels,
            shared,
            commands,
            thread: Some(thread),
            duration_seconds,
        })
    }

    /// Total duration of the track in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    /// Playback position in seconds, computed from frames rendered by
    /// the device (the audio clock), not from system time.
    pub fn position_seconds(&self) -> f64 {
        self.shared.position_seconds()
    }

    /// Device output sample rate chosen at open time.
    pub fn output_rate(&self) -> u32 {
        self.out_rate
    }

    /// Device output channel count chosen at open time.
    pub fn output_channels(&self) -> u16 {
        self.out_channels
    }

    /// True between [`AudioPlayer::play`] and [`AudioPlayer::pause`], and
    /// false after the track ends.
    pub fn is_playing(&self) -> bool {
        self.shared.is_playing()
    }

    /// True once the track has reached its end.
    pub fn is_finished(&self) -> bool {
        self.shared.is_finished()
    }

    /// Start or resume playback. Non-blocking.
    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }

    /// Pause playback; the position is retained. Non-blocking.
    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    /// Seek to `seconds` from the start. Values past the end clamp to
    /// the end; negative values clamp to zero. Non-blocking.
    pub fn seek(&self, seconds: f64) {
        let _ = self.commands.send(Command::Seek(seconds));
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Open the default output device and build the playing stream.
///
/// Runs on the control thread; the cpal [`Stream`](cpal::Stream) is
/// created and dropped on this thread, which some backends require.
fn build_output(audio: DecodedAudio) -> Result<(Arc<PlayerShared>, cpal::Stream), AudioError> {
    let DecodedAudio {
        samples,
        sample_rate,
        channels,
    } = audio;

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no default output device".into()))?;
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Device(format!("default_output_config: {e}")))?;

    let sample_format = supported.sample_format();
    let mut config: cpal::StreamConfig = supported.config();

    // Low latency: ask for the smallest fixed buffer the device reports,
    // clamped to a stable range; fall back to the backend default when
    // the range is unknown.
    if let cpal::SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
        let target = min.clamp(64, 2048).min(max.max(1)).max(1);
        config.buffer_size = cpal::BufferSize::Fixed(target);
    }

    let out_rate = config.sample_rate;
    let out_channels = config.channels;
    if out_rate == 0 || out_channels == 0 {
        return Err(AudioError::Device("device reported invalid config".into()));
    }

    let resampled = if sample_rate == out_rate {
        samples
    } else {
        resample_linear(&samples, sample_rate, channels, out_rate)
    };
    let output = if channels == out_channels {
        resampled
    } else {
        convert_channels(&resampled, channels, out_channels)
    };
    if output.is_empty() {
        return Err(AudioError::Empty);
    }

    let shared = Arc::new(PlayerShared::new(output, out_rate, out_channels));
    let stream = build_stream(&device, &config, sample_format, Arc::clone(&shared))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(format!("stream play: {e}")))?;
    Ok((shared, stream))
}

/// Build the output stream using the device's native sample format.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    shared: Arc<PlayerShared>,
) -> Result<cpal::Stream, AudioError> {
    match format {
        SampleFormat::I8 => build_typed::<i8>(device, config, shared),
        SampleFormat::I16 => build_typed::<i16>(device, config, shared),
        SampleFormat::I24 => build_typed::<cpal::I24>(device, config, shared),
        SampleFormat::I32 => build_typed::<i32>(device, config, shared),
        SampleFormat::I64 => build_typed::<i64>(device, config, shared),
        SampleFormat::U8 => build_typed::<u8>(device, config, shared),
        SampleFormat::U16 => build_typed::<u16>(device, config, shared),
        SampleFormat::U24 => build_typed::<cpal::U24>(device, config, shared),
        SampleFormat::U32 => build_typed::<u32>(device, config, shared),
        SampleFormat::U64 => build_typed::<u64>(device, config, shared),
        SampleFormat::F32 => build_typed::<f32>(device, config, shared),
        SampleFormat::F64 => build_typed::<f64>(device, config, shared),
        other => Err(AudioError::Device(format!(
            "unsupported sample format {other:?}"
        ))),
    }
}

/// Typed stream builder: converts the shared `f32` mix to `T` in the
/// callback. `scratch` is reused across callbacks to avoid allocating on
/// the audio thread after the first (few) callbacks.
fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<PlayerShared>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    let error_callback = |error: cpal::Error| eprintln!("rumo-audio stream error: {error}");

    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                let frames = data.len().checked_div(channels).unwrap_or(0);
                let needed = frames * channels;
                if scratch.len() < needed {
                    scratch.resize(needed, 0.0);
                }
                let written = shared.render_into(&mut scratch[..needed]);
                let count = written * channels;
                for (dst, src) in data[..count].iter_mut().zip(scratch[..count].iter()) {
                    *dst = T::from_sample(*src);
                }
            },
            error_callback,
            None,
        )
        .map_err(|e| AudioError::Device(format!("build_output_stream: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 44-byte PCM WAV header + samples (matches the probe test).
    fn make_wav(sample_rate: u32, channels: u16, frames: u32) -> Vec<u8> {
        let bits: u16 = 16;
        let data_len = frames * u32::from(channels) * u32::from(bits) / 8;
        let mut wav = Vec::with_capacity((44 + data_len) as usize);
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
        wav.extend(std::iter::repeat_n(0u8, data_len as usize));
        wav
    }

    #[test]
    fn decode_wav_reports_parameters() {
        let wav = make_wav(8000, 1, 8000);
        let audio = decode_audio_bytes(&wav).expect("wav must decode");
        assert_eq!(audio.sample_rate, 8000);
        assert_eq!(audio.channels, 1);
        assert_eq!(audio.frames(), 8000);
        assert!((audio.duration_seconds() - 1.0).abs() < 1e-9);
        assert_eq!(audio.samples.len(), 8000);
    }

    #[test]
    fn decode_wav_stereo_parameters() {
        let wav = make_wav(44100, 2, 4410);
        let audio = decode_audio_bytes(&wav).expect("wav must decode");
        assert_eq!(audio.sample_rate, 44100);
        assert_eq!(audio.channels, 2);
        assert_eq!(audio.frames(), 4410);
        assert_eq!(audio.samples.len(), 8820);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_audio_bytes(b"").is_err());
        assert!(decode_audio_bytes(&[0xABu8; 64]).is_err());
        assert!(decode_audio_bytes(b"not audio..............................").is_err());
    }

    #[test]
    fn decode_missing_file_is_error() {
        assert!(decode_audio_file("/nonexistent/rumo-audio-missing.wav").is_err());
    }

    #[test]
    fn convert_channels_mono_to_stereo() {
        let out = convert_channels(&[1.0, 2.0, 3.0], 1, 2);
        assert_eq!(out, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn convert_channels_stereo_to_mono_averages() {
        let out = convert_channels(&[1.0, 3.0, 2.0, 4.0], 2, 1);
        assert_eq!(out, vec![2.0, 3.0]);
    }

    #[test]
    fn convert_channels_same_is_copy() {
        let input = [0.1f32, 0.2, 0.3, 0.4];
        assert_eq!(convert_channels(&input, 2, 2), input.to_vec());
    }

    #[test]
    fn convert_channels_guards() {
        assert!(convert_channels(&[], 1, 2).is_empty());
        assert!(convert_channels(&[1.0], 0, 2).is_empty());
        assert!(convert_channels(&[1.0], 1, 0).is_empty());
    }

    /// Drive [`PlayerShared`] like the device callback: each `render`
    /// call advances the audio clock by exactly the frames it returns.
    #[test]
    fn player_state_machine_on_mock_clock() {
        // 6 mono frames at 1000 Hz.
        let shared = PlayerShared::new(vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5], 1000, 1);
        let mut out = [0f32; 4];

        // Paused: nothing renders, position stays at zero.
        assert_eq!(shared.render_into(&mut out), 0);
        assert_eq!(shared.position_seconds(), 0.0);
        assert!(!shared.is_playing());

        // Play: first callback yields 4 frames, audio clock = 4 ms.
        shared.play();
        assert!(shared.is_playing());
        assert_eq!(shared.render_into(&mut out), 4);
        assert_eq!(&out[..4], &[0.0, 0.1, 0.2, 0.3]);
        assert!((shared.position_seconds() - 0.004).abs() < 1e-9);

        // Pause: position freezes even if more callbacks arrive.
        shared.pause();
        assert!(!shared.is_playing());
        assert_eq!(shared.render_into(&mut out), 0);
        assert!((shared.position_seconds() - 0.004).abs() < 1e-9);

        // Seek backwards while paused, then resume.
        shared.seek_seconds(0.002);
        assert!((shared.position_seconds() - 0.002).abs() < 1e-9);
        shared.play();
        assert_eq!(shared.render_into(&mut out), 4);
        assert_eq!(&out[..4], &[0.2, 0.3, 0.4, 0.5]);
        assert!((shared.position_seconds() - 0.006).abs() < 1e-9);
        // Cursor reached the end: paused and finished.
        assert!(!shared.is_playing());
        assert!(shared.is_finished());
        assert_eq!(shared.render_into(&mut out), 0);
    }

    #[test]
    fn seek_clamps_to_bounds() {
        let shared = PlayerShared::new(vec![0.0; 100], 1000, 1);
        shared.seek_seconds(-5.0);
        assert_eq!(shared.position_seconds(), 0.0);
        shared.seek_seconds(1000.0);
        assert!((shared.position_seconds() - 0.1).abs() < 1e-9);
        assert!(shared.is_finished());
        shared.seek_seconds(f64::NAN);
        assert_eq!(shared.position_seconds(), 0.0);
        assert!(!shared.is_finished());
    }

    #[test]
    fn end_of_track_partial_fill() {
        // 3 frames, ask for 4: only 3 are written, then finished.
        let shared = PlayerShared::new(vec![1.0, 2.0, 3.0], 1000, 1);
        shared.play();
        let mut out = [0f32; 4];
        assert_eq!(shared.render_into(&mut out), 3);
        assert_eq!(&out[..3], &[1.0, 2.0, 3.0]);
        assert_eq!(out[3], 0.0, "tail left as silence");
        assert!(shared.is_finished());
        assert!(!shared.is_playing());
    }

    #[test]
    fn stereo_render_interleaves_frames() {
        let shared = PlayerShared::new(vec![1.0, -1.0, 2.0, -2.0], 48000, 2);
        shared.play();
        let mut out = [0f32; 4];
        assert_eq!(shared.render_into(&mut out), 2);
        assert_eq!(out, [1.0, -1.0, 2.0, -2.0]);
        assert!((shared.position_seconds() - 2.0 / 48_000.0).abs() < 1e-12);
    }

    /// Hardware smoke test: opening a real output device needs a sound
    /// server, so it is not part of the default test run.
    #[test]
    #[ignore = "requires an audio output device"]
    fn open_default_device_smoke() {
        let player = AudioPlayer::open_pcm(vec![0.0; 48_000], 48_000, 1).expect("open device");
        player.play();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(player.position_seconds() >= 0.0);
        player.pause();
    }
}

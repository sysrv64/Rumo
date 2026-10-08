// SPDX-License-Identifier: Apache-2.0

//! Android MediaCodec (H.264 + AAC) + MediaMuxer (MP4) backend.
//!
//! `ndk` 0.9 has no `media_muxer` wrapper, so `AMediaMuxer` is bound
//! directly through the raw `ndk-sys` FFI (`libmediandk.so`).
//!
//! Track ordering is the whole reason the audio path looks the way it does
//! (docs/11 §11.5): `AMediaMuxer` takes *every* track before `start`, the audio
//! track's format only exists after the AAC encoder's first output, and the
//! video format only after the first encoded frame. So audio is mixed and
//! encoded to AAC up front and the encoded samples are kept in memory, and the
//! muxer starts once both tracks — or video alone, when the project has no
//! sound — are known.

use crate::audio::{
    AUDIO_BITRATE, AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioMixer, AudioSource, MIME_AAC,
    frames_to_pts_us, new_mixer, to_i16_pcm,
};
use crate::backend::VideoBackend;
use crate::error::{ExportError, Result};
use crate::exporter::ExporterConfig;
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use ndk_sys as ffi;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

const MIME_AVC: &str = "video/avc";
/// MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar.
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible.
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F00_0789;
const DEQUEUE_TIMEOUT: Duration = Duration::from_millis(10);
const DRAIN_DEADLINE: Duration = Duration::from_secs(10);
/// Mix granularity: the contract's 4096-frame chunks (docs/11 §11.5).
const MIX_CHUNK_FRAMES: usize = 4096;

impl From<ndk::media_error::MediaError> for ExportError {
    fn from(e: ndk::media_error::MediaError) -> Self {
        ExportError::Codec(e.to_string())
    }
}

/// Owns the file descriptor + `AMediaMuxer`, copies encoded samples into an
/// MP4 container. `AMediaMuxer` does not take ownership of the fd, so the
/// [`File`] is kept here and dropped after the muxer is deleted.
///
/// Tracks are added lazily, as each encoder reveals its format, and the muxer
/// starts only once every expected track is in.
struct Muxer {
    ptr: NonNull<ffi::AMediaMuxer>,
    _file: File,
    video_track: Option<usize>,
    audio_track: Option<usize>,
    started: bool,
}

impl Muxer {
    fn open(path: &str) -> Result<Self> {
        let file = File::create(path).map_err(|e| ExportError::Io(e.to_string()))?;
        let ptr = unsafe {
            ffi::AMediaMuxer_new(
                file.as_raw_fd(),
                ffi::OutputFormat::AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4,
            )
        };
        let ptr =
            NonNull::new(ptr).ok_or_else(|| ExportError::Muxer("AMediaMuxer_new failed".into()))?;
        Ok(Self {
            ptr,
            _file: file,
            video_track: None,
            audio_track: None,
            started: false,
        })
    }

    fn add_track(&mut self, format: &MediaFormat) -> Result<usize> {
        let idx = unsafe { ffi::AMediaMuxer_addTrack(self.ptr.as_ptr(), format.as_ptr()) };
        if idx < 0 {
            return Err(ExportError::Muxer(format!("addTrack failed: {idx}")));
        }
        Ok(idx as usize)
    }

    fn add_video_track(&mut self, format: &MediaFormat) -> Result<usize> {
        let idx = self.add_track(format)?;
        self.video_track = Some(idx);
        Ok(idx)
    }

    /// Add the AAC track; may only happen before [`Muxer::start`].
    fn add_audio_track(&mut self, format: &MediaFormat) -> Result<usize> {
        let idx = self.add_track(format)?;
        self.audio_track = Some(idx);
        Ok(idx)
    }

    fn start(&mut self) -> Result<()> {
        let status = unsafe { ffi::AMediaMuxer_start(self.ptr.as_ptr()) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(ExportError::Muxer(format!("start failed: {}", status.0)));
        }
        self.started = true;
        Ok(())
    }

    fn write(
        &mut self,
        track: usize,
        data: &[u8],
        offset: i32,
        size: i32,
        pts_us: i64,
        flags: u32,
    ) -> Result<()> {
        let info = ffi::AMediaCodecBufferInfo {
            offset,
            size,
            presentationTimeUs: pts_us,
            flags,
        };
        let status = unsafe {
            ffi::AMediaMuxer_writeSampleData(self.ptr.as_ptr(), track, data.as_ptr(), &info)
        };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(ExportError::Muxer(format!(
                "writeSampleData failed: {}",
                status.0
            )));
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if !self.started {
            return Ok(());
        }
        let status = unsafe { ffi::AMediaMuxer_stop(self.ptr.as_ptr()) };
        self.started = false;
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(ExportError::Muxer(format!("stop failed: {}", status.0)));
        }
        Ok(())
    }
}

impl Drop for Muxer {
    fn drop(&mut self) {
        if self.started {
            unsafe {
                ffi::AMediaMuxer_stop(self.ptr.as_ptr());
            }
        }
        unsafe {
            ffi::AMediaMuxer_delete(self.ptr.as_ptr());
        }
    }
}

/// One encoded AAC frame waiting for the muxer to start.
struct EncodedSample {
    data: Vec<u8>,
    pts_us: i64,
    flags: u32,
}

/// AAC-LC encoder for the export's single audio track.
///
/// Only ever built once a source actually contributes frames, and dropped at
/// [`AndroidBackend::finish`]: the muxer cannot accept a track after
/// `start`, so the encoder runs to completion (including its end-of-stream
/// drain) while the container is still empty.
struct AacEncoder {
    codec: MediaCodec,
    /// Encoded frames in ascending presentation time.
    samples: Vec<EncodedSample>,
    /// The track format, from the `OutputFormatChanged` that precedes the first
    /// frame. Kept as owned: `MediaFormat::try_clone` needs API 29, which this
    /// crate does not require.
    output_format: Option<MediaFormat>,
    /// Encoded samples already copied into the muxer.
    written: usize,
    eos: bool,
}

impl AacEncoder {
    fn open() -> Result<Self> {
        let codec = MediaCodec::from_encoder_type(MIME_AAC)
            .ok_or_else(|| ExportError::Codec("no AAC encoder on this device".into()))?;
        let mut format = MediaFormat::new();
        format.set_str("mime", MIME_AAC);
        format.set_i32("sample-rate", AUDIO_SAMPLE_RATE as i32);
        format.set_i32("channel-count", i32::from(AUDIO_CHANNELS));
        format.set_i32("bitrate", AUDIO_BITRATE as i32);
        // The profile is left to the encoder: every Android AAC encoder
        // defaults to LC, and naming it here only gives a device that refuses
        // that profile a way to fail `configure`.
        codec.configure(&format, None, MediaCodecDirection::Encoder)?;
        codec.start()?;
        Ok(Self {
            codec,
            samples: Vec::new(),
            output_format: None,
            written: 0,
            eos: false,
        })
    }

    /// Encode interleaved 16-bit PCM starting at output frame `first_frame`.
    ///
    /// The mix arrives in chunks, but the encoder is fed one continuous stream:
    /// the timestamp of a sample is derived from its frame index, never
    /// accumulated, so the presentation times stay monotonic per track no matter
    /// how the mix was assembled.
    fn push(&mut self, pcm: &[i16], first_frame: usize) -> Result<()> {
        if self.eos {
            return Err(ExportError::Codec(
                "audio encoder already reached end of stream".into(),
            ));
        }
        let channels = AUDIO_CHANNELS as usize;
        let frame_bytes = channels * 2;
        let deadline = Instant::now() + DRAIN_DEADLINE;
        let mut offset_samples = 0usize;
        while offset_samples < pcm.len() {
            let mut input = match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedInputBufferResult::Buffer(input)) => input,
                Ok(DequeuedInputBufferResult::TryAgainLater) => {
                    if Instant::now() > deadline {
                        return Err(ExportError::Codec(
                            "audio dequeueInputBuffer timed out".into(),
                        ));
                    }
                    self.drain(false)?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let queued_samples = {
                let buffer = input.buffer_mut();
                // A buffer that cannot hold one frame would loop forever.
                let frames = buffer.len() / frame_bytes;
                if frames == 0 {
                    return Err(ExportError::Codec(format!(
                        "audio input buffer of {} bytes cannot hold one {channels}-channel frame",
                        buffer.len()
                    )));
                }
                let take_samples = (pcm.len() - offset_samples).min(frames * channels);
                let window = &pcm[offset_samples..offset_samples + take_samples];
                for (dst, src) in buffer.iter_mut().zip(window) {
                    dst.write(src.to_le_bytes()[0]);
                    dst.write(src.to_le_bytes()[1]);
                }
                take_samples
            };
            let frame_offset = first_frame + offset_samples / channels;
            let pts_us = frames_to_pts_us(frame_offset, AUDIO_SAMPLE_RATE);
            self.codec
                .queue_input_buffer(input, 0, queued_samples * 2, pts_us as u64, 0)?;
            offset_samples += queued_samples;
            self.drain(false)?;
        }
        Ok(())
    }

    /// Collect every encoded frame that is ready. With `until_eos` it keeps
    /// polling until the end-of-stream buffer arrives or the deadline expires.
    fn drain(&mut self, until_eos: bool) -> Result<()> {
        let deadline = Instant::now() + DRAIN_DEADLINE;
        loop {
            match self.codec.dequeue_output_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {
                    if !until_eos || Instant::now() > deadline {
                        return Ok(());
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    let format = self.codec.output_format();
                    self.output_format = Some(format);
                }
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    let info = *out.info();
                    let flags = info.flags();
                    // A codec-config buffer is the encoder's extradata, not a
                    // sample; the muxer gets what it needs from the format.
                    let codec_config = flags & ffi::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    if info.size() > 0 && !codec_config {
                        let offset = usize::try_from(info.offset()).map_err(|_| {
                            ExportError::Codec(format!("negative audio offset {}", info.offset()))
                        })?;
                        let buffer = out.buffer();
                        let end = offset
                            .saturating_add(info.size() as usize)
                            .min(buffer.len());
                        if end <= offset {
                            return Err(ExportError::Codec(format!(
                                "empty audio buffer slice {offset}..{end} of {}",
                                buffer.len()
                            )));
                        }
                        self.samples.push(EncodedSample {
                            data: buffer[offset..end].to_vec(),
                            pts_us: info.presentation_time_us(),
                            flags,
                        });
                    }
                    let eos = flags & ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                    self.codec.release_output_buffer(out, false)?;
                    if eos {
                        self.eos = true;
                        return Ok(());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Signal end-of-stream and drain the encoder's lookahead tail.
    ///
    /// AAC delays its output by a codec frame or two, so the last part of the
    /// mix only leaves the encoder here; skipping this step would cut the tail
    /// off every export.
    fn finish(&mut self) -> Result<()> {
        let deadline = Instant::now() + DRAIN_DEADLINE;
        loop {
            match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedInputBufferResult::Buffer(input)) => {
                    self.codec.queue_input_buffer(
                        input,
                        0,
                        0,
                        0,
                        ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                    )?;
                    break;
                }
                Ok(DequeuedInputBufferResult::TryAgainLater) => {
                    // Freeing an input buffer means output is waiting for it.
                    self.drain(false)?;
                    if Instant::now() > deadline {
                        return Err(ExportError::Codec(
                            "audio encoder took no end-of-stream input buffer".into(),
                        ));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.drain(true)?;
        self.codec.stop()?;
        Ok(())
    }
}

/// The export's audio state, created by the first source that contributes
/// frames. `None` means the project has no sound at all, and the muxer then
/// starts with the video track alone.
struct AudioState {
    mixer: AudioMixer,
    encoder: AacEncoder,
    /// Mix frames already handed to the encoder.
    submitted_frames: usize,
}

/// H.264 encoder + AAC encoder + MP4 muxer for the export pipeline.
pub struct AndroidBackend {
    codec: MediaCodec,
    muxer: Muxer,
    audio: Option<AudioState>,
}

impl AndroidBackend {
    /// Open the first encoder/color-format combination that configures.
    ///
    /// The NDK exposes no color-format capability query (`getCapabilities`
    /// lives on the Java `MediaCodecInfo`), so support is probed by
    /// attempting `configure` with an ordered candidate list:
    /// `COLOR_FormatYUV420SemiPlanar` first, then `COLOR_FormatYUV420Flexible`.
    fn open_encoder(config: &ExporterConfig) -> Result<MediaCodec> {
        let mut last_err = String::from("no usable H.264 encoder");
        for color_format in [
            COLOR_FORMAT_YUV420_SEMI_PLANAR,
            COLOR_FORMAT_YUV420_FLEXIBLE,
        ] {
            let Some(codec) = MediaCodec::from_encoder_type(MIME_AVC) else {
                break;
            };
            let mut format = MediaFormat::new();
            format.set_str("mime", MIME_AVC);
            format.set_i32("width", config.width as i32);
            format.set_i32("height", config.height as i32);
            format.set_i32("bitrate", config.bitrate as i32);
            format.set_i32("frame-rate", config.fps.round() as i32);
            format.set_f32("i-frame-interval", config.i_frame_interval_secs as f32);
            format.set_i32("color-format", color_format);
            match codec.configure(&format, None, MediaCodecDirection::Encoder) {
                Ok(()) => match codec.start() {
                    Ok(()) => return Ok(codec),
                    Err(e) => {
                        last_err = format!("start failed (color-format {color_format}): {e}");
                    }
                },
                Err(e) => {
                    last_err = format!("configure failed (color-format {color_format}): {e}");
                }
            }
        }
        Err(ExportError::Codec(last_err))
    }

    /// Queue one input buffer, waiting (bounded) for buffer availability.
    fn queue_input(&mut self, data: &[u8], pts_us: i64, flags: u32) -> Result<()> {
        let deadline = Instant::now() + DRAIN_DEADLINE;
        loop {
            match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedInputBufferResult::Buffer(mut input)) => {
                    {
                        let buf = input.buffer_mut();
                        if buf.len() < data.len() {
                            return Err(ExportError::Codec(format!(
                                "input buffer too small: {} < {}",
                                buf.len(),
                                data.len()
                            )));
                        }
                        for (dst, src) in buf.iter_mut().zip(data.iter()) {
                            dst.write(*src);
                        }
                    }
                    self.codec
                        .queue_input_buffer(input, 0, data.len(), pts_us as u64, flags)?;
                    return Ok(());
                }
                Ok(DequeuedInputBufferResult::TryAgainLater) => {
                    if Instant::now() > deadline {
                        return Err(ExportError::Codec("dequeueInputBuffer timed out".into()));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Add the AAC track as soon as the encoder has revealed its format.
    ///
    /// This happens while the muxer is still empty (before any video frame), so
    /// it cannot be left to the first video drain.
    fn add_audio_track(&mut self) -> Result<()> {
        if self.muxer.audio_track.is_some() {
            return Ok(());
        }
        let Some(format) = self
            .audio
            .as_mut()
            .and_then(|state| state.encoder.output_format.take())
        else {
            return Ok(());
        };
        let added = self.muxer.add_audio_track(&format);
        // The format belongs to the encoder, not to the caller of `add_track`.
        if let Some(state) = self.audio.as_mut() {
            state.encoder.output_format = Some(format);
        }
        added.map(|_| ())
    }

    /// Start the muxer once every expected track is in.
    ///
    /// "Expected" is read from the audio *state*, not from the source list: a
    /// project with no sound must still finalize, and a mix shorter than the AAC
    /// encoder's own delay reveals its format only at end-of-stream — which
    /// happens before the video drain gets here, so waiting is always safe.
    fn sync_muxer(&mut self) -> Result<()> {
        if self.muxer.started || self.muxer.video_track.is_none() {
            return Ok(());
        }
        if self.audio.is_some() && self.muxer.audio_track.is_none() {
            return Ok(());
        }
        self.muxer.start()
    }

    /// Copy every encoded AAC frame the muxer has not taken yet.
    fn flush_audio_to_muxer(&mut self) -> Result<()> {
        let track = match (self.muxer.started, self.muxer.audio_track) {
            (true, Some(track)) => track,
            _ => return Ok(()),
        };
        let Some(state) = self.audio.as_mut() else {
            return Ok(());
        };
        while state.encoder.written < state.encoder.samples.len() {
            let sample = &state.encoder.samples[state.encoder.written];
            self.muxer.write(
                track,
                &sample.data,
                0,
                sample.data.len() as i32,
                sample.pts_us,
                sample.flags,
            )?;
            state.encoder.written += 1;
        }
        Ok(())
    }

    /// Encode every mix frame the AAC encoder has not seen yet.
    ///
    /// Only the *encoded* frames are kept (docs/11 §11.5): a minute of AAC is
    /// tens of kilobytes, while a minute of the mixed `f32` PCM would be tens of
    /// megabytes and the whole point of encoding up front is to avoid it.
    fn encode_new_audio(&mut self) -> Result<()> {
        let Some(state) = self.audio.as_mut() else {
            return Ok(());
        };
        let from = state.submitted_frames;
        let total = state.mixer.total_frames();
        if from >= total {
            return Ok(());
        }
        let mut mixed = Vec::new();
        let mut cursor = from;
        while cursor < total {
            let frames = (total - cursor).min(MIX_CHUNK_FRAMES);
            mixed.extend_from_slice(&state.mixer.mix_range(cursor, frames));
            cursor += frames;
        }
        let pcm = to_i16_pcm(&mixed);
        state.submitted_frames = total;
        state.encoder.push(&pcm, from)?;
        self.add_audio_track()
    }

    /// Drain available video output: add the video track on format change,
    /// start the muxer once every expected track is in, and write the encoded
    /// samples — audio first, then the frame. With `until_eos` it keeps polling
    /// until the end-of-stream buffer arrives or the deadline expires.
    fn drain_output(&mut self, until_eos: bool) -> Result<()> {
        let deadline = Instant::now() + DRAIN_DEADLINE;
        loop {
            match self.codec.dequeue_output_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {
                    if !until_eos || Instant::now() > deadline {
                        return Ok(());
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    if self.muxer.video_track.is_none() {
                        let format = self.codec.output_format();
                        self.muxer.add_video_track(&format)?;
                        self.sync_muxer()?;
                        // The muxer may have just opened with audio waiting in
                        // memory; get it in before the video samples.
                        self.flush_audio_to_muxer()?;
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    let info = *out.info();
                    let flags = info.flags();
                    // `out` borrows the codec, so the muxer is reached through disjoint fields.
                    if info.size() > 0 && self.muxer.started {
                        if let Some(track) = self.muxer.video_track {
                            let data = out.buffer();
                            self.muxer.write(
                                track,
                                data,
                                info.offset(),
                                info.size(),
                                info.presentation_time_us(),
                                flags,
                            )?;
                        }
                    }
                    let eos = flags & ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                    self.codec.release_output_buffer(out, false)?;
                    if eos {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl VideoBackend for AndroidBackend {
    fn open(config: &ExporterConfig) -> Result<Self> {
        let codec = Self::open_encoder(config)?;
        let muxer = Muxer::open(&config.output_path)?;
        Ok(Self {
            codec,
            muxer,
            audio: None,
        })
    }

    fn add_audio_source(&mut self, source: AudioSource) -> Result<bool> {
        if self.audio.is_none() {
            // Mix into a throwaway mixer first: a source that yields no frames
            // must not open an AAC encoder and leave the muxer waiting for a
            // track that never appears.
            let mut probe = new_mixer()?;
            probe.add_source(source)?;
            if probe.total_frames() == 0 {
                return Ok(false);
            }
            self.audio = Some(AudioState {
                mixer: probe,
                encoder: AacEncoder::open()?,
                submitted_frames: 0,
            });
            self.encode_new_audio()?;
            return Ok(true);
        }
        let state = self
            .audio
            .as_mut()
            .expect("audio state exists on this path");
        let before = state.mixer.total_frames();
        state.mixer.add_source(source)?;
        if state.mixer.total_frames() == before {
            return Ok(false);
        }
        self.encode_new_audio()?;
        Ok(true)
    }

    fn queue_nv12(&mut self, nv12: &[u8], pts_us: i64) -> Result<()> {
        self.drain_output(false)?;
        self.queue_input(nv12, pts_us, 0)?;
        self.drain_output(false)?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        // Audio first: its tail only leaves the encoder on end-of-stream, and
        // the muxer is still open.
        if let Some(state) = self.audio.as_mut() {
            state.encoder.finish()?;
        }
        // Only when there is a track to write into: if the encoder never
        // reported a format there is nothing to add to the muxer, and the video
        // drain must not keep waiting for an audio track (see `sync_muxer`).
        if self.muxer.audio_track.is_some() {
            self.flush_audio_to_muxer()?;
        }
        self.audio = None;
        self.drain_output(false)?;
        self.queue_input(&[], 0, ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM)?;
        self.drain_output(true)?;
        self.codec.stop()?;
        self.muxer.stop()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Exporter;

    /// End-to-end encode of two frames. Needs a real Android device/emulator
    /// (MediaCodec encoder + libmediandk), so it is always ignored.
    #[test]
    #[ignore = "needs-device: Android MediaCodec/MediaMuxer required"]
    fn device_encode_two_frames_to_mp4() {
        let path = std::env::temp_dir().join("rumo_export_device_smoke.mp4");
        let _ = std::fs::remove_file(&path);
        let config =
            ExporterConfig::new(path.to_string_lossy().into_owned(), 64, 64, 30.0, 1_000_000);
        let mut exporter = Exporter::new(config).expect("open encoder");
        let rgba = vec![0x40u8; 64 * 64 * 4];
        exporter.write_frame_auto(&rgba).unwrap();
        exporter.write_frame_auto(&rgba).unwrap();
        exporter.finish().unwrap();

        let len = std::fs::metadata(&path).expect("mp4 written").len();
        assert!(len > 0, "mp4 must not be empty");
        let _ = std::fs::remove_file(&path);
    }

    /// 0.5 s of 16-bit mono WAV at 44.1 kHz, written to `path`.
    fn write_test_wav(path: &std::path::Path) {
        let (rate, channels, bits) = (44_100u32, 1u16, 16u16);
        let frames = rate as usize / 2;
        let data_len = frames * channels as usize * bits as usize / 8;
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let mut wav: Vec<u8> = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        // A quiet ramp, so the encoder sees signal rather than pure silence.
        for i in 0..data_len / 2 {
            wav.extend_from_slice(&((i as i32 % 8000 - 4000) as i16).to_le_bytes());
        }
        std::fs::write(path, wav).expect("write test wav");
    }

    /// End-to-end export of two frames plus one mixed audio source. Needs a real
    /// device/emulator: AAC encoder + MediaMuxer, so it is always ignored.
    #[test]
    #[ignore = "needs-device: Android MediaCodec/MediaMuxer required"]
    fn device_export_with_mixed_audio_track() {
        let wav = std::env::temp_dir().join("rumo_export_device_audio.wav");
        write_test_wav(&wav);
        let path = std::env::temp_dir().join("rumo_export_device_audio.mp4");
        let _ = std::fs::remove_file(&path);

        let audio = rumo_media::decode_audio_file(wav.to_str().expect("utf-8 path"))
            .expect("decode test wav");
        let mut exporter = Exporter::new(ExporterConfig::new(
            path.to_string_lossy().into_owned(),
            64,
            64,
            30.0,
            1_000_000,
        ))
        .expect("open encoder");

        // Two sources at different offsets and gains: the mix must land in one
        // AAC track, not two.
        let first = AudioSource::new(audio.clone(), 0, 0, 0.5);
        assert!(exporter.add_audio_source(first).expect("mix first"));
        let second = AudioSource::new(audio, 200, 0, 1.0);
        assert!(exporter.add_audio_source(second).expect("mix second"));

        let rgba = vec![0x40u8; 64 * 64 * 4];
        exporter.write_frame_auto(&rgba).unwrap();
        exporter.write_frame_auto(&rgba).unwrap();
        exporter.finish().unwrap();

        let len = std::fs::metadata(&path).expect("mp4 written").len();
        assert!(len > 0, "mp4 must not be empty");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&wav);
    }

    /// A source that mixes to nothing must not leave the muxer waiting for an
    /// audio track. Needs a device/emulator (MediaCodec/MediaMuxer).
    #[test]
    #[ignore = "needs-device: Android MediaCodec/MediaMuxer required"]
    fn device_export_without_audio_still_finalizes() {
        let path = std::env::temp_dir().join("rumo_export_device_silent.mp4");
        let _ = std::fs::remove_file(&path);
        let mut exporter = Exporter::new(ExporterConfig::new(
            path.to_string_lossy().into_owned(),
            64,
            64,
            30.0,
            1_000_000,
        ))
        .expect("open encoder");
        // Shorter than one 48 kHz output frame: nothing to mix.
        let empty = AudioSource {
            samples: vec![0.0; 2],
            sample_rate: 48_000,
            channels: 1,
            start_ms: 0,
            duration_ms: 0,
            gain: 1.0,
        };
        assert!(!exporter.add_audio_source(empty).expect("source skipped"));
        let rgba = vec![0x40u8; 64 * 64 * 4];
        exporter.write_frame_auto(&rgba).unwrap();
        exporter.finish().unwrap();
        assert!(
            std::fs::metadata(&path).expect("mp4 written").len() > 0,
            "a project without sound must still produce a playable file"
        );
        let _ = std::fs::remove_file(&path);
    }
}

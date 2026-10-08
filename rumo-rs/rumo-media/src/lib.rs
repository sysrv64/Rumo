// SPDX-License-Identifier: Apache-2.0

//! Minimal image inspection: PNG dimensions plus EXIF orientation.
//! Pure Rust: `image` (png via `miniz_oxide`) + `kamadak-exif`.
//!
//! Also hosts the audio pipeline: `symphonia` decode + `cpal` playback
//! (see [`audio`]), the mixer that sums several sources into the single track
//! the exporter encodes (see [`mixer`]), and the JNI entry points (see
//! [`audio_jni`]), plus the beat/onset analysis used to place cuts on the beat
//! (see [`beats`]) and the speech/silence segmentation used to cut at pauses
//! (see [`vad`]).
//!
//! And the video-clip pipeline: `MediaCodec`/`MediaExtractor` decode through
//! the NDK FFI (see [`video`]).

use std::io::Cursor;

pub mod audio;
pub mod audio_jni;
pub mod beats;
pub mod mixer;
pub mod resample;
pub mod vad;
pub mod video;
pub mod video_jni;

pub use audio::{
    AudioError, AudioPlayer, DecodedAudio, convert_channels, decode_audio_bytes, decode_audio_file,
};
pub use mixer::{AudioMixer, AudioSource, MixerError};
pub use resample::resample_linear;
pub use video::{
    VideoError, VideoFrame, VideoInfo, VideoSource, audio_source_fd, decode_audio_track_fd,
    open_video_fd, probe_video_fd,
};

/// Summary metadata extracted from an image file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// EXIF orientation tag (274), 1..=8. Defaults to 1 when the
    /// image carries no (parseable) EXIF block.
    pub orientation: u32,
}

/// Parse `bytes` as PNG and return its dimensions and EXIF orientation.
///
/// EXIF failures never fail the whole call: a missing or broken EXIF
/// segment yields `orientation == 1`. Returns `None` only when the
/// bytes are not a decodable PNG.
pub fn inspect_png(bytes: &[u8]) -> Option<ImageInfo> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let (width, height) = (img.width(), img.height());
    Some(ImageInfo {
        width,
        height,
        orientation: exif_orientation(bytes),
    })
}

/// Read EXIF orientation (tag 274) from `bytes`; `1` on any error,
/// on a missing tag, or on an out-of-range value.
fn exif_orientation(bytes: &[u8]) -> u32 {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return 1;
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return 1;
    };
    match field.value.get_uint(0) {
        Some(v @ 1..=8) => v,
        _ => 1,
    }
}

/// Decoded image: dimensions plus flat 8-bit RGBA pixels (`len == w*h*4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Pixel width after orientation + downscale.
    pub width: u32,
    /// Pixel height after orientation + downscale.
    pub height: u32,
    /// Row-major RGBA bytes.
    pub rgba: Vec<u8>,
}

/// Decode `bytes` (png/jpeg/webp/gif, auto-detected) to [`RgbaImage`].
///
/// Applies EXIF orientation 3/6/8 (180 / 90 CW / 270 CW) so the pixels
/// are already upright, then downscales with `thumbnail` when either
/// side exceeds `max_side`. Returns `None` on undecodable input or
/// `max_side == 0`.
pub fn decode_image_rgba(bytes: &[u8], max_side: u32) -> Option<RgbaImage> {
    if max_side == 0 {
        return None;
    }
    let decoded = image::load_from_memory(bytes).ok()?;
    let oriented = match exif_orientation(bytes) {
        3 => decoded.rotate180(),
        6 => decoded.rotate90(),
        8 => decoded.rotate270(),
        _ => decoded,
    };
    let fitted = if oriented.width() > max_side || oriented.height() > max_side {
        oriented.thumbnail(max_side, max_side)
    } else {
        oriented
    };
    let (width, height) = (fitted.width(), fitted.height());
    Some(RgbaImage {
        width,
        height,
        rgba: fitted.to_rgba8().into_raw(),
    })
}

/// Probe `source` with symphonia's default registry and return the
/// duration of the first track that carries both `time_base` and
/// `n_frames`, in whole milliseconds. Tracks without frame counts
/// (typical for streaming mp3) yield `None`, never 0-by-guess.
fn probe_media_source(source: Box<dyn symphonia::core::io::MediaSource>) -> Option<u64> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::formats::TrackType;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::units::Timestamp;

    let mss = MediaSourceStream::new(source, Default::default());
    let reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = reader
        .tracks()
        .iter()
        .find(|t| t.time_base.is_some() && (t.num_frames.is_some() || t.duration.is_some()))
        .or_else(|| reader.default_track(TrackType::Audio))?;
    let time_base = track.time_base?;
    let time = match (track.num_frames, track.duration) {
        (Some(n), _) => time_base.calc_time(Timestamp::new(n.min(i64::MAX as u64) as i64))?,
        (None, Some(d)) => time_base.calc_duration(d)?,
        (None, None) => return None,
    };
    u64::try_from(time.as_millis()).ok()
}

/// Probe an audio file at `path` (e.g. `/proc/self/fd/N`) and return
/// its duration in milliseconds, or `None` when unknown/undecodable.
pub fn probe_audio_file(path: &str) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    probe_media_source(Box::new(file))
}

/// Probe in-memory audio `bytes` and return the duration in
/// milliseconds, or `None` when unknown/undecodable.
pub fn probe_audio_bytes(bytes: &[u8]) -> Option<u64> {
    probe_media_source(Box::new(Cursor::new(bytes.to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ExtendedColorType;
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;

    /// Encode a solid `w`x`h` RGB PNG in memory (no fixture files).
    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let pixels = vec![0x7Fu8; (width * height * 3) as usize];
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(&pixels, width, height, ExtendedColorType::Rgb8)
            .expect("encode test png");
        buf
    }

    #[test]
    fn inspect_generated_png() {
        let png = make_png(4, 2);
        let info = inspect_png(&png).expect("generated png must decode");
        assert_eq!(info.width, 4);
        assert_eq!(info.height, 2);
        // No eXIf chunk in our output -> default orientation.
        assert_eq!(info.orientation, 1);
    }

    #[test]
    fn reject_garbage() {
        assert!(inspect_png(b"").is_none());
        assert!(inspect_png(&[0xABu8; 64]).is_none());
        assert!(inspect_png(b"not a png at all................................").is_none());
    }

    #[test]
    fn decode_generated_png_rgba() {
        let png = make_png(4, 2);
        let img = decode_image_rgba(&png, 1024).expect("generated png must decode");
        assert_eq!(img.width, 4);
        assert_eq!(img.height, 2);
        assert_eq!(img.rgba.len(), 4 * 2 * 4);
    }

    #[test]
    fn decode_downscales_long_side() {
        let png = make_png(8, 4);
        let img = decode_image_rgba(&png, 4).expect("must decode");
        assert!(img.width <= 4 && img.height <= 4, "{img:?}");
        assert_eq!(img.width, 4);
        assert_eq!(img.height, 2);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_image_rgba(b"", 512).is_none());
        assert!(decode_image_rgba(&[0xABu8; 64], 512).is_none());
        assert!(
            decode_image_rgba(b"not an image at all...........................", 512).is_none()
        );
        // Zero budget never decodes.
        let png = make_png(2, 2);
        assert!(decode_image_rgba(&png, 0).is_none());
    }

    /// Minimal 44-byte PCM WAV header + samples, 1 s 8000 Hz mono 16-bit.
    fn make_wav_1s_8k_mono16() -> Vec<u8> {
        let sample_rate: u32 = 8000;
        let channels: u16 = 1;
        let bits: u16 = 16;
        let n_samples: u32 = 8000;
        let data_len = n_samples * u32::from(channels) * u32::from(bits) / 8;
        let mut wav = Vec::with_capacity((44 + data_len) as usize);
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
        wav.extend(std::iter::repeat_n(0u8, data_len as usize));
        debug_assert_eq!(wav.len(), (44 + data_len) as usize);
        wav
    }

    #[test]
    fn probe_wav_bytes_duration() {
        let wav = make_wav_1s_8k_mono16();
        let ms = probe_audio_bytes(&wav).expect("wav must probe");
        assert!((950..=1050).contains(&ms), "got {ms}ms");
    }

    #[test]
    fn probe_rejects_garbage() {
        assert!(probe_audio_bytes(b"").is_none());
        assert!(probe_audio_bytes(&[0xABu8; 64]).is_none());
        assert!(probe_audio_bytes(b"not audio...................................").is_none());
        assert!(probe_audio_file("/nonexistent/rumo-probe-missing.wav").is_none());
    }
}

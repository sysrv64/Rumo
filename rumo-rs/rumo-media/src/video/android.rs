// SPDX-License-Identifier: Apache-2.0
// Android-only backend (see `#![cfg]` below): the host build never compiles
// this file, and the `ndk`/`ndk-sys` dependencies are target-gated in
// Cargo.toml, so `cargo test` on the host never links `libmediandk`.
#![cfg(target_os = "android")]

//! Android video backend: `AMediaExtractor` + `AMediaCodec` (docs/08 §8.4).
//!
//! `ndk` 0.9 wraps `AMediaCodec` but has no `AMediaExtractor` binding, so the
//! extractor is driven through raw `ndk-sys` FFI — the same pattern used for
//! `AMediaMuxer` in `rumo-export/src/android.rs`.
//!
//! # Two strategies
//!
//! [`DecodeStrategy::ByteBuffer`] (the default, unchanged) follows the
//! ByteBuffer route: samples come out of the extractor with their presentation
//! time and sync flag, are queued into the codec, and the codec hands back a
//! YUV420 buffer whose row `stride` / `slice-height` / `color-format` are read
//! from the *output* `MediaFormat` (signalled by `OutputFormatChanged`).
//! Conversion to upright RGBA8 is our pure Rust [`yuv`](super::yuv) code.
//!
//! [`DecodeStrategy::Surface`] (docs/12 §12.3) hands the codec an
//! `AImageReader`'s `ANativeWindow` instead. The decoder then renders into the
//! reader and the pixels are taken as three YUV420 planes — no conversion in
//! this process, so the next stage can upload them as they are. The extractor
//! and codec pumping is identical; only where the frames come from differs.
//!
//! The choice is explicit ([`DecodeStrategy`]), never inferred. If the surface
//! route cannot be opened the caller gets the byte-buffer route *and* the
//! reason in [`DecodeSession::ByteBufferFallback`], because a silent fallback
//! is how "the GPU path is on" turns out to have been untrue for a whole
//! session.
//!
//! The frame cache, the "decode forward until the presentation time covers the
//! request" policy and the end clamps live in
//! [`DecodingSource`](super::DecodingSource), which is platform-neutral; this
//! module only implements the [`FrameDecoder`](super::FrameDecoder) half.

use super::yuv::{self, YuvFormat, YuvLayout, YuvPlanes};
use super::{
    DecodingSource, FrameDecoder, VideoError, VideoFrame, VideoInfo, VideoSource, fd_is_seekable,
    filled_rgba, init_rgba, resolve_data_source_length, uninit_rgba,
};
use crate::audio::DecodedAudio;
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use ndk::native_window::NativeWindow;
use ndk_sys as ffi;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// Per-`dequeue_*` timeout; the pumps are bounded by [`PUMP_DEADLINE`].
const DEQUEUE_TIMEOUT: Duration = Duration::from_millis(10);
/// Upper bound for the work spent on one frame before reporting a stall.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Planar` (I420).
const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar` (NV12).
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible`.
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F00_0789;

/// `MediaFormat.COLOR_STANDARD_BT709`.
const COLOR_STANDARD_BT709: i32 = 1;
/// `MediaFormat.COLOR_STANDARD_BT601_PAL`.
const COLOR_STANDARD_BT601_PAL: i32 = 2;
/// `MediaFormat.COLOR_STANDARD_BT601_NTSC`.
const COLOR_STANDARD_BT601_NTSC: i32 = 4;
/// `MediaFormat.COLOR_STANDARD_BT2020`.
const COLOR_STANDARD_BT2020: i32 = 6;

/// `MediaFormat.COLOR_RANGE_FULL`.
const COLOR_RANGE_FULL: i32 = 1;
/// `MediaFormat.COLOR_RANGE_LIMITED`.
const COLOR_RANGE_LIMITED: i32 = 2;

impl From<ndk::media_error::MediaError> for VideoError {
    fn from(e: ndk::media_error::MediaError) -> Self {
        VideoError::Decode(e.to_string())
    }
}

/// Owns an `AMediaExtractor`.
///
/// `AMediaExtractor_setDataSourceFd` duplicates the descriptor, so the caller
/// keeps ownership of `fd` and this type must not close it.
struct Extractor {
    ptr: NonNull<ffi::AMediaExtractor>,
}

impl Extractor {
    fn open(fd: RawFd, offset: i64, length: i64) -> Result<Self, VideoError> {
        // `length` is a byte count and `0` would be an empty file, but that is
        // refused once at the boundary (`video_jni::check_length`), where it also
        // runs on the host.
        // A seekable descriptor, checked before the platform sees it.
        //
        // `AMediaExtractor_setDataSourceFd` needs one and answers
        // `AMEDIA_ERROR_UNKNOWN` (-10000) when it does not get one, which is the
        // single least informative status the API has: it names neither the
        // descriptor nor the seekability, so the failure arrives looking like a
        // corrupt container. A `content://` descriptor from MediaStore is not
        // guaranteed to be seekable — some providers hand back a pipe — and that
        // is exactly the case that must be named here rather than guessed at.
        //
        // The position is restored, because the caller owns the descriptor and
        // expects to read it from where it was.
        let length = Self::prepare(fd, offset, length)?;
        let ptr = NonNull::new(unsafe { ffi::AMediaExtractor_new() })
            .ok_or_else(|| VideoError::Open("AMediaExtractor_new failed".into()))?;
        let extractor = Self { ptr };
        let status = unsafe {
            ffi::AMediaExtractor_setDataSourceFd(
                extractor.ptr.as_ptr(),
                fd,
                offset as ffi::off64_t,
                length as ffi::off64_t,
            )
        };
        if status != ffi::media_status_t::AMEDIA_OK {
            // `extractor` is dropped here, which deletes it.
            return Err(VideoError::Open(format!(
                "setDataSourceFd failed: {} (the descriptor was seekable, so this is \
                 the container or the codec, not the descriptor)",
                status.0
            )));
        }
        Ok(extractor)
    }

    /// The seekable check plus the byte count the extractor will be given.
    ///
    /// Both halves are the same preparation: `FileSource` needs a descriptor it
    /// can seek **and** a positive length, and it converts a negative length into
    /// zero rather than into "the rest of the file" — see
    /// [`super::resolve_data_source_length`] for why that turns into
    /// `AMEDIA_ERROR_UNKNOWN` on a perfectly good clip. So the length is measured
    /// here, from the descriptor, and the position is put back afterwards because
    /// the caller still owns it.
    fn prepare(fd: RawFd, offset: i64, length: i64) -> Result<i64, VideoError> {
        if !fd_is_seekable(fd) {
            return Err(VideoError::Open(
                "the descriptor cannot seek, and AMediaExtractor needs a seekable one; a \
                 content:// provider that returns a pipe is the usual cause — open the file \
                 by path instead"
                    .into(),
            ));
        }
        let start = offset.clamp(0, i64::MAX);
        // The size is read the same way Android's own path-based FileSource reads
        // it — seek to the end — rather than through `fstat`, which needs a
        // struct layout this binding does not promise.
        let end = unsafe { ffi::lseek(fd, 0, ffi::SEEK_END as i32) };
        let restore = unsafe { ffi::lseek(fd, start, ffi::SEEK_SET as i32) };
        if end < 0 || restore < 0 {
            return Err(VideoError::Open(
                "the descriptor could not be measured, so its length is unknown".into(),
            ));
        }
        resolve_data_source_length(length, start, end)
            .map_err(VideoError::Open)
    }

    fn track_count(&self) -> usize {
        unsafe { ffi::AMediaExtractor_getTrackCount(self.ptr.as_ptr()) }
    }

    fn track_format(&self, idx: usize) -> Result<MediaFormat, VideoError> {
        let raw = unsafe { ffi::AMediaExtractor_getTrackFormat(self.ptr.as_ptr(), idx) };
        let ptr = NonNull::new(raw)
            .ok_or_else(|| VideoError::Open(format!("getTrackFormat({idx}) failed")))?;
        // Takes ownership: MediaFormat::drop calls AMediaFormat_delete.
        Ok(unsafe { MediaFormat::from_ptr(ptr) })
    }

    fn select_track(&self, idx: usize) -> Result<(), VideoError> {
        let status = unsafe { ffi::AMediaExtractor_selectTrack(self.ptr.as_ptr(), idx) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Open(format!(
                "selectTrack({idx}) failed: {}",
                status.0
            )));
        }
        Ok(())
    }

    /// Copy the current sample into `[ptr, ptr + capacity)`; negative on end of
    /// stream. The caller guarantees `capacity` writable bytes behind `ptr`.
    fn read_sample(&self, ptr: *mut u8, capacity: usize) -> isize {
        unsafe { ffi::AMediaExtractor_readSampleData(self.ptr.as_ptr(), ptr, capacity) }
    }

    fn sample_time_us(&self) -> i64 {
        unsafe { ffi::AMediaExtractor_getSampleTime(self.ptr.as_ptr()) }
    }

    fn sample_flags(&self) -> u32 {
        unsafe { ffi::AMediaExtractor_getSampleFlags(self.ptr.as_ptr()) }
    }

    fn advance(&self) -> bool {
        unsafe { ffi::AMediaExtractor_advance(self.ptr.as_ptr()) }
    }

    fn seek(&self, time_us: i64, mode: ffi::SeekMode) -> Result<(), VideoError> {
        let status = unsafe { ffi::AMediaExtractor_seekTo(self.ptr.as_ptr(), time_us, mode) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Seek(format!(
                "seekTo({time_us}us) failed: {}",
                status.0
            )));
        }
        Ok(())
    }
}

impl Drop for Extractor {
    fn drop(&mut self) {
        unsafe {
            ffi::AMediaExtractor_delete(self.ptr.as_ptr());
        }
    }
}

/// Selected video track: mime, decoder configuration and probed metadata.
struct TrackInfo {
    mime: String,
    /// Track `MediaFormat`, kept to configure the decoder.
    format: MediaFormat,
    info: VideoInfo,
}

/// Maps a `color-format` code onto a plane layout.
///
/// Flexible output (the default for software and hardware decoders on a plain
/// ByteBuffer) is treated as NV12: a full-resolution Y plane followed by
/// interleaved UV at the same row stride. That is what the platform produces in
/// practice; if a device ever reports something else, [`FrameLayout`] fails the
/// length checks in [`yuv::yuv_to_rgba`] instead of returning garbage.
fn yuv_format(code: i32) -> Option<YuvFormat> {
    match code {
        COLOR_FORMAT_YUV420_PLANAR => Some(YuvFormat::I420),
        COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_FLEXIBLE => Some(YuvFormat::Nv12),
        _ => None,
    }
}

fn positive_i32(fmt: &MediaFormat, key: &str) -> Option<i32> {
    fmt.i32(key).filter(|v| *v > 0)
}

fn positive_usize(fmt: &MediaFormat, key: &str) -> Option<usize> {
    usize::try_from(positive_i32(fmt, key)?).ok()
}

fn positive_u32(fmt: &MediaFormat, key: &str) -> Option<u32> {
    u32::try_from(positive_i32(fmt, key)?).ok()
}

fn matrix_of(fmt: &MediaFormat, width: u32, height: u32) -> yuv::Matrix {
    match fmt.i32("color-standard") {
        Some(COLOR_STANDARD_BT709) => yuv::Matrix::Bt709,
        Some(COLOR_STANDARD_BT601_PAL | COLOR_STANDARD_BT601_NTSC) => yuv::Matrix::Bt601,
        Some(COLOR_STANDARD_BT2020) => yuv::Matrix::Bt2020,
        // Heuristic when the container does not say: SD-era sizes are usually
        // BT.601, larger ones BT.709.
        _ => {
            if height > 576 || width > 1024 {
                yuv::Matrix::Bt709
            } else {
                yuv::Matrix::Bt601
            }
        }
    }
}

fn range_of(fmt: &MediaFormat) -> yuv::Range {
    match fmt.i32("color-range") {
        Some(COLOR_RANGE_FULL) => yuv::Range::Full,
        Some(COLOR_RANGE_LIMITED) => yuv::Range::Limited,
        _ => yuv::Range::Limited,
    }
}

/// `rotation` from `AMEDIAFORMAT_KEY_ROTATION`, snapped to the quarter turn
/// that the conversion can apply (Android only reports multiples of 90°).
fn normalize_rotation(deg: i32) -> u16 {
    match deg.rem_euclid(360) {
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    }
}

/// Crop rectangle of the decoded frame, when the platform reports one.
///
/// `MediaFormat::rect` is gated behind ndk 0.9's `api-level-28` feature, which
/// this crate does not enable, so the raw binding is used instead. The rect is
/// `(left, top, right, bottom)` with exclusive right/bottom; only the cases a
/// decoder actually produces are accepted.
fn crop_size(fmt: &MediaFormat) -> Option<(u32, u32)> {
    let key = c"crop";
    let (mut left, mut top, mut right, mut bottom) = (0i32, 0i32, 0i32, 0i32);
    let ok = unsafe {
        ffi::AMediaFormat_getRect(
            fmt.as_ptr(),
            key.as_ptr(),
            &mut left,
            &mut top,
            &mut right,
            &mut bottom,
        )
    };
    if !ok {
        return None;
    }
    let width = u32::try_from(right.checked_sub(left)?).ok()?;
    let height = u32::try_from(bottom.checked_sub(top)?).ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Plane geometry and colour parameters needed for one conversion.
///
/// `width`/`height` are the *visible* dimensions before rotation; the decoder's
/// [`FrameLayout::output_size`] is what [`VideoInfo`] reports.
#[derive(Debug, Clone, Copy)]
struct FrameLayout {
    format: YuvFormat,
    matrix: yuv::Matrix,
    range: yuv::Range,
    stride: usize,
    slice_height: usize,
    width: u32,
    height: u32,
    rotation_deg: u16,
}

impl FrameLayout {
    /// Best-effort layout straight from a `MediaFormat`. Padding fields are
    /// optional, so they default to the visible size and are corrected by
    /// [`AndroidDecoder::refresh_layout`] once the codec signals its output
    /// format.
    fn from_format(fmt: &MediaFormat, rotation_deg: u16) -> Option<Self> {
        let width = positive_u32(fmt, "width")?;
        let height = positive_u32(fmt, "height")?;
        let mut layout = Self {
            format: fmt
                .i32("color-format")
                .and_then(yuv_format)
                .unwrap_or(YuvFormat::Nv12),
            matrix: matrix_of(fmt, width, height),
            range: range_of(fmt),
            stride: positive_usize(fmt, "stride").unwrap_or(width as usize),
            slice_height: positive_usize(fmt, "slice-height").unwrap_or(height as usize),
            width,
            height,
            rotation_deg,
        };
        layout.apply_crop(fmt);
        Some(layout)
    }

    /// Adopt the `crop` rect when the decoder reports a valid one.
    ///
    /// Only the crop *size* is used: the visible window is always sampled from
    /// the plane origin, so a decoder that reports a non-zero `left`/`top`
    /// (alignment padding, not just padding on the right/bottom) yields a
    /// window shifted by that origin. Supporting it means threading the origin
    /// through [`crate::video::yuv::yuv_to_rgba`] and widening its bounds
    /// checks (`stride >= origin_x + width`, and the chroma column count) —
    /// deliberately not done here rather than editing a bounds-checked hot loop
    /// without a device to test on. See `docs/08`, 8.9.
    fn apply_crop(&mut self, fmt: &MediaFormat) {
        if let Some((width, height)) = crop_size(fmt)
            && width <= self.width
            && height <= self.height
        {
            self.width = width;
            self.height = height;
        }
    }

    /// Visible size after rotation — what the frame buffer holds.
    fn output_size(&self) -> (u32, u32) {
        if self.rotation_deg == 90 || self.rotation_deg == 270 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    fn output_len(&self) -> usize {
        let (width, height) = self.output_size();
        (width as usize) * (height as usize) * 4
    }
}

/// Which way a clip is decoded.
///
/// This is a decision the caller makes, not one the backend makes: the two
/// routes produce different things (upright RGBA8 versus raw YUV planes) and
/// only the caller knows which one the renderer downstream can take.
/// Which route opens a clip.
///
/// `ByteBuffer` is what every clip uses today; the surface route is built and
/// tested but **not selected**, deliberately. The byte path already hands over
/// planes (docs/12 §12.3), which is where the CPU conversion used to be, so the
/// surface route's remaining win is one copy of the decoded buffer — and that is
/// not worth switching a decode path nobody has watched on a device yet. It gets
/// selected when a clip is proven not to decode through the byte path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
pub(crate) enum DecodeStrategy {
    /// `MediaCodec` ByteBuffer output, converted to RGBA8 by [`yuv`]. The
    /// default; see the module docs.
    ByteBuffer,
    /// `MediaCodec` renders into an `AImageReader`; frames stay YUV420
    /// planes (docs/12 §12.3).
    Surface,
}

/// An opened clip, on whichever route it actually opened.
///
/// [`DecodeSession::ByteBufferFallback`] exists so that "the surface route was
/// refused and we quietly ran the old one" is a value the caller has to look
/// at, rather than something that happens behind its back.
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
pub(crate) enum DecodeSession {
    /// Opened on the byte-buffer route: [`VideoSource`] with RGBA8 frames.
    ByteBuffer(Box<dyn VideoSource>),
    /// Opened on the surface route: YUV planes, no conversion in this process.
    Surface(SurfaceDecoder),
    /// The surface route was asked for and could not be opened; this is the
    /// byte-buffer source that replaced it, plus the reason.
    ByteBufferFallback {
        source: Box<dyn VideoSource>,
        reason: String,
    },
}

#[allow(dead_code)] // see `DecodeStrategy`
impl DecodeSession {
    /// Metadata of the clip, whichever route opened.
    pub(crate) fn info(&self) -> VideoInfo {
        match self {
            Self::ByteBuffer(source) => source.info(),
            Self::ByteBufferFallback { source, .. } => source.info(),
            Self::Surface(decoder) => decoder.info,
        }
    }

    /// The reason the surface route was refused, when it was.
    ///
    /// `None` on a surface session and on a plain byte-buffer session; `Some`
    /// only when a caller *asked* for [`DecodeStrategy::Surface`] and got the
    /// other route instead. This is the value to surface in a log or a
    /// diagnostic — the fallback must be reported, not silent (docs/12 §12.5).
    pub(crate) fn surface_fallback_reason(&self) -> Option<&str> {
        match self {
            Self::ByteBufferFallback { reason, .. } => Some(reason),
            Self::ByteBuffer(_) | Self::Surface(_) => None,
        }
    }
}

/// `AImageReader` queue depth for the surface route.
///
/// A reader with `max_images` slots *stalls the codec* once that many frames
/// are queued and un-acquired, so this is a backpressure knob, not a buffer
/// pool. `2` is the smallest value that lets a decoded frame sit in the queue
/// while the previous one is still being uploaded, which is exactly the
/// pipeline we want: one frame in flight to the GPU, one waiting. Larger would
/// only add latency for a preview (the frames we would hold are stale by the
/// time they are shown); `1` would serialise decode against upload and halve
/// throughput. The stale-frame problem is handled by the *acquire* policy in
/// [`SurfaceDecoder::acquire`], not by queue depth.
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
const SURFACE_MAX_IMAGES: i32 = 2;

/// A borrowed plane of one decoded frame, exactly as the reader produced it.
///
/// This is what makes the surface route worth having: no copy and no colour
/// conversion happened to get here. The caller uploads from these bytes. Note
/// that a plane is *not* guaranteed to be tightly packed — use `row_stride`
/// and `pixel_stride`, never `width`, to walk it.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
pub(crate) struct PlaneView<'a> {
    /// The whole plane allocation, including padding.
    pub data: &'a [u8],
    /// Distance in bytes between the start of two consecutive rows.
    pub row_stride: usize,
    /// Distance in bytes between two consecutive pixels in one row. `1` for
    /// luma and for interleaved chroma; `2` for separate U and V planes.
    pub pixel_stride: usize,
}

/// One frame from the surface route: three YUV420 planes and their geometry.
///
/// The frame **keeps the `AImage` alive** — the plane pointers are only valid
/// while it is held, which is why this is a guard type rather than a slice
/// handed out on its own. Dropping it releases the buffer back to the reader,
/// which is what unblocks the codec (see [`SURFACE_MAX_IMAGES`]).
///
/// Deliberately *not* converted: rotation stays a property of the clip
/// ([`VideoInfo::rotation_deg`]) and the pixels stay in the decoder's coded
/// orientation, so the consumer turns them with a quad transform rather than
/// with a CPU pass. See [`SurfaceDecoder`] for the details.
#[derive(Debug)]
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
pub(crate) struct SurfaceFrame {
    image: AcquiredImage,
    width: u32,
    height: u32,
    /// Visible window inside the planes, as `AImage_getCropRect` reported it.
    crop: (u32, u32, u32, u32),
    time_ms: i64,
    rotation_deg: u16,
    matrix: yuv::Matrix,
    range: yuv::Range,
}

#[allow(dead_code)] // see `DecodeStrategy`
impl SurfaceFrame {
    /// Visible width in pixels (crop applied, rotation **not** applied).
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    /// Visible height in pixels (crop applied, rotation **not** applied).
    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// Crop window as `(left, top, width, height)` in plane pixels.
    ///
    /// Unlike the byte-buffer route, the origin is part of the answer here: the
    /// reader gives us the real crop rect, and a uploader can honour a
    /// non-zero `left`/`top` without the CPU rearrangement the byte route needs.
    pub(crate) fn crop(&self) -> (u32, u32, u32, u32) {
        self.crop
    }

    /// Presentation time of this frame, in milliseconds.
    pub(crate) fn time_ms(&self) -> i64 {
        self.time_ms
    }

    /// Clockwise rotation the consumer must still apply, in degrees.
    pub(crate) fn rotation_deg(&self) -> u16 {
        self.rotation_deg
    }

    /// Luma→RGB matrix the container declares (or the size heuristic).
    pub(crate) fn matrix(&self) -> yuv::Matrix {
        self.matrix
    }

    /// Luma sample range the container declares.
    pub(crate) fn range(&self) -> yuv::Range {
        self.range
    }

    /// Plane `index` (`0` = Y, `1` = Cb, `2` = Cr), or `None` when the image
    /// does not have that plane.
    ///
    /// Every accessor re-validates: the image is only trusted as far as it was
    /// checked when the frame was built, and an `AImage` is foreign memory.
    pub(crate) fn plane(&self, index: usize) -> Option<PlaneView<'_>> {
        self.image.plane(index)
    }
}

/// Number of planes in `YUV_420_888` (Y, Cb, Cr).
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
const YUV420_PLANES: usize = 3;

/// Owns an `AImageReader`.
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
struct SurfaceReader {
    ptr: NonNull<ffi::AImageReader>,
}

#[allow(dead_code)] // see `DecodeStrategy`
impl SurfaceReader {
    /// Open a reader for `width` x `height` `YUV_420_888` frames.
    ///
    /// `AImageReader_newWithUsage` rather than `AImageReader_new` because the
    /// usage flags have to be right for a decoder to be allowed to render into
    /// the window: `CPU_READ_OFTEN` is what makes the planes mappable at all,
    /// and `GPU_SAMPLED_IMAGE` is declared so that a consumer which would
    /// rather sample the hardware buffer directly is not silently handed a
    /// buffer it cannot use. It is also the flag combination the platform
    /// accepts most widely for `YUV_420_888`.
    fn open(width: u32, height: u32) -> Result<Self, VideoError> {
        let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) else {
            return Err(VideoError::Open(format!(
                "coded frame {width}x{height} does not fit the image reader"
            )));
        };
        // OR'd on the inner integers on purpose: `AHardwareBuffer_UsageFlags`
        // is a plain newtype with no `BitOr` impl, so `A | B` does not
        // compile. Widened to `u64` because `c_ulong` is 32-bit on the
        // 32-bit ABIs and the FFI takes a 64-bit flag word.
        let usage = ffi::AHardwareBuffer_UsageFlags::AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN.0
            as u64
            | ffi::AHardwareBuffer_UsageFlags::AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE.0 as u64;
        // Every out-parameter below is seeded with null rather than left
        // uninitialised: a failing `AImageReader_*` call is not required to
        // write it, and `assume_init` on untouched memory is undefined
        // behaviour, not just a garbage pointer.
        let mut ptr = MaybeUninit::new(std::ptr::null_mut());
        let status = unsafe {
            ffi::AImageReader_newWithUsage(
                width,
                height,
                ffi::AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32,
                usage,
                SURFACE_MAX_IMAGES,
                ptr.as_mut_ptr(),
            )
        };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Open(imgreader_error(
                "AImageReader_newWithUsage",
                status,
            )));
        }
        let ptr = NonNull::new(unsafe { ptr.assume_init() }).ok_or_else(|| {
            VideoError::Open("AImageReader_newWithUsage returned no reader".into())
        })?;
        Ok(Self { ptr })
    }

    /// The reader's producer window, with a reference of our own.
    ///
    /// `AImageReader_getWindow` hands back the reader's own window without
    /// transferring ownership, so the pointer is *cloned* (an
    /// `ANativeWindow_acquire`) and released again on drop. The clone has to
    /// outlive `MediaCodec::configure`, which is why the caller keeps it.
    fn window(&self) -> Result<NativeWindow, VideoError> {
        let mut ptr = MaybeUninit::new(std::ptr::null_mut());
        let status = unsafe { ffi::AImageReader_getWindow(self.ptr.as_ptr(), ptr.as_mut_ptr()) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Open(imgreader_error(
                "AImageReader_getWindow",
                status,
            )));
        }
        let ptr = NonNull::new(unsafe { ptr.assume_init() })
            .ok_or_else(|| VideoError::Open("AImageReader_getWindow returned no window".into()))?;
        // SAFETY: `AImageReader_getWindow` succeeded and returned a live
        // `ANativeWindow` that the reader owns; cloning only takes an extra
        // reference, and `NativeWindow` releases exactly that one on drop.
        Ok(unsafe { NativeWindow::clone_from_ptr(ptr) })
    }

    /// Take the newest frame, discarding any older ones still queued.
    ///
    /// Returns `Ok(None)` when the reader has nothing yet *or* when every slot
    /// is already acquired — both mean "not now", neither is an error.
    fn acquire_latest(&self) -> Result<Option<AcquiredImage>, VideoError> {
        let mut ptr = MaybeUninit::new(std::ptr::null_mut());
        let status =
            unsafe { ffi::AImageReader_acquireLatestImage(self.ptr.as_ptr(), ptr.as_mut_ptr()) };
        // "Not right now" rather than "broken": an empty queue, and a full set
        // of acquired slots, both just mean the caller must come back later.
        if status == ffi::media_status_t::AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE
            || status == ffi::media_status_t::AMEDIA_IMGREADER_MAX_IMAGES_ACQUIRED
        {
            return Ok(None);
        }
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Decode(imgreader_error(
                "AImageReader_acquireLatestImage",
                status,
            )));
        }
        let ptr = NonNull::new(unsafe { ptr.assume_init() }).ok_or_else(|| {
            VideoError::Decode("AImageReader_acquireLatestImage returned no image".into())
        })?;
        Ok(Some(AcquiredImage { ptr }))
    }
}

impl Drop for SurfaceReader {
    fn drop(&mut self) {
        unsafe { ffi::AImageReader_delete(self.ptr.as_ptr()) };
    }
}

/// Owns one `AImage`.
///
/// Dropping it is the whole backpressure mechanism: `AImage_delete` returns the
/// buffer to the reader's queue, and until that happens the reader cannot
/// accept more frames, so a caller that holds frames stalls the decoder. One
/// frame in flight is the intended steady state.
///
/// Every accessor below follows the same three rules, which is why the `unsafe`
/// blocks are not each annotated:
///
/// * the out-parameter is *seeded* (null, zero) before the call, because a
///   failing `AImage_*` call need not write it and reading untouched memory is
///   undefined behaviour rather than merely a garbage value;
/// * the returned status is checked **before** the value is read;
/// * `self.ptr` is a live `AImage` for the whole call, because the `SurfaceFrame`
///   that owns it cannot be dropped while a borrow of it exists — which is the
///   reason this type is a guard and not a bare pointer.
#[derive(Debug)]
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
struct AcquiredImage {
    ptr: NonNull<ffi::AImage>,
}

#[allow(dead_code)] // see `DecodeStrategy`
impl AcquiredImage {
    /// Frame width as the reader reports it, or an error.
    fn size(&self) -> Result<(u32, u32), VideoError> {
        let mut width = 0i32;
        let mut height = 0i32;
        let status = unsafe { ffi::AImage_getWidth(self.ptr.as_ptr(), &mut width) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Decode(
                imgreader_error("AImage_getWidth", status).to_string(),
            ));
        }
        let status = unsafe { ffi::AImage_getHeight(self.ptr.as_ptr(), &mut height) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Decode(
                imgreader_error("AImage_getHeight", status).to_string(),
            ));
        }
        let width = u32::try_from(width).ok().filter(|w| *w > 0);
        let height = u32::try_from(height).ok().filter(|h| *h > 0);
        match (width, height) {
            (Some(width), Some(height)) => Ok((width, height)),
            _ => Err(VideoError::Decode(format!(
                "the reader produced a {:?}x{:?} frame, which is not a usable size",
                width, height
            ))),
        }
    }

    /// Presentation timestamp in nanoseconds.
    ///
    /// This is the codec's own pts, which arrives on the surface in
    /// nanoseconds; it replaces the output buffer's
    /// `presentation_time_us` because a surface frame and the buffer that
    /// produced it are not the same object any more.
    fn timestamp_ms(&self) -> i64 {
        let mut nanos = 0i64;
        let status = unsafe { ffi::AImage_getTimestamp(self.ptr.as_ptr(), &mut nanos) };
        if status != ffi::media_status_t::AMEDIA_OK || nanos <= 0 {
            return 0;
        }
        nanos.div_euclid(1_000_000)
    }

    /// Number of planes, or an error. `YUV_420_888` must be three.
    fn plane_count(&self) -> Result<usize, VideoError> {
        let mut count = 0i32;
        let status = unsafe { ffi::AImage_getNumberOfPlanes(self.ptr.as_ptr(), &mut count) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Err(VideoError::Decode(
                imgreader_error("AImage_getNumberOfPlanes", status).to_string(),
            ));
        }
        usize::try_from(count)
            .ok()
            .filter(|count| *count == YUV420_PLANES)
            .ok_or_else(|| {
                VideoError::Decode(format!(
                    "a YUV_420_888 image reported {count} planes, not {YUV420_PLANES}"
                ))
            })
    }

    /// The visible crop window.
    ///
    /// An absent crop (the platform declines to answer, or answers with an
    /// empty rect) means "the whole frame", which is the safe reading. A crop
    /// that is *present but nonsensical* — negative, inverted, or reaching past
    /// the frame — is refused rather than clamped: it means the producer
    /// disagrees with the buffer we were handed, and quietly sampling the
    /// wrong window is the kind of wrong that never shows up as a crash.
    fn crop_rect(&self, width: u32, height: u32) -> Result<(u32, u32, u32, u32), VideoError> {
        // Seeded, not uninitialised: a failing call may leave it untouched.
        let mut rect = MaybeUninit::new(ffi::AImageCropRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        });
        let status = unsafe { ffi::AImage_getCropRect(self.ptr.as_ptr(), rect.as_mut_ptr()) };
        if status != ffi::media_status_t::AMEDIA_OK {
            return Ok((0, 0, width, height));
        }
        let rect = unsafe { rect.assume_init() };
        if rect.left == 0 && rect.top == 0 && rect.right == 0 && rect.bottom == 0 {
            return Ok((0, 0, width, height));
        }
        let invalid = |what: &str| {
            VideoError::Decode(format!(
                "the reader reported a crop rect {what} ({},{},{},{}) for a {width}x{height} frame",
                rect.left, rect.top, rect.right, rect.bottom
            ))
        };
        let (Ok(left), Ok(top), Ok(right), Ok(bottom)) = (
            u32::try_from(rect.left),
            u32::try_from(rect.top),
            u32::try_from(rect.right),
            u32::try_from(rect.bottom),
        ) else {
            return Err(invalid("with a negative origin"));
        };
        let (Some(crop_w), Some(crop_h)) = (right.checked_sub(left), bottom.checked_sub(top))
        else {
            return Err(invalid("that is inverted"));
        };
        if crop_w == 0 || crop_h == 0 {
            return Err(invalid("that is empty"));
        }
        if left.saturating_add(crop_w) > width || top.saturating_add(crop_h) > height {
            return Err(invalid("that reaches past the frame"));
        }
        Ok((left, top, crop_w, crop_h))
    }

    /// Plane `index` with its strides, or `None` when out of range.
    ///
    /// The length comes from `AImage_getPlaneData`, so the slice cannot run
    /// past the allocation; the strides are validated to be positive so a
    /// consumer walking the plane cannot walk it backwards.
    fn plane(&self, index: usize) -> Option<PlaneView<'_>> {
        let index = i32::try_from(index).ok()?;
        let mut row_stride = 0i32;
        if unsafe { ffi::AImage_getPlaneRowStride(self.ptr.as_ptr(), index, &mut row_stride) }
            != ffi::media_status_t::AMEDIA_OK
        {
            return None;
        }
        let mut pixel_stride = 0i32;
        if unsafe { ffi::AImage_getPlanePixelStride(self.ptr.as_ptr(), index, &mut pixel_stride) }
            != ffi::media_status_t::AMEDIA_OK
        {
            return None;
        }
        let (Ok(row_stride), Ok(pixel_stride)) =
            (usize::try_from(row_stride), usize::try_from(pixel_stride))
        else {
            return None;
        };
        if row_stride == 0 || pixel_stride == 0 {
            return None;
        }
        let mut data = MaybeUninit::new(std::ptr::null_mut());
        let mut len = 0i32;
        let status = unsafe {
            ffi::AImage_getPlaneData(self.ptr.as_ptr(), index, data.as_mut_ptr(), &mut len)
        };
        if status != ffi::media_status_t::AMEDIA_OK {
            return None;
        }
        let data = NonNull::new(unsafe { data.assume_init() });
        let (Some(data), Ok(len)) = (data, usize::try_from(len)) else {
            return None;
        };
        if len == 0 {
            return None;
        }
        // SAFETY: `data`/`len` come from `AImage_getPlaneData` for this live
        // `AImage`, which owns the allocation for as long as `self` is held,
        // and `len` is non-zero here.
        Some(PlaneView {
            data: unsafe { std::slice::from_raw_parts(data.as_ptr(), len) },
            row_stride,
            pixel_stride,
        })
    }
}

impl Drop for AcquiredImage {
    fn drop(&mut self) {
        unsafe { ffi::AImage_delete(self.ptr.as_ptr()) };
    }
}

/// Turn a `media_status_t` into a message naming the call that produced it.
///
/// `ndk::media_error::MediaError::from_status` is `pub(crate)`, so the raw
/// statuses this file receives cannot go through it; the mapping is spelled
/// out instead.
fn imgreader_error(call: &str, status: ffi::media_status_t) -> String {
    if status == ffi::media_status_t::AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE {
        return format!("{call}: the reader has no buffer available (not an error here)");
    }
    if status == ffi::media_status_t::AMEDIA_IMGREADER_MAX_IMAGES_ACQUIRED {
        return format!("{call}: every reader slot is acquired, so the codec is stalled");
    }
    format!("{call} failed with media status {}", status.0)
}

struct AndroidDecoder {
    extractor: Extractor,
    codec: MediaCodec,
    layout: FrameLayout,
    info: VideoInfo,
    /// Set after a seek (and at open): the next sample must be a sync sample,
    /// otherwise the extractor did not honour `SEEK_PREVIOUS_SYNC` and the
    /// decoder would be fed the middle of a GOP.
    expect_sync: bool,
    /// The end-of-stream marker has been queued into the codec.
    eos_queued: bool,
    /// The codec returned its end-of-stream output buffer.
    output_eos: bool,
}

impl AndroidDecoder {
    fn start(extractor: Extractor, track: TrackInfo) -> Result<Self, VideoError> {
        let codec = MediaCodec::from_decoder_type(&track.mime).ok_or_else(|| {
            VideoError::Open(format!("no MediaCodec decoder for {}", track.mime))
        })?;
        codec
            .configure(&track.format, None, MediaCodecDirection::Decoder)
            .map_err(|e| VideoError::Open(format!("configure({}) failed: {e}", track.mime)))?;
        codec
            .start()
            .map_err(|e| VideoError::Open(format!("codec start failed: {e}")))?;

        let layout = FrameLayout::from_format(&track.format, track.info.rotation_deg)
            .ok_or(VideoError::Unsupported("no colour layout in the track format"))?;
        Ok(Self {
            extractor,
            codec,
            layout,
            info: track.info,
            expect_sync: true,
            eos_queued: false,
            output_eos: false,
        })
    }

    /// Pull `stride` / `slice-height` / colour parameters from the codec's
    /// output format, signalled by `OutputFormatChanged` before the first frame.
    fn refresh_layout(&mut self) -> Result<(), VideoError> {
        let fmt = self.codec.output_format();
        if let Some(width) = positive_u32(&fmt, "width") {
            self.layout.width = width;
        }
        if let Some(height) = positive_u32(&fmt, "height") {
            self.layout.height = height;
        }
        if let Some(format) = fmt.i32("color-format").and_then(yuv_format) {
            self.layout.format = format;
        }
        if let Some(stride) = positive_usize(&fmt, "stride") {
            self.layout.stride = stride;
        }
        if let Some(slice_height) = positive_usize(&fmt, "slice-height") {
            self.layout.slice_height = slice_height;
        }
        self.layout.apply_crop(&fmt);
        // A decoder must never report less than the visible size.
        self.layout.stride = self.layout.stride.max(self.layout.width as usize);
        self.layout.slice_height = self.layout.slice_height.max(self.layout.height as usize);
        self.layout.matrix = matrix_of(&fmt, self.layout.width, self.layout.height);
        self.layout.range = range_of(&fmt);
        Ok(())
    }

    /// Queue one sample (or the end-of-stream marker). `Ok(false)` when the
    /// codec has no free input buffer, or when nothing is left to feed.
    fn feed_input(&mut self) -> Result<bool, VideoError> {
        if self.eos_queued {
            return Ok(false);
        }
        // One dequeue attempt per call: the caller's pump loop retries, so
        // blocking here as well would only double the timeouts.
        let mut input = match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
            Ok(DequeuedInputBufferResult::Buffer(input)) => input,
            Ok(DequeuedInputBufferResult::TryAgainLater) => return Ok(false),
            Err(e) => return Err(e.into()),
        };

        // The sample is read straight into the codec's input buffer, so no
        // staging copy is needed. SAFETY: `as_mut_ptr` covers `capacity`
        // writable bytes and the buffer stays alive until it is queued.
        let (size, capacity) = {
            let buffer = input.buffer_mut();
            let capacity = buffer.len();
            let ptr = buffer.as_mut_ptr().cast::<u8>();
            (self.extractor.read_sample(ptr, capacity), capacity)
        };
        if size < 0 {
            // End of stream: the extractor is drained, and the codec needs an
            // explicit marker before it flushes the frames still in flight.
            self.eos_queued = true;
            self.codec.queue_input_buffer(
                input,
                0,
                0,
                0,
                ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
            )?;
            return Ok(true);
        }
        let size = size.unsigned_abs();
        if size > capacity {
            return Err(VideoError::Decode(format!(
                "sample of {size} bytes does not fit the {capacity}-byte input buffer"
            )));
        }

        let pts_us = self.extractor.sample_time_us();
        let flags = self.extractor.sample_flags();
        if self.expect_sync {
            if flags & ffi::AMEDIAEXTRACTOR_SAMPLE_FLAG_SYNC == 0 {
                return Err(VideoError::Seek(format!(
                    "extractor did not land on a sync sample (pts {pts_us}us, flags {flags:#x})"
                )));
            }
            self.expect_sync = false;
        }
        let _advanced = self.extractor.advance();

        self.codec
            .queue_input_buffer(input, 0, size, pts_us.unsigned_abs(), 0)?;
        Ok(true)
    }

    /// Convert every output buffer that is ready; `Ok(None)` means "nothing
    /// yet" and [`AndroidDecoder::output_eos`] reports the end of the stream.
    fn try_output(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        loop {
            match self.codec.dequeue_output_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => return Ok(None),
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => self.refresh_layout()?,
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    let info = *out.info();
                    let flags = info.flags();
                    let size = info.size();
                    let pts_us = info.presentation_time_us();
                    let offset = usize::try_from(info.offset()).map_err(|_| {
                        VideoError::Decode(format!("negative buffer offset {}", info.offset()))
                    })?;
                    let codec_config = flags & ffi::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    let eos = flags & ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;

                    let frame = if codec_config || size <= 0 {
                        None
                    } else {
                        let buffer = out.buffer();
                        let end = offset.saturating_add(size as usize).min(buffer.len());
                        if end <= offset {
                            return Err(VideoError::Decode(format!(
                                "empty output buffer slice {offset}..{end} of {}",
                                buffer.len()
                            )));
                        }
                        Some(self.convert(&buffer[offset..end], pts_us)?)
                    };
                    self.codec.release_output_buffer(out, false)?;

                    if eos {
                        self.output_eos = true;
                        return Ok(frame);
                    }
                    if frame.is_some() {
                        return Ok(frame);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The decoded buffer as planes. **No pixel conversion happens here.**
    ///
    /// This is the whole point of the shape: `yuv_to_rgba` used to run on every
    /// decoded frame — 2.07M pixels of f32 arithmetic at 1080p, plus the
    /// rotation — for pixels the compositor never read, because it samples the
    /// planes on the GPU (docs/12 §12.3). A video layer therefore cost several
    /// times its own decode, in the preview and in every exported frame alike,
    /// and the more layers used the clip the worse it got.
    ///
    /// The conversion is now lazy ([`VideoFrame::rgba`]) and happens only for a
    /// caller that asks for pixels — the legacy byte path and the tests.
    fn convert(&self, region: &[u8], pts_us: i64) -> Result<VideoFrame, VideoError> {
        let layout = self.layout;
        let time_ms = pts_us.div_euclid(1000);
        match YuvPlanes::new(
            layout.format,
            YuvLayout {
                stride: layout.stride,
                slice_height: layout.slice_height,
            },
            layout.width,
            layout.height,
            layout.matrix,
            layout.range,
            layout.rotation_deg,
            Arc::<[u8]>::from(region),
        ) {
            Ok(planes) => Ok(VideoFrame::from_planes(
                layout.width,
                layout.height,
                time_ms,
                Arc::new(planes),
            )),
            Err(_) => {
                // The plane description was refused — an odd I420 stride, say.
                // Such a frame cannot travel as samples, so it is converted the
                // old way rather than lost: the fast path exists to *avoid* the
                // conversion, not to make a clip that cannot take it undecodable.
                let planes = Self::plane_slices(region, self.layout)?;
                let rgba = self.convert_rgba(region, &planes)?;
                Ok(VideoFrame::from_rgba(
                    layout.width,
                    layout.height,
                    time_ms,
                    rgba,
                ))
            }
        }
    }

    /// YUV planes → upright RGBA8, honouring `stride` / `slice-height`.
    ///
    /// Kept for the frames the plane description refuses, and for the callers
    /// that ask for pixels. It is deliberately **not** on the path a composited
    /// frame takes any more.
    fn convert_rgba(&self, region: &[u8], planes: &[&[u8]; 3]) -> Result<Arc<[u8]>, VideoError> {
        let layout = self.layout;
        // The destination is deliberately not zeroed: `yuv_to_rgba` writes
        // every byte of it, so a `vec![0u8; …]` here would be an 8 MB memset
        // per frame at 1080p whose result is overwritten byte for byte.
        let mut storage = uninit_rgba(layout.output_len());
        // SAFETY: `yuv_to_rgba` validates the geometry before its first write
        // and its emit loop then covers every output pixel, so on the `Ok` path
        // below every byte of `storage` has been written. On the `Err` path the
        // buffer is dropped unread.
        yuv::yuv_to_rgba(
            layout.format,
            YuvLayout {
                stride: layout.stride,
                slice_height: layout.slice_height,
            },
            *planes,
            layout.width,
            layout.height,
            layout.matrix,
            layout.range,
            layout.rotation_deg,
            unsafe { init_rgba(&mut storage) },
        )
        .map_err(VideoError::Decode)?;
        // SAFETY: the conversion above returned `Ok`, so every byte is written.
        let _ = region;
        // SAFETY: the conversion above returned `Ok`, so every byte is written.
        Ok(unsafe { filled_rgba(storage) })
    }

/// The three plane slices of a decoded buffer, as [`YuvFormat`] packs them.
///
/// The packing rules rather than the visible geometry: each plane is walked by
/// the caller with `stride`/`slice-height`, and a packing that leaves a plane
/// unused returns an empty slice for it. Every length check the conversion needs
/// lives here, so the fast path and the fallback agree about what a valid buffer
/// is instead of each trusting its own arithmetic.
    fn plane_slices(region: &[u8], layout: FrameLayout) -> Result<[&[u8]; 3], VideoError> {
    let stride = layout.stride;
    let slice_height = layout.slice_height;
    let y_len = stride.checked_mul(slice_height).ok_or_else(|| {
        VideoError::Decode(format!("stride {stride} * slice-height {slice_height} overflows"))
    })?;
    if region.len() < y_len {
        return Err(VideoError::Decode(format!(
            "decoded buffer of {} bytes cannot hold a {stride}x{slice_height} luma plane",
            region.len()
        )));
    }
    let luma = &region[..y_len];
    let chroma = &region[y_len..];
    Ok(match layout.format {
        YuvFormat::I420 => {
            let chroma_stride = stride / 2;
            let chroma_len = chroma_stride * slice_height.div_ceil(2);
            if chroma.is_empty() || chroma.len() < 2 * chroma_len {
                return Err(VideoError::Decode(format!(
                    "decoded buffer of {} bytes cannot hold two {chroma_stride}x{} chroma planes",
                    region.len(),
                    slice_height.div_ceil(2)
                )));
            }
            [luma, &chroma[..chroma_len], &chroma[chroma_len..2 * chroma_len]]
        }
        YuvFormat::Nv12 | YuvFormat::Nv21 => [luma, chroma, &[]],
    })
}


/// Drive the codec until one frame comes out or the stream ends.
    fn pump(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        if self.output_eos {
            return Ok(None);
        }
        let deadline = Instant::now() + PUMP_DEADLINE;
        loop {
            if let Some(frame) = self.try_output()? {
                return Ok(Some(frame));
            }
            if self.output_eos {
                return Ok(None);
            }
            let fed = self.feed_input()?;
            if !fed && Instant::now() > deadline {
                return Err(VideoError::Decode(
                    "MediaCodec produced no output for a queued sample".into(),
                ));
            }
        }
    }
}

impl FrameDecoder for AndroidDecoder {
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        self.pump()
    }

    fn seek(&mut self, time_ms: i64) -> Result<(), VideoError> {
        // `SEEK_PREVIOUS_SYNC` is the only mode that guarantees the decoder can
        // reach `time_ms` by decoding forward; the following samples are
        // checked for the sync flag before they are queued.
        self.extractor
            .seek(time_ms.saturating_mul(1000), ffi::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC)?;
        self.codec
            .flush()
            .map_err(|e| VideoError::Seek(format!("codec flush after seek failed: {e}")))?;
        self.expect_sync = true;
        self.eos_queued = false;
        self.output_eos = false;
        Ok(())
    }
}

/// `MediaExtractor` + `MediaCodec` + `AImageReader`, decoding to YUV planes.
///
/// The pump is deliberately the same as [`AndroidDecoder`]'s — same extractor,
/// same codec calls, same sync-sample and deadline policy — because the only
/// difference is where the pixels end up. Two consequences of that difference
/// are worth stating, since both are easy to get wrong:
///
/// * **Rotation.** The reader does *not* hand back upright frames.
///   `MediaCodec` never applies `AMEDIAFORMAT_KEY_ROTATION`; it is metadata
///   for the consumer, and a surface consumer is no more exempt than a
///   ByteBuffer one. The planes are therefore in the decoder's coded
///   orientation and [`SurfaceFrame::rotation_deg`] carries the turn the
///   consumer still owes. On a GPU that is a quad transform; there is
///   deliberately no CPU rotation pass on this route (docs/12 §12.3).
/// * **Frame timing.** A surface frame has no output buffer to read a
///   presentation time from, so the pts comes from `AImage_getTimestamp`.
/// `pub(crate)` only because [`DecodeSession`] names it in a public variant;
/// its fields stay private and it is reached through that enum.
pub(crate) struct SurfaceDecoder {
    extractor: Extractor,
    codec: MediaCodec,
    reader: SurfaceReader,
    /// The reader's window, cloned. `MediaCodec::configure` borrows it and the
    /// codec keeps its own reference, so this exists only to keep the borrow
    /// alive across the call.
    _window: NativeWindow,
    info: VideoInfo,
    /// Luma→RGB matrix and sample range for the consumer's shader. Taken from
    /// the track format at open and refreshed from the codec's output format
    /// when it changes, because a decoder may only declare the real matrix
    /// there.
    matrix: yuv::Matrix,
    range: yuv::Range,
    expect_sync: bool,
    eos_queued: bool,
    output_eos: bool,
}

#[allow(dead_code)] // see `DecodeStrategy`
impl SurfaceDecoder {
    /// Open the surface route for an already-probed track.
    ///
    /// The reader is sized from the track's *coded* width/height rather than
    /// the cropped visible size: the decoder renders the coded frame and
    /// expresses the visible window through the crop rect, so opening at the
    /// visible size would ask the codec to render into a buffer it does not
    /// intend to fill.
    fn start(extractor: Extractor, track: TrackInfo) -> Result<Self, VideoError> {
        let coded_width = positive_u32(&track.format, "width").ok_or(VideoError::Unsupported(
            "the video track has no coded width",
        ))?;
        let coded_height = positive_u32(&track.format, "height").ok_or(VideoError::Unsupported(
            "the video track has no coded height",
        ))?;

        let reader = SurfaceReader::open(coded_width, coded_height)?;
        let window = reader.window()?;
        let codec = MediaCodec::from_decoder_type(&track.mime)
            .ok_or_else(|| VideoError::Open(format!("no MediaCodec decoder for {}", track.mime)))?;
        // The one line that makes this the surface route: the reader's window
        // goes to the codec instead of `None`.
        codec
            .configure(&track.format, Some(&window), MediaCodecDirection::Decoder)
            .map_err(|e| VideoError::Open(format!("configure({}) failed: {e}", track.mime)))?;
        codec
            .start()
            .map_err(|e| VideoError::Open(format!("codec start failed: {e}")))?;

        Ok(Self {
            extractor,
            codec,
            reader,
            _window: window,
            info: track.info,
            matrix: matrix_of(&track.format, coded_width, coded_height),
            range: range_of(&track.format),
            expect_sync: true,
            eos_queued: false,
            output_eos: false,
        })
    }

    /// Take the codec's colour parameters once it declares its output format.
    ///
    /// `MediaCodec` signals this before the first frame, and some decoders only
    /// state the real `color-standard` / `color-range` there rather than in the
    /// track header. Dimensions are deliberately *not* taken from it on this
    /// route: the reader is the authority on frame size, because the codec
    /// renders the coded frame and expresses the visible window through the
    /// crop rect (see [`SurfaceDecoder::acquire`]).
    fn refresh_color_format(&mut self) {
        let fmt = self.codec.output_format();
        self.matrix = matrix_of(&fmt, self.info.width, self.info.height);
        self.range = range_of(&fmt);
    }

    /// Queue one sample (or the end-of-stream marker); see
    /// [`AndroidDecoder::feed_input`], which this mirrors.
    fn feed_input(&mut self) -> Result<bool, VideoError> {
        if self.eos_queued {
            return Ok(false);
        }
        let mut input = match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
            Ok(DequeuedInputBufferResult::Buffer(input)) => input,
            Ok(DequeuedInputBufferResult::TryAgainLater) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let (size, capacity) = {
            let buffer = input.buffer_mut();
            let capacity = buffer.len();
            let ptr = buffer.as_mut_ptr().cast::<u8>();
            (self.extractor.read_sample(ptr, capacity), capacity)
        };
        if size < 0 {
            self.eos_queued = true;
            self.codec.queue_input_buffer(
                input,
                0,
                0,
                0,
                ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
            )?;
            return Ok(true);
        }
        let size = size.unsigned_abs();
        if size > capacity {
            return Err(VideoError::Decode(format!(
                "sample of {size} bytes does not fit the {capacity}-byte input buffer"
            )));
        }
        let pts_us = self.extractor.sample_time_us();
        let flags = self.extractor.sample_flags();
        if self.expect_sync {
            if flags & ffi::AMEDIAEXTRACTOR_SAMPLE_FLAG_SYNC == 0 {
                return Err(VideoError::Seek(format!(
                    "extractor did not land on a sync sample (pts {pts_us}us, flags {flags:#x})"
                )));
            }
            self.expect_sync = false;
        }
        let _advanced = self.extractor.advance();
        self.codec
            .queue_input_buffer(input, 0, size, pts_us.unsigned_abs(), 0)?;
        Ok(true)
    }

    /// Release every output buffer that is ready.
    ///
    /// With a surface configured there are no pixels in an output buffer — the
    /// decoder has already rendered them into the reader's window — so the
    /// size is not inspected and `render = true` is what makes the frame
    /// actually appear in the reader. Releasing with `render = false` would
    /// silently discard every frame, which is the classic way this route
    /// "decodes" and shows nothing.
    fn release_output(&mut self) -> Result<(), VideoError> {
        loop {
            match self.codec.dequeue_output_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => return Ok(()),
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    self.refresh_color_format();
                }
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    let flags = out.info().flags();
                    let eos = flags & ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                    let codec_config = flags & ffi::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    // A codec-config buffer carries extradata, not a frame, so
                    // rendering it would push a bogus image into the reader.
                    self.codec.release_output_buffer(out, !codec_config)?;
                    if eos {
                        self.output_eos = true;
                        return Ok(());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Take the newest frame and validate its geometry.
    ///
    /// `acquire_latest` is the backpressure policy, and it is the reason the
    /// reader uses [`SURFACE_MAX_IMAGES`]: it *discards* queued frames older
    /// than the newest instead of handing them out in order. For a preview that
    /// is the right trade — a frame that arrives after its slot on screen is
    /// worth less than the newest one, and blocking the decoder to deliver
    /// stale frames in order would add latency without adding correctness. The
    /// cost is that this route may skip frames, which is why it is opt-in and
    /// why the caller is the one choosing it.
    ///
    /// Returns `Ok(None)` for "nothing right now", which covers both an empty
    /// queue and every slot being acquired.
    fn acquire(&mut self) -> Result<Option<SurfaceFrame>, VideoError> {
        let Some(image) = self.reader.acquire_latest()? else {
            return Ok(None);
        };
        let (width, height) = image.size()?;
        // Validate the plane count *before* anything reads a plane: an image
        // whose plane count is not 3 cannot be indexed as YUV420, and reading
        // plane 2 of a 1-plane image is the out-of-bounds case.
        image.plane_count()?;
        let crop = image.crop_rect(width, height)?;
        let frame = SurfaceFrame {
            time_ms: image.timestamp_ms(),
            matrix: self.matrix,
            range: self.range,
            width: crop.2,
            height: crop.3,
            crop,
            rotation_deg: self.info.rotation_deg,
            image,
        };
        Ok(Some(frame))
    }

    /// Drive the codec until one frame comes out or the stream ends.
    fn pump(&mut self) -> Result<Option<SurfaceFrame>, VideoError> {
        if self.output_eos {
            // The codec is finished, but the reader still holds whatever was
            // rendered before the end-of-stream buffer was released. Keep
            // draining it one frame per call rather than reporting the end
            // immediately, or the tail of the clip is silently lost.
            return self.acquire();
        }
        let deadline = Instant::now() + PUMP_DEADLINE;
        loop {
            self.release_output()?;
            if let Some(frame) = self.acquire()? {
                return Ok(Some(frame));
            }
            if self.output_eos {
                // The end-of-stream buffer has been released, so the reader
                // still holds whatever the decoder rendered; drain before
                // reporting the end, or the tail of the clip is lost.
                return self.acquire();
            }
            let fed = self.feed_input()?;
            if !fed && Instant::now() > deadline {
                return Err(VideoError::Decode(
                    "MediaCodec produced no output for a queued sample".into(),
                ));
            }
        }
    }

    /// Next YUV-plane frame, or `None` at the end of the stream.
    ///
    /// An inherent method rather than a [`FrameDecoder`] impl because the
    /// surface route hands out [`SurfaceFrame`] (YUV planes), not the RGBA8
    /// [`VideoFrame`] that trait is defined in terms of. Implementing it would
    /// mean converting, which is the thing this route exists to avoid.
    pub(super) fn next_frame(&mut self) -> Result<Option<SurfaceFrame>, VideoError> {
        self.pump()
    }

    /// Seek to `time_ms`, exactly as the byte-buffer route does.
    ///
    /// The reader is not drained here: `MediaCodec::flush` returns the
    /// decoder's pending output, and frames already rendered into the reader's
    /// queue belong to the old position. They are dropped by
    /// [`SurfaceDecoder::acquire`]'s newest-wins policy, so a surface seek can
    /// surface a frame from just before the target and then settle. That is the
    /// price of never blocking the decoder, and the caller chooses to pay it by
    /// picking the strategy.
    pub(super) fn seek(&mut self, time_ms: i64) -> Result<(), VideoError> {
        self.extractor.seek(
            time_ms.saturating_mul(1000),
            ffi::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
        )?;
        self.codec
            .flush()
            .map_err(|e| VideoError::Seek(format!("codec flush after seek failed: {e}")))?;
        self.expect_sync = true;
        self.eos_queued = false;
        self.output_eos = false;
        Ok(())
    }
}

/// Read `mime` from a track format.
fn track_mime(fmt: &mut MediaFormat) -> Option<String> {
    fmt.str("mime").map(str::to_owned)
}

/// Open the container, pick the first video track and probe its metadata.
fn open_extractor(fd: RawFd, offset: i64, length: i64) -> Result<(Extractor, TrackInfo), VideoError> {
    let extractor = Extractor::open(fd, offset, length)?;

    let mut video: Option<usize> = None;
    let mut has_audio = false;
    for idx in 0..extractor.track_count() {
        let Ok(mut fmt) = extractor.track_format(idx) else {
            continue;
        };
        match track_mime(&mut fmt).as_deref() {
            Some(mime) if mime.starts_with("video/") => {
                if video.is_none() {
                    video = Some(idx);
                }
            }
            Some(mime) if mime.starts_with("audio/") => has_audio = true,
            _ => {}
        }
    }

    let index = video.ok_or_else(|| VideoError::Open("no video track in the container".into()))?;
    let mut format = extractor.track_format(index)?;
    let mime = track_mime(&mut format)
        .ok_or_else(|| VideoError::Open("the video track has no mime type".into()))?;
    extractor.select_track(index)?;

    let mut width = positive_u32(&format, "width")
        .ok_or_else(|| VideoError::Open("the video track has no width".into()))?;
    let mut height = positive_u32(&format, "height")
        .ok_or_else(|| VideoError::Open("the video track has no height".into()))?;
    // The track format describes the coded frame; `crop` (when present) is the
    // visible area, and the same reduction is applied to every decoded frame by
    // `FrameLayout`, so `VideoInfo` stays in step with `VideoFrame::rgba`.
    if let Some((cropped_width, cropped_height)) = crop_size(&format)
        && cropped_width <= width
        && cropped_height <= height
    {
        width = cropped_width;
        height = cropped_height;
    }
    let rotation_deg = normalize_rotation(format.i32("rotation").unwrap_or(0));
    let (visible_width, visible_height) = if rotation_deg == 90 || rotation_deg == 270 {
        (height, width)
    } else {
        (width, height)
    };
    let duration_ms = u64::try_from(format.i64("durationUs").unwrap_or(0).max(0) / 1000)
        .unwrap_or(0);
    let fps = format.f32("frame-rate").filter(|f| *f > 0.0).unwrap_or(0.0);

    let info = VideoInfo {
        width: visible_width,
        height: visible_height,
        duration_ms,
        fps,
        rotation_deg,
        has_audio,
    };
    Ok((
        extractor,
        TrackInfo {
            mime,
            format,
            info,
        },
    ))
}

/// Probe the video track in `fd`; see [`super::probe_video_fd`].
pub(super) fn probe_video_fd(fd: RawFd, offset: i64, length: i64) -> Result<VideoInfo, VideoError> {
    let (extractor, track) = open_extractor(fd, offset, length)?;
    drop(extractor);
    Ok(track.info)
}

/// Open `fd` for frame-accurate decoding on the byte-buffer route; see
/// [`super::open_video_fd`].
///
/// This is the historical behaviour and stays the default: callers that have
/// not opted into [`DecodeStrategy::Surface`] get RGBA8 frames exactly as
/// before.
pub(super) fn open_video_fd(
    fd: RawFd,
    offset: i64,
    length: i64,
) -> Result<Box<dyn VideoSource>, VideoError> {
    let (extractor, track) = open_extractor(fd, offset, length)?;
    let decoder = AndroidDecoder::start(extractor, track)?;
    let info = decoder.info;
    Ok(Box::new(DecodingSource::new(info, decoder)))
}

/// Open `fd` on `strategy`, falling back to the byte-buffer route when the
/// surface route cannot be opened.
///
/// The fallback is deliberate and *reported*: [`DecodeStrategy::Surface`] is
/// allowed to fail on a device whose decoder refuses a surface, and falling
/// back is the right thing to do (docs/12 §12.4 keeps the CPU path alive for
/// exactly this reason). What is not acceptable is failing silently, so the
/// reason travels with the session in
/// [`DecodeSession::ByteBufferFallback`] instead of being logged here and lost.
///
/// Only a failure to *open* falls back. Once frames are flowing, an error is
/// returned as-is: switching routes mid-stream would silently change what
/// `frame_at` returns and hide the real fault.
#[allow(dead_code)] // The surface route is built but not selected; see `DecodeStrategy` for why, and `docs/12` §12.3.
pub(super) fn open_video_fd_with_strategy(
    fd: RawFd,
    offset: i64,
    length: i64,
    strategy: DecodeStrategy,
) -> Result<DecodeSession, VideoError> {
    if strategy == DecodeStrategy::ByteBuffer {
        return Ok(DecodeSession::ByteBuffer(open_video_fd(fd, offset, length)?));
    }

    let (extractor, track) = open_extractor(fd, offset, length)?;
    match SurfaceDecoder::start(extractor, track) {
        Ok(decoder) => Ok(DecodeSession::Surface(decoder)),
        Err(surface_error) => {
            // The extractor was moved into the failed attempt, so it is
            // reopened from the descriptor, which that attempt left at the
            // position the caller gave us.
            let (extractor, track) = open_extractor(fd, offset, length)?;
            let decoder = AndroidDecoder::start(extractor, track)?;
            let info = decoder.info;
            Ok(DecodeSession::ByteBufferFallback {
                source: Box::new(DecodingSource::new(info, decoder)),
                reason: format!("{surface_error} (fell back to ByteBuffer decoding)"),
            })
        }
    }
}

/// Audio half of the clip pipeline: the container's sound track, decoded whole.
///
/// The sound of a video file used to be thrown away because only the video
/// track was ever selected. The extractor and the pump are the same code as the
/// video path; only the track selection and the buffer interpretation differ,
/// because an audio ByteBuffer is packed 16-bit PCM rather than padded YUV.
struct AndroidAudioDecoder {
    extractor: Extractor,
    codec: MediaCodec,
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
    eos_queued: bool,
    output_eos: bool,
}

impl AndroidAudioDecoder {
    /// Default until the codec reports its output format: every Android audio
    /// decoder outputs packed 16-bit PCM, and the format signalled before the
    /// first buffer is authoritative.
    const FALLBACK_SAMPLE_RATE: u32 = 48_000;
    const FALLBACK_CHANNELS: u16 = 2;

    fn start(extractor: Extractor, mime: &str, format: MediaFormat) -> Result<Self, VideoError> {
        let codec = MediaCodec::from_decoder_type(mime)
            .ok_or_else(|| VideoError::Open(format!("no MediaCodec decoder for {mime}")))?;
        codec
            .configure(&format, None, MediaCodecDirection::Decoder)
            .map_err(|e| VideoError::Open(format!("configure({mime}) failed: {e}")))?;
        codec
            .start()
            .map_err(|e| VideoError::Open(format!("codec start failed: {e}")))?;
        Ok(Self {
            extractor,
            codec,
            sample_rate: positive_u32(&format, "sample-rate").unwrap_or(Self::FALLBACK_SAMPLE_RATE),
            channels: u16::try_from(positive_u32(&format, "channel-count").unwrap_or(0))
                .ok()
                .filter(|channels| *channels > 0)
                .unwrap_or(Self::FALLBACK_CHANNELS),
            samples: Vec::new(),
            eos_queued: false,
            output_eos: false,
        })
    }

    /// Take the real rate/channel count from the codec's output format.
    fn refresh_stream_format(&mut self) -> Result<(), VideoError> {
        let fmt = self.codec.output_format();
        if let Some(rate) = positive_u32(&fmt, "sample-rate") {
            self.sample_rate = rate;
        }
        if let Some(channels) = u16::try_from(positive_u32(&fmt, "channel-count").unwrap_or(0)).ok()
        {
            if channels > 0 {
                self.channels = channels;
            }
        }
        Ok(())
    }

    /// Queue one compressed sample, or the end-of-stream marker once the
    /// extractor is drained. The sample is read straight into the codec's input
    /// buffer, so no staging copy is needed (see [`AndroidDecoder::feed_input`]).
    fn feed_input(&mut self) -> Result<(), VideoError> {
        if self.eos_queued {
            return Ok(());
        }
        let mut input = match self.codec.dequeue_input_buffer(DEQUEUE_TIMEOUT) {
            Ok(DequeuedInputBufferResult::Buffer(input)) => input,
            Ok(DequeuedInputBufferResult::TryAgainLater) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let (size, capacity) = {
            let buffer = input.buffer_mut();
            let capacity = buffer.len();
            let ptr = buffer.as_mut_ptr().cast::<u8>();
            (self.extractor.read_sample(ptr, capacity), capacity)
        };
        if size < 0 {
            self.eos_queued = true;
            self.codec.queue_input_buffer(
                input,
                0,
                0,
                0,
                ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
            )?;
            return Ok(());
        }
        let size = size.unsigned_abs();
        if size > capacity {
            return Err(VideoError::Decode(format!(
                "audio sample of {size} bytes does not fit the {capacity}-byte input buffer"
            )));
        }
        let pts_us = self.extractor.sample_time_us();
        let _advanced = self.extractor.advance();
        self.codec
            .queue_input_buffer(input, 0, size, pts_us.unsigned_abs(), 0)?;
        Ok(())
    }

    /// Append every output buffer that is ready.
    fn collect_output(&mut self) -> Result<(), VideoError> {
        loop {
            match self.codec.dequeue_output_buffer(DEQUEUE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => return Ok(()),
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    self.refresh_stream_format()?;
                }
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    let info = *out.info();
                    let flags = info.flags();
                    // The codec config buffer carries the extradata for a
                    // `MediaExtractor` that reuses this decoder; it is not PCM.
                    let codec_config = flags & ffi::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    let eos = flags & ffi::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                    let size = info.size();
                    if size > 0 && !codec_config {
                        let offset = usize::try_from(info.offset()).map_err(|_| {
                            VideoError::Decode(format!("negative buffer offset {}", info.offset()))
                        })?;
                        let buffer = out.buffer();
                        let end = offset.saturating_add(size as usize).min(buffer.len());
                        if end <= offset {
                            return Err(VideoError::Decode(format!(
                                "empty audio output buffer slice {offset}..{end} of {}",
                                buffer.len()
                            )));
                        }
                        let pcm = &buffer[offset..end];
                        // A free function, not a `&mut self` method: `buffer`
                        // borrows the codec that the loop is holding.
                        self.samples
                            .extend_from_slice(&decode_pcm16(pcm, self.channels)?);
                    }
                    self.codec.release_output_buffer(out, false)?;
                    if eos {
                        self.output_eos = true;
                        return Ok(());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Feed samples and collect PCM until the decoder signals end of stream.
    fn run(&mut self) -> Result<(), VideoError> {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !self.output_eos {
            self.collect_output()?;
            if self.output_eos {
                break;
            }
            if !self.eos_queued {
                self.feed_input()?;
                continue;
            }
            // Everything is queued; the decoder is still holding frames back.
            if Instant::now() > deadline {
                return Err(VideoError::Decode(
                    "audio decoder produced no output before the deadline".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Decode packed 16-bit PCM into interleaved `f32`.
///
/// Every Android audio decoder emits packed little-endian `i16` in a
/// ByteBuffer, so there is no stride or slice-height to honour here (unlike the
/// YUV video path). A buffer that is not a whole number of frames is rejected
/// rather than truncated: it would mean the channel count we read is wrong, and
/// silently dropping the tail would shift every later frame.
fn decode_pcm16(region: &[u8], channels: u16) -> Result<Vec<f32>, VideoError> {
    let frame_bytes = channels as usize * 2;
    if frame_bytes == 0 {
        return Err(VideoError::Decode("audio output has no channels".into()));
    }
    if region.len() % frame_bytes != 0 {
        return Err(VideoError::Decode(format!(
            "audio buffer of {} bytes is not a whole number of {channels}-channel frames",
            region.len()
        )));
    }
    let mut out = Vec::with_capacity(region.len() / 2);
    for frame in region.chunks_exact(frame_bytes) {
        for channel in frame.chunks_exact(2) {
            // Reinterpreting the bits of a `u16` is the two's complement read of
            // a little-endian `i16` without a fallible conversion.
            let bits = u16::from_le_bytes([channel[0], channel[1]]);
            out.push(f32::from(bits as i16) / 32_768.0);
        }
    }
    Ok(out)
}

/// Open the container, pick the first audio track and select it.
fn open_audio_extractor(
    fd: RawFd,
    offset: i64,
    length: i64,
) -> Result<(Extractor, String, MediaFormat), VideoError> {
    let extractor = Extractor::open(fd, offset, length)?;
    let mut audio = None;
    for idx in 0..extractor.track_count() {
        let Ok(mut fmt) = extractor.track_format(idx) else {
            continue;
        };
        if track_mime(&mut fmt)
            .as_deref()
            .is_some_and(|mime| mime.starts_with("audio/"))
        {
            audio = Some(idx);
            break;
        }
    }
    let index = audio.ok_or(VideoError::NoAudioTrack)?;
    let mut format = extractor.track_format(index)?;
    let mime = track_mime(&mut format)
        .ok_or_else(|| VideoError::Open("the audio track has no mime type".into()))?;
    extractor.select_track(index)?;
    Ok((extractor, mime, format))
}

/// Decode the first audio track of the clip in `fd`; see
/// [`super::decode_audio_track_fd`].
pub(super) fn decode_audio_track_fd(
    fd: RawFd,
    offset: i64,
    length: i64,
) -> Result<DecodedAudio, VideoError> {
    let (extractor, mime, format) = open_audio_extractor(fd, offset, length)?;
    let mut decoder = AndroidAudioDecoder::start(extractor, &mime, format)?;
    decoder.run()?;
    if decoder.samples.is_empty() {
        return Err(VideoError::Decode(format!(
            "the {mime} track of the clip decoded to zero audio frames"
        )));
    }
    Ok(DecodedAudio {
        samples: decoder.samples,
        sample_rate: decoder.sample_rate,
        channels: decoder.channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoding a real clip needs a device/emulator: MediaCodec + libmediandk
    /// cannot run on the host, and there is no fixture file in the repo.
    #[test]
    #[ignore = "needs-device: Android MediaCodec/MediaExtractor required"]
    fn device_decode_frames_and_seek() {
        let path = std::env::var("RUMO_VIDEO_FIXTURE").expect("set RUMO_VIDEO_FIXTURE to a clip");
        let file = std::fs::File::open(path).expect("open fixture");
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);

        let info = probe_video_fd(fd, 0, -1).expect("probe");
        assert!(info.width > 0 && info.height > 0);

        let mut source = open_video_fd(fd, 0, -1).expect("open");
        let first = source.frame_at(0).expect("frame at 0");
        assert_eq!(first.rgba.len(), (info.width * info.height * 4) as usize);
        assert_eq!(first.time_ms, 0);

        // Same time again must come from the cache.
        let again = source.frame_at(0).expect("frame at 0 again");
        assert_eq!(again, first);

        // Backward seek after walking forward.
        let far = (info.duration_ms / 2) as i64;
        let mid = source.frame_at(far).expect("mid frame");
        assert!(mid.time_ms <= far);
        let back = source.frame_at(0).expect("back to 0");
        assert_eq!(back.time_ms, 0);
    }

    /// The surface route needs a device like the byte-buffer route, and more:
    /// whether a given decoder will render into an `AImageReader` at all is
    /// exactly the question only hardware can answer.
    #[test]
    #[ignore = "needs-device: Android MediaCodec surface output + AImageReader required"]
    fn device_surface_route_yields_three_planes() {
        let path = std::env::var("RUMO_VIDEO_FIXTURE").expect("set RUMO_VIDEO_FIXTURE to a clip");
        let file = std::fs::File::open(path).expect("open fixture");
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);

        let mut session =
            open_video_fd_with_strategy(fd, 0, -1, DecodeStrategy::Surface).expect("open");
        // A refused surface route is legal on some devices; when it happens it
        // has to be visible here, which is the whole point of the variant.
        if let Some(reason) = session.surface_fallback_reason() {
            eprintln!("rumo-video: surface route unavailable, {reason}");
            return;
        }
        let info = session.info();
        assert!(info.width > 0 && info.height > 0);

        let DecodeSession::Surface(ref mut decoder) = session else {
            panic!("expected the surface route");
        };
        let frame = decoder.next_frame().expect("first frame").expect("a frame");
        assert_eq!(frame.rotation_deg(), info.rotation_deg);
        assert!(frame.width() > 0 && frame.height() > 0);

        // Three planes, and every one of them walkable with its own strides.
        for index in 0..YUV420_PLANES {
            let plane = frame.plane(index).expect("plane is present");
            assert!(!plane.data.is_empty());
            assert!(plane.row_stride > 0 && plane.pixel_stride > 0);
        }
        assert!(frame.plane(YUV420_PLANES).is_none(), "no fourth plane");

        // `width`/`height` are the crop's size, so the two must agree; a
        // mismatch would mean the crop rect was read with a different frame
        // size than the one it was validated against.
        let (left, top, crop_w, crop_h) = frame.crop();
        assert_eq!((frame.width(), frame.height()), (crop_w, crop_h));
        assert!(left < crop_w.max(1) && top < crop_h.max(1));

        decoder.seek(0).expect("seek back to the start");
    }

    /// Decoding the audio track needs a device/emulator, like the video path.
    #[test]
    #[ignore = "needs-device: Android MediaCodec/MediaExtractor required"]
    fn device_decode_audio_track_into_mixer() {
        let path = std::env::var("RUMO_VIDEO_FIXTURE").expect("set RUMO_VIDEO_FIXTURE to a clip");
        let file = std::fs::File::open(path).expect("open fixture");
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);

        let info = probe_video_fd(fd, 0, -1).expect("probe");
        if !info.has_audio {
            return; // a silent clip is a valid fixture outcome
        }

        let audio = decode_audio_track_fd(fd, 0, -1).expect("decode audio track");
        assert!(audio.sample_rate > 0);
        assert!(audio.channels > 0);
        assert!(!audio.samples.is_empty());
        assert_eq!(
            audio.frames(),
            audio.samples.len() / audio.channels as usize
        );

        let source = crate::video::audio_source_fd(fd, 0, -1, 500, 0, 0.5).expect("mixer source");
        assert_eq!(source.sample_rate, audio.sample_rate);
        assert_eq!(source.channels, audio.channels);
        assert_eq!(source.start_ms, 500);
        assert_eq!(source.gain, 0.5);
    }
}

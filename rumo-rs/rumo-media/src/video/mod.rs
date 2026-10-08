// SPDX-License-Identifier: Apache-2.0

//! Video-clip decoding (docs/08 §8.4).
//!
//! The public surface ([`VideoInfo`], [`VideoFrame`], [`VideoError`],
//! [`VideoSource`], [`open_video_fd`], [`probe_video_fd`]) is
//! platform-neutral. On Android the backend is `AMediaExtractor` +
//! `MediaCodec` (module [`android`]); everywhere else, opening a clip reports
//! [`VideoError::Unsupported`], so host builds and `cargo test` never link
//! `libmediandk`.
//!
//! The seek/cache policy lives in [`DecodingSource`], which is generic over the
//! small internal [`FrameDecoder`] trait. That keeps the "decode forward until
//! `presentation_time >= t`", cache-bound and end-of-stream clamping rules out
//! of `cfg(target_os = "android")`, where they could not be tested; the host
//! tests drive them with [`FakeVideoSource`].
//!
//! YUV→RGBA8 conversion is pure Rust ([`yuv`]): the frame arrives from the
//! codec as a padded YUV420 ByteBuffer, and we convert it ourselves instead of
//! going through `AHardwareBuffer`/external Vulkan memory (see docs/08 §8.4 for
//! that trade-off).

pub mod yuv;

mod android;

use std::collections::VecDeque;
use std::fmt;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::sync::Arc;

/// Largest number of decoded frames one source keeps resident.
///
/// A frame count, not the bound that matters: see [`FRAME_CACHE_BUDGET`].
pub const FRAME_CACHE_CAP: usize = 8;

/// Bytes one source may keep resident in decoded frames.
///
/// The bound used to be a flat [`FRAME_CACHE_CAP`] frames, which is 63 MiB per
/// video layer at 1080p — pixels nobody rewrites, held for the sake of a
/// cache hit nobody always takes. Frames are now held rather than copied (see
/// [`VideoFrame::rgba`]), so `cache.len() * frame_bytes` *is* the working set,
/// and the bound is spent in bytes: a 1080p layer keeps one frame plus a
/// one-frame window (15.8 MiB), a 720p layer keeps four (13.2 MiB), and a small
/// frame still gets the full [`FRAME_CACHE_CAP`]. A frame bigger than the whole
/// budget is cached on its own rather than refused — a 4K clip would otherwise
/// never cache anything, and at end of stream the one cached frame is the only
/// frame there is.
pub const FRAME_CACHE_BUDGET: usize = 16 * 1024 * 1024;

/// Owned RGBA8 storage of `len` bytes that has deliberately **not** been
/// zeroed.
///
/// [`yuv::yuv_to_rgba`] writes every byte of the destination it is handed: it
/// rejects a bad geometry before the first write, and its `emit!` loop then
/// covers `out_w * out_h` pixels with no early exit (`debug_assert_eq!(idx,
/// needed)` checks exactly that). The `vec![0u8; len]` this replaces — 8 MiB of
/// memset per decoded frame at 1080p — therefore had every one of its bytes
/// overwritten a moment later.
///
/// `u8` has no uninitialised value, so the bytes travel as `MaybeUninit` until
/// [`init_rgba`] hands the destination to the converter and [`filled_rgba`]
/// turns it back into readable ones.
// The three helpers below are reached from `android.rs`, which is
// `cfg(target_os = "android")` and therefore not compiled on the host, and from
// the `yuv.rs` test that proves the uninitialised destination behaves exactly
// like the zeroed one it replaced. On a host build both callers are gone and
// Rust would call them dead, which is the compiler being right about this
// target and wrong about the code.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn uninit_rgba(len: usize) -> Vec<MaybeUninit<u8>> {
    let mut buf = Vec::<MaybeUninit<u8>>::with_capacity(len);
    // SAFETY: `MaybeUninit<u8>` has no validity invariant — any bit pattern is
    // a valid `MaybeUninit<u8>` — so extending the length over the capacity
    // this `Vec` owns exposes uninitialised *bytes* and nothing more. Nothing
    // can read them as `u8` until `filled_rgba` has been given a buffer the
    // converter wrote end to end.
    unsafe { buf.set_len(len) };
    buf
}

/// Borrow `buf` as the `u8` destination [`yuv::yuv_to_rgba`] writes into.
///
/// # Safety
/// Every byte of `buf` must be written by the conversion before anything reads
/// it. `yuv_to_rgba` does write every byte (see [`uninit_rgba`]); a caller that
/// hands this a buffer the conversion leaves partly untouched gets undefined
/// behaviour.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) unsafe fn init_rgba(buf: &mut [MaybeUninit<u8>]) -> &mut [u8] {
    // SAFETY: the caller promised every byte of `buf` is written before it is
    // read, and `MaybeUninit<u8>` and `u8` are layout-identical, so the cast
    // exposes nothing that was not already there; `&mut *` keeps the borrow
    // exclusive for as long as the returned slice lives.
    unsafe { &mut *(buf as *mut [MaybeUninit<u8>] as *mut [u8]) }
}

/// The readable, shared pixels of a buffer [`uninit_rgba`] promised.
///
/// # Safety
/// Every byte of `buf` must already be written.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) unsafe fn filled_rgba(buf: Vec<MaybeUninit<u8>>) -> Arc<[u8]> {
    // SAFETY: `Vec<MaybeUninit<u8>>` and `Vec<u8>` have the same layout, and
    // the caller guarantees every element is initialised, so each `u8` the
    // pointer covers is a real value rather than an uninitialised byte.
    let buf: Vec<u8> = unsafe { std::mem::transmute(buf) };
    // `Arc<[T]>: From<Vec<T>>` moves the elements behind a new header; it does
    // not copy them out of the buffer they already live in.
    Arc::from(buf)
}

/// Metadata of a decoded clip. Dimensions are the **visible** (cropped and
/// rotation-applied) size, matching what [`VideoFrame::rgba`] contains.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// Visible width after rotation.
    pub width: u32,
    /// Visible height after rotation.
    pub height: u32,
    /// Clip duration in milliseconds (`0` when unknown).
    pub duration_ms: u64,
    /// Nominal frame rate; `0.0` when the container does not report one.
    pub fps: f32,
    /// Clockwise rotation applied during conversion: `0`, `90`, `180` or `270`.
    pub rotation_deg: u16,
    /// Whether the container also holds at least one audio track.
    pub has_audio: bool,
}

/// One decoded frame, upright and top-left origin.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Frame width in pixels, as the decoder actually produced it.
    ///
    /// Carried per frame rather than taken from [`VideoInfo`]: the probe reads
    /// the track header and the decoder reads the output buffer, and the two
    /// disagree whenever rotation or cropping is in play. Validating the pixel
    /// count against the probe made a cosmetic disagreement between them look
    /// exactly like "this clip has no picture", silently, for every frame of
    /// every clip.
    pub width: u32,
    /// Frame height in pixels; `width * height * 4 == rgba.len()`.
    pub height: u32,
    /// The frame as YUV planes: the decoder's own buffers, shared by refcount.
    ///
    /// `Some` on a decoder that can hand over its planes, `None` when it cannot
    /// (the fake in tests, the audio-track path).
    planes: Option<Arc<crate::video::yuv::YuvPlanes>>,
    /// RGBA8 pixels, converted **on first use** — see [`VideoFrame::rgba`].
    rgba: std::sync::OnceLock<Arc<[u8]>>,
    /// Presentation time of the frame in the source, in milliseconds.
    pub time_ms: i64,
}

impl VideoFrame {
    /// A frame that arrived as planes. RGBA is converted on demand.
    ///
    /// This is the shape a decoded frame has, and the reason it is the shape:
    /// the conversion is 2.07M pixels of f32 arithmetic at 1080p, and the
    /// compositor samples the planes on the GPU. Converting here — on every
    /// decoded frame, for pixels nobody reads — is what made a video layer cost
    /// several times its own decode, in the preview and in every exported frame
    /// alike.
    pub fn from_planes(
        width: u32,
        height: u32,
        time_ms: i64,
        planes: Arc<crate::video::yuv::YuvPlanes>,
    ) -> Self {
        Self {
            width,
            height,
            planes: Some(planes),
            rgba: std::sync::OnceLock::new(),
            time_ms,
        }
    }

    /// A frame that arrived as pixels: an image source, or the fake in tests.
    pub fn from_rgba(width: u32, height: u32, time_ms: i64, rgba: Arc<[u8]>) -> Self {
        let cell = std::sync::OnceLock::new();
        let _ = cell.set(rgba);
        Self {
            width,
            height,
            planes: None,
            rgba: cell,
            time_ms,
        }
    }

    /// The frame's planes, when it has any.
    pub fn planes(&self) -> Option<&Arc<crate::video::yuv::YuvPlanes>> {
        self.planes.as_ref()
    }

    /// RGBA8 pixels, alpha always `255`.
    ///
    /// Converts from the planes the first time it is asked and keeps the result,
    /// so a caller that wants pixels pays once and a caller that wants samples
    /// pays nothing. `None` when the frame has neither pixels nor planes.
    ///
    /// Shared rather than owned: [`DecodingSource`] keeps the frame covering a
    /// time in its cache and hands the caller the very same pixels, so a cache
    /// hit is a refcount bump instead of an 8 MB memcpy per video layer per
    /// frame.
    pub fn rgba(&self) -> Option<Arc<[u8]>> {
        if let Some(pixels) = self.rgba.get() {
            return Some(Arc::clone(pixels));
        }
        let planes = self.planes.as_ref()?;
        let pixels = crate::video::yuv::rgba_from_planes(planes).ok()?;
        // A racing caller may have filled the cell first; either result is the
        // same pixels, and the loser's copy is dropped.
        let _ = self.rgba.set(Arc::clone(&pixels));
        self.rgba.get().map(Arc::clone)
    }

    /// Bytes this frame actually holds, for the cache's budget.
    ///
    /// The planes plus whatever has been converted so far — *not* the nominal
    /// `width * height * 4`, which would charge every frame for pixels that may
    /// never exist. The budget bounds what is held, and at 1080p this is 3 MiB a
    /// frame instead of 7.9, so the same 16 MiB window now reaches further and
    /// the preview re-decodes less.
    pub fn bytes(&self) -> usize {
        self.planes.as_ref().map_or(0, |p| p.bytes().len())
            + self.rgba.get().map_or(0, |p| p.len())
    }
}

/// Two frames are the same when they carry the same pixels at the same instant.
///
/// Hand-written rather than derived: the plane payload is an `Arc` with no
/// ordering, and the tests that use this compare a cached frame with the one
/// just decoded — which is exactly "the same picture, reached again".
impl PartialEq for VideoFrame {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.time_ms == other.time_ms
            && self.rgba == other.rgba
    }
}

impl Eq for VideoFrame {}

/// Errors returned by [`open_video_fd`], [`probe_video_fd`] and
/// [`VideoSource::frame_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    /// The operation is not available in this build (non-Android).
    Unsupported(&'static str),
    /// The container could not be opened or configured for decoding.
    Open(String),
    /// A decoded buffer could not be produced or converted.
    Decode(String),
    /// Repositioning to a requested presentation time failed.
    Seek(String),
    /// The container holds no audio track — an ordinary state for a silent
    /// clip, which is why it is a variant of its own rather than an
    /// [`VideoError::Open`] message the caller has to string-match.
    NoAudioTrack,
    /// [`DecodingSource::close`] was called.
    Closed,
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "video decode unsupported: {what}"),
            Self::Open(what) => write!(f, "video open failed: {what}"),
            Self::Decode(what) => write!(f, "video decode failed: {what}"),
            Self::Seek(what) => write!(f, "video seek failed: {what}"),
            Self::NoAudioTrack => f.write_str("container holds no audio track"),
            Self::Closed => f.write_str("video source is closed"),
        }
    }
}

impl std::error::Error for VideoError {}

/// A seekable video clip that yields upright RGBA8 frames.
pub trait VideoSource {
    /// Metadata; dimensions already include the rotation.
    fn info(&self) -> VideoInfo;
    /// The frame shown at `time_ms`, in milliseconds from the start.
    ///
    /// Idempotent for a time still inside the cached window (bounded by
    /// [`FRAME_CACHE_BUDGET`]), clamps to `0..=duration_ms`, and decodes
    /// forward from the previous position when only a small step ahead is
    /// requested.
    ///
    /// By handle, not by value: the answer is the very same frame the cache
    /// holds, so a cache hit is a refcount bump rather than a copy of the
    /// pixels. The handle *is* the sharing contract — the cache and the caller
    /// alias one [`VideoFrame`] — and an `Arc<[u8]>` inside it makes the pixels
    /// immutable to both (see [`VideoFrame::rgba`]).
    fn frame_at(&mut self, time_ms: i64) -> Result<Arc<VideoFrame>, VideoError>;
}

/// The byte count to hand the extractor for a descriptor.
///
/// Pure on purpose, so the rule can be tested without a device — this arithmetic
/// is the whole bug, and it took four build cycles on a phone to find it.
///
/// On the NDK path `AMediaExtractor_setDataSourceFd` does **not** treat a
/// negative length as "to the end". `FileSource` (libdatasource) does
///
/// ```c
/// if (mLength < 0) { mLength = 0; }
/// ```
///
/// and its `readAt` then returns 0 for every read, because
/// `offset >= mLength`. Sniffing never sees a byte, the extractor factory
/// returns NULL, `NuMediaExtractor` says `ERROR_UNSUPPORTED`, and
/// `NdkMediaErrorPriv.cpp` has no case for that — so it falls through to
/// `AMEDIA_ERROR_UNKNOWN` (-10000). That is the exact shape of every failure
/// reported from a device: a perfectly valid MP4, a descriptor that can seek,
/// and an answer that names neither the descriptor nor the container.
///
/// So "the whole descriptor" is resolved **here**, from the descriptor's own
/// size, and never passed down as a sentinel. A caller that states a positive
/// length is believed; anything else means "the rest of the file".
pub fn resolve_data_source_length(
    declared: i64,
    offset: i64,
    file_size: i64,
) -> Result<i64, String> {
    let start = offset.max(0);
    if start >= file_size {
        return Err(format!(
            "the range starts at {start} but the descriptor is {file_size} bytes"
        ));
    }
    let available = file_size - start;
    if declared > 0 {
        // A caller that knows its own length may name a prefix of the file, but
        // never more than the file has: past the end there is nothing to read.
        if declared > available {
            return Err(format!(
                "the caller asked for {declared} bytes but only {available} are there"
            ));
        }
        return Ok(declared);
    }
    Ok(available)
}

/// Whether `fd` can seek, which is what `AMediaExtractor_setDataSourceFd`
/// requires of it.
///
/// Lives outside `android.rs` on purpose: that module is `cfg(target_os =
/// "android")` and its tests never run on the host, and this is the question
/// that decides whether a clip can be opened at all. It is answered from the
/// JNI boundary, where every platform goes through it.
pub fn fd_is_seekable(fd: RawFd) -> bool {
    #[cfg(target_os = "android")]
    {
        // SEEK_CUR by zero moves nothing and fails only when the descriptor
        // cannot seek at all, so the position is untouched either way.
        unsafe { ndk_sys::lseek(fd, 0, ndk_sys::SEEK_CUR as i32) >= 0 }
    }
    #[cfg(not(target_os = "android"))]
    {
        // No `ndk_sys::ffi` off Android, but std's descriptors are the same
        // POSIX ones, so a seek answers the question without a libc dependency.
        // `ManuallyDrop` because the caller still owns the descriptor: wrapping
        // it in a `File` would otherwise close it on drop.
        use std::io::Seek;
        use std::os::fd::FromRawFd;
        let mut file =
            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.stream_position().is_ok()
    }
}

/// Probe a clip held in `fd` (e.g. an Android `ParcelFileDescriptor`) and
/// return its metadata without building a decoder.
///
/// `offset`/`length` pick a sub-range of the descriptor (`length` of `-1`
/// means "to the end"). The caller keeps ownership of `fd`: the Android
/// implementation duplicates it internally.
///
/// # Errors
/// [`VideoError::Unsupported`] on non-Android builds.
/// Probe a clip held in `fd` on a non-Android build.
///
/// # Errors
/// Always [`VideoError::Unsupported`]: there is no video backend here.
#[cfg(not(target_os = "android"))]
pub fn probe_video_fd(_fd: RawFd, _offset: i64, _length: i64) -> Result<VideoInfo, VideoError> {
    Err(VideoError::Unsupported(NEEDS_ANDROID))
}

/// Probe a clip held in `fd`; see the non-Android variant for the contract.
///
/// # Errors
/// [`VideoError::Open`] when the container cannot be read or has no video
/// track.
#[cfg(target_os = "android")]
pub fn probe_video_fd(fd: RawFd, offset: i64, length: i64) -> Result<VideoInfo, VideoError> {
    android::probe_video_fd(fd, offset, length)
}

/// Open a clip held in `fd` for frame-accurate decoding.
///
/// The returned source owns an `AMediaExtractor` + `AMediaCodec` pair on
/// Android; `fd` itself stays owned by the caller.
///
/// # Errors
/// [`VideoError::Unsupported`] on non-Android builds, [`VideoError::Open`]
/// when the track cannot be decoded.
#[cfg(not(target_os = "android"))]
pub fn open_video_fd(_fd: RawFd, _offset: i64, _length: i64) -> Result<Box<dyn VideoSource>, VideoError> {
    Err(VideoError::Unsupported(NEEDS_ANDROID))
}

/// Open a clip held in `fd`; see the non-Android variant for the contract.
#[cfg(target_os = "android")]
pub fn open_video_fd(fd: RawFd, offset: i64, length: i64) -> Result<Box<dyn VideoSource>, VideoError> {
    android::open_video_fd(fd, offset, length)
}

/// Decode the first audio track of a clip into interleaved `f32` PCM.
///
/// Same container, same `AMediaExtractor`, opposite track: the sound of a video
/// file used to be dropped on the floor because the decoder only ever selected
/// the video track. `fd`/`offset`/`length` mean what they mean for
/// [`open_video_fd`], and the caller keeps ownership of `fd`.
///
/// # Errors
/// [`VideoError::Unsupported`] on non-Android builds,
/// [`VideoError::NoAudioTrack`] when the file carries no sound (an ordinary
/// state, not a failure), [`VideoError::Open`] / [`VideoError::Decode`] when the
/// track cannot be decoded.
#[cfg(not(target_os = "android"))]
pub fn decode_audio_track_fd(
    _fd: RawFd,
    _offset: i64,
    _length: i64,
) -> Result<crate::audio::DecodedAudio, VideoError> {
    Err(VideoError::Unsupported(NEEDS_ANDROID))
}

/// Decode the first audio track of a clip; see the non-Android variant for the
/// contract.
#[cfg(target_os = "android")]
pub fn decode_audio_track_fd(
    fd: RawFd,
    offset: i64,
    length: i64,
) -> Result<crate::audio::DecodedAudio, VideoError> {
    android::decode_audio_track_fd(fd, offset, length)
}

/// Decode a clip's audio track and hand it to the mixer as one source.
///
/// The shortcut the exporter needs: the sound of a `MEDIA` clip becomes an
/// [`AudioSource`](crate::mixer::AudioSource) at `start_ms`, with `duration_ms`
/// (`0` = the whole track) and `gain` applied there rather than at encode time,
/// so the mixed stream stays monotonic in presentation time.
///
/// # Errors
/// The same set as [`decode_audio_track_fd`].
pub fn audio_source_fd(
    fd: RawFd,
    offset: i64,
    length: i64,
    start_ms: i64,
    duration_ms: i64,
    gain: f32,
) -> Result<crate::mixer::AudioSource, VideoError> {
    let audio = decode_audio_track_fd(fd, offset, length)?;
    Ok(crate::mixer::AudioSource::new(
        audio,
        start_ms,
        duration_ms,
        gain,
    ))
}

#[cfg(not(target_os = "android"))]
const NEEDS_ANDROID: &str = "video decoding needs Android MediaCodec/MediaExtractor";

/// Backend contract behind [`DecodingSource`].
///
/// Implemented by the Android `MediaCodec` backend and by [`FakeVideoSource`].
/// It is deliberately narrow: the decoder only knows how to produce the next
/// frame in presentation order and how to reposition, while caching, clamping
/// and the forward-decode policy stay platform-neutral.
///
/// Not part of the supported API surface.
#[doc(hidden)]
pub trait FrameDecoder {
    /// Decode the next frame in presentation order, already converted to
    /// upright RGBA8. `Ok(None)` means end of stream.
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError>;

    /// Reposition to the sync sample at or before `time_ms`, so that the next
    /// [`FrameDecoder::next_frame`] call yields the first frame at or after
    /// that sync sample.
    fn seek(&mut self, time_ms: i64) -> Result<(), VideoError>;
}

/// Platform-neutral driver: frame cache, forward decode and seek policy.
///
/// The answer for `t` is the frame that *covers* it: the decoded frame with the
/// largest presentation time `<= t` (or the earliest frame, when `t` precedes
/// the clip). The cached run is contiguous in presentation time, so
/// [`DecodingSource::cached_answer`] can serve `t` whenever
/// `front <= t <= back` without touching the decoder.
///
/// * A request before the cached front re-seeks — the window is bounded by
///   [`FRAME_CACHE_BUDGET`], so the front is evicted while decoding forward and
///   a backward request usually falls out of the window.
/// * A request past the back keeps decoding forward, which is bounded by the
///   codec rather than by the cache: a one-frame window still walks a whole
///   clip without re-seeking.
/// * At end of stream the closest frame held in the cache is returned, which
///   implements the clamp for requests past the clip end.
///
/// Nothing here copies pixels. A decoded frame is wrapped in an `Arc` exactly
/// once, on its way into the cache, and every later request for that time is
/// handed the same handle. So a frame enters the cache once and leaving it —
/// on a cache hit, at end of stream, or straight off the decoder — costs a
/// refcount bump and nothing else.
pub struct DecodingSource<D: FrameDecoder> {
    info: VideoInfo,
    decoder: D,
    cache: VecDeque<Arc<VideoFrame>>,
    /// Bytes held by `cache`; the bound is spent rather than counted.
    resident: usize,
    eos: bool,
    closed: bool,
}

impl<D: FrameDecoder> DecodingSource<D> {
    /// Wrap a decoder. `info` must describe the frames the decoder returns.
    pub fn new(info: VideoInfo, decoder: D) -> Self {
        Self {
            info,
            decoder,
            cache: VecDeque::new(),
            resident: 0,
            eos: false,
            closed: false,
        }
    }

    /// Release the source; later [`VideoSource::frame_at`] calls fail with
    /// [`VideoError::Closed`].
    pub fn close(&mut self) {
        self.closed = true;
        self.forget_cache();
    }

    /// Number of frames currently cached (never more than [`FRAME_CACHE_CAP`],
    /// and never more bytes than [`FRAME_CACHE_BUDGET`]).
    pub fn cached_frames(&self) -> usize {
        self.cache.len()
    }

    /// Bytes of decoded pixels currently cached, for diagnostics and tests.
    pub fn resident_bytes(&self) -> usize {
        self.resident
    }

    /// The wrapped decoder, for tests and diagnostics.
    #[doc(hidden)]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }

    fn forget_cache(&mut self) {
        self.cache.clear();
        self.resident = 0;
    }

    /// Cache `frame` and hand the same frame back by handle, so a caller that
    /// needs the pixels never has to go looking for them in the cache
    /// afterwards.
    ///
    /// The `Arc` is created here, once, and this is the only place a frame is
    /// ever wrapped: the cache and the answer alias it. That is why the frame
    /// is not cloned on the way in — the previous `handle = frame.clone()` was
    /// a whole struct copy per insert, and would become a 7.9 MiB pixel copy
    /// the moment anything reached for the pixels again.
    ///
    /// Eviction is from the front and spends [`FRAME_CACHE_BUDGET`] in bytes,
    /// not [`FRAME_CACHE_CAP`] in frames: a 1080p frame is 7.9 MiB, so eight of
    /// them are 63 MiB per video layer and a frame-count bound cannot see that.
    /// A frame past the whole budget is cached alone rather than refused.
    fn push(&mut self, frame: VideoFrame) -> Arc<VideoFrame> {
        let bytes = frame.bytes();
        while !self.cache.is_empty()
            && (self.cache.len() >= FRAME_CACHE_CAP
                || self.resident.saturating_add(bytes) > FRAME_CACHE_BUDGET)
        {
            if let Some(evicted) = self.cache.pop_front() {
                self.resident -= evicted.bytes();
            }
        }
        self.resident += bytes;
        let handle = Arc::new(frame);
        self.cache.push_back(Arc::clone(&handle));
        handle
    }

    /// `true` when the cached window is too early (or empty), i.e. the decoder
    /// must be repositioned before it can answer `t`.
    fn needs_seek(&self, t: i64) -> bool {
        match self.cache.front() {
            Some(front) => t < front.time_ms,
            None => true,
        }
    }

    /// The cached frame covering `t`, if the window reaches past it.
    ///
    /// Valid without touching the decoder only while `t <= back.time_ms`: then
    /// every frame with a presentation time in `[t, back]` is cached, so the
    /// last cached frame at or before `t` is the one shown at `t`.
    ///
    /// Cloning an `Arc` is a refcount bump, which is the whole point: the frame
    /// stays in the cache and the caller gets the same one.
    fn cached_answer(&self, t: i64) -> Option<Arc<VideoFrame>> {
        let back = self.cache.back()?;
        if t > back.time_ms {
            return None;
        }
        self.cache.iter().rev().find(|f| f.time_ms <= t).cloned()
    }
}

impl<D: FrameDecoder> VideoSource for DecodingSource<D> {
    fn info(&self) -> VideoInfo {
        self.info
    }

    fn frame_at(&mut self, time_ms: i64) -> Result<Arc<VideoFrame>, VideoError> {
        if self.closed {
            return Err(VideoError::Closed);
        }
        // `duration_ms == 0` means "unknown", not "zero length" (see the
        // `VideoInfo::duration_ms` contract). Clamping against a zero upper
        // bound would pin every request to frame 0 and freeze the whole clip.
        let t = match self.info.duration_ms {
            0 => time_ms.max(0),
            ms => time_ms.clamp(0, i64::try_from(ms).unwrap_or(i64::MAX)),
        };

        if self.needs_seek(t) {
            // First request, or backwards past the cached window. The seek
            // lands on a sync sample, so decoding restarts there.
            self.decoder.seek(t)?;
            self.forget_cache();
            self.eos = false;
        } else {
            if let Some(frame) = self.cached_answer(t) {
                return Ok(frame);
            }
            if self.eos {
                // Drained: the cached tail is the closest frame there is.
                return self
                    .cache
                    .back()
                    .cloned()
                    .ok_or_else(|| VideoError::Decode(format!("no frame near {t}ms")));
            }
        }

        // Decode forward until a frame past `t` arrives: `cover` is then the
        // frame shown at `t`. The decoder's frame goes straight into the cache
        // and comes back out as the same handle, so a decoded frame is never
        // copied — only counted.
        let mut cover: Option<Arc<VideoFrame>> = None;
        loop {
            let Some(frame) = self.decoder.next_frame()? else {
                self.eos = true;
                return cover.or_else(|| self.cache.back().cloned()).ok_or_else(|| {
                    VideoError::Decode(format!("stream holds no frame near {t}ms"))
                });
            };
            let past_t = frame.time_ms > t;
            let frame = self.push(frame);
            if past_t {
                return Ok(cover.unwrap_or(frame));
            }
            cover = Some(frame);
        }
    }
}

#[cfg(test)]
mod lazy_rgba_tests {
    use super::*;
    use crate::video::yuv::{Matrix, Range, YuvFormat, YuvLayout, YuvPlanes};

    /// A plane-backed frame must not convert until something asks for pixels —
    /// that is the whole point of the shape — and when it does, it must produce
    /// the same pixels the eager conversion produced.
    ///
    /// This is the test that pins the fix for the video path's real cost: the
    /// decoder used to run `yuv_to_rgba` on every decoded frame, for pixels the
    /// compositor never read.
    #[test]
    fn a_plane_frame_converts_lazily_and_identically() {
        let (w, h) = (4u32, 4u32);
        let (stride, slice) = (4usize, 4usize);
        // A grey NV12 frame: luma 128, chroma neutral. Not black, so a converter
        // that read the wrong plane or the wrong offset would show up.
        let mut bytes = vec![128u8; stride * slice];
        bytes.extend(std::iter::repeat_n(128u8, stride * slice / 2));
        let planes = Arc::new(
            YuvPlanes::new(
                YuvFormat::Nv12,
                YuvLayout {
                    stride,
                    slice_height: slice,
                },
                w,
                h,
                Matrix::Bt601,
                Range::Full,
                0,
                Arc::<[u8]>::from(bytes.as_slice()),
            )
            .expect("a valid NV12 frame"),
        );

        let frame = VideoFrame::from_planes(w, h, 0, Arc::clone(&planes));
        // Nothing has been converted: the frame holds the decoded buffer and
        // nothing else.
        assert_eq!(
            frame.bytes(),
            planes.bytes().len(),
            "a plane frame must not convert on construction"
        );

        let rgba = frame.rgba().expect("pixels on demand");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(
            frame.bytes() > planes.bytes().len(),
            "the pixels are held once they have been asked for"
        );

        // Byte for byte what the eager path produced for the same planes.
        let expected = crate::video::yuv::rgba_from_planes(&planes).expect("reference conversion");
        assert_eq!(rgba, expected);

        // Asking twice converts once: the second answer is the same allocation.
        let again = frame.rgba().expect("cached");
        assert!(Arc::ptr_eq(&rgba, &again), "the conversion is kept, not redone");

        // And a frame that carries pixels outright — the fake, an image source —
        // answers immediately without planes to convert from.
        let pixel_frame = VideoFrame::from_rgba(1, 1, 0, Arc::<[u8]>::from([1u8, 2, 3, 255]));
        assert_eq!(pixel_frame.rgba().unwrap()[1], 2);
        assert_eq!(pixel_frame.bytes(), 4);
    }
}

/// Synthetic clip used to exercise the seek/cache policy on the host.
///
/// Yields `frames` solid frames spaced `1000 / fps` ms apart, starting at 0.
/// Fractional frame steps are rounded, so pick an `fps` that divides 1000
/// (10 fps ⇒ 100 ms). The first pixel of frame `i` is `i as u8`, which lets
/// tests tell frames apart. Counters expose how much work the policy caused.
#[doc(hidden)]
pub struct FakeVideoSource {
    inner: DecodingSource<FakeDecoder>,
}

/// Decoder half of [`FakeVideoSource`].
#[doc(hidden)]
#[derive(Debug)]
pub struct FakeDecoder {
    times: Vec<i64>,
    next: usize,
    sync_interval: usize,
    width: u32,
    height: u32,
    seeks: usize,
    decodes: usize,
}

impl FakeDecoder {
    fn new(times: Vec<i64>, width: u32, height: u32) -> Self {
        Self {
            times,
            next: 0,
            sync_interval: 1,
            width,
            height,
            seeks: 0,
            decodes: 0,
        }
    }

    /// Frames actually handed out by [`FrameDecoder::next_frame`].
    pub fn decodes(&self) -> usize {
        self.decodes
    }

    /// Calls to [`FrameDecoder::seek`].
    pub fn seeks(&self) -> usize {
        self.seeks
    }

    fn frame_pixels(&self, index: usize) -> Vec<u8> {
        let mut rgba = vec![0u8; (self.width * self.height * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = index as u8;
            px[1] = 0x11;
            px[2] = 0x22;
            px[3] = 255;
        }
        rgba
    }
}

impl FrameDecoder for FakeDecoder {
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        if self.next >= self.times.len() {
            return Ok(None);
        }
        let index = self.next;
        self.next += 1;
        self.decodes += 1;
        Ok(Some(VideoFrame {
            width: self.width,
            height: self.height,
            rgba: std::sync::OnceLock::from(Arc::from(self.frame_pixels(index))),
            // The fake paints a synthetic colour, not a YUV plane, so there is
            // nothing honest to hand the GPU path here.
            planes: None,
            time_ms: self.times[index],
        }))
    }

    fn seek(&mut self, time_ms: i64) -> Result<(), VideoError> {
        self.seeks += 1;
        // Like AMediaExtractor with SEEK_PREVIOUS_SYNC: land on the sync
        // sample at or before `t`, so a following decode walk starts there and
        // still has to run forward to reach `t`. `sync_interval` emulates a GOP.
        let before = self.times.partition_point(|&t| t <= time_ms);
        let last = before.saturating_sub(1);
        self.next = last - (last % self.sync_interval);
        Ok(())
    }
}

impl FakeVideoSource {
    /// A clip of `frames` frames at `fps`.
    pub fn new(fps: f32, frames: usize) -> Self {
        Self::with_size(fps, frames, 4, 4)
    }

    /// A clip of `frames` frames at `fps`, with explicit frame dimensions.
    pub fn with_size(fps: f32, frames: usize, width: u32, height: u32) -> Self {
        let step = (1000.0 / f64::from(fps)).round().max(1.0) as i64;
        let times: Vec<i64> = (0..frames as i64).map(|i| i * step).collect();
        let duration_ms = (frames as i64 * step) as u64;
        let info = VideoInfo {
            width,
            height,
            duration_ms,
            fps,
            rotation_deg: 0,
            has_audio: false,
        };
        Self {
            inner: DecodingSource::new(info, FakeDecoder::new(times, width, height)),
        }
    }

    /// Emulate a GOP: only every `interval`-th frame is a sync sample.
    pub fn with_sync_interval(mut self, interval: usize) -> Self {
        self.inner.decoder.sync_interval = interval.max(1);
        self
    }

    /// Frames handed out by the decoder so far.
    pub fn decodes(&self) -> usize {
        self.inner.decoder.decodes()
    }

    /// Seeks performed so far.
    pub fn seeks(&self) -> usize {
        self.inner.decoder.seeks()
    }

    /// Frames currently cached.
    pub fn cached_frames(&self) -> usize {
        self.inner.cached_frames()
    }

    /// Bytes of decoded pixels currently cached.
    pub fn resident_bytes(&self) -> usize {
        self.inner.resident_bytes()
    }

    /// Release the fake; further [`VideoSource::frame_at`] calls fail.
    pub fn close(&mut self) {
        self.inner.close();
    }
}

impl VideoSource for FakeVideoSource {
    fn info(&self) -> VideoInfo {
        self.inner.info()
    }

    fn frame_at(&mut self, time_ms: i64) -> Result<Arc<VideoFrame>, VideoError> {
        self.inner.frame_at(time_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 fps ⇒ frames 100 ms apart; the first pixel equals the frame index.
    fn fake(frames: usize) -> FakeVideoSource {
        FakeVideoSource::new(10.0, frames)
    }

    #[test]
    fn repeated_request_decodes_once() {
        let mut src = fake(10); // frames at 0..900, 100 ms apart
        // 250 ms is covered by the frame at 200 ms; the seek lands on the
        // previous sync sample and decoding walks forward from there.
        let first = src.frame_at(250).unwrap();
        assert_eq!(first.time_ms, 200);
        assert_eq!(first.rgba().unwrap()[0], 2);
        assert_eq!(first.rgba().unwrap().len(), 4 * 4 * 4);
        assert_eq!(src.decodes(), 2); // 200, then 300 to prove coverage
        assert_eq!(src.seeks(), 1);

        let again = src.frame_at(250).unwrap();
        assert_eq!(again.rgba, first.rgba);
        assert_eq!(again.time_ms, first.time_ms);
        assert_eq!(src.decodes(), 2, "cache hit must not decode again");
        assert_eq!(src.seeks(), 1, "cache hit must not re-seek");
    }

    #[test]
    fn forward_request_decodes_monotonically() {
        let mut src = fake(20);
        let a = src.frame_at(0).unwrap();
        assert_eq!(a.time_ms, 0);
        assert_eq!(a.rgba().unwrap()[0], 0);
        assert_eq!(src.decodes(), 2); // 0 and 100

        let b = src.frame_at(450).unwrap();
        assert_eq!(b.time_ms, 400);
        assert!(b.time_ms > a.time_ms);
        assert_eq!(src.decodes(), 6, "0,100,200,300,400,500");
        assert_eq!(src.seeks(), 1, "a forward step must not re-seek");

        // Stepping back inside the still-cached window is a pure cache hit.
        let c = src.frame_at(120).unwrap();
        assert_eq!(c.time_ms, 100);
        assert_eq!(src.decodes(), 6);
        assert_eq!(src.seeks(), 1);
    }

    #[test]
    fn backward_request_reseeks_and_cache_is_bounded() {
        let mut src = fake(40); // 4 s
        src.frame_at(0).unwrap();

        // Far forward without a seek: the cache cannot cover it, so the
        // decoder walks 200..3100 ms and must stay within the bound.
        let far = src.frame_at(3000).unwrap();
        assert_eq!(far.time_ms, 3000);
        assert_eq!(far.rgba().unwrap()[0], 30);
        assert_eq!(src.seeks(), 1, "forward walking must not re-seek");
        assert_eq!(
            src.cached_frames(),
            FRAME_CACHE_CAP,
            "cache must stop at the bound"
        );

        // 150 ms is older than the cached front: must re-seek.
        let back = src.frame_at(150).unwrap();
        assert_eq!(back.time_ms, 100);
        assert_eq!(src.seeks(), 2, "backward past the window must re-seek");
        assert!(src.cached_frames() <= FRAME_CACHE_CAP);
    }

    #[test]
    fn request_past_end_clamps_to_last_frame() {
        let mut src = fake(5); // 0,100,200,300,400; duration 500
        assert_eq!(src.info().duration_ms, 500);
        assert_eq!(src.info().width, 4);
        assert_eq!(src.info().height, 4);
        assert_eq!(src.info().fps, 10.0);
        assert!(!src.info().has_audio);

        let last = src.frame_at(99_999).unwrap();
        assert_eq!(last.time_ms, 400, "clamped to the end of the stream");
        assert_eq!(last.rgba().unwrap()[0], 4);
        let decoded = src.decodes();

        // Idempotent at the end of stream too: no extra decode, no extra seek.
        let again = src.frame_at(99_999).unwrap();
        assert_eq!(again, last);
        assert_eq!(src.decodes(), decoded);
        assert_eq!(src.seeks(), 1);
    }

    #[test]
    fn negative_time_clamps_to_zero() {
        let mut src = fake(5);
        let f = src.frame_at(-1_000).unwrap();
        assert_eq!(f.time_ms, 0);
        assert_eq!(src.seeks(), 1);
    }

    #[test]
    fn forward_walk_never_exceeds_the_cache_bound() {
        let mut src = fake(100);
        for t in (0..10_000).step_by(100) {
            src.frame_at(t).unwrap();
            assert!(
                src.cached_frames() <= FRAME_CACHE_CAP,
                "cache grew to {} at {t}ms",
                src.cached_frames()
            );
        }
    }

    #[test]
    fn sync_sample_gop_still_finds_the_requested_frame() {
        // Only every 5th frame is a sync sample (a 500 ms GOP at 10 fps).
        let mut src = fake(40).with_sync_interval(5);
        let f = src.frame_at(2600).unwrap();
        assert_eq!(f.time_ms, 2600);
        // The decoder restarts at the sync sample 2500 and walks forward.
        let again = src.frame_at(2600).unwrap();
        assert_eq!(again, f);
    }

    #[test]
    fn a_cached_frame_is_handed_out_not_copied() {
        let mut src = fake(10);
        let first = src.frame_at(250).unwrap();
        // The point of `Arc<VideoFrame>`: the covering frame stays in the cache
        // and the caller gets the very same allocation — one decode, one frame,
        // one set of pixels. Two handles into it. Before this the cache held the
        // frame by value and every insert and every answer copied it.
        let again = src.frame_at(250).unwrap();
        assert_eq!(again, first);
        assert!(
            Arc::ptr_eq(&first, &again),
            "a cache hit must hand back the cached frame itself, not a copy"
        );
        // Still exactly the pixels the decoder produced, readable through the
        // handle rather than through a copied `Vec`.
        assert_eq!(first.rgba().unwrap()[0], 2);
        assert_eq!(first.rgba().unwrap().len(), 4 * 4 * 4);
    }

    /// The handle is what the cache hands out, so a frame that has been evicted
    /// keeps its pixels alive for whoever still holds it. A caller holding a
    /// handle is the reason a frame smaller than the budget can be dropped
    /// from the cache without the bytes it points at going with it.
    #[test]
    fn an_evicted_frame_stays_alive_for_the_handle_that_asked_for_it() {
        let mut src = FakeVideoSource::with_size(10.0, 40, 1920, 1080);
        // One 1080p frame plus a one-frame window fits the byte budget.
        let held = src.frame_at(500).unwrap();
        for t in (1_000..5_000).step_by(100) {
            src.frame_at(t).unwrap();
        }
        assert!(
            src.resident_bytes() <= FRAME_CACHE_BUDGET,
            "the cache is still bounded"
        );
        assert_eq!(held.time_ms, 500);
        assert_eq!(held.rgba().unwrap().len(), 1920 * 1080 * 4);
        assert_eq!(held.rgba().unwrap()[0], 5, "the pixels are still readable");

        // The window now sits at the tail, so 500 ms was evicted — asking again
        // re-seeks and decodes, and the answer is a different allocation even
        // though it is the same frame. `held` is what keeps the old pixels alive.
        let again = src.frame_at(500).unwrap();
        assert_eq!(again.time_ms, held.time_ms);
        assert!(
            !Arc::ptr_eq(&held, &again),
            "the frame must have left the cache, so this is a fresh decode"
        );
    }

    /// The cache is bounded in bytes, not in frames. Eight 1080p frames were
    /// 63 MiB per video layer; the budget holds one frame plus a one-frame
    /// window instead, and forward playback still walks without re-seeking.
    #[test]
    fn the_frame_cache_is_bounded_in_bytes_not_in_frames() {
        const FRAME: usize = 1920 * 1080 * 4;
        assert_eq!(FRAME, 8_294_400, "a 1080p RGBA frame");
        let mut src = FakeVideoSource::with_size(10.0, 40, 1920, 1080);

        for t in (0..1_200).step_by(100) {
            src.frame_at(t).unwrap();
            assert!(
                src.resident_bytes() <= FRAME_CACHE_BUDGET,
                "{} bytes resident at {t}ms",
                src.resident_bytes()
            );
        }
        assert_eq!(src.seeks(), 1, "forward playback must walk, not re-seek");
        assert!(
            src.cached_frames() <= 2,
            "1080p keeps one frame plus a window, got {}",
            src.cached_frames()
        );
        assert!(
            src.resident_bytes() <= 2 * FRAME,
            "a 1080p layer must stay near 16 MiB, not the old 63 MiB, got {}",
            src.resident_bytes()
        );
    }

    /// A frame past the whole budget is cached alone rather than refused: a 4K
    /// clip would otherwise never cache anything, and at end of stream the one
    /// cached frame is the only frame there is.
    #[test]
    fn a_frame_bigger_than_the_budget_is_still_cached() {
        const FRAME: usize = 3840 * 2160 * 4; // 33 MiB, twice the budget
        let mut src = FakeVideoSource::with_size(10.0, 20, 3840, 2160);
        src.frame_at(500).unwrap();
        assert_eq!(src.seeks(), 1);
        assert_eq!(src.cached_frames(), 1, "only one 4K frame fits");
        assert_eq!(src.resident_bytes(), FRAME);

        // Still correct, and still no second seek while walking forward.
        let again = src.frame_at(999).unwrap();
        assert!(again.time_ms > 500, "a forward step must not re-seek");
        assert_eq!(src.seeks(), 1);
        assert_eq!(src.resident_bytes(), FRAME);
    }

    /// A small frame still gets the full frame bound: the byte budget is a
    /// ceiling, not a replacement for [`FRAME_CACHE_CAP`].
    ///
    /// Note that one `frame_at` does not fill the cache. Asking for 1 900 ms
    /// decodes from the sync sample at or before it up to the first frame past
    /// it, which is two frames — the cap is reached by *walking*, not by a
    /// single request, and asserting otherwise would pin the seek behaviour
    /// instead of the cache bound.
    #[test]
    fn small_frames_still_fill_the_frame_cache() {
        let mut src = fake(200);
        src.frame_at(1_900).unwrap();
        let mut t: i64 = 1_900;
        while src.cached_frames() < FRAME_CACHE_CAP && t < 19_900 {
            t += 100;
            src.frame_at(t).unwrap();
        }
        assert_eq!(src.cached_frames(), FRAME_CACHE_CAP);
        assert_eq!(src.resident_bytes(), FRAME_CACHE_CAP * 4 * 4 * 4);
        assert!(
            src.resident_bytes() <= FRAME_CACHE_BUDGET,
            "tiny frames must not be limited by the byte budget"
        );
        // And walking never re-seeks while the window still covers the request.
        assert_eq!(src.seeks(), 1);
    }

    #[test]
    fn closed_source_rejects_everything() {
        let mut src = fake(5);
        assert!(src.frame_at(0).is_ok());
        src.close();
        assert_eq!(src.frame_at(0), Err(VideoError::Closed));
        assert_eq!(src.frame_at(200), Err(VideoError::Closed));
    }

    #[test]
    fn non_android_open_reports_unsupported() {
        let err = probe_video_fd(0, 0, -1).expect_err("host build cannot decode");
        assert!(matches!(err, VideoError::Unsupported(_)));
        assert!(err.to_string().contains("unsupported"));

        let err = open_video_fd(0, 0, -1)
            .err()
            .expect("host build cannot decode");
        assert!(matches!(err, VideoError::Unsupported(_)));

        // Every variant is printable and is a std error.
        for e in [
            VideoError::Unsupported("x"),
            VideoError::Open("x".into()),
            VideoError::Decode("x".into()),
            VideoError::Seek("x".into()),
            VideoError::NoAudioTrack,
            VideoError::Closed,
        ] {
            let _: &dyn std::error::Error = &e;
            assert!(!e.to_string().is_empty());
            assert!(!format!("{e:?}").is_empty());
        }
    }

    #[test]
    fn non_android_audio_decode_reports_unsupported() {
        let err = decode_audio_track_fd(0, 0, -1).expect_err("host build cannot decode");
        assert!(matches!(err, VideoError::Unsupported(_)));
        // Same on the mixer shortcut.
        let err = audio_source_fd(0, 0, -1, 0, 0, 1.0).expect_err("host build cannot decode");
        assert!(matches!(err, VideoError::Unsupported(_)));
    }
}

#[cfg(test)]
mod conversion_cost {
    use crate::video::yuv::{Matrix, Range, YuvFormat, YuvLayout, YuvPlanes};
    use std::sync::Arc;

    /// How long one 1080p conversion costs, printed on demand.
    ///
    /// `#[ignore]` because it is a **diagnostic, not a gate**: a timing
    /// assertion in a test suite is a flake waiting to happen, and this number
    /// belongs to a machine rather than to the code. It exists because the
    /// answer is not obvious and was paid for once already — the compositor used
    /// to convert every decoded frame, which measured 27 ms in release and 216 ms
    /// in the debug build the APK is made of, per frame per video layer. Anyone
    /// tempted to put the conversion back on a hot path can run this and see what
    /// they are about to spend.
    ///
    ///     cargo test -p rumo_media measure_conversion -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic timing, not a gate"]
    fn measure_conversion() {
        let (w, h) = (1920u32, 1080u32);
        let (stride, slice) = (1920usize, 1088usize);
        let mut bytes = vec![128u8; stride * slice];
        bytes.extend(std::iter::repeat_n(128u8, stride * slice / 2));
        let planes = YuvPlanes::new(
            YuvFormat::Nv12,
            YuvLayout {
                stride,
                slice_height: slice,
            },
            w,
            h,
            Matrix::Bt601,
            Range::Limited,
            0,
            Arc::<[u8]>::from(bytes.as_slice()),
        )
        .expect("a valid 1080p NV12 frame");

        // One warm-up, so the number is not a page-fault measurement.
        let _ = crate::video::yuv::rgba_from_planes(&planes).expect("convert");
        let runs = 20;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            let pixels = crate::video::yuv::rgba_from_planes(&planes).expect("convert");
            std::hint::black_box(&pixels);
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("one 1080p yuv_to_rgba: {per_frame:.2} ms");
    }
}

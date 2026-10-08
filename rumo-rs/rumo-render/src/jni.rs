// SPDX-License-Identifier: Apache-2.0

//! JNI entry points for uploading raster images and laying out text.
//!
//! These live in `rumo_render` (not `rumo_bridge`) so the texture registry
//! and the layout/atlas engine sit next to the code that consumes them.
//! The symbols are named for `com.kerneldroid.rumo.data.RumoBridge`; the
//! `rumo_bridge` cdylib only has to reference this module for the linker to
//! keep them exported.
//!
//! Two process-global registries back the handles:
//!
//! * **textures** — CPU-side [`TextureImage`] keyed by an opaque `u64`
//!   (`nativeUploadImage` → id, `nativeFreeTexture`). This is deliberately
//!   *not* a GPU context: uploading never needs a device, so a headless or
//!   driver-less process can still stage images. The GPU layer calls
//!   `GpuRenderer::set_texture(id, stamp, image)` lazily at draw time.
//!   Ids are minted per upload and never reused, which is what lets a still
//!   image keep its [`TextureStamp`] and a decoded video frame always get a
//!   new one.
//! * **layouts** — a [`TextLayout`] keyed by an opaque `u64`
//!   (`nativeLayoutText` → handle). Quads are read back one at a time with
//!   `nativeLayoutQuad`, the atlas page with `nativeLayoutAtlas`.
//!
//! Handles start at 1; `0` means "invalid handle" on the Kotlin side.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{
    JByteArray, JClass, JFloatArray, JIntArray, JLongArray, JString, Reference as _,
};
use jni::sys::{jbyteArray, jfloat, jfloatArray, jint, jintArray, jlong, jstring};

use crate::composite::{
    ATLAS_TEXTURE_ID, DrawWindow, ShapeSpec, argb_to_f32, image_mesh, preview_shape_draws,
    rgba_u32_to_argb, rotate_mesh, scale_text_mesh, text_mesh, window_allows, window_of,
};
use crate::engine::{ENGINE_OK, Engine, EngineScene};
use crate::renderer::{MeshData, SceneDraw, TexturedQuad};
use crate::text::{GlyphQuad, TextEngine, TextLayout};
use crate::texture::{
    Mat4, SceneTexture, TextureImage, TextureStamp, TexturedMesh, YuvTexture,
};
use rumo_core::canvas::CanvasSpec;
use rumo_core::effect::{CustomEffect, EffectChains};

fn bad() -> jni::errors::Error {
    jni::errors::Error::JniCall(jni::errors::JniError::Unknown)
}

// ---------------------------------------------------------------------------
// Texture registry (CPU-side staging; GPU upload is the renderer's job)
// ---------------------------------------------------------------------------

struct TextureStore {
    next_id: u64,
    /// Shared, because every draw of one frame wants the same pixels and a
    /// 1080p frame is 7.9 MiB: cloning it per draw was half a gigabyte per
    /// second of memcpy during playback.
    images: HashMap<u64, Arc<TextureImage>>,
    /// Decoded frames staged as *planes*, in the same id space as `images` and
    /// never the same id: a texture id names either an RGBA8 image or a frame's
    /// planes, and the draw picks its shader from that.
    ///
    /// Staged rather than uploaded here because this runs on a JNI thread with
    /// no device (GPU initialisation and every upload belong to the worker
    /// thread that owns the renderer); the worker claims the frame the first
    /// time a draw references its id, and re-claims each later frame because
    /// every frame arrives under a fresh id.
    frames: HashMap<u64, Arc<YuvTexture>>,
}

impl TextureStore {
    fn new() -> Self {
        Self {
            next_id: 1,
            images: HashMap::new(),
            frames: HashMap::new(),
        }
    }
}

fn textures() -> &'static Mutex<TextureStore> {
    static CELL: OnceLock<Mutex<TextureStore>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(TextureStore::new()))
}

/// Store a validated RGBA8 image under a fresh id. `None` for a zero-sized
/// image or a buffer whose length is not `width * height * 4`.
///
/// The id doubles as the content generation: it is never reused or rewritten,
/// so [`TextureStamp::for_content`] of a still image only changes when the
/// caller hands over new pixels, and a per-frame source gets a new one by
/// definition.
pub fn upload_image(width: u32, height: u32, rgba: Vec<u8>) -> Option<u64> {
    let image = TextureImage::new(width, height, rgba)?;
    let mut store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id = store.next_id;
    store.next_id += 1;
    store.images.insert(id, Arc::new(image));
    Some(id)
}

/// Store a decoded frame's planes under a fresh id, shared rather than copied:
/// the decoder's own buffer covers all three planes, so nothing is converted
/// and nothing is duplicated here.
///
/// Returns `None` only for a poisoned store lock; an unusable plane description
/// never gets this far — [`YuvTexture::new`] has already refused it.
pub fn upload_yuv_frame(frame: YuvTexture) -> Option<u64> {
    let mut store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id = store.next_id;
    store.next_id += 1;
    store.frames.insert(id, Arc::new(frame));
    Some(id)
}

/// The staged frame for `id`, shared with the worker that uploads it, if any.
pub fn yuv_frame(id: u64) -> Option<Arc<YuvTexture>> {
    let store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.frames.get(&id).cloned()
}

/// Forget the payload for `id`; `true` when one was present.
///
/// Drops the staged `Arc`, never the GPU copy: that belongs to the renderer's
/// cache, which frees a slot together with its residency record (docs/08
/// §8.19.10).
pub fn free_texture(id: u64) -> bool {
    let mut store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let image = store.images.remove(&id).is_some();
    let frame = store.frames.remove(&id).is_some();
    image || frame
}

/// Clone of the staged image for `id`, if any.
pub fn texture_image(id: u64) -> Option<TextureImage> {
    let store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store
        .images
        .get(&id)
        .map(|image| image.as_ref().clone())
}

/// The staged image for `id` shared with the rest of the frame, if any.
///
/// This is what the per-frame scene building uses: it hands the same `Arc` to
/// every draw of that image and to the GPU payload, so a frame costs no image
/// copy at all. [`texture_image`] stays for callers that want an owned copy.
pub fn texture_arc(id: u64) -> Option<Arc<TextureImage>> {
    let store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.images.get(&id).cloned()
}

/// Number of staged payloads: images plus decoded frames.
pub fn texture_count() -> usize {
    let store = textures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.images.len() + store.frames.len()
}

// ---------------------------------------------------------------------------
// Layout registry
// ---------------------------------------------------------------------------

struct LayoutStore {
    next_id: u64,
    layouts: HashMap<u64, TextLayout>,
}

impl LayoutStore {
    fn new() -> Self {
        Self {
            next_id: 1,
            layouts: HashMap::new(),
        }
    }
}

fn layouts() -> &'static Mutex<LayoutStore> {
    static CELL: OnceLock<Mutex<LayoutStore>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(LayoutStore::new()))
}

fn text_engine() -> &'static Mutex<TextEngine> {
    static CELL: OnceLock<Mutex<TextEngine>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(TextEngine::new()))
}

/// Store a prebuilt layout under a fresh handle.
pub fn store_layout(layout: TextLayout) -> u64 {
    let mut store = layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id = store.next_id;
    store.next_id += 1;
    store.layouts.insert(id, layout);
    id
}

/// Shape `text` at `size_px` with the shared engine and store the result.
/// `None` for empty input, bad sizes or when no font is available.
pub fn layout_text(text: &str, size_px: f32) -> Option<u64> {
    layout_text_styled(text, &crate::text::TextStyle::new(size_px))
}

/// Shape `text` in `style` with the shared engine and store the result.
///
/// The style carries the weight asked of the font database and the outline
/// thickness: with a stroke the stored layout *is* the outline, so a caller
/// that wants a contour draws this handle behind the plain one.
pub fn layout_text_styled(text: &str, style: &crate::text::TextStyle) -> Option<u64> {
    layout_text_family(text, style, None)
}

/// [`layout_text_styled`], shaped in `family` rather than the engine default.
///
/// The extra parameter is a separate entry point instead of a wider `TextStyle`
/// because `TextStyle` is `Copy` and lives in the per-layer hot path: a
/// `String` field would make every call site clone, and every existing caller
/// would have to answer a question ("which font?") it does not have.
pub fn layout_text_family(
    text: &str,
    style: &crate::text::TextStyle,
    family: Option<&str>,
) -> Option<u64> {
    let mut engine = text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let layout = engine.layout_styled_family(text, style, family)?;
    Some(store_layout(layout))
}

/// Register a face from raw bytes into the shared engine; returns the family
/// name read out of the file, or `None` for bytes it cannot parse.
pub fn register_font(bytes: Vec<u8>) -> Option<String> {
    let mut engine = text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    engine.load_font(bytes)
}

/// How many faces the preview engine keeps registered at once.
///
/// The font database has no way to drop a face, so previewing a catalogue in
/// one engine would pin every face it ever showed — a few hundred megabytes
/// over a full scroll of Google Fonts. The preview engine keeps the *bytes* of
/// the most recent faces and rebuilds itself past this cap, which bounds
/// memory to the cap instead of to the session.
const PREVIEW_FONT_CAP: usize = 12;

/// Total bytes the preview engine keeps registered.
///
/// A count alone was not enough once variable fonts arrived: one file carries
/// every axis and runs to megabytes (Google Sans Flex is 4.15 MB), so twelve of
/// them would have been fifty megabytes held for pictures of a list. The count
/// still bounds the work, the budget bounds the memory.
const PREVIEW_FONT_BYTE_CAP: usize = 24 * 1024 * 1024;

/// A disposable second engine that holds only the faces the shop is currently
/// previewing. Deliberately not the shared [`text_engine`]: an installed font
/// must stay for the editor's lifetime, a previewed one must not.
struct PreviewFonts {
    /// Cache keys in registration order, oldest first.
    order: std::collections::VecDeque<String>,
    bytes: HashMap<String, Vec<u8>>,
    /// Cache key → the family name the face itself declares.
    ///
    /// The shop asks by catalogue name ("Open Sans"), but a file is addressed in
    /// the database by the name it carries. They usually agree, and when they do
    /// not, shaping under the catalogue name would fall back to the default face
    /// — a preview of the wrong font. Resolving once per key is what keeps the
    /// two names apart instead of hoping they match.
    resolved: HashMap<String, String>,
    /// Bytes held across [`Self::bytes`], so the budget can be enforced.
    held_bytes: usize,
    engine: TextEngine,
}

impl PreviewFonts {
    fn new() -> Self {
        Self {
            order: std::collections::VecDeque::new(),
            bytes: HashMap::new(),
            resolved: HashMap::new(),
            held_bytes: 0,
            engine: TextEngine::new(),
        }
    }

    /// Make `key` addressable, evicting the oldest face and rebuilding the
    /// engine once the cap is passed. Returns the family name to shape with.
    fn ensure(&mut self, key: &str, font_bytes: Vec<u8>) -> Option<String> {
        if let Some(name) = self.resolved.get(key).cloned() {
            // Already loaded: only recency changes.
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                if let Some(k) = self.order.remove(pos) {
                    self.order.push_back(k);
                }
            }
            return Some(name);
        }
        let name = self.engine.load_font(font_bytes.clone())?;
        self.held_bytes += font_bytes.len();
        self.bytes.insert(key.to_owned(), font_bytes);
        self.resolved.insert(key.to_owned(), name.clone());
        self.order.push_back(key.to_owned());
        let over_count = self.order.len() > PREVIEW_FONT_CAP;
        let over_bytes = self.held_bytes > PREVIEW_FONT_BYTE_CAP;
        if over_count || over_bytes {
            // Evict until both bounds hold. The face just asked for is at the
            // back, so it is never the one dropped unless it alone exceeds the
            // budget — and then there is nothing to keep anyway.
            while (self.order.len() > PREVIEW_FONT_CAP
                || self.held_bytes > PREVIEW_FONT_BYTE_CAP)
                && self.order.len() > 1
            {
                if let Some(evicted) = self.order.pop_front() {
                    if let Some(gone) = self.bytes.remove(&evicted) {
                        self.held_bytes = self.held_bytes.saturating_sub(gone.len());
                    }
                    self.resolved.remove(&evicted);
                }
            }
            // A face cannot be removed from the database, so honouring the bounds
            // means starting the database over from the retained bytes.
            self.engine = TextEngine::new();
            for k in self.order.clone() {
                if let Some(bytes) = self.bytes.get(&k).cloned() {
                    self.engine.load_font(bytes);
                }
            }
        }
        Some(name)
    }
}

fn preview_fonts() -> &'static Mutex<PreviewFonts> {
    static CELL: OnceLock<Mutex<PreviewFonts>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(PreviewFonts::new()))
}

/// Flat `[width:u32 LE][height:u32 LE][rgba…]` preview of `text` in `key`,
/// rendered from `font_bytes` in the disposable preview engine.
///
/// `key` is the caller's own label for the face (the catalogue name); the name
/// actually shaped with is read out of the file. `None` when the bytes do not
/// register, the text has no ink, or the box is degenerate.
pub fn font_preview(
    key: &str,
    font_bytes: Vec<u8>,
    text: &str,
    style: &crate::text::TextStyle,
    argb: u32,
    pad: u32,
) -> Option<Vec<u8>> {
    let mut preview = preview_fonts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let family = preview.ensure(key, font_bytes)?;
    let layout = preview
        .engine
        .layout_styled_family(text, style, Some(family.as_str()))?;
    let atlas = preview.engine.atlas_image();
    let (w, h, rgba) = crate::composite::rasterize_text(&layout, &atlas, argb, pad)?;
    let mut out = Vec::with_capacity(8 + rgba.len());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&rgba);
    Some(out)
}

/// Size of the shared engine's atlas page, which is what a stored layout's
/// atlas rects must be measured against — the page grows, so this is read when
/// a quad is handed out rather than remembered from layout time.
pub fn atlas_page_size() -> (u32, u32) {
    let engine = text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (engine.atlas().width(), engine.atlas().height())
}

/// Number of quads in layout `id` (0 for an unknown handle).
pub fn layout_quad_count(id: u64) -> usize {
    let store = layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.layouts.get(&id).map(|l| l.quads.len()).unwrap_or(0)
}

/// One quad of layout `id`.
pub fn layout_quad(id: u64, index: usize) -> Option<GlyphQuad> {
    let store = layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.layouts.get(&id)?.quads.get(index).copied()
}

/// `(width, height)` of layout `id`.
pub fn layout_bounds(id: u64) -> Option<(f32, f32)> {
    let store = layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.layouts.get(&id).map(|l| (l.width, l.height))
}

/// Drop layout `id`; `true` when one was present.
pub fn free_layout(id: u64) -> bool {
    let mut store = layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.layouts.remove(&id).is_some()
}

/// Flat `[width:u32 LE][height:u32 LE][rgba...]` of the shared engine's
/// current atlas page, for `id` a known layout. The page contains every
/// glyph rasterized so far, not only this layout's.
pub fn layout_atlas_flat(id: u64) -> Option<Vec<u8>> {
    if !layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .layouts
        .contains_key(&id)
    {
        return None;
    }
    let engine = text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(engine.atlas_image().to_flat())
}

// ---------------------------------------------------------------------------
// JNI: textures
// ---------------------------------------------------------------------------

/// `RumoBridge.nativeUploadImage(width, height, rgba): Long`
///
/// Stages a straight-alpha RGBA8 image and returns its texture id (>= 1), or
/// `0` after throwing `java/lang/RuntimeException` when the dimensions or
/// buffer length are invalid.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeUploadImage(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    width: jint,
    height: jint,
    rgba: JByteArray<'_>,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let bytes = env.convert_byte_array(&rgba)?;
        let id = upload_image(width.max(0) as u32, height.max(0) as u32, bytes).ok_or_else(bad)?;
        Ok(id as jlong)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeFreeTexture(id): Unit` — never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeFreeTexture(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    id: jlong,
) {
    free_texture(id as u64);
}

// ---------------------------------------------------------------------------
// JNI: SVG sources
// ---------------------------------------------------------------------------

/// `RumoBridge.nativeSvgRegister(svg: ByteArray): Int`
///
/// Parses `svg` **once**, stores the document and returns its id (>= 1). Never
/// throws: a source the parser rejects, and an exhausted id space, both yield
/// `-1`. Parsing once is the point — the frame is assembled every frame, and
/// re-parsing an SVG (paths, gradients) per frame would dominate the frame.
///
/// The id must be released with [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRelease`].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRegister(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    svg: JByteArray<'_>,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let Ok(bytes) = env.convert_byte_array(&svg) else {
            // A JNI-side read failure is a registration failure here, not an
            // exception: the caller's contract is "id or -1".
            return Ok(-1);
        };
        Ok(crate::composite::register_svg(&bytes).unwrap_or(-1))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeSvgRelease(id): Unit` — never throws. Unknown ids are
/// ignored; the id is not handed out again — see
/// [`crate::composite::register_svg`].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRelease(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    id: jint,
) {
    crate::composite::free_svg(id);
}

/// `RumoBridge.nativeSvgRasterize(svg: ByteArray, sizePx: Int): ByteArray`
///
/// The legacy fallback. Rasterises the whole document with `resvg` and returns
/// flat `[width:u32 LE][height:u32 LE][rgba...]`, straight-alpha RGBA8, ready
/// for [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeUploadImage`] — the
/// same picture path an imported PNG takes.
///
/// The dimensions travel **inline**, as a two-`u32` little-endian header before
/// the pixels, rather than a second measure call or a base64 JSON wrapper: the
/// existing flat encodings (`nativeLayoutAtlas`, `nativeDecodeImage`) already
/// look exactly like this, the header costs 8 bytes, and a fallback path should
/// not pay for a round trip or an encoding. No separate measure entry exists,
/// so it is one JNI transition per raster.
///
/// Never throws: a JNI read failure, malformed input, `sizePx <= 0`, a raster
/// above the pixel cap and a panic all yield **null**, following
/// `nativeSvgRegister`'s "caller gets a value or null" contract.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRasterize(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    svg: JByteArray<'_>,
    size_px: jint,
) -> jbyteArray {
    // A panic must not unwind into the JVM: `resvg`/`usvg` run on model-supplied
    // bytes here, and this entry's contract is "pixels or null", not "throw".
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.with_env(|env| -> jni::errors::Result<jbyteArray> {
            let Ok(bytes) = env.convert_byte_array(&svg) else {
                return Ok(std::ptr::null_mut());
            };
            if size_px <= 0 {
                return Ok(std::ptr::null_mut());
            }
            match crate::svg_legacy::rasterize_svg(&bytes, size_px as u32) {
                Ok(raster) => {
                    let arr = env.byte_array_from_slice(&raster.to_flat())?;
                    Ok(arr.as_raw() as jbyteArray)
                }
                Err(_) => Ok(std::ptr::null_mut()),
            }
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
    }));
    result.unwrap_or(std::ptr::null_mut())
}

/// `RumoBridge.nativeSvgValidate(svg: ByteArray): String`
///
/// Validates an SVG source **without registering it**, so the `svg_paint` tool
/// can refuse a broken source before it is written into a project. Replies
/// `{"ok":true,"width":F,"height":F,"shapes":N,"flattenedGradients":N,
/// "skipped":N}` for a document [`crate::svg::parse_svg`] accepts, and
/// `{"ok":false,"error":"<reason>"}` for bytes it rejects — the reason is the
/// parser's, escaped so it cannot break the object the caller then parses.
///
/// `skipped` is the **count** of unsupported elements, not the reasons array:
/// Kotlin's `SvgValidation.skipped` is an `Int`. The reasons themselves stay in
/// the parsed document (and on the frame path); a caller that wants them should
/// ask for a separate field rather than overload this one.
///
/// Never throws and never panics: the parser runs on model-supplied bytes, so a
/// panic is caught and answered with `null`, exactly like
/// [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRasterize`]. A JNI read
/// failure is reported in the same `ok:false` shape rather than as a null the
/// caller would have to special-case.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgValidate(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    svg: JByteArray<'_>,
) -> jstring {
    // A panic must not unwind into the JVM: `usvg` runs on model-supplied bytes
    // here, and this entry's contract is "a JSON verdict", not "throw".
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.with_env(|env| -> jni::errors::Result<jstring> {
            let json = match env.convert_byte_array(&svg) {
                Ok(bytes) => svg_validate_json(&bytes),
                // The caller cannot act on a missing byte array differently
                // from a rejected document, so it gets the same `ok:false`.
                Err(_) => "{\"ok\":false,\"error\":\"SVG bytes could not be read\"}".to_string(),
            };
            java_json(env, &json)
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
    }));
    result.unwrap_or(std::ptr::null_mut())
}

/// The body of `nativeSvgValidate`, kept pure so it is unit-testable without a
/// JVM. Success carries the document's own numbers; a rejected source carries
/// the parser's reason and no numbers, because Kotlin reads `ok` first.
fn svg_validate_json(bytes: &[u8]) -> String {
    match crate::svg::parse_svg(bytes) {
        Ok(doc) => format!(
            "{{\"ok\":true,\"width\":{},\"height\":{},\"shapes\":{},\"flattenedGradients\":{},\"skipped\":{}}}",
            doc.width,
            doc.height,
            doc.shapes.len(),
            doc.flattened_gradients,
            doc.skipped.len()
        ),
        Err(reason) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&reason)),
    }
}

// ---------------------------------------------------------------------------
// JNI: text layout
// ---------------------------------------------------------------------------

/// `RumoBridge.nativeLayoutText(text, sizePx): Long`
///
/// Returns a layout handle (>= 1), or `0` after throwing
/// `java/lang/RuntimeException` when layout fails (empty text, non-positive
/// size, no font).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutText(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    text: JString<'_>,
    size_px: jfloat,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let text = text.try_to_string(env)?;
        let handle = layout_text(&text, size_px).ok_or_else(bad)?;
        Ok(handle as jlong)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeLayoutTextStyled(text, sizePx, weight, strokePx): Long`
///
/// Same contract as [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutText`],
/// with the two things a text layer can ask for beyond its size: the face
/// weight (`400` regular, `700` bold — a family that has the face uses it, a
/// family that does not gets a thickened bitmap) and the outline thickness in
/// pixels. With `strokePx > 0` the returned layout is the glyph's *outline*, so
/// a contour is that handle drawn behind the plain one.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutTextStyled(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    text: JString<'_>,
    size_px: jfloat,
    weight: jint,
    stroke_px: jfloat,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let text = text.try_to_string(env)?;
        let style = crate::text::TextStyle::new(size_px)
            .with_weight(weight.clamp(0, u16::MAX as i32) as u16)
            .with_stroke(stroke_px.max(0.0).round() as u32);
        let handle = layout_text_styled(&text, &style).ok_or_else(bad)?;
        Ok(handle as jlong)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeFontRegister(bytes: ByteArray): String`
///
/// Registers a downloaded face (TTF/OTF/TTC) in the shared text engine and
/// returns the **family name read from the file**, or `null` for bytes the
/// database cannot parse. The name is the file's own, not the caller's guess:
/// the shop's job is to hand over what it downloaded, and only the font knows
/// what it calls itself.
///
/// Never throws: unparsable bytes are an expected outcome for a network
/// download, so the contract is "a name or null".
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeFontRegister(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    bytes: JByteArray<'_>,
) -> jstring {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.with_env(|env| -> jni::errors::Result<jstring> {
            let Ok(bytes) = env.convert_byte_array(&bytes) else {
                return Ok(std::ptr::null_mut());
            };
            match register_font(bytes) {
                Some(name) => Ok(env.new_string(&name)?.as_raw() as jstring),
                None => Ok(std::ptr::null_mut()),
            }
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
    }));
    result.unwrap_or(std::ptr::null_mut())
}

/// `RumoBridge.nativeLayoutTextFamily(text, sizePx, weight, strokePx, family): Long`
///
/// [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutTextStyled`] with an
/// explicit family (a name from `nativeFontRegister`). An empty `family` means
/// the engine default, so a caller that has no per-layer font keeps using the
/// styled entry point and this one answers the same.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutTextFamily(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    text: JString<'_>,
    size_px: jfloat,
    weight: jint,
    stroke_px: jfloat,
    family: JString<'_>,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let text = text.try_to_string(env)?;
        let family = family.try_to_string(env)?;
        let style = crate::text::TextStyle::new(size_px)
            .with_weight(weight.clamp(0, u16::MAX as i32) as u16)
            .with_stroke(stroke_px.max(0.0).round() as u32);
        let handle =
            layout_text_family(&text, &style, Some(family.as_str())).ok_or_else(bad)?;
        Ok(handle as jlong)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeFontPreview(family, fontBytes, text, sizePx, weight, argb, pad): ByteArray`
///
/// Flat `[width:u32 LE][height:u32 LE][rgba…]` rendering of `text` in `family`
/// — the shop's preview, drawn by the same shaper, atlas and compositor the
/// editor uses, so a preview cannot disagree with the layer. `argb` is
/// `0xAARRGGBB` (Kotlin `Color`), `pad` is transparent margin in pixels.
///
/// `fontBytes` are the downloaded face. They are passed per call rather than
/// registered once because the preview engine is **disposable and bounded**: it
/// keeps the last [`PREVIEW_FONT_CAP`] faces and rebuilds past that, so looking
/// at a catalogue cannot pin the whole catalogue in memory. An installed font
/// goes to `nativeFontRegister` instead and stays for the session.
///
/// Returns `null` for bytes that do not register as `family`, empty text, or a
/// degenerate box. Never throws: a preview is a nicety, not a contract.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeFontPreview(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    family: JString<'_>,
    font_bytes: JByteArray<'_>,
    text: JString<'_>,
    size_px: jfloat,
    weight: jint,
    argb: jint,
    pad: jint,
) -> jbyteArray {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.with_env(|env| -> jni::errors::Result<jbyteArray> {
            let Ok(family) = family.try_to_string(env) else {
                return Ok(std::ptr::null_mut());
            };
            let Ok(font_bytes) = env.convert_byte_array(&font_bytes) else {
                return Ok(std::ptr::null_mut());
            };
            let Ok(text) = text.try_to_string(env) else {
                return Ok(std::ptr::null_mut());
            };
            let style = crate::text::TextStyle::new(size_px)
                .with_weight(weight.clamp(0, u16::MAX as i32) as u16);
            let flat = font_preview(
                &family,
                font_bytes,
                &text,
                &style,
                argb as u32,
                pad.clamp(0, 256) as u32,
            );
            match flat {
                Some(bytes) => Ok(env.byte_array_from_slice(&bytes)?.as_raw() as jbyteArray),
                None => Ok(std::ptr::null_mut()),
            }
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
    }));
    result.unwrap_or(std::ptr::null_mut())
}

/// `RumoBridge.nativeLayoutQuadCount(handle): Int` — 0 for an unknown handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutQuadCount(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jint {
    layout_quad_count(handle as u64) as jint
}

/// `RumoBridge.nativeLayoutQuad(handle, index): FloatArray` — flat
/// `[x, y, w, h, u0, v0, u1, v1]`, with the UV resolved against the atlas page
/// as it is *now*. Throws for a bad handle/index.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutQuad(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    index: jint,
) -> jfloatArray {
    env.with_env(|env| -> jni::errors::Result<jfloatArray> {
        if index < 0 {
            return Err(bad());
        }
        let q = layout_quad(handle as u64, index as usize).ok_or_else(bad)?;
        let (page_w, page_h) = atlas_page_size();
        let uv = q.uv(page_w, page_h).ok_or_else(bad)?;
        let flat = [q.x, q.y, q.w, q.h, uv[0], uv[1], uv[2], uv[3]];
        let arr = env.new_float_array(flat.len())?;
        arr.set_region(env, 0, &flat)?;
        Ok(arr.as_raw() as jfloatArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeLayoutBounds(handle): FloatArray` — flat `[width, height]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutBounds(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jfloatArray {
    env.with_env(|env| -> jni::errors::Result<jfloatArray> {
        let (w, h) = layout_bounds(handle as u64).ok_or_else(bad)?;
        let flat = [w, h];
        let arr = env.new_float_array(flat.len())?;
        arr.set_region(env, 0, &flat)?;
        Ok(arr.as_raw() as jfloatArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeLayoutAtlas(handle): ByteArray` — flat
/// `[width:u32 LE][height:u32 LE][rgba...]` of the engine's atlas page,
/// ready for `nativeUploadImage`. Throws for an unknown handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutAtlas(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jbyteArray {
    env.with_env(|env| -> jni::errors::Result<jbyteArray> {
        let flat = layout_atlas_flat(handle as u64).ok_or_else(bad)?;
        let arr = env.byte_array_from_slice(&flat)?;
        Ok(arr.as_raw() as jbyteArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeLayoutFree(handle): Unit` — never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutFree(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    free_layout(handle as u64);
}

// ---------------------------------------------------------------------------
// JNI: canvas presets, bitrate, effect catalogue
// ---------------------------------------------------------------------------

/// Minimal JSON string escaping (the tables are static ASCII today, but a
/// label could grow a quote or a backslash tomorrow).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// `{"presets":[{"id","label","width","height"}],"aspects":[{"label","w","h"}],
/// "fps":[…]}` built from the `rumo_core::canvas` tables (the single source of
/// truth for them). Infinitely fallible in theory, never in practice: there is
/// no I/O and no parsing here.
///
/// Hand-formatted on purpose: `serde_json` is *not* a dependency of this crate
/// and the shape is a handful of scalars per entry.
fn resolution_presets_json() -> String {
    use rumo_core::canvas::{ASPECTS, FPS_CHOICES, PRESETS};
    let presets: Vec<String> = PRESETS
        .iter()
        .map(|p| {
            format!(
                r#"{{"id":"{}","label":"{}","width":{},"height":{}}}"#,
                json_escape(p.id),
                json_escape(p.label),
                p.width,
                p.height
            )
        })
        .collect();
    let aspects: Vec<String> = ASPECTS
        .iter()
        .map(|(label, w, h)| {
            format!(
                r#"{{"label":"{}","w":{},"h":{}}}"#,
                json_escape(label),
                w,
                h
            )
        })
        .collect();
    let fps: Vec<String> = FPS_CHOICES.iter().map(|f| f.to_string()).collect();
    format!(
        r#"{{"presets":[{}],"aspects":[{}],"fps":[{}]}}"#,
        presets.join(","),
        aspects.join(","),
        fps.join(",")
    )
}

/// Materialise `json` as a Java string from an env already in hand, never
/// returning a null reference: if the JVM refuses the payload the empty JSON
/// object is attempted instead.
fn java_json(env: &mut jni::Env<'_>, json: &str) -> jni::errors::Result<jstring> {
    match env.new_string(json) {
        Ok(s) => Ok(s.as_raw() as jstring),
        Err(_) => Ok(env.new_string("{}")?.as_raw() as jstring),
    }
}

/// Materialise `json` as a Java string, never returning a null reference: if
/// the JVM refuses the payload the empty JSON object is attempted instead. A
/// null result therefore means even `"{}"` could not be allocated (OOM with a
/// pending exception), which is the documented behaviour of the other
/// string-returning entry points in this workspace.
fn to_java_json(mut env: EnvUnowned<'_>, json: &str) -> jstring {
    env.with_env(|env| java_json(env, json))
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeResolutionPresets(): String`
///
/// JSON `{"presets":[{"id","label","width","height"}],"aspects":[{"label",
/// "w","h"}],"fps":[24,…]}`, straight from [`rumo_core::canvas`] — no second
/// copy of the preset table lives in Kotlin or here. Never returns null; on an
/// internal failure it degrades to `"{}"`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeResolutionPresets(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jstring {
    to_java_json(env, &resolution_presets_json())
}

/// `RumoBridge.nativeBitrateFor(width, height, fps): Int`
///
/// Target encoder bitrate for a canvas, from [`CanvasSpec::bitrate_bps`] (the
/// same heuristic the export path uses), clamped into `Int`. A spec that
/// `CanvasSpec::new` rejects (zero or odd sides, `fps` outside `1..=240`) is
/// answered with a sane 4 Mbit/s default rather than an exception: this is a
/// UI hint, not an operation on a real canvas. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeBitrateFor(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    width: jint,
    height: jint,
    fps: jint,
) -> jint {
    /// Fallback for a spec the canvas model rejects.
    const FALLBACK_BPS: u32 = 4_000_000;
    let bps = match CanvasSpec::new(width.max(0) as u32, height.max(0) as u32, fps.max(0) as u32) {
        Ok(spec) => spec.bitrate_bps(),
        Err(_) => FALLBACK_BPS,
    };
    bps.min(i32::MAX as u32) as jint
}

/// `RumoBridge.nativeRenderDiagnostics(): String`
///
/// The render diagnostics report as JSON (`rumo_render::diag::json`): the path
/// of the last frame, the adapter that was created, a one-line hint, the effect
/// kinds whose pipeline the driver rejected, and the bounded log of every
/// fallback reason. Never returns null; `"{}"` only if serialisation fails.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderDiagnostics(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jstring {
    to_java_json(env, &crate::diag::json())
}

/// `RumoBridge.nativeClearRenderDiagnostics()`
///
/// Empties the log and the stale hint. The adapter name and the rejected-effect
/// list survive on purpose: they are the actionable part of the report.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeClearRenderDiagnostics(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
) {
    crate::diag::clear();
}

/// `RumoBridge.nativeEffectCatalogue(): String`
///
/// The effect catalogue JSON of [`rumo_core::effect::catalogue_json`] (ids,
/// labels, cost, slots, passes, parameter ranges) for the Kotlin UI. Never
/// returns null; on an internal failure it degrades to `"{}"`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectCatalogue(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jstring {
    to_java_json(env, &rumo_core::effect::catalogue_json())
}

/// `RumoBridge.nativeEffectValidate(json: String): String`
///
/// Validates one project-defined effect, given its `CustomEffect` JSON. Needs
/// no GPU device: it is the fast host-side check the authoring UI runs while
/// the user types. On success replies `{"ok":true,"id":…,"label":…,"slots":N,
/// "block_len":N,"passes":N,"fields":[…],"params":[{"key":…,"slots":N,
/// "slot":N}]}`, where `slot` is the parameter's offset in the flat `f32`
/// block. On malformed JSON, a failed structural check, or a module `naga`
/// refuses it replies `{"ok":false,"error":…}` — the error is safe to show to
/// the user. Never returns null; `"{}"` only if the JVM refuses both strings.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectValidate(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    json: JString<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let raw = if json.is_null() {
            String::new()
        } else {
            json.try_to_string(env)?
        };
        java_json(env, &effect_validate_json(&raw))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeEffectCatalogueEx(customsJson: String): String`
///
/// The same catalogue as
/// [`Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectCatalogue`], plus
/// one entry per project-defined effect in `customsJson` (a JSON array of
/// `CustomEffect` objects, or an empty/blank string for the built-ins only). A
/// malformed array is treated as empty rather than failing the UI, the same
/// leniency the chain parser applies. Never returns null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectCatalogueEx(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    customs_json: JString<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let raw = if customs_json.is_null() {
            String::new()
        } else {
            customs_json.try_to_string(env)?
        };
        let customs: Vec<CustomEffect> = if raw.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&raw).unwrap_or_default()
        };
        java_json(env, &rumo_core::effect::catalogue_json_with(&customs))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// The body of `nativeEffectValidate`, kept pure so it is unit-testable without
/// a JVM.
fn effect_validate_json(json: &str) -> String {
    use serde_json::json;

    let reply = |value: serde_json::Value| -> String {
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
    };
    let effect: CustomEffect = match serde_json::from_str(json) {
        Ok(effect) => effect,
        Err(e) => {
            return reply(json!({ "ok": false, "error": format!("malformed effect json: {e}") }));
        }
    };
    if let Err(error) = crate::effect::validate_custom(&effect) {
        return reply(json!({ "ok": false, "error": error }));
    }
    // The structural check has already passed, so `layout()` cannot fail here;
    // the match keeps the JNI boundary panic-free anyway.
    let layout = match effect.layout() {
        Ok(layout) => layout,
        Err(error) => return reply(json!({ "ok": false, "error": error })),
    };
    let params: Vec<serde_json::Value> = layout
        .params
        .iter()
        .map(|p| json!({ "key": p.key, "slots": p.slots(), "slot": p.offset }))
        .collect();
    reply(json!({
        "ok": true,
        "id": layout.id,
        "label": layout.label,
        "slots": layout.slots,
        "block_len": layout.block_len,
        "passes": layout.entries.len(),
        "fields": layout.fields,
        "params": params,
    }))
}

// ---------------------------------------------------------------------------
// Extended preview: SHAPE + text + image layers (CPU composite + engine scene)
// ---------------------------------------------------------------------------

/// Clone of the layout behind `id`, if any.
pub fn layout_copy(id: u64) -> Option<TextLayout> {
    layouts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .layouts
        .get(&id)
        .cloned()
}

/// Snapshot of the shared engine's current atlas page.
pub fn atlas_snapshot() -> TextureImage {
    text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .atlas_image()
}

/// The shared engine's atlas page plus a [`TextureStamp`] of its pixels.
///
/// The page is not in the texture registry and never was: it shares the one
/// constant [`ATLAS_TEXTURE_ID`] with every text quad of every scene. So its
/// content has to say what it is, and the stamp is computed from the page's
/// dimensions and glyph count under the same lock as the snapshot — the two
/// always describe the same page. Any rasterization goes through
/// `layout_text*` here, which takes this very lock, so nothing can slip a
/// glyph in between the stamp and the pixels.
///
/// Read under one lock on purpose: two acquisitions could straddle a layout
/// call from another thread and hand the GPU a stamp that describes the
/// previous page, which would freeze the atlas for a frame.
fn stamped_atlas_page() -> (Arc<TextureImage>, TextureStamp) {
    let engine = text_engine()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let atlas = engine.atlas();
    let stamp = TextureStamp::for_atlas(atlas.width(), atlas.height(), atlas.glyph_count());
    (Arc::new(engine.atlas_image()), stamp)
}

/// `(layout, atlas)` for `id`; `None` for an unknown handle. The atlas is
/// the engine's current page (it holds every glyph rasterized so far, not
/// only this layout's).
pub fn layout_snapshot(id: u64) -> Option<(TextLayout, TextureImage)> {
    let layout = layout_copy(id)?;
    Some((layout, atlas_snapshot()))
}

/// Borrow the engine behind `handle` (`Box<Engine>` raw pointer as `jlong`).
/// `None` for a null handle.
fn engine_from_handle(handle: jlong) -> Option<&'static Engine> {
    if handle == 0 {
        return None;
    }
    // SAFETY: Kotlin only passes handles returned by nativeEngineCreate and
    // never calls after nativeEngineDestroy; the Box keeps the engine alive
    // in between.
    unsafe { (handle as *const Engine).as_ref() }
}

fn read_i32_nullable(
    env: &mut jni::Env<'_>,
    arr: &JIntArray<'_>,
) -> jni::errors::Result<Vec<i32>> {
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let n = arr.len(env)? as usize;
    let mut buf = vec![0i32; n];
    arr.get_region(env, 0, &mut buf)?;
    Ok(buf)
}

fn read_f32_nullable(
    env: &mut jni::Env<'_>,
    arr: &JFloatArray<'_>,
) -> jni::errors::Result<Vec<f32>> {
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let n = arr.len(env)? as usize;
    let mut buf = vec![0f32; n];
    arr.get_region(env, 0, &mut buf)?;
    Ok(buf)
}

fn read_i64_nullable(
    env: &mut jni::Env<'_>,
    arr: &JLongArray<'_>,
) -> jni::errors::Result<Vec<i64>> {
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let n = arr.len(env)? as usize;
    let mut buf = vec![0i64; n];
    arr.get_region(env, 0, &mut buf)?;
    Ok(buf)
}

/// One draw group's per-draw time window, read from its two parallel arrays.
///
/// `null` or empty reads as no window at all, and a shorter durations array
/// leaves the rest of the draws unrestricted: the windows are a *second* gate
/// behind the editor's own filtering, so the cost of a caller bug here must be
/// an extra draw at worst — never a layer that vanished from a frame it belongs
/// to. A window whose duration did not survive clamping draws nothing, which is
/// what the half-open predicate already does.
fn read_windows(
    env: &mut jni::Env<'_>,
    starts: &JLongArray<'_>,
    durations: &JLongArray<'_>,
) -> jni::errors::Result<Vec<DrawWindow>> {
    let starts = read_i64_nullable(env, starts)?;
    let durations = read_i64_nullable(env, durations)?;
    Ok(draw_windows(&starts, &durations))
}

/// Common length of one parallel-array group, or `None` on any mismatch.
/// `null` arrays read as empty above, so "all null" is `Some(0)` (no layers)
/// while "half null" is `None` (the caller throws, like the legacy path).
fn common_len(lens: &[usize]) -> Option<usize> {
    let first = *lens.first()?;
    if lens.iter().all(|&l| l == first) {
        Some(first)
    } else {
        None
    }
}

/// Owned extended scene: SHAPE triples plus CPU textured draws, GPU quads
/// and the payloads the worker uploads before presenting.
///
/// Public, with private fields: the export builds one through
/// [`read_ex_scene`] and hands it straight back to [`ex_frame_request`], so the
/// only thing another crate can do with it is render it.
pub struct ExScene {
    bg: [f32; 4],
    shapes: Vec<(MeshData, [f32; 4], Mat4)>,
    /// Shared, not owned: every text draw of a frame takes the same atlas
    /// `Arc`, and an image draw shares the photo it sampled.
    /// The same textured draws as `quads`, in the form the CPU oracle wants: the
    /// image, the mesh and the tint.
    ///
    /// **Test-only, and deliberately so.** No runtime path reads it — §12.4
    /// deleted the CPU composite, and the oracle that replaced it is compiled
    /// only for tests — and building it costs an `Arc::clone` plus a mesh clone
    /// for every textured draw of every frame.
    #[cfg(test)]
    textured: Vec<(Arc<TextureImage>, TexturedMesh, [f32; 4])>,
    quads: Vec<TexturedQuad>,
    textures: Vec<SceneTexture>,
    /// The scene's global draw order across `shapes` and `quads`, one entry per
    /// draw, naming the group and the index inside it.
    ///
    /// The three caller groups are typed differently but the layer list
    /// interleaves them, so this is the only thing that says *when* a draw
    /// happens. Always populated: when the caller sends no usable order arrays
    /// it is the group order (every mesh, then every textured draw), which is
    /// exactly what this scene produced before the order existed.
    draw_order: Vec<SceneDraw>,
    /// The caller's chains, re-indexed to the draw lists *this* scene actually
    /// produced.
    ///
    /// Effect chains are positional: `shapes[i]` and `textured[i]` are the draws
    /// whose chains are `shapes[i]`/`textures[i]` of the document. Dropping a
    /// draw therefore has to take its chain entry with it, or the next draw down
    /// inherits somebody else's effects (§8.13 of the contract). Every consumer
    /// of this struct — the CPU composite, the offscreen worker and the stored
    /// engine scene — indexes the *filtered* lists, so the remap happens once,
    /// here.
    chains: EffectChains,
}

/// Zip the two parallel window arrays of one draw group into windows.
///
/// Deliberately `zip`, not `enumerate`: a shorter array means "no time limit"
/// for the tail the caller did not describe, and a window that is missing is the
/// same as no window at all. A caller bug must cost an extra draw at worst, not
/// a whole frame.
fn draw_windows(starts: &[i64], durations: &[i64]) -> Vec<DrawWindow> {
    starts
        .iter()
        .zip(durations)
        .map(|(&start_ms, &duration_ms)| DrawWindow {
            start_ms,
            duration_ms,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_ex_scene(
    width: u32,
    height: u32,
    bg_argb: u32,
    time_ms: i64,
    shape: (Vec<i32>, Vec<i32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>),
    shape_scales: &[f32],
    shape_windows: &[DrawWindow],
    text: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<f32>, Vec<f32>),
    text_scales: &[f32],
    text_windows: &[DrawWindow],
    tex: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>),
    tex_windows: &[DrawWindow],
    // Parallel to the three caller groups, each entry the draw's global order
    // (the layer's position in the editor's list). Empty means "the caller
    // knows no order", and the scene keeps the group order. The values are
    // only ever *compared*, never interpreted as indices, so a caller bug
    // costs an ordering mistake at worst.
    shape_order: &[i32],
    text_order: &[i32],
    tex_order: &[i32],
    chains: &EffectChains,
) -> ExScene {
    let bg = argb_to_f32(bg_argb, 1.0);
    // The order arrays are parallel to the *caller's* group arrays, so a usable
    // set is one that covers every entry of every group. Any shorter array
    // degrades the whole scene to the group order rather than reordering part
    // of it: "no reordering" is the behaviour that shipped, and a half-applied
    // order is worse than none.
    let order_usable = shape_order.len() >= shape.0.len()
        && text_order.len() >= text.0.len()
        && tex_order.len() >= tex.0.len();
    let (o, a, dx, dy, rot, al) = shape;
    // `shape_scales` is deliberately *not* part of the strict
    // `common_len` group: a caller that predates it (or sends a short array)
    // must keep rendering, with 1.0 for every layer it did not describe.
    let spec_scale = |i: usize| -> f32 {
        shape_scales
            .get(i)
            .copied()
            .filter(|v| v.is_finite())
            .unwrap_or(1.0)
    };
    // The time window is the second gate (the editor filters before it sends the
    // arrays); a draw outside its window is dropped together with its chain
    // entry, so the survivors keep the chains they were sent.
    let mut kept_specs: Vec<ShapeSpec> = Vec::new();
    let mut kept_windows: Vec<DrawWindow> = Vec::new();
    // The caller's index of each surviving spec, kept so the effect chains can
    // be expanded to match the draws `preview_shape_draws` emits: an SVG spec
    // emits one draw per sub-mesh, so a single spec is not always one draw.
    let mut kept_source: Vec<usize> = Vec::new();
    for i in 0..o.len() {
        if !window_allows(shape_windows, i, time_ms) {
            continue;
        }
        // One slot, two disjoint things (see `SVG_ID_BASE`): a Material ordinal
        // (`< SVG_ID_BASE`) or a registered SVG id (`>= SVG_ID_BASE`). The slot
        // is copied through unchanged so `preview_shape_draws` can dispatch on
        // the range; clamping it to `max(0)` would erase that distinction.
        //
        // A negative slot is the editor's "this layer is not in the frame"
        // (`shapeSlot` returns -1 for an unknown shape or an SVG with no id
        // yet), and it has to be dropped here: `max(0)` would turn it into
        // Material ordinal 0 and silently draw the wrong shape.
        let slot = o[i];
        if slot < 0 {
            continue;
        }
        kept_specs.push(ShapeSpec {
            ordinal: slot as usize,
            argb: a[i] as u32,
            dx: dx[i],
            dy: dy[i],
            rotation_deg: rot[i],
            alpha: al[i],
            scale: spec_scale(i),
            svg_id: None,
        });
        if let Some(window) = window_of(shape_windows, i) {
            kept_windows.push(window);
        }
        kept_source.push(i);
    }
    // The windows go along with the specs they were written for, so the index
    // the preview filters by is still the caller's index.
    let draws = preview_shape_draws(width, height, time_ms, &kept_windows, &kept_specs);
    // One chain entry per *draw*: a spec that expanded to N sub-meshes carries
    // its chain N times, so the chains stay aligned with `shapes` and an SVG
    // layer does not shift the chain of every spec after it.
    let shape_source: Vec<usize> = draws
        .iter()
        .map(|&(_, _, _, source)| kept_source[source])
        .collect();
    let shapes: Vec<(MeshData, [f32; 4], Mat4)> = draws
        .into_iter()
        .map(|(mesh, color, transform, _)| (mesh, color, transform))
        .collect();
    // Every *draw*'s global order, expanded exactly like `shape_source`: all
    // sub-meshes of one SVG spec share that spec's order and stay consecutive
    // (the merge below is a stable sort), so an SVG never splits in two.
    let mesh_orders: Vec<i32> = if order_usable {
        shape_source.iter().map(|&source| shape_order[source]).collect()
    } else {
        Vec::new()
    };

    #[cfg(test)]
    let mut textured: Vec<(Arc<TextureImage>, TexturedMesh, [f32; 4])> = Vec::new();
    let mut quads: Vec<TexturedQuad> = Vec::new();
    let mut textures: Vec<SceneTexture> = Vec::new();
    // For every draw that survives, the index it had in the caller's `textures`
    // chain group: the text entries first, then the image entries, exactly in
    // the order they are pushed below.
    let mut texture_source: Vec<usize> = Vec::new();
    // The global order of every draw in `quads`, parallel to it: a text entry's
    // own order for the text draws, the image's for the images.
    let mut quad_orders: Vec<i32> = Vec::new();

    // Text layers: one shared atlas snapshot, invalid handles skipped.
    // Rotation pivots around the frame centre — same as the Kotlin
    // fallback (`rotate(rotation, pivot = center)`).
    let (handles, tx, ty, targb, talpha, trot) = text;
    let text_count = handles.len();
    let mut atlas: Option<(Arc<TextureImage>, TextureStamp)> = None;
    let mut text_drawn = false;
    for i in 0..handles.len() {
        if !window_allows(text_windows, i, time_ms) {
            continue;
        }
        let Some(layout) = layout_copy(handles[i] as u64) else {
            continue;
        };
        // The page is taken before the mesh is built, because the mesh needs
        // its size: a layout holds atlas rects and the UV is resolved against
        // the page that exists now, not the one that existed when the text was
        // laid out. Only the page is needed per draw; its stamp travels once,
        // with the payload, below.
        let (page, _stamp) = match atlas {
            Some(ref page) => page,
            None => {
                atlas = Some(stamped_atlas_page());
                atlas.as_ref().expect("atlas page taken just above")
            }
        };
        let mut mesh = text_mesh(&layout, tx[i], ty[i], page.width, page.height);
        // An animated size scales the mesh around the centre of the box the
        // layout was made for — `dx`/`dy` and the layout's own bounds, both
        // already in hand — instead of re-laying the text out every frame.
        let scale = text_scales
            .get(i)
            .copied()
            .filter(|v| v.is_finite())
            .unwrap_or(1.0)
            .max(0.0);
        scale_text_mesh(&mut mesh, tx[i], ty[i], layout.width, layout.height, scale);
        // Rotation is applied to the scaled mesh, so the pivot is the frame
        // centre whatever the size is.
        rotate_mesh(&mut mesh, width as f32 / 2.0, height as f32 / 2.0, trot[i]);
        if mesh.is_empty() {
            continue;
        }
        let tint = argb_to_f32(targb[i] as u32, talpha[i]);
        // `Arc::clone`, not `page.clone()`: the page is shared by every text
        // draw of the frame and by the GPU payload, and copying it per draw
        // cost a whole page per glyph per frame.
        #[cfg(test)]
        textured.push((Arc::clone(page), mesh.clone(), tint));
        quads.push(TexturedQuad {
            mesh,
            color: tint,
            transform: crate::composite::ndc_matrix(width as f32, height as f32),
            texture_id: ATLAS_TEXTURE_ID,
        });
        texture_source.push(i);
        if order_usable {
            quad_orders.push(text_order[i]);
        }
        text_drawn = true;
    }
    // Only ship the page when at least one text draw survived. Its stamp rides
    // along: the atlas keeps one id forever, so this is what tells the GPU
    // that a freshly rasterized glyph changed the pixels under it.
    if text_drawn {
        if let Some((page, stamp)) = atlas {
            textures.push(SceneTexture::new(ATLAS_TEXTURE_ID, stamp, page));
        }
    }

    // Image layers: registry lookup, invalid ids and degenerate rects skipped.
    let (ids, ix, iy, iw, ih, ialpha) = tex;
    for i in 0..ids.len() {
        if !window_allows(tex_windows, i, time_ms) {
            continue;
        }
        let id = ids[i] as u64;
        let tint = [1.0, 1.0, 1.0, ialpha[i].clamp(0.0, 1.0)];
        let transform = crate::composite::ndc_matrix(width as f32, height as f32);
        // A decoded frame arrives as planes rather than as an image, and its
        // UVs carry the decoder's rotation — so it gets its own mesh, built
        // from the four corner UVs of the frame's crop. It is deliberately not
        // a `SceneTexture`: there is no converted image to ship, and the worker
        // claims the staged planes itself when the draw references this id.
        if let Some(frame) = yuv_frame(id) {
            let mut mesh = TexturedMesh::new();
            mesh.push_quad_uv_corners(
                ix[i],
                iy[i],
                iw[i],
                ih[i],
                frame.corners(),
            );
            if mesh.is_empty() {
                continue;
            }
            quads.push(TexturedQuad {
                mesh,
                color: tint,
                transform,
                texture_id: id,
            });
            // Images sit after every text entry in the caller's chain group.
            texture_source.push(text_count + i);
            if order_usable {
                quad_orders.push(tex_order[i]);
            }
            continue;
        }
        let Some(image) = texture_arc(id) else {
            continue;
        };
        let mesh = image_mesh(ix[i], iy[i], iw[i], ih[i]);
        if mesh.is_empty() {
            continue;
        }
        #[cfg(test)]
        textured.push((Arc::clone(&image), mesh.clone(), tint));
        quads.push(TexturedQuad {
            mesh,
            color: tint,
            transform,
            texture_id: id,
        });
        // Registry ids are minted per upload and never reused, so the id is
        // the content generation: a still photo keeps this stamp frame after
        // frame (one upload), a video frame never does (a new id every frame).
        textures.push(SceneTexture::new(id, TextureStamp::for_content(id), image));
        // Images sit after every text entry in the caller's chain group.
        texture_source.push(text_count + i);
        if order_usable {
            quad_orders.push(tex_order[i]);
        }
    }

    // The merged global order: every draw tagged with the group it came from
    // and grouped by the order the caller sent. A stable sort means equal
    // values keep the group order (meshes first, then text, then images) and
    // the sub-meshes of one SVG stay consecutive and in their own order.
    let draw_order: Vec<SceneDraw> = if order_usable {
        let mut entries: Vec<(i32, SceneDraw)> =
            Vec::with_capacity(shapes.len() + quads.len());
        for (index, &order) in mesh_orders.iter().enumerate() {
            entries.push((order, SceneDraw::Mesh(index)));
        }
        for (index, &order) in quad_orders.iter().enumerate() {
            entries.push((order, SceneDraw::Textured(index)));
        }
        entries.sort_by_key(|&(order, _)| order);
        entries.into_iter().map(|(_, draw)| draw).collect()
    } else {
        // No usable order: the group order this scene always produced.
        (0..shapes.len())
            .map(SceneDraw::Mesh)
            .chain((0..quads.len()).map(SceneDraw::Textured))
            .collect()
    };

    ExScene {
        bg,
        shapes,
        #[cfg(test)]
        textured,
        quads,
        textures,
        draw_order,
        chains: EffectChains {
            shapes: shape_source
                .iter()
                .map(|&i| chains.shape_chain(i).to_vec())
                .collect(),
            textures: texture_source
                .iter()
                .map(|&i| chains.texture_chain(i).to_vec())
                .collect(),
            custom: chains.custom.clone(),
        },
    }
}

/// CPU composite of an [`ExScene`], **for tests only**.
///
/// This is the reference the offscreen GPU path is compared against on the host:
/// with no adapter in the build container, "the GPU frame equals the CPU frame"
/// is the only check that exists, and it is the check the whole offscreen path
/// rests on. It is deliberately unreachable from every JNI entry — §12.4 deleted
/// the runtime fallback, not the oracle.
#[cfg(test)]
fn render_ex_cpu(width: u32, height: u32, scene: &ExScene) -> Vec<u32> {
    use crate::composite::composite_preview;

    // Composites the RGBA8 draws only. A decoded frame staged as planes is not
    // among them, because composing it here would mean converting it — the very
    // work this path exists to avoid — so a plane-backed layer needs the GPU
    // path, exactly as a quad whose texture id the registry does not hold is
    // skipped here.
    let draws: Vec<(&TextureImage, &TexturedMesh, [f32; 4])> = scene
        .textured
        .iter()
        .map(|(img, mesh, tint)| (img.as_ref(), mesh, *tint))
        .collect();
    composite_preview(width, height, scene.bg, &scene.shapes, &draws)
}

/// One preview frame, from the GPU offscreen path — and only from there.
///
/// The attempt runs on the dedicated worker in [`crate::gpu_worker`] with a
/// bounded wait, because device creation must never block a JNI thread. There is
/// no CPU composite behind it any more (docs/12 §12.4): a device that cannot run
/// this path cannot preview, and saying so is better than quietly showing a
/// different renderer's idea of the frame. Every failure reason is recorded in
/// [`crate::diag`] by `gpu_worker::render` before this returns, so the
/// diagnostics window can say why.
fn render_ex_gpu(
    width: u32,
    height: u32,
    scene: &ExScene,
    time_ms: i64,
) -> Result<Vec<u32>, String> {
    let request = ex_frame_request(width, height, scene, time_ms);
    crate::gpu_worker::render(request, crate::gpu_worker::DEFAULT_TIMEOUT)
}

/// The offscreen worker's request for one frame of `scene`.
///
/// One construction site for both callers — the preview's GPU-then-CPU path and
/// the export's NV12 path — so the export cannot render a frame the preview
/// would not: the same textures, the same chains, the same timeline position.
pub fn ex_frame_request(
    width: u32,
    height: u32,
    scene: &ExScene,
    time_ms: i64,
) -> crate::gpu_worker::FrameRequest {
    crate::gpu_worker::FrameRequest {
        width,
        height,
        bg: scene.bg,
        shapes: scene.shapes.clone(),
        textured: scene.quads.clone(),
        draw_order: scene.draw_order.clone(),
        chains: scene.chains.clone(),
        textures: scene.textures.clone(),
        time_ms,
    }
}

/// Per-draw effect chains for a preview frame.
///
/// A `null` handle reads as "no effects"; so does an empty, blank or
/// unparseable JSON string ([`parse_chains_json`] errors are swallowed) — an
/// effect chain must never fail a frame. Only a genuine JNI failure to read a
/// non-null string is propagated.
fn read_chains(env: &mut jni::Env<'_>, json: &JString<'_>) -> jni::errors::Result<EffectChains> {
    let text = if json.is_null() {
        String::new()
    } else {
        json.try_to_string(env)?
    };
    Ok(rumo_core::effect::parse_chains_json(&text).unwrap_or_default())
}

/// CPU composite of an [`ExScene`] with effect chains, in the same draw order
/// [`build_ex_scene`] produced it: `scene.shapes` by index, then
/// `scene.textured` by index (text draws first, then images, exactly as they
/// were pushed). `time_ms` is the timeline position handed to the effect
/// passes in seconds.
///
/// **Tests only**, like [`render_ex_cpu`]: this is the effect-aware half of the
/// oracle, not a runtime fallback (§12.4).
#[cfg(test)]
fn render_ex_cpu_fx(
    width: u32,
    height: u32,
    scene: &ExScene,
    chains: &EffectChains,
    time_ms: i64,
) -> Vec<u32> {
    use crate::composite::{CpuLayerShape, FxCpuLayer, composite_preview_fx};

    let mut layers: Vec<FxCpuLayer<'_>> =
        Vec::with_capacity(scene.shapes.len() + scene.textured.len());
    for (index, (mesh, color, transform)) in scene.shapes.iter().enumerate() {
        layers.push(FxCpuLayer {
            shape: CpuLayerShape::Mesh {
                mesh,
                color: *color,
                transform: *transform,
            },
            effects: chains.shape_chain(index),
        });
    }
    for (index, (image, mesh, tint)) in scene.textured.iter().enumerate() {
        layers.push(FxCpuLayer {
            shape: CpuLayerShape::Textured((image.as_ref(), mesh, *tint)),
            effects: chains.texture_chain(index),
        });
    }
    composite_preview_fx(width, height, scene.bg, &layers, time_ms as f32 / 1000.0)
}

#[allow(clippy::too_many_arguments)]
fn read_ex_arrays(
    env: &mut jni::Env<'_>,
    ordinals: &JIntArray<'_>,
    argbs: &JIntArray<'_>,
    dxs: &JFloatArray<'_>,
    dys: &JFloatArray<'_>,
    rotations: &JFloatArray<'_>,
    alphas: &JFloatArray<'_>,
    text_handles: &JLongArray<'_>,
    text_x: &JFloatArray<'_>,
    text_y: &JFloatArray<'_>,
    text_argb: &JIntArray<'_>,
    text_alpha: &JFloatArray<'_>,
    text_rot: &JFloatArray<'_>,
    tex_ids: &JLongArray<'_>,
    tex_x: &JFloatArray<'_>,
    tex_y: &JFloatArray<'_>,
    tex_w: &JFloatArray<'_>,
    tex_h: &JFloatArray<'_>,
    tex_alpha: &JFloatArray<'_>,
) -> jni::errors::Result<(
    (Vec<i32>, Vec<i32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>),
    (Vec<i64>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<f32>, Vec<f32>),
    (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>),
)> {
    let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
    let o = read_i32_nullable(env, ordinals)?;
    let ab = read_i32_nullable(env, argbs)?;
    let dx = read_f32_nullable(env, dxs)?;
    let dy = read_f32_nullable(env, dys)?;
    let rot = read_f32_nullable(env, rotations)?;
    let al = read_f32_nullable(env, alphas)?;
    common_len(&[o.len(), ab.len(), dx.len(), dy.len(), rot.len(), al.len()]).ok_or_else(bad)?;

    let th = read_i64_nullable(env, text_handles)?;
    let tx = read_f32_nullable(env, text_x)?;
    let ty = read_f32_nullable(env, text_y)?;
    let ta = read_i32_nullable(env, text_argb)?;
    let tal = read_f32_nullable(env, text_alpha)?;
    let tr = read_f32_nullable(env, text_rot)?;
    common_len(&[th.len(), tx.len(), ty.len(), ta.len(), tal.len(), tr.len()]).ok_or_else(bad)?;

    let ti = read_i64_nullable(env, tex_ids)?;
    let tix = read_f32_nullable(env, tex_x)?;
    let tiy = read_f32_nullable(env, tex_y)?;
    let tiw = read_f32_nullable(env, tex_w)?;
    let tih = read_f32_nullable(env, tex_h)?;
    let tia = read_f32_nullable(env, tex_alpha)?;
    common_len(&[ti.len(), tix.len(), tiy.len(), tiw.len(), tih.len(), tia.len()])
        .ok_or_else(bad)?;

    Ok(((o, ab, dx, dy, rot, al), (th, tx, ty, ta, tal, tr), (ti, tix, tiy, tiw, tih, tia)))
}

/// Read the array groups of `nativeRenderPreviewEx` and build the scene they
/// describe, with no cross-group draw order (the group order).
///
/// Public because the export has its own entry point over the very same arrays
/// (`nativeExportWriteFrameGpu`): the frame the export encodes must be the frame
/// the preview shows, so both read the arguments here — one reader, not two that
/// could drift. Every group must share one length; `null` reads as empty, while
/// a half-`null` group is an error the caller turns into a `RuntimeException`.
///
/// The export entry point does not (yet) carry the three order arrays, so it
/// goes through [`read_ex_scene_ordered`] with none: its scene keeps the group
/// order, exactly as before this change. The preview and the surface engine
/// pass the arrays and get the merged order.
#[allow(clippy::too_many_arguments)]
pub fn read_ex_scene(
    env: &mut jni::Env<'_>,
    width: u32,
    height: u32,
    bg_argb: u32,
    time_ms: i64,
    ordinals: &JIntArray<'_>,
    argbs: &JIntArray<'_>,
    dxs: &JFloatArray<'_>,
    dys: &JFloatArray<'_>,
    rotations: &JFloatArray<'_>,
    alphas: &JFloatArray<'_>,
    shape_scales: &JFloatArray<'_>,
    text_handles: &JLongArray<'_>,
    text_x: &JFloatArray<'_>,
    text_y: &JFloatArray<'_>,
    text_argb: &JIntArray<'_>,
    text_alpha: &JFloatArray<'_>,
    text_rot: &JFloatArray<'_>,
    tex_ids: &JLongArray<'_>,
    tex_x: &JFloatArray<'_>,
    tex_y: &JFloatArray<'_>,
    tex_w: &JFloatArray<'_>,
    tex_h: &JFloatArray<'_>,
    tex_alpha: &JFloatArray<'_>,
    layer_starts: &JLongArray<'_>,
    layer_durations: &JLongArray<'_>,
    text_starts: &JLongArray<'_>,
    text_durations: &JLongArray<'_>,
    tex_starts: &JLongArray<'_>,
    tex_durations: &JLongArray<'_>,
    text_scales: &JFloatArray<'_>,
    effects_json: &JString<'_>,
) -> jni::errors::Result<ExScene> {
    read_ex_scene_ordered(
        env,
        width,
        height,
        bg_argb,
        time_ms,
        ordinals,
        argbs,
        dxs,
        dys,
        rotations,
        alphas,
        shape_scales,
        text_handles,
        text_x,
        text_y,
        text_argb,
        text_alpha,
        text_rot,
        tex_ids,
        tex_x,
        tex_y,
        tex_w,
        tex_h,
        tex_alpha,
        layer_starts,
        layer_durations,
        text_starts,
        text_durations,
        tex_starts,
        tex_durations,
        text_scales,
        None,
        None,
        None,
        effects_json,
    )
}

/// [`read_ex_scene`] with the three per-group draw-order arrays.
///
/// `shape_orders`, `text_orders` and `tex_orders` are each parallel to their
/// own group's arrays and carry that draw's position in the editor's layer
/// list. They are read leniently: `None`, `null`, empty or too short reads as
/// "no order", and [`build_ex_scene`] keeps the group order — a caller bug
/// must not panic and must not drop a draw. Splitting the reader this way keeps
/// `read_ex_scene`'s parameter list (the export's) unchanged rather than
/// overloading a field with a second meaning.
#[allow(clippy::too_many_arguments)]
pub fn read_ex_scene_ordered(
    env: &mut jni::Env<'_>,
    width: u32,
    height: u32,
    bg_argb: u32,
    time_ms: i64,
    ordinals: &JIntArray<'_>,
    argbs: &JIntArray<'_>,
    dxs: &JFloatArray<'_>,
    dys: &JFloatArray<'_>,
    rotations: &JFloatArray<'_>,
    alphas: &JFloatArray<'_>,
    shape_scales: &JFloatArray<'_>,
    text_handles: &JLongArray<'_>,
    text_x: &JFloatArray<'_>,
    text_y: &JFloatArray<'_>,
    text_argb: &JIntArray<'_>,
    text_alpha: &JFloatArray<'_>,
    text_rot: &JFloatArray<'_>,
    tex_ids: &JLongArray<'_>,
    tex_x: &JFloatArray<'_>,
    tex_y: &JFloatArray<'_>,
    tex_w: &JFloatArray<'_>,
    tex_h: &JFloatArray<'_>,
    tex_alpha: &JFloatArray<'_>,
    layer_starts: &JLongArray<'_>,
    layer_durations: &JLongArray<'_>,
    text_starts: &JLongArray<'_>,
    text_durations: &JLongArray<'_>,
    tex_starts: &JLongArray<'_>,
    tex_durations: &JLongArray<'_>,
    text_scales: &JFloatArray<'_>,
    shape_orders: Option<&JIntArray<'_>>,
    text_orders: Option<&JIntArray<'_>>,
    tex_orders: Option<&JIntArray<'_>>,
    effects_json: &JString<'_>,
) -> jni::errors::Result<ExScene> {
    let (shape, text, tex) = read_ex_arrays(
        env,
        ordinals,
        argbs,
        dxs,
        dys,
        rotations,
        alphas,
        text_handles,
        text_x,
        text_y,
        text_argb,
        text_alpha,
        text_rot,
        tex_ids,
        tex_x,
        tex_y,
        tex_w,
        tex_h,
        tex_alpha,
    )?;
    let chains = read_chains(env, effects_json)?;
    let scales = read_f32_nullable(env, shape_scales)?;
    let text_scale = read_f32_nullable(env, text_scales)?;
    let shape_windows = read_windows(env, layer_starts, layer_durations)?;
    let text_windows = read_windows(env, text_starts, text_durations)?;
    let tex_windows = read_windows(env, tex_starts, tex_durations)?;
    let empty: Vec<i32> = Vec::new();
    let shape_order = match shape_orders {
        Some(arr) => read_i32_nullable(env, arr)?,
        None => empty.clone(),
    };
    let text_order = match text_orders {
        Some(arr) => read_i32_nullable(env, arr)?,
        None => empty.clone(),
    };
    let tex_order = match tex_orders {
        Some(arr) => read_i32_nullable(env, arr)?,
        None => empty,
    };
    Ok(build_ex_scene(
        width,
        height,
        bg_argb,
        time_ms,
        shape,
        &scales,
        &shape_windows,
        text,
        &text_scale,
        &text_windows,
        tex,
        &tex_windows,
        &shape_order,
        &text_order,
        &tex_order,
        &chains,
    ))
}

/// `RumoBridge.nativeRenderPreviewEx(...)`: composite SHAPE + text + image
/// layers to a `width`×`height` 0xAARRGGBB int array (same exit pack as
/// `nativeRenderPreview`).
///
/// Every array group (6 SHAPE, 6 text, 6 image arrays) must share one length;
/// `null` reads as empty, so all-`null` means "no layers of that kind" while
/// a half-`null` group throws `RuntimeException`. Unknown text handles and
/// texture ids are skipped, never fatal. CPU reference compositor (always
/// available); the GPU frame is the engine surface path
/// (`nativeEngineSetLayersEx` + `renderFrame`) and
/// [`crate::composite::composite_preview_gpu`].
///
/// `effects_json` is the optional `EffectChains` document
/// (`{"shapes":[[…]],"textures":[[…]]}`); `null`, empty or unparseable means
/// "no effects" and never fails the frame. `time_ms` is the timeline position
/// for the effect passes, in milliseconds. The dispatch is on whether a chain
/// document was supplied **at all**: with `null`/empty the frame goes through
/// the unchanged [`composite_preview`] path, otherwise every layer is rasterised
/// into its own buffer and [`composite_preview_fx`] applies that layer's chain
/// to that layer alone. A chain that parses but contains nothing usable counts
/// as "no chain" and takes the unchanged path.
///
/// `shape_scales` is the optional per-SHAPE-layer uniform size multiplier,
/// parallel to the 6 SHAPE arrays. It is read leniently — `null`, short or
/// non-finite entries fall back to `1.0` per layer — so a caller that does not
/// know about it still renders the legacy 60%-of-height geometry.
///
/// The six `*_starts`/`*_durations` arrays (§11.4) carry each draw's place on
/// the timeline, parallel to the SHAPE arrays, the text entries and the image
/// entries respectively, and are read leniently: `null`, empty or short means
/// "no time limit", never "not drawn". `text_scales` is the per-text-entry size
/// multiplier, read the same lenient way and defaulting to `1.0`.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderPreviewEx(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    width: jint,
    height: jint,
    bg_argb: jint,
    ordinals: JIntArray<'_>,
    argbs: JIntArray<'_>,
    dxs: JFloatArray<'_>,
    dys: JFloatArray<'_>,
    rotations: JFloatArray<'_>,
    alphas: JFloatArray<'_>,
    shape_scales: JFloatArray<'_>,
    text_handles: JLongArray<'_>,
    text_x: JFloatArray<'_>,
    text_y: JFloatArray<'_>,
    text_argb: JIntArray<'_>,
    text_alpha: JFloatArray<'_>,
    text_rot: JFloatArray<'_>,
    tex_ids: JLongArray<'_>,
    tex_x: JFloatArray<'_>,
    tex_y: JFloatArray<'_>,
    tex_w: JFloatArray<'_>,
    tex_h: JFloatArray<'_>,
    tex_alpha: JFloatArray<'_>,
    layer_starts: JLongArray<'_>,
    layer_durations: JLongArray<'_>,
    text_starts: JLongArray<'_>,
    text_durations: JLongArray<'_>,
    tex_starts: JLongArray<'_>,
    tex_durations: JLongArray<'_>,
    text_scales: JFloatArray<'_>,
    layer_orders: JIntArray<'_>,
    text_orders: JIntArray<'_>,
    tex_orders: JIntArray<'_>,
    effects_json: JString<'_>,
    time_ms: jlong,
) -> jintArray {
    env.with_env(|env| -> jni::errors::Result<jintArray> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            return Err(bad());
        }
        let scene = read_ex_scene_ordered(
            env,
            width as u32,
            height as u32,
            bg_argb as u32,
            time_ms,
            &ordinals,
            &argbs,
            &dxs,
            &dys,
            &rotations,
            &alphas,
            &shape_scales,
            &text_handles,
            &text_x,
            &text_y,
            &text_argb,
            &text_alpha,
            &text_rot,
            &tex_ids,
            &tex_x,
            &tex_y,
            &tex_w,
            &tex_h,
            &tex_alpha,
            &layer_starts,
            &layer_durations,
            &text_starts,
            &text_durations,
            &tex_starts,
            &tex_durations,
            &text_scales,
            Some(&layer_orders),
            Some(&text_orders),
            Some(&tex_orders),
            &effects_json,
        )?;
        // The scene's own chains: the same document, re-indexed onto the draws
        // that survived the windows, so no surviving draw inherits a dropped
        // one's effects.
        let Ok(px) = render_ex_gpu(width as u32, height as u32, &scene, time_ms) else {
            // No frame, and no CPU frame either: the fallback is gone (docs/12
            // §12.4). `null` is the honest answer, and the reason is already in
            // the diagnostics log — the caller shows the GPU-unavailable state
            // rather than a different renderer's approximation of the frame.
            return Ok(std::ptr::null_mut());
        };
        let out: Vec<jint> = px.into_iter().map(|v| rgba_u32_to_argb(v) as jint).collect();
        let arr = JIntArray::new(env, out.len())?;
        arr.set_region(env, 0, &out)?;
        Ok(arr.as_raw() as jintArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeEngineSetLayersEx(engine, ...)`: store the extended
/// scene (SHAPE + text + image) the engine presents on each render frame.
/// Same array contract as `nativeRenderPreviewEx`; tessellation and atlas
/// snapshots run on the calling thread, only GPU submission happens on the
/// worker. Errors throw `RuntimeException`.
///
/// `effects_json` (`EffectChains`, `null`/empty/unparseable = no effects) is
/// attached to the stored scene with [`EngineScene::with_chains`], and
/// `time_ms` becomes the scene's timeline position, which the effect-aware
/// render paths hand to the shaders as seconds. With no usable chain the scene
/// keeps the direct (effect-free) draw path. `shape_scales` is the optional
/// per-SHAPE-layer size multiplier; see `nativeRenderPreviewEx`. The six
/// `*_starts`/`*_durations` arrays and `text_scales` are read the same lenient
/// way (§11.4): the windows are evaluated against `time_ms`, the instant the
/// stored scene is built for, so a layer that has not started yet stays out of
/// the scene the engine will keep showing.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSetLayersEx(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
    width: jint,
    height: jint,
    bg_argb: jint,
    ordinals: JIntArray<'_>,
    argbs: JIntArray<'_>,
    dxs: JFloatArray<'_>,
    dys: JFloatArray<'_>,
    rotations: JFloatArray<'_>,
    alphas: JFloatArray<'_>,
    shape_scales: JFloatArray<'_>,
    text_handles: JLongArray<'_>,
    text_x: JFloatArray<'_>,
    text_y: JFloatArray<'_>,
    text_argb: JIntArray<'_>,
    text_alpha: JFloatArray<'_>,
    text_rot: JFloatArray<'_>,
    tex_ids: JLongArray<'_>,
    tex_x: JFloatArray<'_>,
    tex_y: JFloatArray<'_>,
    tex_w: JFloatArray<'_>,
    tex_h: JFloatArray<'_>,
    tex_alpha: JFloatArray<'_>,
    layer_starts: JLongArray<'_>,
    layer_durations: JLongArray<'_>,
    text_starts: JLongArray<'_>,
    text_durations: JLongArray<'_>,
    tex_starts: JLongArray<'_>,
    tex_durations: JLongArray<'_>,
    text_scales: JFloatArray<'_>,
    layer_orders: JIntArray<'_>,
    text_orders: JIntArray<'_>,
    tex_orders: JIntArray<'_>,
    effects_json: JString<'_>,
    time_ms: jlong,
) {
    let result = env.with_env(|env| -> jni::errors::Result<()> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            return Err(bad());
        }
        let Some(engine) = engine_from_handle(engine) else {
            return Err(bad());
        };
        let scene = read_ex_scene_ordered(
            env,
            width as u32,
            height as u32,
            bg_argb as u32,
            time_ms,
            &ordinals,
            &argbs,
            &dxs,
            &dys,
            &rotations,
            &alphas,
            &shape_scales,
            &text_handles,
            &text_x,
            &text_y,
            &text_argb,
            &text_alpha,
            &text_rot,
            &tex_ids,
            &tex_x,
            &tex_y,
            &tex_w,
            &tex_h,
            &tex_alpha,
            &layer_starts,
            &layer_durations,
            &text_starts,
            &text_durations,
            &tex_starts,
            &tex_durations,
            &text_scales,
            Some(&layer_orders),
            Some(&text_orders),
            Some(&tex_orders),
            &effects_json,
        )?;
        let ExScene {
            bg,
            shapes,
            quads,
            textures,
            draw_order,
            chains,
            ..
        } = scene;
        let mut stored = EngineScene::new_ex(width as u32, height as u32, bg, shapes, quads, textures)
            // The chains the scene kept, not the raw document: the worker
            // indexes them by the draws that survived the windows.
            .with_chains(chains)
            // And the global order the scene kept, so the engine draws the
            // layers where the editor's list puts them, across group borders.
            .with_draw_order(draw_order);
        stored.time_ms = time_ms;
        if engine.set_scene(stored) != ENGINE_OK {
            return Err(bad());
        }
        Ok(())
    });
    let _ = result.resolve::<ThrowRuntimeExAndDefault>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::LayerShape;
    use rumo_core::effect::{EffectInstance, EffectKind, EffectTarget};

    fn sample_quad() -> GlyphQuad {
        GlyphQuad {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
            rect: crate::AtlasRect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
        }
    }

    #[test]
    fn resolution_presets_json_covers_every_table_entry() {
        use rumo_core::canvas::{ASPECTS, FPS_CHOICES, PRESETS};
        let json = resolution_presets_json();
        assert!(json.starts_with('{') && json.ends_with('}'), "object shape");
        assert_eq!(
            json.matches(r#""id":""#).count(),
            PRESETS.len(),
            "one preset entry per table row"
        );
        for p in PRESETS {
            assert!(json.contains(&format!(r#""id":"{}""#, p.id)), "missing {}", p.id);
            assert!(json.contains(p.label), "missing label {}", p.label);
            assert!(
                json.contains(&format!(r#""width":{},"height":{}"#, p.width, p.height)),
                "missing size of {}",
                p.id
            );
        }
        for (label, w, h) in ASPECTS {
            assert!(
                json.contains(&format!(r#"{{"label":"{label}","w":{w},"h":{h}}}"#)),
                "missing aspect {label}"
            );
        }
        let fps_expected: Vec<String> = FPS_CHOICES.iter().map(|f| f.to_string()).collect();
        assert!(
            json.contains(&format!(r#""fps":[{}]"#, fps_expected.join(","))),
            "fps list must be the FPS_CHOICES table in order"
        );
    }

    #[test]
    fn effect_validate_json_reports_shape_and_errors() {
        let json = r#"{
            "id": "tint_fx",
            "label": "Tint",
            "space": "display",
            "passes": [{"entry": "fs_main", "shrink": 0}],
            "params": [{"key": "tint", "label": "Tint", "kind": "color",
                        "min": 0.0, "max": 1.0, "default": [0.0, 1.0, 0.0, 1.0],
                        "unit": "", "choices": []}],
            "source": "struct Params { tint_r: f32, tint_g: f32, tint_b: f32, tint_a: f32 }\n@group(1) @binding(0) var<uniform> params: Params;\n@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return vec4<f32>(params.tint_r, params.tint_g, params.tint_b, params.tint_a); }"
        }"#;
        let reply: serde_json::Value =
            serde_json::from_str(&effect_validate_json(json)).expect("valid reply json");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["id"], "tint_fx");
        assert_eq!(reply["label"], "Tint");
        assert_eq!(reply["slots"], 4);
        assert_eq!(reply["block_len"], 4);
        assert_eq!(reply["passes"], 1);
        assert_eq!(reply["fields"][0], "tint_r");
        assert_eq!(reply["fields"][3], "tint_a");
        assert_eq!(reply["params"][0]["key"], "tint");
        assert_eq!(reply["params"][0]["slots"], 4);
        assert_eq!(reply["params"][0]["slot"], 0);

        // Malformed JSON, a structural rejection and a WGSL mismatch all reply
        // with ok:false and a user-safe message, never a panic.
        let bad: serde_json::Value =
            serde_json::from_str(&effect_validate_json("not json")).unwrap();
        assert_eq!(bad["ok"], false);
        assert!(bad["error"].as_str().unwrap().contains("malformed"));

        let collision: serde_json::Value =
            serde_json::from_str(&effect_validate_json(&json.replace("tint_fx", "blur"))).unwrap();
        assert_eq!(collision["ok"], false);
        assert!(collision["error"].as_str().unwrap().contains("built-in"));

        // A `Params` whose fields do not match the declared parameters in order
        // is the most common authoring mistake; it must be refused by name.
        let mismatch_source = json
            .replace("tint_r: f32", "other: f32")
            .replace("params.tint_r", "params.other");
        let mismatch: serde_json::Value =
            serde_json::from_str(&effect_validate_json(&mismatch_source)).unwrap();
        assert_eq!(mismatch["ok"], false);
        assert!(mismatch["error"].as_str().unwrap().contains("Params"));
    }

    #[test]
    fn a_params_struct_smaller_than_its_padded_block_is_accepted() {
        // Two scalars pack into a four-`f32` block, so the struct is half the
        // buffer the host binds. Every built-in module is written this way
        // (`Pixelate` declares `size` and `shape` for exactly this block), and a
        // struct smaller than its buffer is legal — so this must validate. A
        // check written against the *padded* length instead would refuse a
        // module written in the style of the effects that already ship.
        let json = r#"{
            "id": "grid_fx",
            "label": "Grid",
            "space": "display",
            "passes": [{"entry": "fs_main", "shrink": 0}],
            "params": [
                {"key": "cell", "label": "Cell", "kind": "float", "min": 1.0, "max": 64.0,
                 "default": [8.0, 0.0, 0.0, 0.0], "unit": "px", "choices": []},
                {"key": "line", "label": "Line", "kind": "float", "min": 0.0, "max": 1.0,
                 "default": [0.5, 0.0, 0.0, 0.0], "unit": "", "choices": []}
            ],
            "source": "struct Params { cell: f32, line: f32 }\n@group(1) @binding(0) var<uniform> params: Params;\n@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return vec4<f32>(params.cell, params.line, 0.0, 1.0); }"
        }"#;
        let reply: serde_json::Value =
            serde_json::from_str(&effect_validate_json(json)).expect("valid reply json");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["slots"], 2);
        assert_eq!(reply["block_len"], 4);
        assert_eq!(reply["fields"][0], "cell");
        assert_eq!(reply["fields"][1], "line");

        // And a struct that is genuinely too small for its own declared fields
        // is still refused: here the second field is missing from the module.
        let short = json.replace(", line: f32 }", " }").replace(", params.line", "");
        let refused: serde_json::Value =
            serde_json::from_str(&effect_validate_json(&short)).unwrap();
        assert_eq!(refused["ok"], false, "{refused}");
    }

    #[test]
    fn json_escape_quotes_and_controls() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("a\nb\tc"), r#"a\nb\tc"#);
        assert_eq!(json_escape("\u{1}"), r#"\u0001"#);
        assert_eq!(json_escape("plain label (SD, portrait)"), "plain label (SD, portrait)");
    }

    /// `nativeSvgValidate`'s JSON body: a document the parser accepts reports
    /// its own numbers, and `skipped` is the count Kotlin parses as an `Int`.
    #[test]
    fn svg_validate_json_reports_document_numbers() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80">
            <rect x="0" y="0" width="40" height="40" fill="#ff0000"/>
            <rect x="60" y="30" width="40" height="40" fill="#0000ff"/>
        </svg>"##;
        let reply: serde_json::Value =
            serde_json::from_str(&svg_validate_json(svg)).expect("valid reply json");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["width"], 120.0);
        assert_eq!(reply["height"], 80.0);
        assert_eq!(reply["shapes"], 2);
        assert_eq!(reply["flattenedGradients"], 0);
        assert_eq!(reply["skipped"], 0);
    }

    /// A flattened gradient is counted, so the tool can say the colour survives
    /// but the transition does not.
    #[test]
    fn svg_validate_json_counts_flattened_gradients() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <defs>
                <linearGradient id="g">
                    <stop offset="0" stop-color="#000000"/>
                    <stop offset="1" stop-color="#ffffff"/>
                </linearGradient>
            </defs>
            <rect width="10" height="10" fill="url(#g)"/>
        </svg>"##;
        let reply: serde_json::Value =
            serde_json::from_str(&svg_validate_json(svg)).expect("valid reply json");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["shapes"], 1);
        assert_eq!(reply["flattenedGradients"], 1);
    }

    /// Malformed input is a JSON verdict with a non-empty reason, never a panic
    /// and never invalid JSON (the reason is escaped).
    #[test]
    fn svg_validate_json_rejects_malformed_input() {
        for bad in [&b"definitely not an svg"[..], &[0xFF, 0xFE, 0xFD][..]] {
            let reply: serde_json::Value =
                serde_json::from_str(&svg_validate_json(bad)).expect("valid reply json");
            assert_eq!(reply["ok"], false, "{reply}");
            let reason = reply["error"].as_str().expect("an error string");
            assert!(!reason.is_empty(), "the reason must name the failure");
        }
    }

    /// The frame path reads the SHAPE slot as one of two disjoint things: a
    /// Material ordinal or a registered SVG id. A `-1` slot means "not in the
    /// frame" and must not be clamped to ordinal 0, and an unregistered id in
    /// the SVG range must draw nothing.
    #[test]
    fn ex_scene_interprets_the_shape_slot_ranges() {
        let build = |ordinal: i32| {
            build_ex_scene(
                64,
                36,
                0xFF141824,
                0,
                (
                    vec![ordinal],
                    vec![0xFFFF9800u32 as i32],
                    vec![0.0],
                    vec![0.0],
                    vec![0.0],
                    vec![1.0],
                ),
                &[],
                &[],
                (vec![], vec![], vec![], vec![], vec![], vec![]),
                &[],
                &[],
                (vec![], vec![], vec![], vec![], vec![], vec![]),
                &[],
                &[],
                &[],
                &[],
                &EffectChains::default(),
            )
        };
        // A negative slot is dropped, not read as Material ordinal 0.
        assert!(build(-1).shapes.is_empty(), "-1 means 'not in the frame'");
        // A registered SVG's id in that slot draws the document's sub-meshes.
        let id = crate::composite::register_svg(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
                <rect x="0" y="0" width="40" height="40" fill="#ff0000"/>
                <rect x="60" y="60" width="40" height="40" fill="#0000ff"/>
            </svg>"##,
        )
        .expect("register");
        assert_eq!(build(id).shapes.len(), 2, "the SVG's two rects draw");
        assert!(crate::composite::free_svg(id));
        // An id in the SVG range that was never registered is skipped.
        let missing = crate::composite::SVG_ID_BASE + 999_999;
        assert!(
            build(missing).shapes.is_empty(),
            "an unregistered SVG slot draws nothing"
        );
    }

    #[test]
    fn upload_rejects_malformed() {
        assert!(upload_image(0, 0, Vec::new()).is_none());
        assert!(upload_image(2, 2, vec![0u8; 15]).is_none());
    }

    #[test]
    fn upload_get_free_roundtrip() {
        let id = upload_image(1, 1, vec![1, 2, 3, 4]).expect("upload");
        assert!(id >= 1);
        let image = texture_image(id).expect("image");
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.rgba, vec![1, 2, 3, 4]);
        assert!(free_texture(id));
        assert!(texture_image(id).is_none());
        assert!(!free_texture(id), "double free must be false");
    }

    #[test]
    fn common_len_accepts_all_empty_and_rejects_mismatch() {
        assert_eq!(common_len(&[0, 0, 0]), Some(0));
        assert_eq!(common_len(&[2, 2, 2, 2]), Some(2));
        assert_eq!(common_len(&[1, 1, 0]), None);
        assert_eq!(common_len(&[3, 2]), None);
    }

    #[test]
    fn unknown_handles_snapshot_none() {
        assert!(layout_copy(u64::MAX).is_none());
        assert!(layout_snapshot(u64::MAX).is_none());
        assert!(texture_image(u64::MAX).is_none());
        assert!(engine_from_handle(0).is_none());
    }

    #[test]
    fn layout_snapshot_returns_quads_and_atlas() {
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let (layout, atlas) = layout_snapshot(handle).expect("snapshot");
        assert_eq!(layout.quads, vec![sample_quad()]);
        assert!(atlas.width >= 1 && atlas.height >= 1);
        assert_eq!(atlas.rgba.len(), atlas.width as usize * atlas.height as usize * 4);
        assert!(free_layout(handle));
        assert!(layout_snapshot(handle).is_none());
    }

    #[test]
    fn ex_scene_skips_bad_handles_and_degenerate_rects() {
        // Unknown text handle + unknown texture id + zero-size rect:
        // everything skipped, shapes still built.
        let scene = build_ex_scene(
            64,
            36,
            0xFF141824,
            0,
            (vec![0], vec![0xFFFF9800u32 as i32], vec![0.0], vec![0.0], vec![0.0], vec![1.0]),
            &[1.0],
            &[],
            (vec![i64::MAX], vec![0.0], vec![0.0], vec![0xFFFF0000u32 as i32], vec![1.0], vec![0.0]),
            &[],
            &[],
            (
                vec![i64::MAX, 1],
                vec![0.0, 0.0],
                vec![0.0, 0.0],
                vec![4.0, 0.0],
                vec![4.0, 4.0],
                vec![1.0, 1.0],
            ),
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        assert_eq!(scene.shapes.len(), 1, "valid shape survives");
        assert!(scene.textured.is_empty());
        assert!(scene.quads.is_empty());
        assert!(scene.textures.is_empty());
        let px = render_ex_cpu(64, 36, &scene);
        assert_eq!(px.len(), 64 * 36);
        // Circle paints the centre even with all-extra layers invalid.
        assert_ne!(px[18 * 64 + 32], crate::composite::f32_to_rgba_u32(crate::composite::argb_to_f32(0xFF141824, 1.0)));
    }

    #[test]
    fn one_atlas_page_backs_every_text_draw_and_is_shipped_once() {
        // Two text layers, one page. The draws and the GPU payload must all hold
        // the *same* page: cloning it per draw was a whole page per glyph per
        // frame, and the payload carries the stamp the GPU cache compares.
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let scene = build_ex_scene(
            64,
            36,
            0xFF141824,
            0,
            (vec![], vec![], vec![], vec![], vec![], vec![]),
            &[],
            &[],
            (
                vec![handle as i64, handle as i64],
                vec![0.0, 8.0],
                vec![0.0, 8.0],
                vec![0xFFFFFFFFu32 as i32, 0xFFFFFFFFu32 as i32],
                vec![1.0, 1.0],
                vec![0.0, 0.0],
            ),
            &[],
            &[],
            (vec![], vec![], vec![], vec![], vec![], vec![]),
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        assert_eq!(scene.textured.len(), 2, "both text layers draw");
        assert_eq!(
            scene.textures.len(),
            1,
            "one page is one payload, however many draws use it"
        );
        assert_eq!(scene.textures[0].id, ATLAS_TEXTURE_ID);
        assert!(
            Arc::ptr_eq(&scene.textured[0].0, &scene.textured[1].0),
            "both draws share one page"
        );
        assert!(
            Arc::ptr_eq(&scene.textured[0].0, &scene.textures[0].image),
            "the GPU payload shares the page the draws sample"
        );
        // The atlas lives under a constant id, so its stamp has to come from the
        // page itself — the tagged content family would say "same pixels
        // forever" and freeze the first glyphs ever rasterized.
        assert_eq!(
            scene.textures[0].stamp.as_u64() & (1 << 63),
            0,
            "the atlas ships an atlas-family stamp"
        );
        for quad in &scene.quads {
            assert_eq!(quad.texture_id, ATLAS_TEXTURE_ID);
        }
        assert!(free_layout(handle));
    }

    #[test]
    fn ex_scene_empty_extras_match_shapes_only() {
        let shape = (vec![0], vec![0xFFFF9800u32 as i32], vec![0.0], vec![0.0], vec![0.0], vec![1.0]);
        let full = build_ex_scene(
            64,
            36,
            0xFF141824,
            0,
            shape.clone(),
            // Short scale array on purpose: a caller from before this
            // parameter must still render the legacy geometry.
            &[],
            &[],
            (vec![], vec![], vec![], vec![], vec![], vec![]),
            &[],
            &[],
            (vec![], vec![], vec![], vec![], vec![], vec![]),
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        let legacy_shapes = crate::composite::preview_shape_triples(
            64,
            36,
            0,
            &[],
            &[crate::composite::ShapeSpec {
                ordinal: 0,
                argb: 0xFFFF9800,
                dx: 0.0,
                dy: 0.0,
                rotation_deg: 0.0,
                alpha: 1.0,
                scale: 1.0,
                svg_id: None,
            }],
        );
        let draws: Vec<(&TextureImage, &crate::texture::TexturedMesh, [f32; 4])> = vec![];
        let expect = crate::composite::composite_preview(
            64,
            36,
            crate::composite::argb_to_f32(0xFF141824, 1.0),
            &legacy_shapes,
            &draws,
        );
        assert_eq!(render_ex_cpu(64, 36, &full), expect);
    }

    /// Two SHAPE draws that are both visible at 0 ms, with a chain on each: the
    /// first layer's blur must stay on the first layer when the middle one is
    /// filtered out by its time window.
    #[test]
    fn a_filtered_layer_takes_its_chain_entry_with_it() {
        let chains = EffectChains {
            shapes: vec![
                vec![EffectInstance::new(EffectKind::Blur)],
                vec![EffectInstance::new(EffectKind::Glow)],
                vec![EffectInstance::new(EffectKind::Pixelate)],
            ],
            textures: Vec::new(),
            custom: Vec::new(),
        };
        // Three layers, one after another. The editor filters its own array the
        // same way, but this entry point must not rely on it.
        let windows = [
            DrawWindow {
                start_ms: 0,
                duration_ms: 100,
            },
            DrawWindow {
                start_ms: 100,
                duration_ms: 100,
            },
            DrawWindow {
                start_ms: 200,
                duration_ms: 100,
            },
        ];
        let layer_arrays = || {
            (
                vec![0, 0, 0],
                vec![0xFFFF9800u32 as i32; 3],
                vec![0.0, -8.0, 8.0],
                vec![0.0; 3],
                vec![0.0; 3],
                vec![1.0; 3],
            )
        };
        // Two empty groups, two element types: the text group starts with an
        // `i64` handle and carries an `i32` argb, the image group starts with an
        // `i64` handle and has no argb. One shared value would pin a single type
        // and fail on the other slot.
        let no_text: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let no_tex: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let at = |t: i64| {
            build_ex_scene(
                64,
                36,
                0xFF141824,
                t,
                layer_arrays(),
                &[],
                &windows,
                no_text.clone(),
                &[],
                &[],
                no_tex.clone(),
                &[],
                &[],
                &[],
                &[],
                &chains,
            )
        };

        // At 0 ms only the first layer is in frame; the middle one has not
        // started and the third one has not either.
        let first = at(0);
        assert_eq!(first.shapes.len(), 1, "the other two are out of frame");
        // The filtered layers took their chain entries with them, so the
        // survivor keeps the *first* layer's chain and inherits neither the
        // glow nor the third layer's entry.
        assert_eq!(
            first.chains.shapes.len(),
            first.shapes.len(),
            "one chain entry per surviving draw"
        );
        assert_eq!(first.chains.shapes[0], chains.shapes[0]);
        assert_eq!(
            chain_kind(&first, 0),
            EffectTarget::Builtin(EffectKind::Blur),
            "the first layer keeps its own blur"
        );

        // At 150 ms the second layer is the only one in frame, and it gets its
        // own chain — the first layer's entry would have been somebody else's
        // effect applied to it.
        let second = at(150);
        assert_eq!(second.shapes.len(), 1);
        assert_eq!(second.chains.shapes[0], chains.shapes[1]);
        assert_eq!(chain_kind(&second, 0), EffectTarget::Builtin(EffectKind::Glow));

        // At 250 ms the third one, with the third one's chain.
        let third = at(250);
        assert_eq!(third.shapes.len(), 1);
        assert_eq!(third.chains.shapes[0], chains.shapes[2]);
        assert_eq!(
            chain_kind(&third, 0),
            EffectTarget::Builtin(EffectKind::Pixelate)
        );

        // Past the end of the last layer nothing is left, and there is no chain
        // entry to go wrong either.
        let over = at(300);
        assert!(over.shapes.is_empty());
        assert!(over.chains.shapes.is_empty());
        assert!(over.chains.is_empty(), "an empty scene needs no chains");
    }

    /// The `target` of a draw's chain, so "this draw kept *its own* chain" is
    /// readable: the three layers below carry three different effects.
    fn chain_kind(scene: &ExScene, index: usize) -> EffectTarget {
        scene.chains.shapes[index]
            .first()
            .expect("the chain is not empty")
            .target
            .clone()
    }

    #[test]
    fn text_scale_moves_the_mesh_about_the_layout_centre() {
        // The text mesh reaches the frame through `build_ex_scene`, so this goes
        // through the same call: a text entry at half size must occupy half the
        // width around the centre of its own layout box.
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 20.0,
            height: 10.0,
        });
        let text = (
            vec![handle as i64],
            vec![10.0],
            vec![6.0],
            vec![0xFFFF0000u32 as i32],
            vec![1.0],
            vec![0.0],
        );
        let empty_tex = (vec![], vec![], vec![], vec![], vec![], vec![]);
        let build = |scales: &[f32]| {
            build_ex_scene(
                64,
                36,
                0xFF141824,
                0,
                (vec![], vec![], vec![], vec![], vec![], vec![]),
                &[],
                &[],
                text.clone(),
                scales,
                &[],
                empty_tex.clone(),
                &[],
                &[],
                &[],
                &[],
                &EffectChains::default(),
            )
        };
        let plain = build(&[1.0]);
        let half = build(&[0.5]);
        assert_eq!(plain.textured.len(), 1);
        assert_eq!(half.textured.len(), 1);
        let plain_mesh = &plain.textured[0].1;
        let half_mesh = &half.textured[0].1;
        // The anchor is the centre of the box the text was laid out for, from
        // the corner and the layout size the caller already sent — not the
        // corner, and not the mesh's own extent.
        let anchor = [10.0 + 20.0 / 2.0, 6.0 + 10.0 / 2.0];
        for (a, b) in plain_mesh.vertices.iter().zip(half_mesh.vertices.iter()) {
            for k in 0..2 {
                let want = anchor[k] + (a.position[k] - anchor[k]) * 0.5;
                assert!(
                    (b.position[k] - want).abs() < 1e-4,
                    "vertex {k}: {} must become {want}, not {}",
                    a.position[k],
                    b.position[k]
                );
            }
        }
        assert!(
            plain_mesh.vertices
                .iter()
                .zip(half_mesh.vertices.iter())
                .any(|(a, b)| (b.position[0] - a.position[0]).abs() > 1e-4),
            "and the mesh actually moved"
        );
        // A scale of 1.0 changes nothing: the layout is rendered as it stands.
        assert_eq!(build(&[1.0]).textured[0].1, *plain_mesh);
        // No scale at all, a short array and a non-finite entry mean the same.
        assert_eq!(build(&[]).textured[0].1, *plain_mesh);
        assert_eq!(build(&[f32::NAN]).textured[0].1, *plain_mesh);
        assert!(free_layout(handle));
    }

    #[test]
    fn a_layer_outside_its_window_is_not_drawn() {
        // The window is a second gate behind the editor's own filtering, and it
        // must never be *wider* than the editor's decision: a layer that has
        // not started yet, or has already ended, contributes nothing to the
        // frame — and a zero duration contributes nothing ever.
        let shape = |n: usize| {
            (
                vec![0; n],
                vec![0xFFFF9800u32 as i32; n],
                vec![0.0; n],
                vec![0.0; n],
                vec![0.0; n],
                vec![1.0; n],
            )
        };
        let no_text: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let no_tex: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let at = |t: i64, windows: &[DrawWindow]| {
            build_ex_scene(
                64,
                36,
                0xFF141824,
                t,
                shape(1),
                &[],
                windows,
                no_text.clone(),
                &[],
                &[],
                no_tex.clone(),
                &[],
                &[],
                &[],
                &[],
                &EffectChains::default(),
            )
            .shapes
            .len()
        };
        let window = |start_ms: i64, duration_ms: i64| DrawWindow { start_ms, duration_ms };
        assert_eq!(at(0, &[]), 1, "no window at all means no limit");
        assert_eq!(at(10_000, &[]), 1, "even far past the end");
        assert_eq!(at(0, &[window(100, 100)]), 0, "before it starts");
        assert_eq!(at(99, &[window(100, 100)]), 0);
        assert_eq!(at(100, &[window(100, 100)]), 1, "the start instant belongs to it");
        assert_eq!(at(199, &[window(100, 100)]), 1);
        assert_eq!(at(200, &[window(100, 100)]), 0, "the end instant does not");
        assert_eq!(at(10_000, &[window(100, 100)]), 0);
        assert_eq!(at(0, &[window(0, 0)]), 0, "a zero length draws never");
        // A window that *is* present bounds its draw, however many further
        // windows follow: this scene has one shape, so the second window belongs
        // to a draw that is not here and cannot rescue the first.
        assert_eq!(at(10_000, &[window(100, 100), window(200, 100)]), 0);
        assert_eq!(at(0, &[window(100, 100), window(200, 100)]), 0);
    }

    #[test]
    fn layout_handle_roundtrip() {
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        assert!(handle >= 1);
        assert_eq!(layout_quad_count(handle), 1);
        assert_eq!(layout_quad(handle, 0), Some(sample_quad()));
        assert_eq!(layout_quad(handle, 1), None);
        assert_eq!(layout_bounds(handle), Some((3.0, 4.0)));
        assert!(free_layout(handle));
        assert_eq!(layout_quad_count(handle), 0);
        assert!(!free_layout(handle));
    }

    /// Write a composited frame as a PNG, so a broken render can be looked at.
    ///
    /// `rgba_u32_to_argb`-free on purpose: the frame is packed `u32` RGBA8 and
    /// that is what the engine speaks, so the file is written from the same
    /// bytes a device would see rather than from a re-encoding that could hide
    /// the very mistake being looked for.
    #[cfg(test)]
    fn write_png(path: &str, width: u32, height: u32, frame: &[u32]) {
        let mut raw = Vec::with_capacity(frame.len() * 4);
        for px in frame {
            raw.extend_from_slice(&px.to_le_bytes());
        }
        let file = std::fs::File::create(path).expect("create png");
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            width,
            height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&raw).expect("png data");
    }

    /// Share of pixels in `rect` that differ from `bg` by more than a rounding
    /// error. The number a "the text is there" test should be about: the point
    /// is that a handful of stray pixels fails and real glyphs pass.
    #[cfg(test)]
    fn inked_share(frame: &[u32], width: u32, _requested_bg: u32, rect: (u32, u32, u32, u32)) -> f32 {
        // The background the compositor actually produced, read from a corner:
        // asking for `0xFF6B7280` and comparing against it reported every pixel
        // as inked, because the frame's real background is not the constant we
        // passed in. A measurement that cannot distinguish "everything" from
        // "nothing" measures nothing.
        let bg = frame[0];
        let (x0, y0, w, h) = rect;
        let mut inked = 0u32;
        let mut total = 0u32;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let px = frame[(y * width + x) as usize];
                let dr = (px >> 24).abs_diff(bg >> 24);
                let dg = (px >> 16 & 0xFF).abs_diff(bg >> 16 & 0xFF);
                let db = (px >> 8 & 0xFF).abs_diff(bg >> 8 & 0xFF);
                if dr > 2 || dg > 2 || db > 2 {
                    inked += 1;
                }
                total += 1;
            }
        }
        if total == 0 {
            return 0.0;
        }
        inked as f32 / total as f32
    }

    /// Render real text to a PNG and say whether it is legible *by measurement*,
    /// not by an assertion about a number that happens to pass.
    ///
    /// The frame is built the way the editor builds it: canvas 1920×1080, text
    /// laid out at 120 px (the canvas-height share of `TEXT_LAYOUT_RATIO`),
    /// centred, pink on the app's grey. Three sizes are rendered side by side,
    /// because a text layer that breaks only at one size is exactly what the
    /// screenshots from the field show.
    #[test]
    fn real_text_renders_to_a_png_and_covers_its_box() {
        // The *shared* engine, not a private one: `build_ex_scene` snapshots the
        // global atlas, so a layout made by a throwaway engine would point at a
        // page that holds none of its glyphs, and the frame would come out empty
        // for a reason that has nothing to do with the app. Three sizes on one
        // engine also means the second and third see an atlas that has already
        // grown — the case that used to hand back frozen UVs.
        let (w, h) = (1920u32, 1080u32);
        let bg = 0xFF6B7280u32; // the grey the app's canvas shows in the report
        let pink = 0xFFF4A0C0u32;

        let sizes = [32.0f32, 64.0, 120.0];
        let mut scene_text = (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let mut bounds: Vec<(u64, f32, f32)> = Vec::new();
        for size in sizes {
            let handle = layout_text_styled(
                "Hello, Rumo",
                &crate::text::TextStyle::new(size),
            )
            .unwrap_or_else(|| panic!("layout at {size}"));
            let (bw, bh) = layout_bounds(handle).expect("bounds");
            bounds.push((handle, bw, bh));
            // Centred, as the editor does: centre minus half the box.
            scene_text.0.push(handle as i64);
            scene_text.1.push(w as f32 / 2.0 - bw / 2.0);
            scene_text.2.push(h as f32 / 2.0 - bh / 2.0);
            scene_text.3.push(pink as i32);
            scene_text.4.push(1.0);
            scene_text.5.push(0.0);
        }
        let no_shapes: (Vec<i32>, Vec<i32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let no_tex: (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        let scene = build_ex_scene(
            w,
            h,
            bg,
            0,
            no_shapes,
            &[],
            &[],
            scene_text,
            &vec![1.0f32; sizes.len()],
            &[],
            no_tex,
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        let frame = render_ex_cpu_fx(w, h, &scene, &EffectChains::default(), 0);
        write_png("/tmp/rumo_text_render.png", w, h, &frame);

        // Each row of the three must actually put ink on the canvas. The bar is
        // deliberately low — 1.5% of the glyph box is many hundreds of pixels
        // and still far below real text — because the failure being guarded is
        // "a few stray pixels", which lands around 0.05%.
        for (index, (handle, bw_f, bh_f)) in bounds.iter().enumerate() {
            let (bw, bh) = (*bw_f as u32, *bh_f as u32);
            let share = inked_share(
                &frame,
                w,
                bg,
                (
                    (w as f32 / 2.0 - *bw_f / 2.0) as u32,
                    (h as f32 / 2.0 - *bh_f / 2.0) as u32,
                    bw.max(1),
                    bh.max(1),
                ),
            );
            println!(
                "size {} px: box {bw}x{bh}, inked {:.2}% of the box",
                sizes[index],
                share * 100.0
            );
            assert!(
                share > 0.015,
                "size {} px drew only {:.2}% of its own box — see /tmp/rumo_text_render.png",
                sizes[index],
                share * 100.0
            );
            assert!(
                inked_share(&frame, w, bg, (0, 0, w, h)) > 0.0005,
                "size {} px: the frame as a whole is empty",
                sizes[index]
            );
            let _ = handle;
        }
        for (handle, _, _) in &bounds {
            free_layout(*handle);
        }
    }

    // ---- Global draw order across the SHAPE / text / image groups ----------
    //
    // The three Ex entry points used to receive three typed groups, and the
    // engine drew them as groups: every shape, then every text entry, then
    // every image. A layer's place in the editor's list is now carried as a
    // per-group order array and merged into one sequence, so a picture can sit
    // under text or an SVG can sit over a picture. These tests assert the
    // resulting *draw sequence* — never pixels — plus the effect chains each
    // draw carries.

    /// Tag every draw of a request by the group it came from. Text and image
    /// both arrive as textured draws; the atlas id tells them apart.
    fn draw_kinds(request: &crate::gpu_worker::FrameRequest) -> Vec<&'static str> {
        request
            .layer_draws()
            .iter()
            .map(|draw| match draw.shape {
                LayerShape::Mesh { .. } => "mesh",
                LayerShape::Textured(quad) if quad.texture_id == ATLAS_TEXTURE_ID => "text",
                LayerShape::Textured(_) => "image",
            })
            .collect()
    }

    fn empty_text_group() -> (Vec<i64>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<f32>, Vec<f32>) {
        (vec![], vec![], vec![], vec![], vec![], vec![])
    }

    fn empty_image_group() -> (Vec<i64>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        (vec![], vec![], vec![], vec![], vec![], vec![])
    }

    fn empty_shape_group() -> (Vec<i32>, Vec<i32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        (vec![], vec![], vec![], vec![], vec![], vec![])
    }

    /// The frame that must land on the GPU for `scene`.
    fn frame_of(scene: &ExScene) -> crate::gpu_worker::FrameRequest {
        ex_frame_request(64, 36, scene, 0)
    }

    /// The one that matters: a picture in the image group ordered before a text
    /// entry draws first, so the text ends up on top; swapping the two order
    /// values swaps the sequence. Before the fix the text always drew first
    /// (the text group was drawn whole before the image group).
    #[test]
    fn cross_group_order_puts_a_picture_under_text() {
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let id = upload_image(1, 1, vec![255, 0, 0, 255]).expect("upload");
        let text = (
            vec![handle as i64],
            vec![0.0],
            vec![0.0],
            vec![0xFFFFFFFFu32 as i32],
            vec![1.0],
            vec![0.0],
        );
        let image = (vec![id as i64], vec![0.0], vec![0.0], vec![8.0], vec![8.0], vec![1.0]);

        // Picture order 0, text order 1: the picture is drawn under the text.
        let picture_first = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            empty_shape_group(),
            &[],
            &[],
            text.clone(),
            &[],
            &[],
            image.clone(),
            &[],
            &[],
            &[1],
            &[0],
            &EffectChains::default(),
        );
        assert_eq!(
            draw_kinds(&frame_of(&picture_first)),
            vec!["image", "text"],
            "order 0 must draw first, so the picture sits under the text"
        );

        // Swap the two order values: now the text is under the picture.
        let text_first = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            empty_shape_group(),
            &[],
            &[],
            text,
            &[],
            &[],
            image,
            &[],
            &[],
            &[0],
            &[1],
            &EffectChains::default(),
        );
        assert_eq!(
            draw_kinds(&frame_of(&text_first)),
            vec!["text", "image"],
            "swapping the order values must swap the draw sequence"
        );

        assert!(free_layout(handle));
        assert!(free_texture(id));
    }

    /// An SVG layer is a SHAPE layer, so it lives in the mesh group; an SVG
    /// ordered before a picture must draw before it. This is the owner's SVG
    /// case.
    #[test]
    fn cross_group_order_puts_an_svg_over_a_picture() {
        let svg = crate::composite::register_svg(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
                <rect x="0" y="0" width="100" height="100" fill="#ff0000"/>
            </svg>"##,
        )
        .expect("register");
        assert!(svg >= crate::composite::SVG_ID_BASE);
        let id = upload_image(1, 1, vec![0, 0, 255, 255]).expect("upload");
        let image = (vec![id as i64], vec![0.0], vec![0.0], vec![8.0], vec![8.0], vec![1.0]);
        let shape = (
            vec![svg],
            vec![0xFFFF0000u32 as i32],
            vec![0.0],
            vec![0.0],
            vec![0.0],
            vec![1.0],
        );

        let svg_first = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape.clone(),
            &[],
            &[],
            empty_text_group(),
            &[],
            &[],
            image.clone(),
            &[],
            &[0],
            &[],
            &[1],
            &EffectChains::default(),
        );
        assert_eq!(
            draw_kinds(&frame_of(&svg_first)),
            vec!["mesh", "image"],
            "the SVG's mesh must precede the picture"
        );

        let image_first = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape,
            &[],
            &[],
            empty_text_group(),
            &[],
            &[],
            image,
            &[],
            &[1],
            &[],
            &[0],
            &EffectChains::default(),
        );
        assert_eq!(
            draw_kinds(&frame_of(&image_first)),
            vec!["image", "mesh"],
            "a picture can cover an SVG when its order says so"
        );

        assert!(crate::composite::free_svg(svg));
        assert!(free_texture(id));
    }

    /// Two draws of the *same* group keep their relative order: the merge is a
    /// stable sort, and one layer's stroke/fill pair stays consecutive with the
    /// stroke behind its own fill.
    #[test]
    fn same_group_order_is_preserved() {
        let shape = (
            vec![0, 1],
            vec![0xFFFF0000u32 as i32, 0xFF00FF00u32 as i32],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
            vec![1.0, 1.0],
        );
        let scene = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape,
            &[],
            &[],
            empty_text_group(),
            &[],
            &[],
            empty_image_group(),
            &[],
            &[0, 1],
            &[],
            &[],
            &EffectChains::default(),
        );
        assert_eq!(
            scene.draw_order,
            vec![SceneDraw::Mesh(0), SceneDraw::Mesh(1)],
            "increasing orders keep the group's own order"
        );

        // A stroked text layer pushes stroke then fill as two draws; both share
        // a layer, and Rust must keep them adjacent and in that order.
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let stroked = (
            vec![handle as i64, handle as i64],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
            vec![0xFF000000u32 as i32, 0xFFFFFFFFu32 as i32],
            vec![1.0, 1.0],
            vec![0.0, 0.0],
        );
        let scene = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            empty_shape_group(),
            &[],
            &[],
            stroked,
            &[],
            &[],
            empty_image_group(),
            &[],
            &[],
            &[0, 1],
            &[],
            &EffectChains::default(),
        );
        assert_eq!(
            scene.draw_order,
            vec![SceneDraw::Textured(0), SceneDraw::Textured(1)],
            "stroke must stay behind its own fill"
        );
        assert!(free_layout(handle));
    }

    /// No usable order array — absent, empty or too short — degrades to the
    /// group order this code always produced, and drops nothing.
    #[test]
    fn missing_or_short_order_arrays_fall_back_to_group_order() {
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let id = upload_image(1, 1, vec![255, 0, 0, 255]).expect("upload");
        let text = (
            vec![handle as i64],
            vec![0.0],
            vec![0.0],
            vec![0xFFFFFFFFu32 as i32],
            vec![1.0],
            vec![0.0],
        );
        let image = (vec![id as i64], vec![0.0], vec![0.0], vec![8.0], vec![8.0], vec![1.0]);
        let shape = (
            vec![0],
            vec![0xFFFF9800u32 as i32],
            vec![0.0],
            vec![0.0],
            vec![0.0],
            vec![1.0],
        );

        let all_missing = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape.clone(),
            &[],
            &[],
            text.clone(),
            &[],
            &[],
            image.clone(),
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        assert_eq!(
            all_missing.draw_order,
            vec![SceneDraw::Mesh(0), SceneDraw::Textured(0), SceneDraw::Textured(1)],
            "no order at all is the group order: shapes, then text, then images"
        );
        assert_eq!(all_missing.shapes.len(), 1);
        assert_eq!(all_missing.quads.len(), 2, "no draw is dropped");

        // A short shape array (one value for two shapes) is as good as absent:
        // a half-applied order would be worse than none.
        let short = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            (
                vec![0, 1],
                vec![0xFFFF9800u32 as i32, 0xFF2196F3u32 as i32],
                vec![0.0, 0.0],
                vec![0.0, 0.0],
                vec![0.0, 0.0],
                vec![1.0, 1.0],
            ),
            &[],
            &[],
            text,
            &[],
            &[],
            image,
            &[],
            &[5],
            &[7],
            &[1],
            &EffectChains::default(),
        );
        assert_eq!(
            short.draw_order,
            vec![SceneDraw::Mesh(0), SceneDraw::Mesh(1), SceneDraw::Textured(0), SceneDraw::Textured(1)],
            "a short array falls back to the group order, nothing is dropped"
        );

        assert!(free_layout(handle));
        assert!(free_texture(id));
    }

    /// The chains are addressed by a draw's *group* index, so an interleaved
    /// order must not slide a chain onto another layer. Each draw is checked
    /// against the chain it was built with.
    #[test]
    fn effect_chains_stay_attached_to_their_draw_under_interleaving() {
        let handle = store_layout(TextLayout {
            quads: vec![sample_quad()],
            width: 3.0,
            height: 4.0,
        });
        let id = upload_image(1, 1, vec![255, 0, 0, 255]).expect("upload");
        let shape = (
            vec![0, 1],
            vec![0xFFFF0000u32 as i32, 0xFF00FF00u32 as i32],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
            vec![1.0, 1.0],
        );
        let text = (
            vec![handle as i64],
            vec![0.0],
            vec![0.0],
            vec![0xFFFFFFFFu32 as i32],
            vec![1.0],
            vec![0.0],
        );
        let image = (vec![id as i64], vec![0.0], vec![0.0], vec![8.0], vec![8.0], vec![1.0]);
        let chains = EffectChains {
            shapes: vec![
                vec![EffectInstance::new(EffectKind::Blur)],
                vec![EffectInstance::new(EffectKind::Pixelate)],
            ],
            textures: vec![
                vec![EffectInstance::new(EffectKind::Glow)],
                vec![EffectInstance::new(EffectKind::Threshold)],
            ],
            custom: Vec::new(),
        };
        // Order: shape 0, image, text, shape 1 — the groups fully interleave.
        let scene = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape,
            &[],
            &[],
            text,
            &[],
            &[],
            image,
            &[],
            &[0, 4],
            &[3],
            &[1],
            &chains,
        );
        assert_eq!(
            scene.draw_order,
            vec![
                SceneDraw::Mesh(0),
                SceneDraw::Textured(1),
                SceneDraw::Textured(0),
                SceneDraw::Mesh(1),
            ],
            "the merged order interleaves the groups"
        );

        let request = frame_of(&scene);
        let draws = request.layer_draws();
        let effect = |draw: &crate::renderer::LayerDraw<'_>| -> EffectTarget {
            assert_eq!(draw.effects.len(), 1, "each draw carries exactly its one chain");
            draw.effects[0].target.clone()
        };
        assert_eq!(
            effect(&draws[0]),
            EffectTarget::Builtin(EffectKind::Blur),
            "shape 0 keeps its chain"
        );
        assert_eq!(
            effect(&draws[1]),
            EffectTarget::Builtin(EffectKind::Threshold),
            "the image keeps its chain"
        );
        assert_eq!(
            effect(&draws[2]),
            EffectTarget::Builtin(EffectKind::Glow),
            "the text keeps its chain"
        );
        assert_eq!(
            effect(&draws[3]),
            EffectTarget::Builtin(EffectKind::Pixelate),
            "shape 1 keeps its chain"
        );

        assert!(free_layout(handle));
        assert!(free_texture(id));
    }

    /// Regression guard: a scene with only shapes draws exactly as before — the
    /// same sequence with order arrays present or absent, and every draw still
    /// on the mesh path.
    #[test]
    fn shapes_only_scene_draws_as_before() {
        let shape = (
            vec![0, 1, 2],
            vec![0xFFFF0000u32 as i32, 0xFF00FF00u32 as i32, 0xFF0000FFu32 as i32],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![1.0, 1.0, 1.0],
        );
        let legacy = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape.clone(),
            &[],
            &[],
            empty_text_group(),
            &[],
            &[],
            empty_image_group(),
            &[],
            &[],
            &[],
            &[],
            &EffectChains::default(),
        );
        let ordered = build_ex_scene(
            64,
            36,
            0xFF000000,
            0,
            shape,
            &[],
            &[],
            empty_text_group(),
            &[],
            &[],
            empty_image_group(),
            &[],
            &[0, 1, 2],
            &[],
            &[],
            &EffectChains::default(),
        );
        let expected = vec![SceneDraw::Mesh(0), SceneDraw::Mesh(1), SceneDraw::Mesh(2)];
        assert_eq!(legacy.draw_order, expected);
        assert_eq!(ordered.draw_order, expected);
        assert_eq!(draw_kinds(&frame_of(&legacy)), vec!["mesh", "mesh", "mesh"]);
        assert_eq!(draw_kinds(&frame_of(&ordered)), vec!["mesh", "mesh", "mesh"]);
    }

    // -----------------------------------------------------------------------
    // Shop font entries
    // -----------------------------------------------------------------------

    fn first_readable(candidates: &[&str]) -> Option<Vec<u8>> {
        candidates.iter().find_map(|path| std::fs::read(path).ok())
    }

    /// The flat header is the whole contract with Kotlin: `nativeFontPreview`
    /// returns `[w][h][rgba…]` and `flatImageToDecoded` on the other side
    /// derives its length check from the two numbers. A preview that lies about
    /// its size is dropped by the caller, so the header is asserted here.
    #[test]
    fn font_preview_carries_a_flat_header_over_inked_pixels() {
        let Some(bytes) = first_readable(&[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/system/fonts/DroidSans.ttf",
        ]) else {
            eprintln!("skipping: no font file on this host");
            return;
        };
        let Some(family) = register_font(bytes.clone()) else {
            eprintln!("skipping: the host font did not parse");
            return;
        };
        let flat = font_preview(
            &family,
            bytes,
            "Rumo",
            &crate::text::TextStyle::new(40.0),
            0xFF11_2233,
            3,
        )
        .expect("preview");
        let w = u32::from_le_bytes(flat[0..4].try_into().expect("w"));
        let h = u32::from_le_bytes(flat[4..8].try_into().expect("h"));
        assert!(w > 6 && h > 6, "the box must hold the text: {w}x{h}");
        assert_eq!(flat.len(), 8 + w as usize * h as usize * 4);
        assert!(
            flat[8..].chunks_exact(4).any(|px| px[3] > 0),
            "a preview of real text must have ink"
        );
    }

    /// Garbage bytes are what a broken download looks like. Shaping under a
    /// name the engine could not register would fall back to the default face,
    /// and the shop would preview a font the user is not looking at.
    #[test]
    fn font_preview_refuses_bytes_it_cannot_register() {
        assert!(
            font_preview(
                "Rumo Definitely Not A Font 12345",
                b"not a font at all".to_vec(),
                "Rumo",
                &crate::text::TextStyle::new(40.0),
                0xFFFF_FFFF,
                2,
            )
            .is_none()
        );
    }

    /// Past the cap the engine rebuilds from the retained bytes. What must not
    /// happen is the newest face going missing with the oldest: the rebuild is
    /// what keeps memory bounded, and a rebuild that forgets the face just
    /// requested would make previews stop working after twelve fonts.
    #[test]
    fn the_preview_engine_survives_its_cap() {
        let Some(bytes) = first_readable(&[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/system/fonts/DroidSans.ttf",
        ]) else {
            eprintln!("skipping: no font file on this host");
            return;
        };
        let mut preview = PreviewFonts::new();
        if !preview.engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        // Distinct keys, one face: the cap counts keys, so this exercises the
        // eviction and rebuild path without needing a pile of real files.
        let names: Vec<String> = (0..PREVIEW_FONT_CAP + 3)
            .map(|i| format!("Rumo Preview Slot {i}"))
            .collect();
        let mut resolved = None;
        for name in &names {
            resolved = preview.ensure(name, bytes.clone());
        }
        assert!(
            resolved.is_some_and(|n| preview.engine.has_family(&n)),
            "the face must be reachable under the name read out of the file"
        );
        assert_eq!(
            preview.order.len(),
            PREVIEW_FONT_CAP,
            "the cap must bound what stays registered"
        );
        assert_eq!(preview.bytes.len(), PREVIEW_FONT_CAP);
        assert_eq!(preview.resolved.len(), PREVIEW_FONT_CAP);
        let newest = names.last().expect("a name");
        assert!(
            preview.order.iter().any(|name| name == newest),
            "the face just asked for must survive the rebuild"
        );
        assert!(
            !preview.order.iter().any(|name| name == &names[0]),
            "the oldest face must be the one evicted"
        );
    }

    #[test]
    fn layout_text_family_falls_back_for_an_empty_or_unknown_family() {
        let style = crate::text::TextStyle::new(24.0);
        if layout_text_family("Rumo", &style, Some("")).is_none() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        assert!(
            layout_text_family("Rumo", &style, Some("Rumo Definitely Not A Font 12345")).is_some(),
            "an unknown family must fall back, not drop the run"
        );
    }
}

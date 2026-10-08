// SPDX-License-Identifier: Apache-2.0

//! RGBA8 texture management and the CPU-side 2D math shared with the
//! texture render pipeline.
//!
//! The engine is strictly RGBA8 (straight alpha, R,G,B,A byte order); no
//! channel swizzling happens here — the Kotlin boundary owns any ARGB/BGRA
//! conversion. All pixel formats are kept identical so an atlas page, an
//! imported photo and a decoded frame can be sampled by the same shader.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

/// A validated RGBA8 image: `rgba.len() == width * height * 4`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA bytes, row-major, top-left origin.
    pub rgba: Vec<u8>,
}

impl TextureImage {
    /// Wrap `rgba` as an RGBA8 image, or `None` when the size is zero or
    /// the buffer length does not match `width * height * 4`.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected = Self::byte_len(width, height)?;
        if rgba.len() != expected {
            return None;
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    /// `width * height * 4` bytes, or `None` for a zero-sized / overflowing
    /// image.
    pub fn byte_len(width: u32, height: u32) -> Option<usize> {
        if width == 0 || height == 0 {
            return None;
        }
        let pixels = u64::from(width).checked_mul(u64::from(height))?;
        let bytes = pixels.checked_mul(4)?;
        usize::try_from(bytes).ok()
    }

    /// Number of pixels in the image.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Flat `[width:u32 LE][height:u32 LE][rgba...]` encoding used by the
    /// JNI boundary (matches `rumo_bridge::pack_image_flat`).
    pub fn to_flat(&self) -> Vec<u8> {
        let mut flat = Vec::with_capacity(8 + self.rgba.len());
        flat.extend_from_slice(&self.width.to_le_bytes());
        flat.extend_from_slice(&self.height.to_le_bytes());
        flat.extend_from_slice(&self.rgba);
        flat
    }
}

// ---------------------------------------------------------------------------
// CPU transform math
// ---------------------------------------------------------------------------

/// Column-major 4×4 matrix, the same memory layout the WGSL `mat4x4<f32>`
/// uniforms expect (`matrix[column][row]`).
pub type Mat4 = [[f32; 4]; 4];

/// Identity transform.
pub const fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// 2D translation applied to `(x, y)`.
pub fn translate(tx: f32, ty: f32) -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [tx, ty, 0.0, 1.0],
    ]
}

/// 2D scaling about the origin.
pub fn scale(sx: f32, sy: f32) -> Mat4 {
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// 2D rotation by `radians` (right-handed around +Z).
pub fn rotate(radians: f32) -> Mat4 {
    let (s, c) = radians.sin_cos();
    [
        [c, s, 0.0, 0.0],
        [-s, c, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Matrix product `a * b`: applying the result equals applying `b` first,
/// then `a`.
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (col, out_col) in out.iter_mut().enumerate() {
        for (row, value) in out_col.iter_mut().enumerate() {
            for k in 0..4 {
                *value += a[k][row] * b[col][k];
            }
        }
    }
    out
}

/// Convenience composition: scale, then rotate, then translate.
pub fn compose(tx: f32, ty: f32, radians: f32, sx: f32, sy: f32) -> Mat4 {
    mul(&translate(tx, ty), &mul(&rotate(radians), &scale(sx, sy)))
}

/// Apply the affine part of `m` to a 2D point.
pub fn transform_point(m: &Mat4, p: [f32; 2]) -> [f32; 2] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[3][1],
    ]
}

/// Scale the alpha channel of `color` by `alpha` (both clamped to `0..=1`).
pub fn alpha_color(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] = (color[3] * alpha).clamp(0.0, 1.0);
    color
}

// ---------------------------------------------------------------------------
// Quad / UV geometry
// ---------------------------------------------------------------------------

/// `[u0, v0, u1, v1]` for a pixel rect inside a `page_w`×`page_h` atlas.
/// Returns `None` for degenerate pages or out-of-range rects.
pub fn uv_rect(rect: [u32; 4], page_w: u32, page_h: u32) -> Option<[f32; 4]> {
    if page_w == 0 || page_h == 0 {
        return None;
    }
    let [x, y, w, h] = rect;
    let x1 = x.checked_add(w)?;
    let y1 = y.checked_add(h)?;
    if x1 > page_w || y1 > page_h || w == 0 || h == 0 {
        return None;
    }
    Some([
        x as f32 / page_w as f32,
        y as f32 / page_h as f32,
        x1 as f32 / page_w as f32,
        y1 as f32 / page_h as f32,
    ])
}

/// A CPU-owned textured mesh: positions and UVs share one vertex list and
/// one 32-bit index buffer, matching the texture pipeline's vertex layout.
#[derive(Debug, Clone, PartialEq)]
pub struct TexturedMesh {
    pub vertices: Vec<TexVertex>,
    pub indices: Vec<u32>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct TexVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
}

impl TexturedMesh {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Append one axis-aligned quad (top-left `x`,`y`, size `w`×`h`) with the
    /// given UV corners.
    pub fn push_quad(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4]) {
        let [u0, v0, u1, v1] = uv;
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&[
            TexVertex {
                position: [x, y],
                uv: [u0, v0],
            },
            TexVertex {
                position: [x + w, y],
                uv: [u1, v0],
            },
            TexVertex {
                position: [x + w, y + h],
                uv: [u1, v1],
            },
            TexVertex {
                position: [x, y + h],
                uv: [u0, v1],
            },
        ]);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Append one axis-aligned quad whose four UV corners are given
    /// individually, in the same corner order [`Self::push_quad`] uses.
    ///
    /// [`Self::push_quad`]'s `[u0, v0, u1, v1]` rect can only express an axis
    /// aligned sample region, which is enough for an atlas page and not for a
    /// rotated video frame: a quarter turn transposes the image, so its UVs are
    /// a permutation and a mirror rather than a rect. Same vertices, same
    /// topology, same draw — only the UVs differ.
    pub fn push_quad_uv_corners(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [[f32; 2]; 4]) {
        let base = self.vertices.len() as u32;
        let corners = [
            TexVertex {
                position: [x, y],
                uv: uv[0],
            },
            TexVertex {
                position: [x + w, y],
                uv: uv[1],
            },
            TexVertex {
                position: [x + w, y + h],
                uv: uv[2],
            },
            TexVertex {
                position: [x, y + h],
                uv: uv[3],
            },
        ];
        self.vertices.extend_from_slice(&corners);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

impl Default for TexturedMesh {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Texture residency
// ---------------------------------------------------------------------------

/// Cheap identity of a texture's *pixels*, as opposed to the slot they live in.
///
/// A texture id alone cannot say "is this the picture the GPU already has?":
/// the glyph atlas lives under one constant id
/// ([`crate::composite::ATLAS_TEXTURE_ID`]) for the whole session while its
/// page changes every time another glyph is rasterized. So the producer states
/// *what* it is handing over — a generation for registry images, a content
/// signature for the atlas — and the GPU cache re-uploads exactly when that
/// differs from what the slot already holds.
///
/// A stamp therefore also implies the pixel size: skipping an upload can never
/// leave a slot holding a texture of the wrong shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureStamp(u64);

impl TextureStamp {
    /// Set on every stamp minted by [`Self::for_content`], so the two families
    /// can never be mistaken for each other by a single id.
    const CONTENT_TAG: u64 = 1 << 63;

    /// Stamp of an image addressed by a registry id.
    ///
    /// The JNI registry mints one id per upload and never rewrites or reuses
    /// one, so the id *is* the content identity: a still photo keeps both for
    /// as long as its content does, and a decoded video frame arrives under a
    /// brand new id every frame — and therefore with a brand new stamp, which
    /// is what keeps video re-uploading. Nothing in the cache needs to know
    /// which of the two it has been handed.
    pub fn for_content(generation: u64) -> Self {
        Self(generation | Self::CONTENT_TAG)
    }

    /// Stamp of a glyph atlas page.
    ///
    /// The page is append-only: a rasterized glyph is written once and never
    /// rewritten or evicted, and growth only appends zero rows. Its pixels
    /// therefore change *exactly* when a glyph is added (`glyph_count`) or the
    /// page grows (`page_h`) — so typing one new character produces a new
    /// stamp and reaches the GPU, while every frame in between reuses the
    /// resident texture. This is the invariant that makes skipping safe for the
    /// one texture that never changes id.
    pub fn for_atlas(page_w: u32, page_h: u32, glyph_count: usize) -> Self {
        // Every field is masked into its slice so a pathological page or glyph
        // count can never spill into the tag bit and forge a content stamp.
        Self(
            u64::from(page_w & 0xFFFF)
                | ((u64::from(page_h) & 0xFFFF) << 16)
                | (((glyph_count as u64) & 0x7FFF_FFFF) << 32),
        )
    }

    /// Whether a slot that last uploaded `previous` already holds the pixels
    /// `current` describes, so the upload can be skipped. `None` means nothing
    /// was ever uploaded under that id.
    pub fn already_resident(previous: Option<Self>, current: Self) -> bool {
        matches!(previous, Some(previous) if previous == current)
    }

    /// The raw value, for diagnostics and tests.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// What an upload attempt actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The texture was (re)created and its pixels written to the GPU.
    Uploaded,
    /// The slot already held exactly these pixels; nothing was transferred.
    Resident,
}

/// One texture payload travelling to the GPU: the id its draws reference, the
/// stamp that says whether those pixels are new, and the image itself.
///
/// The image is shared rather than copied. A 1080p frame is 7.9 MiB, and every
/// text draw of a scene wants the *same* atlas page while every image draw
/// wants its own photo — handing each draw its own `Arc` costs a refcount,
/// where cloning the bytes cost a full-page memcpy per draw per frame.
#[derive(Debug, Clone)]
pub struct SceneTexture {
    pub id: u64,
    pub stamp: TextureStamp,
    pub image: Arc<TextureImage>,
}

impl SceneTexture {
    /// Takes [`TextureImage`] or an already shared `Arc<TextureImage>`; the
    /// latter keeps a shared page from being wrapped twice.
    pub fn new(id: u64, stamp: TextureStamp, image: impl Into<Arc<TextureImage>>) -> Self {
        Self {
            id,
            stamp,
            image: image.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// YUV planes
// ---------------------------------------------------------------------------

/// How the three planes of a decoded frame are packed — the renderer's copy of
/// `rumo_media::video::yuv::YuvFormat`.
///
/// It is a separate enum rather than a shared one because `rumo_media` cannot
/// depend on `rumo_render` (and vice versa); `rumo_bridge::yuv_format` maps
/// between the two exhaustively and a unit test there pins the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvFormat {
    /// Planar 4:2:0: plane 0 luma, plane 1 U, plane 2 V.
    I420,
    /// Semi-planar 4:2:0: plane 0 luma, plane 1 interleaved U then V.
    Nv12,
    /// Semi-planar 4:2:0: plane 0 luma, plane 1 interleaved V then U.
    Nv21,
}

impl YuvFormat {
    /// The value the shader compares against (`shader_yuv.wgsl`).
    pub fn code(self) -> f32 {
        match self {
            YuvFormat::I420 => 0.0,
            YuvFormat::Nv12 => 1.0,
            YuvFormat::Nv21 => 2.0,
        }
    }

    /// How many planes carry real bytes; the rest are a one-texel dummy so the
    /// bind group always has the same shape.
    pub fn planes_used(self) -> usize {
        match self {
            YuvFormat::I420 => 3,
            YuvFormat::Nv12 | YuvFormat::Nv21 => 2,
        }
    }
}

/// One decoded video frame, as planes, ready to be uploaded and sampled.
///
/// The bytes are the decoder's, shared rather than copied: one [`Arc`] covers
/// all three planes and the description says where each of them starts and how
/// wide its rows are. Nothing here converts a pixel — the shader does that, from
/// the coefficients the CPU converter computed (`coeffs`), which is why the
/// renderer holds no colour constants of its own.
///
/// The crop is stored **before** the decoder's rotation, because the rotation
/// is not something this sampler can undo: it is folded into `corners`, the four
/// UVs of the quad that draws the frame (see
/// `rumo_media::video::yuv::corner_uvs`).
#[derive(Debug, Clone)]
pub struct YuvTexture {
    format: YuvFormat,
    width: u32,
    height: u32,
    corners: [[f32; 2]; 4],
    coeffs: [f32; 7],
    bytes: Arc<[u8]>,
    plane_offset: [usize; 3],
    plane_len: [usize; 3],
    plane_width: [u32; 3],
    plane_height: [u32; 3],
}

impl YuvTexture {
    /// Describe a decoded frame, or `None` when the description cannot be
    /// sampled safely.
    ///
    /// `plane_width`/`plane_height` are the *uploaded* texel size of each plane:
    /// a decoded row carries stride padding, and the padding is uploaded as-is
    /// rather than repacked, so a plane is `row_stride` texels wide and one byte
    /// per texel (the shader reads it as `R8Uint` to keep the decoded value
    /// exact). Planes the packing leaves unused are all-zero.
    ///
    /// # Errors
    /// A zero-sized crop, a luma plane narrower or shorter than the crop, a
    /// plane the packing does not have but that was handed bytes anyway, a plane
    /// that runs past the buffer or is shorter than its own texel size, or a
    /// non-finite coefficient or UV.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        format: YuvFormat,
        width: u32,
        height: u32,
        corners: [[f32; 2]; 4],
        coeffs: [f32; 7],
        bytes: Arc<[u8]>,
        plane_offset: [usize; 3],
        plane_len: [usize; 3],
        plane_width: [u32; 3],
        plane_height: [u32; 3],
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("YuvTexture: zero-sized frame {width}x{height}"));
        }
        if !coeffs.iter().all(|c| c.is_finite()) {
            return Err("YuvTexture: non-finite colour coefficient".to_string());
        }
        for corner in corners.iter().flatten() {
            if !corner.is_finite() {
                return Err("YuvTexture: non-finite UV".to_string());
            }
        }
        if plane_width[0] < width || plane_height[0] < height {
            return Err(format!(
                "YuvTexture: luma plane {}x{} cannot cover a {width}x{height} crop",
                plane_width[0], plane_height[0]
            ));
        }
        for index in 0..3 {
            let len = plane_len[index];
            if index >= format.planes_used() {
                if len != 0 || plane_width[index] != 0 || plane_height[index] != 0 {
                    return Err(format!(
                        "YuvTexture: plane {index} carries bytes but {format:?} has no such plane"
                    ));
                }
                continue;
            }
            if len == 0 {
                return Err(format!("YuvTexture: plane {index} is empty"));
            }
            if plane_width[index] == 0 || plane_height[index] == 0 {
                return Err(format!(
                    "YuvTexture: plane {index} carries {len} bytes but has no texels"
                ));
            }
            let end = plane_offset[index]
                .checked_add(len)
                .ok_or_else(|| format!("YuvTexture: plane {index} offset + length overflows"))?;
            if end > bytes.len() {
                return Err(format!(
                    "YuvTexture: plane {index} ends at {end}, buffer is {} bytes",
                    bytes.len()
                ));
            }
            let texels = u64::from(plane_width[index])
                .checked_mul(u64::from(plane_height[index]))
                .ok_or_else(|| format!("YuvTexture: plane {index} texel count overflows"))?;
            if usize::try_from(texels).map_or(true, |texels| texels > len) {
                return Err(format!(
                    "YuvTexture: plane {index} holds {len} bytes, its texel size \
                     {}x{} needs {texels}",
                    plane_width[index], plane_height[index]
                ));
            }
        }
        Ok(Self {
            format,
            width,
            height,
            corners,
            coeffs,
            bytes,
            plane_offset,
            plane_len,
            plane_width,
            plane_height,
        })
    }

    pub fn format(&self) -> YuvFormat {
        self.format
    }

    /// Visible crop width, before the rotation that `corners` applies.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Visible crop height, before the rotation that `corners` applies.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The quad's four corner UVs, rotation included.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        self.corners
    }

    /// Bytes of plane `index`, or `None` for the plane this packing leaves
    /// unused.
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        let len = *self.plane_len.get(index)?;
        let offset = *self.plane_offset.get(index)?;
        if len == 0 {
            return None;
        }
        self.bytes.get(offset..offset + len)
    }

    /// Texel size of plane `index` as uploaded.
    pub fn plane_size(&self, index: usize) -> (u32, u32) {
        (
            self.plane_width.get(index).copied().unwrap_or(0),
            self.plane_height.get(index).copied().unwrap_or(0),
        )
    }

    /// Uploaded texels per frame — the luma and chroma planes of one frame,
    /// which is what replaces the per-frame RGBA8 write on the GPU path.
    pub fn uploaded_bytes(&self) -> usize {
        self.plane_len.iter().sum()
    }
}

/// What a resident YUV slot needs in order to be drawn: the crop the shader
/// indexes with, the packing it swizzles on, and the seven colour coefficients
/// it multiplies with.
///
/// Held next to the slot's GPU textures so a draw can build its uniform from
/// the texture id alone — the draw list itself stays exactly what it was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YuvSlot {
    pub format: YuvFormat,
    /// Visible crop width, before the rotation the quad's UVs apply.
    pub width: u32,
    /// Visible crop height, before the rotation the quad's UVs apply.
    pub height: u32,
    /// `y_scale`, `y_offset`, `c_center`, `r_v`, `g_u`, `g_v`, `b_u`.
    pub coeffs: [f32; 7],
}

impl YuvSlot {
    /// The part of `texture` a draw needs; the bytes stay where they are.
    pub fn of(texture: &YuvTexture) -> Self {
        Self {
            format: texture.format,
            width: texture.width,
            height: texture.height,
            coeffs: texture.coeffs,
        }
    }
}

/// The uniform a YUV draw uploads: the shared texture transform and colour, the
/// colour coefficients, and the crop size the shader needs to turn a UV back
/// into a texel index.
///
/// `#[repr(C)]` with every field 16-byte aligned so it matches the WGSL struct
/// byte for byte; `size_of` is asserted below rather than assumed.
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct YuvUniforms {
    pub transform: [[f32; 4]; 4],
    pub color: [f32; 4],
    /// `x` = `y_scale`, `y` = `y_offset`, `z` = `c_center`, `w` = format code.
    pub luma: [f32; 4],
    /// `x` = `r_v`, `y` = `g_u`, `z` = `g_v`, `w` = `b_u`.
    pub chroma: [f32; 4],
    /// `x` = crop width, `y` = crop height.
    pub size: [f32; 4],
}

// The WGSL struct is `mat4x4` + four `vec4`s, i.e. 128 bytes with every member
// starting on a 16-byte boundary; this Rust one is the same 128 bytes in the same
// order, which is all a uniform binding cares about.
const _: () = assert!(std::mem::size_of::<YuvUniforms>() == 128);

impl YuvUniforms {
    /// The uniform for one draw of the frame in `slot`.
    pub fn for_draw(slot: &YuvSlot, transform: Mat4, color: [f32; 4]) -> Self {
        let [y_scale, y_offset, c_center, r_v, g_u, g_v, b_u] = slot.coeffs;
        Self {
            transform,
            color,
            luma: [y_scale, y_offset, c_center, slot.format.code()],
            chroma: [r_v, g_u, g_v, b_u],
            size: [slot.width as f32, slot.height as f32, 0.0, 0.0],
        }
    }
}

/// The three texture views a YUV draw binds, with a one-texel dummy standing in
/// for the plane the packing leaves unused so the bind group shape never
/// depends on the format.
#[derive(Debug)]
pub struct YuvViews {
    pub planes: [wgpu::TextureView; 3],
}

/// A resident YUV slot: the views to bind plus what the shader needs in its
/// uniform.
pub struct YuvBind<'a> {
    pub views: &'a YuvViews,
    pub slot: &'a YuvSlot,
}

// ---------------------------------------------------------------------------
// GPU texture cache
// ---------------------------------------------------------------------------

enum GpuTextureEntry {
    /// An uploaded RGBA8 image, sampled by the ordinary texture shader.
    Rgba {
        #[allow(dead_code)]
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    },
    /// A decoded frame's planes, sampled by `shader_yuv.wgsl`.
    Yuv {
        #[allow(dead_code)]
        planes: Vec<wgpu::Texture>,
        views: YuvViews,
        slot: YuvSlot,
    },
}

/// Per-device cache of uploaded textures, keyed by a `u64` id so the JNI texture
/// registry and the renderer agree on one handle namespace.
///
/// Both payload kinds live in one `entries` map under one `resident` stamp map,
/// so a YUV frame re-uploads and expires exactly like a video frame always has —
/// there is no second residency rule to keep in step, and a video frame's fresh
/// id per frame keeps that true without a special case.
///
/// Residency is kept separately from the wgpu objects: `entries` holds what is
/// drawable, `resident` records which [`TextureStamp`] each id currently holds.
/// The split is what makes the residency *rule* testable on the host — the
/// container has no adapter, and a rule that can only be checked against a
/// device is a rule nobody checks.
pub struct GpuTextureCache {
    next_id: u64,
    resident: HashMap<u64, TextureStamp>,
    entries: HashMap<u64, GpuTextureEntry>,
    /// Which decoded frames the cache holds, oldest upload first. See
    /// [`MAX_CACHED_YUV_SLOTS`].
    yuv_order: VecDeque<u64>,
}

/// How many decoded frames the cache keeps on the GPU.
///
/// A video frame arrives under a fresh id every frame, so this map would grow
/// for as long as playback does. The offscreen worker's LRU walks the *scene's*
/// image payloads, and a plane-backed frame ships none of those — it hands the
/// renderer a texture id and the planes themselves — so the bound lives here,
/// next to the entries it protects. It is the image path's rule, applied to the
/// payload that path does not carry.
const MAX_CACHED_YUV_SLOTS: usize = 32;

impl GpuTextureCache {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            resident: HashMap::new(),
            entries: HashMap::new(),
            yuv_order: VecDeque::new(),
        }
    }

    /// Decide whether `stamp` has to be uploaded for `id`, recording it as the
    /// slot's resident content on the way.
    ///
    /// `false` means the GPU already holds exactly these pixels under `id` and
    /// the upload must be skipped — that is the difference between shipping a
    /// still photo and a glyph atlas once and shipping them at frame rate.
    /// Video needs no special case anywhere: a fresh id per frame means a
    /// fresh [`TextureStamp::for_content`] stamp per frame, so it always
    /// claims an upload, while a still image keeps its stamp and does not.
    pub fn claim(&mut self, id: u64, stamp: TextureStamp) -> bool {
        if TextureStamp::already_resident(self.resident.get(&id).copied(), stamp) {
            return false;
        }
        self.resident.insert(id, stamp);
        true
    }

    /// Upload `image` under a fresh id, returning that id. The id doubles as
    /// the content stamp because it is never reused, so the slot can never be
    /// considered up to date for the wrong pixels.
    pub fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &TextureImage,
    ) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.upsert(device, queue, id, TextureStamp::for_content(id), image)?;
        Ok(id)
    }

    /// Upload (or replace) `image` under an explicit `id`, skipping the whole
    /// transfer when `stamp` says the slot already holds these pixels.
    pub fn upsert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u64,
        stamp: TextureStamp,
        image: &TextureImage,
    ) -> Result<UploadOutcome, String> {
        // Claiming before the transfer is safe: nothing below can fail — wgpu
        // validates sizes by panicking, and the image was length-checked when
        // it was staged — so a slot never records residency it does not have.
        if !self.claim(id, stamp) {
            return Ok(UploadOutcome::Resident);
        }
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("texture"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.entries.insert(
            id,
            GpuTextureEntry::Rgba { texture, view },
        );
        Ok(UploadOutcome::Uploaded)
    }

    /// Upload (or replace) a decoded frame's planes under an explicit `id`,
    /// skipping the transfer when `stamp` says the slot already holds them.
    ///
    /// Each plane is uploaded as it was decoded — stride padding included, one
    /// byte per texel — so the frame is copied to the GPU exactly once and never
    /// repacked on the CPU. `R8Uint` rather than `R8Unorm` because the shader
    /// reads the decoded byte itself: a normalised float would round-trip
    /// through `value / 255 * 255` and could come back a whole code value low.
    pub fn upsert_yuv(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u64,
        stamp: TextureStamp,
        texture: &YuvTexture,
    ) -> Result<UploadOutcome, String> {
        if !self.claim(id, stamp) {
            return Ok(UploadOutcome::Resident);
        }
        let mut planes = Vec::with_capacity(3);
        let mut views: Vec<wgpu::TextureView> = Vec::with_capacity(3);
        for index in 0..3 {
            let (width, height) = texture.plane_size(index);
            // A packing that leaves a plane unused still needs *a* texture in the
            // bind group; one texel is the cheapest thing that satisfies it, and
            // the shader never reads it.
            let (width, height) = match texture.plane(index) {
                Some(_) => (width.max(1), height.max(1)),
                None => (1, 1),
            };
            let gpu = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(if index < texture.format().planes_used() {
                    "yuv plane"
                } else {
                    "yuv unused plane"
                }),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            if let Some(bytes) = texture.plane(index) {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &gpu,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width),
                        rows_per_image: Some(height),
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
            let view = gpu.create_view(&wgpu::TextureViewDescriptor::default());
            planes.push(gpu);
            views.push(view);
        }
        let views = YuvViews {
            planes: <[wgpu::TextureView; 3]>::try_from(views)
                .map_err(|_| "yuv: three plane views expected".to_string())?,
        };
        self.entries.insert(
            id,
            GpuTextureEntry::Yuv {
                planes,
                views,
                slot: YuvSlot::of(texture),
            },
        );
        self.touch_yuv(id);
        Ok(UploadOutcome::Uploaded)
    }

    /// Record `id` as the newest decoded frame, dropping the oldest ones past
    /// [`MAX_CACHED_YUV_SLOTS`].
    ///
    /// Dropping a slot frees its plane textures together with its residency
    /// record, so an id that comes back is uploaded again rather than drawn as
    /// whatever it held before — the same guarantee
    /// [`GpuTextureCache::remove`] gives the image path.
    fn touch_yuv(&mut self, id: u64) {
        for stale in self.push_yuv_order(id, MAX_CACHED_YUV_SLOTS) {
            self.remove(stale);
        }
    }

    /// Record `id` as the newest decoded frame and report which older ids fall
    /// past `cap`, oldest first.
    ///
    /// Split from [`Self::touch_yuv`] so the bound is a rule that can be checked
    /// on the host: uploading needs a device, and a leak that only shows up as
    /// growing VRAM during playback is a leak nobody looks for.
    fn push_yuv_order(&mut self, id: u64, cap: usize) -> Vec<u64> {
        if let Some(position) = self.yuv_order.iter().position(|held| *held == id) {
            self.yuv_order.remove(position);
        }
        self.yuv_order.push_back(id);
        let mut stale = Vec::new();
        while self.yuv_order.len() > cap {
            if let Some(oldest) = self.yuv_order.pop_front() {
                stale.push(oldest);
            }
        }
        stale
    }

    /// RGBA8 texture view for `id`, if `id` holds an uploaded image.
    pub fn view(&self, id: u64) -> Option<&wgpu::TextureView> {
        match self.entries.get(&id) {
            Some(GpuTextureEntry::Rgba { view, .. }) => Some(view),
            Some(GpuTextureEntry::Yuv { .. }) => None,
            None => None,
        }
    }

    /// Everything a draw of a decoded frame needs, if `id` holds one.
    ///
    /// The two accessors are complementary by construction: a texture id is
    /// either RGBA8 or YUV, and a draw that finds neither skips itself.
    pub fn yuv_bind(&self, id: u64) -> Option<YuvBind<'_>> {
        match self.entries.get(&id) {
            Some(GpuTextureEntry::Yuv { views, slot, .. }) => Some(YuvBind { views, slot }),
            Some(GpuTextureEntry::Rgba { .. }) => None,
            None => None,
        }
    }

    pub fn contains(&self, id: u64) -> bool {
        self.entries.contains_key(&id)
    }

    /// The stamp `id` currently holds on the GPU, if any.
    pub fn resident_stamp(&self, id: u64) -> Option<TextureStamp> {
        self.resident.get(&id).copied()
    }

    /// Drop the texture for `id` together with its residency record; `true`
    /// when something was removed.
    pub fn remove(&mut self, id: u64) -> bool {
        self.resident.remove(&id);
        if let Some(position) = self.yuv_order.iter().position(|held| *held == id) {
            self.yuv_order.remove(position);
        }
        self.entries.remove(&id).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.resident.clear();
        self.entries.clear();
        self.yuv_order.clear();
    }
}

impl Default for GpuTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::ATLAS_TEXTURE_ID;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn transform_identity_is_noop() {
        let m = identity();
        let p = transform_point(&m, [3.0, -7.0]);
        assert!(close(p[0], 3.0) && close(p[1], -7.0));
    }

    #[test]
    fn translate_moves_point() {
        let m = translate(10.0, -4.0);
        let p = transform_point(&m, [1.0, 2.0]);
        assert!(close(p[0], 11.0) && close(p[1], -2.0));
    }

    #[test]
    fn scale_scales_point() {
        let m = scale(2.0, 3.0);
        let p = transform_point(&m, [4.0, 5.0]);
        assert!(close(p[0], 8.0) && close(p[1], 15.0));
    }

    #[test]
    fn rotate_quarter_turn() {
        let m = rotate(std::f32::consts::FRAC_PI_2);
        let p = transform_point(&m, [1.0, 0.0]);
        assert!(close(p[0], 0.0) && close(p[1], 1.0), "got {p:?}");
    }

    #[test]
    fn compose_applies_scale_then_rotate_then_translate() {
        // 90° CCW: (1,0)->(0,1); scale x by 2 -> (2,0); then +10,+20.
        let m = compose(10.0, 20.0, std::f32::consts::FRAC_PI_2, 2.0, 1.0);
        let p = transform_point(&m, [1.0, 0.0]);
        assert!(close(p[0], 10.0) && close(p[1], 22.0), "got {p:?}");
    }

    #[test]
    fn mul_matches_sequential_application() {
        let a = translate(5.0, 0.0);
        let b = scale(2.0, 2.0);
        let ab = mul(&a, &b);
        let p1 = transform_point(&ab, [1.0, 1.0]);
        let mid = transform_point(&b, [1.0, 1.0]);
        let p2 = transform_point(&a, mid);
        assert!(close(p1[0], p2[0]) && close(p1[1], p2[1]));
    }

    #[test]
    fn alpha_color_multiplies_and_clamps() {
        let c = alpha_color([0.2, 0.4, 0.6, 0.5], 0.5);
        assert!(close(c[3], 0.25));
        let c = alpha_color([0.0, 0.0, 0.0, 1.0], 4.0);
        assert!(close(c[3], 1.0));
    }

    #[test]
    fn uv_rect_is_normalized() {
        let uv = uv_rect([0, 0, 32, 16], 128, 64).expect("rect");
        assert_eq!(uv, [0.0, 0.0, 0.25, 0.25]);
        for v in uv {
            assert!((0.0..=1.0).contains(&v), "uv out of range: {v}");
        }
    }

    #[test]
    fn uv_rect_rejects_out_of_bounds_and_degenerate() {
        assert!(uv_rect([0, 0, 8, 8], 0, 64).is_none());
        assert!(uv_rect([0, 0, 0, 8], 64, 64).is_none());
        assert!(uv_rect([60, 0, 8, 8], 64, 64).is_none());
        assert!(uv_rect([0, 60, 8, 8], 64, 64).is_none());
    }

    #[test]
    fn textured_mesh_has_one_quad() {
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(10.0, 20.0, 4.0, 2.0, [0.0, 0.0, 0.5, 0.5]);
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.vertices[0].position, [10.0, 20.0]);
        assert_eq!(mesh.vertices[2].position, [14.0, 22.0]);
        assert_eq!(mesh.vertices[0].uv, [0.0, 0.0]);
        assert_eq!(mesh.vertices[2].uv, [0.5, 0.5]);
    }

    #[test]
    fn texture_image_validation() {
        assert!(TextureImage::new(0, 4, vec![]).is_none());
        assert!(TextureImage::new(2, 2, vec![0; 15]).is_none());
        let img = TextureImage::new(2, 2, vec![7; 16]).expect("valid");
        assert_eq!(img.pixel_count(), 4);
        assert_eq!(&img.to_flat()[0..8], &[2, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(img.to_flat().len(), 8 + 16);
    }

    // The residency rule itself, exercised on the host: this is the decision
    // that stands between "uploaded once" and "uploaded every frame", and it
    // must hold without an adapter. `GpuTextureCache::claim` is the only place
    // the decision is made.

    #[test]
    fn a_slot_is_claimed_once_per_content_generation() {
        let mut cache = GpuTextureCache::new();
        let still = TextureStamp::for_content(4);
        assert!(cache.claim(4, still), "nothing is resident yet");
        assert!(!cache.claim(4, still), "an unchanged photo must not re-upload");
        // A still image whose content changed comes back under a new id.
        assert!(cache.claim(9, TextureStamp::for_content(9)));
        // A stamp that belongs to another id, presented under this slot, must be
        // taken as *different content* and uploaded — the cache is keyed by slot
        // and compares stamps within a slot, so it must not confuse the two.
        assert!(
            cache.claim(4, TextureStamp::for_content(9)),
            "a foreign stamp under this id is a change, not a hit"
        );
        assert_eq!(cache.resident_stamp(4), Some(TextureStamp::for_content(9)));
        assert_eq!(cache.resident_stamp(9), Some(TextureStamp::for_content(9)));
        // And the first photo's own stamp is still a hit under its own slot.
        assert!(cache.claim(4, still), "the original photo went back to being stale");
    }

    #[test]
    fn a_changed_atlas_page_is_claimed_again_and_an_unchanged_one_is_not() {
        // The atlas is the one texture whose pixels change under a constant id,
        // so it is the one where a naive id-keyed cache loses text forever.
        let mut cache = GpuTextureCache::new();
        let before_edit = TextureStamp::for_atlas(512, 512, 128);
        assert!(cache.claim(ATLAS_TEXTURE_ID, before_edit), "first frame");
        // Frames 2..n of playback: no new glyph, same page, no upload.
        for _ in 0..30 {
            assert!(
                !cache.claim(ATLAS_TEXTURE_ID, before_edit),
                "an unchanged page must not re-upload"
            );
        }
        // One character typed: one more glyph on the same page, new pixels.
        let after_edit = TextureStamp::for_atlas(512, 512, 129);
        assert!(cache.claim(ATLAS_TEXTURE_ID, after_edit), "a new glyph must reach the GPU");
        // And a page that ran out of room grew instead.
        assert!(cache.claim(ATLAS_TEXTURE_ID, TextureStamp::for_atlas(512, 1024, 129)));
        assert_eq!(
            cache.resident_stamp(ATLAS_TEXTURE_ID),
            Some(TextureStamp::for_atlas(512, 1024, 129))
        );
    }

    #[test]
    fn an_evicted_slot_is_uploaded_again_even_with_an_unchanged_stamp() {
        let mut cache = GpuTextureCache::new();
        let stamp = TextureStamp::for_content(3);
        assert!(cache.claim(3, stamp));
        // Eviction drops the residency record together with the texture, so an
        // id that comes back is re-uploaded even though its stamp is the same.
        cache.remove(3);
        assert_eq!(cache.resident_stamp(3), None);
        assert!(cache.claim(3, stamp));
    }

    #[test]
    fn content_and_atlas_stamps_cannot_alias() {
        let atlas = TextureStamp::for_atlas(512, 512, 1 << 20);
        assert_eq!(
            atlas.as_u64() & (1 << 63),
            0,
            "the atlas stamp stays out of the content-tagged space, however many \
             glyphs the page holds"
        );
        for generation in [0u64, 1, 512, 1 << 31, u64::MAX] {
            assert_ne!(TextureStamp::for_content(generation), atlas);
        }
    }

    #[test]
    fn an_atlas_stamp_distinguishes_page_and_glyph_count() {
        let base = TextureStamp::for_atlas(512, 512, 8);
        assert_eq!(base, TextureStamp::for_atlas(512, 512, 8), "same page, same content");
        assert_ne!(base, TextureStamp::for_atlas(512, 512, 9), "one more glyph");
        assert_ne!(base, TextureStamp::for_atlas(512, 1024, 8), "page grew");
        assert_ne!(base, TextureStamp::for_atlas(1024, 512, 8), "page width changed");
    }

    // -----------------------------------------------------------------------
    // The YUV path
    // -----------------------------------------------------------------------

    /// `shader_yuv.wgsl`'s `quantize`: round half up, clamp to `0..=255`.
    fn shader_quantize(x: f32) -> u8 {
        (x + 0.5).floor().clamp(0.0, 255.0) as u8
    }

    /// The CPU `yuv.rs` `quantize`, kept verbatim.
    fn cpu_quantize(x: f32) -> u8 {
        let x = x + 0.5;
        if x <= 0.0 {
            0
        } else if x >= 255.0 {
            255
        } else {
            x as u8
        }
    }

    #[test]
    fn the_shader_quantize_matches_the_cpu_half_up_rounding() {
        // Same function on both sides, so the exact .5 values have to agree too:
        // a shader that wrote `round()` (half to even) would diverge from the
        // converter on exactly those.
        for step in -400i32..=1400 {
            let x = step as f32 / 8.0;
            assert_eq!(
                shader_quantize(x),
                cpu_quantize(x),
                "quantize({x}): the shader and the converter must not round apart"
            );
        }
    }

    /// Some coefficients, with the shape `coeffs_f32` produces. The colour they
    /// produce is pinned in `rumo_media::video::yuv`, next to the converter that
    /// is the reference for them; what matters here is that these exact seven
    /// numbers reach the shader unchanged and in the order it reads them.
    const SOME_COEFFS: [f32; 7] = [
        1.164_383_6,
        16.0,
        128.0,
        1.596_066_1,
        -0.391_788_6,
        -0.812_905_6,
        2.017_149_4,
    ];

    /// A 4:2:0 planar frame of `w × h` texels with a padded stride and padded
    /// rows, the shape a decoder actually hands over.
    fn i420_planes(w: u32, h: u32, stride: u32, rows: u32) -> YuvTexture {
        let c_stride = stride / 2;
        let c_rows = rows.div_ceil(2);
        let y_len = (stride * rows) as usize;
        let c_len = (c_stride * c_rows) as usize;
        let bytes: Arc<[u8]> = vec![16u8; y_len + 2 * c_len].into();
        YuvTexture::new(
            YuvFormat::I420,
            w,
            h,
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            SOME_COEFFS,
            bytes,
            [0, y_len, y_len + c_len],
            [y_len, c_len, c_len],
            [stride, c_stride, c_stride],
            [rows, c_rows, c_rows],
        )
        .expect("valid frame")
    }

    #[test]
    fn a_plane_description_is_carried_without_copying() {
        let texture = i420_planes(4, 4, 8, 6);
        assert_eq!(
            texture.plane_size(0),
            (8, 6),
            "stride padding is uploaded as-is rather than repacked"
        );
        assert_eq!(texture.plane_size(1), (4, 3), "chroma rows are half, rounded up");
        assert_eq!(texture.uploaded_bytes(), (8 * 6 + 4 * 3 * 2) as usize);
        assert_eq!(texture.plane(1).unwrap().len(), 4 * 3);
        assert_eq!(texture.width(), 4);
        assert_eq!(texture.height(), 4);
    }

    #[test]
    fn an_unusable_plane_description_is_refused() {
        let bytes: Arc<[u8]> = vec![0u8; 128].into();
        let good = i420_planes(4, 4, 8, 6);
        let build = |format, w, h, coeffs, offset, len, pw, ph| {
            YuvTexture::new(
                format,
                w,
                h,
                good.corners(),
                coeffs,
                Arc::clone(&bytes),
                offset,
                len,
                pw,
                ph,
            )
            .err()
        };
        // A description that already works, so each case below differs in one
        // field only.
        let good_offset = [0, 48, 60];
        let good_len = [48, 12, 12];
        let good_size = ([8, 4, 4], [6, 3, 3]);
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                good_len,
                good_size.0,
                good_size.1
            )
            .is_none(),
            "the baseline description is the one the other cases perturb"
        );
        assert!(
            build(
                YuvFormat::I420,
                0,
                4,
                SOME_COEFFS,
                [0usize; 3],
                [0usize; 3],
                good_size.0,
                good_size.1
            )
            .is_some(),
            "zero crop"
        );
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                [f32::NAN; 7],
                good_offset,
                good_len,
                good_size.0,
                good_size.1
            )
            .is_some(),
            "a NaN coefficient would sample as garbage"
        );
        // A luma plane 2 texels wide for a 4-wide crop: the shader would clamp
        // every fragment onto its padding column.
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                good_len,
                [2, 4, 4],
                good_size.1
            )
            .is_some(),
            "luma narrower than the crop"
        );
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                good_len,
                good_size.0,
                [2, 3, 3]
            )
            .is_some(),
            "luma shorter than the crop"
        );
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                [48, 12, 1 << 20],
                good_size.0,
                good_size.1
            )
            .is_some(),
            "a plane that runs past the buffer"
        );
        assert!(
            build(
                YuvFormat::I420,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                good_len,
                [8, 4, 64],
                good_size.1
            )
            .is_some(),
            "a plane larger than its own texel size"
        );
        // NV12 has no third plane, so handing one bytes is a mismatch rather
        // than something to upload and drop.
        assert!(
            build(
                YuvFormat::Nv12,
                4,
                4,
                SOME_COEFFS,
                good_offset,
                good_len,
                good_size.0,
                good_size.1
            )
            .is_some(),
            "NV12 has two planes, not three"
        );
    }

    #[test]
    fn a_uniform_carries_the_coefficients_the_shader_multiplies_with() {
        let texture = i420_planes(4, 4, 8, 6);
        let slot = YuvSlot::of(&texture);
        let uniforms = YuvUniforms::for_draw(&slot, identity(), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(&uniforms.luma[..3], &SOME_COEFFS[..3]);
        assert_eq!(uniforms.chroma, SOME_COEFFS[3..]);
        assert_eq!(uniforms.luma[3], YuvFormat::I420.code());
        assert_eq!(
            &uniforms.size[..2],
            &[4.0, 4.0][..],
            "the crop the shader indexes, not the padded plane"
        );
        let nv12 = YuvTexture::new(
            YuvFormat::Nv12,
            4,
            4,
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            SOME_COEFFS,
            vec![0u8; 128].into(),
            [0, 48, 0],
            [48, 24, 0],
            [8, 8, 0],
            [6, 3, 0],
        )
        .expect("valid NV12 frame");
        assert_eq!(
            YuvUniforms::for_draw(&YuvSlot::of(&nv12), identity(), [1.0; 4]).luma[3],
            YuvFormat::Nv12.code(),
            "the swizzle the shader branches on"
        );
    }

    #[test]
    fn decoded_frames_are_bounded_and_the_oldest_goes_first() {
        // Playback uploads a new id per frame, so the cap is the only thing
        // standing between a clip and an ever-growing cache.
        let mut cache = GpuTextureCache::new();
        let cap = MAX_CACHED_YUV_SLOTS;
        for id in 1..=cap as u64 {
            assert!(
                cache.push_yuv_order(id, cap).is_empty(),
                "{cap} frames is the budget, so the first {cap} stay"
            );
        }
        let evicted = cache.push_yuv_order(cap as u64 + 1, cap);
        assert_eq!(evicted, vec![1], "the oldest frame is the one that goes");
        assert_eq!(
            cache.yuv_order.len(),
            cap,
            "and the cache stays at its budget however long playback runs"
        );
        // An id that is already resident becomes the newest again rather than
        // occupying two slots.
        assert!(cache.push_yuv_order(2, cap).is_empty());
        assert_eq!(cache.yuv_order.back(), Some(&2));
        assert_eq!(
            cache.yuv_order.iter().filter(|held| **held == 2).count(),
            1,
            "one slot per id, however often the frame is drawn"
        );
        // `remove` — what freeing a staged frame ends in — takes the id out of
        // the order too, so a freed frame is not evicted a second time later.
        // An id the cache does not hold: `push_yuv_order` never lets one id
        // occupy two slots, so a duplicate would be a state the cache cannot
        // reach, and `remove` would only drop the first of the two.
        let freed = cap as u64 + 1_000;
        cache.yuv_order.push_front(freed);
        assert!(!cache.remove(freed), "nothing was uploaded under this id");
        assert!(
            cache.yuv_order.iter().all(|held| *held != freed),
            "a freed frame must not be evicted a second time later"
        );
    }

    #[test]
    fn format_codes_and_plane_counts_match_the_shader() {
        assert_eq!(YuvFormat::I420.code(), 0.0);
        assert_eq!(YuvFormat::Nv12.code(), 1.0);
        assert_eq!(YuvFormat::Nv21.code(), 2.0);
        assert_eq!(YuvFormat::I420.planes_used(), 3);
        assert_eq!(YuvFormat::Nv12.planes_used(), 2);
        assert_eq!(YuvFormat::Nv21.planes_used(), 2);
    }
}

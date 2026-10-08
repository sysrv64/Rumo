// SPDX-License-Identifier: Apache-2.0

//! Pure-Rust YUV → RGBA8 conversion (docs/08 §8.4).
//!
//! No FFI, no `cfg`, no allocation: [`yuv_to_rgba`] writes into the caller's
//! `out` buffer, so the module is fully testable on the host.
//!
//! The decoded buffer is described by [`YuvLayout`]. `stride` and
//! `slice_height` come from the decoder's `MediaFormat` and are normally
//! larger than the visible `width`/`height`: rows carry alignment padding and
//! there can be padding rows at the bottom. Only the visible
//! `width`×`height` crop is read; padding is never touched. Chroma for
//! `Nv12`/`Nv21` is taken at half resolution with the *luma* row stride.
//!
//! A buffer whose geometry cannot be sampled safely — a `stride` below
//! `width`, an odd `I420` stride, or a chroma row too short for the sampled
//! columns — is rejected instead of read out of bounds.
//!
//! `out` receives an **upright** image: for `rot` 90/270 the output
//! dimensions are swapped (`out_w = height`, `out_h = width`) and the samples
//! are rearranged so the rotation is applied clockwise, matching the
//! `AMEDIAFORMAT_KEY_ROTATION` convention.

use std::sync::Arc;

/// Planes are packed as follows (`planes` is always a 3-element array):
///
/// | format | `planes[0]` | `planes[1]` | `planes[2]` |
/// |---|---|---|---|
/// | `I420` | full-res luma | half-res U | half-res V |
/// | `Nv12` | full-res luma | half-res UV (U first) | *unused, pass `&[]`* |
/// | `Nv21` | full-res luma | half-res VU (V first) | *unused, pass `&[]`* |
///
/// For `Nv12`/`Nv21` the interleaved chroma plane uses the **luma** stride
/// (`layout.stride`), not half of it; `planes[2]` is ignored entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvFormat {
    /// Planar 4:2:0 (three separate planes).
    I420,
    /// Semi-planar 4:2:0, interleaved U then V (`NV12`).
    Nv12,
    /// Semi-planar 4:2:0, interleaved V then U (`NV21`).
    Nv21,
}

/// Colour matrix of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// Rec. 601 (`Kr = 0.299`, `Kb = 0.114`).
    Bt601,
    /// Rec. 709 (`Kr = 0.2126`, `Kb = 0.0722`).
    Bt709,
    /// Rec. 2020 (`Kr = 0.2627`, `Kb = 0.0593`).
    Bt2020,
}

/// Sample range of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// Studio/video range: luma 16..=235, chroma 16..=240.
    Limited,
    /// Full range: luma and chroma span 0..=255.
    Full,
}

/// Row layout of the decoded buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvLayout {
    /// Bytes per row of the luma plane (`AMEDIAFORMAT_KEY_STRIDE`).
    pub stride: usize,
    /// Rows of the luma plane, including bottom padding
    /// (`AMEDIAFORMAT_KEY_SLICE_HEIGHT`).
    pub slice_height: usize,
}

/// Byte-exact description of one decoded YUV buffer: which bytes are which
/// plane, how wide their rows are, and where the visible crop sits inside them.
///
/// Both readers of a decoded frame resolve their geometry through
/// [`YuvPlaneDesc::resolve`]: the CPU conversion ([`yuv_to_rgba`]) and the GPU
/// texture path ([`crate::video::YuvPlanes`], which hands the same description
/// to the renderer). Only the *sampling* differs between them — nearest in both
/// cases — so which byte is which pixel cannot drift, and a difference in the
/// picture can only ever be arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvPlaneDesc {
    /// How the three planes are packed.
    pub format: YuvFormat,
    /// Visible crop width, before the decoder's rotation.
    pub width: u32,
    /// Visible crop height, before the decoder's rotation.
    pub height: u32,
    /// Bytes per row of each plane. `0` for the plane a packing leaves unused.
    pub row_stride: [u32; 3],
    /// Rows of each plane, padding rows included.
    pub plane_rows: [u32; 3],
    /// Byte offset of each plane inside the decoded buffer.
    pub plane_offset: [usize; 3],
    /// Bytes of each plane. `0` for the unused plane.
    pub plane_len: [usize; 3],
}

impl YuvPlaneDesc {
    /// Resolve the geometry of a `width`×`height` crop inside `layout`.
    ///
    /// `layout.stride`/`slice_height` come from the decoder's `MediaFormat` and
    /// are normally larger than the visible crop, so the padding is described
    /// here but never sampled.
    ///
    /// # Errors
    /// A zero-sized crop, a `stride` below the width, a slice height below the
    /// height, an odd luma stride for `I420` (its chroma rows are half the luma
    /// stride, and an odd one cannot cover every sampled column), a stride too
    /// narrow for the interleaved chroma pairs, or an arithmetic overflow.
    pub fn resolve(
        format: YuvFormat,
        layout: YuvLayout,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("yuv: zero-sized frame {width}x{height}"));
        }
        let w = width as usize;
        let h = height as usize;
        let stride = layout.stride;
        let slice_height = layout.slice_height;
        if stride < w {
            return Err(format!("yuv: stride {stride} < width {w}"));
        }
        if slice_height < h {
            return Err(format!("yuv: slice_height {slice_height} < height {h}"));
        }
        // Chroma rows are half the luma rows; `div_ceil` keeps an odd
        // `slice_height` readable instead of silently under-validating.
        let chroma_rows = slice_height.div_ceil(2);
        let chroma_cols = w.div_ceil(2);
        let y_len = stride
            .checked_mul(slice_height)
            .ok_or_else(|| "yuv: stride*slice_height overflows".to_string())?;
        let (c_stride, plane_offset, plane_len) = match format {
            YuvFormat::I420 => {
                if stride & 1 != 0 {
                    return Err(format!("yuv: I420 needs an even stride, got {stride}"));
                }
                let c_stride = stride / 2;
                if c_stride < chroma_cols {
                    return Err(format!(
                        "yuv: I420 chroma stride {c_stride} cannot cover {chroma_cols} columns"
                    ));
                }
                let c_len = c_stride
                    .checked_mul(chroma_rows)
                    .ok_or_else(|| "yuv: chroma stride*chroma rows overflows".to_string())?;
                (c_stride, [0, y_len, y_len + c_len], [y_len, c_len, c_len])
            }
            // The interleaved plane reads U/V pairs with the *luma* stride, and
            // its third plane does not exist.
            YuvFormat::Nv12 | YuvFormat::Nv21 => {
                if stride < 2 * chroma_cols {
                    return Err(format!(
                        "yuv: stride {stride} cannot hold {chroma_cols} interleaved chroma pairs"
                    ));
                }
                let c_len = stride
                    .checked_mul(chroma_rows)
                    .ok_or_else(|| "yuv: chroma stride*chroma rows overflows".to_string())?;
                (stride, [0, y_len, y_len], [y_len, c_len, 0])
            }
        };
        // A stride/height that does not fit a `u32` texel count cannot be
        // uploaded to the GPU either, and clamping it would silently re-read a
        // different plane, so it is refused here rather than in the shader.
        let as_u32 = |n: usize| {
            u32::try_from(n).map_err(|_| format!("yuv: plane geometry {n} does not fit u32"))
        };
        // The interleaved packings have no third plane, and saying so with a
        // zero stride is what lets the GPU side tell "unused" from "not
        // described" — a non-zero stride there would look like a plane that
        // lost its bytes.
        let (row_stride, plane_rows) = match format {
            YuvFormat::I420 => (
                [as_u32(stride)?, as_u32(c_stride)?, as_u32(c_stride)?],
                [
                    as_u32(slice_height)?,
                    as_u32(chroma_rows)?,
                    as_u32(chroma_rows)?,
                ],
            ),
            YuvFormat::Nv12 | YuvFormat::Nv21 => (
                [as_u32(stride)?, as_u32(stride)?, 0],
                [as_u32(slice_height)?, as_u32(chroma_rows)?, 0],
            ),
        };
        Ok(Self {
            format,
            width,
            height,
            row_stride,
            plane_rows,
            plane_offset,
            plane_len,
        })
    }

    /// Bytes a decoded buffer must hold for this geometry: the end of the last
    /// plane the packing uses.
    pub fn buffer_len(&self) -> usize {
        self.plane_offset
            .iter()
            .zip(self.plane_len)
            .map(|(offset, len)| offset + len)
            .max()
            .unwrap_or(0)
    }

    /// Plane `index` of `bytes`, or `None` when the plane does not fit.
    pub fn plane<'a>(&self, bytes: &'a [u8], index: usize) -> Option<&'a [u8]> {
        let len = *self.plane_len.get(index)?;
        let offset = *self.plane_offset.get(index)?;
        if len == 0 {
            return None;
        }
        bytes.get(offset..offset.checked_add(len)?)
    }
}

/// Visible size of a `width`×`height` crop after `rotation`.
///
/// The axes swap for a quarter turn, exactly as [`yuv_to_rgba`] swaps them when
/// it writes an upright image.
pub fn rotated_size(width: u32, height: u32, rotation: u16) -> Result<(u32, u32), String> {
    match rotation {
        0 | 180 => Ok((width, height)),
        90 | 270 => Ok((height, width)),
        other => Err(format!("yuv: rotation {other} is not a multiple of 90")),
    }
}

/// The four corner UVs, in `push_quad` corner order
/// (`(x,y)`, `(x+w,y)`, `(x+w,y+h)`, `(x,y+h)`), that make a quad of size
/// [`rotated_size`] sample a `width`×`height` crop with the decoder's clockwise
/// `rotation`.
///
/// This is the GPU form of the pixel rearrangement [`yuv_to_rgba`] does with its
/// `emit!` loops. A quarter turn transposes the image, which a single
/// `[u0, v0, u1, v1]` rect cannot express — hence four corners rather than a
/// rect, and hence why a rotated clip needs its own quad here instead of the
/// stock axis-aligned one.
///
/// The shader turns a UV back into a texel with `floor(uv * crop_size)`, so the
/// corners are texel-*edge* normalised and the padding of a decoded row never
/// enters the sampling. A unit test in this module walks every output pixel of
/// every rotation against [`yuv_to_rgba`], so this cannot drift from the CPU
/// arrangement.
pub fn corner_uvs(rotation: u16, width: u32, height: u32) -> Result<[[f32; 2]; 4], String> {
    let (out_w, out_h) = rotated_size(width, height, rotation)?;
    if width == 0 || height == 0 {
        return Err(format!("yuv: zero-sized frame {width}x{height}"));
    }
    // Output pixel `(ox, oy)` reads the source pixel below — the same pairing
    // `yuv_to_rgba` emits, read off its four `emit!` loops:
    //
    // | rot | source pixel |
    // |---|---|
    // | 0   | `(ox, oy)` |
    // | 90  | `(oy, h - 1 - ox)` |
    // | 180 | `(w - 1 - ox, h - 1 - oy)` |
    // | 270 | `(w - 1 - oy, ox)` |
    //
    // The corners are evaluated at **half a pixel outside** the output grid —
    // `-0.5` at the near end and `size - 0.5` at the far one — because the
    // shader turns a UV back into a texel with `floor(uv * crop_size)` and
    // interpolation between two *pixel centres* would land the fragment half a
    // texel past the one it must read: `floor((ox + 1) )` is `ox + 1`, the next
    // pixel. Taking the corners at the texel *edges* instead makes the fragment
    // centre land on `ox + 0.5`, which floors to `ox`.
    //
    // That half-pixel is not cosmetic: with centres, the last output row of every
    // non-rotated clip sampled the second-to-last source row, and a quarter turn
    // transposed by a whole texel. `the_rotation_uvs_sample_what_the_cpu_rotates`
    // walks every output pixel against `yuv_to_rgba` precisely to pin this down.
    let uv_at = |ox: f32, oy: f32| -> [f32; 2] {
        let (w, h) = (width as f32, height as f32);
        let (sx, sy) = match rotation {
            0 => (ox, oy),
            90 => (oy, h - 1.0 - ox),
            180 => (w - 1.0 - ox, h - 1.0 - oy),
            _ => (w - 1.0 - oy, ox),
        };
        [(sx + 0.5) / w, (sy + 0.5) / h]
    };
    let (ow, oh) = (out_w as f32, out_h as f32);
    Ok([
        uv_at(-0.5, -0.5),
        uv_at(ow - 0.5, -0.5),
        uv_at(ow - 0.5, oh - 0.5),
        uv_at(-0.5, oh - 0.5),
    ])
}/// The RGBA8 pixels of `planes`, converted on demand.
///
/// This is the *expensive* half of a decoded frame — 2.07M pixels of f32
/// arithmetic at 1080p, plus the rotation — and it exists for the callers that
/// genuinely need pixels rather than samples: the legacy byte path that hands a
/// Java array back, and the tests. The compositor does **not** need it: it
/// samples the planes on the GPU (docs/12 §12.3), so a frame that only ever
/// travels to a texture must never be converted at all.
///
/// Everything needed is already in `planes` — the geometry, the matrix, the
/// range, the rotation and the decoded bytes — which is why this can be lazy
/// without keeping a second copy of the decoder's state alive.
pub fn rgba_from_planes(planes: &YuvPlanes) -> Result<Arc<[u8]>, String> {
    let desc = planes.desc();
    let layout = YuvLayout {
        stride: desc.row_stride[0] as usize,
        slice_height: desc.plane_rows[0] as usize,
    };
    let (out_w, out_h) = rotated_size(desc.width, desc.height, planes.rotation())
        .map_err(|e| format!("rgba_from_planes: {e}"))?;
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    let luma = planes.plane(0).unwrap_or(&[]);
    let cb = planes.plane(1).unwrap_or(&[]);
    let cr = planes.plane(2).unwrap_or(&[]);
    yuv_to_rgba(
        desc.format,
        layout,
        [luma, cb, cr],
        desc.width,
        desc.height,
        planes.matrix(),
        planes.range(),
        planes.rotation(),
        &mut out,
    )
    .map_err(|e| format!("rgba_from_planes: {e}"))?;
    Ok(Arc::from(out.into_boxed_slice()))
}



/// Convert one decoded YUV frame into upright RGBA8.
///
/// * `planes` — see [`YuvFormat`] for the packing rules.
/// * `width`/`height` — the visible (crop) size of the source, in pixels.
/// * `rot` — clockwise rotation to apply: `0`, `90`, `180` or `270`.
/// * `out` — destination; must hold at least
///   `out_w * out_h * 4` bytes where `out_w`/`out_h` are `width`/`height`
///   swapped for `90`/`270`. Exactly that many bytes are written; alpha is
///   always `255`.
///
/// # Errors
/// Zero-sized frames, a rotation that is not a multiple of 90°, planes too
/// short for `layout` (including a `stride` smaller than `width`), or an
/// `out` buffer that is too small.
#[allow(clippy::too_many_arguments)] // signature is fixed by docs/08 §8.4
pub fn yuv_to_rgba(
    format: YuvFormat,
    layout: YuvLayout,
    planes: [&[u8]; 3],
    width: u32,
    height: u32,
    matrix: Matrix,
    range: Range,
    rot: u16,
    out: &mut [u8],
) -> Result<(), String> {
    let (out_w, out_h) =
        rotated_size(width, height, rot).map_err(|e| format!("yuv_to_rgba: {e}"))?;
    let needed = (out_w as usize) * (out_h as usize) * 4;
    if out.len() < needed {
        return Err(format!(
            "yuv_to_rgba: out buffer {} bytes, need {needed}",
            out.len()
        ));
    }

    // One geometry rule for the whole crate: which byte of the decoded buffer is
    // which pixel. The GPU path resolves through the same function, so the two
    // readers cannot disagree about the strides, the chroma rows or the crop.
    let desc = YuvPlaneDesc::resolve(format, layout, width, height)
        .map_err(|e| format!("yuv_to_rgba: {e}"))?;
    // The planes arrive already sliced out of the decoded buffer, so it is their
    // lengths — not their offsets — that have to cover the geometry.
    for (index, len) in desc.plane_len.iter().enumerate() {
        if *len == 0 {
            continue;
        }
        if planes[index].len() < *len {
            return Err(format!(
                "yuv_to_rgba: plane {index} {} bytes, need {len}",
                planes[index].len()
            ));
        }
    }

    let w = width as usize;
    let h = height as usize;
    let sampler = Sampler {
        format,
        y: &planes[0][..desc.plane_len[0]],
        c: &planes[1][..desc.plane_len[1]],
        v: &planes[2][..desc.plane_len[2]],
        stride: desc.row_stride[0] as usize,
        c_stride: desc.row_stride[1] as usize,
        coeffs: Coeffs::new(matrix, range),
    };

    // Row-major walk of the *output*, asking the sampler for the inverse-mapped
    // source pixel. `idx` is the output cursor; the inner loop never allocates.
    let mut idx = 0usize;
    macro_rules! emit {
        ($x:expr, $y:expr) => {{
            let [r, g, b] = sampler.rgb($x, $y);
            out[idx] = r;
            out[idx + 1] = g;
            out[idx + 2] = b;
            out[idx + 3] = 255;
            idx += 4;
        }};
    }

    match rot {
        0 => {
            for y in 0..h {
                for x in 0..w {
                    emit!(x, y);
                }
            }
        }
        // 90° CW: src(x, y) lands at out(h - 1 - y, x). Output rows are `w`
        // tall, so `oy` (the source column) drives the outer loop.
        90 => {
            for oy in 0..w {
                for ox in 0..h {
                    emit!(oy, h - 1 - ox);
                }
            }
        }
        // 180°: src(x, y) lands at out(w - 1 - x, h - 1 - y).
        180 => {
            for oy in 0..h {
                let sy = h - 1 - oy;
                for ox in 0..w {
                    emit!(w - 1 - ox, sy);
                }
            }
        }
        // 270° CW (90° CCW): src(x, y) lands at out(y, w - 1 - x).
        _ => {
            for oy in 0..w {
                for ox in 0..h {
                    emit!(w - 1 - oy, ox);
                }
            }
        }
    }

    debug_assert_eq!(idx, needed);
    Ok(())
}

/// Per-pixel plane reads honouring `stride`/`slice_height`.
struct Sampler<'a> {
    format: YuvFormat,
    y: &'a [u8],
    c: &'a [u8],
    v: &'a [u8],
    stride: usize,
    c_stride: usize,
    coeffs: Coeffs,
}

impl Sampler<'_> {
    #[inline]
    fn rgb(&self, x: usize, y: usize) -> [u8; 3] {
        let luma = self.y[y * self.stride + x];
        let (cb, cr) = match self.format {
            YuvFormat::I420 => {
                let i = (y >> 1) * self.c_stride + (x >> 1);
                (self.c[i], self.v[i])
            }
            YuvFormat::Nv12 => {
                let i = (y >> 1) * self.c_stride + ((x >> 1) << 1);
                (self.c[i], self.c[i + 1])
            }
            YuvFormat::Nv21 => {
                let i = (y >> 1) * self.c_stride + ((x >> 1) << 1);
                (self.c[i + 1], self.c[i])
            }
        };
        self.coeffs.rgb(luma, cb, cr)
    }
}

/// YCbCr → RGB coefficients for one (matrix, range) pair.
///
/// Derived from the non-constant-luminance model
/// `R = Y + 2(1-Kr)·Cr`, `B = Y + 2(1-Kb)·Cb`,
/// `G = Y - (2Kb(1-Kb)/Kg)·Cb - (2Kr(1-Kr)/Kg)·Cr`, with the
/// studio-range stretches `255/219` (luma) and `255/224` (chroma). For
/// BT.601/limited this reproduces the classic 1.596 / 2.018 / 0.391 / 0.813
/// table exactly.
struct Coeffs {
    y_scale: f32,
    y_offset: f32,
    c_center: f32,
    r_v: f32,
    g_u: f32,
    g_v: f32,
    b_u: f32,
}

impl Coeffs {
    fn new(matrix: Matrix, range: Range) -> Self {
        let (kr, kb) = match matrix {
            Matrix::Bt601 => (0.299_f32, 0.114_f32),
            Matrix::Bt709 => (0.2126_f32, 0.0722_f32),
            Matrix::Bt2020 => (0.2627_f32, 0.0593_f32),
        };
        let kg = 1.0 - kr - kb;
        let (y_scale, y_offset, c_scale, c_center) = match range {
            Range::Limited => (255.0 / 219.0, 16.0, 255.0 / 224.0, 128.0),
            Range::Full => (1.0, 0.0, 1.0, 128.0),
        };
        Self {
            y_scale,
            y_offset,
            c_center,
            r_v: c_scale * 2.0 * (1.0 - kr),
            g_u: c_scale * 2.0 * kb * (1.0 - kb) / kg,
            g_v: c_scale * 2.0 * kr * (1.0 - kr) / kg,
            b_u: c_scale * 2.0 * (1.0 - kb),
        }
    }

    #[inline]
    fn rgb(&self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        let luma = self.y_scale * (f32::from(y) - self.y_offset);
        let u = f32::from(cb) - self.c_center;
        let v = f32::from(cr) - self.c_center;
        [
            quantize(luma + self.r_v * v),
            quantize(luma - self.g_u * u - self.g_v * v),
            quantize(luma + self.b_u * u),
        ]
    }
}

/// The same seven coefficients [`Coeffs`] uses, in the order the GPU path's
/// uniform expects: `y_scale`, `y_offset`, `c_center`, `r_v`, `g_u`, `g_v`,
/// `b_u`.
///
/// This is the *only* place the YCbCr → RGB matrix is computed in the whole
/// workspace. The CPU converter calls [`Coeffs::new`] and the GPU path uploads
/// what [`coeffs_f32`] returns, so the shader needs no colour constants of its
/// own — it cannot be given a wrong matrix or a wrong range stretch, because it
/// has nowhere to put one.
pub fn coeffs_f32(matrix: Matrix, range: Range) -> [f32; 7] {
    let c = Coeffs::new(matrix, range);
    [
        c.y_scale, c.y_offset, c.c_center, c.r_v, c.g_u, c.g_v, c.b_u,
    ]
}

/// One decoded frame as planes, ready to be handed to the GPU instead of being
/// converted first.
///
/// The bytes are shared, never copied: one [`Arc`] covers all three planes and
/// the description says where each of them starts, so a decoder that owns the
/// decoded buffer can hand it over as it is. [`YuvPlanes::to_rgba`] is the CPU
/// reference the GPU path is tested against, and the export path still converts
/// through it.
///
/// `rotation` is the decoder's clockwise rotation. It is *not* applied here —
/// [`corner_uvs`] folds it into the quad that samples these planes, exactly as
/// [`yuv_to_rgba`] folds it into the pixels it writes.
#[derive(Debug, Clone)]
pub struct YuvPlanes {
    desc: YuvPlaneDesc,
    matrix: Matrix,
    range: Range,
    rotation: u16,
    bytes: Arc<[u8]>,
}

impl YuvPlanes {
    /// Describe `bytes` as the planes of a `width`×`height` crop.
    ///
    /// # Errors
    /// Whatever [`YuvPlaneDesc::resolve`] rejects (a zero-sized crop, a stride
    /// below the width, an odd luma stride for `I420`, …), a `rotation` that is
    /// not a multiple of 90, or a `bytes` buffer shorter than the geometry needs.
    pub fn new(
        format: YuvFormat,
        layout: YuvLayout,
        width: u32,
        height: u32,
        matrix: Matrix,
        range: Range,
        rotation: u16,
        bytes: Arc<[u8]>,
    ) -> Result<Self, String> {
        let desc = YuvPlaneDesc::resolve(format, layout, width, height)?;
        // Validating the rotation here keeps a bad value from reaching the quad
        // as silently wrong geometry.
        rotated_size(width, height, rotation)?;
        let needed = desc.buffer_len();
        if bytes.len() < needed {
            return Err(format!(
                "YuvPlanes: decoded buffer {} bytes, need {needed}",
                bytes.len()
            ));
        }
        Ok(Self {
            desc,
            matrix,
            range,
            rotation,
            bytes,
        })
    }

    /// The plane geometry, resolved once and shared with the CPU converter.
    pub fn desc(&self) -> &YuvPlaneDesc {
        &self.desc
    }

    /// The packed, crop-relative resolution of the visible plane.
    pub fn width(&self) -> u32 {
        self.desc.width
    }

    /// The packed, crop-relative resolution of the visible plane.
    pub fn height(&self) -> u32 {
        self.desc.height
    }

    /// The decoder's clockwise rotation.
    pub fn rotation(&self) -> u16 {
        self.rotation
    }

    /// The shared decoded buffer, all three planes included.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// The YCbCr → RGB coefficients the GPU path multiplies with.
    pub fn coeffs(&self) -> [f32; 7] {
        coeffs_f32(self.matrix, self.range)
    }

    /// The luma→RGB matrix this frame is described with.
    pub fn matrix(&self) -> Matrix {
        self.matrix
    }

    /// The sample range this frame is described with.
    pub fn range(&self) -> Range {
        self.range
    }

    /// The four corner UVs that sample these planes with the rotation applied.
    pub fn corner_uvs(&self) -> Result<[[f32; 2]; 4], String> {
        corner_uvs(self.rotation, self.desc.width, self.desc.height)
    }

    /// Plane `index`, or `None` for the plane this packing leaves unused.
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        self.desc.plane(&self.bytes, index)
    }

    /// Convert to upright RGBA8 — the reference implementation, unchanged.
    ///
    /// # Errors
    /// Whatever [`yuv_to_rgba`] rejects, or an `out` buffer shorter than the
    /// rotated frame.
    pub fn to_rgba(&self, out: &mut [u8]) -> Result<(), String> {
        let empty: &[u8] = &[];
        let planes = [
            self.plane(0).unwrap_or(empty),
            self.plane(1).unwrap_or(empty),
            self.plane(2).unwrap_or(empty),
        ];
        yuv_to_rgba(
            self.desc.format,
            YuvLayout {
                stride: self.desc.row_stride[0] as usize,
                slice_height: self.desc.plane_rows[0] as usize,
            },
            planes,
            self.desc.width,
            self.desc.height,
            self.matrix,
            self.range,
            self.rotation,
            out,
        )
    }
}

/// Clamp to `0..=255` and round half-up.
#[inline]
fn quantize(x: f32) -> u8 {
    let x = x + 0.5;
    if x <= 0.0 {
        0
    } else if x >= 255.0 {
        255
    } else {
        x as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 4;
    const H: u32 = 4;

    /// Planar/semi-planar buffer with padding rows and padded strides.
    struct Planes {
        y: Vec<u8>,
        c0: Vec<u8>,
        c1: Vec<u8>,
    }

    impl Planes {
        fn new(format: YuvFormat, stride: usize, slice_height: usize, pad: u8) -> Self {
            let y = vec![0u8; stride * slice_height];
            // I420 chroma is stride/2 wide; interleaved chroma uses the luma
            // stride. Rows are slice_height.div_ceil(2).
            let c_stride = match format {
                YuvFormat::I420 => stride / 2,
                YuvFormat::Nv12 | YuvFormat::Nv21 => stride,
            };
            let c_len = c_stride * slice_height.div_ceil(2);
            Self {
                y,
                c0: vec![pad; c_len],
                c1: vec![pad; c_len],
            }
        }
    }

    fn planes(p: &Planes) -> [&[u8]; 3] {
        [&p.y, &p.c0, &p.c1]
    }

    fn run(format: YuvFormat, p: &Planes, layout: YuvLayout, rot: u16) -> Vec<u8> {
        let (out_w, out_h) = if rot == 90 || rot == 270 {
            (H, W)
        } else {
            (W, H)
        };
        let mut out = vec![0u8; (out_w * out_h * 4) as usize];
        yuv_to_rgba(
            format,
            layout,
            planes(p),
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            rot,
            &mut out,
        )
        .expect("conversion must succeed");
        out
    }

    fn px(buf: &[u8], stride_px: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * stride_px + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// Fill a 4×4 I420 buffer with one grey level, no padding.
    fn grey_i420(yv: u8, uv: u8) -> Planes {
        let mut p = Planes::new(YuvFormat::I420, W as usize, H as usize, 0);
        p.y.fill(yv);
        p.c0.fill(uv);
        p.c1.fill(uv);
        p
    }

    /// The frame the golden tables below describe: per-pixel luma, and one
    /// chroma pair per 2×2 cell, which is what 4:2:0 actually samples.
    ///
    /// Laid out for `format`, so the same picture can be converted from any of
    /// the three packings and the results compared.
    fn golden_planes(format: YuvFormat) -> Planes {
        let w = W as usize;
        let mut p = Planes::new(format, w, H as usize, 0);
        for y in 0..H as usize {
            for x in 0..w {
                p.y[y * w + x] = (16 + 3 * x + 7 * y) as u8;
            }
        }
        let c_stride = match format {
            YuvFormat::I420 => w / 2,
            YuvFormat::Nv12 | YuvFormat::Nv21 => w,
        };
        for j in 0..H as usize / 2 {
            for i in 0..w / 2 {
                let cb = (119 + 4 * i) as u8;
                let cr = (139 + 4 * j + 2 * i) as u8;
                match format {
                    YuvFormat::I420 => {
                        let at = j * c_stride + i;
                        p.c0[at] = cb;
                        p.c1[at] = cr;
                    }
                    YuvFormat::Nv12 => {
                        let at = j * c_stride + 2 * i;
                        p.c0[at] = cb;
                        p.c0[at + 1] = cr;
                    }
                    YuvFormat::Nv21 => {
                        let at = j * c_stride + 2 * i;
                        p.c0[at] = cr;
                        p.c0[at + 1] = cb;
                    }
                }
            }
        }
        p
    }

    /// The exact bytes [`yuv_to_rgba`] must produce for [`golden_planes`], one
    /// table per (matrix, range).
    ///
    /// Pinned as literals rather than recomputed from [`Coeffs`], because the
    /// point of the test is that a *memory* change — an uninitialised
    /// destination, a shared pixel buffer — cannot shift a single colour byte,
    /// and an expectation derived from the same coefficients would happily
    /// agree with a changed coefficient.
    ///
    /// The values come from the documented `Coeffs` formulas evaluated in `f32`.
    /// The nearest any channel sits to a truncation boundary is 0.0204, four
    /// orders of magnitude clear of `f32` rounding, so the tables are pinned
    /// rather than flaky.
    mod golden {
        pub const BT601_LIMITED: [u8; 64] = [
            18, 0, 0, 255, 21, 0, 0, 255, 28, 0, 0, 255, 31, 2, 0, 255, 26, 3, 0, 255,
            29, 6, 0, 255, 36, 7, 5, 255, 39, 10, 9, 255, 40, 8, 0, 255, 44, 11, 2, 255,
            50, 11, 13, 255, 54, 15, 17, 255, 48, 16, 6, 255, 52, 19, 10, 255, 59, 20,
            21, 255, 62, 23, 25, 255,
        ];
        pub const BT601_FULL: [u8; 64] = [
            31, 11, 0, 255, 34, 14, 3, 255, 40, 14, 13, 255, 43, 17, 16, 255, 38, 18, 7,
            255, 41, 21, 10, 255, 47, 21, 20, 255, 50, 24, 23, 255, 51, 22, 14, 255, 54,
            25, 17, 255, 60, 26, 27, 255, 63, 29, 30, 255, 58, 29, 21, 255, 61, 32, 24,
            255, 67, 33, 34, 255, 70, 36, 37, 255,
        ];
        pub const BT709_LIMITED: [u8; 64] = [
            20, 0, 0, 255, 23, 0, 0, 255, 30, 1, 0, 255, 34, 5, 0, 255, 28, 4, 0, 255,
            31, 8, 0, 255, 38, 9, 5, 255, 42, 13, 8, 255, 43, 10, 0, 255, 47, 14, 1, 255,
            54, 15, 13, 255, 57, 19, 16, 255, 51, 18, 5, 255, 55, 22, 9, 255, 62, 23, 21,
            255, 65, 27, 24, 255,
        ];
        pub const BT709_FULL: [u8; 64] = [
            33, 13, 0, 255, 36, 16, 2, 255, 42, 17, 13, 255, 45, 20, 16, 255, 40, 20, 6,
            255, 43, 23, 9, 255, 49, 24, 20, 255, 52, 27, 23, 255, 54, 25, 13, 255, 57,
            28, 16, 255, 63, 29, 27, 255, 66, 32, 30, 255, 61, 32, 20, 255, 64, 35, 23,
            255, 70, 36, 34, 255, 73, 39, 37, 255,
        ];
        pub const BT2020_LIMITED: [u8; 64] = [
            18, 0, 0, 255, 22, 0, 0, 255, 29, 0, 0, 255, 32, 3, 0, 255, 27, 3, 0, 255,
            30, 6, 0, 255, 37, 8, 4, 255, 40, 11, 8, 255, 41, 8, 0, 255, 45, 12, 1, 255,
            52, 13, 13, 255, 55, 17, 16, 255, 50, 16, 5, 255, 53, 20, 9, 255, 60, 21, 21,
            255, 63, 25, 24, 255,
        ];
        pub const BT2020_FULL: [u8; 64] = [
            32, 11, 0, 255, 35, 14, 2, 255, 41, 15, 13, 255, 44, 18, 16, 255, 39, 18, 6,
            255, 42, 21, 9, 255, 48, 22, 20, 255, 51, 25, 23, 255, 52, 23, 13, 255, 55,
            26, 16, 255, 61, 27, 27, 255, 64, 30, 30, 255, 59, 30, 20, 255, 62, 33, 23,
            255, 68, 34, 34, 255, 71, 37, 37, 255,
        ];
    }

    /// The conversion is pinned to known colours, one table per matrix/range.
    #[test]
    fn conversion_matches_the_pinned_colour() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = golden_planes(YuvFormat::I420);
        for (matrix, range, expect) in [
            (Matrix::Bt601, Range::Limited, golden::BT601_LIMITED),
            (Matrix::Bt601, Range::Full, golden::BT601_FULL),
            (Matrix::Bt709, Range::Limited, golden::BT709_LIMITED),
            (Matrix::Bt709, Range::Full, golden::BT709_FULL),
            (Matrix::Bt2020, Range::Limited, golden::BT2020_LIMITED),
            (Matrix::Bt2020, Range::Full, golden::BT2020_FULL),
        ] {
            let mut out = vec![0u8; (W * H * 4) as usize];
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                W,
                H,
                matrix,
                range,
                0,
                &mut out,
            )
            .unwrap();
            assert_eq!(out, expect.to_vec(), "{matrix:?}/{range:?}");
        }
    }

    /// The destination the decoder hands the converter is no longer zeroed
    /// (see [`crate::video::uninit_rgba`]), and this is what makes that safe:
    /// the uninitialised path must produce exactly the bytes the zero-filled
    /// path does, for every packing, rotation, matrix and range.
    ///
    /// The reference path is pre-filled with `0xCD` rather than `0` so a byte
    /// the conversion *failed* to write shows up instead of quietly matching.
    #[test]
    fn an_uninitialised_destination_gives_the_same_bytes() {
        use crate::video::{filled_rgba, init_rgba, uninit_rgba};
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        for format in [YuvFormat::I420, YuvFormat::Nv12, YuvFormat::Nv21] {
            let p = golden_planes(format);
            for rot in [0u16, 90, 180, 270] {
                for (matrix, range) in [
                    (Matrix::Bt601, Range::Limited),
                    (Matrix::Bt709, Range::Limited),
                    (Matrix::Bt2020, Range::Full),
                ] {
                    let (out_w, out_h) = if rot == 90 || rot == 270 {
                        (H, W)
                    } else {
                        (W, H)
                    };
                    let len = (out_w * out_h * 4) as usize;
                    let label = format!("{format:?} rot {rot} {matrix:?}/{range:?}");

                    let mut poisoned = vec![0xCDu8; len];
                    yuv_to_rgba(
                        format,
                        layout,
                        planes(&p),
                        W,
                        H,
                        matrix,
                        range,
                        rot,
                        &mut poisoned,
                    )
                    .unwrap_or_else(|e| panic!("{label}: {e}"));

                    let mut storage = uninit_rgba(len);
                    // SAFETY: `yuv_to_rgba` writes every byte of the
                    // destination before returning `Ok`, which is the whole
                    // reason the buffer may be uninitialised.
                    let converted = unsafe {
                        yuv_to_rgba(
                            format,
                            layout,
                            planes(&p),
                            W,
                            H,
                            matrix,
                            range,
                            rot,
                            init_rgba(&mut storage),
                        )
                    };
                    converted.unwrap_or_else(|e| panic!("{label}: {e}"));
                    // SAFETY: the conversion above returned `Ok`, so every byte
                    // of `storage` holds a written `u8`.
                    let uninit = unsafe { filled_rgba(storage) };

                    assert_eq!(&*uninit, &poisoned[..], "{label}");
                }
            }
        }
    }

    #[test]
    fn limited_grey_black_and_white() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let black = run(YuvFormat::I420, &grey_i420(16, 128), layout, 0);
        assert_eq!(px(&black, W as usize, 0, 0), [0, 0, 0, 255]);
        assert_eq!(px(&black, W as usize, 3, 3), [0, 0, 0, 255]);

        let white = run(YuvFormat::I420, &grey_i420(235, 128), layout, 0);
        assert_eq!(px(&white, W as usize, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn full_range_endpoints() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let mut out = vec![0u8; (W * H * 4) as usize];
        for (y, expect) in [(0u8, 0u8), (255, 255)] {
            let p = grey_i420(y, 128);
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                W,
                H,
                Matrix::Bt601,
                Range::Full,
                0,
                &mut out,
            )
            .unwrap();
            assert_eq!(px(&out, W as usize, 1, 2), [expect, expect, expect, 255]);
        }
    }

    #[test]
    fn bt709_limited_red_is_dominant() {
        // BT.709 100% red, studio range.
        let mut p = grey_i420(63, 0);
        p.c0.fill(102); // Cb
        p.c1.fill(240); // Cr
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let mut out = vec![0u8; (W * H * 4) as usize];
        yuv_to_rgba(
            YuvFormat::I420,
            layout,
            planes(&p),
            W,
            H,
            Matrix::Bt709,
            Range::Limited,
            0,
            &mut out,
        )
        .unwrap();
        let [r, g, b, a] = px(&out, W as usize, 2, 2);
        assert_eq!(a, 255);
        assert!(r > 200, "red must dominate, got {r}");
        assert!(g < 60 && b < 60, "green/blue must be crushed, got {g}/{b}");
        assert!(r > g && r > b);
    }

    #[test]
    fn nv12_and_nv21_swizzle_chroma() {
        // Same physical bytes: right interleaved on the NV12 read, wrong on NV21.
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let mut p = Planes::new(YuvFormat::Nv12, W as usize, H as usize, 0);
        p.y.fill(63);
        for pair in p.c0.chunks_exact_mut(2) {
            pair[0] = 102; // U
            pair[1] = 240; // V
        }
        let mut a = vec![0u8; (W * H * 4) as usize];
        let mut b = vec![0u8; (W * H * 4) as usize];
        yuv_to_rgba(
            YuvFormat::Nv12,
            layout,
            planes(&p),
            W,
            H,
            Matrix::Bt709,
            Range::Limited,
            0,
            &mut a,
        )
        .unwrap();
        yuv_to_rgba(
            YuvFormat::Nv21,
            layout,
            planes(&p),
            W,
            H,
            Matrix::Bt709,
            Range::Limited,
            0,
            &mut b,
        )
        .unwrap();
        assert!(a[0] > 200, "NV12 must read U first: {:?}", &a[..4]);
        assert_ne!(a, b);
    }

    #[test]
    fn stride_padding_is_never_read() {
        // 4×4 visible inside an 8-wide, 6-row buffer full of poison padding.
        let mut p = Planes::new(YuvFormat::Nv12, 8, 6, 0xAB);
        p.y.fill(0xAB);
        p.c0.fill(0xAB);
        for y in 0..H as usize {
            for x in 0..W as usize {
                p.y[y * 8 + x] = 235;
            }
        }
        for row in 0..(H as usize) / 2 {
            for x in 0..W as usize {
                p.c0[row * 8 + (x << 1)] = 128;
                p.c0[row * 8 + (x << 1) + 1] = 128;
            }
        }
        let layout = YuvLayout {
            stride: 8,
            slice_height: 6,
        };
        let mut out = vec![0u8; (W * H * 4) as usize];
        yuv_to_rgba(
            YuvFormat::Nv12,
            layout,
            planes(&p),
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            0,
            &mut out,
        )
        .unwrap();
        assert_eq!(px(&out, W as usize, 0, 0), [255, 255, 255, 255]);
        // The poison bytes only exist in padding; a leak would show as
        // 0xAB-derived channels on some pixel.
        for (i, chunk) in out.chunks_exact(4).enumerate() {
            assert_eq!(
                [chunk[0], chunk[1], chunk[2], chunk[3]],
                [255, 255, 255, 255],
                "pixel {i} leaked padding"
            );
        }
    }

    #[test]
    fn slice_height_padding_rows_are_cropped() {
        // Visible 4×2 inside a 4-wide, 4-row buffer; rows 2..4 are white.
        let layout = YuvLayout {
            stride: 4,
            slice_height: 4,
        };
        let mut p = Planes::new(YuvFormat::I420, 4, 4, 0);
        p.y.fill(235); // padding rows (and everything) start white
        for x in 0..4 {
            p.y[x] = 16;
        }
        p.c0.fill(128);
        p.c1.fill(128);
        let mut out = vec![0u8; (W * 2 * 4) as usize];
        yuv_to_rgba(
            YuvFormat::I420,
            layout,
            planes(&p),
            W,
            2,
            Matrix::Bt601,
            Range::Limited,
            0,
            &mut out,
        )
        .unwrap();
        // Row 0 was blackened, row 1 still white, and the buffer holds only
        // the two visible rows (no padding rows copied in).
        assert_eq!(px(&out, W as usize, 0, 0), [0, 0, 0, 255]);
        assert_eq!(px(&out, W as usize, 0, 1), [255, 255, 255, 255]);
        assert_eq!(out.len(), (W * 2 * 4) as usize);
    }

    #[test]
    fn rotation_180_maps_opposite_corner() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let mut p = grey_i420(16, 128);
        p.y[0] = 235; // src(0, 0) = white
        let out = run(YuvFormat::I420, &p, layout, 180);
        assert_eq!(px(&out, W as usize, W as usize - 1, H as usize - 1), [
            255, 255, 255, 255
        ]);
        assert_eq!(px(&out, W as usize, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn rotation_90_swaps_axes_and_maps_corner() {
        // Source is 2 wide, 3 tall; 90° CW => output is 3 wide, 2 tall.
        let (w, h) = (2u32, 3u32);
        let layout = YuvLayout {
            stride: w as usize,
            slice_height: h as usize,
        };
        let mut p = Planes::new(YuvFormat::I420, w as usize, h as usize, 0);
        p.y.fill(16);
        p.c0.fill(128);
        p.c1.fill(128);
        p.y[0] = 235; // src(0, 0) = white, everything else black
        let (out_w, out_h) = (h as usize, w as usize);
        let mut out = vec![0u8; out_w * out_h * 4];
        yuv_to_rgba(
            YuvFormat::I420,
            layout,
            planes(&p),
            w,
            h,
            Matrix::Bt601,
            Range::Limited,
            90,
            &mut out,
        )
        .unwrap();
        assert_eq!(out.len(), out_w * out_h * 4);
        // 90° CW puts the source top-left at the output top-right.
        assert_eq!(px(&out, out_w, out_w - 1, 0), [255, 255, 255, 255]);
        assert_eq!(px(&out, out_w, 0, 0), [0, 0, 0, 255]);
        assert_eq!(px(&out, out_w, out_w - 1, out_h - 1), [0, 0, 0, 255]);
        assert_eq!(px(&out, out_w, 0, out_h - 1), [0, 0, 0, 255]);
    }

    #[test]
    fn rotation_270_is_inverse_of_90() {
        let (w, h) = (2u32, 3u32);
        let layout = YuvLayout {
            stride: w as usize,
            slice_height: h as usize,
        };
        let mut p = Planes::new(YuvFormat::I420, w as usize, h as usize, 0);
        p.y.fill(16);
        p.c0.fill(128);
        p.c1.fill(128);
        p.y[0] = 235; // src(0, 0) = white
        let (out_w, out_h) = (h as usize, w as usize);
        let mut out = vec![0u8; out_w * out_h * 4];
        yuv_to_rgba(
            YuvFormat::I420,
            layout,
            planes(&p),
            w,
            h,
            Matrix::Bt601,
            Range::Limited,
            270,
            &mut out,
        )
        .unwrap();
        // 270° CW puts the source top-left at the output bottom-left.
        assert_eq!(px(&out, out_w, 0, out_h - 1), [255, 255, 255, 255]);
        assert_eq!(px(&out, out_w, 0, 0), [0, 0, 0, 255]);
        assert_eq!(px(&out, out_w, out_w - 1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn rotation_maps_every_pixel_row_major() {
        // A per-pixel check, so a transposed (column-major) write would fail
        // even though the corners still landed correctly.
        // Even on both sides: an I420 luma stride must be even, and the point of
        // this test is the rotation arithmetic, not an odd-stride rejection.
        let (w, h) = (4usize, 6usize);
        let stride = 4usize; // padded, even: what a decoder actually reports
        let layout = YuvLayout {
            stride,
            slice_height: h,
        };
        let value = |x: usize, y: usize| (16 + 2 * (x + 10 * y)) as u8;
        let mut p = Planes::new(YuvFormat::I420, stride, h, 0);
        p.c0.fill(128);
        p.c1.fill(128);
        for y in 0..h {
            for x in 0..w {
                p.y[y * stride + x] = value(x, y);
            }
        }
        for rot in [90u16, 180, 270] {
            let (out_w, out_h) = match rot {
                90 | 270 => (h, w),
                _ => (w, h),
            };
            let mut out = vec![0u8; out_w * out_h * 4];
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                w as u32,
                h as u32,
                Matrix::Bt601,
                Range::Limited,
                rot,
                &mut out,
            )
            .unwrap();
            for oy in 0..out_h {
                for ox in 0..out_w {
                    // The source pixel that must be sitting at (ox, oy).
                    let (sx, sy) = match rot {
                        90 => (oy, h - 1 - ox),
                        180 => (w - 1 - ox, h - 1 - oy),
                        _ => (w - 1 - oy, ox),
                    };
                    let expect = grey_px(value(sx, sy));
                    assert_eq!(
                        px(&out, out_w, ox, oy),
                        expect,
                        "rot {rot} at out({ox},{oy}) should be src({sx},{sy})"
                    );
                }
            }
        }
    }

    /// Reference conversion of one grey sample (neutral chroma), 1×1.
    fn grey_px(y: u8) -> [u8; 4] {
        let mut out = [0u8; 4];
        // 4:2:0 wants an even stride, so the 1-pixel row is padded by one byte.
        let luma = [y, 0u8];
        yuv_to_rgba(
            YuvFormat::I420,
            YuvLayout {
                stride: 2,
                slice_height: 1,
            },
            [&luma, &[128], &[128]],
            1,
            1,
            Matrix::Bt601,
            Range::Limited,
            0,
            &mut out,
        )
        .unwrap();
        out
    }

    #[test]
    fn rejects_bad_input() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = grey_i420(128, 128);
        let mut out = vec![0u8; (W * H * 4) as usize];

        // Zero-sized frames.
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                0,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                W,
                0,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
        // Bad rotation.
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                45,
                &mut out
            )
            .is_err()
        );
        // Out buffer too small (90° needs W*H*4 too, but 180° of a 4×4 into 8 bytes).
        let mut tiny = vec![0u8; 8];
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut tiny
            )
            .is_err()
        );
        // stride < width.
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                YuvLayout {
                    stride: 2,
                    slice_height: H as usize
                },
                planes(&p),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
        // Truncated planes.
        let short = Planes {
            y: vec![0u8; 4],
            c0: vec![0u8; 4],
            c1: vec![0u8; 4],
        };
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&short),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
        // Nv12 needs its interleaved chroma plane to cover the luma stride.
        let bad_nv12 = Planes {
            y: vec![0u8; (W * H) as usize],
            c0: vec![0u8; 4],
            c1: Vec::new(),
        };
        assert!(
            yuv_to_rgba(
                YuvFormat::Nv12,
                layout,
                planes(&bad_nv12),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
        // An odd luma stride cannot be halved into chroma rows that still
        // cover every sampled column.
        assert!(
            yuv_to_rgba(
                YuvFormat::I420,
                YuvLayout {
                    stride: 5,
                    slice_height: H as usize
                },
                planes(&p),
                W,
                H,
                Matrix::Bt601,
                Range::Limited,
                0,
                &mut out
            )
            .is_err()
        );
    }

    #[test]
    fn matrix_and_range_change_coefficients() {
        // A mid-grey luma with neutral chroma is grey for every matrix/range.
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = grey_i420(126, 128);
        for matrix in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020] {
            for range in [Range::Limited, Range::Full] {
                let mut out = vec![0u8; (W * H * 4) as usize];
                yuv_to_rgba(
                    YuvFormat::I420,
                    layout,
                    planes(&p),
                    W,
                    H,
                    matrix,
                    range,
                    0,
                    &mut out,
                )
                .unwrap();
                let [r, g, b, _] = px(&out, W as usize, 1, 1);
                assert_eq!([r, g, b], [r, r, r], "neutral chroma must stay neutral");
            }
        }
    }

    /// Sample one pixel the way `shader_yuv.wgsl` does: `floor(uv * crop)` for
    /// the packed coordinate, then the same `x >> 1` / `y >> 1` chroma fetch
    /// [`Sampler::rgb`] performs.
    ///
    /// This is the only place in the crate that knows how the shader reads, so
    /// the tests below are what keep the UV transform honest.
    fn fetch_as_shader_does(
        sampler: &Sampler<'_>,
        uv: [f32; 2],
        crop: (u32, u32),
    ) -> [u8; 3] {
        let x = (uv[0] * crop.0 as f32) as i64;
        let y = (uv[1] * crop.1 as f32) as i64;
        let x = x.clamp(0, i64::from(crop.0) - 1) as usize;
        let y = y.clamp(0, i64::from(crop.1) - 1) as usize;
        sampler.rgb(x, y)
    }

    /// Barycentric interpolation of a quad's four corner UVs, at output pixel
    /// `(ox, oy)`'s centre — the same bilinear map `push_quad` lays out.
    fn uv_at_centre(corners: [[f32; 2]; 4], out_w: u32, out_h: u32, ox: u32, oy: u32) -> [f32; 2] {
        let tx = (ox as f32 + 0.5) / out_w as f32;
        let ty = (oy as f32 + 0.5) / out_h as f32;
        let lerp = |a: [f32; 2], b: [f32; 2]| {
            [a[0] + (b[0] - a[0]) * tx, a[1] + (b[1] - a[1]) * tx]
        };
        let top = lerp(corners[0], corners[1]);
        let bottom = lerp(corners[3], corners[2]);
        [
            top[0] + (bottom[0] - top[0]) * ty,
            top[1] + (bottom[1] - top[1]) * ty,
        ]
    }

    #[test]
    fn the_rotation_uvs_sample_what_the_cpu_rotates() {
        // A non-square crop with a distinct value per pixel: any mis-pairing of
        // an output pixel with its source shows up immediately. Chroma is flat
        // so the luma channel alone identifies the source pixel.
        // Even width: an I420 luma stride must be even, and this test is about
        // the rotation arithmetic, not about stride validation.
        let w = 6u32;
        let h = 3u32;
        let layout = YuvLayout {
            stride: w as usize,
            slice_height: h as usize,
        };
        let mut p = Planes::new(YuvFormat::I420, w as usize, h as usize, 128);
        for y in 0..h as usize {
            for x in 0..w as usize {
                p.y[y * w as usize + x] = (y * 10 + x * 2 + 1) as u8;
            }
        }
        let sampler = Sampler {
            format: YuvFormat::I420,
            y: &p.y,
            c: &p.c0,
            v: &p.c1,
            stride: w as usize,
            c_stride: (w as usize) / 2,
            coeffs: Coeffs::new(Matrix::Bt601, Range::Limited),
        };
        for rot in [0u16, 90, 180, 270] {
            let corners = corner_uvs(rot, w, h).unwrap();
            let (out_w, out_h) = rotated_size(w, h, rot).unwrap();
            assert_eq!(
                (out_w, out_h),
                if rot == 90 || rot == 270 { (h, w) } else { (w, h) },
                "rot {rot}: the quad has to be the size yuv_to_rgba writes"
            );
            let mut cpu = vec![0u8; (out_w * out_h * 4) as usize];
            yuv_to_rgba(
                YuvFormat::I420,
                layout,
                planes(&p),
                w,
                h,
                Matrix::Bt601,
                Range::Limited,
                rot,
                &mut cpu,
            )
            .unwrap();
            let mut gpu = vec![0u8; (out_w * out_h * 4) as usize];
            for oy in 0..out_h {
                for ox in 0..out_w {
                    let uv = uv_at_centre(corners, out_w, out_h, ox, oy);
                    let [r, g, b] = fetch_as_shader_does(&sampler, uv, (w, h));
                    let i = ((oy * out_w + ox) * 4) as usize;
                    gpu[i] = r;
                    gpu[i + 1] = g;
                    gpu[i + 2] = b;
                    gpu[i + 3] = 255;
                }
            }
            assert_eq!(
                gpu, cpu,
                "rot {rot}: the quad's UVs must read the pixel yuv_to_rgba put there"
            );
        }
    }

    #[test]
    fn rotation_uvs_refuse_what_the_converter_refuses() {
        assert!(corner_uvs(45, 4, 4).is_err());
        assert!(corner_uvs(0, 0, 4).is_err());
        assert!(rotated_size(4, 4, 45).is_err());
    }

    #[test]
    fn planes_carry_the_buffer_without_copying_it() {
        // The geometry is the golden planes' own, not a padded one: this test
        // proves the buffer is shared rather than rebuilt, and a padded stride
        // would demand a padded buffer that the fixture does not have — the
        // padded case is covered by the `yuv_to_rgba` tests instead.
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = golden_planes(YuvFormat::I420);
        let mut buffer = p.y.clone();
        buffer.extend_from_slice(&p.c0);
        buffer.extend_from_slice(&p.c1);
        let bytes: Arc<[u8]> = buffer.clone().into();
        let planes = YuvPlanes::new(
            YuvFormat::I420,
            layout,
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            0,
            Arc::clone(&bytes),
        )
        .unwrap();
        // Shared, not copied: same allocation, and the luma plane is the head of
        // that very buffer.
        assert_eq!(&buffer[..], planes.bytes().as_ref());
        assert_eq!(planes.plane(0).unwrap(), &buffer[..p.y.len()]);
        // The reconstruction through the resolved description is the reference
        // conversion, byte for byte.
        let mut out = vec![0u8; (W * H * 4) as usize];
        planes.to_rgba(&mut out).unwrap();
        assert_eq!(&out[..], &run(YuvFormat::I420, &p, layout, 0)[..]);
    }

    #[test]
    fn planes_refuse_a_short_buffer_or_a_bad_rotation() {
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = golden_planes(YuvFormat::I420);
        let mut buffer = p.y.clone();
        buffer.extend_from_slice(&p.c0);
        buffer.truncate(buffer.len() - 1);
        assert!(YuvPlanes::new(
            YuvFormat::I420,
            layout,
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            0,
            buffer.into()
        )
        .is_err());
        let mut buffer = p.y.clone();
        buffer.extend_from_slice(&p.c0);
        buffer.extend_from_slice(&p.c1);
        assert!(YuvPlanes::new(
            YuvFormat::I420,
            layout,
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            45,
            buffer.into()
        )
        .is_err());
    }

    /// The colour arithmetic of `rumo_render`'s `shader_yuv.wgsl`, transliterated
    /// line for line: `y = y_scale * (luma - y_offset)`,
    /// `quantize(y + r_v * (cr - c_center))`,
    /// `quantize(y - g_u * (cb - c_center) - g_v * (cr - c_center))`,
    /// `quantize(y + b_u * (cb - c_center))` with `quantize(x) = floor(x + 0.5)`
    /// clamped to `0..=255` — the CPU `quantize`, unchanged.
    ///
    /// The shader holds no coefficients of its own; it multiplies with exactly
    /// what [`coeffs_f32`] hands the renderer. What this pins is that the
    /// *arithmetic* built on them is the converter's — the same table the
    /// converter is pinned to below is what the shader's expressions must
    /// produce, for every matrix and range.
    fn shader_rgb(coeffs: [f32; 7], luma: u8, cb: u8, cr: u8) -> [u8; 3] {
        let [y_scale, y_offset, c_center, r_v, g_u, g_v, b_u] = coeffs;
        let y = y_scale * (f32::from(luma) - y_offset);
        let cb = f32::from(cb) - c_center;
        let cr = f32::from(cr) - c_center;
        [
            quantize(y + r_v * cr),
            quantize(y - g_u * cb - g_v * cr),
            quantize(y + b_u * cb),
        ]
    }

    #[test]
    fn the_shader_arithmetic_reproduces_the_pinned_colour() {
        // The expectation is the pinned CPU table, not a recomputation: the same
        // 16 pixels and the same six tables the converter is pinned to, so a GPU
        // whose expressions drifted from the converter's would show up here as
        // differing bytes — a wrong matrix makes video green or pink and nothing
        // crashes.
        let layout = YuvLayout {
            stride: W as usize,
            slice_height: H as usize,
        };
        let p = golden_planes(YuvFormat::I420);
        for (matrix, range, expect) in [
            (Matrix::Bt601, Range::Limited, golden::BT601_LIMITED),
            (Matrix::Bt601, Range::Full, golden::BT601_FULL),
            (Matrix::Bt709, Range::Limited, golden::BT709_LIMITED),
            (Matrix::Bt709, Range::Full, golden::BT709_FULL),
            (Matrix::Bt2020, Range::Limited, golden::BT2020_LIMITED),
            (Matrix::Bt2020, Range::Full, golden::BT2020_FULL),
        ] {
            let coeffs = coeffs_f32(matrix, range);
            for y in 0..H as usize {
                for x in 0..W as usize {
                    // The same nearest 4:2:0 fetch the shader does: one chroma
                    // pair per 2×2 cell, taken at `(x >> 1, y >> 1)`.
                    let i = x >> 1;
                    let j = y >> 1;
                    let cb = (119 + 4 * i) as u8;
                    let cr = (139 + 4 * j + 2 * i) as u8;
                    assert_eq!(p.y[y * W as usize + x], (16 + 3 * x + 7 * y) as u8);
                    let rgb = shader_rgb(coeffs, p.y[y * W as usize + x], cb, cr);
                    let at = ((y * W as usize + x) * 4) as usize;
                    assert_eq!(
                        rgb,
                        [expect[at], expect[at + 1], expect[at + 2]],
                        "{matrix:?}/{range:?} at ({x}, {y})"
                    );
                }
            }
        }
        // And the converter still produces those bytes from the same planes, so
        // the table above is the reference rather than a second opinion.
        let mut out = vec![0u8; (W * H * 4) as usize];
        yuv_to_rgba(
            YuvFormat::I420,
            layout,
            planes(&p),
            W,
            H,
            Matrix::Bt601,
            Range::Limited,
            0,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, golden::BT601_LIMITED.to_vec());
    }

    #[test]
    fn the_gpu_coefficients_are_the_cpu_coefficients() {
        // `coeffs_f32` is what the renderer uploads, so the shader multiplies
        // with exactly these numbers. Pinned against `Coeffs::rgb` over the whole
        // luma domain and a chroma sweep: a shader that reordered or dropped one
        // of the seven would leave this table.
        for matrix in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020] {
            for range in [Range::Limited, Range::Full] {
                let [y_scale, y_offset, c_center, r_v, g_u, g_v, b_u] =
                    coeffs_f32(matrix, range);
                let cpu = Coeffs::new(matrix, range);
                assert_eq!(
                    [y_scale, y_offset, c_center, r_v, g_u, g_v, b_u],
                    [
                        cpu.y_scale, cpu.y_offset, cpu.c_center, cpu.r_v, cpu.g_u, cpu.g_v, cpu.b_u
                    ]
                );
                for y in 0..=u8::MAX {
                    for (cb, cr) in [(0u8, 0u8), (128, 128), (255, 255), (16, 240)] {
                        let luma = y_scale * (f32::from(y) - y_offset);
                        let u = f32::from(cb) - c_center;
                        let v = f32::from(cr) - c_center;
                        let shader = [
                            quantize(luma + r_v * v),
                            quantize(luma - g_u * u - g_v * v),
                            quantize(luma + b_u * u),
                        ];
                        assert_eq!(
                            shader,
                            cpu.rgb(y, cb, cr),
                            "{matrix:?}/{range:?} at luma {y} chroma {cb},{cr}"
                        );
                    }
                }
            }
        }
    }
}

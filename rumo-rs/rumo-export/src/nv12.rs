// SPDX-License-Identifier: Apache-2.0

//! RGBA/ARGB -> NV12 (YUV420 semi-planar) conversion, BT.601 full range.
//!
//! Matrix (JFIF / "computer" range, R,G,B in 0..=255):
//!
//! ```text
//! Y =  0.299R + 0.587G + 0.114B
//! U = -0.168736R - 0.331264G + 0.5B + 128
//! V =  0.5R - 0.418688G - 0.081312B + 128
//! ```
//!
//! Full range is used because it is the lossless-of-graphics choice; if a
//! device encoder insists on studio range this module is the single place
//! to change the matrix. `U`/`V` are averaged over each 2x2 block
//! (4:2:0 chroma subsampling).
//!
//! Both input layouts are four bytes per pixel — tightly-packed `[R,G,B,A]`
//! bytes or one packed `0xAARRGGBB` word (a Java `int`, hence `i32`) — so the
//! alpha byte is dropped and the two entry points are the same arithmetic over
//! the same channels.
//!
//! # Why luma stays in f32 and chroma became integers
//!
//! Chroma is computed in integers: the coefficients are exact rationals scaled
//! by 1e6, the 2x2 average is a factor 4 that folds into the denominator, and
//! the tie rule is `f32::round`'s (half away from zero). This was checked
//! exhaustively against the f32 pass over every possible block sum
//! (1021³ combinations) — no differing byte.
//!
//! Luma deliberately keeps the f32 expression. Its integer form rounds half
//! away from zero, the f32 sum lands just below the tie and rounds down, and
//! the two disagree by 1 for 824 of the 16 777 216 RGB triples. This module is
//! the reference the GPU pack shader (`rumo_render::nv12_pack`, docs/12 §12.3)
//! is compared against, so the exact f32 result has to survive here; what that
//! shader saves is the frame readback, not the arithmetic. The coefficients it
//! receives are [`BT601_FULL`], built from the same constants this module
//! computes with — and a test ties the two sets together, because a matrix that
//! drifted would show up only as a tinted export, with nothing failing anywhere.
//!
//! The pass itself is row-blocked and fused: one traversal of the source
//! produces both planes, where the old code read the frame twice and cleared a
//! freshly allocated output buffer every frame. On device the GPU pack pass
//! replaces it for the export; this module stays the reference and the fallback,
//! and is the only implementation when the GPU cannot produce the frame.

use crate::error::{ExportError, Result};
use rumo_render::nv12_pack::{Nv12Coeffs, nv12_len as pack_nv12_len};

/// Byte length of an NV12 frame with the given dimensions, or `None` when
/// the dimensions are zero or odd (4:2:0 needs even width and height).
///
/// The layout has exactly one implementation, in `rumo_render::nv12_pack`: the
/// GPU pack pass sizes its storage buffer from it, so the frame this module
/// validates and the frame that pass writes cannot disagree about its length.
pub fn nv12_len(width: u32, height: u32) -> Option<usize> {
    pack_nv12_len(width, height)
}

/// BT.601 full-range luma coefficients.
///
/// This is the f32 set the GPU pack shader receives; [`luma`] computes with the
/// same numbers, so the two cannot drift apart.
pub const LUMA_COEFFS: [f32; 3] = [0.299, 0.587, 0.114];

/// BT.601 full-range chroma U coefficients, over the 2x2 block average.
pub const CHROMA_U_COEFFS: [f32; 3] = [-0.168_736, -0.331_264, 0.5];

/// BT.601 full-range chroma V coefficients, over the 2x2 block average.
pub const CHROMA_V_COEFFS: [f32; 3] = [0.5, -0.418_688, -0.081_312];

/// Where chroma is centred: 128 for the full-range pair.
pub const CHROMA_BIAS: f32 = 128.0;

/// The matrix handed to the GPU pack pass — the same numbers [`luma`],
/// [`chroma_u`] and [`chroma_v`] compute with.
///
/// `shader_nv12.wgsl` holds no coefficient of its own, so this value is the only
/// place the export's colour can come from, and a wrong matrix has nowhere to
/// hide. `scale` turns a `Rgba8Unorm` texel back into its byte.
pub const BT601_FULL: Nv12Coeffs = Nv12Coeffs {
    luma: LUMA_COEFFS,
    chroma_u: CHROMA_U_COEFFS,
    chroma_v: CHROMA_V_COEFFS,
    bias: CHROMA_BIAS,
    scale: 255.0,
};

/// The integer coefficients below, scaled by [`SCALE`]: the CPU chroma pass is
/// exact in integers (see the module docs), and this is the factor that keeps it
/// in the same units as the f32 set above.
const SCALE: i64 = 1_000_000;

/// Chroma U coefficients in [`SCALE`] units, used by [`chroma_u`].
const U_R: i64 = -168_736;
const U_G: i64 = -331_264;
const U_B: i64 = 500_000;

/// Chroma V coefficients in [`SCALE`] units, used by [`chroma_v`].
const V_R: i64 = 500_000;
const V_G: i64 = -418_688;
const V_B: i64 = -81_312;

/// The 2x2 average as a denominator: four samples, each in [`SCALE`] units.
const BLOCK_DEN: i64 = 4 * SCALE;

/// `128` in the same units as the sums: the chroma bias, folded in.
const BIAS_SCALED: i64 = 128 * 4 * SCALE;

/// Round and clamp exactly like the f32 pass did: `v.round().clamp(0.0, 255.0)`.
#[inline]
fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// BT.601 full-range luma. Kept in f32 on purpose; see the module docs.
#[inline]
fn luma(r: f32, g: f32, b: f32) -> u8 {
    clamp_u8(LUMA_COEFFS[0] * r + LUMA_COEFFS[1] * g + LUMA_COEFFS[2] * b)
}

/// `n / den` rounded half away from zero — the tie rule `f32::round` uses.
#[inline]
fn div_round_away(n: i64, den: i64) -> i64 {
    if n >= 0 {
        (n + den / 2) / den
    } else {
        -((-n + den / 2) / den)
    }
}

/// Clamp a rounded chroma sample. Separate from [`clamp_u8`] because chroma
/// is now integer all the way: rounding happens in [`div_round_away`].
#[inline]
fn clamp_i(v: i64) -> u8 {
    v.clamp(0, 255) as u8
}

/// Chroma U of one 2x2 block, from per-channel sums (four times the average).
///
/// `128.0` becomes [`BIAS_SCALED`] and the block average becomes the
/// [`BLOCK_DEN`] denominator, so one integer divide replaces five f32
/// operations. The coefficients are [`CHROMA_U_COEFFS`] in [`SCALE`] units —
/// named constants, because a test ties them to the f32 set the GPU shader
/// receives, and two copies of a literal are two chances to drift.
#[inline]
fn chroma_u(sum_r: u32, sum_g: u32, sum_b: u32) -> u8 {
    let n = U_R * i64::from(sum_r) + U_G * i64::from(sum_g) + U_B * i64::from(sum_b) + BIAS_SCALED;
    clamp_i(div_round_away(n, BLOCK_DEN))
}

/// Chroma V of one 2x2 block, from per-channel sums (four times the average).
#[inline]
fn chroma_v(sum_r: u32, sum_g: u32, sum_b: u32) -> u8 {
    let n = V_R * i64::from(sum_r) + V_G * i64::from(sum_g) + V_B * i64::from(sum_b) + BIAS_SCALED;
    clamp_i(div_round_away(n, BLOCK_DEN))
}

/// Validate the dimensions and the source length. Returns `(w, h)`.
///
/// `got` and the expected value in the error are counted in the caller's own
/// units, which `units_per_pixel` spells out: 4 for the RGBA byte layout (the
/// error names bytes, as it always has) and 1 for the packed-word layout (the
/// error names pixels).
fn check_source(
    got: usize,
    units_per_pixel: usize,
    width: u32,
    height: u32,
) -> Result<(usize, usize)> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(ExportError::InvalidConfig("frame dimensions overflow"))?;
    let expected = pixels
        .checked_mul(units_per_pixel)
        .ok_or(ExportError::InvalidConfig("frame length overflows"))?;
    if got != expected {
        return Err(ExportError::InvalidFrameLen { expected, got });
    }
    // Confirms even, non-zero dimensions on the way out.
    nv12_len(width, height).ok_or(ExportError::InvalidConfig(
        "NV12 needs even, non-zero dimensions",
    ))?;
    Ok((width as usize, height as usize))
}

/// Validate the output buffer: exactly one NV12 frame, so no stale byte from
/// an earlier frame can reach the encoder.
fn check_out(out: &[u8], width: u32, height: u32) -> Result<()> {
    let out_len = nv12_len(width, height).ok_or(ExportError::InvalidConfig(
        "NV12 needs even, non-zero dimensions",
    ))?;
    if out.len() != out_len {
        return Err(ExportError::InvalidFrameLen {
            expected: out_len,
            got: out.len(),
        });
    }
    Ok(())
}

/// The conversion itself, one 2x2 block at a time.
///
/// `rgb(index)` yields the pixel at a row-major index; the luma plane and the
/// chroma plane are written in the same traversal, so the source is read once
/// instead of twice. Every byte of `out` is written, which is what lets the
/// caller hand in the same buffer for every frame of an export.
fn nv12_core<F>(width: usize, height: usize, mut rgb: F, out: &mut [u8])
where
    F: FnMut(usize) -> (u32, u32, u32),
{
    let uv_plane = width * height;
    let mut row = 0;
    while row < height {
        // Even height is guaranteed by `check_source`, so `bottom` is in range.
        let top = row * width;
        let bottom = top + width;
        let uv_row = uv_plane + (row / 2) * width;
        let mut col = 0;
        while col < width {
            let i = top + col;
            let j = bottom + col;
            let (r0, g0, b0) = rgb(i);
            let (r1, g1, b1) = rgb(i + 1);
            let (r2, g2, b2) = rgb(j);
            let (r3, g3, b3) = rgb(j + 1);
            out[i] = luma(r0 as f32, g0 as f32, b0 as f32);
            out[i + 1] = luma(r1 as f32, g1 as f32, b1 as f32);
            out[j] = luma(r2 as f32, g2 as f32, b2 as f32);
            out[j + 1] = luma(r3 as f32, g3 as f32, b3 as f32);
            let sum_r = r0 + r1 + r2 + r3;
            let sum_g = g0 + g1 + g2 + g3;
            let sum_b = b0 + b1 + b2 + b3;
            out[uv_row + col] = chroma_u(sum_r, sum_g, sum_b);
            out[uv_row + col + 1] = chroma_v(sum_r, sum_g, sum_b);
            col += 2;
        }
        row += 2;
    }
}

/// Convert tightly-packed RGBA (len `width*height*4`, `[R,G,B,A]` per pixel)
/// into the NV12 frame in `out`.
///
/// `out` must be exactly [`nv12_len`] bytes; every byte of it is written, so
/// one buffer can serve a whole export.
pub fn rgba_to_nv12_into(rgba: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<()> {
    let (w, h) = check_source(rgba.len(), 4, width, height)?;
    check_out(out, width, height)?;
    nv12_core(
        w,
        h,
        |i| {
            let p = i * 4;
            (
                u32::from(rgba[p]),
                u32::from(rgba[p + 1]),
                u32::from(rgba[p + 2]),
            )
        },
        out,
    );
    Ok(())
}

/// Convert one packed `0xAARRGGBB` word per pixel into the NV12 frame in `out`
/// — the layout `RumoBridge.renderPreviewEx` hands to Java, so the export no
/// longer has to repack the frame into RGBA bytes to get here.
///
/// The words are `i32` because that is what a Java `int[]` is: the signedness
/// is not a promise, an `0xFF000000` alpha byte simply makes the word negative,
/// and each one is read back as `u32`. Alpha is ignored, exactly as in the
/// RGBA layout.
pub fn argb_i32_to_nv12_into(
    pixels: &[i32],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<()> {
    let (w, h) = check_source(pixels.len(), 1, width, height)?;
    check_out(out, width, height)?;
    nv12_core(
        w,
        h,
        |i| {
            let p = pixels[i] as u32;
            ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF)
        },
        out,
    );
    Ok(())
}

/// Allocating convenience wrapper over [`rgba_to_nv12_into`].
pub fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = check_source(rgba.len(), 4, width, height)?;
    let mut out = vec![0u8; nv12_len(w as u32, h as u32).unwrap_or(0)];
    rgba_to_nv12_into(rgba, width, height, &mut out)?;
    Ok(out)
}

/// Allocating convenience wrapper over [`argb_i32_to_nv12_into`].
pub fn argb_i32_to_nv12(pixels: &[i32], width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = check_source(pixels.len(), 1, width, height)?;
    let mut out = vec![0u8; nv12_len(w as u32, h as u32).unwrap_or(0)];
    argb_i32_to_nv12_into(pixels, width, height, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        [r, g, b, 255]
            .iter()
            .cycle()
            .take((width * height * 4) as usize)
            .copied()
            .collect()
    }

    /// Deterministic noise, so a failing frame is reproducible.
    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0
        }
    }

    fn noisy_rgba(width: u32, height: u32) -> Vec<u8> {
        let mut rng = Lcg(0x5eed_1234);
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            out.push((rng.next() >> 24) as u8);
            out.push((rng.next() >> 24) as u8);
            out.push((rng.next() >> 24) as u8);
            out.push(255);
        }
        out
    }

    /// The same picture as [`noisy_rgba`], packed one pixel per Java `int`.
    fn pack_argb(rgba: &[u8]) -> Vec<i32> {
        rgba.chunks_exact(4)
            .map(|p| {
                ((0xFF << 24) | (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
                    as i32
            })
            .collect()
    }

    fn assert_close(actual: u8, expected: i32) {
        let diff = (actual as i32 - expected).abs();
        assert!(
            diff <= 2,
            "expected ~{expected}, got {actual} (diff {diff})"
        );
    }

    #[test]
    fn red_maps_to_expected_bt601_full_range() {
        // BT.601 full range: Y=76.245, U=84.973, V=255.5 (clamped 255).
        let nv12 = rgba_to_nv12(&solid(2, 2, 255, 0, 0), 2, 2).unwrap();
        assert_eq!(nv12.len(), nv12_len(2, 2).unwrap());
        for &y in &nv12[0..4] {
            assert_close(y, 76);
        }
        assert_close(nv12[4], 85); // U
        assert_close(nv12[5], 255); // V
    }

    #[test]
    fn white_maps_to_expected_bt601_full_range() {
        let nv12 = rgba_to_nv12(&solid(2, 2, 255, 255, 255), 2, 2).unwrap();
        for &y in &nv12[0..4] {
            assert_close(y, 255);
        }
        assert_close(nv12[4], 128);
        assert_close(nv12[5], 128);
    }

    #[test]
    fn black_maps_to_expected_bt601_full_range() {
        let nv12 = rgba_to_nv12(&solid(4, 2, 0, 0, 0), 4, 2).unwrap();
        for &y in &nv12[0..8] {
            assert_close(y, 0);
        }
        // Two chroma pairs for a 4x2 frame.
        assert_eq!(&nv12[8..12], &[128, 128, 128, 128]);
    }

    #[test]
    fn chroma_is_averaged_over_the_2x2_block() {
        // One 2x2 block holding two red and two blue pixels. The average is
        // (127.5, 0, 127.5), which is decidedly not the top-left red pixel.
        let rgba = [
            255, 0, 0, 255, // (0,0) red
            0, 0, 255, 255, // (0,1) blue
            255, 0, 0, 255, // (1,0) red
            0, 0, 255, 255, // (1,1) blue
        ];
        let nv12 = rgba_to_nv12(&rgba, 2, 2).unwrap();
        assert_close(nv12[4], 170); // U of averaged magenta
        assert_close(nv12[5], 181); // V of averaged magenta
    }

    #[test]
    fn rejects_wrong_rgba_len_and_odd_dims() {
        assert!(matches!(
            rgba_to_nv12(&[0u8; 3], 2, 2),
            Err(ExportError::InvalidFrameLen {
                expected: 16,
                got: 3
            })
        ));
        let odd = solid(3, 2, 1, 2, 3);
        assert!(matches!(
            rgba_to_nv12(&odd, 3, 2),
            Err(ExportError::InvalidConfig(_))
        ));
        assert_eq!(nv12_len(3, 2), None);
        assert_eq!(nv12_len(0, 2), None);
    }

    /// The packed-integer entry point is the same arithmetic over the same
    /// channels, so it must agree with the RGBA one byte for byte — that is
    /// what lets the export skip the RGBA repack.
    #[test]
    fn argb_and_rgba_layouts_agree_byte_for_byte() {
        for (width, height) in [(2u32, 2u32), (6, 4), (16, 2), (2, 8), (14, 10)] {
            let rgba = noisy_rgba(width, height);
            let argb = pack_argb(&rgba);
            assert_eq!(
                argb_i32_to_nv12(&argb, width, height).unwrap(),
                rgba_to_nv12(&rgba, width, height).unwrap(),
                "{width}x{height}"
            );
        }
    }

    /// Clamping is part of the contract: pure red pins V at the top of the
    /// range and pure blue pins U, and the integer chroma must clamp exactly
    /// like `f32.clamp` did.
    #[test]
    fn extreme_colors_still_clamp() {
        // (color, U, V): the f32 reference values, with both ends of the range
        // reached — red clamps V, blue clamps U.
        for ((r, g, b), u, v) in [
            ((255u8, 0u8, 0u8), 85u8, 255u8),
            ((0, 0, 255), 255, 107),
            ((0, 255, 0), 44, 21),
            ((255, 255, 0), 1, 149),
        ] {
            let rgba = solid(4, 4, r, g, b);
            let argb = pack_argb(&rgba);
            let nv12 = argb_i32_to_nv12(&argb, 4, 4).unwrap();
            assert_eq!(nv12, rgba_to_nv12(&rgba, 4, 4).unwrap());
            // First chroma pair of the 4x4 frame.
            assert_eq!((nv12[16], nv12[17]), (u, v), "{r},{g},{b}");
        }
    }

    /// The exporter reuses one output buffer for every frame, which is only
    /// safe because the pass writes every byte of it.
    #[test]
    fn into_variants_overwrite_the_whole_buffer() {
        let rgba = noisy_rgba(8, 4);
        let argb = pack_argb(&rgba);
        let expected = rgba_to_nv12(&rgba, 8, 4).unwrap();

        let mut dirty = vec![0xAAu8; nv12_len(8, 4).unwrap()];
        rgba_to_nv12_into(&rgba, 8, 4, &mut dirty).unwrap();
        assert_eq!(dirty, expected);

        let mut dirty = vec![0x55u8; nv12_len(8, 4).unwrap()];
        argb_i32_to_nv12_into(&argb, 8, 4, &mut dirty).unwrap();
        assert_eq!(dirty, expected);
    }

    #[test]
    fn into_variants_check_source_and_output_lengths() {
        let rgba = solid(2, 2, 1, 2, 3);
        // The RGBA error counts bytes, the packed one counts pixels.
        assert!(matches!(
            rgba_to_nv12_into(&rgba, 2, 2, &mut [0u8; 24]),
            Err(ExportError::InvalidFrameLen {
                expected: 6,
                got: 24
            })
        ));
        // Right source, wrong output: the caller must hand over exactly one
        // NV12 frame, otherwise stale bytes could reach the encoder.
        assert!(matches!(
            rgba_to_nv12_into(&rgba, 2, 2, &mut [0u8; 5]),
            Err(ExportError::InvalidFrameLen {
                expected: 6,
                got: 5
            })
        ));
        let argb = pack_argb(&rgba);
        assert!(matches!(
            argb_i32_to_nv12_into(&argb[..3], 2, 2, &mut [0u8; 6]),
            Err(ExportError::InvalidFrameLen {
                expected: 4,
                got: 3
            })
        ));
        // Right pixel count, odd dimensions: rejected before anything is
        // indexed, because a 2x2 block would read past the last row.
        let odd = pack_argb(&solid(3, 2, 1, 2, 3));
        assert!(matches!(
            argb_i32_to_nv12_into(&odd, 3, 2, &mut [0u8; 9]),
            Err(ExportError::InvalidConfig(_))
        ));
        assert!(matches!(
            rgba_to_nv12_into(&rgba, 0, 2, &mut []),
            Err(ExportError::InvalidFrameLen { .. })
        ));
    }

    /// The f32 coefficients the GPU pack shader receives and the integer ones
    /// this module computes with are the same numbers.
    ///
    /// Two copies of a coefficient is two chances to drift, and a drifted matrix
    /// shows up only as a tinted export — nothing fails, nothing logs. The tie is
    /// a test so that editing either side alone breaks it.
    #[test]
    fn the_shader_coefficients_are_the_integer_ones() {
        let scaled = |f: f32| (f * SCALE as f64 as f32).round() as i64;
        for (coeff, integer, what) in [
            (CHROMA_U_COEFFS[0], U_R, "U.R"),
            (CHROMA_U_COEFFS[1], U_G, "U.G"),
            (CHROMA_U_COEFFS[2], U_B, "U.B"),
            (CHROMA_V_COEFFS[0], V_R, "V.R"),
            (CHROMA_V_COEFFS[1], V_G, "V.G"),
            (CHROMA_V_COEFFS[2], V_B, "V.B"),
        ] {
            assert_eq!(scaled(coeff), integer, "{what}: {coeff} is not {integer}");
        }
        // The bias and the block denominator are folded the same way.
        assert_eq!(scaled(CHROMA_BIAS) * 4, BIAS_SCALED);
        assert_eq!(BLOCK_DEN, 4 * SCALE);
        // And the matrix handed to the GPU is exactly the one above.
        assert_eq!(BT601_FULL.luma, LUMA_COEFFS);
        assert_eq!(BT601_FULL.chroma_u, CHROMA_U_COEFFS);
        assert_eq!(BT601_FULL.chroma_v, CHROMA_V_COEFFS);
        assert_eq!(BT601_FULL.bias, CHROMA_BIAS);
        assert_eq!(BT601_FULL.scale, 255.0);
    }

    /// The integer luma formulation this module's docs compare against: the
    /// coefficients in [`SCALE`] units, one divide, half away from zero.
    fn integer_luma(r: u32, g: u32, b: u32) -> u8 {
        let n = 299_000 * i64::from(r) + 587_000 * i64::from(g) + 114_000 * i64::from(b);
        clamp_i(div_round_away(n, SCALE))
    }

    /// Luma is the one place where the f32 pass and an integer pass disagree —
    /// on the exact ties, where the f32 sum lands just below the half and rounds
    /// down. The disagreement is bounded by one code value, and this pins the
    /// bound over the boundary-heavy part of the cube instead of trusting the
    /// prose in the module docs.
    #[test]
    fn the_f32_luma_stays_within_one_code_value_of_the_integer_one() {
        let mut differing = 0usize;
        let mut scanned = 0usize;
        for r in 0u32..=255 {
            for g in 0u32..=255 {
                // The extremes and the rounding boundary, where a tie can land.
                for b in [0u32, 1, 127, 128, 129, 254, 255] {
                    let f = luma(r as f32, g as f32, b as f32);
                    let i = integer_luma(r, g, b);
                    let diff = (f as i32 - i as i32).abs();
                    assert!(diff <= 1, "rgb({r},{g},{b}): f32 {f}, integer {i}");
                    if diff != 0 {
                        differing += 1;
                    }
                    scanned += 1;
                }
            }
        }
        assert!(scanned > 450_000);
        // Rare enough to be invisible, common enough that the bound is not
        // vacuous: the module docs cite 824 of the 16.7M full triples.
        assert!(
            differing * 20 < scanned,
            "{differing} of {scanned} luma samples disagreed by one — more than a rounding tie"
        );
    }

    /// The GPU pack shader's arithmetic, transcribed: the same expressions over
    /// the same byte values, in the same order.
    ///
    /// A GPU may contract a multiply and an add into one fused operation, so its
    /// last bit is not guaranteed to match this evaluation — which is exactly why
    /// the test below bounds the difference rather than demanding equality. What
    /// it does rule out is a *formulation* error: a matrix applied to the wrong
    /// term, a block average that is not an average, a bias added before instead
    /// of after the rounding.
    fn gpu_luma(p: (f32, f32, f32)) -> u8 {
        clamp_u8(LUMA_COEFFS[0] * p.0 + LUMA_COEFFS[1] * p.1 + LUMA_COEFFS[2] * p.2)
    }

    fn gpu_chroma(coeffs: [f32; 3], block: [(f32, f32, f32); 4]) -> u8 {
        let channel = |pick: fn((f32, f32, f32)) -> f32| {
            (pick(block[0]) + pick(block[1]) + pick(block[2]) + pick(block[3])) * 0.25
        };
        let r = channel(|p| p.0);
        let g = channel(|p| p.1);
        let b = channel(|p| p.2);
        clamp_u8(coeffs[0] * r + coeffs[1] * g + coeffs[2] * b + CHROMA_BIAS)
    }

    /// One whole frame through the shader's formulation, as the compute pass
    /// would produce it.
    fn model_nv12(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut out = vec![0u8; nv12_len(width, height).unwrap()];
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32)
        };
        for y in 0..h {
            for x in 0..w {
                out[y * w + x] = gpu_luma(px(x, y));
            }
        }
        for by in 0..h / 2 {
            for bx in 0..w / 2 {
                let block = [
                    px(bx * 2, by * 2),
                    px(bx * 2 + 1, by * 2),
                    px(bx * 2, by * 2 + 1),
                    px(bx * 2 + 1, by * 2 + 1),
                ];
                let uv = w * h + (by * (w / 2) + bx) * 2;
                out[uv] = gpu_chroma(CHROMA_U_COEFFS, block);
                out[uv + 1] = gpu_chroma(CHROMA_V_COEFFS, block);
            }
        }
        out
    }

    /// The GPU formulation reproduces the CPU reference on whole frames, to
    /// within one code value per byte — the bound the device comparison will
    /// check against a real adapter (docs/12 §12.6).
    #[test]
    fn the_gpu_formulation_agrees_with_the_cpu_reference() {
        let mut differing = 0usize;
        let mut total = 0usize;
        let mut worst = 0i32;
        for (w, h) in [(2u32, 2u32), (6, 4), (16, 2), (14, 10), (8, 8)] {
            // Noise, saturated primaries, and the rounding boundary — where a
            // difference of one code value would show up first.
            let frames = [
                noisy_rgba(w, h),
                solid(w, h, 255, 0, 0),
                solid(w, h, 0, 0, 255),
                solid(w, h, 128, 127, 129),
                {
                    let mut f = Vec::with_capacity((w * h * 4) as usize);
                    for i in 0..(w * h) {
                        // 127/128 alternating: the block averages land on .5.
                        let v = if (i + i / w) % 2 == 0 { 127u8 } else { 128 };
                        f.extend_from_slice(&[v, 255 - v, v, 255]);
                    }
                    f
                },
            ];
            for frame in frames {
                let cpu = rgba_to_nv12(&frame, w, h).unwrap();
                let gpu = model_nv12(&frame, w, h);
                assert_eq!(cpu.len(), gpu.len(), "{w}x{h}");
                for (index, (c, g)) in cpu.iter().zip(gpu.iter()).enumerate() {
                    let diff = (*c as i32 - *g as i32).abs();
                    worst = worst.max(diff);
                    if diff != 0 {
                        differing += 1;
                    }
                    total += 1;
                    assert!(
                        diff <= 1,
                        "{w}x{h} byte {index}: CPU reference {c}, GPU formulation {g}"
                    );
                }
            }
        }
        assert!(total > 1000, "the comparison must cover real frames");
        assert!(worst <= 1, "worst difference was {worst}");
        assert!(
            differing * 100 < total,
            "{differing} of {total} bytes differed — a formulation error, not a rounding tie"
        );
    }
}

// SPDX-License-Identifier: Apache-2.0

//! Legacy SVG path: rasterise a document with [`resvg`] when the vector path
//! cannot express it.
//!
//! # Why this exists next to the vector path
//!
//! [`crate::svg`] is the good path: it parses with `usvg` and tessellates real
//! vector geometry, but it *refuses* what the flat-colour engine cannot draw
//! honestly — it records gradients (flattened) and `pattern`, `clipPath`,
//! `mask`, `filter`, group opacity and text in a `skipped` list. When a
//! document needs those, it silently loses content.
//!
//! This module is the deliberate fallback: it **rasterises the whole document**
//! to a PNG-like RGBA8 image. That is the point, not a shortcut — "any SVG
//! still renders" is worth a bitmap, and the vector path stays the default for
//! documents it *can* express. The caller chooses which path to use.
//!
//! # Rasteriser, licence and weight
//!
//! Rasterisation is [`resvg`] `0.48.1` (`Apache-2.0 OR MIT`), which is `usvg`
//! (parse, with its `text` feature) plus `tiny-skia` (raster). It is pure Rust
//! and pulls in no C toolchain, so the workspace's no-`cc`/`cmake` policy holds.
//! It is pinned with exactly `text` + `raster-images` + `svgz`; the lean
//! variant would refuse the very documents this path exists for (text and
//! embedded raster images). `system-fonts`/`memmap-fonts` are **not** enabled:
//! they reach for fontconfig, which Android does not have.
//!
//! Weight is real and measured on a stripped host release build: this path adds
//! about **+3328 KiB** over a build without it, against about **+640 KiB** for
//! the vector path's `usvg` alone. Most of the difference is `tiny-skia`, the
//! text stack and the image decoders, so this path is the expensive one and is
//! only built into the fallback.
//!
//! # Fitting rule
//!
//! Matching [`crate::svg::svg_meshes`] exactly: the **larger** source dimension
//! (from `width`/`height` or the `viewBox`, as `usvg` resolves it) spans
//! precisely `size_px`, and the other spans `size_px * (minor / major)`, rounded
//! **up** to an integer pixel and to at least 1. Two paths that fitted
//! differently would make a switch to the fallback visibly jump, so the rule is
//! copied, not reinvented.
//!
//! # Pixel cap
//!
//! A hostile or accidental huge document must not allocate gigabytes. Output is
//! refused above [`MAX_RASTER_PIXELS`] (16 Mi-pixels ≈ 64 MiB of RGBA8), so a
//! request for `size_px = 8192` on a square document is an `Err`, not an
//! allocation.
//!
//! # Fonts for `<text>`
//!
//! The `text` feature is required, and it needs a font database, so the first
//! raster call lazily loads Android's system fonts from `/system/fonts` (present
//! on every Android device): `Roboto-Regular.ttf` if it is there, otherwise the
//! first usable `*.ttf`. The loaded face is installed as the database's serif
//! and sans-serif family, so a document that names no family — or names one
//! that is not installed — still gets text. This happens **once** (a
//! [`OnceLock`]), not per call.
//!
//! If no font can be loaded, rasterisation still succeeds: the document renders
//! with `<text>` missing, and [`SvgRaster::fonts_available`] is `false` (and
//! [`font_problem`] states why). The module deliberately does not turn that into
//! an `Err`, because the fallback's contract is "render any SVG" — a document
//! whose text is missing is still worth far more than a failed layer, and the
//! caller is told so it can say as much.
//!
//! # Alpha
//!
//! `tiny-skia` keeps **premultiplied** alpha; the engine composites **straight**
//! alpha, so pixels are demultiplied before return ([`tiny_skia::Pixmap::take_demultiplied`]).
//! The background stays transparent: an untouched pixel is `(0, 0, 0, 0)`, never
//! a filled white or black.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use resvg::tiny_skia;
use resvg::usvg;
use usvg::fontdb;

/// Largest output accepted, in pixels: 16 Mi-pixels, i.e. 4096×4096, or about
/// 64 MiB of RGBA8. Above this a raster request is refused.
pub const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;

/// Directory Android always has, and the only place this module looks for
/// fonts. The host test environment is Android-shaped, so the same path works
/// there.
const ANDROID_FONT_DIR: &str = "/system/fonts";

/// Preferred face: the one every Android device ships.
const PREFERRED_FONT: &str = "Roboto-Regular.ttf";

/// One rasterised SVG: RGBA8, row-major, `w * h * 4` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgRaster {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8, top-left origin.
    pub rgba: Vec<u8>,
    /// Whether a font was available when this raster was produced.
    ///
    /// `false` means `<text>` rasterised as nothing (no font could be loaded);
    /// the caller can then say so instead of showing blank text. It does **not**
    /// mean the raster failed — the rest of the document is there.
    pub fonts_available: bool,
}

impl SvgRaster {
    /// Flat `[width:u32 LE][height:u32 LE][rgba...]`, the same encoding
    /// [`crate::texture::TextureImage::to_flat`] uses for the JNI boundary and
    /// for `nativeUploadImage`.
    pub fn to_flat(&self) -> Vec<u8> {
        let mut flat = Vec::with_capacity(8 + self.rgba.len());
        flat.extend_from_slice(&self.width.to_le_bytes());
        flat.extend_from_slice(&self.height.to_le_bytes());
        flat.extend_from_slice(&self.rgba);
        flat
    }
}

/// Rasterise `bytes` into a `size_px`-wide box, preserving aspect.
///
/// `size_px` sizes the **larger** side (see the module docs); the height (or
/// width, for a landscape document) follows the aspect and is rounded up to at
/// least 1. Errors are reasons, never panics.
pub fn rasterize_svg(bytes: &[u8], size_px: u32) -> Result<SvgRaster, String> {
    if size_px == 0 {
        return Err("SVG raster size must be at least 1 pixel".to_string());
    }

    let (options, fonts_available) = build_options();
    let tree = usvg::Tree::from_data(bytes, &options)
        .map_err(|e| format!("SVG parse error: {e}"))?;

    let source = tree.size();
    let (sw, sh) = (source.width(), source.height());
    if !sw.is_finite() || !sh.is_finite() || sw <= 0.0 || sh <= 0.0 {
        return Err(format!("SVG has no usable size ({sw}x{sh})"));
    }

    // Same fitting rule as `svg::svg_meshes`: the larger side spans exactly
    // `size_px`, the smaller follows the aspect. `minor_px <= size_px` because
    // the minor side is never larger than the major one, so the cast is exact.
    let fit = f64::from(size_px) / f64::from(sw.max(sh));
    let minor_px = ((sw.min(sh) as f64) * fit).ceil().max(1.0) as u32;
    let (out_w, out_h) = if sw >= sh {
        (size_px, minor_px)
    } else {
        (minor_px, size_px)
    };

    let pixels = u64::from(out_w) * u64::from(out_h);
    if pixels > MAX_RASTER_PIXELS {
        return Err(format!(
            "SVG raster {out_w}x{out_h} is {pixels} pixels, above the {MAX_RASTER_PIXELS}-pixel cap"
        ));
    }

    let mut pixmap = tiny_skia::Pixmap::new(out_w, out_h)
        .ok_or_else(|| format!("resvg: could not allocate a {out_w}x{out_h} pixmap"))?;
    let transform = tiny_skia::Transform::from_scale(fit as f32, fit as f32);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Demultiply: tiny-skia is premultiplied, the engine is straight alpha.
    // Untouched pixels stay fully transparent, so the background is not filled.
    let rgba = pixmap.take_demultiplied();
    let expected = out_w as usize * out_h as usize * 4;
    if rgba.len() != expected {
        return Err(format!(
            "resvg: produced {} bytes for a {out_w}x{out_h} image, expected {expected}",
            rgba.len()
        ));
    }

    Ok(SvgRaster {
        width: out_w,
        height: out_h,
        rgba,
        fonts_available,
    })
}

/// Whether a font is available to this module, without rasterising anything.
///
/// This triggers the one-time font load on first call, exactly like
/// [`rasterize_svg`] does.
pub fn fonts_available() -> bool {
    matches!(fonts(), Fonts::Loaded { .. })
}

/// Why no font could be loaded, if none could. `None` when a font is available.
pub fn font_problem() -> Option<String> {
    match fonts() {
        Fonts::Loaded { .. } => None,
        Fonts::Missing(reason) => Some(reason.clone()),
    }
}

// ---------------------------------------------------------------------------
// Font database (loaded once, lazily)
// ---------------------------------------------------------------------------

/// The process-wide font database and the family it provides.
enum Fonts {
    /// A usable database plus the family name its first face carries.
    Loaded {
        db: Arc<fontdb::Database>,
        family: String,
    },
    /// Why loading failed; text will be dropped.
    Missing(String),
}

fn fonts() -> &'static Fonts {
    static CELL: OnceLock<Fonts> = OnceLock::new();
    CELL.get_or_init(load_android_fonts)
}

/// Build `usvg::Options` with the shared font database, and whether it has one.
fn build_options() -> (usvg::Options<'static>, bool) {
    let mut options = usvg::Options::default();
    match fonts() {
        Fonts::Loaded { db, family } => {
            options.fontdb = Arc::clone(db);
            options.font_family = family.clone();
            (options, true)
        }
        // No font: usvg resolves text to nothing, everything else still renders.
        Fonts::Missing(_) => (options, false),
    }
}

/// Load Android's system fonts into a fresh database, once.
///
/// `Roboto-Regular.ttf` is tried first, then any other `*.ttf`. `fontdb`'s own
/// file/dir loading is behind its `fs` feature, which `usvg` does not enable, so
/// the bytes are read here and handed to `load_font_data`.
fn load_android_fonts() -> Fonts {
    let dir = Path::new(ANDROID_FONT_DIR);
    let mut candidates: Vec<PathBuf> = Vec::new();
    let preferred = dir.join(PREFERRED_FONT);
    if preferred.is_file() {
        candidates.push(preferred);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_ttf = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ttf"));
            if is_ttf && !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }

    let mut db = fontdb::Database::new();
    for path in &candidates {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        db.load_font_data(data);
        if db.len() > 0 {
            // One face is enough; `Roboto-Regular.ttf` covers Latin, and the
            // fallback selector can only reach faces that are in the database.
            let family = pick_family(&db);
            db.set_serif_family(family.clone());
            db.set_sans_serif_family(family.clone());
            return Fonts::Loaded {
                db: Arc::new(db),
                family,
            };
        }
    }

    Fonts::Missing(format!(
        "no usable TrueType font in {ANDROID_FONT_DIR} ({} candidate files)",
        candidates.len()
    ))
}

/// The family name to advertise, preferring Roboto so `font-family: sans-serif`
/// and the default `Times New Roman` both land on the face we actually loaded.
fn pick_family(db: &fontdb::Database) -> String {
    let mut first = None;
    for face in db.faces() {
        for (name, _language) in &face.families {
            if name.eq_ignore_ascii_case("Roboto") {
                return name.clone();
            }
            if first.is_none() {
                first = Some(name.clone());
            }
        }
    }
    first.unwrap_or_else(|| "sans-serif".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A red square that leaves the corners transparent, so one document proves
    /// both "the fill is the fill" and "the background is transparent".
    const RED_SQUARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect x="25" y="25" width="50" height="50" fill="#ff0000"/></svg>"##;

    fn pixel(raster: &SvgRaster, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * raster.width + x) * 4) as usize;
        raster.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn filled_rect_rasterises_with_expected_size_and_colour() {
        let raster = rasterize_svg(RED_SQUARE.as_bytes(), 100).expect("rect should rasterise");
        assert_eq!((raster.width, raster.height), (100, 100));
        assert_eq!(raster.rgba.len(), 100 * 100 * 4);
        assert_eq!(pixel(&raster, 50, 50), [255, 0, 0, 255]);
    }

    #[test]
    fn background_is_transparent_not_filled() {
        let raster = rasterize_svg(RED_SQUARE.as_bytes(), 100).expect("rect should rasterise");
        // The corner is outside the shape: straight-alpha RGBA8 all zero.
        assert_eq!(pixel(&raster, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn aspect_is_preserved_with_larger_side_at_size_px() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><rect width="200" height="100" fill="#0000ff"/></svg>"##;
        let raster = rasterize_svg(svg.as_bytes(), 300).expect("viewBox doc should rasterise");
        assert_eq!((raster.width, raster.height), (300, 150));
        assert_eq!(raster.rgba.len(), 300 * 150 * 4);
    }

    #[test]
    fn malformed_input_is_an_error_with_a_reason() {
        for bytes in [&b""[..], &b"<svg"[..], &[0xff, 0xfe, 0xfd][..]] {
            let err = rasterize_svg(bytes, 64).expect_err("malformed input must fail");
            assert!(!err.is_empty(), "error reason must not be empty");
        }
    }

    #[test]
    fn zero_size_is_an_error_not_a_panic() {
        assert!(rasterize_svg(RED_SQUARE.as_bytes(), 0).is_err());
    }

    #[test]
    fn output_above_the_pixel_cap_is_an_error() {
        let err = rasterize_svg(RED_SQUARE.as_bytes(), 8192)
            .expect_err("8192x8192 is above the cap");
        assert!(
            err.contains("cap"),
            "reason should name the cap, got: {err}"
        );
    }

    /// The test that justifies the module: a document the **vector** path
    /// records in `skipped` (its clip-path is ignored) still rasterises here,
    /// so the fallback genuinely covers what the good path cannot.
    #[test]
    fn clip_path_document_the_vector_path_refuses_still_rasterises() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <defs><clipPath id="c"><circle cx="50" cy="50" r="40"/></clipPath></defs>
            <rect x="0" y="0" width="100" height="100" fill="#00ff00" clip-path="url(#c)"/>
        </svg>"##;

        let doc = crate::svg::parse_svg(svg.as_bytes()).expect("vector path parses it");
        assert!(
            doc.skipped.iter().any(|s| s.contains("clip-path")),
            "vector path should record the clip-path it cannot draw: {:?}",
            doc.skipped
        );

        let raster = rasterize_svg(svg.as_bytes(), 100).expect("legacy path should rasterise it");
        // Inside the clip: green. Outside (the corner): transparent — the clip
        // took effect, which is exactly what the vector path could not do.
        assert_eq!(pixel(&raster, 50, 50), [0, 255, 0, 255]);
        assert_eq!(pixel(&raster, 0, 0), [0, 0, 0, 0]);
    }

    /// Honest about which branch it took: with a font, `<text>` must leave
    /// visible pixels; without one, the result must say so instead of the test
    /// pretending to assert on glyphs.
    #[test]
    fn text_renders_when_a_font_is_available() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
            <rect width="200" height="100" fill="#ffffff"/>
            <text x="20" y="70" font-family="Roboto" font-size="60" fill="#000000">A</text>
        </svg>"##;
        let raster = rasterize_svg(svg.as_bytes(), 200).expect("text doc should rasterise");

        if fonts_available() {
            // White background, black glyph: a dark opaque pixel proves the
            // glyph was drawn rather than dropped.
            let drew_text = raster
                .rgba
                .chunks_exact(4)
                .any(|p| p[3] == 255 && p[0] < 128 && p[1] < 128 && p[2] < 128);
            assert!(drew_text, "a font is available but <text> left no dark pixels");
        } else {
            eprintln!(
                "svg_legacy: no system font available ({}); asserting the recorded absence",
                font_problem().unwrap_or_default()
            );
            assert!(!raster.fonts_available);
            assert!(font_problem().is_some());
        }
    }
}

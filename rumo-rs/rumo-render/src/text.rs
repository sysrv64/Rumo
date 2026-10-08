// SPDX-License-Identifier: Apache-2.0

//! CPU-only text layout and glyph atlas on top of `cosmic-text`.
//!
//! [`TextEngine`] shapes a string with the system monospace face, rasterizes
//! each glyph once into a dynamically growing RGBA8 [`Atlas`] and hands back
//! [`GlyphQuad`]s (pixel-space rect + normalized UV). Uploading the atlas and
//! drawing the quads is the GPU side's job; nothing here touches wgpu, so the
//! whole module is unit-testable without an adapter.

use std::collections::HashMap;
use std::path::Path;

use cosmic_text::{
    Attrs, Buffer, CacheKey, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, SubpixelBin,
    SwashCache, SwashContent, fontdb,
};
// `FontRef` is read straight from swash, the same 0.2.x cosmic-text uses, to
// ask a face which axes it has.
use swash::FontRef;


use crate::texture::{TextureImage, TexturedMesh, uv_rect};

/// Default atlas page size and growth ceiling.
const ATLAS_WIDTH: u32 = 512;
const ATLAS_HEIGHT: u32 = 512;
const ATLAS_MAX_HEIGHT: u32 = 4096;
/// Transparent gutter around every glyph to stop bilinear bleeding.
const ATLAS_PADDING: u32 = 1;

/// Monospace faces tried when the platform font database comes up empty
/// (Android's `load_system_fonts` is a no-op there) or when no generic
/// monospace family is registered. This is the built-in fallback list; it
/// points at fonts shipped by Android and common Linux distributions.
const FALLBACK_MONO_FACES: &[&str] = &[
    "/system/fonts/RobotoMono-Regular.ttf",
    "/system/fonts/DroidSansMono.ttf",
    "/system/fonts/CutiveMono.ttf",
    "/system/fonts/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
];

/// Preferred family names, in match order, when picking a default face.
const PREFERRED_MONO_FAMILIES: &[&str] = &[
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Roboto Mono",
    "Droid Sans Mono",
    "Cutive Mono",
];

/// Identity of one rasterized glyph inside the atlas.
///
/// `size_bits` are the raw `f32` bits of the font size so the key is `Hash`
/// and exact without float hashing. `dilate` is the extra thickness baked into
/// the bitmap: 0 is the glyph as the face draws it, anything above is the same
/// glyph grown by that many pixels on every side, which is what carries a
/// heavier weight and an outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font_id: fontdb::ID,
    pub glyph_id: u16,
    pub size_bits: u32,
    pub weight: fontdb::Weight,
    pub dilate: u16,
}

/// Pixel rect of a glyph inside an [`Atlas`] page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

struct Shelf {
    y: u32,
    height: u32,
    cursor_x: u32,
}

/// A shelf-packed RGBA8 glyph atlas. Insertion is idempotent per [`GlyphKey`];
/// when a glyph does not fit the page is grown vertically (coordinates of
/// existing glyphs are unchanged) up to the configured ceiling.
///
/// Growing changes the page height, so normalized UVs handed out before a
/// growth are only valid while the page dimensions stay the same: a consumer
/// that uploads the atlas to the GPU must re-read [`Atlas::to_image`] (and
/// re-upload) after any [`TextEngine::layout`] call that may have grown it.
/// The quads from a single `layout` call are always self-consistent because
/// their UVs are resolved after all of that call's insertions.
pub struct Atlas {
    width: u32,
    height: u32,
    max_height: u32,
    padding: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    entries: HashMap<GlyphKey, AtlasRect>,
}

impl Atlas {
    /// New `width`×`height` page that may grow to `max_height`.
    pub fn new(width: u32, height: u32, max_height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let max_height = max_height.max(height);
        Self {
            width,
            height,
            max_height,
            padding: ATLAS_PADDING,
            pixels: vec![0u8; width as usize * height as usize * 4],
            shelves: Vec::new(),
            entries: HashMap::new(),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn glyph_count(&self) -> usize {
        self.entries.len()
    }

    /// Cached rect for `key`, if the glyph was already rasterized.
    pub fn get(&self, key: GlyphKey) -> Option<AtlasRect> {
        self.entries.get(&key).copied()
    }

    /// Copy a `w`×`h` RGBA8 glyph into the page, returning its rect.
    /// Repeated calls with the same key return the existing rect; malformed
    /// input (zero size, wrong buffer length, no room before the ceiling)
    /// returns `None`.
    pub fn insert(
        &mut self,
        key: GlyphKey,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Option<AtlasRect> {
        if w == 0 || h == 0 {
            return None;
        }
        if rgba.len() != w as usize * h as usize * 4 {
            return None;
        }
        if let Some(rect) = self.entries.get(&key) {
            return Some(*rect);
        }
        let pad = self.padding;
        let rw = w.checked_add(2 * pad)?;
        let rh = h.checked_add(2 * pad)?;
        let (px, py) = self.alloc(rw, rh)?;
        let row_bytes = w as usize * 4;
        // `row * width * 4` is the whole point of the loop: without it every row
        // of the glyph lands on the first one, and the glyph becomes a single
        // line of ink stretched over its quad. That is not a subtle wrong
        // picture, it is unreadable text, and it survives a test that only
        // checks the rectangle bookkeeping.
        let x_off = (px + pad) as usize * 4;
        for row in 0..h as usize {
            let src = &rgba[row * row_bytes..(row + 1) * row_bytes];
            let dst_off = ((py + pad) as usize + row) * self.width as usize * 4 + x_off;
            self.pixels[dst_off..dst_off + row_bytes].copy_from_slice(src);
        }
        let rect = AtlasRect {
            x: px + pad,
            y: py + pad,
            w,
            h,
        };
        self.entries.insert(key, rect);
        Some(rect)
    }

    /// Normalized `[u0, v0, u1, v1]` for `rect` on the current page.
    pub fn uv(&self, rect: AtlasRect) -> Option<[f32; 4]> {
        uv_rect([rect.x, rect.y, rect.w, rect.h], self.width, self.height)
    }

    /// Snapshot of the page as an uploadable RGBA8 image.
    pub fn to_image(&self) -> TextureImage {
        TextureImage {
            width: self.width,
            height: self.height,
            rgba: self.pixels.clone(),
        }
    }

    fn alloc(&mut self, rw: u32, rh: u32) -> Option<(u32, u32)> {
        let width = self.width;
        if rw > width {
            return None;
        }
        loop {
            for shelf in self.shelves.iter_mut() {
                if shelf.height >= rh && width - shelf.cursor_x >= rw {
                    let x = shelf.cursor_x;
                    shelf.cursor_x += rw;
                    return Some((x, shelf.y));
                }
            }
            let next_y = match self.shelves.last() {
                Some(last) => last.y + last.height,
                None => 0,
            };
            if self.height - next_y >= rh {
                self.shelves.push(Shelf {
                    y: next_y,
                    height: rh,
                    cursor_x: rw,
                });
                return Some((0, next_y));
            }
            if !self.grow(rh) {
                return None;
            }
        }
    }

    fn grow(&mut self, needed_h: u32) -> bool {
        if self.height >= self.max_height {
            return false;
        }
        let target = self
            .height
            .saturating_mul(2)
            .max(self.height.saturating_add(needed_h))
            .min(self.max_height);
        if target <= self.height {
            return false;
        }
        self.pixels
            .resize(self.width as usize * target as usize * 4, 0);
        self.height = target;
        true
    }
}

/// One positioned glyph in pixel space plus where its pixels live in the page.
///
/// The atlas UV is *not* stored: the page grows taller as glyphs are added, and
/// a normalized coordinate frozen at layout time would address the wrong texels
/// the moment it does. Callers resolve [`GlyphQuad::uv`] against the page they
/// are about to sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Atlas-space rect of this glyph, in page pixels.
    pub rect: AtlasRect,
}

impl GlyphQuad {
    /// Normalized `[u0, v0, u1, v1]` on a `page_w`×`page_h` page. `None` for a
    /// degenerate page, which a caller must treat as "not drawable".
    pub fn uv(&self, page_w: u32, page_h: u32) -> Option<[f32; 4]> {
        uv_rect(
            [self.rect.x, self.rect.y, self.rect.w, self.rect.h],
            page_w,
            page_h,
        )
    }
}

/// Result of shaping a string: drawable quads and their bounding box.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub quads: Vec<GlyphQuad>,
    /// Bounds of the *text*, in pixels: the same for the glyphs and for their
    /// outline, so a caller that centres a layer by these numbers does not
    /// shift the outline against the fill by the outline's own thickness.
    pub width: f32,
    pub height: f32,
}

impl TextLayout {
    /// Build a [`TexturedMesh`] in the same pixel space as the quads, against
    /// the atlas page it will be sampled from.
    pub fn to_mesh(&self, page_w: u32, page_h: u32) -> TexturedMesh {
        let mut mesh = TexturedMesh::new();
        for q in &self.quads {
            let Some(uv) = q.uv(page_w, page_h) else {
                continue;
            };
            mesh.push_quad(q.x, q.y, q.w, q.h, uv);
        }
        mesh
    }
}

/// What one text layer asks of the shaper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Font size in pixels.
    pub size_px: f32,
    /// Weight asked of the font database, in `fontdb::Weight` units
    /// (400 regular, 700 bold). A family that has the face uses it.
    pub weight: u16,
    /// Outline thickness in pixels; 0 draws the glyph itself.
    pub stroke_px: u32,
}

impl TextStyle {
    /// The plain style: regular weight, no outline.
    pub fn new(size_px: f32) -> Self {
        Self {
            size_px,
            weight: 400,
            stroke_px: 0,
        }
    }

    pub fn with_weight(mut self, weight: u16) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_stroke(mut self, stroke_px: u32) -> Self {
        self.stroke_px = stroke_px;
        self
    }

    /// The weight to hand the font database, clamped to the range real faces
    /// live in so a stray number cannot make the query miss every face.
    fn face_weight(&self) -> fontdb::Weight {
        // 1..1000, not the usual 100..900: a variable font declares its own
        // range (Google Sans Flex runs 1..1000) and clamping to the nine named
        // steps would make the axis unreachable at both ends.
        fontdb::Weight(self.weight.clamp(1, 1000))
    }
}

struct PlacedGlyph {
    rect: AtlasRect,
    left: i32,
    top: i32,
    /// Thickness actually baked into this bitmap: 0 for a colour glyph, which
    /// is drawn from its own colours and is never grown.
    grow: u32,
}

struct PendingQuad {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rect: AtlasRect,
}

/// Shaping + rasterization engine. Owns the font system, the swash scale
/// cache and the atlas; reuse one instance so glyphs stay cached.
pub struct TextEngine {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: Atlas,
    family: String,
    /// Placement of every rasterized glyph, with the thickness its bitmap got.
    placements: HashMap<GlyphKey, (i32, i32, u32)>,
    /// Per-face answer to "does this face have an `fvar` table", cached because
    /// the question is asked per glyph and answered from the font data.
    variable_faces: HashMap<fontdb::ID, bool>,
}

impl TextEngine {
    /// Default engine (512×512 atlas, system monospace).
    pub fn new() -> Self {
        Self::with_atlas_size(ATLAS_WIDTH, ATLAS_HEIGHT, ATLAS_MAX_HEIGHT)
    }

    /// Engine with a custom atlas page.
    pub fn with_atlas_size(width: u32, height: u32, max_height: u32) -> Self {
        let mut font_system = FontSystem::new();
        load_fallback_fonts(font_system.db_mut());
        let family = pick_family(font_system.db()).unwrap_or_default();
        Self {
            font_system,
            swash: SwashCache::new(),
            atlas: Atlas::new(width, height, max_height),
            family,
            placements: HashMap::new(),
            variable_faces: HashMap::new(),
        }
    }

    /// Whether any usable face was found (false → [`Self::layout`] returns
    /// `None`).
    pub fn has_fonts(&self) -> bool {
        !self.family.is_empty()
    }

    /// Family name used for shaping.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Register a face from raw file bytes (TTF/OTF/TTC) and return the family
    /// name the font database read out of it.
    ///
    /// The name comes from the face's own `name` table rather than from the
    /// caller: the shop hands us whatever it downloaded, and only the database
    /// knows what the file calls itself. `None` for empty or unparsable bytes.
    ///
    /// The glyph cache keys on `font_id`, so a face registered here coexists
    /// with the built-in families in the same atlas — nothing is invalidated.
    pub fn load_font(&mut self, bytes: Vec<u8>) -> Option<String> {
        if bytes.is_empty() {
            return None;
        }
        let ids = self
            .font_system
            .db_mut()
            .load_font_source(fontdb::Source::Binary(std::sync::Arc::new(bytes)));
        let id = *ids.first()?;
        let name = self
            .font_system
            .db()
            .face(id)?
            .families
            .first()
            .map(|(name, _)| name.clone())?;
        if name.is_empty() { None } else { Some(name) }
    }

    /// Whether the database holds a face under `name`.
    ///
    /// The shop uses this to tell "the download registered" from "the layout
    /// silently fell back to the default face" — the latter would render a
    /// preview that is not the font the user is looking at.
    pub fn has_family(&self, name: &str) -> bool {
        !name.is_empty()
            && self
                .font_system
                .db()
                .faces()
                .any(|face| face.families.iter().any(|(family, _)| family == name))
    }

    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Current atlas page as an RGBA8 image, ready for upload.
    pub fn atlas_image(&self) -> TextureImage {
        self.atlas.to_image()
    }

    /// Shape `text` at `size_px` with the plain style.
    pub fn layout(&mut self, text: &str, size_px: f32) -> Option<TextLayout> {
        self.layout_styled(text, &TextStyle::new(size_px))
    }

    /// Shape `text` in `style` and rasterize every needed glyph into the atlas.
    ///
    /// With `stroke_px > 0` the quads are the glyph's *outline* — the same
    /// glyph grown by that many pixels — and the caller draws them behind the
    /// plain glyphs to get a contour. Returns `None` for empty/degenerate input
    /// or when no font is available. Whitespace glyphs contribute no quad.
    pub fn layout_styled(&mut self, text: &str, style: &TextStyle) -> Option<TextLayout> {
        self.layout_styled_family(text, style, None)
    }

    /// [`Self::layout_styled`], shaped in `family` instead of the engine's
    /// default one.
    ///
    /// `family` is a name from [`Self::load_font`] or from the platform
    /// database; `None` (and an empty name) keeps the default. A name the
    /// database does not know makes the shaper fall back to the default face —
    /// callers that must not show the wrong font check [`Self::has_family`]
    /// first.
    pub fn layout_styled_family(
        &mut self,
        text: &str,
        style: &TextStyle,
        family: Option<&str>,
    ) -> Option<TextLayout> {
        let size_px = style.size_px;
        if text.is_empty() || !size_px.is_finite() || size_px <= 0.0 || !self.has_fonts() {
            return None;
        }
        let metrics = Metrics::new(size_px, size_px * 1.2);
        let family = family
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.family.clone());
        let asked = style.face_weight();
        let attrs = Attrs::new().family(Family::Name(&family)).weight(asked);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut pending: Vec<PendingQuad> = Vec::new();
        let mut max_x = 0.0f32;
        let mut max_y = 0.0f32;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                // The face the database actually resolved decides how much of
                // the requested weight is still missing, and the outline is
                // thickness on top of that.
                let resolved = self.face_weight_of(glyph.font_id);
                let variable = self.face_is_variable(glyph.font_id);
                let want_grow =
                    synthetic_grow_px(size_px, asked, resolved, variable) + style.stroke_px;
                let key = GlyphKey {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                    size_bits: glyph.font_size.to_bits(),
                    weight: glyph.font_weight,
                    dilate: want_grow.min(u16::MAX as u32) as u16,
                };
                let Some(placed) = self.ensure_glyph(key, want_grow) else {
                    continue;
                };
                let physical = glyph.physical((0.0, run.line_y), 1.0);
                let grow = placed.grow as f32;
                // The bitmap was grown on every side, so the quad moves up and
                // left by the same amount: the glyph stays exactly where it was.
                let x = physical.x as f32 + placed.left as f32 - grow;
                let y = physical.y as f32 - placed.top as f32 - grow;
                let w = placed.rect.w as f32;
                let h = placed.rect.h as f32;
                let ink_x = x + grow;
                let ink_y = y + grow;
                max_x = max_x.max(ink_x + (w - 2.0 * grow));
                max_y = max_y.max(ink_y + (h - 2.0 * grow));
                pending.push(PendingQuad {
                    x,
                    y,
                    w,
                    h,
                    rect: placed.rect,
                });
            }
            max_y = max_y.max(run.line_y + run.line_height);
        }

        let quads = pending
            .into_iter()
            .map(|p| GlyphQuad {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
                rect: p.rect,
            })
            .collect();
        Some(TextLayout {
            quads,
            width: max_x,
            height: max_y,
        })
    }

    /// Weight of the face `id`, as the font database has it.
    /// Whether the face carries an `fvar` table, i.e. can be varied.
    ///
    /// Asked of the font data rather than of the database: `fontdb` reports a
    /// variable face at its default instance and nothing in `FaceInfo` says the
    /// axis exists. Cached per face — the answer cannot change while the data is
    /// loaded, and this sits on the per-glyph path.
    fn face_is_variable(&mut self, id: fontdb::ID) -> bool {
        if let Some(known) = self.variable_faces.get(&id) {
            return *known;
        }
        let variable = self
            .font_system
            .db()
            .with_face_data(id, |data, index| {
                FontRef::from_index(data, index as usize)
                    .map(|face| face.variations().next().is_some())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        self.variable_faces.insert(id, variable);
        variable
    }

    fn face_weight_of(&self, id: fontdb::ID) -> fontdb::Weight {
        self.font_system
            .db()
            .face(id)
            .map(|face| face.weight)
            .unwrap_or(fontdb::Weight::NORMAL)
    }

    /// Rasterize `key` into the atlas if absent, returning its placement.
    fn ensure_glyph(&mut self, key: GlyphKey, grow: u32) -> Option<PlacedGlyph> {
        if let Some(rect) = self.atlas.get(key) {
            let (left, top, used) = self.placements.get(&key).copied().unwrap_or((0, 0, 0));
            return Some(PlacedGlyph {
                rect,
                left,
                top,
                grow: used,
            });
        }
        let cache_key = CacheKey {
            font_id: key.font_id,
            glyph_id: key.glyph_id,
            font_size_bits: key.size_bits,
            x_bin: SubpixelBin::Zero,
            y_bin: SubpixelBin::Zero,
            font_weight: key.weight,
            flags: CacheKeyFlags::empty(),
        };
        let image = self
            .swash
            .get_image_uncached(&mut self.font_system, cache_key)?;
        let (rgba, w, h, left, top, used) = to_rgba8(image, grow);
        let rect = self.atlas.insert(key, w, h, &rgba)?;
        self.placements.insert(key, (left, top, used));
        Some(PlacedGlyph {
            rect,
            left,
            top,
            grow: used,
        })
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Load the built-in monospace candidates into `db` when the platform
/// database is empty (Android) or the generic family is missing.
fn load_fallback_fonts(db: &mut fontdb::Database) {
    if db.is_empty() {
        db.load_fonts_dir("/system/fonts");
    }
    if db.len() == 0 || pick_family(db).is_none() {
        for path in FALLBACK_MONO_FACES {
            if Path::new(path).is_file() {
                let _ = db.load_font_file(path);
            }
        }
    }
}

/// Pick the family name used for default shaping, preferring known
/// monospace faces and falling back to the first monospaced face, then to
/// any face at all.
fn pick_family(db: &fontdb::Database) -> Option<String> {
    for want in PREFERRED_MONO_FAMILIES {
        for face in db.faces() {
            if let Some((name, _)) = face
                .families
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(want))
            {
                return Some(name.clone());
            }
        }
    }
    db.faces()
        .find(|face| face.monospaced)
        .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        .or_else(|| {
            db.faces()
                .next()
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        })
}

/// How much to thicken a glyph when the family has no face as heavy as asked.
///
/// A monospace family on Android ships one weight, so a bold request would
/// otherwise be a control that does nothing on the device it is meant for. The
/// gap is covered by growing the bitmap, which is what a synthetic bold is; a
/// family that *does* have the face resolves to the requested weight, this
/// returns zero, and the real face is never thickened on top of itself.
fn synthetic_grow_px(
    size_px: f32,
    asked: fontdb::Weight,
    resolved: fontdb::Weight,
    variable: bool,
) -> u32 {
    // A variable face reaches the requested weight by its own axis, so there is
    // nothing missing to make up. Without this the database reports the face's
    // *default* instance (400 for most), the gap to 700 looks missing, and a real
    // 700 outline gets thickened on top of itself -- the reason a variable font
    // looked heavier than the weight asked for.
    if variable {
        return 0;
    }
    let missing = asked.0.saturating_sub(resolved.0) as f32;
    if missing <= 0.0 || !size_px.is_finite() || size_px <= 0.0 {
        return 0;
    }
    // 0.08 px of growth per 1000 weight units per pixel of size: a 300-unit gap
    // at 120 px lands on 3 px, which is where a synthetic bold usually sits.
    (missing / 1000.0 * size_px * 0.08).round().clamp(0.0, 8.0) as u32
}

/// Grow a glyph mask by `radius` pixels on every side, keeping it centred.
///
/// The mask carries the glyph's coverage in its alpha channel, so growing it is
/// a max filter over the coverage: every output pixel takes the strongest
/// coverage within `radius`. That is what turns a glyph into a heavier glyph and
/// what turns it into its own outline, and it is the same operation for both.
///
/// The radius is reached through a distance transform rather than by scanning
/// the window at every pixel. Scanning costs about `π·r²` steps per pixel, and
/// the outline goes up to twenty pixels: one glyph would then take tens of
/// millions of steps, which is seconds of work on the thread that lays the text
/// out. The transform is the usual two-pass chamfer (3 for an orthogonal step, 4
/// for a diagonal), so it is linear in the pixels at any radius and rounds the
/// corners the way a disc does; its error against the true Euclidean distance is
/// under 6%, a tenth of a pixel at the radii in use.
fn dilate_mask(rgba: &[u8], w: u32, h: u32, radius: u32) -> (Vec<u8>, u32, u32) {
    if radius == 0 || w == 0 || h == 0 {
        return (rgba.to_vec(), w, h);
    }
    /// Chamfer weights, in thirds of a pixel.
    const ORTHO: i32 = 3;
    const DIAG: i32 = 4;
    /// Stands in for "no ink anywhere near"; halved by the saturating adds.
    const FAR: i32 = i32::MAX / 4;

    let r = radius as i32;
    let ow = w + 2 * radius;
    let oh = h + 2 * radius;
    let (src_w, src_h) = (w as i32, h as i32);
    let width = ow as usize;
    let index = |x: i32, y: i32| (y as usize) * width + x as usize;
    // Coverage of the source mask at a grown-mask coordinate, 0 outside it.
    let source_alpha = |x: i32, y: i32| -> u8 {
        let (sx, sy) = (x - r, y - r);
        if sx < 0 || sy < 0 || sx >= src_w || sy >= src_h {
            0
        } else {
            rgba[((sy as usize) * w as usize + sx as usize) * 4 + 3]
        }
    };

    let mut dist = vec![FAR; width * oh as usize];
    for y in 0..oh as i32 {
        for x in 0..ow as i32 {
            // Half coverage is the boundary of the glyph, so that is where the
            // distance is measured from: a faint edge pixel would otherwise pull
            // the whole contour outwards.
            if source_alpha(x, y) >= 128 {
                dist[index(x, y)] = 0;
            }
        }
    }
    for y in 0..oh as i32 {
        for x in 0..ow as i32 {
            let mut d = dist[index(x, y)];
            if x > 0 {
                d = d.min(dist[index(x - 1, y)].saturating_add(ORTHO));
            }
            if y > 0 {
                d = d.min(dist[index(x, y - 1)].saturating_add(ORTHO));
                if x > 0 {
                    d = d.min(dist[index(x - 1, y - 1)].saturating_add(DIAG));
                }
                if x + 1 < ow as i32 {
                    d = d.min(dist[index(x + 1, y - 1)].saturating_add(DIAG));
                }
            }
            dist[index(x, y)] = d;
        }
    }
    for y in (0..oh as i32).rev() {
        for x in (0..ow as i32).rev() {
            let mut d = dist[index(x, y)];
            if x + 1 < ow as i32 {
                d = d.min(dist[index(x + 1, y)].saturating_add(ORTHO));
            }
            if y + 1 < oh as i32 {
                d = d.min(dist[index(x, y + 1)].saturating_add(ORTHO));
                if x + 1 < ow as i32 {
                    d = d.min(dist[index(x + 1, y + 1)].saturating_add(DIAG));
                }
                if x > 0 {
                    d = d.min(dist[index(x - 1, y + 1)].saturating_add(DIAG));
                }
            }
            dist[index(x, y)] = d;
        }
    }

    // The edge sits exactly at `radius`: a pixel on it gets half coverage, one a
    // pixel inside gets all of it, one a pixel outside gets none. That is what
    // makes "grow by r" mean the edge moves out by r, and it keeps the grown edge
    // as smooth as the glyph's own.
    let reach = radius as f32 * ORTHO as f32;
    let mut out = vec![0u8; width * oh as usize * 4];
    for y in 0..oh as i32 {
        for x in 0..ow as i32 {
            let d = dist[index(x, y)] as f32;
            let grown = ((reach + ORTHO as f32 * 0.5 - d) / ORTHO as f32 * 255.0)
                .clamp(0.0, 255.0) as u8;
            // `max`, not `replace`: the glyph's own antialiased edge is inside
            // the grown one and must survive it.
            let alpha = source_alpha(x, y).max(grown);
            let o = index(x, y) * 4;
            out[o] = 255;
            out[o + 1] = 255;
            out[o + 2] = 255;
            out[o + 3] = alpha;
        }
    }
    (out, ow, oh)
}

/// Convert a swash glyph image to straight-alpha RGBA8, grown by `grow` pixels.
///
/// Returns the thickness actually applied, which is 0 for a colour glyph: an
/// emoji carries its own colours, so it is drawn as it is and the caller must
/// place it without the offset a grown bitmap would need.
fn to_rgba8(image: cosmic_text::SwashImage, grow: u32) -> (Vec<u8>, u32, u32, i32, i32, u32) {
    let w = image.placement.width;
    let h = image.placement.height;
    let left = image.placement.left;
    let top = image.placement.top;
    match image.content {
        SwashContent::Mask => {
            let mut mask = Vec::with_capacity(image.data.len() * 4);
            for alpha in image.data.iter() {
                mask.extend_from_slice(&[255, 255, 255, *alpha]);
            }
            let (mask, w, h) = dilate_mask(&mask, w, h, grow);
            (mask, w, h, left, top, grow)
        }
        // Colour and subpixel bitmaps are already 4 bytes per pixel, and their
        // colours are their own: neither is grown.
        SwashContent::Color | SwashContent::SubpixelMask => (image.data, w, h, left, top, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(glyph_id: u16) -> GlyphKey {
        GlyphKey {
            font_id: fontdb::ID::dummy(),
            glyph_id,
            size_bits: 24.0f32.to_bits(),
            weight: fontdb::Weight::NORMAL,
            dilate: 0,
        }
    }

    fn solid(w: u32, h: u32, value: u8) -> Vec<u8> {
        vec![value; w as usize * h as usize * 4]
    }

    #[test]
    fn atlas_hit_miss_and_uv() {
        let mut atlas = Atlas::new(64, 64, 256);
        let a = key(1);
        let b = key(2);
        assert!(atlas.get(a).is_none(), "empty atlas must miss");
        let rect = atlas.insert(a, 10, 8, &solid(10, 8, 200)).expect("insert");
        assert_eq!(atlas.get(a), Some(rect));
        assert!(atlas.get(b).is_none(), "unknown glyph must miss");
        assert_eq!(atlas.glyph_count(), 1);

        let uv = atlas.uv(rect).expect("uv");
        for v in uv {
            assert!((0.0..=1.0).contains(&v), "uv out of range: {v}");
        }
        // Re-inserting the same key is idempotent.
        assert_eq!(atlas.insert(a, 10, 8, &solid(10, 8, 9)), Some(rect));
        assert_eq!(atlas.glyph_count(), 1);
    }

    #[test]
    fn atlas_rejects_malformed_input() {
        let mut atlas = Atlas::new(32, 32, 64);
        assert!(atlas.insert(key(1), 0, 5, &[]).is_none());
        assert!(atlas.insert(key(2), 5, 5, &[0u8; 10]).is_none());
        assert!(atlas.insert(key(3), 200, 5, &solid(200, 5, 0)).is_none());
    }

    #[test]
    fn atlas_grows_and_preserves_entries() {
        let mut atlas = Atlas::new(32, 32, 256);
        let first = atlas.insert(key(1), 8, 8, &solid(8, 8, 1)).expect("first");
        let start_height = atlas.height();
        let big = atlas.insert(key(2), 8, 48, &solid(8, 48, 2)).expect("grow");
        assert!(atlas.height() > start_height, "atlas must have grown");
        assert_eq!(atlas.get(key(1)), Some(first), "old rect must survive");
        assert_eq!(atlas.get(key(2)), Some(big));
        for v in atlas.uv(first).expect("uv") {
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn atlas_image_is_valid_rgba8() {
        let mut atlas = Atlas::new(16, 16, 64);
        atlas.insert(key(1), 4, 4, &solid(4, 4, 255)).unwrap();
        let img = atlas.to_image();
        assert_eq!(img.width, 16);
        assert_eq!(img.height, 16);
        assert_eq!(img.rgba.len(), 16 * 16 * 4);
    }

    #[test]
    fn layout_empty_or_bad_size_none() {
        let mut engine = TextEngine::new();
        assert!(engine.layout("", 16.0).is_none());
        assert!(engine.layout("hi", 0.0).is_none());
        assert!(engine.layout("hi", f32::NAN).is_none());
    }

    /// The bug that made text render as noise: UVs used to be frozen when the
    /// text was laid out, so the moment a later glyph grew the page, every
    /// earlier layout pointed at the wrong texels. The quad now carries the
    /// atlas rect and the UV is derived from the page it is sampled on, so the
    /// same glyph keeps addressing the same pixels however much the page grows.
    #[test]
    fn uv_survives_atlas_growth() {
        let mut engine = TextEngine::with_atlas_size(128, 128, 4096);
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let first = engine.layout("A", 48.0).expect("first");
        let q1 = first.quads[0];
        let before = (engine.atlas().width(), engine.atlas().height());
        // A pile of distinct glyphs forces the page to grow.
        let _ = engine
            .layout(
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                48.0,
            )
            .expect("pile");
        let after = (engine.atlas().width(), engine.atlas().height());
        assert!(
            after.1 > before.1,
            "the pile must have grown the page: {before:?} -> {after:?}"
        );

        // The stale quad, resolved against the page as it is now, must address
        // the same atlas rect as a freshly laid out one.
        let again = engine.layout("A", 48.0).expect("again");
        let q2 = again.quads[0];
        assert_eq!(q1.rect, q2.rect, "the same glyph has one atlas rect");
        assert_eq!(
            q1.uv(after.0, after.1),
            q2.uv(after.0, after.1),
            "the same glyph must keep its UV after the page grew"
        );
        // And the stale UV is what the old code would have used, which is the
        // wrong one: this asserts the test can tell them apart.
        assert_ne!(
            q1.uv(before.0, before.1),
            q1.uv(after.0, after.1),
            "the page really did change under the layout"
        );
    }

    #[test]
    fn stroke_layout_covers_the_plain_one_and_shares_its_bounds() {
        let mut engine = TextEngine::with_atlas_size(512, 512, 4096);
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let plain = engine.layout("Ag", 64.0).expect("plain");
        let outline = engine
            .layout_styled("Ag", &TextStyle::new(64.0).with_stroke(4))
            .expect("outline");
        assert_eq!(plain.quads.len(), outline.quads.len());
        assert_eq!(
            (plain.width, plain.height),
            (outline.width, outline.height),
            "the outline must report the text's own box, or centring would shift it"
        );
        for (fill, ring) in plain.quads.iter().zip(outline.quads.iter()) {
            assert_eq!(fill.rect.w + 8, ring.rect.w, "outline grows 2×radius");
            assert_eq!(fill.rect.h + 8, ring.rect.h);
            assert_eq!(ring.x, fill.x - 4.0, "and moves up/left by the radius");
            assert_eq!(ring.y, fill.y - 4.0);
            assert_eq!(ring.w, fill.w + 8.0);
        }
    }

    #[test]
    #[test]
    fn a_variable_face_is_never_thickened_synthetically() {
        // The gap that a static face cannot cover is made up by growing the
        // bitmap; a variable face covers it with its own axis, so growing on top
        // would draw a weight nobody asked for.
        let asked = fontdb::Weight(700);
        let reported = fontdb::Weight(400);
        assert!(synthetic_grow_px(120.0, asked, reported, false) > 0);
        assert_eq!(synthetic_grow_px(120.0, asked, reported, true), 0);
        assert_eq!(synthetic_grow_px(120.0, asked, asked, true), 0);
    }

    #[test]
    fn heavier_weight_is_never_lighter_than_the_glyph() {
        let mut engine = TextEngine::with_atlas_size(512, 512, 4096);
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let regular = engine.layout("H", 64.0).expect("regular");
        let bold = engine
            .layout_styled("H", &TextStyle::new(64.0).with_weight(700))
            .expect("bold");
        let q = |l: &TextLayout| (l.quads[0].w, l.quads[0].h);
        let (rw, rh) = q(&regular);
        let (bw, bh) = q(&bold);
        assert!(
            bw >= rw && bh >= rh,
            "a bold request must not thin the glyph: {rw}x{rh} -> {bw}x{bh}"
        );
    }

    #[test]
    fn insert_keeps_every_row_of_the_glyph() {
        // A glyph is a picture, so the atlas has to hold all of it. The bug this
        // guards is a destination offset that does not advance per row: every
        // row of the glyph then lands on the first one and the glyph becomes a
        // line of ink stretched across its quad — unreadable text that a test
        // checking only the rect bookkeeping passes happily.
        let mut atlas = Atlas::new(64, 64, 256);
        let (w, h) = (8u32, 6u32);
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for row in 0..h as usize {
            for col in 0..w as usize {
                let o = (row * w as usize + col) * 4;
                rgba[o] = 255;
                rgba[o + 1] = 255;
                rgba[o + 2] = 255;
                // A distinct alpha per row, so a row that is missing or
                // duplicated is visible by value and not just by "is it inked".
                rgba[o + 3] = (row as u8 + 1) * 10;
            }
        }
        let rect = atlas
            .insert(key(1), w, h, &rgba)
            .expect("insert");
        let image = atlas.to_image();
        for row in 0..h as usize {
            for col in 0..w as usize {
                let x = (rect.x as usize + col) * 4;
                let y = (rect.y as usize + row) * atlas.width as usize * 4;
                let o = x + y;
                let got = image.rgba[o + 3];
                assert_eq!(
                    got,
                    (row as u8 + 1) * 10,
                    "row {row}, col {col} did not survive the insert"
                );
            }
        }
    }

    #[test]
    fn dilate_mask_grows_the_ink_and_keeps_the_centre() {
        // 3×3 mask with the centre filled, grown by one pixel: the ink covers
        // the plus around the centre of a 5×5 bitmap, so the original pixel
        // stays at (2,2) and the edge lands exactly one pixel away from it.
        let mut src = vec![0u8; 3 * 3 * 4];
        let centre = (1 * 3 + 1) * 4;
        src[centre] = 255;
        src[centre + 1] = 255;
        src[centre + 2] = 255;
        src[centre + 3] = 200;
        let (out, w, h) = dilate_mask(&src, 3, 3, 1);
        assert_eq!((w, h), (5, 5));
        let alpha = |x: usize, y: usize| out[(y * 5 + x) * 4 + 3];
        assert_eq!(alpha(2, 2), 255, "the glyph's own pixel is solid ink");
        // The orthogonal neighbours sit on the boundary, so they get half
        // coverage — the edge of the grown mask is at exactly the radius.
        for (x, y) in [(1, 2), (3, 2), (2, 1), (2, 3)] {
            let a = alpha(x, y);
            assert!((110..=145).contains(&a), "edge coverage at ({x},{y}) was {a}");
        }
        // A disc of radius 1 does not reach the diagonals; a square would.
        assert!(alpha(1, 1) < 60, "the diagonal is outside a radius-1 disc");
        assert_eq!(alpha(0, 0), 0, "and the corners are never reached");
        assert_eq!(alpha(4, 4), 0);
        for i in 0..25 {
            assert_eq!(out[i * 4], 255, "grown masks are white, the tint colours them");
        }
        // At radius 2 the diagonals are inside the disc, which is what makes the
        // contour round rather than square.
        let (out2, w2, h2) = dilate_mask(&src, 3, 3, 2);
        assert_eq!((w2, h2), (7, 7));
        let alpha2 = |x: usize, y: usize| out2[(y * 7 + x) * 4 + 3];
        assert_eq!(alpha2(2, 2), 255, "the centre is still the centre");
        assert_eq!(alpha2(3, 3), 255, "the diagonal at radius 2 is solidly inside");
        assert_eq!(alpha2(0, 0), 0, "the far corner is still outside it");
    }

    #[test]
    fn dilate_mask_keeps_a_faint_edge_faint() {
        // A soft edge pixel is not "ink" for the distance transform, so the grown
        // mask around it is measured from the solid part. Far enough from any
        // solid ink it keeps its own coverage instead of being flattened into a
        // blob — which is what the glyph's antialiasing is.
        let n = 8usize;
        let mut src = vec![0u8; n * n * 4];
        let solid = (1 * n + 1) * 4;
        src[solid + 3] = 255;
        let faint = (5 * n + 5) * 4;
        src[faint + 3] = 40;
        let (out, w, h) = dilate_mask(&src, n as u32, n as u32, 2);
        assert_eq!((w, h), (12, 12));
        let alpha = |x: usize, y: usize| out[(y * 12 + x) * 4 + 3];
        assert_eq!(alpha(3, 3), 255, "the solid pixel stays solid");
        assert_eq!(alpha(7, 7), 40, "and the faint one keeps its own coverage");
        // Just next to the solid pixel, though, the growth covers it: it is
        // inside the grown region, and there the contour is what shows.
        let near = (1 * n + 2) * 4;
        src[near + 3] = 40;
        let (out2, _, _) = dilate_mask(&src, n as u32, n as u32, 2);
        let alpha2 = |x: usize, y: usize| out2[(y * 12 + x) * 4 + 3];
        assert_eq!(alpha2(4, 3), 255, "one pixel away is inside a two-pixel growth");
    }

    #[test]
    fn dilate_mask_is_linear_in_the_radius() {
        // The regression this guards: the first implementation scanned the whole
        // window per pixel, which is quadratic in the radius, and the outline
        // goes to twenty pixels. A generous ceiling still catches a return to it.
        let (w, h) = (64u32, 64u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            src[i * 4 + 3] = if i % 97 == 0 { 255 } else { 0 };
        }
        let started = std::time::Instant::now();
        for radius in [1u32, 8, 20] {
            let (_, gw, gh) = dilate_mask(&src, w, h, radius);
            assert_eq!((gw, gh), (w + 2 * radius, h + 2 * radius));
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "growing a 64×64 mask at radii 1, 8 and 20 took {elapsed:?}"
        );
    }

    #[test]
    fn layout_produces_non_empty_quads_with_valid_uv() {
        let mut engine = TextEngine::new();
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let layout = engine.layout("Hello", 24.0).expect("layout");
        assert!(!layout.quads.is_empty(), "Hello must produce glyph quads");
        assert!(layout.width > 0.0 && layout.height > 0.0);
        let (page_w, page_h) = (engine.atlas().width(), engine.atlas().height());
        for q in &layout.quads {
            assert!(q.w > 0.0 && q.h > 0.0);
            let uv = q.uv(page_w, page_h).expect("uv");
            for v in uv {
                assert!((0.0..=1.0).contains(&v), "uv out of range: {v}");
            }
            assert!(uv[2] > uv[0] && uv[3] > uv[1]);
        }
        let mesh = layout.to_mesh(page_w, page_h);
        assert_eq!(mesh.vertices.len(), layout.quads.len() * 4);
        assert_eq!(mesh.indices.len(), layout.quads.len() * 6);
    }

    // -----------------------------------------------------------------------
    // Downloaded faces — the shop's path into the engine
    // -----------------------------------------------------------------------

    /// First readable file from `candidates`. The tests below skip when the
    /// host has no font at all, exactly like the ones above.
    fn read_first(candidates: &[&str]) -> Option<Vec<u8>> {
        candidates.iter().find_map(|path| std::fs::read(path).ok())
    }

    const FACE_A: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/system/fonts/DroidSans.ttf",
        "/system/fonts/Roboto-Regular.ttf",
    ];
    const FACE_B: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        "/system/fonts/CutiveMono.ttf",
        "/system/fonts/DroidSansMono.ttf",
    ];

    #[test]
    fn a_loaded_face_is_reachable_under_its_own_name() {
        let Some(bytes) = read_first(FACE_A) else {
            eprintln!("skipping: no font file on this host");
            return;
        };
        let mut engine = TextEngine::new();
        let name = engine.load_font(bytes).expect("the file must parse");
        assert!(!name.is_empty());
        assert!(
            engine.has_family(&name),
            "the name read out of the file must address the face: {name}"
        );
        let layout = engine
            .layout_styled_family("Rumo", &TextStyle::new(32.0), Some(name.as_str()))
            .expect("layout in the loaded family");
        assert!(!layout.quads.is_empty());
    }

    #[test]
    fn an_unknown_family_is_reported_missing() {
        let engine = TextEngine::new();
        assert!(!engine.has_family("Rumo Definitely Not A Font 12345"));
        assert!(!engine.has_family(""), "an empty name is not a family");
    }

    /// The shop downloads bytes from a network; garbage is an expected input,
    /// and the contract is "registers nothing", not "panics".
    #[test]
    fn unparsable_bytes_register_nothing() {
        let mut engine = TextEngine::new();
        assert!(engine.load_font(Vec::new()).is_none());
        assert!(engine.load_font(vec![0u8; 64]).is_none());
        assert!(engine.load_font(b"not a font at all".to_vec()).is_none());
    }

    /// Two downloaded families in one engine must not collide. The atlas keys
    /// on `font_id`, so the second face's glyphs land in their own rects
    /// instead of overwriting the first's — the failure mode this guards is a
    /// shop preview that shows the previously previewed font.
    #[test]
    fn two_families_share_one_atlas_without_colliding() {
        let (Some(a), Some(b)) = (read_first(FACE_A), read_first(FACE_B)) else {
            eprintln!("skipping: fewer than two font files on this host");
            return;
        };
        let mut engine = TextEngine::with_atlas_size(512, 512, 4096);
        let name_a = engine.load_font(a).expect("A parses");
        let name_b = engine.load_font(b).expect("B parses");
        assert_ne!(name_a, name_b, "the two files must be different families");

        let layout_a = engine
            .layout_styled_family("Hamburgefonstiv", &TextStyle::new(48.0), Some(name_a.as_str()))
            .expect("A layout");
        let after_a = engine.atlas().glyph_count();
        let layout_b = engine
            .layout_styled_family("Hamburgefonstiv", &TextStyle::new(48.0), Some(name_b.as_str()))
            .expect("B layout");
        assert!(
            engine.atlas().glyph_count() > after_a,
            "the second family must add its own glyphs, not reuse the first's"
        );
        let widths_a: Vec<f32> = layout_a.quads.iter().map(|q| q.w).collect();
        let widths_b: Vec<f32> = layout_b.quads.iter().map(|q| q.w).collect();
        assert_ne!(widths_a, widths_b, "two faces must not render identically");
    }

    /// An unknown family does not fail the layout — the shaper falls back to
    /// the default face. That is deliberate for the editor (a project opened on
    /// a device without the font still renders) and is exactly why the shop
    /// checks `has_family` first instead of trusting the picture.
    #[test]
    fn an_unknown_family_falls_back_rather_than_failing() {
        let mut engine = TextEngine::new();
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let layout = engine.layout_styled_family(
            "Rumo",
            &TextStyle::new(32.0),
            Some("Rumo Definitely Not A Font 12345"),
        );
        assert!(
            layout.is_some_and(|l| !l.quads.is_empty()),
            "a missing family must fall back, not drop the layer"
        );
    }

    #[test]
    fn rasterized_text_is_inked_and_sized_to_its_box() {
        let mut engine = TextEngine::with_atlas_size(512, 512, 4096);
        if !engine.has_fonts() {
            eprintln!("skipping: no system fonts available");
            return;
        }
        let layout = engine.layout("Rumo", 48.0).expect("layout");
        let atlas = engine.atlas_image();
        let (w, h, rgba) = crate::composite::rasterize_text(&layout, &atlas, 0xFF00_0000, 4)
            .expect("raster");
        assert!(
            w > 8 && h > 8,
            "the box must be the text plus padding, got {w}x{h}"
        );
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        let inked = rgba.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert!(inked > 0, "the preview must have ink, not just padding");
        // The padding ring stays clear, or the preview reads as a filled block
        // rather than as text.
        assert_eq!(rgba[3], 0, "the first pixel is padding and must be clear");
    }

    #[test]
    fn rasterize_refuses_a_layout_without_ink() {
        let layout = TextLayout {
            quads: Vec::new(),
            width: 0.0,
            height: 0.0,
        };
        let atlas = TextureImage {
            width: 4,
            height: 4,
            rgba: vec![0u8; 64],
        };
        assert!(crate::composite::rasterize_text(&layout, &atlas, 0xFFFF_FFFF, 2).is_none());
    }
}

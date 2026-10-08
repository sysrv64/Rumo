// SPDX-License-Identifier: Apache-2.0

//! Extended preview composition: SHAPE meshes + text quads + image textures.
//!
//! The legacy bitmap/engine paths only knew SHAPE layers. Kotlin already
//! stages text layouts (`jni::layout_text`) and RGBA images
//! (`jni::upload_image`); this module composites all three into one frame:
//!
//! * [`composite_preview`] — synchronous CPU reference (shapes via
//!   [`crate::renderer::render_frame_cpu`], textured draws via
//!   [`crate::renderer::draw_textured_cpu`]). Always available, no GPU.
//! * [`composite_preview_fx`] — the same CPU composite with a per-layer effect
//!   chain ([`crate::effect::cpu::cpu_apply`] on that layer's own buffer
//!   before it is blended). A layer whose chain is empty takes the exact
//!   [`composite_preview`] step, so an effect-free scene is byte-identical.
//! * [`composite_preview_gpu`] — one-`submit` GPU frame via
//!   [`crate::renderer::GpuRenderer::render_scene`] (mesh layers followed by
//!   textured quads). The caller uploads every [`TextureImage`] with
//!   [`crate::renderer::GpuRenderer::set_texture`] first; quads whose id is
//!   unknown are skipped by the renderer.
//!
//! Shape placement (256px box, 60% of frame height, centre + `dx`/`dy`)
//! mirrors `rumo_bridge::preview_meshes` exactly, so an empty text/image set
//! renders byte-for-byte like the legacy SHAPE-only path.
//!
//! Pixel order is engine RGBA8 (`u32` LE, R in the low byte) throughout; the
//! single 0xAARRGGBB conversion for `Bitmap.Config.ARGB_8888` happens once at
//! the JNI exit in [`crate::jni`], never here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::renderer::{GpuRenderer, MeshData, RenderConfig, TexturedQuad, blend_over, render_frame_cpu};
use crate::svg::{SvgDoc, parse_svg, svg_meshes};
use crate::texture::{Mat4, TextureImage, TexturedMesh};
use crate::{ShapeKind, ShapeParams, all_shapes, shape_path};

// ---------------------------------------------------------------------------
// SVG registry (parsed once per source; the frame builder reuses the document)
// ---------------------------------------------------------------------------

/// First id [`register_svg`] hands out, and the boundary between the two things
/// a SHAPE spec's ordinal slot can mean.
///
/// # One slot, two disjoint ranges
///
/// The editor addresses the whole SHAPE group through a single `IntArray`: its
/// `shapeSlot` writes either a Material shape's ordinal
/// (`0..all_shapes().len()`) or a registered SVG's id into the same element, and
/// both frame entry points read it back here. The ranges are kept disjoint **by
/// construction**:
///
/// * `ordinal <  SVG_ID_BASE` — index into [`all_shapes`];
/// * `ordinal >= SVG_ID_BASE` — a [`register_svg`] id.
///
/// Keeping both in one field is deliberate, not convenient. The scene is a set
/// of parallel arrays already threaded through two languages and two editor
/// orientations; a second "is this an SVG" array would have to be kept in
/// lock-step with `ordinals` by every writer on both sides, and one missed index
/// would silently pair an SVG id with a Material ordinal — the wrong layer
/// drawn, with no error to notice. A disjoint range is a single rule
/// (`>= SVG_ID_BASE`) that cannot be forgotten, and it is **enforced by a
/// test**: `svg_id_base_is_above_every_material_ordinal` fails if the shape
/// table ever grows into the SVG range. A second array could only be enforced
/// by everyone remembering.
///
/// The base sits far above the shape table on purpose: the gap is room the
/// table can grow into without renumbering already-registered documents.
pub const SVG_ID_BASE: i32 = 10_000;

/// Registered SVG sources, keyed by a stable id.
///
/// Mirrors the texture registry in [`crate::jni`] — one process-wide
/// `OnceLock<Mutex<..>>`, ids minted upward and never reused, a release
/// that drops the payload, and unknown ids skipped rather than fatal. It lives
/// here rather than beside the texture store because it is read *here*, by the
/// frame builder: the frame builder has no JNI dependency, and putting the
/// store next to its only reader keeps the lookup a plain function call
/// instead of a second lock reached across the module boundary.
struct SvgStore {
    /// Next id to hand out. Starts at [`SVG_ID_BASE`] so a minted id can never
    /// be read as a Material ordinal; anything below the base is either that
    /// ordinal range or the exhausted sentinel [`register_svg`] refuses on.
    next_id: i32,
    /// Shared because the frame builder only borrows a document to tessellate
    /// it. A document is far bigger than the mesh it produces (paths,
    /// gradients, skipped-element names), so a per-frame clone would be the
    /// expensive part.
    docs: HashMap<i32, Arc<SvgDoc>>,
}

impl SvgStore {
    fn new() -> Self {
        Self {
            next_id: SVG_ID_BASE,
            docs: HashMap::new(),
        }
    }
}

fn svgs() -> &'static Mutex<SvgStore> {
    static CELL: OnceLock<Mutex<SvgStore>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(SvgStore::new()))
}

/// Parse `bytes` once and store the document under a fresh id. `None` when
/// [`parse_svg`] rejects the source or the id space is exhausted.
///
/// The id is stable and never reused: a released id is not handed out again,
/// because a frame already being assembled may still reference it, and reuse
/// would silently draw the wrong document instead of skipping a missing one.
/// Ids start at [`SVG_ID_BASE`], above the whole Material ordinal range, so a
/// minted id can never be mistaken for a shape index; the exhaustion check
/// below refuses rather than wraps back into that range. `i32` (not the texture
/// store's `u64`) because the id crosses JNI as a `jint`; the smaller space is
/// still billions of registrations.
pub fn register_svg(bytes: &[u8]) -> Option<i32> {
    let doc = parse_svg(bytes).ok()?;
    let mut store = svgs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id = store.next_id;
    if id < SVG_ID_BASE {
        // The id space is exhausted (the sentinel `i32::MAX + 1` was reached).
        // Refusing is the only alternative to wrapping back into the Material
        // ordinal range, where the same value would be read as a different
        // shape — a silent wrong draw, not an error.
        return None;
    }
    store.next_id = if id == i32::MAX { -1 } else { id + 1 };
    store.docs.insert(id, Arc::new(doc));
    Some(id)
}

/// The registered document for `id`, shared with the frame being assembled.
pub fn svg_doc(id: i32) -> Option<Arc<SvgDoc>> {
    let store = svgs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.docs.get(&id).cloned()
}

/// Forget the document `id`; `true` when one was present. The id stays burned —
/// see [`register_svg`].
pub fn free_svg(id: i32) -> bool {
    let mut store = svgs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.docs.remove(&id).is_some()
}

/// Number of registered documents, for tests and diagnostics.
pub fn svg_count() -> usize {
    let store = svgs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.docs.len()
}

/// Texture id for the shared glyph-atlas snapshot inside
/// [`crate::EngineScene::textures`]. Registry ids start at 1 and grow by one
/// per upload, so `u64::MAX` never collides in practice.
///
/// This id is constant for the whole session while the page under it changes
/// every time another glyph is rasterized, which is why every upload of it
/// carries a [`crate::texture::TextureStamp`] describing the page rather than
/// relying on the id alone.
pub const ATLAS_TEXTURE_ID: u64 = u64::MAX;

/// The in-frame predicate of §11.2, re-exported from the document so the
/// frame builder and the editor cannot drift apart: `t >= start_ms && t <
/// start_ms + duration_ms`.
pub use rumo_core::model::in_frame;

/// One draw's place on the timeline, parallel to a draw list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawWindow {
    /// Where the draw appears, in ms from the start of the project.
    pub start_ms: i64,
    /// How long it stays, in ms. Zero or negative means "never drawn", which is
    /// what the half-open window already says on its own.
    pub duration_ms: i64,
}

impl DrawWindow {
    /// Whether the draw belongs to the frame at `t_ms`.
    pub fn in_frame(&self, t_ms: i64) -> bool {
        in_frame(t_ms, self.start_ms, self.duration_ms)
    }
}

/// The window of draw `index`, or `None` when the caller did not send one.
///
/// `None` is "no time limit" and it is a separate case rather than a sentinel
/// pair of numbers, because that pair cannot be written down: `i64::MIN` with an
/// `i64::MAX` duration sums to `-1`, which is a *representable* result and so
/// survives both a wrapping and a checked addition while describing an empty
/// window. A sentinel of that shape blanks the whole frame instead of bounding
/// it — the failure is silent, and it hits every draw at once.
///
/// An empty or short window array therefore means "no time limit" for the draws
/// it does not cover: a caller from before the parameter sends none at all, and
/// a short array is a bug in the call. Dropping a frame over that would be the
/// worse failure — an extra draw is visible, a missing layer is not.
pub fn window_of(windows: &[DrawWindow], index: usize) -> Option<DrawWindow> {
    windows.get(index).copied()
}

/// [`window_of`] as a predicate, for the loops that skip a draw.
pub fn window_allows(windows: &[DrawWindow], index: usize, t_ms: i64) -> bool {
    window_of(windows, index).is_none_or(|window| window.in_frame(t_ms))
}

/// One SHAPE layer, mirroring `rumo_bridge::PreviewLayer`.
#[derive(Debug, Clone)]
pub struct ShapeSpec {
    /// Index into [`all_shapes`], **or** a registered SVG id when the slot
    /// carries one.
    ///
    /// The scene's SHAPE group is one ordinal array (see [`SVG_ID_BASE`]):
    /// `ordinal < SVG_ID_BASE` is a Material shape, `ordinal >= SVG_ID_BASE` is
    /// a [`register_svg`] id. A spec built directly in Rust may also set
    /// [`ShapeSpec::svg_id`], the explicit spelling of the same thing; when both
    /// are present that field wins.
    pub ordinal: usize,
    pub argb: u32,
    pub dx: f32,
    pub dy: f32,
    pub rotation_deg: f32,
    pub alpha: f32,
    /// Per-layer uniform size multiplier applied on top of the 60%-of-height
    /// base size (`1.0` = the legacy preview geometry). Callers that do not
    /// supply one must pass `1.0`; non-finite values are treated as `1.0`.
    pub scale: f32,
    /// Registered SVG document to draw, or `None` when the id travels in
    /// [`ShapeSpec::ordinal`] instead.
    ///
    /// Two spellings of one thing: the frame path reads the id out of the
    /// ordinal slot (that is the array Kotlin has), while a Rust caller that
    /// builds a spec by hand can name the document here. Either way the value
    /// must be `>= `[`SVG_ID_BASE`], which is what keeps it from being read as a
    /// Material ordinal; see [`SVG_ID_BASE`] for why one field carries both.
    ///
    /// `i32` rather than `u32` with an in-band sentinel: the id comes straight
    /// from [`register_svg`]'s `jint`, so no cast is needed at the JNI edge,
    /// and `None` states "no SVG" without reserving a magic number.
    ///
    /// When an SVG is selected:
    /// * `ordinal` is ignored — the geometry comes from [`svg_meshes`], which
    ///   may emit several sub-meshes with their own colours;
    /// * `argb` is ignored — an SVG carries its own paint, and the Kotlin
    ///   inspector hides the colour control for such a layer, so silently
    ///   tinting the artwork would be worse than leaving the field meaningless;
    /// * `dx`, `dy`, `rotation_deg`, `alpha` and `scale` still apply, exactly
    ///   as they do to an ordinal shape.
    pub svg_id: Option<i32>,
}

/// One CPU textured draw: source image, pixel-space mesh and tint color.
/// For text `image` is the atlas snapshot and `tint` is the text color;
/// for photos `tint` is white scaled by the layer alpha.
pub type CpuTexturedDraw<'a> = (&'a TextureImage, &'a TexturedMesh, [f32; 4]);

/// 0xAARRGGBB (as in Kotlin `Color(argb)`) → linear f32 RGBA.
pub fn argb_to_f32(argb: u32, alpha_mul: f32) -> [f32; 4] {
    let a = ((argb >> 24) & 0xFF) as f32 / 255.0 * alpha_mul.clamp(0.0, 1.0);
    [
        ((argb >> 16) & 0xFF) as f32 / 255.0,
        ((argb >> 8) & 0xFF) as f32 / 255.0,
        (argb & 0xFF) as f32 / 255.0,
        a,
    ]
}

/// Linear f32 RGBA → the engine's packed LE-RGBA `u32` (R in the low byte).
pub fn f32_to_rgba_u32(c: [f32; 4]) -> u32 {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    ch(c[0]) | (ch(c[1]) << 8) | (ch(c[2]) << 16) | (ch(c[3]) << 24)
}

/// The engine's packed LE-RGBA `u32` → 0xAARRGGBB for `Bitmap.Config.ARGB_8888`.
pub fn rgba_u32_to_argb(px: u32) -> u32 {
    let r = px & 0xFF;
    let g = (px >> 8) & 0xFF;
    let b = (px >> 16) & 0xFF;
    let a = (px >> 24) & 0xFF;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Pixel-space → NDC column-major matrix for a `w`×`h` frame.
/// Identical to the legacy preview path.
pub fn ndc_matrix(w: f32, h: f32) -> Mat4 {
    [
        [2.0 / w, 0.0, 0.0, 0.0],
        [0.0, -2.0 / h, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0, 1.0],
    ]
}

/// Tessellate the shape at `ordinal` (`all_shapes()` order) into pixel-space
/// vertices in a `size_px` box (origin top-left) plus its index buffer.
/// Mirrors `rumo_bridge::shape_mesh` (same flatten tolerance, same lyon fill
/// path). `None` for an out-of-range ordinal, invalid params or an empty
/// tessellation.
pub fn shape_mesh_vertices(
    ordinal: usize,
    size_px: f32,
    rounding: f32,
    rotation_deg: f32,
) -> Option<(Vec<[f32; 2]>, Vec<u32>)> {
    let kind = *all_shapes().get(ordinal)?;
    let params = ShapeParams {
        size_px,
        corner_rounding: rounding,
        rotation_deg,
    }
    .validated()?;
    let path = shape_path(kind, params)?;

    let tol = f64::from(size_px) / 512.0;
    let mut flat: Vec<kurbo::PathEl> = Vec::new();
    kurbo::flatten(path.elements().iter().cloned(), tol, |el| {
        flat.push(el);
    });

    use lyon_tessellation::path::PathEvent;
    use lyon_tessellation::path::math::Point;
    let mut events: Vec<PathEvent> = Vec::new();
    let mut first: Option<Point> = None;
    let mut current: Option<Point> = None;
    for el in flat {
        match el {
            kurbo::PathEl::MoveTo(p) => {
                if let (Some(f), Some(c)) = (first, current) {
                    events.push(PathEvent::End {
                        last: c,
                        first: f,
                        close: false,
                    });
                }
                let at = Point::new(p.x as f32, p.y as f32);
                events.push(PathEvent::Begin { at });
                first = Some(at);
                current = Some(at);
            }
            kurbo::PathEl::LineTo(p) => {
                let to = Point::new(p.x as f32, p.y as f32);
                match current {
                    Some(from) => events.push(PathEvent::Line { from, to }),
                    None => {
                        events.push(PathEvent::Begin { at: to });
                        first = Some(to);
                    }
                }
                current = Some(to);
            }
            kurbo::PathEl::ClosePath => {
                if let (Some(f), Some(c)) = (first, current) {
                    events.push(PathEvent::End {
                        last: c,
                        first: f,
                        close: true,
                    });
                }
                first = None;
                current = None;
            }
            _ => {}
        }
    }
    if let (Some(f), Some(c)) = (first, current) {
        events.push(PathEvent::End {
            last: c,
            first: f,
            close: false,
        });
    }
    if events.is_empty() {
        return None;
    }

    let mut buffers: lyon_tessellation::VertexBuffers<Point, u32> =
        lyon_tessellation::VertexBuffers::new();
    {
        let mut builder = lyon_tessellation::BuffersBuilder::new(
            &mut buffers,
            |v: lyon_tessellation::FillVertex| v.position(),
        );
        let mut tess = lyon_tessellation::FillTessellator::new();
        if tess
            .tessellate(
                events,
                &lyon_tessellation::FillOptions::default(),
                &mut builder,
            )
            .is_err()
        {
            return None;
        }
    }
    if buffers.indices.is_empty() {
        return None;
    }
    let half = size_px / 2.0;
    let verts: Vec<[f32; 2]> = buffers
        .vertices
        .iter()
        .map(|v| [v.x + half, v.y + half])
        .collect();
    Some((verts, buffers.indices))
}

/// Tessellate + place SHAPE `specs` for a `width`×`height` frame, exactly like
/// the legacy preview path: each layer at 256px, scaled to 60% of frame
/// height, rotated, moved to frame centre + (`dx`,`dy`). Each layer's own
/// [`ShapeSpec::scale`] multiplies that size (uniform, about the layer centre,
/// which is also the rotation pivot). Unknown ordinals, unknown SVG ids and
/// empty tessellations are skipped (never an error).
///
/// `windows` is parallel to `specs` and is the *second* time gate: a draw
/// outside its window is skipped here, exactly as [`in_frame`] says, so a frame
/// assembled without the editor's own filtering still holds only the layers the
/// timeline says belong to it. A missing entry means no limit — see
/// [`window_of`].
///
/// A spec that selects a registered SVG — via [`ShapeSpec::svg_id`] or an
/// ordinal `>= `[`SVG_ID_BASE`] — contributes one draw per sub-mesh
/// [`svg_meshes`] returns, each with that sub-mesh's own colour; the
/// window gate is still evaluated once per *spec*, before the expansion, so it
/// keeps indexing the caller's arrays. See [`preview_shape_draws`] when the
/// per-draw source spec is needed.
pub fn preview_shape_triples(
    width: u32,
    height: u32,
    time_ms: i64,
    windows: &[DrawWindow],
    specs: &[ShapeSpec],
) -> Vec<(MeshData, [f32; 4], Mat4)> {
    preview_shape_draws(width, height, time_ms, windows, specs)
        .into_iter()
        .map(|(mesh, color, transform, _source)| (mesh, color, transform))
        .collect()
}

/// Like [`preview_shape_triples`], but each draw also carries the index of the
/// spec it came from.
///
/// One spec can produce several draws: a registered SVG expands to one mesh
/// per painted sub-shape. A caller that keeps per-spec state — the effect
/// chains in `jni::build_ex_scene` are indexed per draw — needs that source
/// index to expand its own parallel arrays the same way, or an SVG layer would
/// shift the chains of every spec after it.
pub fn preview_shape_draws(
    width: u32,
    height: u32,
    time_ms: i64,
    windows: &[DrawWindow],
    specs: &[ShapeSpec],
) -> Vec<(MeshData, [f32; 4], Mat4, usize)> {
    let mat = ndc_matrix(width as f32, height as f32);
    let base = 256.0f32;
    let scale = height as f32 * 0.6 / base;
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let mut out = Vec::new();
    for (i, s) in specs.iter().enumerate() {
        // The window gate is evaluated once per spec, before any expansion: a
        // window is written for a spec, not for the draws it emits, so the
        // index tested is always the caller's spec index however many draws
        // follow.
        if !window_allows(windows, i, time_ms) {
            continue;
        }
        // Defensive: a caller that never learned about `scale` (or sends a
        // non-finite value) keeps the legacy 1.0 geometry.
        let layer_scale = if s.scale.is_finite() { s.scale.max(0.0) } else { 1.0 };
        let size = scale * layer_scale;

        // The background is drawn to the frame rather than placed inside it:
        // the vertices are the target's own corners, so no scale, offset or
        // rotation is applied. Those fields answer "where in the frame does this
        // shape sit", and the background *is* the frame — honouring them would
        // make it possible to slide the background off the canvas, which is not
        // something a background can do. Opacity and the effect chain still
        // apply, because those are what the layer is for.
        //
        // Checked before the SVG dispatch: `svg_id` never names this shape, and
        // a spec that carries one is a document, not the frame.
        if s.svg_id.is_none() && matches!(all_shapes().get(s.ordinal), Some(ShapeKind::Frame)) {
            out.push((
                MeshData {
                    vertices: vec![
                        [0.0, 0.0],
                        [width as f32, 0.0],
                        [width as f32, height as f32],
                        [0.0, height as f32],
                    ],
                    indices: vec![0, 1, 2, 0, 2, 3],
                },
                argb_to_f32(s.argb, s.alpha),
                mat,
                i,
            ));
            continue;
        }

        // One slot, two disjoint meanings (see [`SVG_ID_BASE`]): the id travels
        // in `ordinal` when the scene came through JNI, and `svg_id` is the
        // explicit spelling for a spec built in Rust. Reading both here is what
        // lets the frame path reach the registry at all. An ordinal that cannot
        // be an `i32` id is not an SVG, so it falls through to the Material path
        // and is skipped there like any other out-of-range ordinal.
        let svg_id = s
            .svg_id
            .or_else(|| i32::try_from(s.ordinal).ok().filter(|id| *id >= SVG_ID_BASE));
        if let Some(svg_id) = svg_id {
            // Unknown ids are skipped, exactly like unknown texture ids: a
            // layer that lost its source costs one missing draw, never a panic
            // and never a blank frame.
            let Some(doc) = svg_doc(svg_id) else {
                continue;
            };
            // The ordinal path bakes the rotation into `shape_path` before
            // tessellating; an SVG arrives already tessellated, so the rotation
            // is applied here to the placed vertices. Same convention as
            // `shape_path` (screen Y is down, the angle passes through
            // unchanged), rotating about the layer centre.
            let (sin, cos) = s.rotation_deg.to_radians().sin_cos();
            // `svg_meshes` fits the artwork into the `base` box with the same
            // top-left origin `shape_mesh_vertices` uses, so the placement is
            // the ordinal path's with the rotation folded in.
            for sub in svg_meshes(&doc, base) {
                if sub.indices.is_empty() || sub.positions.is_empty() {
                    continue;
                }
                let placed: Vec<[f32; 2]> = sub
                    .positions
                    .iter()
                    .map(|[x, y]| {
                        let (x, y) = (x - base / 2.0, y - base / 2.0);
                        [
                            (x * cos - y * sin) * size + cx + s.dx,
                            (x * sin + y * cos) * size + cy + s.dy,
                        ]
                    })
                    .collect();
                // The SVG's own paint is used; `s.argb` is deliberately *not*
                // applied on top. `s.alpha` still multiplies the paint, so a
                // layer fade keeps working.
                out.push((
                    MeshData {
                        vertices: placed,
                        indices: sub.indices,
                    },
                    argb_to_f32(sub.argb, s.alpha),
                    mat,
                    i,
                ));
            }
            continue;
        }

        let Some((verts, indices)) =
            shape_mesh_vertices(s.ordinal, base, 0.0, s.rotation_deg)
        else {
            continue;
        };
        let placed: Vec<[f32; 2]> = verts
            .iter()
            .map(|[x, y]| {
                [
                    (x - base / 2.0) * size + cx + s.dx,
                    (y - base / 2.0) * size + cy + s.dy,
                ]
            })
            .collect();
        out.push((
            MeshData {
                vertices: placed,
                indices,
            },
            argb_to_f32(s.argb, s.alpha),
            mat,
            i,
        ));
    }
    out
}

/// Rotate mesh vertex positions around (`cx`,`cy`) by `angle_deg`
/// (counter-clockwise in math convention; screen Y is down so pass the
/// Kotlin angle through unchanged to match `rotate()` pivot behaviour).
/// No-op for ~0, non-finite, or empty input.
pub fn rotate_mesh(mesh: &mut TexturedMesh, cx: f32, cy: f32, angle_deg: f32) {
    if !angle_deg.is_finite() || angle_deg.rem_euclid(360.0) < f32::EPSILON {
        return;
    }
    let a = angle_deg.to_radians();
    let (s, c) = a.sin_cos();
    for v in &mut mesh.vertices {
        let x = v.position[0] - cx;
        let y = v.position[1] - cy;
        v.position[0] = cx + x * c - y * s;
        v.position[1] = cy + x * s + y * c;
    }
}

/// Pixel-space textured mesh for one text layout placed at (`dx`,`dy`).
///
/// `page_w`/`page_h` are the atlas page the mesh will be sampled from, and they
/// are passed in rather than stored in the layout: the page grows as glyphs are
/// added, so a layout holds atlas *rects* and the UVs are resolved here, on the
/// page that exists at draw time.
pub fn text_mesh(
    layout: &crate::TextLayout,
    dx: f32,
    dy: f32,
    page_w: u32,
    page_h: u32,
) -> TexturedMesh {
    let mut mesh = TexturedMesh::new();
    for q in &layout.quads {
        let Some(uv) = q.uv(page_w, page_h) else {
            continue;
        };
        mesh.push_quad(q.x + dx, q.y + dy, q.w, q.h, uv);
    }
    mesh
}

/// Scale a text mesh by `s` about the centre of the box the text was laid out
/// in, rather than about its top-left corner.
///
/// The layout is made once per point size, so an animated size has to be a mesh
/// transform; scaling around the corner would drag the layer away from the
/// position `offsetX`/`offsetY` gave it, and the editor's own bounds are what
/// say where that box is. The point size itself does not change, so the glyphs
/// stay the same font and merely get bigger.
///
/// `bounds_w`/`bounds_h` are the layout's unstretched size and `dx`/`dy` its
/// top-left corner, both already in hand at the draw site. Non-finite `s` is a
/// no-op rather than a NaN frame.
pub fn scale_text_mesh(
    mesh: &mut TexturedMesh,
    dx: f32,
    dy: f32,
    bounds_w: f32,
    bounds_h: f32,
    s: f32,
) {
    if !s.is_finite() || s == 1.0 {
        return;
    }
    // Each quad is four corner vertices, so moving every vertex scales the box
    // and its position in one step: `x' = cx + (x - cx) * s`, `w' = w * s`.
    let cx = dx + bounds_w / 2.0;
    let cy = dy + bounds_h / 2.0;
    for v in &mut mesh.vertices {
        v.position[0] = cx + (v.position[0] - cx) * s;
        v.position[1] = cy + (v.position[1] - cy) * s;
    }
}

/// Rasterize one text layout to a standalone RGBA8 image, tinted by `argb`.
///
/// This is the shop's font preview: the same layout and the same atlas the
/// editor draws from, composited on the CPU so a preview cannot disagree with
/// what the layer will look like. `pad` is transparent margin on every side —
/// a preview with the ink flush against the edge reads as clipped.
///
/// The box is measured from the quads, not from [`crate::TextLayout::width`]:
/// a glyph with a negative left side bearing (italics, most script faces) puts
/// ink left of the origin, and the layout's own bounds count only rightward
/// extent. Returns `None` for a layout with no ink or a degenerate box.
pub fn rasterize_text(
    layout: &crate::TextLayout,
    atlas: &TextureImage,
    argb: u32,
    pad: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    use crate::renderer::draw_textured_cpu;
    if layout.quads.is_empty() {
        return None;
    }
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for q in &layout.quads {
        min_x = min_x.min(q.x);
        min_y = min_y.min(q.y);
        max_x = max_x.max(q.x + q.w);
        max_y = max_y.max(q.y + q.h);
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return None;
    }
    let pad_i = i64::from(pad);
    let w = ((max_x - min_x).ceil() as i64 + 2 * pad_i).clamp(1, i64::from(u32::MAX)) as u32;
    let h = ((max_y - min_y).ceil() as i64 + 2 * pad_i).clamp(1, i64::from(u32::MAX)) as u32;
    let mut frame = vec![0u32; w as usize * h as usize];
    let mesh = text_mesh(
        layout,
        pad as f32 - min_x,
        pad as f32 - min_y,
        atlas.width,
        atlas.height,
    );
    draw_textured_cpu(&mut frame, w, h, atlas, &mesh, argb_to_f32(argb, 1.0));
    let mut out = vec![0u8; frame.len() * 4];
    for (i, px) in frame.iter().enumerate() {
        out[i * 4] = (px & 0xFF) as u8;
        out[i * 4 + 1] = ((px >> 8) & 0xFF) as u8;
        out[i * 4 + 2] = ((px >> 16) & 0xFF) as u8;
        out[i * 4 + 3] = ((px >> 24) & 0xFF) as u8;
    }
    Some((w, h, out))
}

/// Pixel-space textured mesh for an image rect. Empty for degenerate sizes
/// (the caller skips those draws).
pub fn image_mesh(x: f32, y: f32, w: f32, h: f32) -> TexturedMesh {    let mut mesh = TexturedMesh::new();
    if w > 0.0 && h > 0.0 && x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() {
        mesh.push_quad(x, y, w, h, [0.0, 0.0, 1.0, 1.0]);
    }
    mesh
}

/// One GPU textured draw for an image rect: full-texture UV mesh, NDC
/// transform for the frame, white tint scaled by `alpha`. Empty mesh for
/// degenerate sizes (the renderer skips empty draws).
pub fn image_quad(
    frame_w: f32,
    frame_h: f32,
    texture_id: u64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    alpha: f32,
) -> TexturedQuad {
    TexturedQuad {
        mesh: image_mesh(x, y, w, h),
        color: [1.0, 1.0, 1.0, alpha.clamp(0.0, 1.0)],
        transform: ndc_matrix(frame_w, frame_h),
        texture_id,
    }
}

/// One GPU textured draw for a text layout: atlas-backed mesh at (`dx`,`dy`),
/// text color tint, NDC transform for the frame. `page_w`/`page_h` size the
/// atlas the mesh samples; see [`text_mesh`].
pub fn text_quad(
    frame_w: f32,
    frame_h: f32,
    layout: &crate::TextLayout,
    texture_id: u64,
    dx: f32,
    dy: f32,
    argb: u32,
    alpha: f32,
    page_w: u32,
    page_h: u32,
) -> TexturedQuad {
    TexturedQuad {
        mesh: text_mesh(layout, dx, dy, page_w, page_h),
        color: argb_to_f32(argb, alpha),
        transform: ndc_matrix(frame_w, frame_h),
        texture_id,
    }
}

/// CPU reference composite: SHAPE triples first (legacy loop), then textured
/// draws (atlas glyphs, photos) on top. Output is engine RGBA8 `u32`.
/// An empty `textured` slice renders exactly like the SHAPE-only path.
pub fn composite_preview(
    width: u32,
    height: u32,
    bg: [f32; 4],
    shapes: &[(MeshData, [f32; 4], Mat4)],
    textured: &[CpuTexturedDraw<'_>],
) -> Vec<u32> {
    use crate::renderer::draw_textured_cpu;
    let n = width as usize * height as usize;
    let mut frame = vec![f32_to_rgba_u32(bg); n];
    if width == 0 || height == 0 {
        return frame;
    }
    let cfg = RenderConfig {
        width,
        height,
        clear_color: [0.0, 0.0, 0.0, 0.0],
    };
    for (md, color, mat) in shapes {
        let layer_px = render_frame_cpu(md, &cfg, mat, *color);
        blend_over(&mut frame, &layer_px);
    }
    for (image, mesh, tint) in textured {
        draw_textured_cpu(&mut frame, width, height, image, mesh, *tint);
    }
    frame
}

/// One CPU layer for [`composite_preview_fx`]: what to draw plus the effect
/// chain that runs on that layer alone.
#[derive(Debug, Clone, Copy)]
pub struct FxCpuLayer<'a> {
    /// The geometry and paint of this layer.
    pub shape: CpuLayerShape<'a>,
    /// Effects applied to this layer's own buffer, in order (the CPU
    /// counterpart of a GPU [`crate::LayerDraw`] chain). An empty chain keeps
    /// the direct [`composite_preview`] step: no effect pass and no temp
    /// buffer at all.
    pub effects: &'a [rumo_core::effect::EffectInstance],
}

/// What one [`FxCpuLayer`] draws: a solid SHAPE mesh or a textured draw.
#[derive(Debug, Clone, Copy)]
pub enum CpuLayerShape<'a> {
    /// Solid-colour tessellated geometry, as produced by
    /// [`preview_shape_triples`].
    Mesh {
        /// Triangle geometry in pixel coordinates.
        mesh: &'a MeshData,
        /// Linear RGBA of the fill.
        color: [f32; 4],
        /// Pixel-space → clip-space transform.
        transform: Mat4,
    },
    /// A textured draw (atlas glyphs, photos), the same triple
    /// [`composite_preview`] takes.
    Textured(CpuTexturedDraw<'a>),
}

/// Engine LE-RGBA `u32` pixels (R in the low byte — exactly
/// [`f32_to_rgba_u32`]'s packing) → straight-alpha RGBA8 **bytes**,
/// `R,G,B,A` per pixel, the layout [`crate::effect::cpu::cpu_apply`]
/// consumes and produces. `dst` is cleared first.
pub fn rgba_u32_to_rgba8(src: &[u32], dst: &mut Vec<u8>) {
    dst.clear();
    dst.reserve(src.len() * 4);
    for px in src {
        dst.push((px & 0xFF) as u8);
        dst.push(((px >> 8) & 0xFF) as u8);
        dst.push(((px >> 16) & 0xFF) as u8);
        dst.push(((px >> 24) & 0xFF) as u8);
    }
}

/// Inverse of [`rgba_u32_to_rgba8`]: straight-alpha RGBA8 bytes → engine
/// LE-RGBA `u32` (R in the low byte). `dst` is cleared first; a trailing
/// partial pixel (a length that is not a multiple of four) is ignored.
pub fn rgba8_to_rgba_u32(src: &[u8], dst: &mut Vec<u32>) {
    dst.clear();
    dst.reserve(src.len() / 4);
    for px in src.chunks_exact(4) {
        dst.push(
            px[0] as u32 | (px[1] as u32) << 8 | (px[2] as u32) << 16 | (px[3] as u32) << 24,
        );
    }
}

/// Run `chain` over one full-frame engine `u32` buffer in place.
///
/// One conversion into RGBA8 bytes, every instance applied in order
/// ([`cpu_apply`] returns its input unchanged for a disabled one, so the chain
/// is exactly as long as its enabled prefix), one conversion back: the CPU
/// twin of the GPU ping-pong. The returned buffer is the next pass's input, so
/// a chain of `n` effects is `n` passes.
fn apply_chain(
    px: &mut Vec<u32>,
    width: u32,
    height: u32,
    chain: &[rumo_core::effect::EffectInstance],
    time: f32,
) {
    use crate::effect::cpu::{cpu_apply, EffectFrame};
    let mut bytes = Vec::new();
    rgba_u32_to_rgba8(px, &mut bytes);
    for inst in chain {
        // A project-defined effect has no CPU oracle: only the author's WGSL
        // knows what it does, and this reference path cannot mirror it. It is
        // skipped here (the layer passes through untouched) because inventing a
        // CPU result would be worse than visibly not applying it.
        if inst.target.builtin().is_none() {
            continue;
        }
        let src = EffectFrame {
            rgba: &bytes,
            width,
            height,
            time,
        };
        bytes = cpu_apply(inst, &src);
    }
    rgba8_to_rgba_u32(&bytes, px);
}

/// Per-layer CPU composite with effect chains: `width * height` engine
/// LE-RGBA `u32`s, R in the low byte, the same convention as
/// [`composite_preview`].
///
/// Each layer is rasterised into its own full-frame buffer, its chain runs on
/// that buffer alone, and only the result is blended into the accumulator —
/// which is what makes per-layer effects possible at all: the textured path of
/// [`composite_preview`] draws straight into the accumulator and so cannot be
/// isolated after the fact.
///
/// * A [`CpuLayerShape::Mesh`] layer goes
///   [`crate::renderer::render_frame_cpu`] → `cpu_apply` → `blend_over`; a
///   [`CpuLayerShape::Textured`] layer is drawn with
///   [`crate::renderer::draw_textured_cpu`] into its own *fully transparent*
///   buffer first, so its chain sees that layer only.
/// * An **empty chain** skips `cpu_apply` and the temp buffer entirely, which
///   makes the effect-free case byte-identical to [`composite_preview`] (and
///   as fast).
/// * A non-empty chain costs one full RGBA8 frame per layer plus one more per
///   chain pass: ~33 MB for a single RGBA8 frame at 3840x2160, so a 4K
///   preview with effects is memory-hungry by design (the GPU path is the one
///   to use interactively).
/// * `width == 0 || height == 0` returns exactly what [`composite_preview`]
///   returns (a zero-length buffer), and nothing here panics.
///
/// `time` is the layer time in seconds, handed to the effect passes.
pub fn composite_preview_fx(
    width: u32,
    height: u32,
    bg: [f32; 4],
    layers: &[FxCpuLayer<'_>],
    time: f32,
) -> Vec<u32> {
    use crate::renderer::draw_textured_cpu;
    let n = width as usize * height as usize;
    let mut frame = vec![f32_to_rgba_u32(bg); n];
    if width == 0 || height == 0 {
        return frame;
    }
    let cfg = RenderConfig {
        width,
        height,
        clear_color: [0.0, 0.0, 0.0, 0.0],
    };
    for layer in layers {
        match &layer.shape {
            CpuLayerShape::Mesh {
                mesh,
                color,
                transform,
            } => {
                let mut px = render_frame_cpu(mesh, &cfg, transform, *color);
                if !layer.effects.is_empty() {
                    apply_chain(&mut px, width, height, layer.effects, time);
                }
                blend_over(&mut frame, &px);
            }
            CpuLayerShape::Textured((image, mesh, tint)) => {
                if layer.effects.is_empty() {
                    // No chain: the exact `composite_preview` step, straight
                    // into the accumulator, without a temp buffer.
                    draw_textured_cpu(&mut frame, width, height, image, mesh, *tint);
                } else {
                    // Own buffer, cleared to fully transparent (0 = alpha 0 in
                    // this packing), so the chain sees this layer and nothing
                    // else. See the 4K memory note above.
                    let mut px = vec![0u32; n];
                    draw_textured_cpu(&mut px, width, height, image, mesh, *tint);
                    apply_chain(&mut px, width, height, layer.effects, time);
                    blend_over(&mut frame, &px);
                }
            }
        }
    }
    frame
}

/// GPU composite: SHAPE layers followed by textured quads in a single submit
/// via [`GpuRenderer::render_scene`]. Upload every backing image with
/// [`GpuRenderer::set_texture`] under the matching id first; quads with an
/// unknown id are skipped by the renderer. `timeout` bounds the readback.
pub async fn composite_preview_gpu(
    renderer: &GpuRenderer,
    width: u32,
    height: u32,
    bg: [f32; 4],
    shapes: &[(MeshData, [f32; 4], Mat4)],
    textured: &[TexturedQuad],
    timeout: Duration,
) -> Result<Vec<u32>, String> {
    renderer
        .render_scene(width, height, bg, shapes, textured, timeout)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextLayout;
    use rumo_core::effect::{EffectInstance, EffectKind};

    /// Black/white checkerboard: pixelating a block of it can never reproduce
    /// the nearest-sampled texel by accident, so "the chain changed something"
    /// is a real assertion.
    fn checker_image(w: u32, h: u32) -> TextureImage {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        TextureImage::new(w, h, rgba).expect("checker image")
    }

    /// Every shape of `shapes`, then every draw of `draws`, each with an empty
    /// chain — the layer order [`composite_preview_fx`] must reproduce.
    fn fx_from<'a>(
        shapes: &'a [(MeshData, [f32; 4], Mat4)],
        draws: &'a [CpuTexturedDraw<'a>],
    ) -> Vec<FxCpuLayer<'a>> {
        let mut layers = Vec::with_capacity(shapes.len() + draws.len());
        for (mesh, color, transform) in shapes {
            layers.push(FxCpuLayer {
                shape: CpuLayerShape::Mesh {
                    mesh,
                    color: *color,
                    transform: *transform,
                },
                effects: &[],
            });
        }
        for draw in draws {
            layers.push(FxCpuLayer {
                shape: CpuLayerShape::Textured(*draw),
                effects: &[],
            });
        }
        layers
    }

    /// `true` when `a` and `b` differ at any pixel of the `x0..x1` × `y0..y1`
    /// region of a `w`-wide frame.
    fn any_diff(a: &[u32], b: &[u32], w: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> bool {
        (y0..y1).any(|y| (x0..x1).any(|x| a[y * w + x] != b[y * w + x]))
    }

    fn chain(inst: EffectInstance) -> Vec<EffectInstance> {
        vec![inst]
    }

    /// One regression case: label, SHAPE triples, textured draws.
    type FxScene<'a> = (
        &'a str,
        &'a [(MeshData, [f32; 4], Mat4)],
        &'a [CpuTexturedDraw<'a>],
    );

    fn blur(radius: f32) -> EffectInstance {
        let mut inst = EffectInstance::new(EffectKind::Blur);
        assert!(inst.set("radius", &[radius]), "radius is a Blur parameter");
        inst
    }

    fn pixelate(size: f32) -> EffectInstance {
        let mut inst = EffectInstance::new(EffectKind::Pixelate);
        assert!(inst.set("size", &[size]), "size is a Pixelate parameter");
        inst
    }

    fn solid_image(w: u32, h: u32, px: [u8; 4]) -> TextureImage {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for _ in 0..w as usize * h as usize {
            rgba.extend_from_slice(&px);
        }
        TextureImage::new(w, h, rgba).expect("solid image")
    }

    /// A layout whose single quad covers a `page_w`×`page_h` atlas, so the mesh
    /// samples the whole page the way the old full-page UV pair did.
    fn one_quad_layout(x: f32, y: f32, w: f32, h: f32, page_w: u32, page_h: u32) -> TextLayout {
        TextLayout {
            quads: vec![crate::GlyphQuad {
                x,
                y,
                w,
                h,
                rect: crate::AtlasRect {
                    x: 0,
                    y: 0,
                    w: page_w,
                    h: page_h,
                },
            }],
            width: x + w,
            height: y + h,
        }
    }

    fn bg() -> [f32; 4] {
        argb_to_f32(0xFF141824, 1.0)
    }

    fn bg_px() -> u32 {
        f32_to_rgba_u32(bg())
    }

    #[test]
    fn text_quad_over_shape_paints_text_zone() {
        // Orange circle in the centre, opaque red "glyph" (white atlas texel
        // tinted red) overlapping the centre: the centre pixel must come out
        // red, the corner must stay background.
        let specs = vec![ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        assert!(!shapes.is_empty());

        let layout = one_quad_layout(30.0, 16.0, 4.0, 4.0, 4, 4);
        let atlas = solid_image(4, 4, [255, 255, 255, 255]);
        let mesh = text_mesh(&layout, 0.0, 0.0, 4, 4);
        let tint = argb_to_f32(0xFFFF0000, 1.0);
        let frame = composite_preview(64, 36, bg(), &shapes, &[(&atlas, &mesh, tint)]);

        // Atlas texel is opaque white, tint is opaque red -> opaque red.
        let expect_red = f32_to_rgba_u32([1.0, 0.0, 0.0, 1.0]);
        assert_eq!(frame[18 * 64 + 32], expect_red, "text must cover shape centre");
        assert_eq!(frame[0], bg_px(), "corner stays background");
        // A pixel far from both shape and text stays background too.
        assert_eq!(frame[35 * 64 + 63], bg_px());
    }

    #[test]
    fn shape_scale_multiplies_the_legacy_geometry() {
        let spec = |scale: f32| ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale,
            svg_id: None,
        };
        let (verts_1, _) = {
            let s = preview_shape_triples(64, 36, 0, &[], &[spec(1.0)]);
            let m = s[0].0.clone();
            (m.vertices.clone(), m.indices.clone())
        };
        let s2 = preview_shape_triples(64, 36, 0, &[], &[spec(2.0)]);
        let verts_2 = &s2[0].0.vertices;
        assert_eq!(verts_2.len(), verts_1.len(), "same tessellation");
        // Uniform about the frame centre: every vertex is exactly twice as far
        // from (32, 18) as with scale 1.0.
        let (cx, cy) = (32.0f32, 18.0f32);
        for (a, b) in verts_1.iter().zip(verts_2.iter()) {
            let (dx1, dy1) = (a[0] - cx, a[1] - cy);
            let (dx2, dy2) = (b[0] - cx, b[1] - cy);
            assert!(
                (dx2 - dx1 * 2.0).abs() < 1e-3 && (dy2 - dy1 * 2.0).abs() < 1e-3,
                "scale 2.0 must double the offset from the centre: {a:?} -> {b:?}"
            );
        }
        // A non-finite scale degrades to 1.0 instead of emitting NaN geometry.
        let s_nan = preview_shape_triples(64, 36, 0, &[], &[spec(f32::NAN)]);
        assert_eq!(s_nan[0].0.vertices, verts_1);
    }

    #[test]
    fn shape_window_skips_the_layers_outside_the_frame() {
        let spec = |dx: f32| ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        let specs = vec![spec(-8.0), spec(0.0), spec(8.0)];
        // Three layers on the timeline, one after another.
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
        assert_eq!(preview_shape_triples(64, 36, 0, &windows, &specs).len(), 1);
        assert_eq!(preview_shape_triples(64, 36, 99, &windows, &specs).len(), 1);
        assert_eq!(preview_shape_triples(64, 36, 150, &windows, &specs).len(), 1);
        assert_eq!(preview_shape_triples(64, 36, 299, &windows, &specs).len(), 1);
        // The window is half-open: at the instant a layer ends the next one has
        // already begun, and after the last one nothing is left.
        assert_eq!(preview_shape_triples(64, 36, 100, &windows, &specs).len(), 1);
        assert_eq!(preview_shape_triples(64, 36, 200, &windows, &specs).len(), 1);
        assert!(preview_shape_triples(64, 36, 300, &windows, &specs).is_empty());
        // A zero length means "never", not "always" — and the two draws the
        // single-element array does not reach stay unlimited, which is the same
        // rule as the short-array case below. Both halves are checked here
        // because they are one behaviour.
        let zero = [DrawWindow {
            start_ms: 0,
            duration_ms: 0,
        }];
        assert_eq!(
            preview_shape_triples(64, 36, 0, &zero, &specs).len(),
            2,
            "the zero-length draw goes, the two the array never mentions stay"
        );
        assert_eq!(
            preview_shape_triples(64, 36, 0, &zero, &specs[..1]).len(),
            0,
            "with nothing beyond it, nothing is left"
        );
        // No window at all, and a window array that stops early, both mean "no
        // time limit" for the draws they do not cover: a caller from before the
        // parameter must keep rendering.
        assert_eq!(preview_shape_triples(64, 36, 1_000_000, &[], &specs).len(), 3);
        // The short array has to *contain* the instant, or this would be testing
        // the bound above rather than the missing tail.
        let covers_now = [DrawWindow {
            start_ms: 1_000_000,
            duration_ms: 1_000,
        }];
        assert_eq!(
            preview_shape_triples(64, 36, 1_000_000, &covers_now, &specs).len(),
            3,
            "one window, the other two unlimited"
        );
        // And the layer that survives is the one the window named: a filtered
        // middle draw must not leave the third one standing in for it.
        let kept = preview_shape_triples(64, 36, 0, &windows, &specs);
        let first_only = preview_shape_triples(64, 36, 0, &[], &specs[..1]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0.vertices, first_only[0].0.vertices);
    }

    #[test]
    fn scale_text_mesh_grows_the_box_about_its_centre() {
        // A layout whose single quad exactly fills its bounds, placed at
        // (`dx`,`dy`) — the shape the editor hands over.
        let layout = TextLayout {
            quads: vec![crate::GlyphQuad {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 4.0,
                rect: crate::AtlasRect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                },
            }],
            width: 10.0,
            height: 4.0,
        };
        let (dx, dy) = (20.0f32, 6.0f32);
        let base = text_mesh(&layout, dx, dy, 4, 4);
        // `(min_x, min_y, max_x, max_y)` and the centre of that box.
        let bounds = |mesh: &TexturedMesh| {
            let mut lo = [f32::INFINITY; 2];
            let mut hi = [f32::NEG_INFINITY; 2];
            for v in &mesh.vertices {
                for k in 0..2 {
                    lo[k] = lo[k].min(v.position[k]);
                    hi[k] = hi[k].max(v.position[k]);
                }
            }
            (
                [lo[0], lo[1], hi[0], hi[1]],
                [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0],
            )
        };
        assert_eq!(bounds(&base).0, [dx, dy, dx + 10.0, dy + 4.0]);
        // The centre of the unstretched box, which is what the scale pivots on.
        let centre = [dx + layout.width / 2.0, dy + layout.height / 2.0];
        assert_eq!(bounds(&base).1, centre);

        // Twice as big about that centre: the box grows outwards, so its corner
        // moves while the centre stays. Scaling around the corner instead would
        // have dragged the centre off to (30, 10).
        let mut twice = base.clone();
        scale_text_mesh(&mut twice, dx, dy, layout.width, layout.height, 2.0);
        assert_eq!(bounds(&twice).0, [15.0, 4.0, 35.0, 12.0]);
        assert_eq!(bounds(&twice).1, centre, "the centre is the anchor");

        // Halved: the same centre, a quarter of the area.
        let mut half = base.clone();
        scale_text_mesh(&mut half, dx, dy, layout.width, layout.height, 0.5);
        assert_eq!(bounds(&half).0, [22.5, 7.0, 27.5, 9.0]);
        assert_eq!(bounds(&half).1, centre);

        // A scale of 1.0 and a non-finite scale leave the mesh alone: a caller
        // that does not animate the size must render the layout as it stands.
        let mut untouched = base.clone();
        scale_text_mesh(&mut untouched, dx, dy, layout.width, layout.height, 1.0);
        scale_text_mesh(&mut untouched, dx, dy, layout.width, layout.height, f32::NAN);
        scale_text_mesh(&mut untouched, dx, dy, layout.width, layout.height, f32::INFINITY);
        assert_eq!(untouched, base);
        // Zero collapses the box onto its centre rather than emitting NaN.
        let mut none = base.clone();
        scale_text_mesh(&mut none, dx, dy, layout.width, layout.height, 0.0);
        assert!(none.vertices.iter().all(|v| {
            (v.position[0] - centre[0]).abs() < 1e-5 && (v.position[1] - centre[1]).abs() < 1e-5
        }));
    }

    #[test]
    fn image_2x2_with_alpha_maps_to_expected_pixels() {
        let img = TextureImage::new(
            2,
            2,
            vec![
                255, 0, 0, 255, // (0,0) opaque red
                0, 255, 0, 255, // (1,0) opaque green
                0, 0, 255, 255, // (0,1) opaque blue
                255, 255, 255, 128, // (1,1) half white
            ],
        )
        .expect("2x2");
        // 2x2 texture over a 4x4 rect: checked pixels stay clear of the
        // quad diagonal (the two triangles parametrize that edge
        // differently, on CPU exactly as on GPU).
        let mesh = image_mesh(10.0, 10.0, 4.0, 4.0);
        let frame =
            composite_preview(64, 36, bg(), &[], &[(&img, &mesh, [1.0, 1.0, 1.0, 1.0])]);

        let at = |x: u32, y: u32| frame[(y * 64 + x) as usize];
        assert_eq!(at(11, 10), 0xFF0000FF, "red texel (LE-RGBA)");
        assert_eq!(at(12, 10), 0xFF00FF00, "green texel");
        assert_eq!(at(10, 12), 0xFFFF0000, "blue texel");
        // Half-white over opaque bg 0xFF141824: (146, 140, 138, 255).
        let px = at(13, 12);
        let ch = |v: u32, s: u32| ((v >> s) & 0xFF) as i32;
        let bgc = bg_px();
        for s in [0u32, 8, 16] {
            let sa = 128.0 / 255.0;
            let expect = 255.0 * sa + (ch(bgc, s) as f32) * (1.0 - sa);
            assert!(
                (ch(px, s) as f32 - expect).abs() <= 1.5,
                "half-white blend channel {s}: got {}, want ~{expect:.1}",
                ch(px, s)
            );
        }
        assert_eq!(ch(px, 24), 255, "result stays opaque over opaque bg");
        assert_eq!(at(0, 0), bg_px());
    }

    #[test]
    fn empty_textured_matches_shapes_only_reference() {
        let specs = vec![
            ShapeSpec {
                ordinal: 0,
                argb: 0xFFFF9800,
                dx: 0.0,
                dy: 0.0,
                rotation_deg: 0.0,
                alpha: 1.0,
                scale: 1.0,
                svg_id: None,
            },
            ShapeSpec {
                ordinal: 1,
                argb: 0xFF00FF00,
                dx: 5.0,
                dy: -3.0,
                rotation_deg: 15.0,
                alpha: 0.5,
                scale: 1.0,
                svg_id: None,
            },
        ];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        let got = composite_preview(64, 36, bg(), &shapes, &[]);

        // Legacy reference loop (same as the old SHAPE-only path).
        let cfg = RenderConfig {
            width: 64,
            height: 36,
            clear_color: [0.0, 0.0, 0.0, 0.0],
        };
        let mut expect = vec![bg_px(); 64 * 36];
        for (md, color, mat) in &shapes {
            let layer_px = render_frame_cpu(md, &cfg, mat, *color);
            blend_over(&mut expect, &layer_px);
        }
        assert_eq!(got, expect, "empty textured set must equal legacy output");

        let bg_only = composite_preview(64, 36, bg(), &[], &[]);
        assert!(bg_only.iter().all(|&p| p == bg_px()));
    }

    #[test]
    fn bad_shape_ordinals_are_skipped_not_fatal() {
        let specs = vec![ShapeSpec {
            ordinal: usize::MAX,
            argb: 0xFFFF0000,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        assert!(preview_shape_triples(64, 36, 0, &[], &specs).is_empty());
        let frame = composite_preview(64, 36, bg(), &[], &[]);
        assert!(frame.iter().all(|&p| p == bg_px()));
    }

    #[test]
    fn degenerate_image_mesh_draws_nothing() {
        let img = solid_image(2, 2, [255, 0, 0, 255]);
        let mesh = image_mesh(5.0, 5.0, 0.0, 2.0);
        assert!(mesh.is_empty());
        let frame = composite_preview(64, 36, bg(), &[], &[(&img, &mesh, [1.0; 4])]);
        assert!(frame.iter().all(|&p| p == bg_px()));
    }

    #[test]
    fn argb_converters_roundtrip() {
        for argb in [0xFF141824u32, 0xFFFF9800, 0xFFFF0000, 0x80010203, 0x00000000] {
            // f32 for transparent black stays transparent black.
            let px = f32_to_rgba_u32(argb_to_f32(argb, 1.0));
            assert_eq!(rgba_u32_to_argb(px), argb, "roundtrip {argb:#X}");
        }
    }

    #[test]
    fn rotate_mesh_quarter_turn_moves_corners() {
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, 2.0, 2.0, [0.0, 0.0, 1.0, 1.0]);
        rotate_mesh(&mut mesh, 1.0, 1.0, 90.0);
        // (0,0) around (1,1) by +90° (screen-clockwise): (2,0).
        let p = mesh.vertices[0].position;
        assert!((p[0] - 2.0).abs() < 1e-5 && (p[1] - 0.0).abs() < 1e-5, "got {p:?}");
        // UVs untouched by rotation.
        assert_eq!(mesh.vertices[0].uv, [0.0, 0.0]);
    }

    #[test]
    fn rotate_mesh_zero_and_full_turn_are_noops() {
        for angle in [0.0, 360.0, -360.0, 720.0] {
            let mut mesh = TexturedMesh::new();
            mesh.push_quad(3.0, 5.0, 7.0, 9.0, [0.0, 0.0, 1.0, 1.0]);
            let before = mesh.vertices.clone();
            rotate_mesh(&mut mesh, 100.0, 100.0, angle);
            assert_eq!(mesh.vertices, before, "angle {angle}");
        }
        // Non-finite angle never panics, mesh unchanged.
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(3.0, 5.0, 7.0, 9.0, [0.0, 0.0, 1.0, 1.0]);
        let before = mesh.vertices.clone();
        rotate_mesh(&mut mesh, 1.0, 1.0, f32::NAN);
        assert_eq!(mesh.vertices, before);
    }

    // --- composite_preview_fx (CPU effect chains) ---------------------------

    #[test]
    fn rgba_u32_bytes_roundtrip_matches_the_packing() {
        let colors = [
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.2, 0.9, 0.2, 1.0],
            [0.5, 0.25, 0.75, 0.5],
            [0.0, 1.0, 0.0, 0.3],
        ];
        for c in colors {
            let px = f32_to_rgba_u32(c);
            let mut bytes = Vec::new();
            rgba_u32_to_rgba8(&[px], &mut bytes);
            assert_eq!(
                bytes,
                vec![px as u8, (px >> 8) as u8, (px >> 16) as u8, (px >> 24) as u8],
                "R must be the low byte of the packed u32 (colour {c:?})"
            );
            let mut back = Vec::new();
            rgba8_to_rgba_u32(&bytes, &mut back);
            assert_eq!(back, vec![px], "roundtrip for {c:?}");
        }
        // A trailing partial pixel is dropped; an empty buffer stays empty.
        let mut bytes = Vec::new();
        rgba_u32_to_rgba8(&[0x0403_0201, 0x0807_0605], &mut bytes);
        let mut back = Vec::new();
        rgba8_to_rgba_u32(&bytes[..7], &mut back);
        assert_eq!(back, vec![0x0403_0201]);
        let mut empty = Vec::new();
        rgba8_to_rgba_u32(&[], &mut empty);
        assert!(empty.is_empty());
        let mut empty8 = Vec::new();
        rgba_u32_to_rgba8(&[], &mut empty8);
        assert!(empty8.is_empty());
    }

    #[test]
    fn fx_empty_chains_match_composite_preview_byte_for_byte() {
        let specs = vec![
            ShapeSpec {
                ordinal: 0,
                argb: 0xFFFF9800,
                dx: -6.0,
                dy: -4.0,
                rotation_deg: 0.0,
                alpha: 1.0,
                scale: 1.0,
                svg_id: None,
            },
            ShapeSpec {
                ordinal: 5,
                argb: 0x8000_FF00,
                dx: 9.0,
                dy: 5.0,
                rotation_deg: 25.0,
                alpha: 0.6,
                scale: 1.0,
                svg_id: None,
            },
        ];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        assert_eq!(shapes.len(), 2, "both shapes must tessellate");
        let img = checker_image(8, 8);
        let mesh = image_mesh(4.0, 3.0, 30.0, 20.0);
        let draws: Vec<CpuTexturedDraw<'_>> = vec![(&img, &mesh, [1.0, 1.0, 1.0, 0.75])];

        let cases: [FxScene<'_>; 4] = [
            ("empty", &[], &[]),
            ("shape-only", &shapes, &[]),
            ("textured-only", &[], &draws),
            ("mixed", &shapes, &draws),
        ];
        for (name, s, d) in cases {
            let fx = fx_from(s, d);
            let want = composite_preview(64, 36, bg(), s, d);
            let got = composite_preview_fx(64, 36, bg(), &fx, 0.75);
            assert_eq!(got, want, "scene `{name}` differs from composite_preview");
        }
    }

    #[test]
    fn blur_chain_changes_the_layer_and_spares_far_pixels() {
        let specs = vec![ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        assert_eq!(shapes.len(), 1);
        let plain = fx_from(&shapes, &[]);
        let blurred_chain = chain(blur(6.0));
        let mut blurred = fx_from(&shapes, &[]);
        blurred[0].effects = &blurred_chain;

        let a = composite_preview_fx(64, 36, bg(), &plain, 0.0);
        let b = composite_preview_fx(64, 36, bg(), &blurred, 0.0);
        assert_ne!(a, b, "a Blur chain must change the frame");
        // The circle is ~22 px across in the centre and the blur reaches ~6 px,
        // so both corners stay pure background.
        assert_eq!(b[0], bg_px(), "top-left corner must stay background");
        assert_eq!(b[35 * 64 + 63], bg_px(), "bottom-right corner must stay background");
    }

    #[test]
    fn fx_chain_is_isolated_per_layer() {
        const W: usize = 96;
        const H: usize = 64;
        // Bottom quad (8,8)-(40,40) and top quad (24,24)-(56,56) overlap in
        // 24..40 x 24..40; each has a region only it covers.
        let img = checker_image(8, 8);
        let bottom = image_mesh(8.0, 8.0, 32.0, 32.0);
        let top = image_mesh(24.0, 24.0, 32.0, 32.0);
        let half = [1.0, 1.0, 1.0, 0.5];
        let blocks = chain(pixelate(6.0));

        let scene = |bottom_fx: &[EffectInstance], top_fx: &[EffectInstance]| -> Vec<u32> {
            let layers = [
                FxCpuLayer {
                    shape: CpuLayerShape::Textured((&img, &bottom, half)),
                    effects: bottom_fx,
                },
                FxCpuLayer {
                    shape: CpuLayerShape::Textured((&img, &top, half)),
                    effects: top_fx,
                },
            ];
            composite_preview_fx(W as u32, H as u32, bg(), &layers, 0.0)
        };
        // Regions chosen so a 6 px pixelate block of a 32 px / 8 texel quad can
        // never read across the other quad's edge.
        let (bx0, bx1, by0, by1) = (9, 19, 9, 19);
        let (tx0, tx1, ty0, ty1) = (44, 55, 44, 55);

        let reference = scene(&[], &[]);
        let bottom_fx = scene(&blocks, &[]);
        let top_fx = scene(&[], &blocks);

        assert!(
            any_diff(&bottom_fx, &reference, W, bx0, bx1, by0, by1),
            "the bottom layer's own Pixelate must change the bottom layer"
        );
        assert!(
            !any_diff(&bottom_fx, &reference, W, tx0, tx1, ty0, ty1),
            "the bottom chain must not touch the top layer's contribution"
        );
        assert!(
            any_diff(&top_fx, &reference, W, tx0, tx1, ty0, ty1),
            "the top layer's own Pixelate must change the top layer"
        );
        assert!(
            !any_diff(&top_fx, &reference, W, bx0, bx1, by0, by1),
            "the top chain must not touch the bottom layer's contribution"
        );
    }

    #[test]
    fn chain_that_removes_the_layer_leaves_the_background() {
        let argb = 0xFF33_E633;
        let fill = argb_to_f32(argb, 1.0);
        let specs = vec![ShapeSpec {
            ordinal: 0,
            argb,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        assert_eq!(shapes.len(), 1);
        let mut layers = fx_from(&shapes, &[]);

        // Key exactly on the layer's own colour, with a similarity wide enough
        // to also swallow the transparent pixels (their chroma distance to the
        // key is ~0.71): the whole layer drops out.
        let mut key = EffectInstance::new(EffectKind::ChromaKey);
        assert!(key.set("key", &fill));
        assert!(key.set("similarity", &[0.8]));
        assert!(key.set("softness", &[0.05]));
        let keyed = chain(key);
        layers[0].effects = &keyed;

        let got = composite_preview_fx(64, 36, bg(), &layers, 0.0);
        assert_eq!(got.len(), 64 * 36);
        let empty = composite_preview(64, 36, bg(), &[], &[]);
        assert_eq!(got, empty, "a fully chroma-keyed layer must vanish");
        let plain = composite_preview(64, 36, bg(), &shapes, &[]);
        assert_ne!(got, plain, "the removing chain must differ from no chain at all");

        // A colour-tune chain instead keeps the layer but changes it.
        let mut bright = EffectInstance::new(EffectKind::ColorTune);
        assert!(bright.set("brightness", &[0.3]));
        let tuned = chain(bright);
        layers[0].effects = &tuned;
        let tuned_px = composite_preview_fx(64, 36, bg(), &layers, 0.0);
        assert_eq!(tuned_px.len(), 64 * 36);
        assert_ne!(tuned_px, empty, "a colour-tune chain must change the layer");
    }

    #[test]
    fn fx_degenerate_sizes_survive_every_effect_kind() {
        let img = checker_image(2, 2);
        for (w, h) in [(1u32, 1u32), (3, 5)] {
            let mesh = MeshData {
                vertices: vec![[0.0, 0.0], [w as f32, 0.0], [0.0, h as f32]],
                indices: vec![0, 1, 2],
            };
            let tm = image_mesh(0.0, 0.0, w as f32, h as f32);
            for kind in EffectKind::ALL {
                let chain = chain(EffectInstance::new(*kind));
                let layers = [
                    FxCpuLayer {
                        shape: CpuLayerShape::Mesh {
                            mesh: &mesh,
                            color: [0.5, 0.2, 0.7, 1.0],
                            transform: ndc_matrix(w as f32, h as f32),
                        },
                        effects: &chain,
                    },
                    FxCpuLayer {
                        shape: CpuLayerShape::Textured((&img, &tm, [1.0, 1.0, 1.0, 0.5])),
                        effects: &chain,
                    },
                ];
                let out = composite_preview_fx(w, h, bg(), &layers, 0.5);
                assert_eq!(out.len(), (w * h) as usize, "{} at {w}x{h}", kind.id());
            }
        }
        // A degenerate frame returns exactly what `composite_preview` returns.
        assert_eq!(
            composite_preview_fx(0, 0, bg(), &[], 0.0),
            composite_preview(0, 0, bg(), &[], &[])
        );
        assert_eq!(
            composite_preview_fx(0, 7, bg(), &[], 0.0),
            composite_preview(0, 7, bg(), &[], &[])
        );
        assert_eq!(
            composite_preview_fx(5, 0, bg(), &[], 0.0),
            composite_preview(5, 0, bg(), &[], &[])
        );
    }

    #[test]
    fn disabled_instance_behaves_as_if_absent() {
        let specs = vec![ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        let img = checker_image(8, 8);
        let mesh = image_mesh(6.0, 4.0, 24.0, 16.0);
        let draws: Vec<CpuTexturedDraw<'_>> = vec![(&img, &mesh, [1.0, 1.0, 1.0, 0.8])];

        let mut off = pixelate(6.0);
        off.enabled = false;
        let disabled = chain(off);

        let mut layers = fx_from(&shapes, &draws);
        for layer in &mut layers {
            layer.effects = &disabled;
        }
        let got = composite_preview_fx(64, 36, bg(), &layers, 0.0);
        let want = composite_preview(64, 36, bg(), &shapes, &draws);
        assert_eq!(got, want, "a disabled instance must not change a single byte");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter (Vulkan/GL) at runtime"]
    fn gpu_composite_matches_cpu_for_solid_layers() {
        use std::time::Duration;
        let specs = vec![ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        }];
        let shapes = preview_shape_triples(64, 36, 0, &[], &specs);
        let img = solid_image(8, 8, [0, 0, 255, 255]);
        let mesh = image_mesh(4.0, 4.0, 8.0, 8.0);
        let cpu = composite_preview(64, 36, bg(), &shapes, &[(&img, &mesh, [1.0; 4])]);

        let gpu = pollster::block_on(GpuRenderer::new()).expect("gpu");
        gpu.set_texture(7, crate::texture::TextureStamp::for_content(7), &img)
            .expect("upload");
        let quads = vec![TexturedQuad {
            mesh,
            color: [1.0, 1.0, 1.0, 1.0],
            transform: ndc_matrix(64.0, 36.0),
            texture_id: 7,
        }];
        let gpu_px = pollster::block_on(composite_preview_gpu(
            &gpu,
            64,
            36,
            bg(),
            &shapes,
            &quads,
            Duration::from_secs(30),
        ))
        .expect("gpu composite");
        // Solid fills agree within ±2 per channel (linear vs CPU rounding).
        for (c, g) in cpu.iter().zip(gpu_px.iter()) {
            for s in [0u32, 8, 16, 24] {
                let d = (((*c >> s) & 0xFF) as i32 - ((*g >> s) & 0xFF) as i32).abs();
                assert!(d <= 2, "channel {s} differs by {d}");
            }
        }
    }

    // -----------------------------------------------------------------
    // SVG-backed SHAPE specs
    // -----------------------------------------------------------------

    /// Two filled rectangles with different colours: enough paint for
    /// `svg_meshes` to return two meshes, in document order.
    const TWO_RECTS_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="0" y="0" width="40" height="40" fill="#ff0000"/>
        <rect x="60" y="60" width="40" height="40" fill="#0000ff"/>
    </svg>"##;

    fn svg_spec(svg_id: i32) -> ShapeSpec {
        ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: Some(svg_id),
        }
    }

    /// Union of every vertex of every draw, or `None` when there are none.
    fn draws_bounds(draws: &[(MeshData, [f32; 4], Mat4)]) -> Option<(f32, f32, f32, f32)> {
        let mut it = draws.iter().flat_map(|(mesh, _, _)| mesh.vertices.iter());
        let first = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
        for [x, y] in it {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        Some((x0, y0, x1, y1))
    }

    /// Regression guard: an ordinal spec with `svg_id: None` is placed exactly
    /// as the legacy path placed it.
    #[test]
    fn ordinal_spec_without_svg_keeps_the_legacy_geometry() {
        let spec = ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 3.0,
            dy: -2.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        let draws = preview_shape_triples(64, 36, 0, &[], &[spec]);
        assert_eq!(draws.len(), 1, "an ordinal spec is one draw");
        // Reproduce the legacy placement by hand and require an exact match.
        let (verts, indices) = shape_mesh_vertices(0, 256.0, 0.0, 0.0).expect("circle");
        let size = 36.0f32 * 0.6 / 256.0;
        let placed: Vec<[f32; 2]> = verts
            .iter()
            .map(|[x, y]| {
                [
                    (x - 128.0) * size + 32.0 + 3.0,
                    (y - 128.0) * size + 18.0 - 2.0,
                ]
            })
            .collect();
        assert_eq!(draws[0].0.vertices, placed);
        assert_eq!(draws[0].0.indices, indices);
        assert_eq!(draws[0].1, argb_to_f32(0xFFFF9800, 1.0));
    }

    /// An unregistered id is skipped, not a panic and not a blank frame.
    #[test]
    fn unknown_svg_id_is_skipped_not_fatal() {
        assert!(svg_doc(999_999).is_none(), "id must be unregistered");
        let draws = preview_shape_triples(64, 36, 0, &[], &[svg_spec(999_999)]);
        assert!(draws.is_empty(), "an unknown id contributes no draw");
    }

    /// The encoding is only safe while the two ranges stay disjoint: a Material
    /// ordinal that reached the SVG base would be read as a registered document.
    /// This is the test that makes adding a shape fail loudly instead of
    /// silently reinterpreting an existing layer.
    #[test]
    fn svg_id_base_is_above_every_material_ordinal() {
        let shapes = all_shapes().len();
        assert!(
            shapes < SVG_ID_BASE as usize,
            "the shape table ({shapes} shapes) must stay strictly below SVG_ID_BASE \
             ({SVG_ID_BASE}); raise the base instead of letting the ranges overlap"
        );
    }

    /// The background covers the frame, and is not placed inside it.
    ///
    /// This is the one behaviour of the background layer that no other shape
    /// test would catch: every other shape is scaled to 60% of the height and
    /// moved to `dx`/`dy`, so a Frame that went through the same path would draw
    /// a small square in the middle instead of a background. The offset and the
    /// scale are set to values that would move it visibly, so a regression to
    /// the shared path fails here rather than silently in a render.
    #[test]
    fn frame_covers_the_target_and_ignores_placement() {
        let frame_ordinal = all_shapes()
            .iter()
            .position(|k| *k == ShapeKind::Frame)
            .expect("the catalogue has a Frame");
        let spec = ShapeSpec {
            ordinal: frame_ordinal,
            argb: 0xFF112233,
            // Both of these are deliberately non-default: if they were honoured
            // the quad would be small and off-centre.
            dx: 40.0,
            dy: -25.0,
            rotation_deg: 90.0,
            alpha: 0.5,
            scale: 0.25,
            svg_id: None,
        };
        let draws = preview_shape_draws(64, 36, 0, &[], &[spec]);
        assert_eq!(draws.len(), 1, "one draw for the background");
        let (mesh, paint, _, index) = &draws[0];
        assert_eq!(*index, 0, "the chain index stays the caller's spec index");
        assert_eq!(*paint, argb_to_f32(0xFF112233, 0.5), "colour and opacity apply");

        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v[0]).collect();
        let ys: Vec<f32> = mesh.vertices.iter().map(|v| v[1]).collect();
        let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert_eq!((min_x, max_x), (0.0, 64.0), "spans the target's width");
        assert_eq!((min_y, max_y), (0.0, 36.0), "spans the target's height");
    }

    /// Registration mints ids inside the SVG range and never reuses one, which
    /// is what makes the `ordinal >= SVG_ID_BASE` dispatch a real test.
    #[test]
    fn registered_svg_ids_start_in_the_svg_range_and_are_distinct() {
        let a = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register a");
        let b = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register b");
        assert!(a >= SVG_ID_BASE, "id {a} must be in the SVG range");
        assert!(b >= SVG_ID_BASE, "id {b} must be in the SVG range");
        assert_ne!(a, b, "two registrations must be two distinct ids");
        assert!(free_svg(a), "a was registered");
        assert!(free_svg(b), "b was registered");
    }

    /// The dispatch itself: one spec, only the ordinal differs. At or above the
    /// base it draws the SVG's own sub-meshes; below it, the Material shape.
    #[test]
    fn ordinal_slot_dispatches_between_svg_and_material() {
        let id = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register");
        let material = ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        // Below the base: the Material shape, with the spec's own colour.
        let m = preview_shape_draws(64, 36, 0, &[], &[material.clone()]);
        assert_eq!(m.len(), 1, "one Material draw");
        assert_eq!(m[0].1, argb_to_f32(0xFFFF9800, 1.0), "Material paint");
        // At or above the base: the SVG's two rects, in document order, with
        // their own colours and the spec's `argb` deliberately unused.
        assert!(id >= SVG_ID_BASE, "the registered id is the encoded slot");
        let svg = ShapeSpec {
            ordinal: id as usize,
            ..material.clone()
        };
        let s = preview_shape_draws(64, 36, 0, &[], &[svg]);
        assert_eq!(s.len(), 2, "the SVG's two sub-meshes");
        assert_eq!(s[0].1, argb_to_f32(0xFFFF0000, 1.0), "red rect");
        assert_eq!(s[1].1, argb_to_f32(0xFF0000FF, 1.0), "blue rect");
        assert!(s.iter().all(|draw| draw.1 != argb_to_f32(0xFFFF9800, 1.0)));
        assert!(free_svg(id));
    }

    /// An ordinal in the SVG range that names no registered document is
    /// skipped: the encoding must fail safe, never fall back to a Material
    /// ordinal and never panic.
    #[test]
    fn unregistered_ordinal_in_the_svg_range_is_skipped() {
        // Far above any id these tests mint, but still a valid `i32`.
        let missing = SVG_ID_BASE + 999_999;
        assert!(svg_doc(missing).is_none(), "id must be unregistered");
        let spec = ShapeSpec {
            ordinal: missing as usize,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        assert!(
            preview_shape_draws(64, 36, 0, &[], &[spec]).is_empty(),
            "an unregistered SVG slot contributes nothing, and nothing is fatal"
        );
    }

    /// A registered SVG draws its sub-meshes, each in its own colour, in
    /// document order, and the spec's `argb` is not applied on top.
    #[test]
    fn registered_svg_draws_one_triple_per_submesh_in_document_order() {
        let id = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register");
        let draws = preview_shape_triples(64, 36, 0, &[], &[svg_spec(id)]);
        assert_eq!(draws.len(), 2, "two rects, two draws");
        assert_eq!(draws[0].1, argb_to_f32(0xFFFF0000, 1.0), "red comes first");
        assert_eq!(draws[1].1, argb_to_f32(0xFF0000FF, 1.0), "blue comes second");
        // The spec's own `argb` must not tint the artwork.
        assert_ne!(draws[0].1, argb_to_f32(0xFFFF9800, 1.0));
        assert!(draws.iter().all(|(mesh, _, _)| !mesh.indices.is_empty()));
        assert!(free_svg(id));
        assert!(
            preview_shape_triples(64, 36, 0, &[], &[svg_spec(id)]).is_empty(),
            "a released id is skipped, not redrawn"
        );
    }

    /// `dx`/`dy` translate every sub-mesh and `scale` grows it about the layer
    /// centre, exactly like an ordinal spec.
    #[test]
    fn svg_spec_dx_dy_scale_move_every_submesh() {
        let id = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register");
        let a = preview_shape_triples(64, 36, 0, &[], &[svg_spec(id)]);
        let mut moved = svg_spec(id);
        moved.dx = 12.0;
        moved.dy = -7.0;
        let b = preview_shape_triples(64, 36, 0, &[], &[moved]);
        assert_eq!(a.len(), b.len());
        for ((ma, _, _), (mb, _, _)) in a.iter().zip(b.iter()) {
            assert_eq!(ma.vertices.len(), mb.vertices.len(), "same tessellation");
            for (va, vb) in ma.vertices.iter().zip(mb.vertices.iter()) {
                assert!((vb[0] - va[0] - 12.0).abs() < 1e-4, "dx applied to x");
                assert!((vb[1] - va[1] + 7.0).abs() < 1e-4, "dy applied to y");
            }
        }
        let mut scaled = svg_spec(id);
        scaled.scale = 2.0;
        let c = preview_shape_triples(64, 36, 0, &[], &[scaled]);
        let (ax0, ay0, ax1, ay1) = draws_bounds(&a).expect("bounds");
        let (cx0, cy0, cx1, cy1) = draws_bounds(&c).expect("bounds");
        // The 256px box is centred at the frame centre (32, 18); scaling is
        // about that centre, so it stays put and the span doubles.
        assert!(((ax0 + ax1) / 2.0 - 32.0).abs() < 1e-3, "x centre unmoved");
        assert!(((cy0 + cy1) / 2.0 - 18.0).abs() < 1e-3, "y centre unmoved");
        assert!((((cx1 - cx0) / (ax1 - ax0)) - 2.0).abs() < 1e-3, "x span doubled");
        assert!((((cy1 - cy0) / (ay1 - ay0)) - 2.0).abs() < 1e-3, "y span doubled");
        assert!(free_svg(id));
    }

    /// The window gate drops the spec the timeline says, even when an SVG spec
    /// among several ordinary ones expands to more than one draw. If the gate
    /// ran per expanded draw, the SVG's second mesh would consume the next
    /// spec's window and the counts below would be wrong.
    #[test]
    fn window_gate_drops_the_right_spec_around_an_svg_layer() {
        let id = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register");
        let ordinary = |ordinal: usize| ShapeSpec {
            ordinal,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        let specs = vec![ordinary(0), svg_spec(id), ordinary(1)];
        let windows = vec![
            DrawWindow { start_ms: 0, duration_ms: 100 },
            DrawWindow { start_ms: 100, duration_ms: 100 },
            DrawWindow { start_ms: 200, duration_ms: 100 },
        ];
        // t=0: only the first ordinary spec.
        assert_eq!(preview_shape_triples(64, 36, 0, &windows, &specs).len(), 1);
        // t=150: only the SVG spec, which expands to its two sub-meshes.
        assert_eq!(preview_shape_triples(64, 36, 150, &windows, &specs).len(), 2);
        // t=250: only the third spec.
        assert_eq!(preview_shape_triples(64, 36, 250, &windows, &specs).len(), 1);
        // Past every window: nothing.
        assert!(preview_shape_triples(64, 36, 300, &windows, &specs).is_empty());
        assert!(free_svg(id));
    }

    /// One spec, N draws: every draw reports the spec that produced it, which
    /// is what keeps a caller's per-spec chain array aligned.
    #[test]
    fn draws_carry_the_source_spec_index() {
        let id = register_svg(TWO_RECTS_SVG.as_bytes()).expect("register");
        let plain = ShapeSpec {
            ordinal: 0,
            argb: 0xFFFF9800,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
            scale: 1.0,
            svg_id: None,
        };
        let specs = vec![plain, svg_spec(id)];
        let draws = preview_shape_draws(64, 36, 0, &[], &specs);
        assert_eq!(draws.len(), 3, "one ordinal draw + two SVG sub-meshes");
        let sources: Vec<usize> = draws.iter().map(|draw| draw.3).collect();
        assert_eq!(sources, vec![0, 1, 1]);
        assert!(free_svg(id));
    }
}

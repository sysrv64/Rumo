// SPDX-License-Identifier: Apache-2.0

//! Real SVG rendering: SVG is parsed into **vector geometry** and tessellated
//! into meshes the engine draws. It is never rasterised to a bitmap.
//!
//! # Parser and licence
//!
//! Parsing is done by [`usvg`], pinned to `0.48.1` with `default-features = false`.
//! `usvg` is dual-licensed `Apache-2.0 OR MIT` (it left the MPL behind when it
//! moved under linebender), and it is pure Rust — it pulls in no C toolchain.
//! The disabled default features are `text` (fontdb/harfrust/skrifa), `svgz`
//! (`flate2`), `system-fonts`, `memmap-fonts` and `writer`: we supply our own
//! text engine and do not want any of them in the build graph.
//!
//! `usvg` resolves everything that is annoying about SVG before we see it:
//! `viewBox`, nested `transform`s, `use`, unit conversion, `fill-rule`,
//! colours and `gradient`/`pattern` definitions. What reaches this module is a
//! tree of `Group`s and `Path`s, each `Path` carrying absolute geometry, an
//! absolute transform and resolved paints.
//!
//! # Coordinate space produced by [`svg_meshes`]
//!
//! The doc is fitted into the `size_px` box preserving aspect: `s = size_px /
//! max(width, height)`, so the **larger** source dimension spans exactly
//! `size_px` and the smaller one spans `size_px * (minor/major)`. Each shape's
//! own absolute transform is then applied. Positions are therefore in **pixel
//! units with the origin at the top-left corner of the fitted box** (SVG's own
//! `y`-down convention, matching the engine's clip space once the caller
//! applies its viewport matrix). The fitted content starts at `(0, 0)` and
//! extends to at most `size_px` in the major axis.
//!
//! # How gradients are flattened
//!
//! The engine's fragment shader returns one uniform colour per draw, so a
//! gradient that spans a shape cannot be drawn as a gradient. Rather than
//! silently painting it as one arbitrary stop, every gradient paint is
//! flattened to a single colour by averaging its stops:
//!
//! * each stop's colour is converted from sRGB8 to **linear-light sRGB**;
//! * stops are weighted by the length of the offset interval they dominate
//!   (trapezoid rule over `stop-offset`: endpoints get `(o1 - o0) / 2`, interior
//!   stops get `(o[i+1] - o[i-1]) / 2`), which integrates the piecewise-linear
//!   gradient over `[0, 1]`; if all offsets are equal the weights fall back to
//!   uniform;
//! * the linear average is converted back to sRGB8, and stop opacity is
//!   averaged with the same weights;
//! * the owning fill/stroke opacity multiplies the result.
//!
//! Every such flattening increments [`SvgDoc::flattened_gradients`], so a caller
//! that wants real gradients can detect that this happened instead of trusting
//! the flat colour.
//!
//! # Deliberate omissions
//!
//! Anything the engine cannot express honestly is recorded in
//! [`SvgDoc::skipped`] instead of being faked:
//!
//! * **`pattern` paint servers** — the engine has no tiling paint; dropped.
//! * **group opacity < 1** — the engine has per-layer alpha but no per-group
//!   alpha, and drawing a translucent group as opaque is a visible lie; the
//!   group is recorded and its shapes are drawn opaque.
//! * **`clipPath`, `mask`, `filter`, `mix-blend-mode`** — no engine support;
//!   recorded and ignored.
//! * **`text`** — parsed by our own text engine, not here; `usvg`'s text feature
//!   is compiled out, so text nodes do not occur.
//! * **`image`** — raster images are not vector geometry.
//! * **`paint-order: stroke fill`** — fill is always emitted before stroke, so
//!   the SVG's request to paint the stroke first is not honoured.
//! * **per-shape `opacity` on a nested group's children** is folded into the
//!   shape's paint alpha where it is a fill/stroke opacity; group opacity is not.
//!
//! # Failures
//!
//! [`parse_svg`] returns `Err(String)` for bytes that are not UTF-8 or not a
//! parsable SVG, and an empty [`SvgDoc`] for a structurally valid but empty
//! document. Nothing here panics on malformed input.

use kurbo::{Affine, BezPath, PathEl};
use lyon_tessellation::path::PathEvent;
use lyon_tessellation::path::math::Point as LyonPoint;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, LineCap, LineJoin,
    StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
};
use usvg::tiny_skia_path::{PathSegment, Transform};
use usvg::{Group, Node, Paint, Stop};

/// One drawable piece of an SVG: geometry, where it goes, and what colour.
#[derive(Debug, Clone)]
pub struct SvgShape {
    /// Geometry in SVG user space (before `transform` is applied).
    pub path: BezPath,
    /// The path's absolute transform, all ancestor group transforms composed.
    pub transform: Affine,
    /// Fill rule and packed 0xAARRGGBB colour.
    pub fill: Option<(FillRule, u32)>,
    /// Stroke width (user space) and packed 0xAARRGGBB colour.
    pub stroke: Option<(f32, u32)>,
}

/// A parsed SVG document.
#[derive(Debug, Clone)]
pub struct SvgDoc {
    /// Source width from `tree.size()`.
    pub width: f32,
    /// Source height from `tree.size()`.
    pub height: f32,
    /// Shapes in document order (later shapes paint on top).
    pub shapes: Vec<SvgShape>,
    /// How many paints were a gradient and had to be flattened to one colour.
    pub flattened_gradients: usize,
    /// How many things were dropped entirely, with why.
    pub skipped: Vec<String>,
}

/// One mesh plus the flat colour it is drawn with, in draw order.
#[derive(Debug, Clone)]
pub struct SvgMesh {
    /// Triangle vertices in fitted pixel space (origin top-left, see module docs).
    pub positions: Vec<[f32; 2]>,
    /// Triangle indices into `positions`.
    pub indices: Vec<u32>,
    /// Packed 0xAARRGGBB colour for the whole mesh.
    pub argb: u32,
}

/// Parse SVG bytes into vector shapes.
///
/// Returns `Err` for input that is not UTF-8 or not a parsable SVG. A valid
/// but empty document yields a doc with no shapes. Never panics.
pub fn parse_svg(bytes: &[u8]) -> Result<SvgDoc, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("SVG is not valid UTF-8: {e}"))?;
    let tree = usvg::Tree::from_str(text, &usvg::Options::default())
        .map_err(|e| format!("SVG parse error: {e}"))?;

    let size = tree.size();
    let mut doc = SvgDoc {
        width: size.width(),
        height: size.height(),
        shapes: Vec::new(),
        flattened_gradients: 0,
        skipped: Vec::new(),
    };
    walk_group(tree.root(), &mut doc);
    Ok(doc)
}

/// Walk a group's children in document order, recording unsupported group state.
fn walk_group(group: &Group, out: &mut SvgDoc) {
    // Group opacity cannot be expressed by the per-mesh flat-colour engine.
    // Record it once, then still descend: the shapes are drawn opaque.
    let opacity = group.opacity().get();
    if opacity < 1.0 {
        out.skipped.push(format!(
            "group opacity {opacity} not supported by the flat-colour engine; \
             its shapes are drawn opaque"
        ));
    }
    if group.clip_path().is_some() {
        out.skipped
            .push("clip-path ignored: engine has no clipping".to_string());
    }
    if group.mask().is_some() {
        out.skipped
            .push("mask ignored: engine has no masking".to_string());
    }
    if !group.filters().is_empty() {
        out.skipped
            .push("filter ignored: engine has no SVG filter pipeline".to_string());
    }
    if !matches!(group.blend_mode(), usvg::BlendMode::Normal) {
        out.skipped.push(format!(
            "mix-blend-mode {:?} ignored: engine has no SVG blend modes",
            group.blend_mode()
        ));
    }

    for child in group.children() {
        match child {
            Node::Group(g) => walk_group(g, out),
            Node::Path(p) => add_path(p, out),
            Node::Image(_) => out
                .skipped
                .push("image element skipped: raster images are not vector geometry".to_string()),
            Node::Text(_) => out
                .skipped
                .push("text element skipped: text belongs to the engine's text layer".to_string()),
        }
    }
}

/// Convert one `usvg` path into an [`SvgShape`], if it draws anything.
fn add_path(p: &usvg::Path, out: &mut SvgDoc) {
    if !p.is_visible() {
        out.skipped
            .push("invisible path skipped (visibility:hidden)".to_string());
        return;
    }

    let fill = p.fill().and_then(|f| {
        resolve_paint(f.paint(), f.opacity().get(), "fill", out)
            .map(|argb| (to_fill_rule(f.rule()), argb))
    });
    let stroke = p.stroke().and_then(|s| {
        resolve_paint(s.paint(), s.opacity().get(), "stroke", out)
            .map(|argb| (s.width().get(), argb))
    });

    // `fill="none" stroke="none"` (or a paint we refused) draws nothing. This is
    // normal, not an error, so it is not recorded.
    if fill.is_none() && stroke.is_none() {
        return;
    }

    out.shapes.push(SvgShape {
        path: to_kurbo(p.data()),
        transform: ts_to_kurbo(p.abs_transform()),
        fill,
        stroke,
    });
}

/// Convert `tiny_skia_path::Path` segments into a `kurbo::BezPath`.
///
/// `tiny_skia_path::PathSegment` has exactly five variants and every one has a
/// direct `kurbo` counterpart, so no segment can be silently lost.
fn to_kurbo(path: &usvg::tiny_skia_path::Path) -> BezPath {
    let mut out = BezPath::new();
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => out.move_to((p.x as f64, p.y as f64)),
            PathSegment::LineTo(p) => out.line_to((p.x as f64, p.y as f64)),
            PathSegment::QuadTo(p1, p) => {
                out.quad_to((p1.x as f64, p1.y as f64), (p.x as f64, p.y as f64))
            }
            PathSegment::CubicTo(p1, p2, p) => out.curve_to(
                (p1.x as f64, p1.y as f64),
                (p2.x as f64, p2.y as f64),
                (p.x as f64, p.y as f64),
            ),
            PathSegment::Close => out.close_path(),
        }
    }
    out
}

/// Map `tiny_skia_path::Transform` to `kurbo::Affine`.
///
/// Both are 2×3 affines but their coefficient order differs. `tiny-skia` maps a
/// point as `x' = x*sx + y*kx + tx`, `y' = x*ky + y*sy + ty`; `kurbo`'s
/// `[a, b, c, d, e, f]` maps `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
/// Matching coefficients therefore gives `a = sx`, `b = ky`, `c = kx`,
/// `d = sy`, `e = tx`, `f = ty` — note `b`/`c` swap.
fn ts_to_kurbo(t: Transform) -> Affine {
    Affine::new([
        t.sx as f64,
        t.ky as f64,
        t.kx as f64,
        t.sy as f64,
        t.tx as f64,
        t.ty as f64,
    ])
}

/// Map `usvg`'s fill rule onto `lyon`'s.
fn to_fill_rule(rule: usvg::FillRule) -> FillRule {
    match rule {
        usvg::FillRule::NonZero => FillRule::NonZero,
        usvg::FillRule::EvenOdd => FillRule::EvenOdd,
    }
}

/// Resolve a paint to a packed 0xAARRGGBB colour, folding in `opacity`.
///
/// Gradients are flattened (and counted); patterns are refused and recorded.
fn resolve_paint(paint: &Paint, opacity: f32, what: &str, out: &mut SvgDoc) -> Option<u32> {
    let opacity = opacity.clamp(0.0, 1.0);
    match paint {
        Paint::Color(c) => Some(pack(
            c.red,
            c.green,
            c.blue,
            (255.0 * opacity).round() as u32,
        )),
        Paint::LinearGradient(g) => {
            out.flattened_gradients += 1;
            let (r, gg, b, a) = flatten_stops(g.stops());
            Some(pack(r, gg, b, (255.0 * a * opacity).round() as u32))
        }
        Paint::RadialGradient(g) => {
            out.flattened_gradients += 1;
            let (r, gg, b, a) = flatten_stops(g.stops());
            Some(pack(r, gg, b, (255.0 * a * opacity).round() as u32))
        }
        Paint::Pattern(_) => {
            out.skipped.push(format!(
                "{what}: pattern paint server is not supported (engine fills with a flat colour only)"
            ));
            None
        }
    }
}

/// Flatten gradient stops to one sRGB8 colour plus an alpha in `[0, 1]`.
///
/// See the module docs for the weighting rule: trapezoidal offset-interval
/// weights, averaged in linear-light sRGB.
fn flatten_stops(stops: &[Stop]) -> (u8, u8, u8, f32) {
    if stops.is_empty() {
        // A gradient with no stops is invalid SVG; paint it black rather than
        // dropping the shape, so the geometry is still visible for debugging.
        return (0, 0, 0, 1.0);
    }
    if stops.len() == 1 {
        let s = &stops[0];
        return (
            s.color().red,
            s.color().green,
            s.color().blue,
            s.opacity().get(),
        );
    }

    let offsets: Vec<f64> = stops.iter().map(|s| s.offset().get() as f64).collect();
    let n = stops.len();
    let mut weights: Vec<f64> = (0..n)
        .map(|i| {
            let w = if i == 0 {
                (offsets[1] - offsets[0]) / 2.0
            } else if i == n - 1 {
                (offsets[n - 1] - offsets[n - 2]) / 2.0
            } else {
                (offsets[i + 1] - offsets[i - 1]) / 2.0
            };
            w.max(0.0)
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        for w in &mut weights {
            *w /= total;
        }
    } else {
        // All offsets equal: fall back to a uniform mean.
        let uniform = 1.0 / n as f64;
        weights.iter_mut().for_each(|w| *w = uniform);
    }

    let mut r = 0.0f64;
    let mut g = 0.0f64;
    let mut b = 0.0f64;
    let mut a = 0.0f64;
    for (stop, &w) in stops.iter().zip(&weights) {
        let c = stop.color();
        r += w * srgb_to_linear(c.red as f32 / 255.0);
        g += w * srgb_to_linear(c.green as f32 / 255.0);
        b += w * srgb_to_linear(c.blue as f32 / 255.0);
        a += w * stop.opacity().get() as f64;
    }
    (
        to_u8(linear_to_srgb(r as f32) * 255.0),
        to_u8(linear_to_srgb(g as f32) * 255.0),
        to_u8(linear_to_srgb(b as f32) * 255.0),
        a as f32,
    )
}

/// sRGB transfer function (IEC 61966-2-1), encoded → linear.
fn srgb_to_linear(v: f32) -> f64 {
    let c = v.clamp(0.0, 1.0) as f64;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function, linear → encoded.
fn linear_to_srgb(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0) as f64;
    let out = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    out as f32
}

/// Round a channel to 0..=255.
fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Pack sRGB8 + alpha into the engine's 0xAARRGGBB.
fn pack(r: u8, g: u8, b: u8, a: u32) -> u32 {
    (a.min(255) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Tessellate every shape into meshes sized to `size_px`, in draw order.
///
/// Geometry is fitted as described in the module docs (origin top-left, larger
/// source dimension = `size_px`), then each shape's own transform is applied.
/// A shape with both fill and stroke emits **two** meshes, **fill first** then
/// stroke, matching SVG's default `paint-order: fill stroke` (the reverse order
/// is not honoured, see module docs). Stroke caps and joins are round.
///
/// A degenerate `size_px` (0, negative, NaN, infinite) or a doc with no usable
/// aspect returns an empty vec. A shape whose tessellation yields no triangles
/// is omitted.
///
/// Note: this API has no diagnostics channel, so a shape that tessellates to
/// nothing is dropped silently; callers that care can compare the number of
/// painted shapes with the number of produced meshes.
pub fn svg_meshes(doc: &SvgDoc, size_px: f32) -> Vec<SvgMesh> {
    if !size_px.is_finite() || size_px <= 0.0 {
        return Vec::new();
    }
    let (w, h) = (doc.width, doc.height);
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let fit = (size_px / w.max(h)) as f64;
    let fit_affine = Affine::scale(fit);
    let tol = (size_px / 256.0).max(1e-4);

    let mut meshes = Vec::new();
    for shape in &doc.shapes {
        // Scale the geometry first, then the shape's own absolute transform.
        let combined = fit_affine * shape.transform;
        let device = combined * &shape.path;
        let events = lyon_events(&device, tol as f64);
        if events.is_empty() {
            continue;
        }

        if let Some((rule, argb)) = shape.fill {
            if let Some(mesh) = fill_mesh(&events, rule, argb, tol) {
                meshes.push(mesh);
            }
        }
        if let Some((width, argb)) = shape.stroke {
            // A single width cannot represent a non-uniform transform; use the
            // geometric-mean scale sqrt(|det|), which is exact for uniform scale.
            let scale = combined.determinant().abs().sqrt() as f32;
            if let Some(mesh) = stroke_mesh(&events, width * scale, argb, tol) {
                meshes.push(mesh);
            }
        }
    }
    meshes
}

/// Flatten a `kurbo` path into lyon `PathEvent`s in device space.
///
/// Mirrors the house pattern in `tess.rs`: flatten curves to polylines with
/// `tol`, then rebuild sub-path events. `tol` is in device pixels.
fn lyon_events(path: &BezPath, tol: f64) -> Vec<PathEvent> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut flat: Vec<PathEl> = Vec::new();
    kurbo::flatten(path.elements().iter().cloned(), tol, |el| flat.push(el));

    let mut events: Vec<PathEvent> = Vec::new();
    let mut first: Option<LyonPoint> = None;
    let mut current: Option<LyonPoint> = None;
    for el in flat {
        match el {
            PathEl::MoveTo(p) => {
                if let (Some(f), Some(c)) = (first, current) {
                    events.push(PathEvent::End {
                        last: c,
                        first: f,
                        close: false,
                    });
                }
                let at = LyonPoint::new(p.x as f32, p.y as f32);
                events.push(PathEvent::Begin { at });
                first = Some(at);
                current = Some(at);
            }
            PathEl::LineTo(p) => {
                let to = LyonPoint::new(p.x as f32, p.y as f32);
                match current {
                    Some(from) => events.push(PathEvent::Line { from, to }),
                    None => {
                        events.push(PathEvent::Begin { at: to });
                        first = Some(to);
                    }
                }
                current = Some(to);
            }
            PathEl::ClosePath => {
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
            // `kurbo::flatten` never emits quad/cubic; if it ever did, dropping
            // them here would be a bug, so treat them as a line to their end.
            PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => {
                let to = LyonPoint::new(p.x as f32, p.y as f32);
                match current {
                    Some(from) => events.push(PathEvent::Line { from, to }),
                    None => {
                        events.push(PathEvent::Begin { at: to });
                        first = Some(to);
                    }
                }
                current = Some(to);
            }
        }
    }
    if let (Some(f), Some(c)) = (first, current) {
        events.push(PathEvent::End {
            last: c,
            first: f,
            close: false,
        });
    }
    events
}

/// Fill-tessellate `events` with the shape's rule; `None` if nothing is produced.
fn fill_mesh(events: &[PathEvent], rule: FillRule, argb: u32, tol: f32) -> Option<SvgMesh> {
    let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    {
        let mut builder = BuffersBuilder::new(&mut buffers, |v: FillVertex| {
            let p = v.position();
            [p.x, p.y]
        });
        let opts = FillOptions::tolerance(tol).with_fill_rule(rule);
        FillTessellator::new()
            .tessellate(events.iter().copied(), &opts, &mut builder)
            .ok()?;
    }
    if buffers.indices.is_empty() {
        return None;
    }
    Some(SvgMesh {
        positions: buffers.vertices,
        indices: buffers.indices,
        argb,
    })
}

/// Stroke-tessellate `events` with round caps/joins; `None` if nothing is produced.
fn stroke_mesh(events: &[PathEvent], width: f32, argb: u32, tol: f32) -> Option<SvgMesh> {
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    {
        let mut builder = BuffersBuilder::new(&mut buffers, |v: StrokeVertex| {
            let p = v.position();
            [p.x, p.y]
        });
        let mut opts = StrokeOptions::default();
        opts.line_width = width;
        opts.start_cap = LineCap::Round;
        opts.end_cap = LineCap::Round;
        opts.line_join = LineJoin::Round;
        opts.tolerance = tol;
        StrokeTessellator::new()
            .tessellate(events.iter().copied(), &opts, &mut builder)
            .ok()?;
    }
    if buffers.indices.is_empty() {
        return None;
    }
    Some(SvgMesh {
        positions: buffers.vertices,
        indices: buffers.indices,
        argb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> SvgDoc {
        parse_svg(src.as_bytes()).expect("expected a valid SVG")
    }

    fn approx(a: Affine, b: Affine) -> bool {
        a.as_coeffs()
            .iter()
            .zip(b.as_coeffs().iter())
            .all(|(x, y)| (x - y).abs() < 1e-4)
    }

    /// (min_x, min_y, max_x, max_y) over a mesh's vertices.
    fn bounds(mesh: &SvgMesh) -> (f32, f32, f32, f32) {
        let mut b = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for p in &mesh.positions {
            b.0 = b.0.min(p[0]);
            b.1 = b.1.min(p[1]);
            b.2 = b.2.max(p[0]);
            b.3 = b.3.max(p[1]);
        }
        b
    }

    /// 1. `fill-rule` is read per shape; `evenodd` is not defaulted away.
    #[test]
    fn fill_rule_is_honoured_not_defaulted() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <path d="M0 0 L10 0 L10 10 L0 10 Z" fill="#ff0000" fill-rule="evenodd"/>
                 <path d="M0 0 L10 0 L10 10 Z" fill="#00ff00"/>
               </svg>"##,
        );
        assert_eq!(doc.shapes.len(), 2);
        assert_eq!(doc.shapes[0].fill, Some((FillRule::EvenOdd, 0xFFFF0000)));
        // SVG's default rule is NonZero; it must not be flipped to EvenOdd.
        assert_eq!(doc.shapes[1].fill, Some((FillRule::NonZero, 0xFF00FF00)));
    }

    /// 2. A group transform is composed into the path's absolute transform.
    #[test]
    fn group_transform_reaches_the_shape() {
        let doc = parse(
            r##"<svg width="100" height="100">
                 <g transform="translate(10,20) scale(2)">
                   <rect x="0" y="0" width="10" height="10" fill="#000000"/>
                 </g>
               </svg>"##,
        );
        assert_eq!(doc.shapes.len(), 1);
        // translate then scale => x' = 2x + 10, y' = 2y + 20.
        let expected = Affine::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]);
        assert!(
            approx(doc.shapes[0].transform, expected),
            "got {:?}",
            doc.shapes[0].transform.as_coeffs()
        );
    }

    /// 3. `fill="none"` yields no fill, and the stroke width is carried through.
    #[test]
    fn stroke_only_shape_has_no_fill() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <path d="M0 0 L10 10" fill="none" stroke="#00ff00" stroke-width="3"/>
               </svg>"##,
        );
        assert_eq!(doc.shapes.len(), 1);
        assert_eq!(doc.shapes[0].fill, None);
        assert_eq!(doc.shapes[0].stroke, Some((3.0, 0xFF00FF00)));
    }

    /// 4. A linear gradient is flattened, counted, and equal to the documented mean.
    #[test]
    fn linear_gradient_is_flattened_and_counted() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <defs>
                   <linearGradient id="g" x1="0" y1="0" x2="1" y2="0">
                     <stop offset="0" stop-color="#ff0000"/>
                     <stop offset="1" stop-color="#0000ff"/>
                   </linearGradient>
                 </defs>
                 <rect width="10" height="10" fill="url(#g)"/>
               </svg>"##,
        );
        assert_eq!(doc.flattened_gradients, 1);
        assert_eq!(doc.shapes.len(), 1);
        let (_, argb) = doc.shapes[0].fill.expect("gradient flattened to a fill");
        let a = argb >> 24;
        let r = (argb >> 16) & 0xff;
        let g = (argb >> 8) & 0xff;
        let b = argb & 0xff;
        // Red and blue have equal trapezoid weights, so the linear-light mean is
        // (0.5, 0, 0.5); back in sRGB that is ~188 per channel.
        assert_eq!(a, 255);
        assert_eq!(g, 0);
        assert_eq!(r, b);
        assert!((r as i32 - 188).abs() <= 1, "red channel was {r}");
    }

    /// 5. A `pattern` fill is refused and recorded, not faked, and does not panic.
    #[test]
    fn pattern_fill_is_skipped() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <defs>
                   <pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse">
                     <rect width="2" height="2" fill="#000000"/>
                   </pattern>
                 </defs>
                 <rect width="10" height="10" fill="url(#p)"/>
               </svg>"##,
        );
        assert!(
            doc.skipped.iter().any(|s| s.contains("pattern")),
            "skipped was {:?}",
            doc.skipped
        );
        // The rect's only paint was the pattern, so there is nothing to draw.
        assert!(doc.shapes.is_empty());
    }

    /// 6. Malformed input returns `Err`, never panics.
    #[test]
    fn malformed_input_errors() {
        for bad in ["<svg", "", "not xml"] {
            assert!(parse_svg(bad.as_bytes()).is_err(), "accepted {bad:?}");
        }
        // Invalid UTF-8 also errors rather than panicking.
        assert!(parse_svg(&[0xff, 0xfe, 0x00]).is_err());
    }

    /// 7. A valid but empty document parses to zero shapes.
    #[test]
    fn empty_svg_has_no_shapes() {
        let doc = parse("<svg/>");
        assert!(doc.shapes.is_empty());
        assert!(doc.skipped.is_empty());
        assert_eq!(doc.flattened_gradients, 0);
    }

    /// 8. A filled rect tessellates and lands inside the `size_px` box.
    #[test]
    fn filled_rect_tessellates_inside_the_box() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <rect x="2" y="2" width="6" height="6" fill="#ff0000"/>
               </svg>"##,
        );
        let meshes = svg_meshes(&doc, 100.0);
        assert_eq!(meshes.len(), 1);
        assert!(!meshes[0].indices.is_empty());
        assert_eq!(meshes[0].indices.len() % 3, 0);
        let (minx, miny, maxx, maxy) = bounds(&meshes[0]);
        // Rect 2..8 scaled by 100/10 => 20..80 in both axes.
        assert!(minx >= -0.01 && miny >= -0.01, "({minx},{miny})");
        assert!(maxx <= 100.01 && maxy <= 100.01, "({maxx},{maxy})");
        assert!((minx - 20.0).abs() < 0.01 && (maxx - 80.0).abs() < 0.01);
    }

    /// 9. Fill + stroke emits two meshes, fill first.
    #[test]
    fn fill_and_stroke_emit_two_meshes_fill_first() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <rect width="10" height="10" fill="#ff0000" stroke="#0000ff" stroke-width="1"/>
               </svg>"##,
        );
        let meshes = svg_meshes(&doc, 100.0);
        assert_eq!(meshes.len(), 2, "expected fill then stroke");
        assert_eq!(meshes[0].argb, 0xFFFF0000);
        assert_eq!(meshes[1].argb, 0xFF0000FF);
        assert!(!meshes[0].indices.is_empty() && !meshes[1].indices.is_empty());
    }

    /// 10. A degenerate `size_px` yields no meshes, without panicking.
    #[test]
    fn degenerate_size_yields_no_meshes() {
        let doc = parse(
            r##"<svg width="10" height="10">
                 <rect width="10" height="10" fill="#ff0000"/>
               </svg>"##,
        );
        assert!(svg_meshes(&doc, 0.0).is_empty());
        assert!(svg_meshes(&doc, -5.0).is_empty());
        assert!(svg_meshes(&doc, f32::NAN).is_empty());
        assert!(svg_meshes(&doc, f32::INFINITY).is_empty());
    }

    /// 11. A non-unit viewBox aspect is fitted preserving aspect.
    #[test]
    fn non_unit_aspect_is_preserved() {
        let doc = parse(
            r##"<svg viewBox="0 0 200 100">
                 <rect x="0" y="0" width="200" height="100" fill="#000000"/>
               </svg>"##,
        );
        let meshes = svg_meshes(&doc, 300.0);
        assert_eq!(meshes.len(), 1);
        let (minx, miny, maxx, maxy) = bounds(&meshes[0]);
        let (w, h) = (maxx - minx, maxy - miny);
        // 200x100 fitted so the larger (200) side is 300 => 300x150, aspect 2.
        assert!((w - 300.0).abs() < 0.5, "width {w}");
        assert!((h - 150.0).abs() < 0.5, "height {h}");
        assert!((w / h - 2.0).abs() < 0.01, "aspect {}", w / h);
    }
}

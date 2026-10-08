// SPDX-License-Identifier: Apache-2.0

use kurbo::{PathEl, Shape};
use lyon_tessellation::path::PathEvent;
use lyon_tessellation::path::math::Point;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};

/// Fill-tessellates a Bézier path and returns the number of triangles.
///
/// The path is first flattened to polylines with tolerance `size / 256`,
/// where `size` is the longest side of the path's own bounding box (the
/// signature carries no params, so the tolerance is derived from the path).
/// An empty or degenerate path yields 0, as does a tessellator failure.
pub fn tessellate(path: &kurbo::BezPath) -> usize {
    if path.is_empty() {
        return 0;
    }
    let bounds = path.bounding_box();
    let size = bounds.width().max(bounds.height());
    if !(size > 0.0) {
        return 0;
    }
    let tol = size / 256.0;

    let mut flat: Vec<PathEl> = Vec::new();
    kurbo::flatten(path.elements().iter().cloned(), tol, |el| {
        flat.push(el);
    });

    let mut events: Vec<PathEvent> = Vec::new();
    let mut first: Option<Point> = None;
    let mut current: Option<Point> = None;
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
                let at = Point::new(p.x as f32, p.y as f32);
                events.push(PathEvent::Begin { at });
                first = Some(at);
                current = Some(at);
            }
            PathEl::LineTo(p) => {
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
        return 0;
    }

    let mut buffers: VertexBuffers<Point, u32> = VertexBuffers::new();
    {
        let mut builder = BuffersBuilder::new(&mut buffers, |v: FillVertex| v.position());
        let mut tess = FillTessellator::new();
        if tess
            .tessellate(events, &FillOptions::default(), &mut builder)
            .is_err()
        {
            return 0;
        }
    }
    buffers.indices.len() / 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShapeKind;
    use crate::params::ShapeParams;
    use crate::path::shape_path;

    #[test]
    fn circle_tessellates_non_empty() {
        let path = shape_path(ShapeKind::Circle, ShapeParams::default()).unwrap();
        assert!(tessellate(&path) > 0);
    }

    /// More rounding cuts more of the corner off, so the square gets *smaller*.
    ///
    /// The old assertion here was that rounding adds triangles, which was true of
    /// the hand-rolled geometry this project used to have and is not true of
    /// Material's: a fully rounded corner is one arc, and it tessellates to fewer,
    /// larger triangles than a sharp corner's two. Area is the property that
    /// actually means "rounder", and it holds for either implementation.
    #[test]
    fn square_rounding_cuts_the_corners_off() {
        use kurbo::Shape as _;
        let base = shape_path(ShapeKind::Square, ShapeParams::default()).unwrap();
        let round = shape_path(
            ShapeKind::Square,
            ShapeParams {
                corner_rounding: 0.25,
                ..ShapeParams::default()
            },
        )
        .unwrap();
        let (base_area, round_area) = (base.area(), round.area());
        assert!(base_area > 0.0 && round_area > 0.0);
        assert!(
            round_area < base_area,
            "rounding must remove area: {round_area} vs {base_area}"
        );
        assert!(tessellate(&base) > 0 && tessellate(&round) > 0);
    }
}

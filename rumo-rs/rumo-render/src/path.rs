// SPDX-License-Identifier: Apache-2.0

use crate::ShapeKind;
use crate::params::ShapeParams;
use kurbo::{BezPath, Point};

/// Builds real 2D geometry for a shape.
///
/// The geometry itself is **Material 3's own**, ported in [`crate::m3shape`]: this
/// function only places it. Everything before that port was an approximation —
/// `cookie9sided` was a plain 9-gon, `sunny` a 12-ray star with invented valleys,
/// and every corner was sharp at the default rounding. Material's shapes are
/// specific curves with specific parameters, and the reference file they come
/// from is named in the module docs.
///
/// The shape arrives normalised into the unit box, so placing it is: move the
/// centre to the origin, scale by the requested size, rotate. One transform for
/// every shape instead of one per family, which is also why the rotation is now
/// the *same* rotation for all of them.
pub fn shape_path(kind: ShapeKind, p: ShapeParams) -> Option<BezPath> {
    let p = p.validated()?;
    let size = f64::from(p.size_px);
    let angle = f64::from(p.rotation_deg).to_radians();
    let (sin, cos) = (angle.sin(), angle.cos());
    let place = |x: f32, y: f32| {
        let (x, y) = (f64::from(x) - 0.5, f64::from(y) - 0.5);
        Point::new((x * cos - y * sin) * size, (x * sin + y * cos) * size)
    };

    let shape = crate::m3shape::shape(kind, p.corner_rounding);
    let mut path = BezPath::new();
    let mut started = false;
    for c in &shape.cubics {
        let p0 = place(c.p[0], c.p[1]);
        let c0 = place(c.p[2], c.p[3]);
        let c1 = place(c.p[4], c.p[5]);
        let p1 = place(c.p[6], c.p[7]);
        if !started {
            path.move_to(p0);
            started = true;
        }
        // Cubic curves go straight into the path: lyon flattens them at
        // tolerance when the mesh is built, so the curve survives to the
        // tessellator instead of being approximated here.
        path.curve_to(c0, c1, p1);
    }
    path.close_path();
    Some(path)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_35_shapes_tessellate_non_empty() {
        use crate::{all_shapes, tess::tessellate};
        let p = ShapeParams::default();
        for kind in all_shapes() {
            let path = shape_path(kind, p).unwrap_or_else(|| panic!("{kind:?} produced no path"));
            let tris = tessellate(&path);
            assert!(tris > 0, "{kind:?} tessellated to 0 triangles");
        }
    }

    /// The soft variants are their own shapes in Material's table, not a
    /// shallower version of the hard one — so what is checked is that they are
    /// *different shapes*, which is the thing the port must not collapse.
    #[test]
    fn soft_variants_are_their_own_shapes() {
        use crate::tess::tessellate;
        use kurbo::Shape as _;
        let p = ShapeParams::default();
        for (hard_kind, soft_kind) in [
            (ShapeKind::Boom, ShapeKind::SoftBoom),
            (ShapeKind::Burst, ShapeKind::SoftBurst),
        ] {
            let hard = shape_path(hard_kind, p).unwrap();
            let soft = shape_path(soft_kind, p).unwrap();
            assert!(tessellate(&hard) > 0 && tessellate(&soft) > 0);
            let (a, b) = (hard.area(), soft.area());
            let relative = (a - b).abs() / a.max(b);
            assert!(
                relative > 0.01,
                "{soft_kind:?} and {hard_kind:?} must differ: {a} vs {b}"
            );
        }
    }

    /// Every shape must be *curved* somewhere: Material's shapes are rounded by
    /// construction, and a port that dropped the rounding would produce polygons
    /// that still tessellate and still look plausible in a test that only counts
    /// triangles.
    #[test]
    fn every_shape_carries_curves() {
        use crate::{all_shapes, m3shape};
        for kind in all_shapes() {
            // The pixel shapes are deliberately stepped slabs, so they are the
            // two that may be all lines.
            if matches!(kind, ShapeKind::PixelCircle | ShapeKind::PixelTriangle) {
                continue;
            }
            let shape = m3shape::shape(kind, 0.0);
            let curved = shape
                .cubics
                .iter()
                .filter(|c| {
                    let (x0, y0) = (c.p[0], c.p[1]);
                    let (x1, y1) = (c.p[6], c.p[7]);
                    ((c.p[2] - x0) * (y1 - y0) - (c.p[3] - y0) * (x1 - x0)).abs() > 1e-5
                })
                .count();
            assert!(curved > 0, "{kind:?} has no curves at all");
        }
    }

    #[test]
    fn invalid_params_yield_none_path() {
        let bad = ShapeParams {
            size_px: 0.0,
            ..ShapeParams::default()
        };
        assert!(shape_path(ShapeKind::Circle, bad).is_none());
        let bad = ShapeParams {
            corner_rounding: 0.9,
            ..ShapeParams::default()
        };
        assert!(shape_path(ShapeKind::Square, bad).is_none());
    }
}

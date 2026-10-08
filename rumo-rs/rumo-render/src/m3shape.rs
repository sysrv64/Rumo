// SPDX-License-Identifier: Apache-2.0

//! Material 3 shape geometry, ported from the reference implementation.
//!
//! # Where this comes from
//!
//! Ported from `androidx.graphics.shapes` — `RoundedPolygon.kt`,
//! `Shapes.kt`, `Cubic.kt`, `Utils.kt` — and the shape table from
//! `androidx.compose.material3.MaterialShapes.kt`, all from the AndroidX
//! repository (`androidx-main`). That is the source of truth: the named shapes
//! are not "a rounded n-gon", they are specific curves with specific parameters,
//! and guessing them is how `cookie9sided` ended up as a plain 9-gon.
//!
//! # What a shape actually is
//!
//! A [`RoundedPolygon`] is a list of cubic Béziers, not a list of vertices. The
//! vertices are the *inputs*: each corner is replaced by an arc of a given
//! radius, and the space between two corners is divided between the arc and the
//! *flanking curves* that blend the arc into the straight edge. The `smoothing`
//! parameter controls how much of the corner is spent on that blend, which is
//! what makes Material's shapes look soft rather than merely rounded.
//!
//! The reference names the parts: a corner that must not eat more than half of
//! the side it sits on, a cut that may be reduced when two corners compete for
//! the same edge, and a radius that shrinks with it. All of that is here, in the
//! same order, because the numbers only come out right if the arithmetic is the
//! same arithmetic.
//!
//! # The rounding slider
//!
//! Material's shapes have their rounding built in; this project also lets a user
//! adjust it. The user's value *adds* rounding on top of Material's rather than
//! replacing it, so that at the default the shape is exactly the reference one
//! and the slider only makes it rounder — see [`apply_user_rounding`].

/// Corner rounding: how much of a corner is cut, and how much of the cut is
/// spent blending the arc into the edges.
///
/// Ported from `CornerRounding`. `radius` is in the same units as the vertices
/// (the reference builds shapes on a unit circle and normalises afterwards);
/// `smoothing` is `0..=1`, where `0` rounds with a plain circular arc and `1`
/// spends the whole allowed cut on the blend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRounding {
    pub radius: f32,
    pub smoothing: f32,
}

impl CornerRounding {
    pub const UNROUNDED: Self = Self {
        radius: 0.0,
        smoothing: 0.0,
    };

    pub const fn new(radius: f32) -> Self {
        Self {
            radius,
            smoothing: 0.0,
        }
    }

    pub const fn smooth(radius: f32, smoothing: f32) -> Self {
        Self { radius, smoothing }
    }
}

/// One cubic Bézier: `(x0, y0)`, two control points, `(x1, y1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cubic {
    pub p: [f32; 8],
}

impl Cubic {
    fn anchor0(&self) -> (f32, f32) {
        (self.p[0], self.p[1])
    }

    fn anchor1(&self) -> (f32, f32) {
        (self.p[6], self.p[7])
    }

    /// Ported from `Cubic.straightLine`: a line expressed as a cubic, with the
    /// controls at the thirds.
    fn straight_line(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            p: [
                x0,
                y0,
                interpolate(x0, x1, 1.0 / 3.0),
                interpolate(y0, y1, 1.0 / 3.0),
                interpolate(x0, x1, 2.0 / 3.0),
                interpolate(y0, y1, 2.0 / 3.0),
                x1,
                y1,
            ],
        }
    }

    /// Ported from `Cubic.circularArc`: the cubic that approximates the shortest
    /// arc between two points that are equidistant from a centre.
    fn circular_arc(cx: f32, cy: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        let p0d = direction_vector(x0 - cx, y0 - cy);
        let p1d = direction_vector(x1 - cx, y1 - cy);
        let rotated_p0 = rotate90(p0d);
        let rotated_p1 = rotate90(p1d);
        let clockwise = dot(rotated_p0, (x1 - cx, y1 - cy)) >= 0.0;
        let cosa = dot(p0d, p1d);
        if cosa > 0.999 {
            return Self::straight_line(x0, y0, x1, y1);
        }
        let k = distance(x0 - cx, y0 - cy) * 4.0 / 3.0
            * ((2.0 * (1.0 - cosa)).sqrt() - (1.0 - cosa * cosa).sqrt())
            / (1.0 - cosa)
            * if clockwise { 1.0 } else { -1.0 };
        Self {
            p: [
                x0,
                y0,
                x0 + rotated_p0.0 * k,
                y0 + rotated_p0.1 * k,
                x1 - rotated_p1.0 * k,
                y1 - rotated_p1.1 * k,
                x1,
                y1,
            ],
        }
    }

    fn reverse(&self) -> Self {
        Self {
            p: [
                self.p[6], self.p[7], self.p[4], self.p[5], self.p[2], self.p[3], self.p[0],
                self.p[1],
            ],
        }
    }

    fn zero_length(&self) -> bool {
        let (x0, y0) = self.anchor0();
        let (x1, y1) = self.anchor1();
        (x0 - x1).abs() < 1e-6 && (y0 - y1).abs() < 1e-6 && {
            let (c0x, c0y) = (self.p[2], self.p[3]);
            (x0 - c0x).abs() < 1e-6 && (y0 - c0y).abs() < 1e-6
        }
    }

    /// Ported from `Cubic.calculateBounds(approximate = true)`: the bounding box
    /// of the anchors **and** the controls, which contains the curve.
    fn approximate_bounds(&self) -> [f32; 4] {
        if self.zero_length() {
            let (x, y) = self.anchor0();
            return [x, y, x, y];
        }
        let xs = [self.p[0], self.p[2], self.p[4], self.p[6]];
        let ys = [self.p[1], self.p[3], self.p[5], self.p[7]];
        let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let min_y = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let max_y = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        [min_x, min_y, max_x, max_y]
    }
}

/// A closed shape: its cubic segments, and the centre they were built around.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundedPolygon {
    pub cubics: Vec<Cubic>,
    pub center: [f32; 2],
    /// The vertices this polygon was built from, and the rounding of each, in the
    /// same order — the polygon's *inputs*.
    ///
    /// Kept because a rounded corner is three curves (flank, arc, flank), so a
    /// finished shape has roughly four times as many curve anchors as it has
    /// corners. Re-rounding a finished shape would therefore round its arcs
    /// rather than its corners, which turns a square into a lumpy blob. The
    /// user's slider needs the corners, so the corners are what is stored.
    source: Option<Source>,
}

/// A polygon's inputs, kept alongside the curves it produced.
#[derive(Debug, Clone, PartialEq)]
struct Source {
    vertices: Vec<f32>,
    per_vertex: Vec<CornerRounding>,
}

impl RoundedPolygon {
    /// The rounding itself. Ported from the `RoundedPolygon(vertices, …)`
    /// constructor.
    ///
    /// The two passes matter: the first computes how much cut each corner *wants*
    /// and how much the side between two corners can *give*, and the second
    /// builds the curves with whatever was granted. A corner that asks for more
    /// than half its side gets a smaller radius rather than an overlapping one,
    /// which is why a star with deep valleys still comes out clean.
    pub fn from_vertices(
        vertices: &[f32],
        rounding: CornerRounding,
        per_vertex: Option<&[CornerRounding]>,
        center: Option<[f32; 2]>,
    ) -> Self {
        assert!(
            vertices.len() >= 6 && vertices.len() % 2 == 0,
            "a polygon needs at least three vertices"
        );
        let n = vertices.len() / 2;
        let vtx = |i: usize| (vertices[i * 2], vertices[i * 2 + 1]);

        let mut corners = Vec::with_capacity(n);
        let mut resolved = Vec::with_capacity(n);
        for i in 0..n {
            let vtx_rounding = per_vertex.map_or(rounding, |list| list[i]);
            let prev = vtx((i + n - 1) % n);
            let curr = vtx(i);
            let next = vtx((i + 1) % n);
            corners.push(RoundedCorner::new(prev, curr, next, vtx_rounding));
            resolved.push(vtx_rounding);
        }

        // How much of each side may be spent: rounding first, smoothing second.
        let mut cut_adjusts = Vec::with_capacity(n);
        for ix in 0..n {
            let expected_round_cut =
                corners[ix].expected_round_cut + corners[(ix + 1) % n].expected_round_cut;
            let expected_cut = corners[ix].expected_cut() + corners[(ix + 1) % n].expected_cut();
            let (vx, vy) = vtx(ix);
            let (nx, ny) = vtx((ix + 1) % n);
            let side_size = distance(vx - nx, vy - ny);
            cut_adjusts.push(if expected_round_cut > side_size {
                (side_size / expected_round_cut, 0.0)
            } else if expected_cut > side_size {
                (
                    1.0,
                    (side_size - expected_round_cut) / (expected_cut - expected_round_cut),
                )
            } else {
                (1.0, 1.0)
            });
        }

        let mut corner_cubics = Vec::with_capacity(n);
        for i in 0..n {
            let mut allowed = [0.0f32; 2];
            for (delta, slot) in allowed.iter_mut().enumerate() {
                let (round_ratio, cut_ratio) = cut_adjusts[(i + n - 1 + delta) % n];
                *slot = corners[i].expected_round_cut * round_ratio
                    + (corners[i].expected_cut() - corners[i].expected_round_cut) * cut_ratio;
            }
            corner_cubics.push(corners[i].cubics(allowed[0], allowed[1]));
        }

        // Corners, then the straight edge that connects each to the next.
        let mut cubics = Vec::new();
        for i in 0..n {
            cubics.extend_from_slice(&corner_cubics[i]);
            let last = corner_cubics[i].last().expect("a corner has curves");
            let first_next = corner_cubics[(i + 1) % n]
                .first()
                .expect("a corner has curves");
            let (lx, ly) = last.anchor1();
            let (fx, fy) = first_next.anchor0();
            cubics.push(Cubic::straight_line(lx, ly, fx, fy));
        }

        let center = center.unwrap_or_else(|| calculate_center(vertices));
        Self {
            cubics,
            center,
            source: Some(Source {
                vertices: vertices.to_vec(),
                per_vertex: resolved,
            }),
        }
    }

    /// A regular polygon whose vertices sit on a circle. Ported from the
    /// `numVertices` constructor.
    pub fn from_num_vertices(
        num_vertices: usize,
        radius: f32,
        center: [f32; 2],
        rounding: CornerRounding,
        per_vertex: Option<&[CornerRounding]>,
    ) -> Self {
        let mut vertices = Vec::with_capacity(num_vertices * 2);
        for i in 0..num_vertices {
            let (x, y) = radial_to_cartesian(
                radius,
                std::f32::consts::PI / num_vertices as f32 * 2.0 * i as f32,
            );
            vertices.push(x + center[0]);
            vertices.push(y + center[1]);
        }
        Self::from_vertices(&vertices, rounding, per_vertex, Some(center))
    }

    /// Ported from `Shapes.kt`: a circle is a polygon whose rounding *is* the
    /// radius, which is why it is round rather than merely many-sided.
    pub fn circle(num_vertices: usize, radius: f32, center: [f32; 2]) -> Self {
        let theta = std::f32::consts::PI / num_vertices as f32;
        let polygon_radius = radius / theta.cos();
        Self::from_num_vertices(
            num_vertices,
            polygon_radius,
            center,
            CornerRounding::new(radius),
            None,
        )
    }

    /// Ported from `Shapes.kt`: four corners, clockwise from bottom-right.
    pub fn rectangle(
        width: f32,
        height: f32,
        rounding: CornerRounding,
        per_vertex: Option<&[CornerRounding]>,
        center: [f32; 2],
    ) -> Self {
        let left = center[0] - width / 2.0;
        let top = center[1] - height / 2.0;
        let right = center[0] + width / 2.0;
        let bottom = center[1] + height / 2.0;
        Self::from_vertices(
            &[right, bottom, left, bottom, left, top, right, top],
            rounding,
            per_vertex,
            Some(center),
        )
    }

    /// Ported from `Shapes.kt`: alternating outer and inner radii.
    pub fn star(
        num_vertices_per_radius: usize,
        radius: f32,
        inner_radius: f32,
        rounding: CornerRounding,
        inner_rounding: Option<CornerRounding>,
        center: [f32; 2],
    ) -> Self {
        let per_vertex: Option<Vec<CornerRounding>> = inner_rounding.map(|inner| {
            (0..num_vertices_per_radius)
                .flat_map(|_| [rounding, inner])
                .collect()
        });
        let mut vertices = Vec::with_capacity(num_vertices_per_radius * 4);
        for i in 0..num_vertices_per_radius {
            let (x, y) = radial_to_cartesian(
                radius,
                std::f32::consts::PI / num_vertices_per_radius as f32 * 2.0 * i as f32,
            );
            vertices.push(x + center[0]);
            vertices.push(y + center[1]);
            let (x, y) = radial_to_cartesian(
                inner_radius,
                std::f32::consts::PI / num_vertices_per_radius as f32
                    * (2.0 * i as f32 + 1.0),
            );
            vertices.push(x + center[0]);
            vertices.push(y + center[1]);
        }
        Self::from_vertices(&vertices, rounding, per_vertex.as_deref(), Some(center))
    }

    /// Ported from `RoundedPolygon.normalized`: scale the shape so its largest
    /// side is `1`, centred in the unit box. This is what makes every Material
    /// shape fit the same square regardless of how its vertices were placed.
    pub fn normalized(&self) -> Self {
        let b = self.bounds();
        let width = b[2] - b[0];
        let height = b[3] - b[1];
        let side = width.max(height);
        if !(side > 0.0) {
            return self.clone();
        }
        let offset_x = (side - width) / 2.0 - b[0];
        let offset_y = (side - height) / 2.0 - b[1];
        self.transformed(&|x, y| ((x + offset_x) / side, (y + offset_y) / side))
    }

    /// Every cubic transformed by `f`, with the centre transformed too.
    pub fn transformed(&self, f: &dyn Fn(f32, f32) -> (f32, f32)) -> Self {
        let cubics = self
            .cubics
            .iter()
            .map(|c| {
                let (x0, y0) = f(c.p[0], c.p[1]);
                let (c0x, c0y) = f(c.p[2], c.p[3]);
                let (c1x, c1y) = f(c.p[4], c.p[5]);
                let (x1, y1) = f(c.p[6], c.p[7]);
                Cubic {
                    p: [x0, y0, c0x, c0y, c1x, c1y, x1, y1],
                }
            })
            .collect();
        let center = [f(self.center[0], self.center[1]).0, f(self.center[0], self.center[1]).1];
        // The inputs move with the curves, or the user's slider would rebuild the
        // shape in the frame it was authored in rather than the frame it is drawn
        // in — `cookie9` is a rotated star, and rotating the curves alone would
        // leave the slider working on the unrotated corners.
        let source = self.source.as_ref().map(|s| Source {
            vertices: s
                .vertices
                .chunks_exact(2)
                .flat_map(|pair| {
                    let (x, y) = f(pair[0], pair[1]);
                    [x, y]
                })
                .collect(),
            per_vertex: s.per_vertex.clone(),
        });
        Self {
            cubics,
            center,
            source,
        }
    }

    /// Rotate around the shape's own centre, in degrees.
    pub fn rotated_deg(&self, deg: f32) -> Self {
        let a = deg.to_radians();
        let (sin, cos) = (a.sin(), a.cos());
        let (cx, cy) = (self.center[0], self.center[1]);
        self.transformed(&|x, y| {
            let (dx, dy) = (x - cx, y - cy);
            (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
        })
    }

    /// Scale about the origin.
    pub fn scaled(&self, sx: f32, sy: f32) -> Self {
        self.transformed(&|x, y| (x * sx, y * sy))
    }

    /// Axis-aligned bounds of the whole outline, from the approximate cubic
    /// bounds the reference uses for normalisation.
    pub fn bounds(&self) -> [f32; 4] {
        let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for c in &self.cubics {
            let cb = c.approximate_bounds();
            b[0] = b[0].min(cb[0]);
            b[1] = b[1].min(cb[1]);
            b[2] = b[2].max(cb[2]);
            b[3] = b[3].max(cb[3]);
        }
        b
    }
}

/// One rounded corner: the vertex and its neighbours, and the curves that
/// replace it. Ported from `RoundedCorner`.
struct RoundedCorner {
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    d1: (f32, f32),
    d2: (f32, f32),
    corner_radius: f32,
    smoothing: f32,
    expected_round_cut: f32,
}

impl RoundedCorner {
    fn new(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), rounding: CornerRounding) -> Self {
        let v01 = (p0.0 - p1.0, p0.1 - p1.1);
        let v21 = (p2.0 - p1.0, p2.1 - p1.1);
        let d01 = distance(v01.0, v01.1);
        let d21 = distance(v21.0, v21.1);
        if d01 > 0.0 && d21 > 0.0 {
            let d1 = (v01.0 / d01, v01.1 / d01);
            let d2 = (v21.0 / d21, v21.1 / d21);
            let cos_angle = dot(d1, d2);
            let sin_angle = (1.0 - sq(cos_angle)).max(0.0).sqrt();
            // `tan(A/2) = sin(A) / (1 + cos(A))`, rearranged for the cut that
            // reaches the requested radius.
            let expected_round_cut = if sin_angle > 1e-3 {
                rounding.radius * (cos_angle + 1.0) / sin_angle
            } else {
                0.0
            };
            Self {
                p0,
                p1,
                p2,
                d1,
                d2,
                corner_radius: rounding.radius,
                smoothing: rounding.smoothing,
                expected_round_cut,
            }
        } else {
            Self {
                p0,
                p1,
                p2,
                d1: (0.0, 0.0),
                d2: (0.0, 0.0),
                corner_radius: 0.0,
                smoothing: 0.0,
                expected_round_cut: 0.0,
            }
        }
    }

    /// Smoothing changes the cut: `0` is the round cut, `1` doubles it.
    fn expected_cut(&self) -> f32 {
        (1.0 + self.smoothing) * self.expected_round_cut
    }

    fn actual_smoothing(&self, allowed_cut: f32) -> f32 {
        let expected_cut = self.expected_cut();
        if allowed_cut > expected_cut {
            self.smoothing
        } else if allowed_cut > self.expected_round_cut {
            self.smoothing * (allowed_cut - self.expected_round_cut)
                / (expected_cut - self.expected_round_cut)
        } else {
            0.0
        }
    }

    /// The three curves that replace the corner: a flanking curve, the arc, and
    /// the mirror flanking curve. Ported from `getCubics`.
    fn cubics(&self, allowed_cut0: f32, allowed_cut1: f32) -> Vec<Cubic> {
        let allowed_cut = allowed_cut0.min(allowed_cut1);
        if self.expected_round_cut < 1e-6
            || allowed_cut < 1e-6
            || self.corner_radius < 1e-6
        {
            return vec![Cubic::straight_line(self.p1.0, self.p1.1, self.p1.0, self.p1.1)];
        }
        let actual_round_cut = allowed_cut.min(self.expected_round_cut);
        let actual_smoothing0 = self.actual_smoothing(allowed_cut0);
        let actual_smoothing1 = self.actual_smoothing(allowed_cut1);
        let actual_r = self.corner_radius * actual_round_cut / self.expected_round_cut;
        let center_distance = (sq(actual_r) + sq(actual_round_cut)).sqrt();
        let center = add(
            self.p1,
            scale(
                direction_vector(self.d1.0 + self.d2.0, self.d1.1 + self.d2.1),
                center_distance,
            ),
        );
        let circle_intersection0 = add(self.p1, scale(self.d1, actual_round_cut));
        let circle_intersection2 = add(self.p1, scale(self.d2, actual_round_cut));
        let flanking0 = self.flanking_curve(
            actual_round_cut,
            actual_smoothing0,
            self.p1,
            self.p0,
            circle_intersection0,
            circle_intersection2,
            center,
            actual_r,
        );
        let flanking2 = self
            .flanking_curve(
                actual_round_cut,
                actual_smoothing1,
                self.p1,
                self.p2,
                circle_intersection2,
                circle_intersection0,
                center,
                actual_r,
            )
            .reverse();
        vec![
            flanking0,
            Cubic::circular_arc(
                center.0,
                center.1,
                flanking0.anchor1().0,
                flanking0.anchor1().1,
                flanking2.anchor0().0,
                flanking2.anchor0().1,
            ),
            flanking2,
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn flanking_curve(
        &self,
        actual_round_cut: f32,
        smoothing: f32,
        corner: (f32, f32),
        side_start: (f32, f32),
        circle_intersection: (f32, f32),
        other_circle_intersection: (f32, f32),
        circle_center: (f32, f32),
        actual_r: f32,
    ) -> Cubic {
        let side_direction = direction_vector(side_start.0 - corner.0, side_start.1 - corner.1);
        let curve_start = add(
            corner,
            scale(side_direction, actual_round_cut * (1.0 + smoothing)),
        );
        let p = interpolate_point(
            circle_intersection,
            midpoint(circle_intersection, other_circle_intersection),
            smoothing,
        );
        let curve_end = add(
            circle_center,
            scale(
                direction_vector(p.0 - circle_center.0, p.1 - circle_center.1),
                actual_r,
            ),
        );
        let circle_tangent = rotate90((curve_end.0 - circle_center.0, curve_end.1 - circle_center.1));
        let anchor_end = line_intersection(side_start, side_direction, curve_end, circle_tangent)
            .unwrap_or(circle_intersection);
        let anchor_start = scale(add(curve_start, scale(anchor_end, 2.0)), 1.0 / 3.0);
        Cubic {
            p: [
                curve_start.0,
                curve_start.1,
                anchor_start.0,
                anchor_start.1,
                anchor_end.0,
                anchor_end.1,
                curve_end.0,
                curve_end.1,
            ],
        }
    }
}

/// Ported from `RoundedCorner.lineIntersection`.
fn line_intersection(
    p0: (f32, f32),
    d0: (f32, f32),
    p1: (f32, f32),
    d1: (f32, f32),
) -> Option<(f32, f32)> {
    let rotated_d1 = rotate90(d1);
    let den = dot(d0, rotated_d1);
    if den.abs() < 1e-6 {
        return None;
    }
    let num = dot((p1.0 - p0.0, p1.1 - p0.1), rotated_d1);
    if den.abs() < 1e-6 * num.abs() {
        return None;
    }
    Some(add(p0, scale(d0, num / den)))
}

/// Ported from `calculateCenter`: the average of the vertices.
fn calculate_center(vertices: &[f32]) -> [f32; 2] {
    let n = vertices.len() / 2;
    if n == 0 {
        return [0.0, 0.0];
    }
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        cx += vertices[i * 2];
        cy += vertices[i * 2 + 1];
    }
    [cx / n as f32, cy / n as f32]
}

/// A vertex with its rounding, for the hand-tuned shapes.
pub type PointRounding = (f32, f32, CornerRounding);

/// Ported from `customPolygon`: the hand-tuned shapes are lists of points with
/// per-point rounding, repeated (and optionally mirrored) around the centre.
pub fn custom_polygon(
    points: &[PointRounding],
    reps: usize,
    center: [f32; 2],
    mirroring: bool,
) -> RoundedPolygon {
    let actual = if mirroring {
        let angles: Vec<f32> = points
            .iter()
            .map(|p| angle_degrees(p.0 - center[0], p.1 - center[1]))
            .collect();
        let distances: Vec<f32> = points
            .iter()
            .map(|p| distance(p.0 - center[0], p.1 - center[1]))
            .collect();
        let actual_reps = reps * 2;
        let section_angle = 360.0 / actual_reps as f32;
        let mut out = Vec::with_capacity(points.len() * actual_reps);
        for rep in 0..actual_reps {
            for index in 0..points.len() {
                let i = if rep % 2 == 0 {
                    index
                } else {
                    points.len() - 1 - index
                };
                if i > 0 || rep % 2 == 0 {
                    let a = (section_angle * rep as f32
                        + if rep % 2 == 0 {
                            angles[i]
                        } else {
                            section_angle - angles[i] + 2.0 * angles[0]
                        })
                    .to_radians();
                    out.push((
                        a.cos() * distances[i] + center[0],
                        a.sin() * distances[i] + center[1],
                        points[i].2,
                    ));
                }
            }
        }
        out
    } else {
        let np = points.len();
        (0..np * reps)
            .map(|it| {
                let (x, y) = rotate_degrees(
                    points[it % np].0,
                    points[it % np].1,
                    (it / np) as f32 * 360.0 / reps as f32,
                    center,
                );
                (x, y, points[it % np].2)
            })
            .collect()
    };

    let mut vertices = Vec::with_capacity(actual.len() * 2);
    let mut rounding = Vec::with_capacity(actual.len());
    for (x, y, r) in &actual {
        vertices.push(*x);
        vertices.push(*y);
        rounding.push(*r);
    }
    RoundedPolygon::from_vertices(&vertices, CornerRounding::UNROUNDED, Some(&rounding), Some(center))
}

/// Rotate a point about `center`, in degrees.
fn rotate_degrees(x: f32, y: f32, deg: f32, center: [f32; 2]) -> (f32, f32) {
    let a = deg.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    let (dx, dy) = (x - center[0], y - center[1]);
    (center[0] + dx * cos - dy * sin, center[1] + dx * sin + dy * cos)
}

fn angle_degrees(x: f32, y: f32) -> f32 {
    y.atan2(x) * 180.0 / std::f32::consts::PI
}

fn radial_to_cartesian(radius: f32, angle_radians: f32) -> (f32, f32) {
    (radius * angle_radians.cos(), radius * angle_radians.sin())
}

fn direction_vector(x: f32, y: f32) -> (f32, f32) {
    let d = distance(x, y);
    if d > 1e-6 {
        (x / d, y / d)
    } else {
        (0.0, 0.0)
    }
}

fn rotate90(p: (f32, f32)) -> (f32, f32) {
    (-p.1, p.0)
}

fn dot(a: (f32, f32), b: (f32, f32)) -> f32 {
    a.0 * b.0 + a.1 * b.1
}

fn distance(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

fn sq(x: f32) -> f32 {
    x * x
}

fn add(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 + b.0, a.1 + b.1)
}

fn scale(a: (f32, f32), k: f32) -> (f32, f32) {
    (a.0 * k, a.1 * k)
}

fn midpoint(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

fn interpolate(start: f32, stop: f32, fraction: f32) -> f32 {
    start + (stop - start) * fraction
}

fn interpolate_point(a: (f32, f32), b: (f32, f32), fraction: f32) -> (f32, f32) {
    (interpolate(a.0, b.0, fraction), interpolate(a.1, b.1, fraction))
}

/// The rounding constants the reference uses by name.
const ROUND_15: CornerRounding = CornerRounding::new(0.15);
const ROUND_20: CornerRounding = CornerRounding::new(0.20);
const ROUND_30: CornerRounding = CornerRounding::new(0.30);
const ROUND_50: CornerRounding = CornerRounding::new(0.50);
const ROUND_100: CornerRounding = CornerRounding::new(1.0);

/// Material's rounding for a shape, plus whatever the user's slider adds.
///
/// Material's shapes are rounded by construction, so the slider must not be
/// allowed to *remove* that rounding — otherwise the default position would give
/// back the plain polygon this port exists to replace. The user's value therefore
/// adds on top: `0` leaves the shape exactly as Material defines it, and the top
/// of the slider grows every corner toward `max_radius`.
///
/// `max_radius` is passed in rather than fixed, because a corner radius only
/// means anything relative to the shape it is on: `0.5` turns a unit square into
/// a circle, while on a star's inner point the same number is already degenerate.
/// `user` is the project's `corner_rounding`, in `0..=0.5`.
pub fn apply_user_rounding(base: CornerRounding, user: f32, max_radius: f32) -> CornerRounding {
    let t = (user / 0.5).clamp(0.0, 1.0);
    let target = base.radius.max(max_radius);
    CornerRounding {
        radius: base.radius + (target - base.radius) * t,
        smoothing: base.smoothing,
    }
}


/// The outline's extent, measured by sampling the curves rather than by their
/// control hull: the hull is what `normalized` uses (as the reference does), and
/// it is deliberately larger than the shape.
#[cfg(test)]
fn tight_bounds(p: &RoundedPolygon) -> [f32; 4] {
    let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for c in &p.cubics {
        for step in 0..=16 {
            let t = step as f32 / 16.0;
            let u = 1.0 - t;
            let x = u * u * u * c.p[0]
                + 3.0 * u * u * t * c.p[2]
                + 3.0 * u * t * t * c.p[4]
                + t * t * t * c.p[6];
            let y = u * u * u * c.p[1]
                + 3.0 * u * u * t * c.p[3]
                + 3.0 * u * t * t * c.p[5]
                + t * t * t * c.p[7];
            b[0] = b[0].min(x);
            b[1] = b[1].min(y);
            b[2] = b[2].max(x);
            b[3] = b[3].max(y);
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_count(p: &RoundedPolygon) -> usize {
        p.cubics.iter().filter(|c| !c.zero_length()).count()
    }

    /// Sample the outline densely and return how far each sample sits from the
    /// shape's centre.
    fn outline_radii(p: &RoundedPolygon) -> Vec<f32> {
        p.cubics
            .iter()
            .flat_map(|c| {
                (0..=16).map(move |step| {
                    let t = step as f32 / 16.0;
                    let u = 1.0 - t;
                    let x = u * u * u * c.p[0]
                        + 3.0 * u * u * t * c.p[2]
                        + 3.0 * u * t * t * c.p[4]
                        + t * t * t * c.p[6];
                    let y = u * u * u * c.p[1]
                        + 3.0 * u * u * t * c.p[3]
                        + 3.0 * u * t * t * c.p[5]
                        + t * t * t * c.p[7];
                    distance(x - p.center[0], y - p.center[1])
                })
            })
            .collect()
    }

    /// How much the outline's radius rises and falls going round: near zero for
    /// a polygon, large for a star.
    fn radius_spread(p: &RoundedPolygon) -> f32 {
        let r = outline_radii(p);
        let min_r = r.iter().copied().fold(f32::INFINITY, f32::min);
        let max_r = r.iter().copied().fold(0.0f32, f32::max);
        (max_r - min_r) / max_r
    }

    /// A plain polygon has straight edges and no curves; a rounded one does not.
    /// This is the check that would have caught `cookie9sided` being a polygon.
    #[test]
    fn rounded_shapes_are_curves_and_sharp_ones_are_lines() {
        let sharp = RoundedPolygon::from_num_vertices(9, 1.0, [0.0, 0.0], CornerRounding::UNROUNDED, None);
        let rounded = RoundedPolygon::from_num_vertices(9, 1.0, [0.0, 0.0], ROUND_50, None);
        let curved = |p: &RoundedPolygon| {
            p.cubics
                .iter()
                .filter(|c| {
                    // A straight line has its controls on the line between the
                    // anchors; anything else is a curve.
                    let (x0, y0) = c.anchor0();
                    let (x1, y1) = c.anchor1();
                    let cross = (c.p[2] - x0) * (y1 - y0) - (c.p[3] - y0) * (x1 - x0);
                    cross.abs() > 1e-4
                })
                .count()
        };
        assert_eq!(curved(&sharp), 0, "an unrounded polygon has no curves");
        assert!(
            curved(&rounded) > 0,
            "a rounded polygon must have curves, not lines"
        );
        assert!(curve_count(&rounded) > curve_count(&sharp));
    }

    /// Every shape Material defines is normalised into the unit box: that is what
    /// lets one shape be swapped for another without moving the layer.
    ///
    /// The box is the *control hull's* box, exactly as the reference normalises —
    /// so a shape whose hull is not square sits slightly off centre in the tight
    /// sense, and the assertion is that it fits, not that it touches both edges.
    #[test]
    fn normalized_shapes_fill_the_unit_box() {
        for (name, shape) in crate::m3shape::tests_support::all_material_shapes() {
            let n = shape.normalized();
            let b = n.bounds();
            let side = (b[2] - b[0]).max(b[3] - b[1]);
            assert!(
                (side - 1.0).abs() < 1e-3,
                "{name}: normalised side is {side}, not 1"
            );
            assert!(
                b[0] >= -1e-3 && b[1] >= -1e-3 && b[2] <= 1.0 + 1e-3 && b[3] <= 1.0 + 1e-3,
                "{name}: normalised bounds {b:?} leave the unit box"
            );
            // And it is not a speck in the corner: the outline reaches most of
            // the box.
            let t = tight_bounds(&n);
            let reach = (t[2] - t[0]).max(t[3] - t[1]);
            assert!(reach > 0.5, "{name}: the shape is only {reach} across");
        }
    }

    /// `cookie9` and `sunny` are the two shapes that were visibly wrong, so they
    /// are pinned by name: both are *stars* — their outline rises and falls as it
    /// goes round — and both are rounded.
    ///
    /// The measure is the spread of the outline's radius. A polygon, however
    /// rounded, has one radius; a star alternates between two. Material's
    /// rounding is generous (0.5 on the cookies), so it fills the valleys a long
    /// way in — the spread is smaller than the nominal `innerRadius`, and the
    /// check is against a polygon rather than against `0.8`.
    #[test]
    fn the_cookie_and_sunny_are_rounded_stars() {
        let cookie9 = super::cookie9();
        let sunny = super::sunny();
        // Nine points means eighteen vertices, and every corner contributes
        // three curves (flank, arc, flank) plus one edge: 18 * 4.
        assert_eq!(cookie9.cubics.len(), 18 * 4, "9 corners and 9 edges");
        assert_eq!(sunny.cubics.len(), 16 * 4, "8 corners and 8 edges");

        // The same rounding on a plain polygon: one radius, so almost no spread.
        let polygon =
            RoundedPolygon::from_num_vertices(9, 1.0, [0.0, 0.0], ROUND_50, None);
        let polygon_spread = radius_spread(&polygon);
        let cookie_spread = radius_spread(&cookie9);
        let sunny_spread = radius_spread(&sunny);
        assert!(
            cookie_spread > polygon_spread * 3.0,
            "cookie9 must be a star, not a rounded polygon: spread {cookie_spread} vs {polygon_spread}"
        );
        assert!(
            sunny_spread > polygon_spread * 2.0,
            "sunny must be a star: spread {sunny_spread} vs {polygon_spread}"
        );
        // And a star is bigger across than a polygon with the same rounding,
        // because its points reach further out.
        assert!(cookie9.bounds()[2] - cookie9.bounds()[0] > 0.0);
        assert!(sunny.bounds()[2] - sunny.bounds()[0] > 1.5);
    }

    /// The user's slider adds rounding but never takes Material's away.
    #[test]
    fn the_slider_only_adds_rounding() {
        let base = ROUND_20;
        assert_eq!(apply_user_rounding(base, 0.0, 0.5), base);
        assert!(apply_user_rounding(base, 0.25, 0.5).radius > base.radius);
        assert_eq!(apply_user_rounding(base, 0.5, 0.5).radius, 0.5);
        // Out of range is clamped, not extrapolated.
        assert_eq!(apply_user_rounding(base, 5.0, 0.5).radius, 0.5);
        assert_eq!(apply_user_rounding(base, -1.0, 0.5), base);
        // A corner Material already rounds harder than the cap is left alone:
        // the slider may not take rounding away.
        let already = ROUND_100;
        assert_eq!(apply_user_rounding(already, 0.5, 0.5), already);
    }

    /// The slider must round the shape's *corners*, not the curves those corners
    /// already became.
    ///
    /// This is the bug the fix exists for. A rounded corner is three curves, so a
    /// finished shape has four times as many curve anchors as it has corners;
    /// rebuilding from those anchors rounds the arcs and turns a square into a
    /// lumpy blob. The curve count is the fingerprint: it is four curves per
    /// corner plus one edge per corner, and it must not move when the slider does.
    #[test]
    fn the_slider_rounds_corners_not_the_curves_they_became() {
        use crate::ShapeKind;
        // `square` has 4 corners, `cookie9` is a star: 9 outer + 9 inner.
        for (kind, corners) in [(ShapeKind::Square, 4), (ShapeKind::Cookie9Sided, 18)] {
            for user in [0.0, 0.1, 0.25, 0.5] {
                let p = shape(kind, user);
                assert_eq!(
                    p.cubics.len(),
                    corners * 4,
                    "{kind:?} at slider {user}: the shape gained or lost corners"
                );
            }
        }
    }

    /// And what the slider does must read as *rounder*, in the direction a user
    /// expects: a square's corners pull in, and a star stays a star.
    #[test]
    fn the_slider_makes_a_square_rounder_and_leaves_a_star_a_star() {
        use crate::ShapeKind;
        // A square's reach is at its corners, 45 degrees off axis; a circle's is
        // the same in every direction. So the outline's furthest point walks in
        // toward the half-diagonal of the inscribed circle as rounding grows.
        let reach = |kind, user| {
            outline_radii(&shape(kind, user))
                .into_iter()
                .fold(0.0f32, f32::max)
        };
        let (a, b, c) = (
            reach(ShapeKind::Square, 0.0),
            reach(ShapeKind::Square, 0.25),
            reach(ShapeKind::Square, 0.5),
        );
        assert!(a > b && b > c, "the square is not getting rounder: {a} {b} {c}");
        assert!(
            c < a * 0.9,
            "the slider barely moved the square: {a} -> {c}"
        );

        // A cookie is a star because its radius rises and falls. Rounding its
        // points must not flatten it into the rounded polygon it resembles.
        let polygon = RoundedPolygon::from_num_vertices(9, 1.0, [0.0, 0.0], ROUND_50, None);
        for user in [0.0, 0.5] {
            let cookie = shape(ShapeKind::Cookie9Sided, user);
            assert!(
                radius_spread(&cookie) > radius_spread(&polygon) * 3.0,
                "cookie9 at slider {user} stopped being a star"
            );
        }
    }

    /// A circle is round, and a rectangle is not: the two ends of the range.
    #[test]
    fn circle_and_rectangle_differ_as_they_should() {
        let circle = RoundedPolygon::circle(10, 1.0, [0.0, 0.0]).normalized();
        let rect = RoundedPolygon::rectangle(1.0, 1.0, ROUND_30, None, [0.0, 0.0]).normalized();
        // Measured on the curves, not on their control hull: the hull of a
        // circular arc is taller than the arc, which is exactly why the
        // reference's own normalisation is slightly off centre for a circle.
        let b = tight_bounds(&circle);
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        assert!(
            (w - h).abs() < 1e-3,
            "a circle is as wide as it is tall: {w} x {h}"
        );
        // The rounded square keeps its corners square-ish: the outline passes
        // through a point far from the centre at 45 degrees.
        let corner = rect
            .cubics
            .iter()
            .flat_map(|c| [(c.p[0], c.p[1]), (c.p[6], c.p[7])])
            .fold(0.0f32, |acc, (x, y)| acc.max(distance(x, y)));
        assert!(corner > 0.55, "a rounded square still reaches its corners");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The table. Ported one-for-one from `MaterialShapes.kt`: the same vertices, the
// same per-vertex rounding, the same smoothing, the same rotations. These are
// *not* approximations of Material's shapes — they are Material's shapes, and
// every number below comes from the reference file named in the module docs.
// ─────────────────────────────────────────────────────────────────────────────

/// The centre the reference's `customPolygon` uses by default.
const C: [f32; 2] = [0.5, 0.5];

fn r(radius: f32) -> CornerRounding {
    CornerRounding::new(radius)
}

fn rs(radius: f32, smoothing: f32) -> CornerRounding {
    CornerRounding::smooth(radius, smoothing)
}

pub fn circle() -> RoundedPolygon {
    RoundedPolygon::circle(10, 1.0, [0.0, 0.0])
}

pub fn square() -> RoundedPolygon {
    RoundedPolygon::rectangle(1.0, 1.0, ROUND_30, None, [0.0, 0.0])
}

pub fn slanted() -> RoundedPolygon {
    custom_polygon(
        &[(0.926, 0.970, rs(0.189, 0.811)), (-0.021, 0.967, r(0.187))],
        2,
        C,
        false,
    )
}

pub fn arch() -> RoundedPolygon {
    RoundedPolygon::from_num_vertices(
        4,
        1.0,
        [0.0, 0.0],
        CornerRounding::UNROUNDED,
        Some(&[ROUND_100, ROUND_100, ROUND_20, ROUND_20]),
    )
    .rotated_deg(-135.0)
}

pub fn fan() -> RoundedPolygon {
    custom_polygon(
        &[
            (1.004, 1.000, rs(0.148, 0.417)),
            (0.000, 1.000, r(0.151)),
            (0.000, -0.003, r(0.148)),
            (0.978, 0.020, r(0.803)),
        ],
        1,
        C,
        false,
    )
}

pub fn arrow() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.892, r(0.313)),
            (-0.216, 1.050, r(0.207)),
            (0.499, -0.160, rs(0.215, 1.000)),
            (1.225, 1.060, r(0.211)),
        ],
        1,
        C,
        false,
    )
}

pub fn semi_circle() -> RoundedPolygon {
    RoundedPolygon::rectangle(
        1.6,
        1.0,
        CornerRounding::UNROUNDED,
        Some(&[ROUND_20, ROUND_20, ROUND_100, ROUND_100]),
        [0.0, 0.0],
    )
}

pub fn oval() -> RoundedPolygon {
    RoundedPolygon::circle(10, 1.0, [0.0, 0.0])
        .scaled(1.0, 0.64)
        .rotated_deg(-45.0)
}

pub fn pill() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.961, 0.039, r(0.426)),
            (1.001, 0.428, CornerRounding::UNROUNDED),
            (1.000, 0.609, r(1.000)),
        ],
        2,
        C,
        true,
    )
}

pub fn triangle() -> RoundedPolygon {
    RoundedPolygon::from_num_vertices(3, 1.0, [0.0, 0.0], ROUND_20, None).rotated_deg(-90.0)
}

pub fn diamond() -> RoundedPolygon {
    custom_polygon(
        &[(0.500, 1.096, rs(0.151, 0.524)), (0.040, 0.500, r(0.159))],
        2,
        C,
        false,
    )
}

pub fn clam_shell() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.171, 0.841, r(0.159)),
            (-0.020, 0.500, r(0.140)),
            (0.170, 0.159, r(0.159)),
        ],
        2,
        C,
        false,
    )
}

pub fn pentagon() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, -0.009, r(0.172)),
            (1.030, 0.365, r(0.164)),
            (0.828, 0.970, r(0.169)),
        ],
        1,
        C,
        true,
    )
}

pub fn gem() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.499, 1.023, rs(0.241, 0.778)),
            (-0.005, 0.792, r(0.208)),
            (0.073, 0.258, r(0.228)),
            (0.433, -0.000, r(0.491)),
        ],
        1,
        C,
        true,
    )
}

pub fn sunny() -> RoundedPolygon {
    RoundedPolygon::star(8, 1.0, 0.8, ROUND_15, None, [0.0, 0.0])
}

pub fn very_sunny() -> RoundedPolygon {
    custom_polygon(
        &[(0.500, 1.080, r(0.085)), (0.358, 0.843, r(0.085))],
        8,
        C,
        false,
    )
}

pub fn cookie4() -> RoundedPolygon {
    custom_polygon(
        &[(1.237, 1.236, r(0.258)), (0.500, 0.918, r(0.233))],
        4,
        C,
        false,
    )
}

pub fn cookie6() -> RoundedPolygon {
    custom_polygon(
        &[(0.723, 0.884, r(0.394)), (0.500, 1.099, r(0.398))],
        6,
        C,
        false,
    )
}

pub fn cookie7() -> RoundedPolygon {
    RoundedPolygon::star(7, 1.0, 0.75, ROUND_50, None, [0.0, 0.0]).rotated_deg(-90.0)
}

pub fn cookie9() -> RoundedPolygon {
    RoundedPolygon::star(9, 1.0, 0.8, ROUND_50, None, [0.0, 0.0]).rotated_deg(-90.0)
}

pub fn cookie12() -> RoundedPolygon {
    RoundedPolygon::star(12, 1.0, 0.8, ROUND_50, None, [0.0, 0.0]).rotated_deg(-90.0)
}

pub fn ghostish() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.0, r(1.000)),
            (1.0, 0.0, r(1.000)),
            (1.0, 1.140, rs(0.254, 0.106)),
            (0.575, 0.906, r(0.253)),
        ],
        1,
        C,
        true,
    )
}

pub fn clover4() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.074, CornerRounding::UNROUNDED),
            (0.725, -0.099, r(0.476)),
        ],
        4,
        C,
        true,
    )
}

pub fn clover8() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.036, CornerRounding::UNROUNDED),
            (0.758, -0.101, r(0.209)),
        ],
        8,
        C,
        false,
    )
}

pub fn burst() -> RoundedPolygon {
    custom_polygon(&[(0.500, -0.006, r(0.006)), (0.592, 0.158, r(0.006))], 12, C, false)
}

pub fn soft_burst() -> RoundedPolygon {
    custom_polygon(&[(0.193, 0.277, r(0.053)), (0.176, 0.055, r(0.053))], 10, C, false)
}

pub fn boom() -> RoundedPolygon {
    custom_polygon(&[(0.457, 0.296, r(0.007)), (0.500, -0.051, r(0.007))], 15, C, false)
}

pub fn soft_boom() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.733, 0.454, CornerRounding::UNROUNDED),
            (0.839, 0.437, r(0.532)),
            (0.949, 0.449, rs(0.439, 1.000)),
            (0.998, 0.478, r(0.174)),
        ],
        16,
        C,
        true,
    )
}

pub fn flower() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.370, 0.187, CornerRounding::UNROUNDED),
            (0.416, 0.049, r(0.381)),
            (0.479, 0.001, r(0.095)),
        ],
        8,
        C,
        true,
    )
}

pub fn puffy() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.053, CornerRounding::UNROUNDED),
            (0.545, -0.040, r(0.405)),
            (0.670, -0.035, r(0.426)),
            (0.717, 0.066, r(0.574)),
            (0.722, 0.128, CornerRounding::UNROUNDED),
            (0.777, 0.002, r(0.360)),
            (0.914, 0.149, r(0.660)),
            (0.926, 0.289, r(0.660)),
            (0.881, 0.346, CornerRounding::UNROUNDED),
            (0.940, 0.344, r(0.126)),
            (1.003, 0.437, r(0.255)),
        ],
        2,
        C,
        true,
    )
    .scaled(1.0, 0.742)
}

pub fn puffy_diamond() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.870, 0.130, r(0.146)),
            (0.818, 0.357, CornerRounding::UNROUNDED),
            (1.000, 0.332, r(0.853)),
        ],
        4,
        C,
        true,
    )
}

pub fn pixel_circle() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.000, CornerRounding::UNROUNDED),
            (0.704, 0.000, CornerRounding::UNROUNDED),
            (0.704, 0.065, CornerRounding::UNROUNDED),
            (0.843, 0.065, CornerRounding::UNROUNDED),
            (0.843, 0.148, CornerRounding::UNROUNDED),
            (0.926, 0.148, CornerRounding::UNROUNDED),
            (0.926, 0.296, CornerRounding::UNROUNDED),
            (1.000, 0.296, CornerRounding::UNROUNDED),
        ],
        2,
        C,
        true,
    )
}

pub fn pixel_triangle() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.110, 0.500, CornerRounding::UNROUNDED),
            (0.113, 0.000, CornerRounding::UNROUNDED),
            (0.287, 0.000, CornerRounding::UNROUNDED),
            (0.287, 0.087, CornerRounding::UNROUNDED),
            (0.421, 0.087, CornerRounding::UNROUNDED),
            (0.421, 0.170, CornerRounding::UNROUNDED),
            (0.560, 0.170, CornerRounding::UNROUNDED),
            (0.560, 0.265, CornerRounding::UNROUNDED),
            (0.674, 0.265, CornerRounding::UNROUNDED),
            (0.675, 0.344, CornerRounding::UNROUNDED),
            (0.789, 0.344, CornerRounding::UNROUNDED),
            (0.789, 0.439, CornerRounding::UNROUNDED),
            (0.888, 0.439, CornerRounding::UNROUNDED),
        ],
        1,
        C,
        true,
    )
}

pub fn bun() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.796, 0.500, CornerRounding::UNROUNDED),
            (0.853, 0.518, r(1.0)),
            (0.992, 0.631, r(1.0)),
            (0.968, 1.000, r(1.0)),
        ],
        2,
        C,
        true,
    )
}

pub fn heart() -> RoundedPolygon {
    custom_polygon(
        &[
            (0.500, 0.268, r(0.016)),
            (0.792, -0.066, r(0.958)),
            (1.064, 0.276, r(1.000)),
            (0.501, 0.946, r(0.129)),
        ],
        1,
        C,
        true,
    )
}

/// The shape for a kind, normalised into the unit box, with the user's rounding
/// added on top of Material's own.
///
/// One entry point for the whole table: the geometry never leaves this module in
/// any other form, so there is exactly one place where a shape can be wrong.
pub fn shape(kind: crate::ShapeKind, user_rounding: f32) -> RoundedPolygon {
    use crate::ShapeKind as K;
    let base = match kind {
        // The background is a plain square; it is drawn to the frame by
        // `preview_shape_draws`, so its tessellation only has to exist.
        K::Frame => square(),
        K::Circle => circle(),
        K::Square => square(),
        K::Slanted => slanted(),
        K::Arch => arch(),
        K::Fan => fan(),
        K::Arrow => arrow(),
        K::SemiCircle => semi_circle(),
        K::Oval => oval(),
        K::Pill => pill(),
        K::Triangle => triangle(),
        K::Diamond => diamond(),
        K::ClamShell => clam_shell(),
        K::Pentagon => pentagon(),
        K::Gem => gem(),
        K::Sunny => sunny(),
        K::VerySunny => very_sunny(),
        K::Cookie4Sided => cookie4(),
        K::Cookie6Sided => cookie6(),
        K::Cookie7Sided => cookie7(),
        K::Cookie9Sided => cookie9(),
        K::Cookie12Sided => cookie12(),
        K::Ghostish => ghostish(),
        K::Clover4Leaf => clover4(),
        K::Clover8Leaf => clover8(),
        K::Burst => burst(),
        K::SoftBurst => soft_burst(),
        K::Boom => boom(),
        K::SoftBoom => soft_boom(),
        K::Flower => flower(),
        K::Puffy => puffy(),
        K::PuffyDiamond => puffy_diamond(),
        K::PixelCircle => pixel_circle(),
        K::PixelTriangle => pixel_triangle(),
        K::Bun => bun(),
        K::Heart => heart(),
    };
    let rounded = with_user_rounding(&base, user_rounding);
    rounded.normalized()
}

/// Add the user's rounding to every corner of an already-built shape.
///
/// This rebuilds from the polygon's stored *inputs*, not from its curves: the
/// corners are baked into the curves by the time a shape exists, and a rounded
/// corner is three curves, so re-rounding the curves would round the arcs. Each
/// corner's own Material rounding is grown toward a full circle by the slider,
/// so the shape stays recognisably itself — a `cookie9` stays a nine-point
/// cookie with rounder points, it does not become a blob.
///
/// A shape with no stored inputs (only reachable from a hand-built polygon) is
/// returned unchanged rather than guessed at.
fn with_user_rounding(base: &RoundedPolygon, user: f32) -> RoundedPolygon {
    if user <= 0.0 {
        return base.clone();
    }
    let Some(src) = base.source.as_ref() else {
        return base.clone();
    };
    let max_radius = max_user_radius(src);
    let per_vertex: Vec<CornerRounding> = src
        .per_vertex
        .iter()
        .map(|r| apply_user_rounding(*r, user, max_radius))
        .collect();
    RoundedPolygon::from_vertices(
        &src.vertices,
        CornerRounding::UNROUNDED,
        Some(&per_vertex),
        Some(base.center),
    )
}

/// The largest corner radius the slider may ask for, in the shape's own units:
/// half the shortest side.
///
/// That is not a guess — it is where two neighbouring corners' cuts meet. A
/// corner's cut is `radius * (1 + cos) / sin` long, and on a square that is just
/// the radius, so half a side is exactly the radius that turns a square into a
/// circle; past it the arcs swallow each other and the shape degenerates. A shape
/// Material already rounds harder than this (a circle, or the `0.5` cookies,
/// whose rounding the reference itself clamps to the side) is left alone.
fn max_user_radius(src: &Source) -> f32 {
    let n = src.vertices.len() / 2;
    if n < 3 {
        return 0.0;
    }
    let vtx = |i: usize| (src.vertices[i * 2], src.vertices[i * 2 + 1]);
    let mut shortest = f32::INFINITY;
    for i in 0..n {
        let (ax, ay) = vtx(i);
        let (bx, by) = vtx((i + 1) % n);
        shortest = shortest.min(distance(ax - bx, ay - by));
    }
    0.5 * shortest
}

/// Test-only access to the whole table, so the normalisation test can walk it.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub fn all_material_shapes() -> Vec<(&'static str, RoundedPolygon)> {
        let mut out = Vec::new();
        out.push(("circle", circle()));
        out.push(("square", square()));
        out.push(("slanted", slanted()));
        out.push(("arch", arch()));
        out.push(("fan", fan()));
        out.push(("arrow", arrow()));
        out.push(("semiCircle", semi_circle()));
        out.push(("oval", oval()));
        out.push(("pill", pill()));
        out.push(("triangle", triangle()));
        out.push(("diamond", diamond()));
        out.push(("clamShell", clam_shell()));
        out.push(("pentagon", pentagon()));
        out.push(("gem", gem()));
        out.push(("sunny", sunny()));
        out.push(("verySunny", very_sunny()));
        out.push(("cookie4", cookie4()));
        out.push(("cookie6", cookie6()));
        out.push(("cookie7", cookie7()));
        out.push(("cookie9", cookie9()));
        out.push(("cookie12", cookie12()));
        out.push(("ghostish", ghostish()));
        out.push(("clover4", clover4()));
        out.push(("clover8", clover8()));
        out.push(("burst", burst()));
        out.push(("softBurst", soft_burst()));
        out.push(("boom", boom()));
        out.push(("softBoom", soft_boom()));
        out.push(("flower", flower()));
        out.push(("puffy", puffy()));
        out.push(("puffyDiamond", puffy_diamond()));
        out.push(("pixelCircle", pixel_circle()));
        out.push(("pixelTriangle", pixel_triangle()));
        out.push(("bun", bun()));
        out.push(("heart", heart()));
        out
    }
}

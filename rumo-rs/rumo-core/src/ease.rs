// SPDX-License-Identifier: Apache-2.0

//! Easing curves: the shape of the motion between two values.
//!
//! A keyframe says *what* a property is and *when*. This module says how it gets
//! there — and that is where "the animation feels dead" or "the animation feels
//! snappy" actually lives. Linear interpolation between two keys is the reason
//! most hand-made motion looks mechanical: real motion leaves slowly, travels,
//! and settles, and the two control points of a cubic Bézier are enough to
//! describe all of that with two numbers per end.
//!
//! # Why cubic Bézier and not a bag of named curves
//!
//! A named set (`ease-in`, `ease-out`, `bounce`, …) cannot be tuned: the moment
//! a user wants the settle to be *slightly* harder, there is nothing to turn. The
//! cubic Bézier form — two control points with the endpoints pinned at `(0,0)`
//! and `(1,1)`, the same shape CSS `cubic-bezier()` and every motion tool expose —
//! is one curve that names all of them: the presets below are ordinary values of
//! the same type, not special cases. `y` is deliberately **not** clamped to
//! `0..=1`: letting it pass 1 and come back is exactly what an overshoot is, and
//! an overshoot is what "snappy" means for a layer that lands.
//!
//! # The two degenerate curves are values too
//!
//! [`Ease::Linear`] is the straight line, and [`Ease::Hold`] is the step — the
//! value stays put for the whole segment and jumps at the next key. Both are
//! cases of the same type rather than an `Option`, so a sampler has one shape to
//! handle and a document has one field to carry.
//!
//! # Backward compatibility is a *value*, not a promise
//!
//! The editor's cross-fade used a hardcoded `3t² - 2t³` smoothstep before this
//! module existed. That curve **is** `cubic-bezier(1/3, 0, 2/3, 1)` — exactly,
//! not approximately — and [`Ease::SMOOTH`] is that value, used as the default
//! for every transition that never chose one. So a project written before curves
//! existed keeps its exact look, and `the_legacy_ramp_is_a_curve` pins that
//! equality rather than trusting it.

use serde::{Deserialize, Serialize};

/// A timing function for one segment between two keys.
///
/// Serde's **externally** tagged form, which is the one both formats can carry:
/// `postcard` writes an enum as a variant index followed by the payload and
/// refuses anything else (`WontImplement`), so an internally tagged enum — the
/// flatter JSON shape this started as — cannot be saved at all. The cost is a
/// JSON shape with two branches, `"linear"` and
/// `{"cubic":{"x1":0.34,…}}`, which the hand-written Kotlin reader handles as
/// two cases rather than one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// A straight line: the value moves at a constant rate.
    Linear,
    /// A step: the value holds until the next key, then jumps.
    Hold,
    /// `cubic-bezier(x1, y1, x2, y2)`: `x` is time, `y` is the eased progress.
    Cubic { x1: f32, y1: f32, x2: f32, y2: f32 },
}

impl Default for Ease {
    /// Linear, so a document that never mentions a curve behaves exactly as it
    /// did before curves existed.
    fn default() -> Self {
        Self::Linear
    }
}

impl Ease {
    /// The classic "starts slow, settles slow": CSS `ease`.
    pub const EASE: Ease = Ease::Cubic {
        x1: 0.25,
        y1: 0.1,
        x2: 0.25,
        y2: 1.0,
    };
    /// CSS `ease-in`: leaves slowly, arrives at full speed.
    pub const EASE_IN: Ease = Ease::Cubic {
        x1: 0.42,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };
    /// CSS `ease-out`: leaves fast, settles.
    pub const EASE_OUT: Ease = Ease::Cubic {
        x1: 0.0,
        y1: 0.0,
        x2: 0.58,
        y2: 1.0,
    };
    /// CSS `ease-in-out`.
    pub const EASE_IN_OUT: Ease = Ease::Cubic {
        x1: 0.42,
        y1: 0.0,
        x2: 0.58,
        y2: 1.0,
    };
    /// Overshoot and come back: the "snap" a layer lands with.
    ///
    /// `y1 > 1` is what makes it overshoot, and it is the reason `y` is not
    /// clamped. The overshoot is a real value on a real track, so a layer may
    /// legitimately pass its target and return.
    pub const SNAP: Ease = Ease::Cubic {
        x1: 0.34,
        y1: 1.56,
        x2: 0.64,
        y2: 1.0,
    };
    /// Leave fast and stop dead: the hard-edged one, for cuts and hits.
    pub const HIT: Ease = Ease::Cubic {
        x1: 0.05,
        y1: 0.7,
        x2: 0.1,
        y2: 1.0,
    };
    /// Exactly the `3t² - 2t³` ramp the editor used before curves existed.
    ///
    /// `cubic-bezier(1/3, 0, 2/3, 1)` has `x(u) = u` identically (the three
    /// terms of the x polynomial collapse), so its `y` is `3u² - 2u³` in the
    /// parameter itself — that is, the same function of the same argument.
    pub const SMOOTH: Ease = Ease::Cubic {
        x1: 1.0 / 3.0,
        y1: 0.0,
        x2: 2.0 / 3.0,
        y2: 1.0,
    };

    /// The presets, with the names a tool or a UI offers.
    ///
    /// One list, so the editor's picker, the assistant's tool schema and the
    /// tests all read the same names — a preset that exists in the UI but not
    /// here would be a name that saves and never loads.
    pub const PRESETS: &'static [(&'static str, Ease)] = &[
        ("linear", Ease::Linear),
        ("hold", Ease::Hold),
        ("ease", Ease::EASE),
        ("ease_in", Ease::EASE_IN),
        ("ease_out", Ease::EASE_OUT),
        ("ease_in_out", Ease::EASE_IN_OUT),
        ("smooth", Ease::SMOOTH),
        ("snap", Ease::SNAP),
        ("hit", Ease::HIT),
    ];

    /// Look a preset up by name.
    pub fn preset(name: &str) -> Option<Ease> {
        Self::PRESETS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, ease)| *ease)
    }

    /// The name of this curve when it is exactly a preset, for the UI and the
    /// assistant to report back.
    pub fn preset_name(&self) -> Option<&'static str> {
        Self::PRESETS
            .iter()
            .find(|(_, ease)| ease == self)
            .map(|(name, _)| *name)
    }

    /// Eased progress for linear progress `t`.
    ///
    /// `t` is clamped to `0..=1` and a non-finite `t` reads as `0`, because this
    /// value multiplies into a layer's position: a NaN that reached the frame
    /// would take the whole layer off screen rather than look wrong.
    pub fn at(&self, t: f32) -> f32 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        match *self {
            Ease::Linear => t,
            // The step: the value holds for the whole segment and jumps at the
            // next key. The jump belongs to the segment's *end*, so at exactly
            // `t = 1` the next key's value is already the answer. Without that,
            // a held key would still be holding at the instant the next key
            // starts, and the two would disagree about a frame boundary — the
            // one place where an off-by-one is visible as a stuck frame.
            Ease::Hold => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Ease::Cubic { x1, y1, x2, y2 } => cubic_bezier(t, x1, y1, x2, y2),
        }
    }

    /// Whether this curve is the straight line, so a caller can take the exact
    /// arithmetic path it took before curves existed.
    pub fn is_linear(&self) -> bool {
        matches!(self, Ease::Linear)
    }

    /// The control points, for an editor that draws the curve. `None` for the
    /// two curves that have no handles.
    pub fn handles(&self) -> Option<(f32, f32, f32, f32)> {
        match *self {
            Ease::Cubic { x1, y1, x2, y2 } => Some((x1, y1, x2, y2)),
            Ease::Linear | Ease::Hold => None,
        }
    }
}

/// Evaluate a cubic Bézier timing function at linear progress `t`.
///
/// The curve runs from `(0, 0)` to `(1, 1)` with control points `(x1, y1)` and
/// `(x2, y2)`. `x` is time, so the parameter `u` of the curve is not `t`: the
/// first step is to find the `u` whose `x(u)` equals `t`, and the answer is
/// `y(u)`. Both coordinates are cubics in `u`:
///
/// ```text
/// x(u) = ((ax·u + bx)·u + cx)·u     ax = 1 - cx - bx, bx = 3(x2 - x1) - cx, cx = 3·x1
/// ```
///
/// # How `u` is found
///
/// Newton's method from `u = t`, which converges in three or four steps for every
/// curve a person draws, and bisection when it cannot: a curve with `x1 = x2 = 0`
/// has a zero derivative at the start, and Newton alone diverges there. Bisection
/// is slower and unconditionally convergent, so the fallback is bounded by
/// iteration count rather than by hope.
///
/// `x1` and `x2` are clamped to `0..=1` — outside that range `x(u)` stops being
/// monotonic and the curve stops being a function of time, so there would be no
/// answer to find. `y1` and `y2` are left alone: passing 1 is an overshoot, which
/// is a thing users ask for by name.
pub fn cubic_bezier(t: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    // A curve with a non-finite handle is a corrupt document, not a shape: read
    // it as the straight line so the frame stays finite.
    if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
        return t.clamp(0.0, 1.0);
    }
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let x1 = x1.clamp(0.0, 1.0);
    let x2 = x2.clamp(0.0, 1.0);

    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;

    let sample_x = |u: f32| ((ax * u + bx) * u + cx) * u;
    let sample_dx = |u: f32| (3.0 * ax * u + 2.0 * bx) * u + cx;
    let sample_y = |u: f32| ((ay * u + by) * u + cy) * u;

    // Newton.
    let mut u = t;
    for _ in 0..8 {
        let error = sample_x(u) - t;
        if error.abs() < EPSILON {
            return sample_y(u);
        }
        let slope = sample_dx(u);
        if slope.abs() < EPSILON {
            break;
        }
        u -= error / slope;
    }

    // Bisection, for the curves Newton cannot follow.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    let mut u = t;
    for _ in 0..64 {
        let x = sample_x(u);
        if (x - t).abs() < EPSILON {
            break;
        }
        if x < t {
            low = u;
        } else {
            high = u;
        }
        u = (low + high) * 0.5;
    }
    sample_y(u)
}

/// How close `x(u)` has to be to `t` before the parameter is accepted.
///
/// `1e-6` in the parameter is well below one 1080p pixel of motion for any
/// realistic layer size — a key that moves a layer across 1000 px over a second
/// moves it 1/1000 px in a millisecond — so this is finer than any frame can show
/// and coarse enough for the solver to stop in a few steps.
const EPSILON: f32 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every preset must be a function: one input, one output, no surprise.
    ///
    /// Progress in *time* must never go backwards — that is what makes a curve
    /// usable as a ramp — but the *value* is allowed to: an overshoot is a curve
    /// that passes its target and returns, and [`Ease::SNAP`] does exactly that.
    /// So the monotonicity check is applied to the curves that claim to be
    /// monotonic, and the overshoot is asserted where it belongs.
    #[test]
    fn every_preset_is_a_function_of_time() {
        for (name, ease) in Ease::PRESETS {
            assert_eq!(ease.at(0.0), 0.0, "{name} must start at 0");
            assert_eq!(ease.at(1.0), 1.0, "{name} must end at 1");
            let overshoots = ease
                .handles()
                .map(|(_, y1, _, y2)| y1 > 1.0 || y2 > 1.0)
                .unwrap_or(false);
            let mut previous = f32::NEG_INFINITY;
            for step in 0..=100 {
                let t = step as f32 / 100.0;
                let v = ease.at(t);
                assert!(v.is_finite(), "{name} at {t} is {v}");
                if !overshoots {
                    assert!(
                        v >= previous - 1e-5,
                        "{name} went backwards at {t}: {v} after {previous}"
                    );
                }
                previous = v;
            }
        }
    }

    /// The two degenerate curves are values of the same type, not special cases
    /// a caller has to know about.
    #[test]
    fn linear_is_the_identity_and_hold_is_a_step() {
        for step in 0..=10 {
            let t = step as f32 / 10.0;
            assert_eq!(Ease::Linear.at(t), t);
        }
        for t in [0.0f32, 0.25, 0.5, 0.99] {
            assert_eq!(Ease::Hold.at(t), 0.0, "hold keeps the from-value at {t}");
        }
        // ...and at the segment's very end the jump has already happened, so a
        // held key hands over to the next one on the frame it starts.
        assert_eq!(Ease::Hold.at(1.0), 1.0);
    }

    /// The curve the editor used before curves existed is a value in this
    /// module, and the two agree to the last bit — not "closely".
    ///
    /// This is what makes the default safe: a project written before this module
    /// existed keeps its exact look, because nothing about its ramp changed.
    #[test]
    fn the_legacy_ramp_is_a_curve() {
        for step in 0..=1000 {
            let p = step as f32 / 1000.0;
            let legacy = p * p * (3.0 - 2.0 * p);
            let curve = Ease::SMOOTH.at(p);
            assert!(
                (legacy - curve).abs() < 1e-5,
                "smoothstep({p}) = {legacy}, SMOOTH = {curve}"
            );
        }
        // And the curve's x really is the identity, which is why the two are the
        // same function rather than merely similar.
        let (x1, _, x2, _) = Ease::SMOOTH.handles().unwrap();
        assert!((x1 - 1.0 / 3.0).abs() < 1e-7);
        assert!((x2 - 2.0 / 3.0).abs() < 1e-7);
    }

    /// A symmetric curve is symmetric: `ease_in_out` must be its own mirror.
    #[test]
    fn a_symmetric_curve_mirrors_itself() {
        for step in 1..100 {
            let t = step as f32 / 100.0;
            let forward = Ease::EASE_IN_OUT.at(t);
            let backward = 1.0 - Ease::EASE_IN_OUT.at(1.0 - t);
            assert!(
                (forward - backward).abs() < 1e-4,
                "ease_in_out({t}) = {forward}, mirror = {backward}"
            );
        }
    }

    /// The curves do what their names claim, measured rather than asserted:
    /// `ease_in` is below the line early, `ease_out` above it, and `snap`
    /// actually passes its target.
    #[test]
    fn the_names_match_the_shapes() {
        for step in 1..10 {
            let t = step as f32 / 10.0;
            assert!(Ease::EASE_IN.at(t) < t, "ease_in must lag at {t}");
            assert!(Ease::EASE_OUT.at(t) > t, "ease_out must lead at {t}");
        }
        let peak = (1..100)
            .map(|step| Ease::SNAP.at(step as f32 / 100.0))
            .fold(f32::MIN, f32::max);
        assert!(peak > 1.0, "snap must overshoot, peaked at {peak}");
        // ...and come back to exactly 1, or the layer would not land.
        assert_eq!(Ease::SNAP.at(1.0), 1.0);
    }

    /// A curve with no slope at the start is the case Newton alone cannot solve;
    /// the bisection fallback has to carry it.
    #[test]
    fn a_flat_start_still_solves() {
        let flat = Ease::Cubic {
            x1: 0.0,
            y1: 0.0,
            x2: 0.0,
            y2: 1.0,
        };
        // x(u) = u³ here, so t = 0.125 is exactly u = 0.5, and y(0.5) = 0.5.
        let v = flat.at(0.125);
        assert!((v - 0.5).abs() < 1e-3, "flat start solved to {v}");
        for step in 0..=100 {
            let t = step as f32 / 100.0;
            let v = flat.at(t);
            assert!(v.is_finite() && (0.0..=1.0).contains(&v), "flat at {t} = {v}");
        }
    }

    /// Out-of-range and non-finite input is a corrupt document, not a shape: it
    /// reads as the straight line rather than as a layer leaving the frame.
    #[test]
    fn nonsense_input_reads_as_linear() {
        let nan = Ease::Cubic {
            x1: f32::NAN,
            y1: 0.0,
            x2: 0.5,
            y2: 1.0,
        };
        assert_eq!(nan.at(0.5), 0.5);
        for t in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(nan.at(t), 0.0, "non-finite progress reads as 0");
            assert_eq!(Ease::Linear.at(t), 0.0);
        }
        // x outside 0..=1 would make x(u) non-monotonic; it is clamped instead,
        // so the answer still exists.
        let wild = Ease::Cubic {
            x1: -4.0,
            y1: 0.0,
            x2: 5.0,
            y2: 1.0,
        };
        assert_eq!(wild.at(0.0), 0.0);
        assert_eq!(wild.at(1.0), 1.0);
        let mid = wild.at(0.5);
        assert!(mid.is_finite() && (0.0..=1.0).contains(&mid), "{mid}");
    }

    /// The preset table is the single list the UI, the assistant and the tests
    /// read: a name must survive a round trip through it.
    #[test]
    fn presets_round_trip_by_name() {
        for (name, ease) in Ease::PRESETS {
            let found = Ease::preset(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(found, *ease);
            assert_eq!(found.preset_name(), Some(*name));
        }
        assert_eq!(Ease::preset("no_such_curve"), None);
        // A hand-tuned curve is not a preset, and says so instead of lying.
        assert_eq!(
            Ease::Cubic {
                x1: 0.1,
                y1: 0.2,
                x2: 0.3,
                y2: 0.4
            }
            .preset_name(),
            None
        );
    }

    /// The JSON shape is what Kotlin reads by hand: stable, and one of exactly
    /// two branches.
    #[test]
    fn the_json_shape_is_stable() {
        assert_eq!(serde_json::to_string(&Ease::Linear).unwrap(), "\"linear\"");
        assert_eq!(serde_json::to_string(&Ease::Hold).unwrap(), "\"hold\"");
        let snap = serde_json::to_string(&Ease::SNAP).unwrap();
        assert_eq!(
            snap,
            "{\"cubic\":{\"x1\":0.34,\"y1\":1.56,\"x2\":0.64,\"y2\":1.0}}"
        );
        for ease in [Ease::Linear, Ease::Hold, Ease::SNAP, Ease::EASE_IN_OUT] {
            let text = serde_json::to_string(&ease).unwrap();
            let back: Ease = serde_json::from_str(&text).unwrap();
            assert_eq!(back, ease, "{text}");
        }
        // The default is the straight line, so an older document that never
        // mentioned a curve reads as the arithmetic it was written with.
        assert_eq!(Ease::default(), Ease::Linear);
        assert!(Ease::Linear.is_linear());
        assert!(!Ease::SMOOTH.is_linear());
    }

    /// `postcard` is the format the project file is actually saved in, and it
    /// refuses serde shapes it cannot write. A curve that saves as JSON and not
    /// as `.rumo` would be a field that silently disappears on the next open.
    #[test]
    fn the_binary_shape_round_trips() {
        for ease in [
            Ease::Linear,
            Ease::Hold,
            Ease::SNAP,
            Ease::EASE,
            Ease::Cubic {
                x1: 0.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
            },
        ] {
            let bytes = postcard::to_extend(&ease, Vec::new()).expect("postcard encode");
            let (back, rest) = postcard::take_from_bytes::<Ease>(&bytes).expect("postcard decode");
            assert_eq!(back, ease);
            assert!(rest.is_empty(), "the curve must use exactly its own bytes");
        }
    }

    /// The value table, pinned.
    ///
    /// This is the contract with the Kotlin mirror (`EaseUi.cubicBezier`): the
    /// same curve, evaluated at the same times, has to give these numbers. Kotlin
    /// has no test source set in this project and adding a test framework would
    /// be a new Gradle dependency (AGENTS.md forbids it), so the table is pinned
    /// *here* — where it can be checked — and published in `docs/13 §13.8` for
    /// the Kotlin side to be read against. A change to the solver that this test
    /// accepts is a change Kotlin must make too, and the test failing is the
    /// signal to say so.
    ///
    /// The values are exact expectations, not ranges: a curve is arithmetic, and
    /// "close enough" between two languages is the bug this exists to catch.
    #[test]
    fn the_value_table_kotlin_must_reproduce() {
        // (name, [t = 0, 0.125, 0.25, 0.5, 0.75, 1])
        let table: &[(&str, [f32; 6])] = &[
            ("linear", [0.0, 0.125, 0.25, 0.5, 0.75, 1.0]),
            ("hold", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0]),
            ("ease", [0.0, 0.136887, 0.408511, 0.802403, 0.960459, 1.0]),
            ("ease_in", [0.0, 0.025985, 0.093465, 0.315357, 0.621862, 1.0]),
            ("ease_out", [0.0, 0.198580, 0.378139, 0.684643, 0.906535, 1.0]),
            ("ease_in_out", [0.0, 0.031114, 0.129162, 0.5, 0.870838, 1.0]),
            ("smooth", [0.0, 0.042969, 0.156250, 0.5, 0.843750, 1.0]),
            ("snap", [0.0, 0.488204, 0.816289, 1.087401, 1.059647, 1.0]),
            ("hit", [0.0, 0.675860, 0.831530, 0.950248, 0.990511, 1.0]),
        ];
        let times = [0.0f32, 0.125, 0.25, 0.5, 0.75, 1.0];
        for (name, expected) in table {
            let ease = Ease::preset(name).unwrap_or_else(|| panic!("{name} missing"));
            for (i, t) in times.iter().enumerate() {
                let got = ease.at(*t);
                assert!(
                    (got - expected[i]).abs() < 1e-6,
                    "{name} at {t}: got {got}, table says {} — Kotlin mirrors this table, so a \
                     change here is a change there too",
                    expected[i]
                );
            }
        }
        // The overshoot is in the table, not only in prose: `snap` passes its
        // target in the middle of the segment and is back on it at the end.
        let snap = Ease::SNAP;
        assert!(snap.at(0.5) > 1.0);
    }
}

// SPDX-License-Identifier: Apache-2.0

/// Pixel-space parameters shared by all 2D shape geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeParams {
    /// Overall shape size in px (diameter for round shapes, long side otherwise).
    /// Must be finite and > 0.
    pub size_px: f32,
    /// Corner rounding as a fraction of `size_px`, valid range 0.0..=0.5.
    pub corner_rounding: f32,
    /// Extra rotation in degrees, applied around the shape centre.
    pub rotation_deg: f32,
}

impl Default for ShapeParams {
    fn default() -> Self {
        Self {
            size_px: 256.0,
            corner_rounding: 0.0,
            rotation_deg: 0.0,
        }
    }
}

impl ShapeParams {
    /// Returns `Some(self)` when the parameters are usable, `None` when
    /// `size_px` is not > 0 or `corner_rounding` is outside 0.0..=0.5.
    pub fn validated(self) -> Option<Self> {
        if !(self.size_px > 0.0) {
            return None;
        }
        if !(0.0..=0.5).contains(&self.corner_rounding) {
            return None;
        }
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_params_yield_none() {
        let bad_sizes = [-10.0, 0.0, f32::NAN];
        for size_px in bad_sizes {
            let p = ShapeParams {
                size_px,
                ..ShapeParams::default()
            };
            assert_eq!(p.validated(), None, "size_px={size_px}");
        }
        let bad_roundings = [-0.1, -1.0, 0.51, 1.0, f32::NAN];
        for corner_rounding in bad_roundings {
            let p = ShapeParams {
                corner_rounding,
                ..ShapeParams::default()
            };
            assert_eq!(p.validated(), None, "corner_rounding={corner_rounding}");
        }
        assert!(ShapeParams::default().validated().is_some());
    }
}

// SPDX-License-Identifier: Apache-2.0

//! Frame sources for the export pipeline.

use crate::error::{ExportError, Result};

/// Anything that can produce a full RGBA frame for a point in time.
///
/// `out_rgba` is always `width * height * 4` bytes in `[R,G,B,A]` order.
/// Implementations must be cheap to call from a render thread; the exporter
/// pulls one frame per output frame. Rendering that needs an async GPU
/// context must not block on the JNI thread (see [`crate::LayerFrameSource`]).
pub trait FrameSource {
    fn render_frame(&self, t_seconds: f64, out_rgba: &mut [u8]);
}

/// Built-in [`FrameSource`] over a pre-rendered sequence of RGBA frames.
///
/// `t_seconds` is mapped to a frame index by `round(t * fps)`, clamped to
/// the available range, so the source works for both fps-driven and
/// explicit-timestamp callers.
pub struct RgbaFrames {
    width: u32,
    height: u32,
    fps: f64,
    frames: Vec<Vec<u8>>,
}

impl RgbaFrames {
    /// Wrap `frames` (each `width*height*4` bytes). Errors when `fps` is not
    /// positive or any frame has the wrong length.
    pub fn new(width: u32, height: u32, fps: f64, frames: Vec<Vec<u8>>) -> Result<Self> {
        if !(fps > 0.0) || !fps.is_finite() {
            return Err(ExportError::InvalidConfig(
                "fps must be positive and finite",
            ));
        }
        let expected = width as usize * height as usize * 4;
        for frame in &frames {
            if frame.len() != expected {
                return Err(ExportError::InvalidFrameLen {
                    expected,
                    got: frame.len(),
                });
            }
        }
        Ok(Self {
            width,
            height,
            fps,
            frames,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Frame index selected for `t_seconds`, clamped to `0..len-1`.
    /// Returns 0 for an empty sequence (the caller should not render then).
    pub fn index_for_time(&self, t_seconds: f64) -> usize {
        if self.frames.is_empty() {
            return 0;
        }
        let idx = (t_seconds * self.fps).round();
        if !idx.is_finite() || idx < 0.0 {
            return 0;
        }
        (idx as usize).min(self.frames.len() - 1)
    }
}

impl FrameSource for RgbaFrames {
    fn render_frame(&self, t_seconds: f64, out_rgba: &mut [u8]) {
        if self.frames.is_empty() {
            return;
        }
        let idx = self.index_for_time(t_seconds);
        let frame = &self.frames[idx];
        if out_rgba.len() == frame.len() {
            out_rgba.copy_from_slice(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![v; (w * h * 4) as usize]
    }

    #[test]
    fn index_maps_time_to_frame_and_clamps() {
        let frames = RgbaFrames::new(2, 2, 10.0, vec![frame(2, 2, 1), frame(2, 2, 2)]).unwrap();
        assert_eq!(frames.index_for_time(0.0), 0);
        assert_eq!(frames.index_for_time(0.04), 0); // round(0.4) = 0
        assert_eq!(frames.index_for_time(0.05), 1); // round(0.5) = 1
        assert_eq!(frames.index_for_time(100.0), 1); // clamped
        assert_eq!(frames.index_for_time(-5.0), 0); // clamped
    }

    #[test]
    fn render_copies_selected_frame() {
        let frames = RgbaFrames::new(2, 2, 10.0, vec![frame(2, 2, 1), frame(2, 2, 2)]).unwrap();
        let mut out = vec![0u8; 16];
        frames.render_frame(0.0, &mut out);
        assert_eq!(out, frame(2, 2, 1));
        frames.render_frame(0.2, &mut out);
        assert_eq!(out, frame(2, 2, 2));
    }

    #[test]
    fn rejects_bad_fps_and_bad_frame_len() {
        assert!(matches!(
            RgbaFrames::new(2, 2, 0.0, vec![]),
            Err(ExportError::InvalidConfig(_))
        ));
        assert!(matches!(
            RgbaFrames::new(2, 2, 10.0, vec![vec![0u8; 3]]),
            Err(ExportError::InvalidFrameLen {
                expected: 16,
                got: 3
            })
        ));
    }

    #[test]
    fn empty_sequence_is_a_noop() {
        let frames = RgbaFrames::new(2, 2, 10.0, vec![]).unwrap();
        let mut out = vec![7u8; 16];
        frames.render_frame(1.0, &mut out);
        assert_eq!(out, vec![7u8; 16]);
    }
}

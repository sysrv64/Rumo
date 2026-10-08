// SPDX-License-Identifier: Apache-2.0

//! Frame timing: presentation timestamps in microseconds from a frame index.

/// Presentation timestamp (us) of `frame_index` at `fps`, rounded to the
/// nearest microsecond. Frame 0 is always 0.
pub fn pts_for_frame(frame_index: u64, fps: f64) -> i64 {
    (frame_index as f64 * 1_000_000.0 / fps).round() as i64
}

/// Nominal duration (us) of one frame at `fps`.
pub fn frame_interval_us(fps: f64) -> i64 {
    (1_000_000.0 / fps).round() as i64
}

/// True when `pts` is strictly increasing. Export timestamps must be
/// monotonic for MediaMuxer to accept them.
pub fn is_strictly_increasing(pts: &[i64]) -> bool {
    pts.windows(2).all(|w| w[1] > w[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_30fps_is_monotonic_and_expected() {
        let fps = 30.0;
        let pts: Vec<i64> = (0..120).map(|i| pts_for_frame(i, fps)).collect();
        assert_eq!(pts[0], 0);
        assert_eq!(pts[1], 33_333);
        assert_eq!(pts[2], 66_667);
        assert_eq!(pts[3], 100_000);
        assert_eq!(pts[30], 1_000_000);
        assert!(is_strictly_increasing(&pts));
    }

    #[test]
    fn pts_25fps_is_exact() {
        assert_eq!(frame_interval_us(25.0), 40_000);
        assert_eq!(pts_for_frame(25, 25.0), 1_000_000);
        assert!(is_strictly_increasing(
            &(0..200).map(|i| pts_for_frame(i, 25.0)).collect::<Vec<_>>()
        ));
    }

    #[test]
    fn non_increasing_detected() {
        assert!(!is_strictly_increasing(&[0, 10, 10]));
        assert!(!is_strictly_increasing(&[0, 20, 10]));
        assert!(is_strictly_increasing(&[0]));
        assert!(is_strictly_increasing(&[]));
    }
}

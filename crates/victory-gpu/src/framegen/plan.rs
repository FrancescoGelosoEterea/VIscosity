//! What frame generation is told of the camera, without the library
//! (`super::abi` has the structures): the constants of a frame made from the
//! little that is known here, and the arithmetic for them.
//!
//! The projection, the inverse and the constants' layout are taken from
//! trustno1-coder/VIctory-NVIDIA (`streamline/plan.rs`, GPL-3.0-or-later).

use super::abi;

/// The camera of a frame, as far as frame generation is told.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The size of the depth and the motion vectors.
    pub render: [u32; 2],
    /// The nearer and the farther plane, in the scene's units.
    pub near: f32,
    pub far: f32,
    /// The vertical field of view, in radians.
    pub fov_y: f32,
    /// Depth 1 at the near plane and 0 far away (the title's).
    pub depth_inverted: bool,
    /// The first frame after a cut: nothing before it counts.
    pub reset: bool,
}

impl Camera {
    /// Streamline's motion vector scale for vectors in pixels of the render
    /// size: it wants them as shares of the picture.
    pub fn mvec_scale(&self, multiplier: [f32; 2]) -> [f32; 2] {
        [0, 1].map(|i| multiplier[i] / self.render[i].max(1) as f32)
    }

    /// The common constants of a frame. No camera matrices are known: the
    /// projection is made of the field of view, the aspect and the depth
    /// range; camera motion is in the motion vectors, so the
    /// clip-to-previous-clip matrices stay the identity and the camera stands
    /// at the origin looking down +z.
    pub fn constants(&self, mvec_multiplier: [f32; 2]) -> abi::Constants {
        let aspect = self.render[0].max(1) as f32 / self.render[1].max(1) as f32;
        let view_to_clip = projection(self.near, self.far, self.fov_y, aspect, self.depth_inverted);
        let clip_to_view = invert(&view_to_clip).unwrap_or(IDENTITY);
        abi::Constants {
            base: abi::BaseStructure::new(abi::CONSTANTS, 2),
            camera_view_to_clip: view_to_clip,
            clip_to_camera_view: clip_to_view,
            clip_to_lens_clip: IDENTITY,
            clip_to_prev_clip: IDENTITY,
            prev_clip_to_clip: IDENTITY,
            jitter_offset: [0.0, 0.0],
            mvec_scale: self.mvec_scale(mvec_multiplier),
            camera_pinhole_offset: [0.0, 0.0],
            camera_pos: [0.0, 0.0, 0.0],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_fwd: [0.0, 0.0, 1.0],
            camera_near: self.near,
            camera_far: self.far,
            camera_fov: self.fov_y,
            camera_aspect_ratio: aspect,
            motion_vectors_invalid_value: 0.0,
            depth_inverted: abi::boolean(self.depth_inverted),
            camera_motion_included: abi::TRUE,
            motion_vectors_3d: abi::FALSE,
            reset: abi::boolean(self.reset),
            orthographic_projection: abi::FALSE,
            motion_vectors_dilated: abi::FALSE,
            motion_vectors_jittered: abi::FALSE,
            min_relative_linear_depth_object_separation: 40.0,
        }
    }
}

pub const IDENTITY: abi::Float4x4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// A left-handed perspective projection in Streamline's layout (row vectors,
/// rows as stored): depth 0 at `near` and 1 at `far`, or reversed; a far
/// plane past 1e30 (or not finite) is at infinity. Degenerate inputs give
/// the identity.
pub fn projection(near: f32, far: f32, fov_y: f32, aspect: f32, inverted: bool) -> abi::Float4x4 {
    if !(near > 0.0 && fov_y > 0.0 && fov_y < std::f32::consts::PI && aspect > 0.0) || near.is_nan() {
        return IDENTITY;
    }
    let y = 1.0 / (fov_y * 0.5).tan();
    let x = y / aspect;
    let infinite = !far.is_finite() || far > 1e30 || far <= near;
    let (a, b) = match (infinite, inverted) {
        (false, false) => (far / (far - near), -near * far / (far - near)),
        (false, true) => (near / (near - far), far * near / (far - near)),
        (true, false) => (1.0, -near),
        (true, true) => (0.0, near),
    };
    [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, a, 1.0], [0.0, 0.0, b, 0.0]]
}

/// A 4x4 inverse by cofactors; `None` if singular.
pub fn invert(m: &abi::Float4x4) -> Option<abi::Float4x4> {
    let a: [f64; 16] = std::array::from_fn(|i| f64::from(m[i / 4][i % 4]));
    let mut inv = [0f64; 16];
    inv[0] = a[5] * a[10] * a[15] - a[5] * a[11] * a[14] - a[9] * a[6] * a[15] + a[9] * a[7] * a[14] + a[13] * a[6] * a[11]
        - a[13] * a[7] * a[10];
    inv[4] = -a[4] * a[10] * a[15] + a[4] * a[11] * a[14] + a[8] * a[6] * a[15] - a[8] * a[7] * a[14] - a[12] * a[6] * a[11]
        + a[12] * a[7] * a[10];
    inv[8] = a[4] * a[9] * a[15] - a[4] * a[11] * a[13] - a[8] * a[5] * a[15] + a[8] * a[7] * a[13] + a[12] * a[5] * a[11]
        - a[12] * a[7] * a[9];
    inv[12] = -a[4] * a[9] * a[14] + a[4] * a[10] * a[13] + a[8] * a[5] * a[14] - a[8] * a[6] * a[13] - a[12] * a[5] * a[10]
        + a[12] * a[6] * a[9];
    inv[1] = -a[1] * a[10] * a[15] + a[1] * a[11] * a[14] + a[9] * a[2] * a[15] - a[9] * a[3] * a[14] - a[13] * a[2] * a[11]
        + a[13] * a[3] * a[10];
    inv[5] = a[0] * a[10] * a[15] - a[0] * a[11] * a[14] - a[8] * a[2] * a[15] + a[8] * a[3] * a[14] + a[12] * a[2] * a[11]
        - a[12] * a[3] * a[10];
    inv[9] = -a[0] * a[9] * a[15] + a[0] * a[11] * a[13] + a[8] * a[1] * a[15] - a[8] * a[3] * a[13] - a[12] * a[1] * a[11]
        + a[12] * a[3] * a[9];
    inv[13] = a[0] * a[9] * a[14] - a[0] * a[10] * a[13] - a[8] * a[1] * a[14] + a[8] * a[2] * a[13] + a[12] * a[1] * a[10]
        - a[12] * a[2] * a[9];
    inv[2] = a[1] * a[6] * a[15] - a[1] * a[7] * a[14] - a[5] * a[2] * a[15] + a[5] * a[3] * a[14] + a[13] * a[2] * a[7]
        - a[13] * a[3] * a[6];
    inv[6] = -a[0] * a[6] * a[15] + a[0] * a[7] * a[14] + a[4] * a[2] * a[15] - a[4] * a[3] * a[14] - a[12] * a[2] * a[7]
        + a[12] * a[3] * a[6];
    inv[10] = a[0] * a[5] * a[15] - a[0] * a[7] * a[13] - a[4] * a[1] * a[15] + a[4] * a[3] * a[13] + a[12] * a[1] * a[7]
        - a[12] * a[3] * a[5];
    inv[14] = -a[0] * a[5] * a[14] + a[0] * a[6] * a[13] + a[4] * a[1] * a[14] - a[4] * a[2] * a[13] - a[12] * a[1] * a[6]
        + a[12] * a[2] * a[5];
    inv[3] = -a[1] * a[6] * a[11] + a[1] * a[7] * a[10] + a[5] * a[2] * a[11] - a[5] * a[3] * a[10] - a[9] * a[2] * a[7]
        + a[9] * a[3] * a[6];
    inv[7] = a[0] * a[6] * a[11] - a[0] * a[7] * a[10] - a[4] * a[2] * a[11] + a[4] * a[3] * a[10] + a[8] * a[2] * a[7]
        - a[8] * a[3] * a[6];
    inv[11] = -a[0] * a[5] * a[11] + a[0] * a[7] * a[9] + a[4] * a[1] * a[11] - a[4] * a[3] * a[9] - a[8] * a[1] * a[7]
        + a[8] * a[3] * a[5];
    inv[15] = a[0] * a[5] * a[10] - a[0] * a[6] * a[9] - a[4] * a[1] * a[10] + a[4] * a[2] * a[9] + a[8] * a[1] * a[6]
        - a[8] * a[2] * a[5];
    let det = a[0] * inv[0] + a[1] * inv[4] + a[2] * inv[8] + a[3] * inv[12];
    if det.abs() < 1e-30 || !det.is_finite() {
        return None;
    }
    Some(std::array::from_fn(|r| std::array::from_fn(|c| (inv[r * 4 + c] / det) as f32)))
}


/// `"x,y"` as two multipliers (`VICTORY_FRAMEGEN_MV`): what a run may have to
/// flip; unset or malformed is `[1, 1]`.
pub fn pair(value: Option<&str>) -> [f32; 2] {
    let Some(value) = value else { return [1.0, 1.0] };
    let mut parts = value.split(',').map(|p| p.trim().parse::<f32>());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y)), None) if x.is_finite() && y.is_finite() => [x, y],
        _ => [1.0, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE: Camera = Camera { render: [1920, 1080], near: 0.15, far: 10_000.0, fov_y: 0.583, depth_inverted: true, reset: false };

    #[test]
    fn the_projection_puts_the_planes_at_the_ends_of_the_depth_range() {
        let depth = |m: &abi::Float4x4, z: f32| (z * m[2][2] + m[3][2]) / (z * m[2][3] + m[3][3]);
        let plain = projection(0.15, 10_000.0, 0.583, 16.0 / 9.0, false);
        assert!(depth(&plain, 0.15).abs() < 1e-5 && (depth(&plain, 10_000.0) - 1.0).abs() < 1e-5);
        let inverted = projection(0.15, 10_000.0, 0.583, 16.0 / 9.0, true);
        assert!((depth(&inverted, 0.15) - 1.0).abs() < 1e-5 && depth(&inverted, 10_000.0).abs() < 1e-5);
        assert_eq!(projection(0.0, 1.0, 1.0, 1.0, false), IDENTITY);
    }

    #[test]
    fn the_inverse_undoes_the_projection() {
        let m = projection(0.15, 10_000.0, 0.583, 16.0 / 9.0, true);
        let inverse = invert(&m).expect("a projection has an inverse");
        for r in 0..4 {
            for c in 0..4 {
                let cell: f32 = (0..4).map(|k| m[r][k] * inverse[k][c]).sum();
                assert!((cell - if r == c { 1.0 } else { 0.0 }).abs() < 1e-3, "row {r} column {c}: {cell}");
            }
        }
        assert!(invert(&[[0.0; 4]; 4]).is_none());
    }

    #[test]
    fn vectors_in_pixels_become_shares_of_the_picture() {
        assert_eq!(TITLE.mvec_scale([1.0, 1.0]), [1.0 / 1920.0, 1.0 / 1080.0]);
        assert_eq!(TITLE.mvec_scale([-1.0, 2.0]), [-1.0 / 1920.0, 2.0 / 1080.0]);
        let constants = TITLE.constants([1.0, 1.0]);
        assert_eq!((constants.camera_near, constants.camera_far), (0.15, 10_000.0));
        assert_eq!(constants.depth_inverted, abi::TRUE);
        assert_eq!(constants.clip_to_prev_clip, IDENTITY);
    }

    #[test]
    fn pairs_of_multipliers() {
        assert_eq!(pair(None), [1.0, 1.0]);
        assert_eq!(pair(Some("-1, 1")), [-1.0, 1.0]);
        assert_eq!(pair(Some("2")), [1.0, 1.0]);
        assert_eq!(pair(Some("a,b")), [1.0, 1.0]);
    }
}

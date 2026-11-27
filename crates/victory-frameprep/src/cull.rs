//! What a view volume holds, found on several threads.
//!
//! Bounds are spheres (`[x, y, z, radius]`); the volume is six planes with
//! their normals inwards. A sphere is kept unless it lies wholly behind one
//! plane, so a sphere near a corner may be kept though it is outside: that
//! errs on the side of drawing.

use crate::sched::{RunStats, WorkerPool};

/// Six planes `[nx, ny, nz, d]`, a point `p` being inside one when
/// `dot(n, p) + d >= 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [[f32; 4]; 6],
}

impl Frustum {
    /// The volume of a matrix that takes a row vector `[x, y, z, 1]` to clip
    /// space with depth from 0 to 1 (Direct3D's): `rows` is the matrix by
    /// rows. Planes are scaled so that distances are in the scene's units.
    pub fn from_view_projection(rows: [[f32; 4]; 4]) -> Frustum {
        let column = |c: usize| [rows[0][c], rows[1][c], rows[2][c], rows[3][c]];
        let (x, y, z, w) = (column(0), column(1), column(2), column(3));
        let add = |a: [f32; 4], b: [f32; 4]| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
        let sub = |a: [f32; 4], b: [f32; 4]| [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
        let mut planes = [add(w, x), sub(w, x), add(w, y), sub(w, y), z, sub(w, z)];
        for plane in &mut planes {
            let length = (plane[0] * plane[0] + plane[1] * plane[1] + plane[2] * plane[2]).sqrt().max(f32::MIN_POSITIVE);
            for value in plane.iter_mut() {
                *value /= length;
            }
        }
        Frustum { planes }
    }

    /// Whether a sphere is not wholly outside.
    #[inline]
    pub fn holds(&self, sphere: &[f32; 4]) -> bool {
        self.planes.iter().all(|p| p[0] * sphere[0] + p[1] * sphere[1] + p[2] * sphere[2] + p[3] >= -sphere[3])
    }
}

struct Out(*mut u8);
// SAFETY: each participant writes only the stretch of the pieces it is
// given, and the pieces are apart.
unsafe impl Sync for Out {}

/// Writes 1 into `visible[i]` where `frustum` holds `spheres[i]`, 0 where
/// not, on `pool`, `grain` spheres a piece. The two slices are of one length.
pub fn cull_spheres(pool: &WorkerPool, frustum: &Frustum, spheres: &[[f32; 4]], visible: &mut [u8], grain: usize) -> RunStats {
    assert_eq!(spheres.len(), visible.len());
    let out = Out(visible.as_mut_ptr());
    pool.parallel_for(spheres.len(), grain, &|piece, _| {
        let out = &out;
        // SAFETY: `piece` lies in `visible`, and no other call has it.
        let part = unsafe { std::slice::from_raw_parts_mut(out.0.add(piece.start), piece.len()) };
        for (seen, sphere) in part.iter_mut().zip(&spheres[piece]) {
            *seen = u8::from(frustum.holds(sphere));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view down +z from the origin: 90 degrees, square, near 1, far 100.
    fn view() -> Frustum {
        let (near, far) = (1.0f32, 100.0f32);
        let q = far / (far - near);
        Frustum::from_view_projection([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, q, 1.0], [0.0, 0.0, -q * near, 0.0]])
    }

    #[test]
    fn spheres_inside_are_kept_and_those_outside_are_not() {
        let f = view();
        assert!(f.holds(&[0.0, 0.0, 50.0, 1.0]));
        assert!(f.holds(&[40.0, 0.0, 50.0, 1.0]), "inside the 45 degree side");
        assert!(!f.holds(&[60.0, 0.0, 50.0, 1.0]), "outside it");
        assert!(f.holds(&[53.0, 0.0, 50.0, 5.0]), "outside, but reaching in");
        assert!(!f.holds(&[0.0, 0.0, -5.0, 1.0]), "behind the eye");
        assert!(f.holds(&[0.0, 0.0, 0.5, 1.0]), "reaching past the near plane");
        assert!(!f.holds(&[0.0, 0.0, 110.0, 5.0]), "past the far plane");
        assert!(!f.holds(&[0.0, -70.0, 50.0, 2.0]));
    }

    #[test]
    fn the_pool_finds_what_one_thread_finds() {
        let f = view();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let spheres: Vec<[f32; 4]> = (0..100_003).map(|_| [next() * 300.0 - 150.0, next() * 300.0 - 150.0, next() * 260.0 - 80.0, next() * 6.0]).collect();
        let expected: Vec<u8> = spheres.iter().map(|s| u8::from(f.holds(s))).collect();
        let kept = expected.iter().filter(|&&v| v == 1).count();
        assert!(kept > 3_000 && kept < 80_000, "{kept} of {}", spheres.len());
        for workers in [1, 4, 16] {
            let pool = WorkerPool::new(workers);
            let mut visible = vec![7u8; spheres.len()];
            let stats = cull_spheres(&pool, &f, &spheres, &mut visible, 1024);
            assert_eq!(visible, expected, "{workers} workers");
            assert_eq!(stats.workers.iter().map(|w| w.items).sum::<usize>(), spheres.len());
        }
    }
}

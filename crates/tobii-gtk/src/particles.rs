//! Pure math for the "particle burst" explosion animation shown when a
//! calibration point is captured (drawn by a later task's cairo code — this
//! module has no GTK/cairo dependency, only geometry and a small deterministic
//! PRNG).
//!
//! `burst(seed)` produces a fixed set of particles flying outward from the
//! capture point; `particle_pos(t, p)` maps a normalized animation progress
//! `t` to that particle's displacement and fade at that moment. Keeping this
//! pure and seed-deterministic means a retried calibration point reproduces
//! the exact same burst rather than a new random flicker, and the whole thing
//! is unit-testable without a display.

use std::f64::consts::TAU;

/// Number of particles in one burst.
pub const PARTICLE_COUNT: usize = 10;

/// Speed multiplier range (relative units, see `particle_pos`'s `SPEED_SCALE`
/// for how this maps to on-screen pixels): centered on 1.0 with +/-50% spread
/// so the burst doesn't look mechanically uniform, while every particle still
/// travels a comparable distance.
const SPEED_MIN: f64 = 0.5;
const SPEED_MAX: f64 = 1.5;

/// Rendering size range (px, consumed by the Task 9 cairo draw code): small
/// dots with enough spread to read as varied specks rather than uniform
/// circles.
const SIZE_MIN: f64 = 2.0;
const SIZE_MAX: f64 = 6.0;

/// Pixels of outward travel per unit of `speed` at `t = 1.0`. Chosen so a
/// `speed = 1.0` particle travels ~60px over the ~0.6s burst referenced in the
/// task brief — enough to clearly leave the calibration dot without flying off
/// a typical stimulus's immediate surroundings.
const DISTANCE_SCALE: f64 = 60.0;

/// One outward-flying particle of a burst.
///
/// `angle` is the direction of travel in radians (`0` = along +x, increasing
/// counter-clockwise, standard math convention), `speed` is a per-particle
/// outward-motion multiplier, and `size` is a per-particle rendering size in
/// pixels (interpreted by the caller, e.g. as a circle radius).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub angle: f64,
    pub speed: f64,
    pub size: f64,
}

/// Splitmix64 mix step: deterministic, well-distributed, public-domain
/// constants (Sebastiano Vigna's splitmix64). Rolled locally rather than
/// pulling in the `rand` crate for one call site, mirroring this crate's
/// existing precedent of a small local deterministic hash for a similar need
/// (`tobii_config::DisplaySetup::fingerprint`, which folds a `DefaultHasher`
/// over its fields instead of adding a hashing dependency).
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Map a raw `u64` to a `f64` in `[0, 1)`, using the top 53 bits (an `f64`'s
/// mantissa width) so every output bit is well-mixed.
fn unit_f64(bits: u64) -> f64 {
    (bits >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// Deterministic burst of [`PARTICLE_COUNT`] particles for `seed`.
///
/// Same `seed` always produces bit-identical output, so a retried
/// calibration point replays the same-looking burst instead of a new random
/// one, and the animation is testable without capturing real device input.
pub fn burst(seed: u64) -> [Particle; PARTICLE_COUNT] {
    let mut state = seed;
    std::array::from_fn(|_| {
        let angle = unit_f64(splitmix64(&mut state)) * TAU;
        let speed = SPEED_MIN + unit_f64(splitmix64(&mut state)) * (SPEED_MAX - SPEED_MIN);
        let size = SIZE_MIN + unit_f64(splitmix64(&mut state)) * (SIZE_MAX - SIZE_MIN);
        Particle { angle, speed, size }
    })
}

/// Position and opacity of particle `p` at normalized animation progress `t`
/// (`0.0` = burst start, at the capture point, full opacity; `1.0` = animation
/// done, fully faded).
///
/// Returns `(dx, dy, alpha)`: `dx`/`dy` are the outward displacement in pixels
/// from the burst's origin, and `alpha` is a `1.0 -> 0.0` linear fade (simplest
/// choice that satisfies "starts opaque, ends invisible"; the brief allows an
/// eased curve instead but a linear fade over a ~0.6s burst does not need one).
/// Motion is purely radial and scales with `t`, so distance from the origin is
/// monotonically non-decreasing as `t` increases from 0 to 1.
pub fn particle_pos(t: f64, p: Particle) -> (f64, f64, f64) {
    let dist = p.speed * t * DISTANCE_SCALE;
    let dx = p.angle.cos() * dist;
    let dy = p.angle.sin() * dist;
    let alpha = 1.0 - t;
    (dx, dy, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_is_deterministic() {
        let a = burst(42);
        let b = burst(42);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_differ() {
        let a = burst(1);
        let b = burst(2);
        assert_ne!(a, b);
    }

    #[test]
    fn particles_stay_within_documented_ranges() {
        for seed in [0, 1, 42, u64::MAX] {
            for p in burst(seed) {
                assert!((0.0..TAU).contains(&p.angle), "angle={}", p.angle);
                assert!(
                    (SPEED_MIN..=SPEED_MAX).contains(&p.speed),
                    "speed={}",
                    p.speed
                );
                assert!((SIZE_MIN..=SIZE_MAX).contains(&p.size), "size={}", p.size);
            }
        }
    }

    #[test]
    fn position_starts_at_origin_full_alpha_and_ends_faded() {
        let p = Particle {
            angle: 0.7,
            speed: 1.2,
            size: 4.0,
        };
        let (dx0, dy0, alpha0) = particle_pos(0.0, p);
        assert!((dx0 - 0.0).abs() < 1e-9);
        assert!((dy0 - 0.0).abs() < 1e-9);
        assert!((alpha0 - 1.0).abs() < 1e-9);

        let (_, _, alpha1) = particle_pos(1.0, p);
        assert!((alpha1 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn particles_move_monotonically_outward() {
        for p in burst(7) {
            let mut prev_dist = 0.0_f64;
            for i in 0..=4 {
                let t = i as f64 / 4.0;
                let (dx, dy, _) = particle_pos(t, p);
                let dist = (dx * dx + dy * dy).sqrt();
                assert!(
                    dist >= prev_dist - 1e-9,
                    "distance decreased at t={t}: {dist} < {prev_dist}"
                );
                prev_dist = dist;
            }
        }
    }
}

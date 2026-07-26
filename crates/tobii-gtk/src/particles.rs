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

/// Per-particle "blend toward white" factor range (consumed by the cairo draw
/// code): `0.0` renders the particle in the plain base teal, `0.5` blends it
/// halfway to white. Keeps the burst from reading as perfectly uniform dots.
const BRIGHTNESS_MIN: f64 = 0.0;
const BRIGHTNESS_MAX: f64 = 0.5;

/// Pixels of outward travel per unit of `speed` at `t = 1.0`. Chosen so a
/// `speed = 1.0` particle travels ~60px over the ~0.6s burst referenced in the
/// task brief — enough to clearly leave the calibration dot without flying off
/// a typical stimulus's immediate surroundings.
///
/// `pub` so the cairo draw code (`calibrate_flow::draw_scene`) can scale its
/// own shockwave-ring effect off this exact constant instead of hardcoding a
/// second magic number that could silently drift out of sync with the
/// particles' own travel distance.
pub const DISTANCE_SCALE: f64 = 60.0;

/// One outward-flying particle of a burst.
///
/// `angle` is the direction of travel in radians (`0` = along +x, increasing
/// counter-clockwise, standard math convention), `speed` is a per-particle
/// outward-motion multiplier, `size` is a per-particle rendering size in
/// pixels (interpreted by the caller, e.g. as a circle radius), and
/// `brightness` is a per-particle "blend toward white" factor in
/// `[0.0, 0.5]` (`0.0` = plain base color, `0.5` = halfway to white) that the
/// draw code uses to give particles some color/brightness variation instead
/// of perfectly uniform dots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub angle: f64,
    pub speed: f64,
    pub size: f64,
    pub brightness: f64,
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
        let brightness =
            BRIGHTNESS_MIN + unit_f64(splitmix64(&mut state)) * (BRIGHTNESS_MAX - BRIGHTNESS_MIN);
        Particle {
            angle,
            speed,
            size,
            brightness,
        }
    })
}

/// Position and opacity of particle `p` at normalized animation progress `t`
/// (`0.0` = burst start, at the capture point, full opacity; `1.0` = animation
/// done, fully faded).
///
/// Returns `(dx, dy, alpha)`: `dx`/`dy` are the outward displacement in pixels
/// from the burst's origin, and `alpha` fades from `1.0` to `0.0`. Both curves
/// are eased rather than linear, so the burst reads as a real explosion
/// instead of a mechanically uniform expansion:
///
/// - Motion uses an ease-out curve (`1 - (1-t)^2`): fast initial motion that
///   decelerates, reaching the exact same total distance at `t = 1.0` as the
///   old plain `speed * t * DISTANCE_SCALE` linear formula did — only the
///   curve getting there changes, not the resting distance.
/// - Alpha uses an ease-in fade (`1 - t^2`): stays close to fully opaque for
///   longer, then drops off faster near the end, instead of fading evenly
///   from the very first frame.
///
/// Motion is purely radial and still monotonically non-decreasing in `t` (the
/// ease-out curve is itself monotonic over `[0, 1]`), so distance from the
/// origin never decreases as `t` increases from 0 to 1.
pub fn particle_pos(t: f64, p: Particle) -> (f64, f64, f64) {
    let eased_t = 1.0 - (1.0 - t) * (1.0 - t);
    let dist = p.speed * eased_t * DISTANCE_SCALE;
    let dx = p.angle.cos() * dist;
    let dy = p.angle.sin() * dist;
    let alpha = (1.0 - t * t).max(0.0);
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
                assert!(
                    (BRIGHTNESS_MIN..=BRIGHTNESS_MAX).contains(&p.brightness),
                    "brightness={}",
                    p.brightness
                );
            }
        }
    }

    #[test]
    fn brightness_is_deterministic_per_seed() {
        // Same property as `burst_is_deterministic`, but called out separately
        // by name for the new field: a regression that reorders/adds a PRNG
        // draw could shift `brightness` alone while leaving angle/speed/size
        // (and thus `burst_is_deterministic`'s `assert_eq!`) unaffected on
        // some seeds, so this gets its own explicit check.
        let a = burst(99);
        let b = burst(99);
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert!((pa.brightness - pb.brightness).abs() < 1e-12);
        }
    }

    #[test]
    fn position_starts_at_origin_full_alpha_and_ends_faded() {
        let p = Particle {
            angle: 0.7,
            speed: 1.2,
            size: 4.0,
            brightness: 0.25,
        };
        let (dx0, dy0, alpha0) = particle_pos(0.0, p);
        assert!((dx0 - 0.0).abs() < 1e-9);
        assert!((dy0 - 0.0).abs() < 1e-9);
        assert!((alpha0 - 1.0).abs() < 1e-9);

        let (_, _, alpha1) = particle_pos(1.0, p);
        assert!((alpha1 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn eased_motion_reaches_same_endpoint_distance_as_old_linear_formula() {
        let p = Particle {
            angle: 0.3,
            speed: 1.4,
            size: 3.0,
            brightness: 0.1,
        };
        let (dx, dy, alpha) = particle_pos(1.0, p);
        let dist = (dx * dx + dy * dy).sqrt();
        // What the old `p.speed * t * DISTANCE_SCALE` linear formula gave at
        // t=1.0 — the eased curve must still land on the same resting
        // distance, changing only the motion curve getting there.
        let old_linear_dist_at_1 = p.speed * DISTANCE_SCALE;
        assert!(
            (dist - old_linear_dist_at_1).abs() < 1e-9,
            "dist={dist} old_linear_dist_at_1={old_linear_dist_at_1}"
        );
        assert!((alpha - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ease_out_motion_covers_more_than_half_distance_by_half_time() {
        let p = Particle {
            angle: 1.1,
            speed: 1.0,
            size: 5.0,
            brightness: 0.4,
        };
        let dist_at = |t: f64| {
            let (dx, dy, _) = particle_pos(t, p);
            (dx * dx + dy * dy).sqrt()
        };
        let half = dist_at(0.5);
        let full = dist_at(1.0);
        assert!(
            half > 0.5 * full,
            "ease-out motion should be more than half covered by t=0.5: half={half} full={full}"
        );
    }

    #[test]
    fn ease_in_fade_lingers_above_the_old_linear_midpoint() {
        let p = Particle {
            angle: 2.0,
            speed: 0.8,
            size: 2.5,
            brightness: 0.0,
        };
        let (_, _, alpha_half) = particle_pos(0.5, p);
        assert!(
            alpha_half > 0.5,
            "ease-in fade should still read above the old linear formula's 0.5 at t=0.5: {alpha_half}"
        );
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

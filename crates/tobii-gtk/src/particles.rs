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

/// Number of particles in one burst. Matches the decompiled original's
/// `GenerateParticles()` (`Tobii.Configuration.Common.Calibration.Views.
/// CalibrationProcessView`), which allocates a fixed `ParticleTarget[20]`.
pub const PARTICLE_COUNT: usize = 20;

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

/// Per-particle fade-start delay range, as a fraction of the whole burst's
/// normalized `t` timeline (see `particle_pos`): the particle stays fully
/// opaque until `t` passes its own `fade_delay`, then fades linearly. Mirrors
/// the decompiled original's per-particle `Rand.Next(400)` `FadeBeginTime`
/// (0-399ms) staggered within its overall ~800ms burst lifetime — a
/// proportionally similar `0.0..=0.5` fraction of the total duration.
const FADE_DELAY_MIN: f64 = 0.0;
const FADE_DELAY_MAX: f64 = 0.5;

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
/// of perfectly uniform dots, and `fade_delay` is a per-particle fraction of
/// the burst's `t` timeline (`[0.0, 0.5]`) that the particle stays fully
/// opaque for before it starts fading — see `particle_pos`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub angle: f64,
    pub speed: f64,
    pub size: f64,
    pub brightness: f64,
    pub fade_delay: f64,
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
        let fade_delay =
            FADE_DELAY_MIN + unit_f64(splitmix64(&mut state)) * (FADE_DELAY_MAX - FADE_DELAY_MIN);
        Particle {
            angle,
            speed,
            size,
            brightness,
            fade_delay,
        }
    })
}

/// Position and opacity of particle `p` at normalized animation progress `t`
/// (`0.0` = burst start, at the capture point, full opacity; `1.0` = animation
/// done, fully faded).
///
/// Returns `(dx, dy, alpha)`: `dx`/`dy` are the outward displacement in pixels
/// from the burst's origin, and `alpha` fades from `1.0` to `0.0`. This
/// mirrors the decompiled original's actual per-particle storyboard
/// (`CalibrationProcessStoryboardFactory.CreateParticleCalibratedAnimation`)
/// rather than a synchronized, uniformly-eased approximation:
///
/// - Motion uses a `CircleEase`-EaseOut curve (`sqrt(1 - (1-t)^2)`): a
///   sharper, more front-loaded deceleration than a plain quadratic ease-out,
///   reaching the exact same total distance at `t = 1.0` (`sqrt(1-0) = 1`) as
///   the old plain `speed * t * DISTANCE_SCALE` linear formula did — only the
///   curve getting there changes, not the resting distance. `eased_t = 0.0`
///   at `t = 0.0` (`sqrt(1-1) = 0`), so the particle still starts at the
///   origin.
/// - Alpha stays fully opaque (`1.0`) until `t` passes the particle's own
///   `fade_delay`, then fades **linearly** (matching the original's
///   `DoubleAnimation` with no `EasingFunction`, i.e. WPF's default linear
///   interpolation) down to `0.0` over the remaining `1.0 - fade_delay`
///   fraction of the timeline. Because every particle in a burst has its own
///   random `fade_delay`, particles fade out at staggered times rather than
///   all together.
///
/// Motion is purely radial and still monotonically non-decreasing in `t` (the
/// ease-out curve is itself monotonic over `[0, 1]`), so distance from the
/// origin never decreases as `t` increases from 0 to 1.
pub fn particle_pos(t: f64, p: Particle) -> (f64, f64, f64) {
    let eased_t = (1.0 - (1.0 - t) * (1.0 - t)).sqrt();
    let dist = p.speed * eased_t * DISTANCE_SCALE;
    let dx = p.angle.cos() * dist;
    let dy = p.angle.sin() * dist;
    let alpha = if t < p.fade_delay {
        1.0
    } else {
        let fade_t = (t - p.fade_delay) / (1.0 - p.fade_delay);
        (1.0 - fade_t).max(0.0)
    };
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
                assert!(
                    (FADE_DELAY_MIN..=FADE_DELAY_MAX).contains(&p.fade_delay),
                    "fade_delay={}",
                    p.fade_delay
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
    fn fade_delay_is_deterministic_per_seed() {
        // Same property as `burst_is_deterministic`/`brightness_is_deterministic_per_seed`,
        // called out separately for `fade_delay` since it's the newest field
        // and a regression that reorders/adds a PRNG draw could shift it
        // alone while leaving the others unaffected on some seeds.
        let a = burst(99);
        let b = burst(99);
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert!((pa.fade_delay - pb.fade_delay).abs() < 1e-12);
        }
    }

    #[test]
    fn position_starts_at_origin_full_alpha_and_ends_faded() {
        let p = Particle {
            angle: 0.7,
            speed: 1.2,
            size: 4.0,
            brightness: 0.25,
            fade_delay: 0.0,
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
            fade_delay: 0.2,
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
            fade_delay: 0.1,
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
    fn alpha_stays_fully_opaque_before_its_fade_delay() {
        // Mirrors the original's `DoubleAnimation.BeginTime = FadeBeginTime`:
        // the particle does not start fading at all until `t` passes its own
        // `fade_delay`.
        let p = Particle {
            angle: 2.0,
            speed: 0.8,
            size: 2.5,
            brightness: 0.0,
            fade_delay: 0.4,
        };
        let (_, _, alpha) = particle_pos(0.3, p);
        assert!(
            (alpha - 1.0).abs() < 1e-9,
            "particle should still be fully opaque before its fade_delay elapses: {alpha}"
        );
    }

    #[test]
    fn alpha_fades_linearly_once_past_fade_delay() {
        // Direct behavioral proof the fade is no longer eased: with
        // `fade_delay: 0.0` the fade starts immediately, so at the burst's
        // halfway point a LINEAR fade should read close to 0.5 — not the old
        // ease-in curve's 0.75 (`1 - 0.5^2`).
        let p = Particle {
            angle: 2.0,
            speed: 0.8,
            size: 2.5,
            brightness: 0.0,
            fade_delay: 0.0,
        };
        let (_, _, alpha_half) = particle_pos(0.5, p);
        assert!(
            (alpha_half - 0.5).abs() < 1e-9,
            "linear fade with fade_delay=0.0 should read ~0.5 at t=0.5, not the old ease-in 0.75: {alpha_half}"
        );
    }

    #[test]
    fn higher_fade_delay_stays_more_opaque_at_the_same_t() {
        // Direct proof of staggering: two particles differing only in
        // `fade_delay`, evaluated at the same `t`, must show the higher-delay
        // one strictly more opaque.
        let low_delay = Particle {
            angle: 0.0,
            speed: 1.0,
            size: 3.0,
            brightness: 0.0,
            fade_delay: 0.0,
        };
        let high_delay = Particle {
            fade_delay: 0.4,
            ..low_delay
        };
        let (_, _, alpha_low) = particle_pos(0.6, low_delay);
        let (_, _, alpha_high) = particle_pos(0.6, high_delay);
        assert!(
            alpha_high > alpha_low,
            "higher fade_delay should still be more opaque at the same t: alpha_low={alpha_low} alpha_high={alpha_high}"
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

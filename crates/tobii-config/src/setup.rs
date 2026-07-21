//! Planar display-setup geometry (Spike S3 "Model B").
//!
//! Convention: tracker-space mm, right-handed (+X right, +Y up, +Z backward).
//! Tilt is a screen lean-back angle in degrees (+ = top edge toward +Z). The
//! top edge is level (no roll/yaw). See
//! `docs/superpowers/specs/2026-07-14-spike-s3-display-setup-math.md`.

use std::f64::consts::FRAC_PI_2;

use tobii_protocol::DisplayCorners;

/// Physical display-setup parameters a user edits. Lengths in millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplaySetup {
    /// Active-area width as the **chord**: the straight, corner-to-corner
    /// distance along tracker +X — what a rigid ruler laid across the screen
    /// measures, and what [`to_corners`](DisplaySetup::to_corners) needs, since
    /// the device is only ever told about a flat plane.
    ///
    /// On a curved panel this is *not* the width EDID reports: a curved screen
    /// is a flat sheet bent into an arc, so EDID gives the **arc** length. Only
    /// EDID-sourced widths need converting — see [`chord_from_arc`].
    pub width_mm: f64,
    /// Active-area height along the screen surface (the tilted side-edge length).
    pub height_mm: f64,
    /// Screen tilt back from vertical, degrees; + = top edge leans toward +Z.
    pub tilt_deg: f64,
    /// Horizontal offset of the screen centre from the tracker (usually 0).
    pub offset_x_mm: f64,
    /// Height of the screen's bottom edge above the tracker.
    pub offset_y_mm: f64,
    /// Depth of the screen's bottom edge from the tracker.
    pub offset_z_mm: f64,
    /// Panel curve radius in mm from the monitor's spec sheet ("1800R" → 1800).
    /// `0.0` means a flat screen — the default, and what every pre-curvature
    /// config on disk parses as. Curvature is cylindrical with a vertical axis,
    /// bulging away from the viewer. It never changes what we send the device
    /// (which only accepts a plane); it drives [`crate::correct_gaze_x`].
    pub curvature_radius_mm: f64,
}

/// Chord (straight-line) length of an arc of `arc_mm` on a circle of
/// `radius_mm`: `2R·sin(arc / 2R)`. Returns `arc_mm` unchanged for a
/// non-positive or non-finite radius (i.e. a flat screen).
pub fn chord_from_arc(arc_mm: f64, radius_mm: f64) -> f64 {
    if !radius_mm.is_finite() || radius_mm <= 0.0 || !arc_mm.is_finite() {
        return arc_mm;
    }
    // Beyond a half turn the chord shrinks again; clamp so the answer stays
    // monotonic and never exceeds the diameter.
    let half = (arc_mm / (2.0 * radius_mm)).clamp(-FRAC_PI_2, FRAC_PI_2);
    2.0 * radius_mm * half.sin()
}

/// Arc length subtending a chord of `chord_mm` on a circle of `radius_mm`:
/// `2R·asin(chord / 2R)`. Returns `chord_mm` unchanged for a non-positive or
/// non-finite radius; a chord longer than the diameter has no solution, so it
/// is clamped to the diameter (giving the half-circumference).
pub fn arc_from_chord(chord_mm: f64, radius_mm: f64) -> f64 {
    if !radius_mm.is_finite() || radius_mm <= 0.0 || !chord_mm.is_finite() {
        return chord_mm;
    }
    let ratio = (chord_mm / (2.0 * radius_mm)).clamp(-1.0, 1.0);
    2.0 * radius_mm * ratio.asin()
}

impl DisplaySetup {
    /// Forward construction: parameters → the three tracker-space corners
    /// (TL, TR, BL). Bottom-right is implied by the device.
    pub fn to_corners(&self) -> DisplayCorners {
        let tilt = self.tilt_deg.to_radians();
        let tilt_mm = self.height_mm * tilt.sin();
        let dy = self.height_mm * tilt.cos();
        let half_w = self.width_mm / 2.0;
        let (cx, cy, cz) = (self.offset_x_mm, self.offset_y_mm, self.offset_z_mm);
        DisplayCorners {
            bl: [cx - half_w, cy, cz],
            tl: [cx - half_w, cy + dy, cz + tilt_mm],
            tr: [cx + half_w, cy + dy, cz + tilt_mm],
        }
    }

    /// Inverse: a device-reported (or edited) set of corners → editable params.
    ///
    /// Corners describe a plane, so they carry no curvature information:
    /// `curvature_radius_mm` always comes back as `0.0`.
    pub fn from_corners(c: &DisplayCorners) -> DisplaySetup {
        let dy = c.tl[1] - c.bl[1];
        let dz = c.tl[2] - c.bl[2];
        DisplaySetup {
            width_mm: c.tr[0] - c.tl[0],
            height_mm: dy.hypot(dz),
            tilt_deg: dz.atan2(dy).to_degrees(),
            offset_x_mm: (c.tl[0] + c.tr[0]) / 2.0,
            offset_y_mm: c.bl[1],
            offset_z_mm: c.bl[2],
            curvature_radius_mm: 0.0,
        }
    }

    /// Serialize to a `[display]` TOML section.
    pub fn to_toml(&self) -> String {
        format!(
            "# tobii-linux display setup — edit with `tobii setup`\n\
             [display]\n\
             width_mm = {}\n\
             height_mm = {}\n\
             tilt_deg = {}\n\
             offset_x_mm = {}\n\
             offset_y_mm = {}\n\
             offset_z_mm = {}\n\
             curvature_radius_mm = {}\n",
            self.width_mm,
            self.height_mm,
            self.tilt_deg,
            self.offset_x_mm,
            self.offset_y_mm,
            self.offset_z_mm,
            self.curvature_radius_mm,
        )
    }

    /// Parse a `[display]` TOML section. Returns `None` unless all six geometry
    /// keys are present and parse as `f64`. `curvature_radius_mm` is optional
    /// and defaults to `0.0` (flat), so configs written before curvature
    /// existed keep loading. Ignores comments, blanks, other sections.
    pub fn from_toml(s: &str) -> Option<DisplaySetup> {
        let mut in_display = false;
        let (mut w, mut h, mut t) = (None, None, None);
        let (mut ox, mut oy, mut oz) = (None, None, None);
        let mut curve = 0.0;
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_display = line == "[display]";
                continue;
            }
            if !in_display {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let val = val.split('#').next().unwrap_or("").trim();
            let Ok(v) = val.parse::<f64>() else {
                continue;
            };
            match key.trim() {
                "width_mm" => w = Some(v),
                "height_mm" => h = Some(v),
                "tilt_deg" => t = Some(v),
                "offset_x_mm" => ox = Some(v),
                "offset_y_mm" => oy = Some(v),
                "offset_z_mm" => oz = Some(v),
                "curvature_radius_mm" => curve = v,
                _ => {}
            }
        }
        Some(DisplaySetup {
            width_mm: w?,
            height_mm: h?,
            tilt_deg: t?,
            offset_x_mm: ox?,
            offset_y_mm: oy?,
            offset_z_mm: oz?,
            curvature_radius_mm: curve,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::DisplayCorners;

    // Spike S3 golden vector (a real, working display area).
    const GOLDEN: DisplayCorners = DisplayCorners {
        tl: [-451.8, 413.6, 157.5],
        tr: [479.8, 413.6, 157.5],
        bl: [-451.8, 68.0, -11.0],
    };

    #[test]
    fn flat_untilted_rectangle() {
        let s = DisplaySetup {
            width_mm: 400.0,
            height_mm: 300.0,
            tilt_deg: 0.0,
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
            offset_z_mm: 0.0,
            curvature_radius_mm: 0.0,
        };
        let c = s.to_corners();
        assert_eq!(c.bl, [-200.0, 0.0, 0.0]);
        assert_eq!(c.tl, [-200.0, 300.0, 0.0]);
        assert_eq!(c.tr, [200.0, 300.0, 0.0]);
    }

    #[test]
    fn tilt_preserves_edge_length_and_pushes_top_back() {
        let s = DisplaySetup {
            width_mm: 500.0,
            height_mm: 300.0,
            tilt_deg: 30.0,
            offset_x_mm: 10.0,
            offset_y_mm: 50.0,
            offset_z_mm: -5.0,
            curvature_radius_mm: 0.0,
        };
        let c = s.to_corners();
        // Bottom edge is unaffected by tilt.
        assert!((c.bl[0] - (10.0 - 250.0)).abs() < 1e-9);
        assert!((c.bl[1] - 50.0).abs() < 1e-9);
        assert!((c.bl[2] - (-5.0)).abs() < 1e-9);
        // Side-edge length is preserved (== height).
        let dy = c.tl[1] - c.bl[1];
        let dz = c.tl[2] - c.bl[2];
        assert!(((dy * dy + dz * dz).sqrt() - 300.0).abs() < 1e-9);
        // z displacement == height * sin(tilt).
        assert!((dz - 300.0 * 30f64.to_radians().sin()).abs() < 1e-9);
        // Width is preserved and the top edge is level.
        assert!((c.tr[0] - c.tl[0] - 500.0).abs() < 1e-9);
        assert!((c.tl[1] - c.tr[1]).abs() < 1e-9);
        assert!((c.tl[2] - c.tr[2]).abs() < 1e-9);
    }

    #[test]
    fn from_corners_recovers_golden_params() {
        let s = DisplaySetup::from_corners(&GOLDEN);
        assert!((s.width_mm - 931.6).abs() < 0.05);
        assert!((s.height_mm - 384.489).abs() < 0.05);
        assert!((s.tilt_deg - 26.0).abs() < 0.05);
        assert!((s.offset_x_mm - 14.0).abs() < 0.05);
        assert!((s.offset_y_mm - 68.0).abs() < 1e-9);
        assert!((s.offset_z_mm - (-11.0)).abs() < 1e-9);
    }

    #[test]
    fn corners_setup_roundtrip_is_exact() {
        let s = DisplaySetup::from_corners(&GOLDEN);
        let c = s.to_corners();
        for i in 0..3 {
            assert!((c.tl[i] - GOLDEN.tl[i]).abs() < 1e-6);
            assert!((c.tr[i] - GOLDEN.tr[i]).abs() < 1e-6);
            assert!((c.bl[i] - GOLDEN.bl[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn toml_roundtrips() {
        let s = DisplaySetup {
            width_mm: 931.6,
            height_mm: 384.5,
            tilt_deg: 26.0,
            offset_x_mm: 14.0,
            offset_y_mm: 68.0,
            offset_z_mm: -11.0,
            curvature_radius_mm: 1800.0,
        };
        let text = s.to_toml();
        assert!(text.contains("[display]"));
        assert_eq!(DisplaySetup::from_toml(&text), Some(s));
    }

    #[test]
    fn from_toml_ignores_comments_blanks_and_inline_comments() {
        let text = "# my monitor\n\n\
                    [display]\n\
                    width_mm = 800.0   # active area\n\
                    height_mm = 335.0\n\
                    tilt_deg = 20.0\n\
                    offset_x_mm = 0.0\n\
                    offset_y_mm = 40.0\n\
                    offset_z_mm = -5.0\n";
        let s = DisplaySetup::from_toml(text).expect("parse");
        assert_eq!(s.width_mm, 800.0);
        assert_eq!(s.offset_z_mm, -5.0);
    }

    #[test]
    fn from_toml_missing_key_is_none() {
        let text = "[display]\nwidth_mm = 800.0\n";
        assert_eq!(DisplaySetup::from_toml(text), None);
    }

    /// Every config.toml written before curvature existed must keep working,
    /// and must mean "flat".
    #[test]
    fn legacy_toml_without_curvature_parses_as_flat() {
        let text = "# tobii-linux display setup\n\
                    [display]\n\
                    width_mm = 931.6\n\
                    height_mm = 384.5\n\
                    tilt_deg = 26.0\n\
                    offset_x_mm = 14.0\n\
                    offset_y_mm = 68.0\n\
                    offset_z_mm = -11.0\n";
        let s = DisplaySetup::from_toml(text).expect("legacy config still parses");
        assert_eq!(s.curvature_radius_mm, 0.0);
        assert_eq!(s.width_mm, 931.6);
    }

    #[test]
    fn odyssey_g93sc_arc_to_chord() {
        // 49" 1800R: EDID reports the 1193 mm arc; the ruler chord is ~1171 mm.
        let chord = chord_from_arc(1193.0, 1800.0);
        assert!((chord - 1171.0).abs() < 1.0, "chord={chord}");
        // Sagitta ≈ 98 mm — the depth the flat plane model throws away.
        let half_w = chord / 2.0;
        let sagitta = 1800.0 - (1800.0f64 * 1800.0 - half_w * half_w).sqrt();
        assert!((sagitta - 98.0).abs() < 1.0, "sagitta={sagitta}");
    }

    #[test]
    fn arc_chord_roundtrips() {
        for &(arc, r) in &[(1193.0, 1800.0), (800.0, 1000.0), (600.0, 4000.0)] {
            let back = arc_from_chord(chord_from_arc(arc, r), r);
            assert!((back - arc).abs() < 1e-9, "arc={arc} r={r} back={back}");
        }
    }

    #[test]
    fn zero_or_negative_radius_is_identity() {
        for &r in &[0.0, -1.0, -1800.0, f64::NAN, f64::INFINITY] {
            assert_eq!(chord_from_arc(1193.0, r), 1193.0);
            assert_eq!(arc_from_chord(1171.0, r), 1171.0);
        }
    }

    #[test]
    fn chord_is_never_longer_than_the_arc_or_the_diameter() {
        // A very tight curve: the chord shortens, and clamps at the diameter.
        assert!(chord_from_arc(1193.0, 200.0) <= 400.0);
        assert!(chord_from_arc(1193.0, 1800.0) < 1193.0);
        // Impossible chord (longer than the diameter) clamps instead of NaN.
        let a = arc_from_chord(5000.0, 1000.0);
        assert!(a.is_finite() && (a - std::f64::consts::PI * 1000.0).abs() < 1e-9);
    }

    /// A vanishingly gentle curve is indistinguishable from flat.
    #[test]
    fn huge_radius_tends_to_identity() {
        assert!((chord_from_arc(1193.0, 1e9) - 1193.0).abs() < 1e-3);
    }
}

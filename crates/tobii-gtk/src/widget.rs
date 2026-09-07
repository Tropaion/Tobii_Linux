//! GTK/cairo rendering + pure presentation helpers over the `eyeview` data.
//! The pure helpers (`eye_view_for`, `guidance_message`) are
//! unit-tested; `draw_eye_view` is the cairo drawing (live-validated).

use gtk::cairo;
use gtk::prelude::*;

use crate::device::{ConnStatus, DeviceState};
use crate::eyeview::{EyeView, Guidance};

/// Inset, in pixels, from a drawing area's edge to the drawn trackbox rectangle.
/// Callers sizing a container that should hold a specific trackbox *aspect* must
/// add `2 * EYE_VIEW_PAD` to both axes to compensate.
pub const EYE_VIEW_PAD: f64 = 10.0;

/// Width/height of the drawn trackbox. The original's own
/// `SetTrackBoxToGoldenRatio`.
pub const EYE_VIEW_RATIO: f64 = 1.618;

/// The rectangle the trackbox is drawn into: the largest golden-ratio box that
/// fits inside `w`×`h` after [`EYE_VIEW_PAD`], centred.
///
/// **The aspect is not decoration.** The trackbox is a volume in front of the
/// sensor, and the normalized `[0,1]` coordinates are stretched to fill this
/// rectangle — so its shape sets the ratio of horizontal to vertical motion
/// gain. Drawn into a widget's raw allocation on a 3.56:1 monitor, the box came
/// out 4.1:1 and compressed vertical head movement ~2.6x against horizontal,
/// which reads as a dot that is sluggish vertically and twitchy horizontally.
///
/// Letterboxing here rather than pinning the widget to a fixed pixel size is
/// what lets the panel resize with the window and still be honest.
///
/// Both [`draw_eye_view`] and [`draw_head_overlay`] map through this, so they
/// cannot disagree about where the box is.
pub fn eye_view_rect(w: f64, h: f64) -> (f64, f64, f64, f64) {
    let avail_w = (w - 2.0 * EYE_VIEW_PAD).max(0.0);
    let avail_h = (h - 2.0 * EYE_VIEW_PAD).max(0.0);
    if avail_w <= 0.0 || avail_h <= 0.0 {
        return (w / 2.0, h / 2.0, 0.0, 0.0);
    }
    let (rw, rh) = if avail_w / avail_h > EYE_VIEW_RATIO {
        (avail_h * EYE_VIEW_RATIO, avail_h)
    } else {
        (avail_w, avail_w / EYE_VIEW_RATIO)
    };
    ((w - rw) / 2.0, (h - rh) / 2.0, rw, rh)
}

/// The `EyeView` to render for a device snapshot: never show stale gaze — force
/// "no eyes" unless the device is connected AND a sample is present.
pub fn eye_view_for(state: &DeviceState) -> EyeView {
    if !matches!(state.status, ConnStatus::Connected) {
        return EyeView::none();
    }
    state.eye_view.unwrap_or_else(EyeView::none)
}

/// Human-readable eye-position guidance line.
///
/// The nudge wording is the original Windows software's own, recovered from its
/// `LanguageResources` string table (`EyesPositioning_FullScreenMessage_*`), so
/// the guidance reads exactly as it does there. `Centered` is the one
/// deliberate departure: the original shows "Press a key to continue" on its
/// fullscreen step, which would be wrong on our always-on hub, so we report the
/// live distance instead.
pub fn guidance_message(view: &EyeView) -> String {
    match view.guidance {
        Guidance::NoEyes => "Are you there?".to_string(),
        Guidance::MoveCloser => "Move closer".to_string(),
        Guidance::MoveBack => "Lean back".to_string(),
        Guidance::MoveRight => "Move right".to_string(),
        Guidance::MoveLeft => "Move left".to_string(),
        Guidance::MoveDown => "Move down".to_string(),
        Guidance::MoveUp => "Move up".to_string(),
        Guidance::Centered => match view.distance_mm {
            Some(d) => format!("Good position ({d:.0} mm)."),
            None => "Good position.".to_string(),
        },
    }
}

/// Draw the trackbox rectangle + both eyes into a cairo context of size `w`×`h`.
///
/// Positions are the mirror-view normalized `[0,1]` coords from `EyeView`, and
/// the three visual channels all mirror the original software's own encoding
/// (see `eyeview`'s module header for provenance):
/// - **size** tracks operating distance — the dot grows as you lean in and
///   shrinks as you move away (`EyeView::eye_size`, `50` being ideal);
/// - **brightness** peaks in the comfortable distance band and dims toward
///   either extreme (`EyeView::brightness`);
/// - **opacity** fades a dot out as it nears the trackbox edge, so a position
///   projected across a tracking gap glides away rather than parking itself
///   against the boundary (`left_alpha`/`right_alpha`).
///
/// Hue is this project's own: green when well-positioned, amber while being
/// nudged. The original encodes that in brightness alone (its dots are white
/// dimming to grey), so keeping hue is a deliberate, additive departure.
pub fn draw_eye_view(cr: &cairo::Context, w: i32, h: i32, view: &EyeView) {
    let (w, h) = (w as f64, h as f64);
    let (rx, ry, rw, rh) = eye_view_rect(w, h);
    if rw <= 0.0 || rh <= 0.0 {
        return;
    }
    let nx = |t: f64| rx + t * rw;
    let ny = |t: f64| ry + t * rh;

    // --- Graticule -------------------------------------------------------
    // The box is a normalized [0,1] volume, and until now it was an empty
    // rectangle: a dot could sit anywhere in it with nothing to read its
    // position against. The grid is at 10% so the eye can count, and it is dim
    // enough to stay behind the data.
    cr.set_line_width(1.0);
    cr.set_source_rgb(0.11, 0.13, 0.15);
    let mut t = 0.1;
    while t < 0.999 {
        cr.move_to(nx(t).floor() + 0.5, ry);
        cr.line_to(nx(t).floor() + 0.5, ry + rh);
        cr.move_to(rx, ny(t).floor() + 0.5);
        cr.line_to(rx + rw, ny(t).floor() + 0.5);
        t += 0.1;
    }
    let _ = cr.stroke();

    // --- Tolerance region -------------------------------------------------
    // 0.1..0.9 on both axes is where the tracker stops nudging (`XY_MIN`,
    // `XY_MAX`). Drawing it makes the guidance text's threshold visible instead
    // of implicit: you can see how much room is left before it complains.
    let (lo, hi) = (crate::eyeview::XY_MIN as f64, crate::eyeview::XY_MAX as f64);
    cr.set_source_rgb(0.20, 0.24, 0.28);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.rectangle(nx(lo), ny(lo), (hi - lo) * rw, (hi - lo) * rh);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    // --- Centre reticle ---------------------------------------------------
    // The target. Short ticks rather than full crosshairs, so it marks the
    // middle without drawing a line through the data.
    let (cx, cy) = (nx(0.5), ny(0.5));
    let tick = (rw.min(rh) * 0.05).clamp(3.0, 9.0);
    cr.set_source_rgb(0.28, 0.32, 0.36);
    cr.move_to(cx - tick, cy);
    cr.line_to(cx + tick, cy);
    cr.move_to(cx, cy - tick);
    cr.line_to(cx, cy + tick);
    let _ = cr.stroke();

    // --- Frame and edge ticks ---------------------------------------------
    cr.set_source_rgb(0.42, 0.45, 0.5);
    cr.set_line_width(1.5);
    cr.rectangle(rx, ry, rw, rh);
    let _ = cr.stroke();
    // Quarter marks on each edge, longer at the midpoint: a scale to read the
    // dots against.
    cr.set_line_width(1.0);
    for (i, t) in [0.25, 0.5, 0.75].iter().enumerate() {
        let len = if i == 1 { 6.0 } else { 3.5 };
        let (x, y) = (nx(*t).floor() + 0.5, ny(*t).floor() + 0.5);
        cr.move_to(x, ry);
        cr.line_to(x, ry + len);
        cr.move_to(x, ry + rh);
        cr.line_to(x, ry + rh - len);
        cr.move_to(rx, y);
        cr.line_to(rx + len, y);
        cr.move_to(rx + rw, y);
        cr.line_to(rx + rw - len, y);
    }
    let _ = cr.stroke();

    // --- The eyes ---------------------------------------------------------
    // `raw_guidance`, not `guidance`: the damping exists to steady the *text*,
    // and colouring from it would leave the dots green for up to ~1.5 s after
    // the user has already moved out of position. See `EyeView`'s doc comment.
    let centered = matches!(view.raw_guidance, Guidance::Centered);
    let (r, g, b) = if centered {
        (0.18, 0.80, 0.55)
    } else {
        (0.95, 0.80, 0.25)
    };
    // Dim toward the distance extremes, but never all the way to invisible —
    // opacity is the channel that carries "gone", brightness only carries
    // "poorly positioned".
    let dim = view.brightness.clamp(0.0, 1.0) as f64;
    let (r, g, b) = (r * dim, g * dim, b * dim);

    // `eye_size` is the original's own 25..100 scale about an ideal of 50.
    let base = (rw.min(rh) * 0.06).clamp(6.0, 22.0);
    let size_scale = if view.eye_size > 0 {
        view.eye_size as f64 / crate::eyeview::EYE_SIZE_DEFAULT as f64
    } else {
        1.0
    };
    let radius = base * size_scale;

    // Plain circles, deliberately. The original renders each eye as a single
    // `Ellipse Stretch="UniformToFill"` in a Grid whose only size binding is
    // `Width={Binding EyeSize}` — a circle of that diameter, with no rotation
    // and no anisotropic scale. It *does* compute a head-tilt `EyeAngle` every
    // frame, but binds it to nothing at all (verified by decoding the view's
    // BAML record stream: `Canvas.Left`/`Top`/`EyeSize`/`LeftEyeColor` are all
    // bound, `EyeAngle` appears nowhere), so the shipped screen never tilts.
    // `EyeView::angle_deg` mirrors that computation for completeness and is
    // likewise unread here: rotating an eccentric shape by an angle rederived
    // from two noisy dot positions makes the dots visibly rock even when the
    // head is still, which reads as instability rather than as head tracking.
    for (eye, alpha) in [(view.left, view.left_alpha), (view.right, view.right_alpha)] {
        let Some(eye) = eye else { continue };
        let ex = rx + (eye[0].clamp(0.0, 1.0) as f64) * rw;
        let ey = ry + (eye[1].clamp(0.0, 1.0) as f64) * rh;
        // A thin drop line to each axis: it turns a floating dot into a reading,
        // and it is what makes an off-centre position legible at a glance.
        cr.set_source_rgba(r, g, b, 0.22 * alpha as f64);
        cr.set_line_width(1.0);
        cr.move_to(ex.floor() + 0.5, ry);
        cr.line_to(ex.floor() + 0.5, ry + rh);
        cr.move_to(rx, ey.floor() + 0.5);
        cr.line_to(rx + rw, ey.floor() + 0.5);
        let _ = cr.stroke();

        cr.set_source_rgba(r, g, b, alpha as f64);
        cr.arc(ex, ey, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

/// Corner radius of the sensor image, matching the `.surface` cards it sits
/// among so the panel reads as one family of shapes.
pub const CAMERA_CORNER_RADIUS: f64 = 10.0;

/// Append a rounded rectangle to the current path.
pub fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let (hp, tau4) = (std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -hp, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, hp);
    cr.arc(x + r, y + h - r, r, hp, tau4);
    cr.arc(x + r, y + r, r, tau4, tau4 + hp);
    cr.close_path();
}

/// Draw the latest NIR camera frame into a cairo context of size `w`×`h`,
/// contrast-stretched (the raw frames are very dark) and letterboxed to preserve
/// the square aspect. Mirrored horizontally so it reads like a mirror.
pub fn draw_camera_view(cr: &cairo::Context, w: i32, h: i32, frame: &tobii_protocol::CameraFrame) {
    let (iw, ih) = (frame.width as i32, frame.height as i32);
    if iw <= 0 || ih <= 0 || frame.pixels.len() < (iw * ih) as usize {
        return;
    }
    // Contrast stretch (min→0, max→255) so the dark IR face is visible.
    let (mut mn, mut mx) = (255u8, 0u8);
    for &p in &frame.pixels {
        mn = mn.min(p);
        mx = mx.max(p);
    }
    let range = (mx.saturating_sub(mn)).max(1) as f32;

    let Ok(stride) = cairo::Format::Rgb24.stride_for_width(iw as u32) else {
        return;
    };
    let mut buf = vec![0u8; (stride * ih) as usize];
    for y in 0..ih {
        for x in 0..iw {
            let p = frame.pixels[(y * iw + x) as usize];
            let v = (((p.saturating_sub(mn)) as f32 / range) * 255.0) as u8;
            let off = (y * stride + x * 4) as usize;
            // Rgb24 is 0x00RRGGBB in a native-endian u32; grayscale ⇒ B=G=R=v.
            buf[off] = v;
            buf[off + 1] = v;
            buf[off + 2] = v;
        }
    }
    let Ok(surface) =
        cairo::ImageSurface::create_for_data(buf, cairo::Format::Rgb24, iw, ih, stride)
    else {
        return;
    };

    let scale = (w as f64 / iw as f64).min(h as f64 / ih as f64);
    let (dw, dh) = (iw as f64 * scale, ih as f64 * scale);
    let (ox, oy) = ((w as f64 - dw) / 2.0, (h as f64 - dh) / 2.0);
    cr.save().ok();
    // Round the image's own corners to match the cards it sits among. Clipping
    // to the LETTERBOXED rect rather than the widget keeps the radius on the
    // picture: rounding the widget would round empty space when the allocation
    // is not square.
    rounded_rect(cr, ox, oy, dw, dh, CAMERA_CORNER_RADIUS);
    cr.clip();
    cr.set_source_rgb(0.05, 0.05, 0.06);
    let _ = cr.paint();
    // Centre, then mirror horizontally (flip x about the image centre).
    cr.translate(ox, oy);
    cr.translate(dw, 0.0);
    cr.scale(-scale, scale);
    let _ = cr.set_source_surface(&surface, 0.0, 0.0);
    let _ = cr.paint();
    cr.restore().ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disconnected device must never leave the last pose on screen looking
    /// live — the same rule `eye_view_for` follows.
    #[test]
    fn a_device_that_is_not_connected_has_no_head_view() {
        let mut s = DeviceState {
            status: ConnStatus::Connected,
            head_pose: Some(tobii_headpose::HeadPose {
                yaw_deg: 5.0,
                ..Default::default()
            }),
            head_sigma: Some(0.07),
            ..Default::default()
        };
        assert!(head_view_for(&s).is_some());
        s.status = ConnStatus::Error("unplugged".into());
        assert!(head_view_for(&s).is_none());
    }

    use crate::eyeview::{EyeView, Guidance};

    fn view(g: Guidance, d: Option<f32>) -> EyeView {
        EyeView {
            distance_mm: d,
            guidance: g,
            ..EyeView::none()
        }
    }

    fn valid_sample() -> tobii_protocol::GazeSample {
        use tobii_protocol::gaze::present;
        tobii_protocol::GazeSample {
            trackbox_eye_l: [0.5, 0.5, 0.5],
            trackbox_eye_r: [0.5, 0.5, 0.5],
            eye_origin_l_mm: [0.0, 0.0, 680.0],
            eye_origin_r_mm: [0.0, 0.0, 680.0],
            present_mask: present::TRACKBOX_L
                | present::TRACKBOX_R
                | present::EYE_ORIGIN_L
                | present::EYE_ORIGIN_R
                | present::VALIDITY_L
                | present::VALIDITY_R,
            validity_l: 0,
            validity_r: 0,
            ..Default::default()
        }
    }

    #[test]
    fn guidance_messages_match_each_state() {
        // Wording is the original software's own (see `guidance_message`).
        assert_eq!(
            guidance_message(&view(Guidance::NoEyes, None)),
            "Are you there?"
        );
        assert_eq!(
            guidance_message(&view(Guidance::MoveCloser, None)),
            "Move closer"
        );
        assert_eq!(
            guidance_message(&view(Guidance::MoveBack, None)),
            "Lean back"
        );
        assert_eq!(
            guidance_message(&view(Guidance::Centered, Some(680.0))),
            "Good position (680 mm)."
        );
    }

    #[test]
    fn guidance_messages_name_every_direction() {
        // The original nudges the user a specific way rather than saying
        // "center yourself", so each direction needs its own line.
        for (g, want) in [
            (Guidance::MoveRight, "Move right"),
            (Guidance::MoveLeft, "Move left"),
            (Guidance::MoveDown, "Move down"),
            (Guidance::MoveUp, "Move up"),
        ] {
            assert_eq!(guidance_message(&view(g, None)), want);
        }
    }

    #[test]
    fn eye_view_for_not_connected_is_no_eyes_even_with_cached_gaze() {
        // A stale sample/view must not render as live when disconnected.
        let mut s = DeviceState {
            status: ConnStatus::Error("unplugged".into()),
            latest_gaze: Some(valid_sample()),
            eye_view: Some(EyeView::from_gaze(&valid_sample())),
            ..Default::default()
        };
        assert!(matches!(eye_view_for(&s).guidance, Guidance::NoEyes));
        // Connected + no view is also "no eyes".
        s.status = ConnStatus::Connected;
        s.latest_gaze = None;
        s.eye_view = None;
        assert!(matches!(eye_view_for(&s).guidance, Guidance::NoEyes));
    }

    fn eyes_at(l: [f32; 2], r: [f32; 2]) -> EyeView {
        EyeView {
            left: Some(l),
            right: Some(r),
            left_alpha: 1.0,
            right_alpha: 1.0,
            ..EyeView::none()
        }
    }

    fn a_head(yaw: f64, pitch: f64, sigma: Option<f32>) -> HeadView {
        HeadView {
            yaw_deg: yaw,
            pitch_deg: pitch,
            roll_deg: 0.0,
            z_mm: 680.0,
            sigma,
            has_pitch: sigma.is_some(),
        }
    }

    /// The overlay needs BOTH eyes: from a reconstructed midpoint the arrow
    /// would move with the reconstruction rather than with the head.
    #[test]
    fn the_head_overlay_is_skipped_unless_both_eyes_and_a_pose_are_present() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 200, 120).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let v = a_head(20.0, 0.0, Some(0.07));
        let one_eye = EyeView {
            left: Some([0.4, 0.5]),
            right: None,
            ..EyeView::none()
        };
        draw_head_overlay(&cr, 200, 120, &one_eye, Some(&v));
        draw_head_overlay(&cr, 200, 120, &EyeView::none(), Some(&v));
        draw_head_overlay(&cr, 200, 120, &eyes_at([0.4, 0.5], [0.6, 0.5]), None);
        drop(cr);
        let data = surface.take_data().unwrap();
        assert!(
            data.iter().all(|&b| b == 0),
            "nothing should be drawn without two eyes and a pose"
        );
    }

    #[test]
    fn the_head_overlay_draws_when_both_eyes_and_a_pose_are_present() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 200, 120).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        draw_head_overlay(
            &cr,
            200,
            120,
            &eyes_at([0.4, 0.5], [0.6, 0.5]),
            Some(&a_head(25.0, -10.0, Some(0.07))),
        );
        drop(cr);
        let data = surface.take_data().unwrap();
        assert!(data.iter().any(|&b| b != 0), "the overlay drew nothing");
    }

    #[test]
    fn the_head_overlay_survives_a_degenerate_box_and_extreme_angles() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        for (w, h) in [(0, 0), (1, 1), (380, 240)] {
            draw_head_overlay(
                &cr,
                w,
                h,
                &eyes_at([0.0, 0.0], [1.0, 1.0]),
                Some(&a_head(180.0, -180.0, None)),
            );
        }
    }

    fn ev(guidance: Guidance, dist: Option<f32>, tracked: bool) -> EyeView {
        EyeView {
            left: tracked.then_some([0.45, 0.5]),
            right: tracked.then_some([0.55, 0.5]),
            left_alpha: 1.0,
            right_alpha: 1.0,
            distance_mm: dist,
            guidance,
            raw_guidance: guidance,
            ..EyeView::none()
        }
    }

    /// Every row is always present. A value that is not available reads as
    /// absent rather than being dropped: a row that vanishes leaves the user
    /// wondering whether the feature broke.
    /// Every row is always present, in two groups: where the head is, and
    /// which way it faces.
    #[test]
    fn the_readout_keeps_every_row_and_marks_missing_values_absent() {
        let [placement, facing] = readout_groups(&EyeView::none(), None);
        let names = |g: &[ReadoutRow]| g.iter().map(|r| r.label).collect::<Vec<_>>();
        assert_eq!(names(&placement), ["Position", "Distance"]);
        assert_eq!(names(&facing), ["Yaw", "Pitch", "Roll"]);
        assert_eq!(placement[0].value, "not detected");
        for r in placement[1..].iter().chain(facing.iter()) {
            assert_eq!(r.value, "—", "{} should read as absent", r.label);
        }
    }

    /// Side by side, the taller group sets the height. Five values stacked was
    /// what made the panel too long.
    #[test]
    fn the_readout_is_three_rows_tall_not_five() {
        let [placement, facing] = readout_groups(&EyeView::none(), None);
        assert_eq!(placement.len().max(facing.len()), 3);
        assert_eq!(placement.len() + facing.len(), 5, "no value may be dropped");
    }

    /// Pitch without a model is hardcoded 0.0, so printing "+0.0°" would be a
    /// lie. It has to say why it is missing.
    #[test]
    fn pitch_says_it_needs_a_model_rather_than_reporting_zero() {
        let head = HeadView {
            yaw_deg: 3.0,
            pitch_deg: 0.0,
            roll_deg: -1.0,
            z_mm: 680.0,
            sigma: None,
            has_pitch: false,
        };
        let [_, facing] = readout_groups(&ev(Guidance::Centered, Some(684.0), true), Some(&head));
        let get = |n: &str| facing.iter().find(|r| r.label == n).unwrap().value.clone();
        let pitch = get("Pitch");
        assert!(pitch.contains("no model"), "{pitch}");
        assert!(
            pitch.len() <= 12,
            "the pitch value shares a fixed-width column: {pitch:?}"
        );
        assert!(!pitch.contains("0.0"), "{pitch}");
        assert_eq!(get("Yaw"), "+3.0°");
        assert_eq!(get("Roll"), "-1.0°");
    }

    #[test]
    fn the_position_row_is_emphasised_only_while_it_is_asking_for_a_move() {
        let alert = |g: Guidance, d: Option<f32>, tracked: bool| {
            readout_groups(&ev(g, d, tracked), None)[0][0].alert
        };
        assert!(
            !alert(Guidance::Centered, Some(684.0), true),
            "a centred head must not shout"
        );
        assert!(
            alert(Guidance::MoveCloser, Some(900.0), true),
            "a nudge should be emphasised"
        );
        // "Not detected" is a state, not a nudge; it must not flash either.
        assert!(!alert(Guidance::NoEyes, None, false));
    }

    #[test]
    fn the_distance_is_reported_in_whole_millimetres() {
        let [placement, _] = readout_groups(&ev(Guidance::Centered, Some(683.7), true), None);
        assert_eq!(placement[1].value, "684 mm");
    }

    /// `guidance_message` appends the distance to its "centred" wording, which
    /// in a table would print the same millimetres twice, one row apart.
    #[test]
    fn the_position_row_does_not_repeat_the_distance() {
        let [placement, _] = readout_groups(&ev(Guidance::Centered, Some(684.0), true), None);
        assert_eq!(placement[0].value, "good");
        assert!(
            !placement[0].value.contains("684"),
            "{}",
            placement[0].value
        );
        assert_eq!(placement[1].value, "684 mm");
    }

    /// The graticule, reticle and drop lines all draw from the same rectangle;
    /// a zero-sized allocation happens during window construction.
    #[test]
    fn the_eye_view_survives_a_degenerate_allocation() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        for (w, h) in [(0, 0), (1, 1), (20, 20), (380, 240)] {
            draw_eye_view(&cr, w, h, &EyeView::none());
            draw_eye_view(&cr, w, h, &ev(Guidance::Centered, Some(684.0), true));
        }
    }

    /// The drawn box keeps its aspect at every allocation. It is the ratio of
    /// horizontal to vertical motion gain, so a box that stretched with the
    /// widget would make the dot sluggish on one axis and twitchy on the other.
    #[test]
    fn the_trackbox_keeps_its_aspect_at_any_size() {
        for (w, h) in [
            (380.0, 245.0),
            (900.0, 250.0),
            (300.0, 900.0),
            (640.0, 400.0),
        ] {
            let (x, y, rw, rh) = eye_view_rect(w, h);
            assert!(
                (rw / rh - EYE_VIEW_RATIO).abs() < 1e-9,
                "{w}x{h} gave {rw}x{rh}, ratio {}",
                rw / rh
            );
            assert!(rw <= w - 2.0 * EYE_VIEW_PAD + 1e-9, "wider than the widget");
            assert!(
                rh <= h - 2.0 * EYE_VIEW_PAD + 1e-9,
                "taller than the widget"
            );
            // Centred, so growing the window does not slide the instrument.
            assert!((x - (w - rw) / 2.0).abs() < 1e-9);
            assert!((y - (h - rh) / 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_widget_too_small_for_the_padding_draws_nothing() {
        for (w, h) in [(0.0, 0.0), (10.0, 10.0), (20.0, 5.0)] {
            let (_, _, rw, rh) = eye_view_rect(w, h);
            assert_eq!((rw, rh), (0.0, 0.0), "{w}x{h} should be empty");
        }
    }

    /// The overlay maps through the same rectangle, so the head can never be
    /// drawn somewhere the eye dots are not.
    #[test]
    fn the_head_overlay_follows_the_box_when_the_widget_grows() {
        let e = eyes_at([0.5, 0.5], [0.6, 0.5]);
        let v = a_head(0.0, 0.0, Some(0.07));
        for (w, h) in [(380, 245), (900, 500), (500, 900)] {
            let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            draw_eye_view(&cr, w, h, &e);
            draw_head_overlay(&cr, w, h, &e, Some(&v));
            drop(cr);
            let data = surface.take_data().unwrap();
            assert!(data.iter().any(|&b| b != 0), "{w}x{h} drew nothing");
        }
    }
}

/// A button whose label is a standalone [`gtk::Label`] with a little vertical
/// slack, instead of the button's own built-in label.
///
/// **This is a workaround for a real, repeatedly-reported rendering fault**: on
/// this theme the tops of tall glyphs are shaved off a widget's *built-in*
/// label, while a standalone `Label` in the same place renders whole. The
/// eye-selection radios in the hub hit exactly this and were fixed exactly this
/// way; the buttons were left on `Button::with_label` and kept clipping.
///
/// What has been ruled out first, so nobody repeats it: every CSS rule in this
/// app's stylesheet (GTK4 pushes no clip anywhere in the Button->Label path),
/// and the `GSK_RENDERER=gl` override this program used to force (removed; the
/// clipping survived it). Rendering the same widgets offscreen through GTK's own
/// renderer at scale 1.0, 1.15, 1.25, 1.5 and 2.0 does not reproduce it either,
/// which is why the fix is empirical rather than explanatory.
///
/// Use this instead of [`gtk::Button::with_label`] everywhere in this app.
pub fn button(text: &str) -> gtk::Button {
    let label = gtk::Label::new(Some(text));
    // The slack is the load-bearing part: the ink of a tall glyph needs a
    // little more vertical room than the label's logical box gives it here.
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    let b = gtk::Button::new();
    b.set_child(Some(&label));
    b
}

/// Replace the text of a button built by [`button`].
///
/// `gtk::Button::set_label` would throw the child label away and go back to the
/// built-in one, reintroducing the clipping on that button alone — which is a
/// nasty way to find this out, because it only shows up after the first state
/// change.
pub fn set_button_text(b: &impl IsA<gtk::Button>, text: &str) {
    let b = b.as_ref();
    match b.child().and_downcast::<gtk::Label>() {
        Some(l) => l.set_text(text),
        None => b.set_label(text),
    }
}

/// [`button`], but a [`gtk::ToggleButton`] — same built-in-label problem, same
/// workaround.
pub fn toggle_button(text: &str) -> gtk::ToggleButton {
    let label = gtk::Label::new(Some(text));
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    let b = gtk::ToggleButton::new();
    b.set_child(Some(&label));
    b
}

/// What the head-pose preview draws for one frame.
///
/// Pure data so the mapping from a pose to what you see is testable without a
/// drawing context — the same split as [`eye_view_for`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadView {
    /// Degrees, this crate's convention: +yaw = turned to the user's right,
    /// +pitch = looking up, +roll = tilted to the user's right.
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub roll_deg: f64,
    /// Distance from the tracker, millimetres.
    pub z_mm: f64,
    /// Model confidence, if this pose came from the model. `None` means the
    /// rotation is stale or geometric, and the view says so rather than
    /// implying a live measurement.
    pub sigma: Option<f32>,
    /// Whether pitch is real. Without a model it is hardcoded 0.0, and drawing
    /// that as a level head would be a lie.
    pub has_pitch: bool,
}

/// The head view for a device snapshot, or `None` when there is no pose at all.
pub fn head_view_for(state: &DeviceState) -> Option<HeadView> {
    if !matches!(state.status, ConnStatus::Connected) {
        return None;
    }
    let p = state.head_pose?;
    Some(HeadView {
        yaw_deg: p.yaw_deg,
        pitch_deg: p.pitch_deg,
        roll_deg: p.roll_deg,
        z_mm: p.z_mm,
        sigma: state.head_sigma,
        has_pitch: state.head_sigma.is_some() || p.pitch_deg != 0.0,
    })
}

/// One row of the live-view readout.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadoutRow {
    pub label: &'static str,
    pub value: String,
    /// Whether this row currently wants attention — the position, while it is
    /// asking the user to move.
    pub alert: bool,
}

/// The readout under the live view, as two side-by-side groups.
///
/// This replaces two separate lines — a guidance nudge and a head-pose sentence
/// — that sat under the same picture reporting different halves of the same
/// measurement in different formats. It is grouped rather than a single list
/// because the two halves answer different questions: **where the head is**, and
/// **which way it faces**. Side by side, that is three rows instead of five, and
/// the split is the meaning rather than a way to save space.
///
/// Every row is always present. A value that is unavailable reads as absent
/// rather than being dropped: a row that vanishes leaves the user wondering
/// whether the feature broke.
pub fn readout_groups(eyes: &EyeView, head: Option<&HeadView>) -> [Vec<ReadoutRow>; 2] {
    const ABSENT: &str = "—";
    let tracked = eyes.left.is_some() || eyes.right.is_some();
    let row = |label, value: String, alert| ReadoutRow {
        label,
        value,
        alert,
    };

    // NOT `guidance_message`: its "centred" arm appends the distance, which
    // here would print the same millimetres twice, one row apart. Distance has
    // its own row, so this one is the status alone.
    let position = match (tracked, eyes.guidance) {
        (false, _) | (_, Guidance::NoEyes) => "not detected".to_string(),
        (_, Guidance::Centered) => "good".to_string(),
        (_, Guidance::MoveCloser) => "move closer".to_string(),
        (_, Guidance::MoveBack) => "lean back".to_string(),
        (_, Guidance::MoveRight) => "move right".to_string(),
        (_, Guidance::MoveLeft) => "move left".to_string(),
        (_, Guidance::MoveDown) => "move down".to_string(),
        (_, Guidance::MoveUp) => "move up".to_string(),
    };
    let placement = vec![
        row(
            "Position",
            position,
            tracked && !matches!(eyes.raw_guidance, Guidance::Centered),
        ),
        row(
            "Distance",
            match eyes.distance_mm {
                Some(mm) => format!("{mm:.0} mm"),
                None => ABSENT.to_string(),
            },
            false,
        ),
    ];

    // Pitch is listed even with no model installed, reading as absent rather
    // than being silently dropped — and never as a zero, which is what
    // `pose_from_eyes` hardcodes and would be a lie.
    let (yaw, pitch, roll) = match head {
        Some(v) => (
            format!("{:+.1}°", v.yaw_deg),
            if v.has_pitch {
                format!("{:+.1}°", v.pitch_deg)
            } else {
                "no model".to_string()
            },
            format!("{:+.1}°", v.roll_deg),
        ),
        None => (ABSENT.to_string(), ABSENT.to_string(), ABSENT.to_string()),
    };
    let facing = vec![
        row("Yaw", yaw, false),
        row("Pitch", pitch, false),
        row("Roll", roll, false),
    ];

    [placement, facing]
}

/// Draw the head those eyes belong to, onto the eye-position box.
///
/// One view rather than two. The eye dots already carry position in the
/// trackbox and roll; what they cannot show is which way the face is *pointing*.
///
/// Two earlier attempts are recorded here because both were worse in a way that
/// is not obvious until you look at it:
///
/// * An arrow from between the eyes. It carried the right numbers and read as
///   an abstract gauge stuck over the dots — it did not look like anything.
/// * An ellipse with a full-height midline and a brow line. Two lines crossing
///   inside a circle read as a gunsight, not as a face.
///
/// So this draws an actual silhouette — cranium, temples, tapering jaw, chin —
/// with a nose. Foreshortening swings the outline as the head turns and the
/// nose says which way it points; nothing else is drawn inside, because the
/// dots in there are already the eyes.
///
/// Geometry is anchored to the interocular distance on screen, so the head
/// scales with the user as they move nearer or further away. Roll comes from
/// the *drawn dots* rather than the reported angle: the two agree, and taking
/// it from the dots means the face can never appear tilted differently from the
/// eyes inside it.
///
/// Drawn only when both eyes are present — from a reconstructed midpoint the
/// head would move with the reconstruction rather than with the user, and
/// report motion that did not happen.
pub fn draw_head_overlay(
    cr: &cairo::Context,
    w: i32,
    h: i32,
    eyes: &EyeView,
    head: Option<&HeadView>,
) {
    let (Some(v), Some(l), Some(r)) = (head, eyes.left, eyes.right) else {
        return;
    };
    let (w, h) = (w as f64, h as f64);
    let (bx, by, bw, bh) = eye_view_rect(w, h);
    if bw <= 0.0 || bh <= 0.0 {
        return;
    }
    let to_px = |p: [f32; 2]| -> (f64, f64) { (bx + p[0] as f64 * bw, by + p[1] as f64 * bh) };
    let (lx, ly) = to_px(l);
    let (rx, ry) = to_px(r);
    let (mx, my) = ((lx + rx) / 2.0, (ly + ry) / 2.0);
    let ipd = ((rx - lx).powi(2) + (ry - ly).powi(2)).sqrt();
    // `is_finite` first, then a plain comparison: a NaN here must mean "draw
    // nothing", never "draw something arbitrary".
    if !ipd.is_finite() || ipd <= 4.0 {
        return;
    }
    let roll = (ry - ly).atan2(rx - lx);
    let yaw = v.yaw_deg.to_radians().clamp(-1.3, 1.3);
    let pitch = if v.has_pitch {
        v.pitch_deg.to_radians().clamp(-1.3, 1.3)
    } else {
        0.0
    };

    match v.sigma {
        // Confident poses in the accent colour; a held or geometric one in grey,
        // so a frozen overlay never reads as a live one. Translucent either way:
        // the head is the context the eye dots sit in, not the subject.
        //
        // Setting this explicitly is load-bearing. Without it the head inherits
        // whatever `draw_eye_view` last set, so it silently took the eye-dot
        // colour — green when centred, amber when being nudged. It looked
        // coherent and was wrong: that colour is a *position* signal, and here it
        // has to mean model confidence.
        Some(_) => cr.set_source_rgba(0.16, 0.68, 0.70, 0.75),
        None => cr.set_source_rgba(0.45, 0.49, 0.53, 0.6),
    }

    let _ = cr.save();
    // Clip to the trackbox. Sitting close, a head genuinely does not fit in the
    // tracking volume's view, and letting it spill outside the box would say the
    // opposite of what the box means.
    cr.rectangle(bx, by, bw, bh);
    cr.clip();
    cr.translate(mx, my);
    cr.rotate(roll);

    // Turning swings the face across and narrows it; nodding does the same
    // vertically. The shift is what makes it read as a head rotating rather
    // than as a shape being squashed.
    let (sy, cyaw) = (yaw.sin(), yaw.cos());
    let (sp, cpitch) = (pitch.sin(), pitch.cos());
    let shift_x = sy * ipd * 0.30;
    let shift_y = -sp * ipd * 0.30;

    // Proportions in interocular distances. Deliberately a little smaller than
    // a real head (a face is nearer 2.2 IPD across): at true scale it fills the
    // trackbox and competes with the dots for attention, when its job is to be
    // the context they sit in.
    let hw = 0.95 * ipd * cyaw;
    let hh = 1.25 * ipd * cpitch;
    // Eyes sit ~45% down a head, so the outline's centre is just below them.
    let cy = shift_y + 0.10 * hh;
    let px = |x: f64| shift_x + x * hw;
    let py = |y: f64| cy + y * hh;

    // The outline: chin -> jaw -> cheek -> temple -> over the cranium and back
    // down the other side. Widest at the temples, tapering to a rounded chin.
    cr.set_line_width(1.4);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(px(0.0), py(1.0));
    cr.curve_to(
        px(-0.42),
        py(0.97),
        px(-0.80),
        py(0.72),
        px(-0.93),
        py(0.18),
    );
    cr.curve_to(
        px(-1.0),
        py(-0.16),
        px(-1.0),
        py(-0.68),
        px(-0.60),
        py(-0.92),
    );
    cr.curve_to(
        px(-0.36),
        py(-1.06),
        px(0.36),
        py(-1.06),
        px(0.60),
        py(-0.92),
    );
    cr.curve_to(px(1.0), py(-0.68), px(1.0), py(-0.16), px(0.93), py(0.18));
    cr.curve_to(px(0.80), py(0.72), px(0.42), py(0.97), px(0.0), py(1.0));
    cr.close_path();
    let _ = cr.stroke();

    // The nose. Seen straight on it is a short line down from the bridge; as
    // the head turns it swings across and as the head nods it shortens and
    // rises. It is the only mark inside the outline, and the one that actually
    // says which way the face points — the dots are already the eyes.
    let base_y = shift_y + 0.10 * ipd;
    let tip_x = shift_x + sy * ipd * 0.66;
    let tip_y = base_y + (0.62 - sp * 0.66) * ipd * cpitch.max(0.4);
    cr.set_line_width(2.2);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.move_to(shift_x, base_y);
    cr.line_to(tip_x, tip_y);
    let _ = cr.stroke();
    cr.set_line_cap(cairo::LineCap::Butt);
    let _ = cr.restore();
}

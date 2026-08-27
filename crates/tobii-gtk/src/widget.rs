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
    let pad = EYE_VIEW_PAD;
    let (rx, ry, rw, rh) = (pad, pad, (w - 2.0 * pad).max(0.0), (h - 2.0 * pad).max(0.0));

    // Trackbox outline.
    cr.set_source_rgb(0.42, 0.45, 0.5);
    cr.set_line_width(1.5);
    cr.rectangle(rx, ry, rw, rh);
    let _ = cr.stroke();

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
        cr.set_source_rgba(r, g, b, alpha as f64);
        cr.arc(ex, ey, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

/// Draw the latest NIR camera frame into a cairo context of size `w`×`h`,
/// contrast-stretched (the raw frames are very dark) and letterboxed to preserve
/// the square aspect. Mirrored horizontally so it reads like a mirror.
pub fn draw_camera_view(cr: &cairo::Context, w: i32, h: i32, frame: &tobii_protocol::CameraFrame) {
    cr.set_source_rgb(0.05, 0.05, 0.06);
    let _ = cr.paint();
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
    cr.save().ok();
    // Centre, then mirror horizontally (flip x about the image centre).
    cr.translate((w as f64 - dw) / 2.0, (h as f64 - dh) / 2.0);
    cr.translate(dw, 0.0);
    cr.scale(-scale, scale);
    let _ = cr.set_source_surface(&surface, 0.0, 0.0);
    let _ = cr.paint();
    cr.restore().ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(yaw: f64, pitch: f64, roll: f64, sigma: Option<f32>) -> HeadView {
        HeadView {
            yaw_deg: yaw,
            pitch_deg: pitch,
            roll_deg: roll,
            z_mm: 680.0,
            sigma,
            has_pitch: sigma.is_some(),
        }
    }

    /// Without a model, `pitch_deg` is hardcoded 0.0. Reporting that as a level
    /// head would be a lie, so the message has to say the number is absent
    /// rather than print a zero.
    #[test]
    fn a_pose_without_a_model_does_not_claim_a_pitch_of_zero() {
        let v = HeadView {
            has_pitch: false,
            sigma: None,
            ..head(12.0, 0.0, -3.0, None)
        };
        let m = head_message(Some(&v));
        assert!(m.contains("no pitch"), "{m}");
        assert!(!m.contains("pitch +0"), "{m}");
        assert!(m.contains("yaw +12"), "{m}");
    }

    #[test]
    fn a_full_pose_reports_all_three_angles_and_a_distance() {
        let m = head_message(Some(&head(-5.0, 8.0, 2.0, Some(0.07))));
        assert!(m.contains("yaw -5"), "{m}");
        assert!(m.contains("pitch +8"), "{m}");
        assert!(m.contains("roll +2"), "{m}");
        assert!(m.contains("68 cm"), "{m}");
    }

    #[test]
    fn no_pose_says_so_rather_than_showing_zeros() {
        assert_eq!(head_message(None), "No head detected");
    }

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

    #[test]
    fn drawing_a_head_view_never_panics_on_a_degenerate_size() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        for (w, h) in [(0, 0), (1, 1), (220, 220)] {
            draw_head_view(&cr, w, h, None);
            draw_head_view(&cr, w, h, Some(&head(90.0, -90.0, 180.0, Some(0.05))));
        }
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

/// One-line summary under the head preview.
pub fn head_message(view: Option<&HeadView>) -> String {
    match view {
        None => "No head detected".to_string(),
        Some(v) if !v.has_pitch => format!(
            "yaw {:+.0}°  roll {:+.0}°  ·  {:.0} cm  ·  no model, so no pitch",
            v.yaw_deg,
            v.roll_deg,
            v.z_mm / 10.0
        ),
        Some(v) => format!(
            "yaw {:+.0}°  pitch {:+.0}°  roll {:+.0}°  ·  {:.0} cm",
            v.yaw_deg,
            v.pitch_deg,
            v.roll_deg,
            v.z_mm / 10.0
        ),
    }
}

/// Draw the head preview: a head seen from the front, turned and tilted as the
/// tracker reports, over a fixed reference frame so the motion has something to
/// be relative to.
pub fn draw_head_view(cr: &cairo::Context, w: i32, h: i32, view: Option<&HeadView>) {
    let (w, h) = (w as f64, h as f64);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let r = (w.min(h) / 2.0) - 12.0;

    // Reference frame: a fixed ring and crosshair, so "centred" is visible even
    // when nothing is detected.
    cr.set_source_rgb(0.16, 0.18, 0.21);
    cr.set_line_width(1.0);
    cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
    cr.move_to(cx - r, cy);
    cr.line_to(cx + r, cy);
    cr.move_to(cx, cy - r);
    cr.line_to(cx, cy + r);
    let _ = cr.stroke();

    let Some(v) = view else {
        return;
    };

    // Yaw and pitch move the head within the ring; roll rotates it. The gain is
    // chosen so a comfortable +/-30 degrees reaches the ring, which is about the
    // range this tracker keeps both eyes for.
    const FULL_SCALE_DEG: f64 = 30.0;
    let dx = (v.yaw_deg / FULL_SCALE_DEG).clamp(-1.0, 1.0) * r * 0.55;
    let dy = if v.has_pitch {
        -(v.pitch_deg / FULL_SCALE_DEG).clamp(-1.0, 1.0) * r * 0.55
    } else {
        0.0
    };

    let _ = cr.save();
    cr.translate(cx + dx, cy + dy);
    cr.rotate(v.roll_deg.to_radians());

    // Confident poses are drawn in the app's accent; a held or geometric pose is
    // drawn grey, so a frozen preview never looks like a live one.
    match v.sigma {
        Some(_) => cr.set_source_rgb(0.12, 0.62, 0.63),
        None => cr.set_source_rgb(0.42, 0.46, 0.50),
    }
    cr.set_line_width(2.0);

    let hr = r * 0.42;
    // Head
    cr.arc(0.0, 0.0, hr, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
    // Nose, pointing the way the head is turned — the cue that reads fastest.
    cr.move_to(0.0, 0.0);
    cr.line_to(
        (v.yaw_deg / FULL_SCALE_DEG).clamp(-1.5, 1.5) * hr,
        if v.has_pitch {
            -(v.pitch_deg / FULL_SCALE_DEG).clamp(-1.5, 1.5) * hr
        } else {
            0.0
        },
    );
    let _ = cr.stroke();
    // Eye line: shows roll directly, and is what the geometric pose measures.
    cr.move_to(-hr * 0.55, -hr * 0.22);
    cr.line_to(hr * 0.55, -hr * 0.22);
    let _ = cr.stroke();
    let _ = cr.restore();
}

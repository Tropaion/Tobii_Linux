//! GTK/cairo rendering + pure presentation helpers over the `eyeview` data.
//! The pure helpers (`eye_view_for`, `guidance_message`) are
//! unit-tested; `draw_eye_view` is the cairo drawing (live-validated).

use gtk::cairo;

use crate::device::{ConnStatus, DeviceState};
use crate::eyeview::{EyeView, Guidance};

/// The `EyeView` to render for a device snapshot: never show stale gaze — force
/// "no eyes" unless the device is connected AND a sample is present.
pub fn eye_view_for(state: &DeviceState) -> EyeView {
    if !matches!(state.status, ConnStatus::Connected) {
        return EyeView::none();
    }
    state
        .latest_gaze
        .as_ref()
        .map(EyeView::from_gaze)
        .unwrap_or_else(EyeView::none)
}

/// Human-readable eye-position guidance line.
pub fn guidance_message(view: &EyeView) -> String {
    match view.guidance {
        Guidance::NoEyes => "No eyes detected — sit in front of the tracker.".to_string(),
        Guidance::MoveCloser => "Move a little closer.".to_string(),
        Guidance::MoveBack => "Move back a little.".to_string(),
        Guidance::OffCenter => "Center yourself in front of the screen.".to_string(),
        Guidance::Centered => match view.distance_mm {
            Some(d) => format!("Good position ({d:.0} mm)."),
            None => "Good position.".to_string(),
        },
    }
}

/// Draw the trackbox rectangle + both eyes into a cairo context of size `w`×`h`.
/// Eyes are green when centered, amber otherwise; positions are the mirror-view
/// normalized `[0,1]` coords from `EyeView`.
pub fn draw_eye_view(cr: &cairo::Context, w: i32, h: i32, view: &EyeView) {
    let (w, h) = (w as f64, h as f64);
    let pad = 10.0;
    let (rx, ry, rw, rh) = (pad, pad, (w - 2.0 * pad).max(0.0), (h - 2.0 * pad).max(0.0));

    // Trackbox outline.
    cr.set_source_rgb(0.42, 0.45, 0.5);
    cr.set_line_width(1.5);
    cr.rectangle(rx, ry, rw, rh);
    let _ = cr.stroke();

    let centered = matches!(view.guidance, Guidance::Centered);
    if centered {
        cr.set_source_rgb(0.18, 0.80, 0.55);
    } else {
        cr.set_source_rgb(0.95, 0.80, 0.25);
    }
    let radius = (rw.min(rh) * 0.06).clamp(6.0, 22.0);
    for eye in [view.left, view.right].into_iter().flatten() {
        let ex = rx + (eye[0].clamp(0.0, 1.0) as f64) * rw;
        let ey = ry + (eye[1].clamp(0.0, 1.0) as f64) * rh;
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
    use crate::eyeview::{EyeView, Guidance};

    fn view(g: Guidance, d: Option<f32>) -> EyeView {
        EyeView {
            left: None,
            right: None,
            distance_mm: d,
            guidance: g,
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
        assert!(guidance_message(&view(Guidance::NoEyes, None)).contains("No eyes"));
        assert!(guidance_message(&view(Guidance::MoveCloser, None)).contains("closer"));
        assert!(guidance_message(&view(Guidance::MoveBack, None)).contains("back"));
        assert!(guidance_message(&view(Guidance::OffCenter, None)).contains("Center"));
        assert_eq!(
            guidance_message(&view(Guidance::Centered, Some(680.0))),
            "Good position (680 mm)."
        );
    }

    #[test]
    fn eye_view_for_not_connected_is_no_eyes_even_with_cached_gaze() {
        // A stale sample must not render as live when disconnected.
        let mut s = DeviceState {
            status: ConnStatus::Error("unplugged".into()),
            latest_gaze: Some(valid_sample()),
            ..Default::default()
        };
        assert!(matches!(eye_view_for(&s).guidance, Guidance::NoEyes));
        // Connected + no sample is also "no eyes".
        s.status = ConnStatus::Connected;
        s.latest_gaze = None;
        assert!(matches!(eye_view_for(&s).guidance, Guidance::NoEyes));
    }
}

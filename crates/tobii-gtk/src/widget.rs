//! GTK/cairo rendering + pure presentation helpers over the `eyeview` data.
//! The pure helpers (`eye_view_for`, `status_text`, `guidance_message`) are
//! unit-tested; `draw_eye_view` is the cairo drawing (live-validated).

use gtk::cairo;

use crate::device::{ConnStatus, DeviceState};
use crate::eyeview::{EyeView, Guidance};

/// The `EyeView` to render for a device snapshot: never show stale gaze — force
/// "no eyes" unless the device is connected AND a sample is present.
pub fn eye_view_for(state: &DeviceState) -> EyeView {
    let no_eyes = EyeView {
        left: None,
        right: None,
        distance_mm: None,
        guidance: Guidance::NoEyes,
    };
    if matches!(state.status, ConnStatus::Connected) {
        state
            .latest_gaze
            .as_ref()
            .map(EyeView::from_gaze)
            .unwrap_or(no_eyes)
    } else {
        no_eyes
    }
}

/// Human-readable connection status line.
pub fn status_text(status: &ConnStatus) -> String {
    match status {
        ConnStatus::Connecting => "Connecting to the eye tracker…".to_string(),
        ConnStatus::Connected => "Eye tracker connected".to_string(),
        ConnStatus::Error(e) => format!("Not connected: {e}"),
    }
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
    fn status_text_covers_each_state() {
        assert!(status_text(&ConnStatus::Connecting).contains("Connecting"));
        assert!(status_text(&ConnStatus::Connected).contains("connected"));
        assert!(status_text(&ConnStatus::Error("x".into())).contains("Not connected: x"));
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

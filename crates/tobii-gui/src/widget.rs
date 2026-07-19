//! Shared eye-position visualization: a trackbox rectangle with both eyes at
//! their normalized positions, plus the human-readable guidance line. Used by
//! the hub mini-view and both guided flows so they stay visually identical.

use eframe::egui;

use crate::eyeview::{EyeView, Guidance};

/// Human-readable guidance for the current eye-position view.
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

/// The eye-position view to render for the current device state: `NoEyes`
/// unless the device is actually connected AND a gaze sample has arrived.
/// A disconnect (or a fresh connect with no sample yet) must never keep
/// rendering a stale cached sample as if it were live.
pub fn eye_view_for(state: &crate::device::DeviceState) -> EyeView {
    let no_eyes = EyeView {
        left: None,
        right: None,
        distance_mm: None,
        guidance: Guidance::NoEyes,
    };
    if !matches!(state.status, crate::device::ConnStatus::Connected) {
        return no_eyes;
    }
    state
        .latest_gaze
        .as_ref()
        .map(EyeView::from_gaze)
        .unwrap_or(no_eyes)
}

/// A colored status line for guided flows, shown only while the device isn't
/// connected, so a mid-flow disconnect is visible instead of masked by a
/// frozen eye view. Wording differs from `hub::draw`'s cold-start
/// "Connecting…" on purpose: entering a flow implies the device was already
/// connected, so this reads as a reconnect.
pub fn disconnect_status_line(ui: &mut egui::Ui, status: &crate::device::ConnStatus) {
    match status {
        crate::device::ConnStatus::Connecting => {
            ui.colored_label(egui::Color32::YELLOW, "Reconnecting to the eye tracker…");
        }
        crate::device::ConnStatus::Connected => {}
        crate::device::ConnStatus::Error(e) => {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("Not connected: {e}"));
        }
    }
}

/// Draw the trackbox rectangle with the two eyes at their normalized positions,
/// green when centered, yellow otherwise.
pub fn draw_eye_view(ui: &mut egui::Ui, view: &EyeView, size: egui::Vec2) {
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = resp.rect;
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.5, egui::Color32::GRAY));
    let plot = |p: [f32; 2]| {
        egui::pos2(
            rect.left() + p[0].clamp(0.0, 1.0) * rect.width(),
            rect.top() + p[1].clamp(0.0, 1.0) * rect.height(),
        )
    };
    let color = if matches!(view.guidance, Guidance::Centered) {
        egui::Color32::GREEN
    } else {
        egui::Color32::YELLOW
    };
    // Radius scales gently with the widget so the hub mini-view and the
    // fullscreen flow both look right.
    let r = (size.min_elem() * 0.035).clamp(6.0, 22.0);
    for eye in [view.left, view.right].into_iter().flatten() {
        painter.circle_filled(plot(eye), r, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ConnStatus, DeviceState};
    use tobii_protocol::gaze::present;
    use tobii_protocol::GazeSample;

    fn view(g: Guidance, d: Option<f32>) -> EyeView {
        EyeView {
            left: None,
            right: None,
            distance_mm: d,
            guidance: g,
        }
    }

    /// A gaze sample with valid trackbox + eye-origin columns, built the same
    /// way `eyeview.rs`'s tests do (trackbox cols present, validity 0,
    /// eye-origin z set) so it decodes to a non-`NoEyes` `EyeView`.
    fn valid_gaze_sample() -> GazeSample {
        GazeSample {
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
    fn eye_view_for_disconnected_ignores_cached_gaze() {
        let state = DeviceState {
            status: ConnStatus::Error("x".to_string()),
            latest_gaze: Some(valid_gaze_sample()),
        };
        assert_eq!(eye_view_for(&state).guidance, Guidance::NoEyes);
    }

    #[test]
    fn eye_view_for_connected_with_no_sample_is_no_eyes() {
        let state = DeviceState {
            status: ConnStatus::Connected,
            latest_gaze: None,
        };
        assert_eq!(eye_view_for(&state).guidance, Guidance::NoEyes);
    }

    #[test]
    fn eye_view_for_connected_with_sample_maps_gaze() {
        let state = DeviceState {
            status: ConnStatus::Connected,
            latest_gaze: Some(valid_gaze_sample()),
        };
        assert_ne!(eye_view_for(&state).guidance, Guidance::NoEyes);
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
        assert_eq!(
            guidance_message(&view(Guidance::Centered, None)),
            "Good position."
        );
    }
}

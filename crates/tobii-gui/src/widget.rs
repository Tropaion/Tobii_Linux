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
    use crate::eyeview::{EyeView, Guidance};

    fn view(g: Guidance, d: Option<f32>) -> EyeView {
        EyeView {
            left: None,
            right: None,
            distance_mm: d,
            guidance: g,
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
        assert_eq!(
            guidance_message(&view(Guidance::Centered, None)),
            "Good position."
        );
    }
}

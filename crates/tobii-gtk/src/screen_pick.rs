//! Pure logic for the screen-picker step of the setup flow.
//!
//! Decides whether to show the picker UI (only with 2+ monitors) and how to
//! label each monitor for a clickable button (model name preferred, falling
//! back through connector, EDID id, and a generic placeholder).

use tobii_config::MonitorInfo;

/// Should the setup flow show the monitor-picker UI?
///
/// Returns `true` only when there are multiple monitors to choose from.
/// A single monitor (or none at all) has only one sensible choice, so the
/// picker is skipped and the single monitor (if present) is used automatically.
pub fn should_show_picker(monitors: &[MonitorInfo]) -> bool {
    monitors.len() > 1
}

/// Human-readable label for a monitor button in the picker.
///
/// Prefers the monitor's model name (if present and non-empty after trimming).
/// Falls back to the DRM connector name, then the stable EDID id, then the
/// literal string `"Unknown display"` as a last resort — always something
/// clickable and never blank.
pub fn monitor_label(m: &MonitorInfo) -> String {
    // Prefer model name if present and non-empty after trim
    let trimmed_model = m.model.trim();
    if !trimmed_model.is_empty() {
        return trimmed_model.to_string();
    }

    // Fall back to connector name
    if let Some(conn) = &m.connector {
        if !conn.is_empty() {
            return conn.clone();
        }
    }

    // Fall back to EDID id
    if let Some(id) = &m.id {
        if !id.is_empty() {
            return id.clone();
        }
    }

    // Final fallback: generic placeholder
    "Unknown display".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: build a MonitorInfo with all fields filled.
    fn mon(model: &str, connector: Option<&str>, id: Option<&str>) -> MonitorInfo {
        MonitorInfo {
            model: model.to_string(),
            width_mm: 500.0,
            height_mm: 300.0,
            id: id.map(|s| s.to_string()),
            connector: connector.map(|s| s.to_string()),
        }
    }

    #[test]
    fn no_picker_when_zero_monitors() {
        assert!(!should_show_picker(&[]));
    }

    #[test]
    fn no_picker_when_one_monitor() {
        let monitors = vec![mon("Dell", Some("HDMI-1"), Some("DEL1234"))];
        assert!(!should_show_picker(&monitors));
    }

    #[test]
    fn show_picker_when_two_monitors() {
        let monitors = vec![
            mon("Dell", Some("HDMI-1"), Some("DEL1234")),
            mon("LG", Some("DP-1"), Some("LGE5678")),
        ];
        assert!(should_show_picker(&monitors));
    }

    #[test]
    fn show_picker_when_many_monitors() {
        let monitors = vec![
            mon("Dell", Some("HDMI-1"), Some("DEL1234")),
            mon("LG", Some("DP-1"), Some("LGE5678")),
            mon("ASUS", Some("HDMI-2"), Some("ASU9012")),
        ];
        assert!(should_show_picker(&monitors));
    }

    #[test]
    fn label_prefers_model() {
        let m = mon("Dell UltraSharp", Some("HDMI-1"), Some("DEL1234"));
        assert_eq!(monitor_label(&m), "Dell UltraSharp");
    }

    #[test]
    fn label_falls_back_to_connector_when_model_empty() {
        let m = mon("", Some("HDMI-1"), Some("DEL1234"));
        assert_eq!(monitor_label(&m), "HDMI-1");
    }

    #[test]
    fn label_falls_back_to_id_when_model_and_connector_empty() {
        let m = mon("", None, Some("DEL1234"));
        assert_eq!(monitor_label(&m), "DEL1234");
    }

    #[test]
    fn label_uses_fallback_when_all_empty() {
        let m = mon("", None, None);
        assert_eq!(monitor_label(&m), "Unknown display");
    }

    #[test]
    fn label_trims_model_whitespace() {
        let m = mon("  Dell UltraSharp  ", Some("HDMI-1"), Some("DEL1234"));
        assert_eq!(monitor_label(&m), "Dell UltraSharp");
    }

    #[test]
    fn label_skips_whitespace_only_model() {
        let m = mon("   ", Some("HDMI-1"), Some("DEL1234"));
        assert_eq!(monitor_label(&m), "HDMI-1");
    }
}

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

/// What the saved configuration says, before the monitor it names is looked up.
///
/// Three answers and not two, and the third is why this is not a `bool`. A
/// configuration file that is *there* and will not parse is not the same thing
/// as no configuration at all, and folding them said "No display set up yet."
/// — glossed in the help window as "the tracker has never been told where the
/// sensor sits" — over a file sitting on disk.
///
/// Named rather than spelled `Option<bool>`, which is what it was: `Some(false)`
/// meant "nothing saved" and `None` meant "unreadable", the least guessable
/// spelling available for either, and every call site had to carry a comment
/// saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// A configuration is saved and was read.
    Done,
    /// Nothing is saved: nobody has run the setup here.
    Never,
    /// Something is saved and this program could not read it.
    Unreadable,
}

/// What the "Change screen" card says it is set up for.
///
/// The card's description was *"Needed if the sensor moves to another
/// monitor."* — a conditional whose antecedent the hub never stated. Nothing
/// anywhere in it said which monitor the display setup had been done for, so
/// the one question that sentence provokes ("well, which one is it set up for
/// now?") was the one thing the card could not answer. The sentence itself is
/// not lost: it is in the help topic, with the consequence there was no room
/// for on a card.
///
/// Six answers, and *"Set up, but no monitor could be read here."* is the one
/// that keeps this from being two lines of `if` — named rather than counted to,
/// because the count has drifted twice already. A
/// saved id that matches nothing is *"not connected"* only when there were
/// monitors to compare it against; with none read at all — no `/sys/class/drm`,
/// a container, a permission — "that monitor is not connected" is a confident
/// negative about hardware this program could not look at, which is the one
/// shape of claim the rest of this tree is written against.
///
/// `saved` folds "no id was stored" and "the id could not be read" together,
/// and that is deliberate rather than the defect [`Setup::Unreadable`] exists
/// to prevent: both come out as *"for a monitor this machine cannot name"*,
/// which is true of each. The distinction that mattered was between a setup
/// having happened and not, because only one of those is a claim about what the
/// user has done.
///
/// Matched on the saved EDID id, which is what is stored, and never on the
/// model string: two identical monitors share a model and differ by serial.
pub fn setup_line(setup: Setup, saved: Option<&str>, monitors: &[MonitorInfo]) -> String {
    match setup {
        Setup::Never => return "No display set up yet.".to_string(),
        Setup::Unreadable => {
            return "Set up, and this program could not read its own configuration.".to_string()
        }
        Setup::Done => {}
    }
    let Some(id) = saved.map(str::trim).filter(|s| !s.is_empty()) else {
        // Set up, and the monitor it was set up for cannot be named. The setup
        // flow saves the EDID id of the monitor it used, and that id is `None`
        // whenever the panel's EDID has no usable serial or no monitor could be
        // read at all — in a VM, in a container, on a panel with a blank
        // descriptor. Keying "is a display set up" on the ID rather than on the
        // SETUP reported "No display set up yet." over a completed setup, for
        // every session after, and the help topic glosses that sentence as "the
        // tracker has never been told where the sensor sits".
        return "Set up, for a monitor this machine cannot name.".to_string();
    };
    if let Some(m) = monitors.iter().find(|m| m.id.as_deref() == Some(id)) {
        return format!("Set up for \u{201c}{}\u{201d}.", monitor_label(m));
    }
    if monitors.is_empty() {
        return "Set up, but no monitor could be read here.".to_string();
    }
    "Set up for a monitor that is not connected.".to_string()
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

    /// The card answers the question its old sentence provoked, in all five
    /// states — and the last is the one worth having a test for.
    ///
    /// "Set up for a monitor that is not connected" is a confident negative:
    /// it says this program looked at what is plugged in and did not find that
    /// screen. With no monitors read at all — no `/sys/class/drm`, a container,
    /// a permission — nothing was looked at, and saying it anyway is the shape
    /// of claim the rest of this tree is written against.
    #[test]
    fn the_card_says_which_monitor_it_is_set_up_for_or_why_it_cannot() {
        let here = mon("Odyssey G9", Some("card1-DP-1"), Some("SAM0001"));
        let other = mon("Dell", Some("card1-HDMI-1"), Some("DEL1234"));

        assert_eq!(
            setup_line(Setup::Never, None, std::slice::from_ref(&here)),
            "No display set up yet.",
            "nothing saved at all"
        );
        // A file that is there and will not read. Folded into the line above,
        // it told somebody with a config sitting on disk that the tracker had
        // never been told where the sensor sits.
        assert_eq!(
            setup_line(
                Setup::Unreadable,
                Some("SAM0001"),
                std::slice::from_ref(&here)
            ),
            "Set up, and this program could not read its own configuration."
        );
        // Set up, and the monitor cannot be named — a panel whose EDID has no
        // usable serial, a VM, a container. This is the case that read as
        // "never set up", and it is the reason the first argument exists: the
        // SETUP is what was done, and the id is only how the card names it. An
        // empty id is the same answer, since that is what a truncated write
        // leaves.
        for id in [None, Some("  ")] {
            assert_eq!(
                setup_line(Setup::Done, id, std::slice::from_ref(&here)),
                "Set up, for a monitor this machine cannot name.",
                "{id:?}"
            );
        }

        assert_eq!(
            setup_line(Setup::Done, Some("SAM0001"), &[other.clone(), here.clone()]),
            "Set up for \u{201c}Odyssey G9\u{201d}.",
            "the monitor by name, picked out of the ones that are plugged in"
        );
        assert_eq!(
            setup_line(Setup::Done, Some("SAM0001"), std::slice::from_ref(&other)),
            "Set up for a monitor that is not connected.",
            "monitors were read and this one is not among them"
        );
        assert_eq!(
            setup_line(Setup::Done, Some("SAM0001"), &[]),
            "Set up, but no monitor could be read here.",
            "nothing was read, so nothing may be concluded about what is plugged in"
        );
    }

    /// Matched on the EDID id and never on the model, which is what makes two
    /// identical screens two different answers.
    ///
    /// The id is what the setup flow saves, and it carries the serial; the
    /// model string is the same on both panels of a matched pair. A lookup by
    /// model would name whichever one came first out of `/sys/class/drm`, which
    /// is a directory order, and be right half the time.
    #[test]
    fn two_identical_monitors_are_told_apart() {
        let left = mon("Odyssey G9", Some("card1-DP-1"), Some("SAM0001"));
        let right = mon("Odyssey G9", Some("card1-DP-2"), Some("SAM0002"));
        // Both answers name the same words, so the test that means something is
        // that the id decides which row was found — asserted by removing it.
        assert_eq!(
            setup_line(Setup::Done, Some("SAM0002"), &[left.clone(), right.clone()]),
            "Set up for \u{201c}Odyssey G9\u{201d}."
        );
        assert_eq!(
            setup_line(Setup::Done, Some("SAM0002"), std::slice::from_ref(&left)),
            "Set up for a monitor that is not connected.",
            "the other panel of a matched pair is not this one"
        );
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

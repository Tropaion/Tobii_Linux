//! Lookup tables mapping TTP op codes to human-readable names.
//!
//! [`op_name`] is seeded from the `OP_*` constants in `tobii_protocol::frame`
//! plus the handshake ops. Extend that one table as new ops are
//! reverse-engineered — an op that returns `None` there is exactly a mapping
//! target and is surfaced as `UNKNOWN` in the catalog.
//!
//! [`third_party_label`] is a second, deliberately separate table of ops we
//! have only ever seen named in someone else's log. It feeds [`op_label`] so
//! the timeline says something more useful than `?unknown`, but not
//! [`op_name`], so the catalog keeps listing those ops as mapping targets.

use tobii_protocol::frame::{
    OP_CAL_ADD_POINT, OP_CAL_APPLY, OP_CAL_CLEAR, OP_CAL_COMPUTE, OP_CAL_RETRIEVE, OP_CAL_START,
    OP_CAL_STOP, OP_CLOSE_REALM, OP_GAZE_NOTIFY, OP_GET_DISPLAY_AREA, OP_GET_ENABLED_EYE, OP_HELLO,
    OP_OPEN_REALM, OP_QUERY_REALM, OP_REALM_RESPONSE, OP_SET_DISPLAY_AREA, OP_SET_ENABLED_EYE,
    OP_SUBSCRIBE,
};

/// Return the known name for an op code, or `None` if it is unmapped.
pub fn op_name(op: u32) -> Option<&'static str> {
    Some(match op {
        OP_HELLO => "hello",
        OP_SUBSCRIBE => "subscribe",
        OP_QUERY_REALM => "query_realm",
        OP_OPEN_REALM => "open_realm",
        OP_REALM_RESPONSE => "realm_response",
        OP_CLOSE_REALM => "close_realm",
        OP_GET_DISPLAY_AREA => "get_display_area",
        OP_SET_DISPLAY_AREA => "set_display_area",
        OP_CAL_START => "cal_start",
        OP_CAL_STOP => "cal_stop",
        OP_CAL_CLEAR => "cal_clear",
        OP_CAL_ADD_POINT => "cal_add_point",
        OP_CAL_COMPUTE => "cal_compute",
        OP_CAL_RETRIEVE => "cal_retrieve",
        OP_CAL_APPLY => "cal_apply",
        OP_GET_ENABLED_EYE => "get_enabled_eye",
        OP_SET_ENABLED_EYE => "set_enabled_eye",
        OP_GAZE_NOTIFY => "gaze_notify",
        _ => return None,
    })
}

/// Ops the stock Windows runtime asks for during startup discovery, as labelled
/// by a third party — never sent or answered here.
///
/// Source: `njmill/tobii-linux`, which logged the stock Windows Star Citizen
/// Tobii DLL requesting these object ids while it enumerated the device. The
/// ids are **[UNCONFIRMED]** (we have not reproduced that capture) and the
/// names are that project's reading of them, so each name is **[HYPOTHESIS]**.
/// Ids that project logged without naming stay unnamed here rather than
/// acquiring an invented one.
///
/// Every label carries the `?3p:` prefix — `?` as in [`op_label`]'s `?unknown`,
/// `3p` for third-party — so no output of this tool can be mistaken for a name
/// we stand behind.
///
/// That same log names `0xc62` "runtime_metadata". It is wrong: we have `0xc62`
/// live-verified as `get_enabled_eye` (see [`op_name`]), which is why it is
/// absent below.
pub fn third_party_label(op: u32) -> Option<&'static str> {
    Some(match op {
        0x532 => "?3p:unnamed",
        0x546 => "?3p:capabilities",
        0x58c => "?3p:device_info",
        0x5b4 => "?3p:unnamed",
        0x5d2 => "?3p:unnamed",
        0x672 => "?3p:unnamed",
        0x6a4 => "?3p:model_name",
        0x83e => "?3p:session_metadata",
        0xbf4 => "?3p:unnamed",
        _ => return None,
    })
}

/// A display label for an op: its known name, a third-party label, or the
/// `?unknown` marker used in the timeline.
pub fn op_label(op: u32) -> &'static str {
    op_name(op)
        .or_else(|| third_party_label(op))
        .unwrap_or("?unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ops_resolve() {
        assert_eq!(op_name(OP_HELLO), Some("hello"));
        assert_eq!(op_name(OP_GAZE_NOTIFY), Some("gaze_notify"));
        assert_eq!(op_name(OP_CAL_COMPUTE), Some("cal_compute"));
    }

    #[test]
    fn unknown_op_is_none() {
        assert_eq!(op_name(0xABCD), None);
        assert_eq!(op_label(0xABCD), "?unknown");
    }

    /// The ids from the third-party startup-discovery log.
    const THIRD_PARTY_OPS: [u32; 9] = [
        0x532, 0x546, 0x58c, 0x5b4, 0x5d2, 0x672, 0x6a4, 0x83e, 0xbf4,
    ];

    #[test]
    fn an_op_seen_only_in_a_third_party_log_gets_a_label_but_no_name() {
        for op in THIRD_PARTY_OPS {
            assert_eq!(op_name(op), None, "op 0x{op:x} must stay a mapping target");
            assert_ne!(op_label(op), "?unknown", "op 0x{op:x} should be labelled");
        }
    }

    #[test]
    fn every_third_party_label_is_marked_as_third_party() {
        for op in THIRD_PARTY_OPS {
            let label = op_label(op);
            assert!(
                label.starts_with("?3p:"),
                "op 0x{op:x} label {label:?} must not read as a name we verified"
            );
        }
    }

    #[test]
    fn an_id_the_third_party_logged_without_a_name_is_not_given_one() {
        for op in [0x532, 0x5b4, 0x5d2, 0x672, 0xbf4] {
            assert_eq!(op_label(op), "?3p:unnamed");
        }
    }

    #[test]
    fn our_live_verified_name_for_0xc62_wins_over_the_third_party_one() {
        // They call it "runtime_metadata"; we probed it. Ours stands.
        assert_eq!(op_name(0xc62), Some("get_enabled_eye"));
        assert_eq!(op_label(0xc62), "get_enabled_eye");
        assert_eq!(third_party_label(0xc62), None);
    }
}

//! Single lookup table mapping TTP op codes to human-readable names.
//!
//! Seeded from the `OP_*` constants in `tobii_protocol::frame` plus the
//! handshake ops. Extend this one table as new ops are reverse-engineered — an
//! op that returns `None` here is exactly a mapping target and is surfaced as
//! `UNKNOWN` in the catalog.

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

/// A display label for an op: its known name or the `?unknown` marker used in
/// the timeline.
pub fn op_label(op: u32) -> &'static str {
    op_name(op).unwrap_or("?unknown")
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
}

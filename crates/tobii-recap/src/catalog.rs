//! Aggregate decoded frames into a per-op catalog.

use std::collections::BTreeMap;

use crate::decode::TimelineFrame;
use crate::opnames::op_name;

/// Aggregate stats for one op code across the whole capture.
#[derive(Debug, Clone)]
pub struct OpStat {
    pub op: u32,
    /// True if the op was seen host -> device (a request).
    pub host_to_device: bool,
    /// True if the op was seen device -> host (a response/notify).
    pub device_to_host: bool,
    pub count: usize,
    pub min_len: usize,
    pub max_len: usize,
    /// Known name, or `None` for a reverse-engineering target.
    pub name: Option<&'static str>,
}

impl OpStat {
    /// A compact `>`/`<`/`<>` direction marker.
    pub fn dir_marker(&self) -> &'static str {
        match (self.host_to_device, self.device_to_host) {
            (true, true) => "<>",
            (true, false) => ">",
            (false, true) => "<",
            (false, false) => "?",
        }
    }
}

/// Build the op catalog from the decoded timeline frames.
///
/// Sorted with unknown (unmapped) ops first — those are the RE targets — then
/// by ascending op code within each group.
pub fn build(frames: &[TimelineFrame]) -> Vec<OpStat> {
    let mut by_op: BTreeMap<u32, OpStat> = BTreeMap::new();
    for tf in frames {
        let len = tf.frame.payload.len();
        let entry = by_op.entry(tf.frame.op).or_insert_with(|| OpStat {
            op: tf.frame.op,
            host_to_device: false,
            device_to_host: false,
            count: 0,
            min_len: usize::MAX,
            max_len: 0,
            name: op_name(tf.frame.op),
        });
        entry.count += 1;
        entry.min_len = entry.min_len.min(len);
        entry.max_len = entry.max_len.max(len);
        if tf.dir_in {
            entry.device_to_host = true;
        } else {
            entry.host_to_device = true;
        }
    }

    let mut stats: Vec<OpStat> = by_op.into_values().collect();
    // Unknown ops (name == None) sort first; then ascending op code.
    stats.sort_by_key(|s| (s.name.is_some(), s.op));
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::frame::{OP_GAZE_NOTIFY, OP_HELLO};
    use tobii_protocol::Frame;

    fn tf(op: u32, dir_in: bool, plen: usize) -> TimelineFrame {
        TimelineFrame {
            ts_rel: 0.0,
            dir_in,
            frame: Frame {
                magic: 0,
                seq: 0,
                op,
                payload: vec![0u8; plen],
            },
        }
    }

    #[test]
    fn aggregates_counts_and_lengths() {
        let frames = vec![
            tf(OP_HELLO, false, 10),
            tf(OP_HELLO, false, 4),
            tf(OP_HELLO, true, 20),
        ];
        let cat = build(&frames);
        assert_eq!(cat.len(), 1);
        let s = &cat[0];
        assert_eq!(s.count, 3);
        assert_eq!(s.min_len, 4);
        assert_eq!(s.max_len, 20);
        assert_eq!(s.dir_marker(), "<>");
        assert_eq!(s.name, Some("hello"));
    }

    #[test]
    fn unknown_ops_sort_to_the_top() {
        let frames = vec![
            tf(OP_HELLO, false, 0),
            tf(0xABCD, true, 8),
            tf(OP_GAZE_NOTIFY, true, 1692),
        ];
        let cat = build(&frames);
        // The unmapped 0xABCD op must be first.
        assert_eq!(cat[0].op, 0xABCD);
        assert!(cat[0].name.is_none());
        // Known ops follow, ordered by op code.
        assert!(cat[1..].iter().all(|s| s.name.is_some()));
    }
}

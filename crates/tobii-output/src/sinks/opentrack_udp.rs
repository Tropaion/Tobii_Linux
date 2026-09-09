//! Sink that speaks opentrack's "UDP over network" input protocol.
//!
//! The encoding itself lives in `tobii_headpose::opentrack` and is *not*
//! duplicated here: that module owns wire encodings of a pose, along with the
//! six exhaustive tests that pin the datagram's field order and endianness.
//! This file owns only the socket and the delivery policy.

use std::net::{SocketAddr, UdpSocket};

use tobii_headpose::opentrack;

use crate::{Sink, SinkError, TrackingFrame};

/// Sends each frame's head pose to an opentrack UDP input.
pub struct OpentrackUdp {
    socket: UdpSocket,
    addr: SocketAddr,
}

impl OpentrackUdp {
    /// Bind an ephemeral local port and target `addr`.
    ///
    /// The socket is send-only — opentrack never replies — so the local port
    /// is left to the OS and never inspected.
    pub fn new(addr: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        Ok(Self { socket, addr })
    }

    /// Where frames are being sent.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Sink for OpentrackUdp {
    fn name(&self) -> &'static str {
        "opentrack"
    }

    /// Frames with no pose are dropped rather than sent as a zero pose.
    ///
    /// In practice [`Router`](crate::Router) already withholds those, so this
    /// is belt-and-braces for anyone driving the sink directly — but it is the
    /// same policy either way, and it is the policy that matters: opentrack
    /// holding its last value beats the view snapping to centre on a blink.
    fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError> {
        let Some(pose) = frame.pose else {
            return Ok(());
        };
        self.socket
            .send_to(&opentrack::to_opentrack_datagram(&pose), self.addr)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Presence;
    use std::time::Duration;
    use tobii_headpose::HeadPose;

    /// Bind a receiver on an OS-assigned loopback port and return it with its
    /// address, so tests never collide on a fixed port.
    fn receiver() -> (UdpSocket, SocketAddr) {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        sock.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let addr = sock.local_addr().expect("local addr");
        (sock, addr)
    }

    fn pose() -> HeadPose {
        HeadPose {
            x_mm: -12.5,
            y_mm: 33.25,
            z_mm: 681.0,
            yaw_deg: -7.5,
            pitch_deg: 3.25,
            roll_deg: 4.25,
        }
    }

    #[test]
    fn a_pose_arrives_as_the_opentrack_datagram() {
        let (rx, addr) = receiver();
        let mut sink = OpentrackUdp::new(addr).expect("sink");
        sink.emit(&TrackingFrame::from_pose(1, pose()))
            .expect("emit");

        let mut buf = [0u8; 128];
        let n = rx.recv(&mut buf).expect("a datagram");
        assert_eq!(n, opentrack::DATAGRAM_LEN);
        assert_eq!(&buf[..n], &opentrack::to_opentrack_datagram(&pose()));
        assert_eq!(sink.addr(), addr);
        assert_eq!(sink.name(), "opentrack");
    }

    #[test]
    fn a_frame_without_a_pose_sends_nothing() {
        let (rx, addr) = receiver();
        rx.set_read_timeout(Some(Duration::from_millis(120)))
            .expect("timeout");
        let mut sink = OpentrackUdp::new(addr).expect("sink");
        sink.emit(&TrackingFrame {
            timestamp_us: 1,
            pose: None,
            gaze: Some([0.5, 0.5]),
            presence: Presence::OneEye,
        })
        .expect("emit");

        let mut buf = [0u8; 128];
        assert!(
            rx.recv(&mut buf).is_err(),
            "a pose-less frame must not put a zero pose on the wire"
        );
    }

    /// Nothing is listening on this port, and that must not be an error: a game
    /// or an opentrack instance that has not started yet is the normal case,
    /// not a failure worth taking the pipeline down for.
    #[test]
    fn sending_into_the_void_is_not_an_error() {
        let addr: SocketAddr = "127.0.0.1:1".parse().expect("addr");
        let mut sink = OpentrackUdp::new(addr).expect("sink");
        assert!(sink.emit(&TrackingFrame::from_pose(1, pose())).is_ok());
    }
}

//! Sink that feeds the Wine-side bridge over loopback UDP.
//!
//! # Why UDP across the Wine boundary
//!
//! Wine's winsock is a thin shim over host sockets, so a datagram sent from a
//! Linux process to `127.0.0.1:4243` is delivered to a Wine process bound
//! there with no special support and no Linux-side code beyond a plain
//! [`UdpSocket`]. That buys three things the alternatives do not:
//!
//! * **Prefix independence.** Wine, Proton, Lutris, a flatpak'd runner — even a
//!   Windows VM or another machine on the LAN — all work unchanged. Shared
//!   memory reached through the prefix's `Z:` drive would couple us to one
//!   prefix layout and one machine.
//! * **Benign failure.** If the bridge is not running, datagrams are dropped by
//!   the kernel. Nothing blocks, nothing errors, and the moment the bridge
//!   starts it begins receiving. A connection-oriented transport would need
//!   reconnect logic for the ordinary case of "the game is not open yet".
//! * **One format.** The payload is the same [`TrackingFrame`] encoding the
//!   daemon publishes to its own clients, so there is a single encoder and a
//!   single test suite rather than two that drift.
//!
//! Wine named pipes were rejected outright: they are wineserver-internal, and a
//! Linux process can only reach them through undocumented prefix plumbing.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use crate::{Sink, SinkError, TrackingFrame};

/// Default loopback port the bridge listens on.
///
/// Not 4242 — that is opentrack's — so both can run at once, which is exactly
/// what comparing our bridge against a known-good opentrack requires.
pub const DEFAULT_BRIDGE_PORT: u16 = 4243;

/// Sends each frame to the Wine-side bridge.
pub struct BridgeUdp {
    socket: UdpSocket,
    addr: SocketAddr,
}

impl BridgeUdp {
    /// Target `127.0.0.1:port`.
    ///
    /// Loopback only: the bridge runs inside a Wine prefix on this machine, and
    /// broadcasting head-tracking data onto a LAN by default would be a
    /// surprising thing for a driver to do. [`BridgeUdp::to_addr`] exists for
    /// anyone who genuinely wants a VM or another host.
    pub fn new(port: u16) -> std::io::Result<Self> {
        Self::to_addr(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)))
    }

    /// Target an arbitrary address — a Windows VM, or another machine.
    pub fn to_addr(addr: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        Ok(Self { socket, addr })
    }

    /// Where frames are being sent.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Sink for BridgeUdp {
    fn name(&self) -> &'static str {
        "bridge"
    }

    /// Unlike the opentrack sink, this one sends **every** frame, including
    /// pose-less ones.
    ///
    /// The bridge needs them: gaze and presence stay meaningful when the
    /// geometric pose is not available (one eye is enough for both), and the
    /// frame's flags already say the pose is absent, so the bridge can hold its
    /// last value without being starved of everything else.
    fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError> {
        self.socket.send_to(&frame.encode(), self.addr)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Presence, Router};
    use std::time::{Duration, Instant};
    use tobii_headpose::HeadPose;

    fn receiver() -> (UdpSocket, SocketAddr) {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        sock.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let addr = sock.local_addr().expect("local addr");
        (sock, addr)
    }

    fn sample() -> TrackingFrame {
        TrackingFrame {
            timestamp_us: 42,
            pose: Some(HeadPose {
                x_mm: -12.5,
                y_mm: 33.25,
                z_mm: 681.0,
                yaw_deg: -7.5,
                pitch_deg: 3.25,
                roll_deg: 4.25,
            }),
            gaze: Some([0.25, 0.75]),
            presence: Presence::BothEyes,
        }
    }

    #[test]
    fn a_frame_arrives_and_decodes_back_to_itself() {
        let (rx, addr) = receiver();
        let mut sink = BridgeUdp::to_addr(addr).expect("sink");
        sink.emit(&sample()).expect("emit");

        let mut buf = [0u8; 256];
        let n = rx.recv(&mut buf).expect("a datagram");
        assert_eq!(n, crate::frame::FRAME_LEN);
        assert_eq!(TrackingFrame::decode(&buf[..n]), Ok(sample()));
        assert_eq!(sink.name(), "bridge");
    }

    /// Gaze and presence survive a lost pose — one eye is enough for both — so
    /// the bridge must still hear about those frames, unlike opentrack which
    /// has nowhere to put them.
    #[test]
    fn a_pose_less_frame_is_still_sent_because_gaze_and_presence_remain() {
        let (rx, addr) = receiver();
        let mut sink = BridgeUdp::to_addr(addr).expect("sink");
        let f = TrackingFrame {
            timestamp_us: 7,
            pose: None,
            gaze: Some([0.5, 0.5]),
            presence: Presence::OneEye,
        };
        sink.emit(&f).expect("emit");

        let mut buf = [0u8; 256];
        let n = rx.recv(&mut buf).expect("a datagram");
        let got = TrackingFrame::decode(&buf[..n]).expect("decode");
        assert_eq!(got.pose, None);
        assert_eq!(got.gaze, Some([0.5, 0.5]));
        assert_eq!(got.presence, Presence::OneEye);
    }

    /// The bridge not running is the ordinary case — the game is not open yet —
    /// so it must cost nothing at all.
    #[test]
    fn sending_with_no_bridge_listening_is_not_an_error() {
        let mut sink = BridgeUdp::new(1).expect("sink");
        assert!(sink.emit(&sample()).is_ok());
    }

    #[test]
    fn the_default_port_is_not_opentracks() {
        assert_eq!(DEFAULT_BRIDGE_PORT, 4243);
        assert_ne!(
            DEFAULT_BRIDGE_PORT,
            tobii_headpose::opentrack::DEFAULT_PORT,
            "both must be able to run at once"
        );
        let sink = BridgeUdp::new(DEFAULT_BRIDGE_PORT).expect("sink");
        assert!(
            sink.addr().ip().is_loopback(),
            "must not default to the LAN"
        );
        assert_eq!(sink.addr().port(), DEFAULT_BRIDGE_PORT);
    }

    /// End to end through the router: the throttle and the tracking-loss policy
    /// are the router's job, and the sink must see exactly what survives them.
    #[test]
    fn the_router_drives_this_sink_like_any_other() {
        let (rx, addr) = receiver();
        rx.set_read_timeout(Some(Duration::from_millis(150)))
            .expect("timeout");
        let mut router = Router::new(100.0);
        router.add(Box::new(BridgeUdp::to_addr(addr).expect("sink")));

        let t0 = Instant::now();
        router.offer(&sample(), t0);
        router.offer(&sample(), t0 + Duration::from_millis(1)); // throttled

        let mut buf = [0u8; 256];
        assert!(rx.recv(&mut buf).is_ok(), "the first frame goes out");
        assert!(rx.recv(&mut buf).is_err(), "the throttled one does not");
    }
}

//! Is anything bound where we send game output?
//!
//! # Why this exists
//!
//! The README used to tell everybody to wrap their game — `tobii game --
//! %command%` — even for opentrack, which is *just a socket*. The wrapper
//! transports nothing: it connects to the hub and subscribes, and its only
//! effect is that the hub's `Demand` count goes above zero and the illuminators
//! come on. Somebody who starts opentrack, or X-Plane (which binds the same
//! port natively), has told the machine perfectly clearly that they want head
//! tracking — and then got a dark tracker, because nothing in the hub was
//! looking at the place the datagrams go.
//!
//! This module answers the one question that closes that gap: *is a local
//! socket bound where our opentrack datagrams would land?* The hub polls it and
//! turns the answer into an ordinary `DemandGuard`, so there is still exactly
//! one notion of who wants the device.
//!
//! # How the kernel answers it
//!
//! `/proc/net/udp` and `/proc/net/udp6` list every UDP socket on the machine,
//! one per line, with the second field the local address as `ADDR:PORT` in hex.
//! Measured on this machine (a socket bound to each of the three, then the file
//! read back):
//!
//! ```text
//! 127.0.0.1:4242  ->  0100007F:1092
//! 0.0.0.0:4444    ->  00000000:115C
//! [::1]:4343      ->  00000000000000000000000001000000:10F7   (in udp6)
//! ```
//!
//! PORT is the port in hex, big-endian, so it reads as the number. ADDR is not:
//! the kernel prints a `__be32` (the address in network byte order) with
//! `%08X`, so on a little-endian host the four bytes come back reversed —
//! `0100007F` is `7F 00 00 01` is `127.0.0.1`. IPv6 is the same trick four
//! times, one 32-bit word each. **This decode assumes a little-endian host**,
//! which every target this project ships for (x86_64, aarch64) is; on a
//! big-endian one the addresses would come out byte-swapped and simply never
//! match, so detection would be off rather than wrong.
//!
//! The uid column is read by nobody here. It is the socket's owner, and a
//! listener we care about is very often not us — opentrack started by the
//! desktop user, a hub running as somebody else. The file lists every socket on
//! the machine regardless of who asks, which is also why the tests below can
//! run as root in CI without changing meaning.
//!
//! # Which of those sockets count
//!
//! Two kinds are skipped, both because they cannot receive what we send:
//!
//! * **A socket that has `connect`ed.** Its `rem_address` column is non-zero,
//!   and the kernel delivers it only datagrams from that one peer — so it is
//!   not a listener *for us*, whatever port it holds. This is most of the
//!   outbound UDP on a desktop (DNS stubs, QUIC), and every one of them draws
//!   an ephemeral port that could otherwise collide with a configured one.
//! * **One of our own sinks.** `OpentrackUdp` and `BridgeUdp` bind `0.0.0.0:0`
//!   and send with `send_to`, so each is an unconnected wildcard socket on an
//!   ephemeral port — exactly the shape the wildcard arm below reports as a
//!   listener. The kernel draws that port from `ip_local_port_range`, and it is
//!   free to draw the configured opentrack port precisely when nothing is bound
//!   to it, which is the case this whole module is asked about. Counting our own
//!   sink would then latch: the answer takes a hold, the hold keeps the session,
//!   the session keeps the sink bound, and the sink is what the answer was
//!   reading — the tracker would never return to standby, which is the one rule
//!   this subsystem exists to enforce. A socket is ours when its `inode` column
//!   matches one of the `socket:[N]` links in `/proc/self/fd`; it is a *sender*
//!   when it is also bound to a wildcard, which is a fact about this project
//!   rather than about the kernel — those two sinks are the only wildcard binds
//!   in the tree, and every receiver we write names a loopback address.
//!
//! # What this deliberately cannot see
//!
//! * **A listener on another machine.** Nothing in `/proc` knows about other
//!   hosts' sockets, and there is no way to ask. If the opentrack address is
//!   not a loopback address, the answer is [`Listening::Unknown`] — never
//!   `No` — because "I looked and there is nobody" and "I cannot look" are
//!   different facts and only one of them is true here.
//! * **Anything on a non-Linux target.** This crate cross-compiles to
//!   `x86_64-pc-windows-gnu` for the Wine bridge, which has no `/proc`, so the
//!   read is behind `cfg` and the other arm answers `Unknown` — same shape as
//!   `sinks::uinput_joystick`, except that the module stays compiled
//!   everywhere because the parsing is pure and the hub's call site should not
//!   need a `cfg` of its own.
//! * **Whether the peer is actually reading.** A bound socket is a program that
//!   asked for this port; it is not proof that anybody is calling `recv`. That
//!   is the same standard the rest of the demand machinery uses — a subscribed
//!   IPC client is not proof either.

use std::net::SocketAddr;

/// What we found at an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listening {
    /// A local socket is bound where our datagrams would arrive.
    Yes,
    /// We could look, and nothing is bound there.
    No,
    /// The question cannot be answered here, for the reason given. Callers must
    /// treat this as "do not claim a listener", never as `No`.
    Unknown(&'static str),
}

/// The reason `Unknown` is returned for an address on some other machine.
pub const NOT_LOCAL: &str = "only sockets on this machine can be seen";

/// The reason `Unknown` is returned where there is no `/proc`.
pub const NOT_LINUX: &str = "listening sockets can only be enumerated on Linux";

/// The reason `Unknown` is returned when neither file could be read.
pub const NO_PROC: &str = "/proc/net/udp could not be read";

/// The `/proc/net/udp` half, which exists only on Linux.
///
/// One `cfg` around the whole path rather than six, and a re-export below, so
/// that neither the hub's call site nor this module's own callers need a `cfg`
/// of their own — `probe` is there on every target and answers `Unknown` where
/// it cannot look. Same shape as `sinks::uinput_joystick`, for the same reason:
/// this crate cross-compiles to `x86_64-pc-windows-gnu` for the Wine bridge.
#[cfg(target_os = "linux")]
mod proc_net {
    use super::*;
    use std::net::IpAddr;

    /// Whether an address we send to could possibly be served by a socket we can
    /// see in `/proc`.
    ///
    /// Loopback only, plus the unspecified address (`0.0.0.0`, which Linux routes
    /// to the local host). A LAN or public address may well be this machine's own,
    /// but deciding that needs the interface list, and getting it wrong the other
    /// way — treating a wildcard socket here as "opentrack on the other PC is
    /// running" — would light the tracker for a program on a different computer.
    fn is_visible_here(ip: IpAddr) -> bool {
        let ip = canonical(ip);
        ip.is_loopback() || ip.is_unspecified()
    }

    /// An IPv4-mapped IPv6 address as its IPv4 self, everything else unchanged.
    ///
    /// A socket bound to `::ffff:127.0.0.1` receives what a socket bound to
    /// `127.0.0.1` receives, so the two must compare equal.
    fn canonical(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => v6.to_canonical(),
            v4 => v4,
        }
    }

    /// Whether a datagram sent to `want` would reach a socket bound to `bound`.
    ///
    /// Two ways for that to be true: the socket is bound to exactly that address,
    /// or it is bound to a wildcard and takes everything arriving at the host.
    ///
    /// The wildcard arm slightly over-matches on one configuration: a socket on
    /// `[::]` receives IPv4 traffic only while `net.ipv6.bindv6only` is 0, which is
    /// the Linux default but can be turned on. Over-matching there costs a lit
    /// illuminator for a program that is not ours; under-matching would cost the
    /// whole feature for every dual-stack listener, which is most of them.
    ///
    /// This asks only where a socket is bound. Whether it is one that would take
    /// our datagrams at all — unconnected, and not one of ours — is decided by
    /// [`bound_sockets`], because that question is answered by other columns.
    fn would_receive(bound: SocketAddr, want: SocketAddr) -> bool {
        if bound.port() != want.port() {
            return false;
        }
        let (b, w) = (canonical(bound.ip()), canonical(want.ip()));
        b.is_unspecified() || b == w
    }

    /// Parse one `/proc/net/udp{,6}` local-address field, `ADDR:PORT` in hex.
    ///
    /// `None` for anything that is not one — which is how the header line is
    /// skipped, rather than by counting lines.
    fn parse_local(field: &str) -> Option<SocketAddr> {
        let (addr, port) = field.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let ip = match addr.len() {
            // One big-endian u32, printed through a little-endian host: reverse it.
            8 => {
                let v = u32::from_str_radix(addr, 16).ok()?;
                IpAddr::from(v.to_le_bytes())
            }
            // Four of them, in order.
            32 => {
                let mut bytes = [0u8; 16];
                for (i, chunk) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let word = u32::from_str_radix(addr.get(i * 8..i * 8 + 8)?, 16).ok()?;
                    *chunk = word.to_le_bytes();
                }
                IpAddr::from(bytes)
            }
            _ => return None,
        };
        Some(SocketAddr::new(ip, port))
    }

    /// One UDP socket, as far as this module reads it.
    struct Bound {
        /// Where it is bound — the only address in the line that matters.
        local: SocketAddr,
        /// Whether it has a peer, i.e. somebody called `connect` on it.
        connected: bool,
        /// The socket's inode, which is what `/proc/self/fd` reports it as.
        inode: u64,
    }

    /// Every UDP socket in one `/proc/net/udp` or `/proc/net/udp6` document.
    ///
    /// Lines whose local address does not decode are dropped, which is how the
    /// header is skipped rather than by counting lines. The other two fields only
    /// ever narrow the answer, so an unreadable one is read as the permissive
    /// value: a column we cannot parse must not cost us a real listener.
    fn bound_sockets(text: &str) -> impl Iterator<Item = Bound> + '_ {
        text.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let local = parse_local(fields.nth(1)?)?;
            let connected = fields
                .next()
                .and_then(parse_local)
                .is_some_and(|peer| peer.port() != 0);
            let inode = fields.nth(6).and_then(|f| f.parse().ok()).unwrap_or(0);
            Some(Bound {
                local,
                connected,
                inode,
            })
        })
    }

    /// Whether a socket is one of ours, sending.
    ///
    /// A wildcard bind is what makes it a sender rather than a receiver, and
    /// that is a fact about this project rather than about the kernel: the two
    /// sinks are the only `0.0.0.0` binds in the tree, and every receiver we
    /// write — the test sockets here, the hub's, the Wine bridge's — names a
    /// loopback address. So "ours and bound to a wildcard" is our own sink, and
    /// the narrow rule leaves an in-process socket on a real address counting
    /// as the listener it is standing in for.
    fn is_our_sender(s: &Bound, ours: &[u64]) -> bool {
        s.local.ip().is_unspecified() && ours.contains(&s.inode)
    }

    /// Answer the question against two documents already read.
    ///
    /// Pure, so the fixtures below can be real captured files: the interesting
    /// mistakes are all in the decoding and the matching, and neither needs a
    /// socket to get wrong. `ours` is the inode list from [`own_socket_inodes`],
    /// empty when the caller wants the machine's answer without that filter.
    fn scan(udp: &str, udp6: &str, want: SocketAddr, ours: &[u64]) -> Listening {
        let found = bound_sockets(udp)
            .chain(bound_sockets(udp6))
            .any(|s| !s.connected && !is_our_sender(&s, ours) && would_receive(s.local, want));
        if found {
            Listening::Yes
        } else {
            Listening::No
        }
    }

    /// The inode of every socket this process holds open.
    ///
    /// `/proc/self/fd/N` links to `socket:[INODE]` for a socket, and that number
    /// is the inode column of `/proc/net/udp`. Unreadable — a hardened `/proc`,
    /// or a target where the link is not spelled that way — answers "none of
    /// them are ours", which is the behaviour of not looking at all rather than a
    /// new failure mode.
    fn own_socket_inodes() -> Vec<u64> {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let target = std::fs::read_link(entry.path()).ok()?;
                let link = target.to_str()?;
                link.strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// Whether something is bound where `want` would arrive, `ours` aside.
    ///
    /// Split from [`probe`] so that a test can ask the real kernel the
    /// unfiltered question — the premise "this socket would otherwise have been
    /// counted" is half of what the exclusion's test has to say.
    fn look(want: SocketAddr, ours: &[u64]) -> Listening {
        if !is_visible_here(want.ip()) {
            return Listening::Unknown(NOT_LOCAL);
        }
        let udp = std::fs::read_to_string("/proc/net/udp");
        // A kernel built without IPv6 has no `udp6`, and that is not a failure —
        // an absent file is an empty list of IPv6 sockets. Only both missing means
        // we could not look at all.
        let udp6 = std::fs::read_to_string("/proc/net/udp6");
        if udp.is_err() && udp6.is_err() {
            return Listening::Unknown(NO_PROC);
        }
        scan(
            udp.as_deref().unwrap_or(""),
            udp6.as_deref().unwrap_or(""),
            want,
            ours,
        )
    }

    /// Whether somebody else on this machine is bound where `want` would arrive.
    pub fn probe(want: SocketAddr) -> Listening {
        look(want, &own_socket_inodes())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::Sink;
        use std::net::UdpSocket;

        /// A real capture. Bound in one process were `127.0.0.1:4242`,
        /// `0.0.0.0:4444` and `[::1]:4343`; the `:0035` and `:14E9` lines are
        /// systemd-resolved's, owned by **uid 975** rather than the reader, which
        /// is the point of keeping them: nothing here may filter by owner, or the
        /// feature would stop working the moment opentrack ran as somebody else —
        /// and CI, which runs the tests as root, would still pass.
        const UDP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
10521: 3600007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000   975        0 18491 2 0000000000000000 0
14710: 0100007F:1092 00000000:0000 07 00000000:000003C0 00:00000000 00000000  1000        0 45013 2 0000000000000000 0
14912: 00000000:115C 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 45015 2 0000000000000000 0
    ";

        const UDP6: &str = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
14811: 00000000000000000000000001000000:10F7 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 45014 2 0000000000000000 0
15821: 00000000000000000000000000000000:14E9 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000   975        0 18485 2 0000000000000000 0
    ";

        fn addr(s: &str) -> SocketAddr {
            s.parse().expect("an address")
        }

        /// The decode, against the three addresses that were actually bound when
        /// the capture above was taken.
        #[test]
        fn the_hex_columns_decode_to_the_addresses_that_were_bound() {
            let v4: Vec<SocketAddr> = bound_sockets(UDP).map(|s| s.local).collect();
            assert_eq!(
                v4,
                vec![
                    addr("127.0.0.54:53"),
                    addr("127.0.0.1:4242"),
                    addr("0.0.0.0:4444"),
                ],
                "the header line must be skipped and the rest byte-reversed"
            );

            let v6: Vec<SocketAddr> = bound_sockets(UDP6).map(|s| s.local).collect();
            assert_eq!(v6, vec![addr("[::1]:4343"), addr("[::]:5353")]);

            let inodes: Vec<u64> = bound_sockets(UDP).map(|s| s.inode).collect();
            assert_eq!(
                inodes,
                vec![18491, 45013, 45015],
                "the inode column is what says whose socket it is"
            );
        }

        #[test]
        fn a_socket_bound_to_the_exact_address_is_a_listener() {
            assert_eq!(scan(UDP, UDP6, addr("127.0.0.1:4242"), &[]), Listening::Yes);
        }

        /// What X-Plane does: bind the wildcard. A datagram to 127.0.0.1 arrives
        /// there, so it counts.
        #[test]
        fn a_wildcard_bind_counts_as_listening_on_loopback() {
            assert_eq!(scan(UDP, UDP6, addr("127.0.0.1:4444"), &[]), Listening::Yes);
            assert_eq!(scan(UDP, UDP6, addr("127.0.0.1:5353"), &[]), Listening::Yes);
        }

        /// The port is the whole question. Matching the address and ignoring the
        /// port would report every loopback socket on the machine as opentrack.
        #[test]
        fn a_different_port_is_not_a_listener() {
            assert_eq!(scan(UDP, UDP6, addr("127.0.0.1:4243"), &[]), Listening::No);
            assert_eq!(scan(UDP, UDP6, addr("127.0.0.1:1"), &[]), Listening::No);
        }

        /// v4 and v6 loopback are different addresses, and a datagram to one does
        /// not reach a socket bound to the other.
        #[test]
        fn the_two_loopbacks_are_not_interchangeable() {
            assert_eq!(scan(UDP, UDP6, addr("[::1]:4343"), &[]), Listening::Yes);
            assert_eq!(
                scan(UDP, UDP6, addr("127.0.0.1:4343"), &[]),
                Listening::No,
                "a v6-only listener does not receive v4 loopback traffic"
            );
            assert_eq!(scan(UDP, UDP6, addr("[::1]:4242"), &[]), Listening::No);
        }

        /// A dual-stack `[::]` socket does receive IPv4 loopback traffic, and a
        /// v4-mapped bind is the same address written differently.
        #[test]
        fn a_v6_wildcard_and_a_v4_mapped_bind_both_match() {
            assert!(would_receive(addr("[::]:4242"), addr("127.0.0.1:4242")));
            assert!(would_receive(
                addr("[::ffff:127.0.0.1]:4242"),
                addr("127.0.0.1:4242")
            ));
            assert!(!would_receive(
                addr("[::ffff:127.0.0.2]:4242"),
                addr("127.0.0.1:4242")
            ));
        }

        /// Garbage must be skipped, not panic or be mistaken for an address. This
        /// file is parsed once a second for as long as the hub runs.
        #[test]
        fn junk_lines_are_skipped_rather_than_guessed_at() {
            for junk in [
                "",
                "\n\n",
                "sl local_address\n",
                "1: xyz:1092 0:0\n",
                "1: 0100007F 0:0\n",
                "1: 0100007F:ZZZZ 0:0\n",
                "1: 0100007F0100:1092 0:0\n",
                "1:\n",
            ] {
                assert_eq!(bound_sockets(junk).count(), 0, "{junk:?} parsed");
            }
        }

        /// "Nobody is listening" and "I cannot see" are different answers, and the
        /// hub must never act on the second as if it were the first.
        #[test]
        fn a_remote_address_is_unknown_rather_than_absent() {
            assert!(!is_visible_here("192.168.1.7".parse().unwrap()));
            assert!(!is_visible_here("8.8.8.8".parse().unwrap()));
            assert!(is_visible_here("127.0.0.1".parse().unwrap()));
            assert!(is_visible_here("::1".parse().unwrap()));
            assert!(is_visible_here("0.0.0.0".parse().unwrap()));

            assert_eq!(
                probe(addr("192.168.1.7:4242")),
                Listening::Unknown(NOT_LOCAL),
                "a remote address is Unknown, and Unknown must never read as a listener"
            );
        }

        /// The whole mechanism through the real kernel: bind a socket, it is seen;
        /// close it, it is not.
        ///
        /// The port is whatever the OS hands out rather than 4242, so the test does
        /// not fight an opentrack the developer happens to have running — and
        /// nothing here reads the socket's owner, so it means the same run as root
        /// in CI as it does on a desktop. A real address rather than a wildcard,
        /// which is what a socket this process holds has to be to stand in for
        /// somebody else's listener.
        #[test]
        fn a_real_socket_is_seen_while_it_is_open_and_not_after() {
            let sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
            let addr = sock.local_addr().expect("local addr");

            assert_eq!(
                probe(addr),
                Listening::Yes,
                "a socket this process holds open must be visible at {addr}"
            );

            drop(sock);
            assert_eq!(
                probe(addr),
                Listening::No,
                "and gone once it is closed, with no timeout to wait out"
            );
        }

        /// Our own opentrack sink must never be mistaken for the thing it is
        /// sending to. It binds `0.0.0.0:0`, so the kernel hands it a port out of
        /// `ip_local_port_range` — and the port we are watching is eligible for
        /// that draw whenever nothing is bound to it, which is exactly when the
        /// question is being asked. So the guarantee cannot be "the draw misses";
        /// it is "the socket is ours". Forced here rather than waited for, because
        /// if it were wrong the hub would hold the tracker on for its own sink,
        /// forever, and the standby rule would be quietly dead.
        #[test]
        fn our_own_sender_is_not_mistaken_for_a_listener() {
            // A port that was free a moment ago: bound to learn the number, then
            // released, so nothing is listening there for the rest of the test.
            let probe_sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
            let target = probe_sock.local_addr().expect("local addr");
            drop(probe_sock);
            assert_eq!(probe(target), Listening::No, "premise: {target} is free");

            // The collision the kernel is free to hand us: a wildcard sender of
            // ours holding the very port the hub watches.
            let collided = UdpSocket::bind(("0.0.0.0", target.port())).expect("bind");
            assert_eq!(
                probe(target),
                Listening::No,
                "a sender of ours on {target} must not read as a listener on it"
            );
            assert_eq!(
                look(target, &[]),
                Listening::Yes,
                "premise: without the exclusion that same socket does read as one"
            );
            drop(collided);

            let mut sink = crate::sinks::OpentrackUdp::new(target).expect("sink");
            sink.emit(&crate::TrackingFrame::from_pose(
                1,
                tobii_headpose::HeadPose::default(),
            ))
            .expect("emit");

            assert_eq!(
                probe(target),
                Listening::No,
                "and nor must the real sink after it has sent to {target}"
            );
        }

        /// A socket with a peer receives only that peer's datagrams, so whatever
        /// port it holds it is not a listener for ours. Most outbound UDP on a
        /// desktop is this shape, and all of it sits on ephemeral ports that a
        /// configured opentrack port can collide with.
        #[test]
        fn a_connected_socket_is_not_a_listener_for_us() {
            let elsewhere = UdpSocket::bind("127.0.0.1:0").expect("bind");
            let peer = elsewhere.local_addr().expect("local addr");

            let sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
            let addr = sock.local_addr().expect("local addr");
            assert_eq!(
                probe(addr),
                Listening::Yes,
                "premise: unconnected, {addr} is a listener"
            );

            sock.connect(peer).expect("connect");
            assert_eq!(
                probe(addr),
                Listening::No,
                "connected to {peer}, it can no longer receive ours"
            );
        }
    }
}

#[cfg(target_os = "linux")]
pub use proc_net::probe;

/// Whether something on this machine is bound where `want` would arrive.
///
/// Always `Unknown` off Linux: the Wine bridge builds this crate for
/// `x86_64-pc-windows-gnu`, where there is no `/proc` to read.
#[cfg(not(target_os = "linux"))]
pub fn probe(_want: SocketAddr) -> Listening {
    Listening::Unknown(NOT_LINUX)
}

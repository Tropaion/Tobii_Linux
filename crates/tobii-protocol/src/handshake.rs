//! ET5 connection handshake state machine (pure, transport-agnostic).
//!
//! Drive it from a transport loop:
//!   let mut hs = Handshake::new(STREAM_GAZE);
//!   loop {
//!       match hs.poll() {
//!           HandshakeAction::Send(bytes) => { transport.send(&bytes); /* then read
//!               responses and call hs.on_response(payload) for each */ }
//!           HandshakeAction::Recv => { /* read more, call hs.on_response(...) */ }
//!           HandshakeAction::Done => break,
//!           HandshakeAction::Failed => return Err(...),
//!       }
//!   }
//!
//! Handshake responses use a looser TLV walk than requests: each field header
//! is [type:u8][pad:u8][size:u16 BE] followed by `size` body bytes.

use crate::commands::{build_hello, build_subscribe};
use crate::md5::hmac_md5;
use crate::realm::{build_open_realm, build_query_realm, build_realm_response, REALM_KEY};

fn resp_u16_be(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

/// Walk the response field stream after the 2-byte prefix, yielding each
/// field as `(size, body)`. Header is `[type:u8][pad:u8][size:u16 BE]`;
/// fields whose declared body runs past the buffer end are skipped.
fn resp_fields(data: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut pos = 2usize; // skip 2-byte prefix
    std::iter::from_fn(move || {
        while pos + 4 <= data.len() {
            let size = resp_u16_be(data, pos + 2) as usize;
            pos += 4;
            let body = data.get(pos..pos + size);
            pos += size;
            if let Some(b) = body {
                return Some((size, b));
            }
        }
        None
    })
}

/// First `size==4` field's u32 value, or 0 if none.
pub(crate) fn resp_first_u32(data: &[u8]) -> u32 {
    resp_fields(data)
        .find(|(size, _)| *size == 4)
        .map(|(_, b)| u32::from_be_bytes(b.try_into().unwrap()))
        .unwrap_or(0)
}

/// The `index`-th `size==4` field's u32 value, or 0 if out of range.
pub(crate) fn resp_u32_at(data: &[u8], index: usize) -> u32 {
    resp_fields(data)
        .filter(|(size, _)| *size == 4)
        .nth(index)
        .map(|(_, b)| u32::from_be_bytes(b.try_into().unwrap()))
        .unwrap_or(0)
}

/// First field whose `size > 4` — the realm challenge — or None.
pub(crate) fn resp_extract_challenge(data: &[u8]) -> Option<&[u8]> {
    resp_fields(data)
        .find(|(size, _)| *size > 4)
        .map(|(_, b)| b)
}

/// What the transport must do next, returned by [`Handshake::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeAction {
    /// Send these bytes, read any responses (calling [`Handshake::on_response`]),
    /// then call `poll` again.
    Send(Vec<u8>),
    /// No outbound frame; read more inbound data, feed responses, then `poll`.
    Recv,
    /// Handshake complete — the device is subscribed and streaming.
    Done,
    /// Handshake failed.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    BuildHello,
    AwaitHello,
    BuildQueryRealm,
    AwaitQueryRealm,
    BuildOpenRealm,
    AwaitOpenRealm,
    BuildRealmAuth,
    AwaitRealmAuth,
    BuildSubscribe,
    Done,
    Failed,
}

/// The ET5 connection handshake state machine. Transport-agnostic: it produces
/// frames to send and consumes response payloads, but performs no I/O.
#[derive(Debug, Clone)]
pub struct Handshake {
    state: State,
    seq: u32,
    stream_id: u16,
    realm_type: u32,
    realm_id: u32,
    field_210: u32,
    /// Last response payload received since the most recent request was sent.
    resp: Option<Vec<u8>>,
}

impl Handshake {
    /// Create a handshake that will subscribe to `stream_id` (typically 0x500).
    pub fn new(stream_id: u16) -> Self {
        Self {
            state: State::BuildHello,
            seq: 1,
            stream_id,
            realm_type: 0,
            realm_id: 0,
            field_210: 0,
            resp: None,
        }
    }

    fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.seq = 1;
        }
        s
    }

    /// Feed a response payload (the payload of a TTP response frame).
    pub fn on_response(&mut self, payload: &[u8]) {
        self.resp = Some(payload.to_vec());
    }

    /// Advance the handshake one step.
    pub fn poll(&mut self) -> HandshakeAction {
        loop {
            match self.state {
                State::BuildHello => {
                    let f = build_hello(self.next_seq());
                    self.resp = None;
                    self.state = State::AwaitHello;
                    return HandshakeAction::Send(f);
                }
                State::AwaitHello => {
                    if self.resp.is_some() {
                        self.state = State::BuildQueryRealm;
                        continue;
                    }
                    return HandshakeAction::Recv;
                }
                State::BuildQueryRealm => {
                    let f = build_query_realm(self.next_seq());
                    self.resp = None;
                    self.state = State::AwaitQueryRealm;
                    return HandshakeAction::Send(f);
                }
                State::AwaitQueryRealm => match self.resp.take() {
                    Some(r) => {
                        // resp_first_u32 returns 0 (== no realm) on a short buffer.
                        self.realm_type = resp_first_u32(&r);
                        self.state = State::BuildOpenRealm;
                        continue;
                    }
                    None => return HandshakeAction::Recv,
                },
                State::BuildOpenRealm => {
                    let f = build_open_realm(self.next_seq(), self.realm_type);
                    self.resp = None;
                    self.state = State::AwaitOpenRealm;
                    return HandshakeAction::Send(f);
                }
                State::AwaitOpenRealm => {
                    // Read fields through a borrow into locals, then mutate state.
                    // `self.resp` is intentionally left intact — BuildRealmAuth
                    // reads the challenge from this same open-realm response.
                    let (realm_id, field_210) = match self.resp.as_deref() {
                        None => return HandshakeAction::Recv,
                        Some(_) if self.realm_type == 0 => {
                            self.state = State::BuildSubscribe;
                            continue;
                        }
                        // Reference-derived minimum length for a valid open-realm
                        // reply (2-byte prefix + realm_id + field_210 fields).
                        Some(r) if r.len() < 12 => {
                            self.state = State::Failed;
                            return HandshakeAction::Failed;
                        }
                        Some(r) => (resp_u32_at(r, 0), resp_u32_at(r, 1)),
                    };
                    self.realm_id = realm_id;
                    self.field_210 = field_210;
                    self.state = State::BuildRealmAuth;
                    continue;
                }
                State::BuildRealmAuth => {
                    let r = self.resp.take().unwrap_or_default();
                    let challenge = match resp_extract_challenge(&r) {
                        Some(c) => c.to_vec(),
                        None => {
                            self.state = State::Failed;
                            return HandshakeAction::Failed;
                        }
                    };
                    let digest = hmac_md5(REALM_KEY, &challenge);
                    let f = build_realm_response(
                        self.next_seq(),
                        self.realm_id,
                        self.field_210,
                        &digest,
                    );
                    self.state = State::AwaitRealmAuth;
                    return HandshakeAction::Send(f);
                }
                State::AwaitRealmAuth => {
                    if self.resp.is_some() {
                        self.state = State::BuildSubscribe;
                        continue;
                    }
                    return HandshakeAction::Recv;
                }
                State::BuildSubscribe => {
                    let f = build_subscribe(self.next_seq(), self.stream_id);
                    self.state = State::Done;
                    return HandshakeAction::Send(f);
                }
                State::Done => return HandshakeAction::Done,
                State::Failed => return HandshakeAction::Failed,
            }
        }
    }
}

#[cfg(test)]
mod resp_tests {
    use super::*;

    // A response field: [type=0x02][pad=0x00][size:u16 BE][body].
    fn u32_field(v: u32) -> Vec<u8> {
        let mut f = vec![0x02, 0x00, 0x00, 0x04];
        f.extend_from_slice(&v.to_be_bytes());
        f
    }
    fn blob_field(body: &[u8]) -> Vec<u8> {
        let mut f = vec![0x02, 0x00];
        f.extend_from_slice(&(body.len() as u16).to_be_bytes());
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn first_u32_reads_first_size4_field() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x2a));
        p.extend(u32_field(0x99));
        assert_eq!(resp_first_u32(&p), 0x2a);
    }

    #[test]
    fn first_u32_is_zero_when_none() {
        let p = vec![0x00, 0x00];
        assert_eq!(resp_first_u32(&p), 0);
    }

    #[test]
    fn u32_at_indexes_size4_fields() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x11));
        p.extend(u32_field(0x22));
        assert_eq!(resp_u32_at(&p, 0), 0x11);
        assert_eq!(resp_u32_at(&p, 1), 0x22);
        assert_eq!(resp_u32_at(&p, 2), 0);
    }

    #[test]
    fn extract_challenge_returns_first_long_field() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5));
        p.extend(blob_field(&[0xaa; 16]));
        assert_eq!(resp_extract_challenge(&p), Some(&[0xaa; 16][..]));
    }

    #[test]
    fn extract_challenge_none_when_all_short() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5));
        assert_eq!(resp_extract_challenge(&p), None);
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use crate::frame::{OP_HELLO, OP_OPEN_REALM, OP_QUERY_REALM, OP_REALM_RESPONSE, OP_SUBSCRIBE};

    #[test]
    fn poll_asks_to_recv_before_any_response() {
        let mut hs = Handshake::new(0x500);
        // First poll builds + sends hello.
        assert!(matches!(hs.poll(), HandshakeAction::Send(_)));
        // With no response fed yet, the next poll must ask the transport to read.
        assert!(matches!(hs.poll(), HandshakeAction::Recv));
        // Still Recv on a repeat call (idempotent until a response arrives).
        assert!(matches!(hs.poll(), HandshakeAction::Recv));
    }

    fn op_of(frame: &[u8]) -> u32 {
        u32::from_be_bytes([frame[20], frame[21], frame[22], frame[23]])
    }

    fn u32_field(v: u32) -> Vec<u8> {
        let mut f = vec![0x02, 0x00, 0x00, 0x04];
        f.extend_from_slice(&v.to_be_bytes());
        f
    }
    fn prefixed(fields: &[Vec<u8>]) -> Vec<u8> {
        let mut p = vec![0x00, 0x00];
        for f in fields {
            p.extend_from_slice(f);
        }
        p
    }

    /// Drive the handshake to a terminal action, feeding `responses` in order.
    fn run(hs: &mut Handshake, responses: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, HandshakeAction) {
        let mut sent = Vec::new();
        let mut replies = responses.into_iter();
        for _ in 0..50 {
            match hs.poll() {
                HandshakeAction::Send(bytes) => {
                    sent.push(bytes);
                    if let Some(r) = replies.next() {
                        hs.on_response(&r);
                    }
                }
                HandshakeAction::Recv => {
                    if let Some(r) = replies.next() {
                        hs.on_response(&r);
                    } else {
                        return (sent, HandshakeAction::Recv);
                    }
                }
                term => return (sent, term),
            }
        }
        panic!("handshake did not terminate");
    }

    #[test]
    fn no_auth_path_reaches_done() {
        let mut hs = Handshake::new(0x500);
        let responses = vec![prefixed(&[]), prefixed(&[u32_field(0)]), prefixed(&[])];
        let (sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Done));
        let ops: Vec<u32> = sent.iter().map(|f| op_of(f)).collect();
        assert_eq!(
            ops,
            vec![OP_HELLO, OP_QUERY_REALM, OP_OPEN_REALM, OP_SUBSCRIBE]
        );
    }

    #[test]
    fn auth_path_sends_correct_digest_and_reaches_done() {
        let mut hs = Handshake::new(0x500);
        let challenge = [0xABu8; 16];
        let mut open_reply = vec![0x00, 0x00];
        open_reply.extend(u32_field(5));
        open_reply.extend(u32_field(7));
        open_reply.extend({
            let mut f = vec![0x02, 0x00];
            f.extend_from_slice(&(challenge.len() as u16).to_be_bytes());
            f.extend_from_slice(&challenge);
            f
        });
        let responses = vec![
            prefixed(&[]),
            prefixed(&[u32_field(1)]),
            open_reply,
            prefixed(&[]),
        ];
        let (sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Done));
        let ops: Vec<u32> = sent.iter().map(|f| op_of(f)).collect();
        assert_eq!(
            ops,
            vec![
                OP_HELLO,
                OP_QUERY_REALM,
                OP_OPEN_REALM,
                OP_REALM_RESPONSE,
                OP_SUBSCRIBE
            ]
        );
        let realm_resp = &sent[3];
        let expected = hmac_md5(REALM_KEY, &challenge);
        assert_eq!(&realm_resp[52..68], &expected[..]);
    }

    #[test]
    fn missing_challenge_fails() {
        let mut hs = Handshake::new(0x500);
        let mut open_reply = vec![0x00, 0x00];
        open_reply.extend(u32_field(5));
        open_reply.extend(u32_field(7));
        let responses = vec![prefixed(&[]), prefixed(&[u32_field(1)]), open_reply];
        let (_sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Failed));
    }
}

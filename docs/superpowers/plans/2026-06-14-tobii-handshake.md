# tobii-protocol Handshake Engine — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure, transport-agnostic ET5 connection **handshake state machine** to the `tobii-protocol` crate (hello → query/open realm → HMAC-MD5 auth → subscribe), driven by `poll()`/`on_response()`, fully unit-tested with mocked device responses and no hardware.

**Architecture:** A `Handshake` struct owns the sequence counter and handshake state. `poll()` returns an `Action` (`Send(bytes)` / `Recv` / `Done` / `Failed`); the future transport loop sends the bytes, reads frames, and calls `on_response(payload)` for each response. Realm fields (type/id/challenge) are parsed from response payloads with small heuristic helpers ported from the reference. This is the last pure piece of the protocol engine — the live `rusb` transport that drives it is a separate later crate (`tobii-usb`).

**Tech Stack:** Rust (edition 2021), `cargo test`. Builds on the existing `tobii-protocol` modules (`commands`, `realm`, `md5`). Reference: `ressources/tobiifree/driver/src/tobiifree_core.zig` (the `handshake_*` state machine + `hsTlv*` helpers, lines ~1494–2118). GPL-3.0.

**Scope note:** Plan 2 of the v1 roadmap. Pure logic only — no USB, no I/O, no new dependencies. The transport + `tobii stream` CLI are Plan 3 (needs the ET5 plugged in). The calibration realm sub-machines from the reference are Phase 2 and excluded.

---

### Task 1: Response-field parsing helpers

The device's handshake **responses** use a looser TLV walk than the request encoders: each field header is `[type:u8][pad:u8][size:u16 BE]` followed by `size` body bytes (distinct from the request-side `[type][size:u32]`). These helpers extract realm fields from a response payload. Ported verbatim from the reference `hsTlvFirstU32` / `hsTlvU32At` / `hsTlvExtractChallenge`.

**Files:**
- Create: `crates/tobii-protocol/src/handshake.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod handshake;`)
- Test: inline in `handshake.rs`

- [ ] **Step 1: Add the module declaration**

In `crates/tobii-protocol/src/lib.rs`, add (keep modules alphabetical — this goes between `frame` and `md5`):

```rust
pub mod handshake;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/tobii-protocol/src/handshake.rs` with:

```rust
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
        let mut p = vec![0x00, 0x00]; // 2-byte prefix
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
        p.extend(u32_field(0x11)); // index 0
        p.extend(u32_field(0x22)); // index 1
        assert_eq!(resp_u32_at(&p, 0), 0x11);
        assert_eq!(resp_u32_at(&p, 1), 0x22);
        assert_eq!(resp_u32_at(&p, 2), 0);
    }

    #[test]
    fn extract_challenge_returns_first_long_field() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5)); // size 4 — skipped
        p.extend(blob_field(&[0xaa; 16])); // size 16 — the challenge
        assert_eq!(resp_extract_challenge(&p), Some(&[0xaa; 16][..]));
    }

    #[test]
    fn extract_challenge_none_when_all_short() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5));
        assert_eq!(resp_extract_challenge(&p), None);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::resp_tests`
Expected: FAIL — `cannot find function resp_first_u32`.

- [ ] **Step 4: Write the implementation**

PREPEND to `handshake.rs` (above the test module):

```rust
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

/// First `size==4` field's u32 value, or 0 if none.
pub(crate) fn resp_first_u32(data: &[u8]) -> u32 {
    let mut pos = 2usize; // skip 2-byte prefix
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size == 4 && pos + 4 <= data.len() {
            return u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        }
        pos += size;
    }
    0
}

/// The `index`-th `size==4` field's u32 value, or 0 if out of range.
pub(crate) fn resp_u32_at(data: &[u8], index: usize) -> u32 {
    let mut pos = 2usize;
    let mut found = 0usize;
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size == 4 && pos + 4 <= data.len() {
            if found == index {
                return u32::from_be_bytes([
                    data[pos],
                    data[pos + 1],
                    data[pos + 2],
                    data[pos + 3],
                ]);
            }
            found += 1;
        }
        pos += size;
    }
    0
}

/// First field whose `size > 4` — the realm challenge — or None.
pub(crate) fn resp_extract_challenge(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size > 4 && pos + size <= data.len() {
            return Some(&data[pos..pos + size]);
        }
        pos += size;
    }
    None
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::resp_tests`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/handshake.rs
git commit -m "feat(protocol): add handshake response-field parsers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Handshake state machine

**Files:**
- Modify: `crates/tobii-protocol/src/handshake.rs` (add `HandshakeAction`, `Handshake`)
- Test: inline in `handshake.rs`

- [ ] **Step 1: Write the failing tests**

Add this test module to `handshake.rs` AFTER `resp_tests`:

```rust
#[cfg(test)]
mod handshake_tests {
    use super::*;
    use crate::frame::{OP_HELLO, OP_OPEN_REALM, OP_QUERY_REALM, OP_REALM_RESPONSE, OP_SUBSCRIBE};

    fn op_of(frame: &[u8]) -> u32 {
        u32::from_be_bytes([frame[20], frame[21], frame[22], frame[23]])
    }

    // Build a response payload with the [type][pad][size:u16][body] field format.
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

    /// Drive the handshake to a terminal action, feeding `responses` in order
    /// (one reply per request that expects one). Returns (sent_frames, terminal).
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
        // realm_type == 0 → skip auth, go straight to subscribe.
        let mut hs = Handshake::new(0x500);
        let responses = vec![
            prefixed(&[]),                  // hello reply (content ignored)
            prefixed(&[u32_field(0)]),      // query_realm → realm_type = 0
            prefixed(&[]),                  // open_realm reply
        ];
        let (sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Done));
        let ops: Vec<u32> = sent.iter().map(|f| op_of(f)).collect();
        assert_eq!(ops, vec![OP_HELLO, OP_QUERY_REALM, OP_OPEN_REALM, OP_SUBSCRIBE]);
    }

    #[test]
    fn auth_path_sends_correct_digest_and_reaches_done() {
        let mut hs = Handshake::new(0x500);
        let challenge = [0xABu8; 16];
        // open_realm reply: realm_id=5 (idx0), field_210=7 (idx1), challenge (size>4).
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
            prefixed(&[]),             // hello reply
            prefixed(&[u32_field(1)]), // query_realm → realm_type = 1 (auth required)
            open_reply,                // open_realm → id/field/challenge
            prefixed(&[]),             // realm_response ack
        ];
        let (sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Done));

        let ops: Vec<u32> = sent.iter().map(|f| op_of(f)).collect();
        assert_eq!(
            ops,
            vec![OP_HELLO, OP_QUERY_REALM, OP_OPEN_REALM, OP_REALM_RESPONSE, OP_SUBSCRIBE]
        );

        // The realm_response frame must carry HMAC-MD5(REALM_KEY, challenge) at
        // payload offset 2+9+9=20 → frame offset 8+24+20 = 52..68.
        let realm_resp = &sent[3];
        let expected = hmac_md5(REALM_KEY, &challenge);
        assert_eq!(&realm_resp[52..68], &expected[..]);
    }

    #[test]
    fn missing_challenge_fails() {
        let mut hs = Handshake::new(0x500);
        // realm_type=1 but open_realm reply has no challenge (all size==4) and is
        // long enough to pass the length gate (>= 12 bytes after prefix).
        let mut open_reply = vec![0x00, 0x00];
        open_reply.extend(u32_field(5));
        open_reply.extend(u32_field(7));
        let responses = vec![prefixed(&[]), prefixed(&[u32_field(1)]), open_reply];
        let (_sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Failed));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::handshake_tests`
Expected: FAIL — `cannot find type Handshake`.

- [ ] **Step 3: Write the implementation**

In `handshake.rs`, insert this AFTER the `resp_extract_challenge` function and BEFORE the test modules:

```rust
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
                State::AwaitQueryRealm => {
                    match self.resp.take() {
                        Some(r) => {
                            self.realm_type = if r.len() >= 6 { resp_first_u32(&r) } else { 0 };
                            self.state = State::BuildOpenRealm;
                            continue;
                        }
                        None => return HandshakeAction::Recv,
                    }
                }
                State::BuildOpenRealm => {
                    let f = build_open_realm(self.next_seq(), self.realm_type);
                    self.resp = None;
                    self.state = State::AwaitOpenRealm;
                    return HandshakeAction::Send(f);
                }
                State::AwaitOpenRealm => {
                    // Keep the response: the challenge is extracted in BuildRealmAuth.
                    let r = match &self.resp {
                        Some(r) => r.clone(),
                        None => return HandshakeAction::Recv,
                    };
                    if self.realm_type == 0 {
                        self.state = State::BuildSubscribe;
                        continue;
                    }
                    if r.len() < 12 {
                        self.state = State::Failed;
                        return HandshakeAction::Failed;
                    }
                    self.realm_id = resp_u32_at(&r, 0);
                    self.field_210 = resp_u32_at(&r, 1);
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
                    let f = build_realm_response(self.next_seq(), self.realm_id, self.field_210, &digest);
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
```

> **Subtlety to preserve (matches the reference):** `AwaitOpenRealm` must NOT clear `self.resp` — the open-realm response carries the challenge that `BuildRealmAuth` reads. Only the `Build*` states clear `resp` (right before sending the next request) and `AwaitQueryRealm`/`BuildRealmAuth` consume it via `take()`. Do not add a `self.resp = None` to `AwaitOpenRealm`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::handshake_tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Run the whole crate + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol && cargo clippy -p tobii-protocol --all-targets -- -D warnings`
Expected: all tests pass; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/handshake.rs
git commit -m "feat(protocol): add connection handshake state machine

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Re-exports, fmt, README

**Files:**
- Modify: `crates/tobii-protocol/src/lib.rs`
- Modify: `crates/tobii-protocol/README.md`

- [ ] **Step 1: Add re-exports to `lib.rs`**

In the re-export block of `crates/tobii-protocol/src/lib.rs`, add (keep the existing `pub use` lines):

```rust
pub use handshake::{Handshake, HandshakeAction};
```

- [ ] **Step 2: Add the module to the README**

In `crates/tobii-protocol/README.md`, under `## Modules`, add this line after the `realm` entry:

```markdown
- `handshake` — `Handshake` connection state machine (hello → realm auth → subscribe).
```

- [ ] **Step 3: Verify, format, lint**

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt -p tobii-protocol
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol
export PATH="$HOME/.cargo/bin:$PATH"; cargo clippy -p tobii-protocol --all-targets -- -D warnings
```
Expected: fmt clean, all tests pass (Plan-1's 33 + 8 new = 41), clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/README.md
git commit -m "chore(protocol): re-export Handshake, update README

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Roadmap (after this plan)

With the handshake done, `tobii-protocol` is the complete protocol engine. Next:

- **Plan 3 — `tobii-usb` + `tobii stream` (FIRST LIVE-HARDWARE milestone):** a new `tobii-usb` crate depending on `rusb`, implementing `UsbTransport` (open `2104:0313`, detach kernel driver on interface 0, claim, vendor control `0x41` session-open / `0x42` close, bulk OUT `0x05` / IN `0x83` with the reference's timeout pattern). A driver loop wires `Handshake` + `Parser` + `GazeSample`. A `tobii-cli` binary `tobii stream` prints live gaze. Runs Spike R2 (does the device stream after just the handshake — i.e. is `realm_type` 0 or auth-required on the gaze stream?). **Plug in the ET5 for the final task of that plan.**
- **Plan 4 — `tobii-config`** (Spike S3: MSI display-setup decompile).
- **Plan 5 — `tobii-headpose` + opentrack** (Spikes S1 pitch, S2 UDP format) + the live Star Citizen test. (See memory `gaze-decode-deferred-columns` for the extra gaze columns to add here.)

## Plan self-review notes

- **Spec coverage:** the spec's handshake requirement (hello → query/open realm `0x640`/`0x76c` → HMAC-MD5 challenge response `0x776` → subscribe `0x4c4`, stream `0x500`) is implemented in Task 2; realm-field parsing in Task 1. Calibration realm sub-machines remain out of scope (Phase 2), as the spec dictates.
- **No placeholders:** every step has complete code and exact commands. The one called-out deletion (`build_close_realm as _unused_close`) is explicit, not a TODO.
- **Type consistency:** `Handshake::new`, `poll() -> HandshakeAction`, `on_response(&[u8])`, and the `resp_*` helpers are used with identical signatures across Tasks 1–2 and the re-exports in Task 3. Op-code constants (`OP_HELLO` etc.) referenced in tests already exist in `frame.rs` from Plan 1.
- **Hardware-free:** all 8 new tests use crafted byte payloads and a mock-driven `run()` loop; no USB.

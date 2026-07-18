# Calibration Protocol + EDID Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the backend for per-user gaze calibration (the calibration wire ops + blob persistence) and monitor EDID auto-detection, with a headless validation path — no GUI.

**Architecture:** Pure calibration payload builders in `tobii-protocol` (reusing the existing TLV/Q42/frame primitives) + thin `Connection` methods in `tobii-usb` driving them through the existing `request()` (enter/leave reuse the realm ops the handshake already runs). EDID parsing + calibration-blob persistence live in `tobii-config`. `tobii-cli` gains EDID-seeded `setup` and a `tobii calibrate` command. This is sub-project A + B1 of the Tobii config/calibration parity effort; the GUI (B2/B3) is a later spec.

**Tech Stack:** Rust (edition 2021), `cargo test`/`clippy`. Builds on `tobii-protocol`, `tobii-usb` (incl. `Connection::request` from the display-area work), and `tobii-config`. Reference: `docs/superpowers/specs/2026-07-19-tobii-calibration-edid-backend-design.md` (verified protocol reference in §4). GPL-3.0.

## Global Constraints

- Rust **edition 2021**, license **GPL-3.0-only** (inherited via `.workspace`).
- **Zero new external dependencies.** Calibration reuses existing `tobii-protocol` primitives; EDID parse + blob persistence are std-only. No `serde`/`toml`/GUI crates.
- All `cargo` commands: prefix `export PATH="$HOME/.cargo/bin:$PATH"`.
- Every task ends **rustfmt-clean** (`cargo fmt`) and **clippy-clean** (`cargo clippy --all-targets -- -D warnings`).
- **Calibration wire facts (verified, spec §4):** every calibration/realm request payload begins with the 2-byte prefix `00 00`. New TTP ops: `cal_add_point 0x408`, `cal_compute 0x42f` (compute *and* apply), `cal_retrieve 0x44c`, `cal_apply 0x456`. `add_point` payload = `00 00` + **two bare Q42** (`x`, `y`, normalized `[0,1]`) + **u32** `eye` (0=both/1=L/2=R) — **no point2d prolog**. `compute`/`retrieve` payload = `00 00`. `apply` payload = `00 00` + **raw blob** (no TLV header). `retrieve` response payload **is** the opaque blob, verbatim. First response to `compute`/`apply` = done (no status field). Enter/leave reuse the existing realm ops (`0x640/0x76c/0x776/0x77b`).
- **Realm assumption (spec §10 Q1):** the backend assumes the realm unlocked during the handshake also authorizes calibration, so the `Connection` calibration methods issue ops on the already-open connection with **no re-unlock**. If the live device NAKs (Task 7), a follow-up adds an explicit re-unlock — do not add it speculatively.
- Commit messages end with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

## File Structure

- `crates/tobii-protocol/src/frame.rs` — add 4 `OP_CAL_*` constants (modify).
- `crates/tobii-protocol/src/calibration.rs` — **new**: payload builders + `CalibrationBlob`.
- `crates/tobii-protocol/src/lib.rs` — add `pub mod calibration;` + re-export `CalibrationBlob` (modify).
- `crates/tobii-usb/src/transport.rs` — add `UsbError::NoResponse { op }` (modify).
- `crates/tobii-usb/src/connection.rs` — add `Connection` calibration methods (modify).
- `crates/tobii-config/src/edid.rs` — **new**: EDID parse + monitor detect.
- `crates/tobii-config/src/store.rs` — add calibration-blob save/load (modify).
- `crates/tobii-config/src/lib.rs` — wire `edid` module + re-exports (modify).
- `crates/tobii-config/src/testdata/odyssey-g93sc.edid` — EDID fixture (**already committed**).
- `crates/tobii-cli/src/main.rs` — EDID-seeded `setup` + `tobii calibrate` (modify).
- `README.md` — document calibrate + auto-detect (modify).

---

### Task 1: calibration payload builders + op constants (tobii-protocol)

**Files:**
- Modify: `crates/tobii-protocol/src/frame.rs`
- Create: `crates/tobii-protocol/src/calibration.rs`
- Modify: `crates/tobii-protocol/src/lib.rs`

**Interfaces:**
- Consumes: `crate::bytes::Writer`, `crate::tlv::{write_f64_q42, write_u32}`.
- Produces:
  - `frame::{OP_CAL_ADD_POINT=0x408, OP_CAL_COMPUTE=0x42f, OP_CAL_RETRIEVE=0x44c, OP_CAL_APPLY=0x456}`
  - `calibration::cal_add_point_payload(x: f64, y: f64, eye: u32) -> Vec<u8>`
  - `calibration::cal_compute_payload() -> Vec<u8>`
  - `calibration::cal_retrieve_payload() -> Vec<u8>`
  - `calibration::cal_apply_payload(blob: &[u8]) -> Vec<u8>`
  - `calibration::CalibrationBlob(pub Vec<u8>)`

- [ ] **Step 1: Add the op-code constants**

In `crates/tobii-protocol/src/frame.rs`, after the existing `OP_CLOSE_REALM` line (~line 27), add:

```rust
// Calibration ops (Phase 2). Enter/leave reuse the realm ops above.
pub const OP_CAL_ADD_POINT: u32 = 0x408;
pub const OP_CAL_COMPUTE: u32 = 0x42f; // compute AND apply
pub const OP_CAL_RETRIEVE: u32 = 0x44c;
pub const OP_CAL_APPLY: u32 = 0x456;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/tobii-protocol/src/calibration.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_point_payload_is_exact() {
        // x=0.25 -> q42 = 0x0000_0100_0000_0000; y=0.75 -> 0x0000_0300_0000_0000; eye=0.
        let p = cal_add_point_payload(0.25, 0.75, 0);
        let expected: &[u8] = &[
            0x00, 0x00, // universal prefix
            0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // Q42(0.25)
            0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, // Q42(0.75)
            0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, // u32(0) eye
        ];
        assert_eq!(p, expected);
        assert_eq!(p.len(), 37); // frame will be 8 + 24 + 37 = 69
    }

    #[test]
    fn compute_and_retrieve_payloads_are_prefix_only() {
        assert_eq!(cal_compute_payload(), vec![0x00, 0x00]);
        assert_eq!(cal_retrieve_payload(), vec![0x00, 0x00]);
    }

    #[test]
    fn apply_payload_is_prefix_plus_raw_blob() {
        let p = cal_apply_payload(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(p, vec![0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn add_point_encodes_eye_choice() {
        // eye is the trailing u32; byte at index 36 is its low byte.
        assert_eq!(*cal_add_point_payload(0.5, 0.5, 2).last().unwrap(), 2);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib calibration::tests`
Expected: FAIL — `cannot find function cal_add_point_payload`.

- [ ] **Step 4: Write the implementation**

PREPEND to `crates/tobii-protocol/src/calibration.rs` (above the tests):

```rust
//! Per-user gaze-calibration wire payloads (Phase 2).
//!
//! Payload builders only — the op code is applied by the transport's
//! request/response path (`Connection::request`). Every payload carries the
//! universal 2-byte `00 00` prefix. See the design spec §4 for the wire facts.

use crate::bytes::Writer;
use crate::tlv::{write_f64_q42, write_u32};

/// An opaque device calibration blob (the verbatim `cal_retrieve` response).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalibrationBlob(pub Vec<u8>);

/// `cal_add_point` payload: `00 00` + Q42(x) + Q42(y) + u32(eye).
/// `x`/`y` are normalized display coords in `[0,1]`; `eye` is 0=both/1=L/2=R.
/// Note: two bare Q42 fields — NOT a point2d prolog.
pub fn cal_add_point_payload(x: f64, y: f64, eye: u32) -> Vec<u8> {
    let mut p = Writer::new();
    p.push_u8(0);
    p.push_u8(0);
    write_f64_q42(&mut p, x);
    write_f64_q42(&mut p, y);
    write_u32(&mut p, eye);
    p.into_vec()
}

/// `cal_compute` (compute AND apply) payload: the `00 00` prefix only.
pub fn cal_compute_payload() -> Vec<u8> {
    vec![0x00, 0x00]
}

/// `cal_retrieve` payload: the `00 00` prefix only.
pub fn cal_retrieve_payload() -> Vec<u8> {
    vec![0x00, 0x00]
}

/// `cal_apply` payload: `00 00` + the raw blob bytes (no TLV header).
pub fn cal_apply_payload(blob: &[u8]) -> Vec<u8> {
    let mut p = Writer::new();
    p.push_u8(0);
    p.push_u8(0);
    p.push_bytes(blob);
    p.into_vec()
}
```

- [ ] **Step 5: Wire the module + re-export**

In `crates/tobii-protocol/src/lib.rs`, add `pub mod calibration;` in the module list (alphabetically, after `pub mod bytes;`) and add to the re-export block:

```rust
pub use calibration::CalibrationBlob;
```

- [ ] **Step 6: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib calibration::tests && cargo clippy -p tobii-protocol --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 4 tests pass; clippy clean. (If `Writer::push_bytes` is named differently, check `crates/tobii-protocol/src/bytes.rs` — it exposes `push_bytes(&mut self, &[u8])`.)

- [ ] **Step 7: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-protocol/src/frame.rs crates/tobii-protocol/src/calibration.rs crates/tobii-protocol/src/lib.rs
git commit -m "feat(protocol): add calibration op codes + payload builders

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `Connection` calibration methods (tobii-usb)

**Files:**
- Modify: `crates/tobii-usb/src/transport.rs` (add `UsbError::NoResponse`)
- Modify: `crates/tobii-usb/src/connection.rs`

**Interfaces:**
- Consumes: `Connection::request` (existing); `tobii_protocol::frame::{OP_CAL_ADD_POINT, OP_CAL_COMPUTE, OP_CAL_RETRIEVE, OP_CAL_APPLY}`; `tobii_protocol::calibration::{cal_add_point_payload, cal_compute_payload, cal_retrieve_payload, cal_apply_payload, CalibrationBlob}`.
- Produces:
  - `UsbError::NoResponse { op: u32 }`
  - `Connection::add_calibration_point(&mut self, x: f64, y: f64, eye: u32) -> Result<(), UsbError>`
  - `Connection::compute_and_apply_calibration(&mut self) -> Result<(), UsbError>`
  - `Connection::retrieve_calibration(&mut self) -> Result<CalibrationBlob, UsbError>`
  - `Connection::apply_calibration(&mut self, blob: &[u8]) -> Result<(), UsbError>`

- [ ] **Step 1: Add the `NoResponse` error variant**

In `crates/tobii-usb/src/transport.rs`, add a variant to `enum UsbError` (after `Handshake`):

```rust
    /// A request was sent but no matching response arrived within the read window.
    NoResponse { op: u32 },
```

and add its `Display` arm (in the `match self` of the `Display` impl, after the `Handshake` arm):

```rust
            UsbError::NoResponse { op } => write!(f, "no device response for op {op:#x}"),
```

- [ ] **Step 2: Write the failing tests**

In `crates/tobii-usb/src/connection.rs`, inside `mod tests`, add. These reuse the existing `MockTransport`, `inbound`, `realm_type_zero` helpers. The `Vec::new()` filler at index 4 is consumed by the extra drain `run_handshake` does after the subscribe send (same reason as `request_queues_gaze_that_trails_the_response_in_one_chunk`), so the calibration response survives for the method to read:

```rust
    fn connected_with(post: Vec<Vec<u8>>) -> Connection<MockTransport> {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            Vec::new(), // filler consumed by the post-subscribe drain
        ]);
        to_recv.extend(post);
        Connection::connect(MockTransport { sent: Vec::new(), to_recv }).expect("connect")
    }

    #[test]
    fn add_calibration_point_gets_ack() {
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x408, &[])]);
        assert!(conn.add_calibration_point(0.25, 0.75, 0).is_ok());
    }

    #[test]
    fn retrieve_calibration_returns_blob_verbatim() {
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x44c, &[0xDE, 0xAD, 0xBE, 0xEF])]);
        let blob = conn.retrieve_calibration().expect("blob");
        assert_eq!(blob.0, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn compute_without_response_errors() {
        // No post-connect responses -> compute drains to NoResponse.
        let mut conn = connected_with(vec![]);
        assert!(matches!(
            conn.compute_and_apply_calibration(),
            Err(UsbError::NoResponse { op }) if op == 0x42f
        ));
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests`
Expected: FAIL — `no method named add_calibration_point` (and `no variant NoResponse` until Step 1 built).

- [ ] **Step 4: Write the implementation**

In `crates/tobii-usb/src/connection.rs`, extend the imports:

```rust
use tobii_protocol::calibration::{
    cal_add_point_payload, cal_apply_payload, cal_compute_payload, cal_retrieve_payload,
    CalibrationBlob,
};
use tobii_protocol::frame::{
    OP_CAL_ADD_POINT, OP_CAL_APPLY, OP_CAL_COMPUTE, OP_CAL_RETRIEVE,
};
```

(Add these alongside the existing `tobii_protocol::frame::{...}` and `tobii_protocol::{...}` use lines; keep the existing imports.)

Then add these methods inside `impl<T: Transport> Connection<T>` (after `request`):

```rust
    /// Send a request and require a matching response (calibration ops always
    /// reply). Returns the response payload, or `NoResponse` on timeout.
    fn expect_response(&mut self, op: u32, payload: &[u8]) -> Result<Vec<u8>, UsbError> {
        self.request(op, payload)?.ok_or(UsbError::NoResponse { op })
    }

    /// Sample one calibration stimulus point. `x`/`y` normalized `[0,1]`;
    /// `eye` 0=both/1=L/2=R. Assumes calibration runs in the already-open realm.
    pub fn add_calibration_point(&mut self, x: f64, y: f64, eye: u32) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_ADD_POINT, &cal_add_point_payload(x, y, eye))?;
        Ok(())
    }

    /// Compute and apply the calibration from the collected points.
    pub fn compute_and_apply_calibration(&mut self) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_COMPUTE, &cal_compute_payload())?;
        Ok(())
    }

    /// Retrieve the opaque calibration blob (the verbatim response payload).
    pub fn retrieve_calibration(&mut self) -> Result<CalibrationBlob, UsbError> {
        let payload = self.expect_response(OP_CAL_RETRIEVE, &cal_retrieve_payload())?;
        Ok(CalibrationBlob(payload))
    }

    /// Re-apply a previously saved calibration blob.
    pub fn apply_calibration(&mut self, blob: &[u8]) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_APPLY, &cal_apply_payload(blob))?;
        Ok(())
    }
```

- [ ] **Step 5: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests && cargo clippy -p tobii-usb --all-targets -- -D warnings 2>&1 | tail -2`
Expected: all connection tests pass (existing + 3 new); clippy clean.

- [ ] **Step 6: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-usb/src/transport.rs crates/tobii-usb/src/connection.rs
git commit -m "feat(usb): add Connection calibration methods (add-point/compute/retrieve/apply)

Reuse the already-open realm (no re-unlock); mock-tested. New UsbError::NoResponse.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: EDID parser + monitor detection (tobii-config)

**Files:**
- Create: `crates/tobii-config/src/edid.rs`
- Modify: `crates/tobii-config/src/lib.rs`
- Fixture (already committed): `crates/tobii-config/src/testdata/odyssey-g93sc.edid`

**Interfaces:**
- Produces:
  - `edid::MonitorInfo { pub model: String, pub width_mm: f64, pub height_mm: f64 }`
  - `edid::parse_edid(edid: &[u8]) -> Option<MonitorInfo>`
  - `edid::detect_monitors() -> Vec<MonitorInfo>`

- [ ] **Step 1: Write the failing tests**

Create `crates/tobii-config/src/edid.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_odyssey_g93sc() {
        let bytes = include_bytes!("testdata/odyssey-g93sc.edid");
        let m = parse_edid(bytes).expect("valid EDID parses");
        assert_eq!(m.model, "Odyssey G93SC");
        assert!((m.width_mm - 1193.0).abs() < 1.0, "width_mm={}", m.width_mm);
        assert!((m.height_mm - 336.0).abs() < 1.0, "height_mm={}", m.height_mm);
    }

    #[test]
    fn rejects_bad_header_and_short_input() {
        assert!(parse_edid(&[0u8; 128]).is_none()); // header all zero
        assert!(parse_edid(&[0xff; 10]).is_none()); // too short
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib edid::tests`
Expected: FAIL — `cannot find function parse_edid` (also add `mod edid;` in Step 4 or this fails to compile — expected FAIL either way).

- [ ] **Step 3: Write the implementation**

PREPEND to `crates/tobii-config/src/edid.rs` (above the tests):

```rust
//! Monitor EDID parsing: physical size (mm) + model, from `/sys/class/drm/*/edid`.
//!
//! We read only the 128-byte base block. Physical size comes from the first
//! Detailed Timing Descriptor's image size (mm), falling back to the basic
//! display block's cm field. Pure `parse_edid`; `detect_monitors` does the I/O.

use std::path::Path;

/// A detected monitor's model name and physical active-area size (mm).
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub model: String,
    pub width_mm: f64,
    pub height_mm: f64,
}

const EDID_HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Parse an EDID base block into [`MonitorInfo`]. Returns `None` if the header
/// is invalid, the block is too short, or no physical size can be determined.
pub fn parse_edid(edid: &[u8]) -> Option<MonitorInfo> {
    if edid.len() < 128 || edid[0..8] != EDID_HEADER {
        return None;
    }

    // Physical size (mm): prefer the first Detailed Timing Descriptor at byte 54.
    // A descriptor is a DTD when its pixel-clock (bytes 54..56) is non-zero;
    // its image size mm is bytes 66 (h low), 67 (v low), 68 (upper nibbles).
    let (mut w_mm, mut h_mm) = (0u32, 0u32);
    if edid[54] != 0 || edid[55] != 0 {
        w_mm = edid[66] as u32 | (((edid[68] >> 4) as u32) << 8);
        h_mm = edid[67] as u32 | (((edid[68] & 0x0f) as u32) << 8);
    }
    // Fallback: basic display block cm (bytes 21, 22) -> mm.
    if w_mm == 0 || h_mm == 0 {
        w_mm = edid[21] as u32 * 10;
        h_mm = edid[22] as u32 * 10;
    }
    if w_mm == 0 || h_mm == 0 {
        return None;
    }

    // Model name: the descriptor tagged 0xFC (bytes 0..3 == 00 00 00, byte 3 == FC).
    let mut model = String::new();
    for &off in &[54usize, 72, 90, 108] {
        let d = &edid[off..off + 18];
        if d[0] == 0 && d[1] == 0 && d[2] == 0 && d[3] == 0xfc {
            model = d[5..18]
                .iter()
                .take_while(|&&b| b != 0x0a)
                .map(|&b| b as char)
                .collect::<String>()
                .trim()
                .to_string();
            break;
        }
    }

    Some(MonitorInfo { model, width_mm: w_mm as f64, height_mm: h_mm as f64 })
}

/// Read every `/sys/class/drm/*/edid` and parse the ones that are valid.
pub fn detect_monitors() -> Vec<MonitorInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Path::new("/sys/class/drm")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("edid");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(info) = parse_edid(&bytes) {
                out.push(info);
            }
        }
    }
    out
}
```

- [ ] **Step 4: Wire the module + re-export**

In `crates/tobii-config/src/lib.rs`, add `mod edid;` (after `mod setup;`) and extend the re-exports:

```rust
pub use edid::{detect_monitors, MonitorInfo};
```

- [ ] **Step 5: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib edid::tests && cargo clippy -p tobii-config --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 2 tests pass; clippy clean.

- [ ] **Step 6: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-config/src/edid.rs crates/tobii-config/src/lib.rs
git commit -m "feat(config): EDID parse + monitor auto-detect

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: calibration-blob persistence (tobii-config)

**Files:**
- Modify: `crates/tobii-config/src/store.rs`

**Interfaces:**
- Consumes: `crate::store::config_path` (existing).
- Produces:
  - `store::calibration_path() -> std::path::PathBuf`
  - `store::save_calibration_to(path: &Path, blob: &[u8]) -> std::io::Result<()>`
  - `store::load_calibration_from(path: &Path) -> std::io::Result<Option<Vec<u8>>>`
  - `store::save_calibration(blob: &[u8]) -> std::io::Result<()>`
  - `store::load_calibration() -> std::io::Result<Option<Vec<u8>>>`

- [ ] **Step 1: Write the failing tests**

In `crates/tobii-config/src/store.rs` `mod tests`, add:

```rust
    #[test]
    fn calibration_blob_roundtrips() {
        let dir = std::env::temp_dir().join("tobii-config-test-cal");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("calibration.bin");
        let blob = vec![0x01, 0x02, 0x03, 0xFE, 0xFF];
        save_calibration_to(&path, &blob).expect("save");
        assert_eq!(load_calibration_from(&path).expect("load io").expect("some"), blob);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_calibration_missing_is_none() {
        let path = std::env::temp_dir()
            .join("tobii-config-test-cal-missing")
            .join("calibration.bin");
        let _ = std::fs::remove_file(&path);
        assert!(load_calibration_from(&path).expect("io ok").is_none());
    }

    #[test]
    fn calibration_path_sits_beside_config() {
        assert!(calibration_path().ends_with("tobii-linux/calibration.bin"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib store::tests::calibration`
Expected: FAIL — `cannot find function save_calibration_to`.

- [ ] **Step 3: Write the implementation**

In `crates/tobii-config/src/store.rs`, add (after the existing `load` fn):

```rust
/// Path to the calibration blob, beside `config.toml`.
pub fn calibration_path() -> PathBuf {
    config_path().with_file_name("calibration.bin")
}

/// Write the opaque calibration blob to `path`, creating parent dirs as needed.
pub fn save_calibration_to(path: &Path, blob: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, blob)
}

/// Read a calibration blob from `path`. `Ok(None)` if the file does not exist.
pub fn load_calibration_from(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save to the default [`calibration_path`].
pub fn save_calibration(blob: &[u8]) -> io::Result<()> {
    save_calibration_to(&calibration_path(), blob)
}

/// Load from the default [`calibration_path`].
pub fn load_calibration() -> io::Result<Option<Vec<u8>>> {
    load_calibration_from(&calibration_path())
}
```

- [ ] **Step 4: Re-export**

In `crates/tobii-config/src/lib.rs`, extend the `pub use store::{...}` line to add `calibration_path, load_calibration, load_calibration_from, save_calibration, save_calibration_to`.

- [ ] **Step 5: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib store::tests && cargo clippy -p tobii-config --all-targets -- -D warnings 2>&1 | tail -2`
Expected: all store tests pass; clippy clean.

- [ ] **Step 6: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-config/src/store.rs crates/tobii-config/src/lib.rs
git commit -m "feat(config): persist the opaque calibration blob (calibration.bin)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `tobii calibrate` + EDID-seeded `setup` (tobii-cli)

**Files:**
- Modify: `crates/tobii-cli/src/main.rs`

**Interfaces:**
- Consumes: `tobii_config::{detect_monitors, save_calibration, load_calibration}`; `Connection::{add_calibration_point, compute_and_apply_calibration, retrieve_calibration, apply_calibration}`; existing `UsbTransport`, `DisplaySetup`, `prompt_f64`.

- [ ] **Step 1: Seed `setup` defaults from EDID**

In `crates/tobii-cli/src/main.rs`, at the top of `setup()` (before the prompts), add:

```rust
    let (mut w_def, mut h_def) = (600.0, 340.0);
    let monitors = tobii_config::detect_monitors();
    if let Some(m) = monitors.iter().find(|m| m.width_mm > 0.0 && m.height_mm > 0.0) {
        println!("detected monitor: {} ({:.0} x {:.0} mm)", m.model, m.width_mm, m.height_mm);
        w_def = m.width_mm;
        h_def = m.height_mm;
    }
```

and change the first two prompts to use the detected defaults:

```rust
        width_mm: prompt_f64("Monitor active-area WIDTH (mm)", w_def)?,
        height_mm: prompt_f64("Monitor active-area HEIGHT (mm)", h_def)?,
```

(Leave the other four prompts unchanged.)

- [ ] **Step 2: Add the `calibrate` command + dispatch**

In `main()`'s match, add an arm (after the `display` arm):

```rust
        (Some("calibrate"), _) => calibrate(args.iter().any(|a| a == "--apply")),
```

and update the `usage` text to include:

```
  tobii calibrate [--apply]
```

Then add the command functions (near `display_set`):

```rust
/// Host-chosen stimulus points (normalized). Center then four corners, inset
/// from the edges. NOTE: headless — no dots are drawn, so this validates the
/// protocol, not gaze accuracy (accuracy needs the stimulus UI, a later phase).
const CAL_POINTS: [(f64, f64); 5] = [(0.5, 0.5), (0.1, 0.1), (0.9, 0.1), (0.1, 0.9), (0.9, 0.9)];

fn calibrate(apply_saved: bool) -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;

    if apply_saved {
        let blob = tobii_config::load_calibration()?
            .ok_or("no saved calibration — run `tobii calibrate` first")?;
        conn.apply_calibration(&blob)?;
        println!("re-applied saved calibration ({} bytes).", blob.len());
        return Ok(());
    }

    eprintln!(
        "NOTE: headless calibration — no stimulus is drawn, so this validates the \
         protocol only, not gaze accuracy."
    );
    for (i, &(x, y)) in CAL_POINTS.iter().enumerate() {
        conn.add_calibration_point(x, y, 0)?;
        println!("  point {}/{} at ({x:.2}, {y:.2}) sampled", i + 1, CAL_POINTS.len());
    }
    conn.compute_and_apply_calibration()?;
    let blob = conn.retrieve_calibration()?;
    tobii_config::save_calibration(&blob.0)?;
    println!("calibration computed + applied; saved {} bytes.", blob.0.len());
    Ok(())
}
```

- [ ] **Step 3: Build + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-cli && cargo clippy -p tobii-cli --all-targets -- -D warnings 2>&1 | tail -3`
Expected: builds cleanly; clippy clean. (Running `calibrate` needs hardware — Task 7. `CalibrationBlob.0` is the `Vec<u8>`; `tobii_config::save_calibration` takes `&[u8]`.)

- [ ] **Step 4: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-cli/src/main.rs
git commit -m "feat(cli): add tobii calibrate [--apply]; seed setup from EDID

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: docs — README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document the new commands**

In `README.md`, in the `### Display setup` section (after the `tobii display set` line), add a calibration subsection and note EDID auto-detect. Insert after the existing setup command block:

```markdown

`tobii setup` auto-detects your monitor's size from its EDID (via
`/sys/class/drm`), pre-filling the width/height — just confirm or override.

### Gaze calibration (experimental)

    ./target/release/tobii calibrate          # run + save a calibration
    ./target/release/tobii calibrate --apply   # re-apply the saved calibration

Calibration is stored at `$XDG_CONFIG_HOME/tobii-linux/calibration.bin`.
The current `calibrate` is **headless** (no on-screen stimulus yet), so it
exercises the device protocol but does not itself produce an accurate
per-user calibration — the follow-the-dot stimulus UI is a later milestone.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document tobii calibrate + EDID auto-detect

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: LIVE smoke test (requires the ET5 + a monitor)

Proves the calibration protocol on hardware and settles the spec's four open
questions. Needs the physical ET5 (`2104:0313`) connected and the udev rule
installed (see README). **Do not skip or simulate.**

**Files:**
- (Possibly) Modify: `docs/superpowers/specs/2026-07-19-tobii-calibration-edid-backend-design.md` (record findings)

- [ ] **Step 1: Confirm device present**

Run: `lsusb | grep -i 2104`
Expected: `2104:0313`. If absent, ask the user to connect it (and install the udev rule) before continuing.

- [ ] **Step 2: EDID auto-detect end-to-end**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; printf '\n\n\n\n\n\n' | ./target/release/tobii setup`
Expected: prints `detected monitor: <model> (<w> x <h> mm)` and uses it as the width/height defaults (Enter accepts them). Confirms `detect_monitors()` works on real sysfs.

- [ ] **Step 3: Run headless calibration**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- calibrate`
Expected: five `point N/5 ... sampled` lines, then `calibration computed + applied; saved N bytes`. **Settle the open questions and record them in the design spec §10:**
- **Q1 realm:** did the calibration ops succeed on the already-open connection (no re-unlock)? If any op returned `no device response`/error, note it — a re-unlock is then required.
- **Q3 compute-done:** did `compute` return promptly with one response?
- **Q4 per-point ack:** did each `add_point` get an ack (no hang)?
- Note the blob size.

- [ ] **Step 4: Blob round-trip (Q2 — prefix hazard)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- calibrate --apply`
Expected: `re-applied saved calibration (N bytes).` with no device error. **If the device rejects the blob** (`no device response for op 0x456` or an error), the save→restore prefix is doubled (spec §10 Q2): add a follow-up that strips the leading 2 bytes of the retrieved blob before saving (or before apply), add a regression test, and re-run. Record the outcome in §10.

- [ ] **Step 5: Capture the real blob as a golden fixture**

Copy the saved blob into the repo as a fixture and add a decode/shape regression test:
```bash
cp "${XDG_CONFIG_HOME:-$HOME/.config}/tobii-linux/calibration.bin" crates/tobii-protocol/src/testdata/real-calibration.blob
```
Add a test to `crates/tobii-protocol/src/calibration.rs` `mod tests` that `include_bytes!`-loads it and asserts it is non-empty and `<= 4096` bytes (the observed device cap). Commit:
```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib calibration
git add crates/tobii-protocol/src/testdata/real-calibration.blob crates/tobii-protocol/src/calibration.rs docs/superpowers/specs/2026-07-19-tobii-calibration-edid-backend-design.md
git commit -m "test(protocol): real captured calibration blob fixture; record live findings

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Final verification**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E "test result: ok"` and `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -2`
Expected: all tests pass; clippy clean.

---

## Roadmap (after this backend lands)

- **B2** — GUI shell (toolkit chosen then) + display-setup screen (EDID-seeded) +
  eye-position view (needs decoding gaze cols `0x03`/`0x09`).
- **B3** — fullscreen follow-the-dot stimulus wired to this backend; gaze accuracy
  finally validated end-to-end.
- Independently: **Plan 5** (`tobii-headpose` + opentrack) for Star Citizen.

## Plan self-review notes

- **Spec coverage:** calibration ops + builders (§4/§5) → Task 1; `Connection` methods (§5d) → Task 2; EDID (A, §8) → Task 3; blob persistence (§9) → Task 4; CLI `calibrate` + EDID-seeded setup → Task 5; docs → Task 6; live validation + the four §10 open questions → Task 7. The `auto_apply`-on-connect config flag (§9) is intentionally deferred to the connect-flow/GUI (B2/B3) and exposed here as the explicit `tobii calibrate --apply` instead — no behavior silently changes on existing commands. `cal_stimulus 0x460` is out of scope per the spec (dead op). Eye-position column decode (§4.4) is a B2 concern, noted not built.
- **Zero-dependency constraint:** calibration reuses `tobii-protocol` primitives; EDID + persistence are std-only; no new crates.
- **Type consistency:** `cal_add_point_payload(f64,f64,u32)`, `cal_compute_payload()`, `cal_retrieve_payload()`, `cal_apply_payload(&[u8])`, `CalibrationBlob(Vec<u8>)` (Task 1) are consumed unchanged in Tasks 2 and 5. `OP_CAL_ADD_POINT/COMPUTE/RETRIEVE/APPLY` (Task 1, frame.rs) used in Task 2. `Connection::{add_calibration_point, compute_and_apply_calibration, retrieve_calibration, apply_calibration}` (Task 2) used in Task 5. `MonitorInfo{model,width_mm,height_mm}` + `detect_monitors` (Task 3) used in Task 5. `save_calibration`/`load_calibration` (Task 4) used in Task 5. `UsbError::NoResponse{op}` (Task 2) is the miss path.
- **Placeholder scan:** every code step is complete. The only intrinsic fill-in is the real captured blob bytes in Task 7 Step 5 (a hardware-capture step).
- **Hardware boundary:** Tasks 1–6 are fully verifiable on the dev machine (unit/mock tests + build); only Task 7 needs the ET5. The `add_point` golden bytes and frame sizes are cross-checked against the verified reference (spec §4).

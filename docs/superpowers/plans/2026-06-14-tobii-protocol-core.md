# tobii-protocol Codec Crate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tobii-protocol`, a pure, dependency-free Rust crate that encodes/decodes the Tobii Eye Tracker 5 USB wire protocol (TTP framing, TLV/Q42 codec, command frames, realm HMAC-MD5 auth, inbound reassembly, gaze + display-area decode), fully unit-tested against golden byte vectors with no hardware required.

**Architecture:** One library crate inside a Cargo workspace. No I/O, no globals, no external dependencies (std only). Outbound builders return `Vec<u8>`; the inbound `Parser` accumulates USB chunks and yields complete `Frame`s; decoders turn payloads into typed `GazeSample` / `DisplayCorners`. This crate is the protocol reference for every later crate (`tobii-usb`, `tobii-config`, `tobii-headpose`).

**Tech Stack:** Rust (edition 2021), `cargo test`. Reference implementation: `ressources/tobiifree/driver/src/{tobiifree_core.zig,tlv.zig}` (GPL-3.0). This crate is GPL-3.0.

**Scope note:** This is Plan 1 of the v1 roadmap. It deliberately excludes the handshake *state machine* (paired with the USB transport in Plan 2, where it can be exercised live) and all calibration ops (Phase 2). It DOES include the realm frame builders + HMAC-MD5, since those are pure and testable now.

---

### Task 1: Workspace + crate skeleton + toolchain

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/tobii-protocol/Cargo.toml`
- Create: `crates/tobii-protocol/src/lib.rs`
- Modify: `.gitignore`

- [ ] **Step 1: Install the Rust toolchain**

Rust is not installed yet. On CachyOS (Arch):

```bash
sudo pacman -S --needed rustup && rustup default stable
rustc --version && cargo --version
```

Expected: prints a `rustc 1.x` and `cargo 1.x` version. (The `sudo` step may prompt for a password; if `rustup` is already present, only the `rustup default stable` line is needed.)

- [ ] **Step 2: Add Rust build output to .gitignore**

Append these lines to `.gitignore`:

```gitignore
# Rust
target/
Cargo.lock
```

(Workspace is a library-only repo at this stage, so `Cargo.lock` is not committed yet — revisit when the first binary crate lands in Plan 2.)

- [ ] **Step 3: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/tobii-protocol"]

[workspace.package]
edition = "2021"
license = "GPL-3.0-only"
authors = ["Fabian Plaimauer"]
repository = ""
```

- [ ] **Step 4: Create `crates/tobii-protocol/Cargo.toml`**

```toml
[package]
name = "tobii-protocol"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Pure codec for the Tobii Eye Tracker 5 USB wire protocol (TTP/TLV)."

[dependencies]
```

(No dependencies — this crate is std-only on purpose.)

- [ ] **Step 5: Create a minimal `crates/tobii-protocol/src/lib.rs`**

```rust
//! Pure codec for the Tobii Eye Tracker 5 USB wire protocol.
//!
//! No I/O, no global state, no external dependencies. Outbound builders
//! return `Vec<u8>`; the inbound [`parser::Parser`] yields complete frames;
//! decoders produce typed [`gaze::GazeSample`] / [`display::DisplayCorners`].
//!
//! Protocol decoded by the `tobiifree` project (GPL-3.0) from USB captures.

pub mod error;
```

- [ ] **Step 6: Verify it builds**

Run: `cargo build -p tobii-protocol`
Expected: compiles with no errors (a warning about the empty `error` module is fine until Task 2; if the module doesn't exist yet the build fails — create `error.rs` as an empty file first, or do Task 2 before building).

To avoid that, create an empty placeholder now:

Run: `touch crates/tobii-protocol/src/error.rs && cargo build -p tobii-protocol`
Expected: `Finished` with no errors.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/tobii-protocol .gitignore
git commit -m "feat(protocol): scaffold tobii-protocol crate + workspace"
```

---

### Task 2: Error type

**Files:**
- Modify: `crates/tobii-protocol/src/error.rs`
- Test: inline `#[cfg(test)]` in `error.rs`

- [ ] **Step 1: Write the failing test**

Put this in `crates/tobii-protocol/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays_human_text() {
        let e = ProtocolError::WrongType { expected: 2, found: 5 };
        assert_eq!(format!("{e}"), "wrong TLV type: expected 2, found 5");
        assert!(ProtocolError::ShortRead.to_string().contains("short read"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tobii-protocol error_displays_human_text`
Expected: FAIL — `cannot find type ProtocolError`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/tobii-protocol/src/error.rs` (above the test module):

```rust
//! Error type for the protocol codec.

/// Errors produced while encoding or (mostly) decoding protocol bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Not enough bytes remaining to read the requested field.
    ShortRead,
    /// A TLV field had an unexpected type byte.
    WrongType { expected: u8, found: u8 },
    /// A TLV field had an unexpected size.
    WrongSize { expected: u32, found: u32 },
    /// A prolog/struct tag did not match what was expected.
    WrongTag { expected: u32, found: u32 },
    /// Inbound USB envelope had a direction byte other than 0x01.
    BadDirection(u8),
    /// Inbound envelope length field was impossibly small.
    BadLength(u32),
    /// Reassembly accumulator would overflow its cap.
    Overflow,
}

impl core::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProtocolError::ShortRead => write!(f, "short read: not enough bytes"),
            ProtocolError::WrongType { expected, found } => {
                write!(f, "wrong TLV type: expected {expected}, found {found}")
            }
            ProtocolError::WrongSize { expected, found } => {
                write!(f, "wrong TLV size: expected {expected}, found {found}")
            }
            ProtocolError::WrongTag { expected, found } => {
                write!(f, "wrong tag: expected {expected:#x}, found {found:#x}")
            }
            ProtocolError::BadDirection(b) => write!(f, "bad envelope direction byte: {b:#x}"),
            ProtocolError::BadLength(n) => write!(f, "bad envelope length: {n}"),
            ProtocolError::Overflow => write!(f, "reassembly buffer overflow"),
        }
    }
}

impl std::error::Error for ProtocolError {}

```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p tobii-protocol error_displays_human_text`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-protocol/src/error.rs
git commit -m "feat(protocol): add ProtocolError type"
```

---

### Task 3: Byte writer

**Files:**
- Create: `crates/tobii-protocol/src/bytes.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod bytes;`)
- Test: inline in `bytes.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs`, add after `pub mod error;`:

```rust
pub mod bytes;
```

- [ ] **Step 2: Write the failing test**

Create `crates/tobii-protocol/src/bytes.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_big_and_little_endian() {
        let mut w = Writer::new();
        w.push_u8(0xAB);
        w.push_be32(0x0011_2233);
        w.push_le32(0x0011_2233);
        w.push_be64(0x0102_0304_0506_0708);
        w.push_bytes(&[0xEE, 0xFF]);
        assert_eq!(
            w.into_vec(),
            vec![
                0xAB, // u8
                0x00, 0x11, 0x22, 0x33, // be32
                0x33, 0x22, 0x11, 0x00, // le32
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // be64
                0xEE, 0xFF, // bytes
            ]
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p tobii-protocol writes_big_and_little_endian`
Expected: FAIL — `cannot find type Writer`.

- [ ] **Step 4: Write the implementation**

Prepend to `bytes.rs` (above the tests):

```rust
//! A tiny growable byte writer with explicit-endianness helpers.

/// Accumulates bytes for an outbound frame.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n) }
    }

    pub fn push_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn push_be32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn push_be64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn push_le32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn push_bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p tobii-protocol writes_big_and_little_endian`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/bytes.rs
git commit -m "feat(protocol): add byte Writer"
```

---

### Task 4: TLV encoders + Q42

**Files:**
- Create: `crates/tobii-protocol/src/tlv.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod tlv;`)
- Test: inline in `tlv.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod tlv;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/tobii-protocol/src/tlv.rs` with:

```rust
#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::bytes::Writer;

    #[test]
    fn q42_matches_reference() {
        // Ported from tobiifree: q42_encode(200.0) == 879609302220800.
        assert_eq!(q42_encode(200.0), 879_609_302_220_800);
        assert_eq!(q42_encode(0.0), 0);
        assert_eq!(q42_encode(-200.0), -879_609_302_220_800);
    }

    #[test]
    fn encodes_u32_tlv() {
        let mut w = Writer::new();
        write_u32(&mut w, 0x3039);
        // type=2, size=4 (BE), value=0x3039 (BE)
        assert_eq!(w.into_vec(), vec![0x02, 0, 0, 0, 4, 0, 0, 0x30, 0x39]);
    }

    #[test]
    fn encodes_tag_tlv() {
        let mut w = Writer::new();
        write_tag(&mut w, 0x10100);
        assert_eq!(w.into_vec(), vec![0x05, 0, 0, 0, 4, 0, 0x01, 0x01, 0x00]);
    }

    #[test]
    fn point_is_48_bytes() {
        let mut w = Writer::new();
        write_point(&mut w, 1.0, 2.0, 3.0);
        // tag(9) + 3 * f64_q42(13) = 9 + 39 = 48
        assert_eq!(w.len(), 48);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib tlv::encode_tests`
Expected: FAIL — `cannot find function q42_encode`.

- [ ] **Step 4: Write the implementation**

Prepend to `tlv.rs`:

```rust
//! TLV codec for the ET5 wire protocol.
//!
//! Field header is a 1-byte TYPE followed by a 4-byte big-endian SIZE,
//! then SIZE bytes of body. Struct fields begin with a `type=5` prolog
//! carrying a 4-byte tag.

use crate::bytes::Writer;
use crate::error::ProtocolError;

/// Q42 fixed-point scale: 2^42.
pub const Q42_SCALE: f64 = 4_398_046_511_104.0;

/// Struct tags (found after a type=5 prolog).
pub const TAG_XDS_ROW_MASK: u32 = 0x0bb8; // low 16 bits of an xds_row tag
pub const TAG_XDS_COLUMN: u32 = 0x020bb9;
pub const TAG_POINT2D: u32 = 0x021f40;
pub const TAG_POINT3D: u32 = 0x031f41;

/// Encode millimetres as a Q42 fixed-point integer: round(mm * 2^42).
pub fn q42_encode(mm: f64) -> i64 {
    (mm * Q42_SCALE).round() as i64
}

/// type=5 prolog carrying a 4-byte tag.
pub fn write_tag(w: &mut Writer, tag: u32) {
    w.push_u8(5);
    w.push_be32(4);
    w.push_be32(tag);
}

/// type=2 u32.
pub fn write_u32(w: &mut Writer, v: u32) {
    w.push_u8(2);
    w.push_be32(4);
    w.push_be32(v);
}

/// type=4 Q42 fixed-point f64 (8-byte BE signed body).
pub fn write_f64_q42(w: &mut Writer, v: f64) {
    w.push_u8(4);
    w.push_be32(8);
    w.push_be64(q42_encode(v) as u64);
}

/// point3d = prolog(0x031f41) + 3 × Q42.
pub fn write_point(w: &mut Writer, x: f64, y: f64, z: f64) {
    write_tag(w, TAG_POINT3D);
    write_f64_q42(w, x);
    write_f64_q42(w, y);
    write_f64_q42(w, z);
}

```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib tlv::encode_tests`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/tlv.rs
git commit -m "feat(protocol): add TLV encoders and Q42"
```

---

### Task 5: TLV Reader (decoders)

**Files:**
- Modify: `crates/tobii-protocol/src/tlv.rs` (add `Reader`)
- Test: inline in `tlv.rs`

- [ ] **Step 1: Write the failing tests**

Add this test module to `tlv.rs` (after `encode_tests`):

```rust
#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::bytes::Writer;

    #[test]
    fn round_trips_u32_and_q42() {
        let mut w = Writer::new();
        write_u32(&mut w, 0xDEAD_BEEF);
        write_f64_q42(&mut w, 12.5);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u32().unwrap(), 0xDEAD_BEEF);
        assert!((r.read_fixed22x42().unwrap() - 12.5).abs() < 1e-9);
    }

    #[test]
    fn reads_point3d() {
        let mut w = Writer::new();
        write_point(&mut w, -1.0, 2.0, 300.0);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        let p = r.read_point3d().unwrap();
        assert!((p[0] + 1.0).abs() < 1e-9);
        assert!((p[1] - 2.0).abs() < 1e-9);
        assert!((p[2] - 300.0).abs() < 1e-9);
    }

    #[test]
    fn xds_row_decodes_count_from_tag() {
        // xds_row tag = (count << 16) | 0x0bb8.
        let mut w = Writer::new();
        write_tag(&mut w, (5u32 << 16) | 0x0bb8);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_xds_row().unwrap(), 5);
    }

    #[test]
    fn short_read_errors() {
        let buf = [0x02u8, 0, 0, 0]; // truncated u32 header
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u32(), Err(crate::error::ProtocolError::ShortRead));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib tlv::reader_tests`
Expected: FAIL — `cannot find type Reader`.

- [ ] **Step 3: Write the implementation**

Add to `tlv.rs` (after the encoders, before the test modules):

```rust
/// Cursor over a TLV byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        if self.remaining() < 1 {
            return Err(ProtocolError::ShortRead);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining() < n {
            return Err(ProtocolError::ShortRead);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u32_be(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32_be(&mut self) -> Result<i32, ProtocolError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64_be(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// Read [type=5][size=4][tag:u32], returning the tag.
    pub fn read_prolog_tag(&mut self) -> Result<u32, ProtocolError> {
        let t = self.u8()?;
        if t != 5 {
            return Err(ProtocolError::WrongType { expected: 5, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        self.u32_be()
    }

    pub fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        let t = self.u8()?;
        if t != 2 {
            return Err(ProtocolError::WrongType { expected: 2, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        self.u32_be()
    }

    pub fn read_fixed16x16(&mut self) -> Result<f64, ProtocolError> {
        let t = self.u8()?;
        if t != 3 {
            return Err(ProtocolError::WrongType { expected: 3, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        Ok(self.i32_be()? as f64 / 65536.0)
    }

    pub fn read_fixed22x42(&mut self) -> Result<f64, ProtocolError> {
        let t = self.u8()?;
        if t != 4 {
            return Err(ProtocolError::WrongType { expected: 4, found: t });
        }
        let s = self.u32_be()?;
        if s != 8 {
            return Err(ProtocolError::WrongSize { expected: 8, found: s });
        }
        Ok(self.i64_be()? as f64 / Q42_SCALE)
    }

    pub fn read_s64(&mut self) -> Result<i64, ProtocolError> {
        let t = self.u8()?;
        if t != 6 {
            return Err(ProtocolError::WrongType { expected: 6, found: t });
        }
        let s = self.u32_be()?;
        if s != 8 {
            return Err(ProtocolError::WrongSize { expected: 8, found: s });
        }
        self.i64_be()
    }

    /// Consume an xds_row prolog; returns the column count packed in the tag.
    pub fn read_xds_row(&mut self) -> Result<u32, ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag & 0xffff != TAG_XDS_ROW_MASK {
            return Err(ProtocolError::WrongTag { expected: TAG_XDS_ROW_MASK, found: tag });
        }
        Ok((tag >> 16) & 0xfff)
    }

    /// Consume an xds_column prolog + u32; returns the column id.
    pub fn read_xds_column(&mut self) -> Result<u32, ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_XDS_COLUMN {
            return Err(ProtocolError::WrongTag { expected: TAG_XDS_COLUMN, found: tag });
        }
        self.read_u32()
    }

    pub fn read_point3d(&mut self) -> Result<[f64; 3], ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_POINT3D {
            return Err(ProtocolError::WrongTag { expected: TAG_POINT3D, found: tag });
        }
        Ok([self.read_fixed22x42()?, self.read_fixed22x42()?, self.read_fixed22x42()?])
    }

    pub fn read_point2d(&mut self) -> Result<[f64; 2], ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_POINT2D {
            return Err(ProtocolError::WrongTag { expected: TAG_POINT2D, found: tag });
        }
        Ok([self.read_fixed22x42()?, self.read_fixed22x42()?])
    }
}

```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib tlv::reader_tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-protocol/src/tlv.rs
git commit -m "feat(protocol): add TLV Reader decoders"
```

---

### Task 6: TTP frame builder + envelope + constants

**Files:**
- Create: `crates/tobii-protocol/src/frame.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod frame;`)
- Test: inline in `frame.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod frame;
```

- [ ] **Step 2: Write the failing test**

Create `crates/tobii-protocol/src/frame.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_frame_layout() {
        // Build a frame with op=0x3e8, seq=1, 3-byte payload.
        let f = build_out_frame(1, OP_HELLO, &[0xAA, 0xBB, 0xCC]);
        // envelope(8) + header(24) + payload(3) = 35
        assert_eq!(f.len(), 35);
        // envelope dir byte
        assert_eq!(f[0], 0x00);
        // LE length excludes envelope: 24 + 3 = 27
        assert_eq!(f[4], 27);
        assert_eq!(f[5], 0);
        // TTP magic BE = 0x51 at bytes 8..12
        assert_eq!(&f[8..12], &[0, 0, 0, 0x51]);
        // seq BE = 1 at bytes 12..16
        assert_eq!(&f[12..16], &[0, 0, 0, 1]);
        // op BE = 0x3e8 at bytes 20..24
        assert_eq!(&f[20..24], &[0, 0, 0x03, 0xe8]);
        // plen BE = 3 at bytes 28..32
        assert_eq!(&f[28..32], &[0, 0, 0, 3]);
        // payload
        assert_eq!(&f[32..35], &[0xAA, 0xBB, 0xCC]);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p tobii-protocol --lib frame::tests`
Expected: FAIL — `cannot find function build_out_frame`.

- [ ] **Step 4: Write the implementation**

Prepend to `frame.rs`:

```rust
//! TTP framing and the outbound USB envelope.
//!
//! Outbound wire format (host → device):
//!   [dir=0x00][0 0 0][len_LE:u32 = ttp_len][ttp_header:24 BE][payload]
//! The OUT envelope's length field excludes the 8-byte envelope itself.
//!
//! TTP header (24 bytes, big-endian):
//!   [magic:u32][seq:u32][flag:u32=0][op:u32][0:u32][plen:u32]

use crate::bytes::Writer;

pub const TTP_HDR_SIZE: usize = 24;
pub const ENVELOPE_SIZE: usize = 8;

pub const TTP_MAGIC_REQ: u32 = 0x51;
pub const TTP_MAGIC_RSP: u32 = 0x52;
pub const TTP_MAGIC_NOTIFY: u32 = 0x53;

// Operation codes used in v1 (calibration ops deferred to Phase 2).
pub const OP_HELLO: u32 = 0x3e8;
pub const OP_SUBSCRIBE: u32 = 0x4c4;
pub const OP_SET_DISPLAY_AREA: u32 = 0x5a0;
pub const OP_GET_DISPLAY_AREA: u32 = 0x596;
pub const OP_QUERY_REALM: u32 = 0x640;
pub const OP_OPEN_REALM: u32 = 0x76c;
pub const OP_REALM_RESPONSE: u32 = 0x776;
pub const OP_CLOSE_REALM: u32 = 0x77b;

/// The gaze notification stream id and its op code.
pub const STREAM_GAZE: u16 = 0x500;
pub const OP_GAZE_NOTIFY: u32 = 0x500;

/// Build a request TTP frame and wrap it in the outbound USB envelope.
pub fn build_out_frame(seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
    let mut ttp = Writer::with_capacity(TTP_HDR_SIZE + payload.len());
    ttp.push_be32(TTP_MAGIC_REQ);
    ttp.push_be32(seq);
    ttp.push_be32(0); // flag
    ttp.push_be32(op);
    ttp.push_be32(0);
    ttp.push_be32(payload.len() as u32);
    ttp.push_bytes(payload);
    let ttp = ttp.into_vec();

    let mut out = Writer::with_capacity(ENVELOPE_SIZE + ttp.len());
    out.push_u8(0x00);
    out.push_u8(0);
    out.push_u8(0);
    out.push_u8(0);
    out.push_le32(ttp.len() as u32);
    out.push_bytes(&ttp);
    out.into_vec()
}

```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p tobii-protocol --lib frame::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/frame.rs
git commit -m "feat(protocol): add TTP frame builder, envelope, constants"
```

---

### Task 7: Command builders (hello, subscribe, display area)

**Files:**
- Create: `crates/tobii-protocol/src/commands.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod commands;`)
- Test: inline in `commands.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod commands;
```

- [ ] **Step 2: Write the failing tests (ported golden vectors)**

Create `crates/tobii-protocol/src/commands.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_is_79_bytes() {
        let f = build_hello(1);
        // envelope(8) + header(24) + payload(47) = 79
        assert_eq!(f.len(), 79);
        assert_eq!(f[0], 0x00); // dir
        assert_eq!(f[4], 71); // LE len = 24 + 47
        assert_eq!(&f[8..12], &[0, 0, 0, 0x51]); // magic
        assert_eq!(&f[20..24], &[0, 0, 0x03, 0xe8]); // op
        assert_eq!(&f[28..32], &[0, 0, 0, 47]); // plen
        assert_eq!(f[32], 0x00); // payload[0]
    }

    #[test]
    fn subscribe_frame_carries_stream_id() {
        let f = build_subscribe(3, 0x500);
        // envelope(8) + header(24) + payload(20) = 52
        assert_eq!(f.len(), 52);
        assert_eq!(&f[20..24], &[0, 0, 0x04, 0xc4]); // op
        // stream_id at payload bytes 9..10 → frame 41..42
        assert_eq!(f[41], 0x05);
        assert_eq!(f[42], 0x00);
    }

    #[test]
    fn get_display_area_is_empty_payload() {
        let f = build_get_display_area(4);
        assert_eq!(f.len(), 32); // 8 + 24 + 0
        assert_eq!(&f[20..24], &[0, 0, 0x05, 0x96]);
    }

    #[test]
    fn set_display_area_frame_structure() {
        let f = build_set_display_area(2, 400.0, 300.0, -200.0, 0.0, 0.0);
        // payload = 2 + 3*48 + 9 (tag) + 9 (u32) = 164 → frame 196
        assert_eq!(f.len(), 196);
        assert_eq!(&f[20..24], &[0, 0, 0x05, 0xa0]);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib commands::tests`
Expected: FAIL — `cannot find function build_hello`.

- [ ] **Step 4: Write the implementation**

Prepend to `commands.rs`:

```rust
//! Outbound command frame builders (v1 subset).

use crate::bytes::Writer;
use crate::frame::{build_out_frame, OP_GET_DISPLAY_AREA, OP_HELLO, OP_SET_DISPLAY_AREA, OP_SUBSCRIBE};
use crate::tlv::{write_point, write_tag, write_u32};

/// Captured 47-byte hello payload (op 0x3e8).
const HELLO_PAYLOAD: [u8; 47] = [
    0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x09, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x04, 0x00,
    0x01, 0x00, 0x05, 0x00, 0x01, 0x00, 0x06, 0x00, 0x01, 0x00, 0x07, 0x00, 0x01, 0x00, 0x08,
];

pub fn build_hello(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_HELLO, &HELLO_PAYLOAD)
}

/// Subscribe to a TTP stream (stream_id at payload bytes 9..10, BE).
pub fn build_subscribe(seq: u32, stream_id: u16) -> Vec<u8> {
    let mut pay: [u8; 20] = [
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x00,
        0x04, 0x00, 0x00, 0x00, 0x00,
    ];
    pay[9] = (stream_id >> 8) as u8;
    pay[10] = stream_id as u8;
    build_out_frame(seq, OP_SUBSCRIBE, &pay)
}

pub fn build_get_display_area(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_GET_DISPLAY_AREA, &[])
}

/// Set display area from a rect in mm. `ox`/`oy` are the bottom-left offset
/// (tracker-relative); `z` is plane depth. Sends TL, TR, BL corners.
pub fn build_set_display_area(
    seq: u32,
    w_mm: f64,
    h_mm: f64,
    ox_mm: f64,
    oy_mm: f64,
    z_mm: f64,
) -> Vec<u8> {
    let x0 = ox_mm;
    let x1 = ox_mm + w_mm;
    let y0 = oy_mm;
    let y1 = oy_mm + h_mm;
    build_set_display_area_corners(
        seq,
        x0, y1, z_mm, // TL
        x1, y1, z_mm, // TR
        x0, y0, z_mm, // BL
    )
}

/// Set display area from explicit corners (each tracker-relative, mm).
#[allow(clippy::too_many_arguments)]
pub fn build_set_display_area_corners(
    seq: u32,
    tl_x: f64, tl_y: f64, tl_z: f64,
    tr_x: f64, tr_y: f64, tr_z: f64,
    bl_x: f64, bl_y: f64, bl_z: f64,
) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_point(&mut pay, tl_x, tl_y, tl_z);
    write_point(&mut pay, tr_x, tr_y, tr_z);
    write_point(&mut pay, bl_x, bl_y, bl_z);
    write_tag(&mut pay, 0x10100);
    write_u32(&mut pay, 0x3039);
    build_out_frame(seq, OP_SET_DISPLAY_AREA, &pay.into_vec())
}

```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib commands::tests`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/commands.rs
git commit -m "feat(protocol): add hello/subscribe/display-area command builders"
```

---

### Task 8: MD5 + HMAC-MD5

**Files:**
- Create: `crates/tobii-protocol/src/md5.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod md5;`)
- Test: inline in `md5.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod md5;
```

- [ ] **Step 2: Write the failing tests (RFC 2202 vectors)**

Create `crates/tobii-protocol/src/md5.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn hmac_md5_rfc2202_vectors() {
        // Vector 1: key = 0x0b × 16, data = "Hi There".
        let key1 = [0x0bu8; 16];
        assert_eq!(hex(&hmac_md5(&key1, b"Hi There")), "9294727a3638bb1c13f48ef8158bfc9d");
        // Vector 2: key = "Jefe", data = "what do ya want for nothing?".
        assert_eq!(
            hex(&hmac_md5(b"Jefe", b"what do ya want for nothing?")),
            "750c783e6ab0b503eaa86e310a5db738"
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib md5::tests`
Expected: FAIL — `cannot find function md5`.

- [ ] **Step 4: Write the implementation**

Prepend to `md5.rs`:

```rust
//! Minimal MD5 + HMAC-MD5 for ET5 realm authentication. No dependencies.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

struct Md5State {
    state: [u32; 4],
    count: u64,
    buf: [u8; 64],
    buf_len: usize,
}

impl Md5State {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            count: 0,
            buf: [0u8; 64],
            buf_len: 0,
        }
    }

    fn transform(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (j, slot) in m.iter_mut().enumerate() {
            *slot = u32::from_le_bytes([
                block[j * 4],
                block[j * 4 + 1],
                block[j * 4 + 2],
                block[j * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) =
            (self.state[0], self.state[1], self.state[2], self.state[3]);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let tmp = d;
            d = c;
            c = b;
            let x = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(x.rotate_left(S[i]));
            a = tmp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }

    fn update(&mut self, mut data: &[u8]) {
        self.count = self.count.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.transform(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.transform(&block);
            data = &data[64..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.buf_len = data.len();
    }

    fn finalize(mut self) -> [u8; 16] {
        let bit_len = self.count.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for i in self.buf_len..64 {
                self.buf[i] = 0;
            }
            let block = self.buf;
            self.transform(&block);
            self.buf_len = 0;
        }
        for i in self.buf_len..56 {
            self.buf[i] = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_le_bytes());
        let block = self.buf;
        self.transform(&block);
        let mut out = [0u8; 16];
        for i in 0..4 {
            out[i * 4..i * 4 + 4].copy_from_slice(&self.state[i].to_le_bytes());
        }
        out
    }
}

/// MD5 digest of `data`.
pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut h = Md5State::new();
    h.update(data);
    h.finalize()
}

/// HMAC-MD5 of `msg` under `key`.
pub fn hmac_md5(key: &[u8], msg: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let hashed = md5(key);
        k[..16].copy_from_slice(&hashed);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = k[i] ^ 0x36;
        opad[i] = k[i] ^ 0x5c;
    }
    let mut inner = Md5State::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner_digest = inner.finalize();

    let mut outer = Md5State::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    outer.finalize()
}

```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib md5::tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/md5.rs
git commit -m "feat(protocol): add MD5 and HMAC-MD5"
```

---

### Task 9: Realm command builders

**Files:**
- Create: `crates/tobii-protocol/src/realm.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod realm;`)
- Test: inline in `realm.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod realm;
```

- [ ] **Step 2: Write the failing tests (ported golden vectors)**

Create `crates/tobii-protocol/src/realm.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_realm_frame() {
        let f = build_query_realm(5);
        assert_eq!(f.len(), 34); // 8 + 24 + 2
        assert_eq!(&f[20..24], &[0, 0, 0x06, 0x40]);
    }

    #[test]
    fn open_realm_frame() {
        let f = build_open_realm(5, 1);
        assert_eq!(f.len(), 44); // 8 + 24 + (2 + 9 + 1)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x6c]);
        assert_eq!(f[34], 0x02); // TLV type=2
        assert_eq!(f[42], 0x01); // realm_type LSB
        assert_eq!(f[43], 0x00); // choice
    }

    #[test]
    fn realm_response_frame() {
        let digest = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let f = build_realm_response(5, 42, 7, &digest);
        assert_eq!(f.len(), 68); // 8 + 24 + (2 + 9 + 9 + 16)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x76]);
        // digest at payload offset 20 → frame offset 52
        assert_eq!(f[52], 1);
        assert_eq!(f[67], 16);
    }

    #[test]
    fn close_realm_frame() {
        let f = build_close_realm(5, 42);
        assert_eq!(f.len(), 43); // 8 + 24 + (2 + 9)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x7b]);
    }

    #[test]
    fn realm_key_is_seventeen_bytes() {
        // "IS2LJC6GIRBBEK2K" + trailing NUL.
        assert_eq!(REALM_KEY.len(), 17);
        assert_eq!(REALM_KEY[16], 0x00);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib realm::tests`
Expected: FAIL — `cannot find function build_query_realm`.

- [ ] **Step 4: Write the implementation**

Prepend to `realm.rs`:

```rust
//! Realm (authentication) command builders. The HMAC-MD5 of the device
//! challenge is computed with [`crate::md5::hmac_md5`] and the [`REALM_KEY`].

use crate::bytes::Writer;
use crate::frame::{build_out_frame, OP_CLOSE_REALM, OP_OPEN_REALM, OP_QUERY_REALM, OP_REALM_RESPONSE};
use crate::tlv::write_u32;

/// The realm HMAC key (16 ASCII chars + trailing NUL), as used on the wire.
pub const REALM_KEY: &[u8; 17] = b"IS2LJC6GIRBBEK2K\x00";

pub fn build_query_realm(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_QUERY_REALM, &[0x00, 0x00])
}

pub fn build_open_realm(seq: u32, realm_type: u32) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_type);
    pay.push_u8(0x00); // 1-byte choice = 0 (raw, no TLV header)
    build_out_frame(seq, OP_OPEN_REALM, &pay.into_vec())
}

pub fn build_realm_response(seq: u32, realm_id: u32, field_210: u32, digest: &[u8; 16]) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_id);
    write_u32(&mut pay, field_210);
    pay.push_bytes(digest);
    build_out_frame(seq, OP_REALM_RESPONSE, &pay.into_vec())
}

pub fn build_close_realm(seq: u32, realm_id: u32) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_id);
    build_out_frame(seq, OP_CLOSE_REALM, &pay.into_vec())
}

```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib realm::tests`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/realm.rs
git commit -m "feat(protocol): add realm command builders + key"
```

---

### Task 10: Inbound parser (envelope reassembly)

**Files:**
- Create: `crates/tobii-protocol/src/parser.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod parser;`)
- Test: inline in `parser.rs`

This ports `feed_usb_in` from the reference, including the multi-transfer continuation handling. The Rust API returns owned `Frame`s from `feed()` instead of using callbacks.

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod parser;
```

- [ ] **Step 2: Write the failing tests (ported reassembly cases)**

Create `crates/tobii-protocol/src/parser.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};

    /// Build a fake inbound envelope: [01 00 00 00][len_LE:4][ttp_hdr:24][payload].
    fn fake_inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
        let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(&[0x01, 0, 0, 0]);
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(&magic.to_be_bytes());
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes()); // flag
        v.extend_from_slice(&op.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn single_complete_frame() {
        let mut p = Parser::new();
        let buf = fake_inbound(TTP_MAGIC_RSP, 42, 0x3e8, &[0xde, 0xad, 0xbe, 0xef]);
        let frames = p.feed(&buf).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].magic, TTP_MAGIC_RSP);
        assert_eq!(frames[0].seq, 42);
        assert_eq!(frames[0].op, 0x3e8);
        assert_eq!(frames[0].payload, vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn two_frames_concatenated() {
        let mut p = Parser::new();
        let mut buf = fake_inbound(TTP_MAGIC_RSP, 1, 0x100, &[0x11]);
        buf.extend(fake_inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &[0x22, 0x23]));
        let frames = p.feed(&buf).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].op, 0x500);
        assert_eq!(frames[1].payload, vec![0x22, 0x23]);
    }

    #[test]
    fn frame_split_across_two_chunks() {
        let mut p = Parser::new();
        let buf = fake_inbound(TTP_MAGIC_RSP, 7, 0x200, &[0xa1, 0xa2, 0xa3, 0xa4]);
        let frames = p.feed(&buf[..20]).unwrap();
        assert_eq!(frames.len(), 0);
        assert_eq!(p.buffered(), 20);
        let frames = p.feed(&buf[20..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 7);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn rejects_bad_direction() {
        let mut p = Parser::new();
        let buf = [0x02u8, 0, 0, 0, 0x20, 0, 0, 0];
        assert_eq!(p.feed(&buf), Err(crate::error::ProtocolError::BadDirection(0x02)));
        assert_eq!(p.buffered(), 0); // reset after error
    }

    #[test]
    fn rejects_impossibly_small_length() {
        let mut p = Parser::new();
        let buf = [0x01u8, 0, 0, 0, 10, 0, 0, 0]; // len=10 < 8+24
        assert_eq!(p.feed(&buf), Err(crate::error::ProtocolError::BadLength(10)));
    }

    #[test]
    fn fragmented_multi_envelope_response() {
        // 200-byte payload split: chunk1 = env+hdr+11, chunk2 = env+92,
        // chunk3 = raw 97 (no envelope).
        let mut p = Parser::new();
        let full: Vec<u8> = (0..200u32).map(|i| i as u8).collect();

        // chunk1: envelope + ttp header (plen=200) + first 11 payload bytes
        let mut c1 = Vec::new();
        c1.extend_from_slice(&[0x01, 0, 0, 0]);
        c1.extend_from_slice(&43u32.to_le_bytes());
        c1.extend_from_slice(&TTP_MAGIC_RSP.to_be_bytes());
        c1.extend_from_slice(&99u32.to_be_bytes());
        c1.extend_from_slice(&0u32.to_be_bytes());
        c1.extend_from_slice(&0x44Cu32.to_be_bytes());
        c1.extend_from_slice(&0u32.to_be_bytes());
        c1.extend_from_slice(&200u32.to_be_bytes());
        c1.extend_from_slice(&full[..11]);
        assert_eq!(p.feed(&c1).unwrap().len(), 0);

        // chunk2: continuation envelope + 92 payload bytes
        let mut c2 = Vec::new();
        c2.extend_from_slice(&[0x01, 0, 0, 0]);
        c2.extend_from_slice(&100u32.to_le_bytes());
        c2.extend_from_slice(&full[11..103]);
        assert_eq!(p.feed(&c2).unwrap().len(), 0);

        // chunk3: raw continuation, no envelope, 97 bytes
        let frames = p.feed(&full[103..200]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 99);
        assert_eq!(frames[0].op, 0x44C);
        assert_eq!(frames[0].payload.len(), 200);
        assert_eq!(frames[0].payload[0], 0);
        assert_eq!(p.buffered(), 0);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tobii-protocol --lib parser::tests`
Expected: FAIL — `cannot find type Parser`.

- [ ] **Step 4: Write the implementation**

Prepend to `parser.rs`:

```rust
//! Inbound USB byte-stream reassembler.
//!
//! Device → host bytes arrive as length-prefixed envelopes:
//!   [dir=0x01][0 0 0][len_LE:u32][ttp_header:24][payload]
//! The IN envelope length field INCLUDES the 8-byte envelope (asymmetric vs OUT).
//! Large TTP responses are split across multiple USB transfers; continuation
//! transfers carry their own 8-byte envelope header wrapping raw payload bytes,
//! which we strip so the accumulator holds a clean [env][ttp_hdr][payload].

use crate::error::ProtocolError;
use crate::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE};

const ACC_CAP: usize = 1 << 21; // 2 MiB

/// A fully reassembled TTP frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub magic: u32,
    pub seq: u32,
    pub op: u32,
    pub payload: Vec<u8>,
}

/// Accumulates inbound USB chunks and yields complete frames.
#[derive(Debug, Default)]
pub struct Parser {
    acc: Vec<u8>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Parser {
    pub fn new() -> Self {
        Self { acc: Vec::new() }
    }

    /// Bytes currently buffered (incomplete frame in progress).
    pub fn buffered(&self) -> usize {
        self.acc.len()
    }

    /// Reset the accumulator (e.g. after reconnect).
    pub fn reset(&mut self) {
        self.acc.clear();
    }

    /// Feed a USB chunk; returns any complete frames it produced.
    /// On a framing error the accumulator is reset and the error returned.
    pub fn feed(&mut self, src: &[u8]) -> Result<Vec<Frame>, ProtocolError> {
        // If we have a header and the frame is still incomplete, this chunk is
        // a continuation — strip its 8-byte IN envelope header if present.
        let mut data = src;
        if self.acc.len() >= ENVELOPE_SIZE + TTP_HDR_SIZE {
            let plen = be32(&self.acc[ENVELOPE_SIZE + 20..]);
            let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen as usize;
            if self.acc.len() < frame_size
                && src.len() >= ENVELOPE_SIZE
                && src[0] == 0x01
                && src[1] == 0x00
                && src[2] == 0x00
                && src[3] == 0x00
            {
                data = &src[ENVELOPE_SIZE..];
            }
        }

        if self.acc.len() + data.len() > ACC_CAP {
            self.acc.clear();
            return Err(ProtocolError::Overflow);
        }
        self.acc.extend_from_slice(data);

        let mut frames = Vec::new();
        loop {
            match self.drain_one() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break, // need more bytes
                Err(e) => {
                    self.acc.clear();
                    return Err(e);
                }
            }
        }
        Ok(frames)
    }

    /// Try to drain one frame from the head of the accumulator.
    /// Ok(Some) = frame produced (and removed); Ok(None) = need more bytes.
    fn drain_one(&mut self) -> Result<Option<Frame>, ProtocolError> {
        if self.acc.len() < ENVELOPE_SIZE {
            return Ok(None);
        }
        if self.acc[0] != 0x01 {
            return Err(ProtocolError::BadDirection(self.acc[0]));
        }
        let env_len = le32(&self.acc[4..]);
        if (env_len as usize) < ENVELOPE_SIZE + TTP_HDR_SIZE {
            return Err(ProtocolError::BadLength(env_len));
        }
        if self.acc.len() < ENVELOPE_SIZE + TTP_HDR_SIZE {
            return Ok(None);
        }
        let hdr = &self.acc[ENVELOPE_SIZE..ENVELOPE_SIZE + TTP_HDR_SIZE];
        let magic = be32(&hdr[0..]);
        let seq = be32(&hdr[4..]);
        let op = be32(&hdr[12..]);
        let plen = be32(&hdr[20..]) as usize;
        let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen;
        if frame_size > ACC_CAP {
            return Err(ProtocolError::BadLength(plen as u32));
        }
        if self.acc.len() < frame_size {
            return Ok(None);
        }
        let payload = self.acc[ENVELOPE_SIZE + TTP_HDR_SIZE..frame_size].to_vec();
        // Remove the consumed frame from the head.
        self.acc.drain(..frame_size);
        Ok(Some(Frame { magic, seq, op, payload }))
    }
}

```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tobii-protocol --lib parser::tests`
Expected: PASS (6 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/parser.rs
git commit -m "feat(protocol): add inbound reassembly Parser"
```

---

### Task 11: Gaze sample decode

**Files:**
- Create: `crates/tobii-protocol/src/gaze.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod gaze;`)
- Test: inline in `gaze.rs`

This ports `decodeGazeSample` and `columnKind` from the reference. The `GazeSample` mirrors the documented columns; a `present` bitset records which fields were populated.

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod gaze;
```

- [ ] **Step 2: Write the failing test (synthetic payload built from encoders)**

Create `crates/tobii-protocol/src/gaze.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Writer;
    use crate::tlv::{write_f64_q42, write_tag, write_u32, TAG_POINT2D, TAG_XDS_COLUMN};

    /// Build a minimal 0x500 gaze payload with: 2-byte prefix, xds_row(count=3),
    /// column 0x01 (timestamp s64), column 0x07 (validity_L u32=0),
    /// column 0x1c (gaze_point_2d = (0.25, 0.75)).
    fn synthetic_payload() -> Vec<u8> {
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00); // 2-byte prefix
        // xds_row: tag = (3 << 16) | 0x0bb8
        write_tag(&mut w, (3u32 << 16) | 0x0bb8);

        // column 0x01: timestamp (s64, type=6)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x01);
        w.push_u8(6);
        w.push_be32(8);
        w.push_be64(123456i64 as u64);

        // column 0x07: validity_L (u32)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x07);
        write_u32(&mut w, 0);

        // column 0x1c: gaze_point_2d (point2d = 2 × Q42)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x1c);
        write_tag(&mut w, TAG_POINT2D);
        write_f64_q42(&mut w, 0.25);
        write_f64_q42(&mut w, 0.75);

        w.into_vec()
    }

    #[test]
    fn decodes_known_columns() {
        let payload = synthetic_payload();
        let s = GazeSample::decode(&payload).expect("decode");
        assert!(s.has(present::TIMESTAMP));
        assert_eq!(s.timestamp_us, 123456);
        assert!(s.has(present::VALIDITY_L));
        assert_eq!(s.validity_l, 0);
        assert!(s.has(present::GAZE_2D));
        assert!((s.gaze_point_2d[0] - 0.25).abs() < 1e-9);
        assert!((s.gaze_point_2d[1] - 0.75).abs() < 1e-9);
        // Fields not present stay at default and report absent.
        assert!(!s.has(present::PUPIL_L));
    }

    #[test]
    fn rejects_too_short() {
        assert!(GazeSample::decode(&[0x00]).is_none());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p tobii-protocol --lib gaze::tests`
Expected: FAIL — `cannot find type GazeSample`.

- [ ] **Step 4: Write the implementation**

Prepend to `gaze.rs`:

```rust
//! Gaze sample decoding (0x500 notification payloads).
//!
//! Coordinate spaces (per the reference): tracker-space (mm, origin at the IR
//! sensor array), display-space (tracker-space shifted by the display-area
//! offset), and normalized 2D ([0,1]² ray→plane intersection on display-area).

use crate::tlv::Reader;

/// `present` bitmask flags for the populated fields of a [`GazeSample`].
pub mod present {
    pub const TIMESTAMP: u32 = 1 << 0;
    pub const FRAME_COUNTER: u32 = 1 << 1;
    pub const VALIDITY_L: u32 = 1 << 2;
    pub const VALIDITY_R: u32 = 1 << 3;
    pub const PUPIL_L: u32 = 1 << 4;
    pub const PUPIL_R: u32 = 1 << 5;
    pub const GAZE_2D: u32 = 1 << 6;
    pub const GAZE_2D_L: u32 = 1 << 7;
    pub const GAZE_2D_R: u32 = 1 << 8;
    pub const EYE_ORIGIN_L: u32 = 1 << 9;
    pub const EYE_ORIGIN_R: u32 = 1 << 10;
    pub const GAZE_2D_UNFILTERED: u32 = 1 << 11;
    pub const EYE_ORIGIN_RAW_L: u32 = 1 << 12;
    pub const EYE_ORIGIN_RAW_R: u32 = 1 << 13;
    pub const GAZE_3D_L: u32 = 1 << 14;
    pub const GAZE_3D_R: u32 = 1 << 15;
}

/// A decoded gaze frame. Fields are only meaningful when their `present` bit
/// is set (check via [`GazeSample::has`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GazeSample {
    pub present_mask: u32,
    pub frame_counter: u32,
    pub validity_l: u32, // 0 = valid, 4 = not detected
    pub validity_r: u32,
    pub timestamp_us: i64,
    pub pupil_l_mm: f64,
    pub pupil_r_mm: f64,
    pub gaze_point_2d: [f64; 2],            // combined, temporally filtered (final)
    pub gaze_point_2d_unfiltered: [f64; 2], // combined, pre-smoothing
    pub gaze_point_2d_l: [f64; 2],
    pub gaze_point_2d_r: [f64; 2],
    pub eye_origin_l_mm: [f64; 3],     // calibrated (tracker-space)
    pub eye_origin_r_mm: [f64; 3],
    pub eye_origin_raw_l_mm: [f64; 3], // pre-calibration (tracker-space)
    pub eye_origin_raw_r_mm: [f64; 3],
    pub gaze_point_3d_l_mm: [f64; 3],  // ray–plane hit (tracker-space)
    pub gaze_point_3d_r_mm: [f64; 3],
}

/// TLV data kind for an unknown column, used to skip it by reading its width.
enum Kind {
    S64,
    U32,
    Fixed16x16,
    Point2d,
    Point3d,
}

/// Map a column id to its TLV kind (mirrors the reference `columnKind`).
fn column_kind(col: u32) -> Option<Kind> {
    match col {
        0x01 => Some(Kind::S64),
        0x02 | 0x03 | 0x04 | 0x08 | 0x09 | 0x0a | 0x17 | 0x18 | 0x22 | 0x24 | 0x25 | 0x27 => {
            Some(Kind::Point3d)
        }
        0x05 | 0x0b | 0x1c | 0x20 | 0x19 | 0x1a => Some(Kind::Point2d),
        0x06 | 0x0c | 0x29 | 0x2b => Some(Kind::Fixed16x16),
        0x07 | 0x0d | 0x0e | 0x11 | 0x14 | 0x15 | 0x16 | 0x1b | 0x1d | 0x1e | 0x1f | 0x21 | 0x23
        | 0x26 | 0x28 | 0x2a | 0x2c => Some(Kind::U32),
        _ => None,
    }
}

impl GazeSample {
    /// True if `flag` (a [`present`] constant) was populated.
    pub fn has(&self, flag: u32) -> bool {
        self.present_mask & flag != 0
    }

    /// Decode a 0x500 notification payload. Returns `None` if the payload is
    /// too short, the row header is unreadable, or a modeled column is
    /// truncated mid-value (the whole frame is dropped — see the worker note
    /// below for the partial-tolerance variant a later phase may want).
    pub fn decode(payload: &[u8]) -> Option<GazeSample> {
        if payload.len() < 2 {
            return None;
        }
        let mut r = Reader::new(payload);
        r.pos = 2; // skip 2-byte prefix
        let n_cols = r.read_xds_row().ok()?;

        let mut s = GazeSample::default();
        let mut i = 0;
        while i < n_cols && r.remaining() > 0 {
            i += 1;
            let col = match r.read_xds_column() {
                Ok(c) => c,
                Err(_) => return Some(s),
            };
            match col {
                0x01 => match r.read_s64() {
                    Ok(v) => {
                        s.timestamp_us = v;
                        s.present_mask |= present::TIMESTAMP;
                    }
                    Err(_) => return Some(s),
                },
                0x02 => set3(&mut r, &mut s.eye_origin_l_mm, &mut s.present_mask, present::EYE_ORIGIN_L)?,
                0x08 => set3(&mut r, &mut s.eye_origin_r_mm, &mut s.present_mask, present::EYE_ORIGIN_R)?,
                0x04 => set3(&mut r, &mut s.gaze_point_3d_l_mm, &mut s.present_mask, present::GAZE_3D_L)?,
                0x0a => set3(&mut r, &mut s.gaze_point_3d_r_mm, &mut s.present_mask, present::GAZE_3D_R)?,
                0x17 => set3(&mut r, &mut s.eye_origin_raw_l_mm, &mut s.present_mask, present::EYE_ORIGIN_RAW_L)?,
                0x18 => set3(&mut r, &mut s.eye_origin_raw_r_mm, &mut s.present_mask, present::EYE_ORIGIN_RAW_R)?,
                0x05 => set2(&mut r, &mut s.gaze_point_2d_l, &mut s.present_mask, present::GAZE_2D_L)?,
                0x0b => set2(&mut r, &mut s.gaze_point_2d_r, &mut s.present_mask, present::GAZE_2D_R)?,
                0x1c => set2(&mut r, &mut s.gaze_point_2d, &mut s.present_mask, present::GAZE_2D)?,
                0x20 => set2(&mut r, &mut s.gaze_point_2d_unfiltered, &mut s.present_mask, present::GAZE_2D_UNFILTERED)?,
                0x06 => match r.read_fixed16x16() {
                    Ok(v) => {
                        s.pupil_l_mm = v;
                        s.present_mask |= present::PUPIL_L;
                    }
                    Err(_) => return Some(s),
                },
                0x0c => match r.read_fixed16x16() {
                    Ok(v) => {
                        s.pupil_r_mm = v;
                        s.present_mask |= present::PUPIL_R;
                    }
                    Err(_) => return Some(s),
                },
                0x07 => match r.read_u32() {
                    Ok(v) => {
                        s.validity_l = v;
                        s.present_mask |= present::VALIDITY_L;
                    }
                    Err(_) => return Some(s),
                },
                0x0d => match r.read_u32() {
                    Ok(v) => {
                        s.validity_r = v;
                        s.present_mask |= present::VALIDITY_R;
                    }
                    Err(_) => return Some(s),
                },
                0x14 => match r.read_u32() {
                    Ok(v) => {
                        s.frame_counter = v;
                        s.present_mask |= present::FRAME_COUNTER;
                    }
                    Err(_) => return Some(s),
                },
                // Known-but-unmodeled column: skip by its declared width.
                other => {
                    match column_kind(other) {
                        Some(Kind::S64) => {
                            if r.read_s64().is_err() {
                                return Some(s);
                            }
                        }
                        Some(Kind::U32) => {
                            if r.read_u32().is_err() {
                                return Some(s);
                            }
                        }
                        Some(Kind::Fixed16x16) => {
                            if r.read_fixed16x16().is_err() {
                                return Some(s);
                            }
                        }
                        Some(Kind::Point2d) => {
                            if r.read_point2d().is_err() {
                                return Some(s);
                            }
                        }
                        Some(Kind::Point3d) => {
                            if r.read_point3d().is_err() {
                                return Some(s);
                            }
                        }
                        None => return Some(s), // unknown column — stop
                    }
                }
            }
        }
        Some(s)
    }
}

fn set3(
    r: &mut Reader,
    dst: &mut [f64; 3],
    mask: &mut u32,
    flag: u32,
) -> Option<()> {
    match r.read_point3d() {
        Ok(v) => {
            *dst = v;
            *mask |= flag;
            Some(())
        }
        // Truncated point: propagate None via `?` so decode() drops the frame.
        Err(_) => None,
    }
}

fn set2(
    r: &mut Reader,
    dst: &mut [f64; 2],
    mask: &mut u32,
    flag: u32,
) -> Option<()> {
    match r.read_point2d() {
        Ok(v) => {
            *dst = v;
            *mask |= flag;
            Some(())
        }
        Err(_) => None,
    }
}

```

> **Implementation note for the worker:** the `?` on `set2`/`set3` makes `decode` return `None` on a truncated point. That differs slightly from the reference (which returns the partial sample). For v1 this is acceptable and simpler — a truncated gaze frame is dropped rather than partially reported. The test above uses complete fields, so it passes. If a later phase needs partial-frame tolerance, change `set2`/`set3` to return `Option<bool>` (false = stop) and `break` the loop instead of `?`. Do **not** leave this note in shipped code — delete it once implemented.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p tobii-protocol --lib gaze::tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/gaze.rs
git commit -m "feat(protocol): add gaze sample decode"
```

---

### Task 12: Display-area response decode

**Files:**
- Create: `crates/tobii-protocol/src/display.rs`
- Modify: `crates/tobii-protocol/src/lib.rs` (add `pub mod display;`)
- Test: inline in `display.rs`

- [ ] **Step 1: Add the module declaration**

In `lib.rs` add:

```rust
pub mod display;
```

- [ ] **Step 2: Write the failing test**

Create `crates/tobii-protocol/src/display.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Writer;
    use crate::tlv::write_point;

    #[test]
    fn decodes_three_corners() {
        // Build a get_display_area-style payload: 2-byte prefix + TL + TR + BL.
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_point(&mut w, -200.0, 150.0, 0.0); // TL
        write_point(&mut w, 200.0, 150.0, 0.0); // TR
        write_point(&mut w, -200.0, -150.0, 0.0); // BL
        let buf = w.into_vec();

        let c = DisplayCorners::decode(&buf).expect("decode");
        assert!((c.tl[0] + 200.0).abs() < 1e-9);
        assert!((c.tr[0] - 200.0).abs() < 1e-9);
        assert!((c.bl[1] + 150.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_short_payload() {
        assert!(DisplayCorners::decode(&[0x00, 0x00]).is_none());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p tobii-protocol --lib display::tests`
Expected: FAIL — `cannot find type DisplayCorners`.

- [ ] **Step 4: Write the implementation**

Prepend to `display.rs`:

```rust
//! Display-area decoding. The get_display_area response payload matches the
//! set wire format: [00 00][point TL][point TR][point BL][...].

use crate::tlv::Reader;

/// The three display-area corners reported by the device (tracker-space mm).
/// The bottom-right corner is implied (not sent on the wire).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DisplayCorners {
    pub tl: [f64; 3],
    pub tr: [f64; 3],
    pub bl: [f64; 3],
}

impl DisplayCorners {
    /// Decode a display-area payload. Returns `None` if it does not parse.
    pub fn decode(payload: &[u8]) -> Option<DisplayCorners> {
        if payload.len() < 2 {
            return None;
        }
        let mut r = Reader::new(payload);
        r.pos = 2;
        let tl = r.read_point3d().ok()?;
        let tr = r.read_point3d().ok()?;
        let bl = r.read_point3d().ok()?;
        Some(DisplayCorners { tl, tr, bl })
    }
}

```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p tobii-protocol --lib display::tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/src/display.rs
git commit -m "feat(protocol): add display-area decode"
```

---

### Task 13: Crate polish — re-exports, clippy, fmt, README

**Files:**
- Modify: `crates/tobii-protocol/src/lib.rs`
- Create: `crates/tobii-protocol/README.md`

- [ ] **Step 1: Add convenience re-exports to `lib.rs`**

Ensure the top of `lib.rs` reads (module list then re-exports):

```rust
//! Pure codec for the Tobii Eye Tracker 5 USB wire protocol.
//!
//! No I/O, no global state, no external dependencies. Outbound builders
//! return `Vec<u8>`; the inbound [`parser::Parser`] yields complete frames;
//! decoders produce typed [`gaze::GazeSample`] / [`display::DisplayCorners`].
//!
//! Protocol decoded by the `tobiifree` project (GPL-3.0) from USB captures.

pub mod bytes;
pub mod commands;
pub mod display;
pub mod error;
pub mod frame;
pub mod gaze;
pub mod md5;
pub mod parser;
pub mod realm;
pub mod tlv;

pub use display::DisplayCorners;
pub use error::ProtocolError;
pub use gaze::GazeSample;
pub use parser::{Frame, Parser};
```

- [ ] **Step 2: Run the full test suite**

Run: `cargo test -p tobii-protocol`
Expected: PASS — all tests across all modules (roughly 25+), 0 failures.

- [ ] **Step 3: Run clippy and fmt**

Run: `cargo clippy -p tobii-protocol -- -D warnings && cargo fmt -p tobii-protocol`
Expected: clippy reports no warnings; fmt makes no further changes (or only whitespace). If clippy flags anything, fix it and re-run.

- [ ] **Step 4: Write the crate README**

Create `crates/tobii-protocol/README.md`:

```markdown
# tobii-protocol

Pure, dependency-free Rust codec for the Tobii Eye Tracker 5 USB wire protocol
(TTP framing + TLV/Q42), reverse-engineered with the GPL-3.0 `tobiifree` project
as the protocol reference.

This crate does **no I/O**. It builds outbound command frames, reassembles the
inbound USB byte stream into frames, and decodes gaze + display-area payloads.
The USB transport and handshake state machine live in `tobii-usb` (separate
crate).

## Modules
- `frame` — TTP framing + outbound USB envelope, op/magic constants.
- `tlv` — TLV encoders + `Reader` decoders, Q42 fixed-point.
- `commands` — hello, subscribe, get/set display area.
- `realm` — query/open/response/close realm + the realm key.
- `md5` — MD5 + HMAC-MD5 for realm auth.
- `parser` — inbound `Parser` → `Frame`s (handles multi-transfer reassembly).
- `gaze` — `GazeSample::decode` for 0x500 notifications.
- `display` — `DisplayCorners::decode`.

License: GPL-3.0-only.
```

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-protocol/src/lib.rs crates/tobii-protocol/README.md
git commit -m "chore(protocol): re-exports, clippy clean, README"
```

---

## Roadmap (after this plan)

This plan delivers the tested codec. The remaining v1 plans, in order:

1. **`tobii-usb` + handshake** — `rusb` transport (open `2104:0313`, claim
   interface, bulk IN/OUT), port the handshake state machine (hello → realm
   HMAC-MD5 → display area → subscribe), and a `tobii stream` binary printing
   live gaze. **First plan that needs the ET5 plugged in.** Includes Spike R2
   (does the device stream after only the handshake?).
2. **`tobii-config`** — Spike S3 (decompile `Tobii.Configuration`/`TetConfig`
   from the MSI), reimplement the display-setup math, `tobii setup` + TOML.
3. **`tobii-headpose` + opentrack output** — Spike S1 (pitch axis) and Spike S2
   (opentrack UDP packet format), derive 6DOF, `tobii opentrack`, live SC test.

## Plan self-review notes

- **Spec coverage (this crate's slice):** TTP framing ✓ (Task 6), TLV/Q42 ✓
  (Tasks 4–5), command frames ✓ (Task 7), realm HMAC-MD5 ✓ (Tasks 8–9), inbound
  reassembly ✓ (Task 10), gaze decode ✓ (Task 11), display-area decode ✓ (Task
  12). Handshake state machine and calibration are intentionally out of scope
  (Plan 2 / Phase 2) per the spec's phasing.
- **Type consistency:** `Writer` methods (`push_*`, `into_vec`, `len`) are used
  identically in Tasks 4, 6, 7, 9. `Reader` methods (Task 5) are consumed in
  Tasks 11–12. `Frame`/`Parser` (Task 10) names match the re-exports (Task 13).
- **No hardware:** every test is a golden vector or a synthetic payload built
  from this crate's own encoders — runnable on the dev machine with no ET5.

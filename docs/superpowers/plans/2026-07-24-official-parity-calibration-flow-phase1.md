# Official-Parity Calibration Flow — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Linux gaze accurate to the screen edges (send the correct display plane) and make the GUI calibration-state-aware — forcing calibration when none exists and recommending it when the stored calibration doesn't match the active screen.

**Architecture:** All decision logic and geometry live in `tobii-config` as pure, unit-tested functions; `tobii-gtk` gets thin wiring. The fix's core is: the device plane must use the EDID **arc** width (not the chord), validated against Tobii's own captured corner triple (`screenplane.setpm`), and calibrations get bound to the monitor they were made for via an EDID-derived id.

**Tech Stack:** Rust (workspace crates `tobii-config`, `tobii-gtk`, `tobii-cli`), GTK4 (GUI, not CI-tested — manual verification), inline `#[cfg(test)]` unit tests, `include_bytes!` fixtures.

Design spec: `docs/superpowers/specs/2026-07-24-official-parity-calibration-flow-design.md`.

## Global Constraints

- GPL-3.0-only; clean-room. Never ship or commit Tobii's proprietary model assets. (Display-geometry corner data captured from the user's own monitor is the user's data and is fine as a test fixture.)
- Preserve verbatim calibration-blob replay: the raw `calibration.bin` bytes must remain untouched (the ET5 re-applies them on every connect). Metadata goes in a **separate sidecar** file.
- Follow existing repo conventions: inline `#[cfg(test)] mod tests`; fixtures under `crates/<crate>/src/testdata/` loaded via `include_bytes!`.
- `clippy --workspace --all-targets` and `fmt` must stay clean.
- Tracker-space convention (do not change): right-handed, +X right, +Y up, +Z backward; tilt = lean-back angle (`crates/tobii-config/src/setup.rs:3-6`).
- Do **not** reintroduce any runtime gaze/curvature correction. The device gets a flat plane; only its *width* is being corrected.
- Flat panels are unaffected: arc == chord when `curvature_radius_mm == 0`.

---

### Task 1: `.setpm` decoder + Tobii-corner golden fixture

**Files:**
- Create: `crates/tobii-config/src/setpm.rs`
- Create (binary fixture): `crates/tobii-config/src/testdata/screenplane.setpm`
- Modify: `crates/tobii-config/src/lib.rs` (add `mod setpm;` + re-export)

**Interfaces:**
- Consumes: `tobii_protocol::DisplayCorners { tl:[f64;3], tr:[f64;3], bl:[f64;3] }`.
- Produces: `pub fn parse_setpm_corners(bytes: &[u8]) -> Option<tobii_protocol::DisplayCorners>`.

- [ ] **Step 1: Add the fixture file**

Copy the 48-byte capture into the crate's testdata dir. From the repo root:

```bash
cp "docs/TobiiSetupProcess/calibration/screenplane.setpm" \
   "crates/tobii-config/src/testdata/screenplane.setpm"
```

Verify it is exactly these 48 bytes (little-endian header `04 00 00 00 | 01 00 00 00 | 24 00 00 00` then 9× f32):

```bash
python -c "print(open('crates/tobii-config/src/testdata/screenplane.setpm','rb').read().hex())"
# expect: 040000000100000024000000002015c4fc462441bb6646c0002015c423c8a2431d51df420020154423c8a2431d51df42
```

- [ ] **Step 2: Write the failing test**

Create `crates/tobii-config/src/setpm.rs`:

```rust
//! Decoder for Tobii's `.setpm` screen-plane capture (ground-truth display area).
//!
//! Layout (little-endian): u32 version (=4), u32 count (=1), u32 payload_len (=36),
//! then 9× f32 = three tracker-space corners in order BL, TL, TR (millimetres).

use tobii_protocol::DisplayCorners;

#[cfg(test)]
mod tests {
    use super::*;

    const SCREENPLANE: &[u8] = include_bytes!("testdata/screenplane.setpm");

    #[test]
    fn decodes_tobii_screenplane_corners() {
        let c = parse_setpm_corners(SCREENPLANE).expect("valid setpm");
        // Captured ground truth for the Samsung Odyssey G93SC (49" 1800R).
        let approx = |a: f64, b: f64| (a - b).abs() < 0.05;
        assert!(approx(c.bl[0], -596.5) && approx(c.bl[1], 10.27) && approx(c.bl[2], -3.10), "bl={:?}", c.bl);
        assert!(approx(c.tl[0], -596.5) && approx(c.tl[1], 325.56) && approx(c.tl[2], 111.66), "tl={:?}", c.tl);
        assert!(approx(c.tr[0], 596.5) && approx(c.tr[1], 325.56) && approx(c.tr[2], 111.66), "tr={:?}", c.tr);
    }

    #[test]
    fn rejects_short_or_bad_header() {
        assert!(parse_setpm_corners(&[]).is_none());
        assert!(parse_setpm_corners(&[0u8; 20]).is_none()); // header ok-ish but no payload
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p tobii-config setpm 2>&1 | tail -20`
Expected: FAIL — `cannot find function parse_setpm_corners`.

- [ ] **Step 4: Implement the decoder**

Add above the `#[cfg(test)]` block in `setpm.rs`:

```rust
fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn le_f32(b: &[u8], off: usize) -> f64 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as f64
}

/// Parse a `.setpm` screen-plane capture into tracker-space corners.
/// Returns `None` if the header/length is not the expected 3-corner plane.
pub fn parse_setpm_corners(bytes: &[u8]) -> Option<DisplayCorners> {
    if bytes.len() < 12 {
        return None;
    }
    let version = le_u32(bytes, 0);
    let payload_len = le_u32(bytes, 8) as usize;
    // Expect version 4 and a 9× f32 (36-byte) payload following the 12-byte header.
    if version != 4 || payload_len != 36 || bytes.len() < 12 + 36 {
        return None;
    }
    let f = |i: usize| le_f32(bytes, 12 + i * 4);
    Some(DisplayCorners {
        bl: [f(0), f(1), f(2)],
        tl: [f(3), f(4), f(5)],
        tr: [f(6), f(7), f(8)],
    })
}
```

Wire the module in `crates/tobii-config/src/lib.rs` (add near the other `mod`/`pub use` lines, ~`lib.rs:20-26`):

```rust
mod setpm;
pub use setpm::parse_setpm_corners;
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p tobii-config setpm 2>&1 | tail -20`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-config/src/setpm.rs crates/tobii-config/src/lib.rs \
        crates/tobii-config/src/testdata/screenplane.setpm
git commit -m "feat(config): decode Tobii .setpm screen-plane capture (ground-truth corners)"
```

---

### Task 2: Send the arc width to the device (drop the chord conversion)

**Files:**
- Modify: `crates/tobii-config/src/setup.rs` (add `plane_width_from_edid`, `DisplaySetup::fingerprint`; update the width_mm doc and the `odyssey_g93sc_arc_to_chord` test; add a golden test)
- Modify: `crates/tobii-config/src/lib.rs` (export `plane_width_from_edid`)
- Modify: `crates/tobii-gtk/src/setup_flow.rs:446-450` and `:766`
- Modify: `crates/tobii-cli/src/main.rs:789` (route through the helper for DRY)

**Interfaces:**
- Consumes: `parse_setpm_corners` (Task 1), `DisplaySetup::{to_corners,from_corners}`.
- Produces:
  - `pub fn plane_width_from_edid(edid_active_width_mm: f64) -> f64` (returns the arc unchanged).
  - `impl DisplaySetup { pub fn fingerprint(&self) -> u64 }` (stable hash of the geometry; used by Task 5).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/tobii-config/src/setup.rs`:

```rust
#[test]
fn plane_width_is_the_arc_not_the_chord() {
    // Curved panel: the device plane must use the EDID arc width unchanged,
    // NOT the chord — Tobii's own capture uses the arc (see golden test below).
    let arc = 1193.0;
    assert_eq!(super::plane_width_from_edid(arc), arc);
    // ...and that is deliberately different from the old chord conversion.
    assert!((super::plane_width_from_edid(arc) - super::chord_from_arc(arc, 1800.0)).abs() > 20.0);
}

#[test]
fn odyssey_setup_reproduces_tobii_captured_corners() {
    // Seeding the Odyssey G93SC from EDID (arc width) + current defaults must
    // land within a few mm of Tobii's own captured plane.
    let tobii = crate::parse_setpm_corners(include_bytes!("testdata/screenplane.setpm")).unwrap();
    let s = DisplaySetup {
        width_mm: super::plane_width_from_edid(1193.0),
        height_mm: 335.53,
        tilt_deg: 20.0,
        offset_x_mm: 0.0,
        offset_y_mm: 10.27,
        offset_z_mm: -3.10,
        curvature_radius_mm: 1800.0,
    };
    let c = s.to_corners();
    for (got, exp) in [(c.tl, tobii.tl), (c.tr, tobii.tr), (c.bl, tobii.bl)] {
        for i in 0..3 {
            assert!((got[i] - exp[i]).abs() < 1.0, "corner off: got {got:?} exp {exp:?}");
        }
    }
}

#[test]
fn fingerprint_changes_with_geometry() {
    let a = DisplaySetup { width_mm: 1193.0, ..GOLDEN_SETUP };
    let b = DisplaySetup { width_mm: 1171.0, ..GOLDEN_SETUP };
    assert_ne!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.fingerprint(), a.fingerprint()); // stable
}
```

Add this helper constant to the test module (next to the existing `GOLDEN` corners):

```rust
const GOLDEN_SETUP: DisplaySetup = DisplaySetup {
    width_mm: 1193.0, height_mm: 335.5, tilt_deg: 20.0,
    offset_x_mm: 0.0, offset_y_mm: 10.27, offset_z_mm: -3.10, curvature_radius_mm: 1800.0,
};
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p tobii-config setup 2>&1 | tail -25`
Expected: FAIL — `plane_width_from_edid` / `fingerprint` not found.

- [ ] **Step 3: Implement helper + fingerprint, and re-point the width doc**

In `crates/tobii-config/src/setup.rs`, update the `width_mm` doc comment (`:15-22`) to:

```rust
    /// Active-area width the device projects gaze across, in millimetres.
    ///
    /// This is the EDID **active-area (arc) width** — for a curved panel the flat
    /// plane the device is told about spans the *unrolled* pixel width, NOT the
    /// straight-line chord. Tobii's own captured plane uses the arc; sending the
    /// chord makes the plane too narrow and compresses gaze toward the edges. For
    /// a flat panel arc == chord. See [`plane_width_from_edid`].
    pub width_mm: f64,
```

Add near `chord_from_arc` (after `arc_from_chord`, ~`:66`):

```rust
/// The width to send the device for an EDID-reported active-area width.
///
/// Deliberately the identity: the device plane uses the EDID **arc** width, not
/// the chord (see [`DisplaySetup::width_mm`]). Kept as a named seam so both the
/// GUI and CLI seed the plane identically and a regression back to chord is caught.
pub fn plane_width_from_edid(edid_active_width_mm: f64) -> f64 {
    edid_active_width_mm
}
```

Add to `impl DisplaySetup` (after `from_corners`, ~`:99`):

```rust
    /// Stable hash of the geometry, used to detect that a calibration was
    /// computed against a since-changed display plane.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in [
            self.width_mm, self.height_mm, self.tilt_deg,
            self.offset_x_mm, self.offset_y_mm, self.offset_z_mm,
        ] {
            v.to_bits().hash(&mut h);
        }
        h.finish()
    }
```

Replace the existing `odyssey_g93sc_arc_to_chord` test (`:307-316`) — keep the arc/chord *helper math* assertion but rename and re-purpose it to assert the plane uses the arc:

```rust
    #[test]
    fn curved_panel_helper_math_still_available_but_plane_uses_arc() {
        // The chord helper still computes correctly (used nowhere for the plane now).
        let chord = chord_from_arc(1193.0, 1800.0);
        assert!((chord - 1171.0).abs() < 1.0, "chord={chord}");
        // The device plane, however, uses the arc.
        assert_eq!(super::plane_width_from_edid(1193.0), 1193.0);
    }
```

Export in `crates/tobii-config/src/lib.rs` (alongside `chord_from_arc`, `arc_from_chord`, ~`:20-26`):

```rust
pub use setup::plane_width_from_edid;
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p tobii-config setup 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 5: Re-point GTK and CLI seeding to the arc width**

In `crates/tobii-gtk/src/setup_flow.rs:446-450`, replace the `chord_from_arc` seeding:

```rust
    let edid_arc_mm: Option<f64> = detected.as_ref().map(|m| m.width_mm);
    if let Some(m) = &detected {
        initial.width_mm = tobii_config::plane_width_from_edid(m.width_mm);
        initial.height_mm = m.height_mm;
    }
```

In the `on_curve` hook (`setup_flow.rs:757-772`, the `chord = tobii_config::chord_from_arc(arc, r)` at ~`:766`), stop shrinking the width by curvature — the plane width stays the arc regardless of curve radius:

```rust
    // Curvature no longer changes the plane width (the device plane uses the arc).
    // Keep the radius only as stored metadata.
    let _ = r; // radius retained in the setup struct; not applied to width
    initial.width_mm = tobii_config::plane_width_from_edid(arc);
```

In `crates/tobii-cli/src/main.rs:789` (already arc), route through the helper for DRY/intent:

```rust
        w_def = tobii_config::plane_width_from_edid(m.width_mm);
```

- [ ] **Step 6: Build + lint the GUI/CLI (GTK is not unit-tested)**

Run: `cargo build -p tobii-gtk -p tobii-cli && cargo clippy -p tobii-gtk -p tobii-cli --all-targets 2>&1 | tail -15`
Expected: builds clean, no new clippy warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/tobii-config/src/setup.rs crates/tobii-config/src/lib.rs \
        crates/tobii-gtk/src/setup_flow.rs crates/tobii-cli/src/main.rs
git commit -m "fix(config,gtk,cli): send the EDID arc width as the device plane (was chord); fixes edge gaze error"
```

---

### Task 3: Stable EDID monitor id

**Files:**
- Modify: `crates/tobii-config/src/edid.rs` (add `id`/`connector` to `MonitorInfo`, add `edid_monitor_id`, capture connector in `detect_monitors`)

**Interfaces:**
- Produces:
  - `pub fn edid_monitor_id(edid: &[u8]) -> Option<String>` — e.g. `"SAM7454-HNTY900001"`.
  - `MonitorInfo` gains `pub id: Option<String>` and `pub connector: Option<String>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/tobii-config/src/edid.rs`:

```rust
    #[test]
    fn extracts_stable_monitor_id_from_fixture() {
        let bytes = include_bytes!("testdata/odyssey-g93sc.edid");
        assert_eq!(super::edid_monitor_id(bytes).as_deref(), Some("SAM7454-HNTY900001"));
    }

    #[test]
    fn monitor_id_none_for_garbage() {
        assert!(super::edid_monitor_id(&[0u8; 8]).is_none());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tobii-config edid 2>&1 | tail -20`
Expected: FAIL — `edid_monitor_id` not found.

- [ ] **Step 3: Implement `edid_monitor_id` and extend `MonitorInfo`**

Add to `crates/tobii-config/src/edid.rs`:

```rust
/// Stable id from EDID: PnP manufacturer + product code, plus the serial
/// descriptor (0xFF) when present. E.g. `"SAM7454-HNTY900001"`.
pub fn edid_monitor_id(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    // Manufacturer: bytes 8-9, big-endian, three 5-bit letters (1=A).
    let m = ((edid[8] as u16) << 8) | edid[9] as u16;
    let letter = |shift: u16| (((m >> shift) & 0x1f) as u8 + b'A' - 1) as char;
    let (a, b, c) = (letter(10), letter(5), letter(0));
    if !a.is_ascii_uppercase() || !b.is_ascii_uppercase() || !c.is_ascii_uppercase() {
        return None;
    }
    // Product code: bytes 10-11, little-endian.
    let product = (edid[10] as u16) | ((edid[11] as u16) << 8);
    let mut id = format!("{a}{b}{c}{product:04X}");
    // Serial string descriptor (tag 0xFF) if present.
    for off in [54usize, 72, 90, 108] {
        if edid.get(off..off + 3) == Some(&[0, 0, 0]) && edid.get(off + 3) == Some(&0xFF) {
            let raw = &edid[off + 5..off + 18];
            let s: String = raw.iter().take_while(|&&x| x != 0x0a).map(|&x| x as char).collect();
            let s = s.trim();
            if !s.is_empty() {
                id.push('-');
                id.push_str(s);
            }
            break;
        }
    }
    Some(id)
}
```

Extend `MonitorInfo` (`edid.rs:10-15`):

```rust
pub struct MonitorInfo {
    pub model: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub id: Option<String>,
    pub connector: Option<String>,
}
```

In `detect_monitors` (`edid.rs:76-91`): capture the connector name (the DRM dir file name, currently discarded at `:83`) and set `id`/`connector` when constructing each `MonitorInfo`. For the base-block parse, set `id: edid_monitor_id(&bytes)`. The connector is the `entry.file_name()` of the `/sys/class/drm/*` dir being read.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tobii-config edid 2>&1 | tail -20`
Expected: PASS. Then `cargo build --workspace 2>&1 | tail -15` — fix any `MonitorInfo { .. }` construction sites that now need `id`/`connector` (CLI `main.rs` uses `m.width_mm`/`m.height_mm` by field, so struct literals only exist inside `detect_monitors`; confirm with the build).

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-config/src/edid.rs
git commit -m "feat(config): derive a stable monitor id from EDID (mfg+product+serial)"
```

---

### Task 4: Calibration metadata sidecar (monitor binding)

**Files:**
- Modify: `crates/tobii-config/src/store.rs` (add `CalMeta`, meta save/load; keep `calibration.bin` verbatim)
- Modify: `crates/tobii-config/src/lib.rs` (export `CalMeta`, new save/load signatures)

**Interfaces:**
- Consumes: `DisplaySetup::fingerprint` (Task 2).
- Produces:
  - `pub struct CalMeta { pub monitor_id: Option<String>, pub created_utc: i64, pub mode: String, pub display_fingerprint: u64 }`
  - `pub fn save_calibration(blob: &[u8], meta: &CalMeta) -> io::Result<()>`
  - `pub fn load_calibration() -> io::Result<Option<(Vec<u8>, Option<CalMeta>)>>`
  - `fn calibration_meta_path() -> PathBuf` (private) → `calibration.meta.toml` beside `calibration.bin`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `crates/tobii-config/src/store.rs` (mirror the existing tempdir pattern used by the store tests):

```rust
    #[test]
    fn calibration_meta_round_trips_and_blob_is_verbatim() {
        let dir = tempdir().unwrap();
        let bin = dir.path().join("calibration.bin");
        let meta = dir.path().join("calibration.meta.toml");
        let blob = vec![1u8, 2, 3, 4, 5];
        let m = CalMeta { monitor_id: Some("SAM7454-HNTY900001".into()), created_utc: 42, mode: "full".into(), display_fingerprint: 0xDEAD_BEEF };
        save_calibration_to(&bin, &meta, &blob, &m).unwrap();
        let (got_blob, got_meta) = load_calibration_from(&bin, &meta).unwrap().unwrap();
        assert_eq!(got_blob, blob, "blob must round-trip verbatim");
        let got_meta = got_meta.expect("meta present");
        assert_eq!(got_meta.monitor_id.as_deref(), Some("SAM7454-HNTY900001"));
        assert_eq!(got_meta.display_fingerprint, 0xDEAD_BEEF);
    }

    #[test]
    fn legacy_blob_without_meta_loads_with_none_meta() {
        let dir = tempdir().unwrap();
        let bin = dir.path().join("calibration.bin");
        let meta = dir.path().join("calibration.meta.toml");
        std::fs::write(&bin, [9u8, 9, 9]).unwrap(); // no meta file
        let (blob, m) = load_calibration_from(&bin, &meta).unwrap().unwrap();
        assert_eq!(blob, vec![9, 9, 9]);
        assert!(m.is_none(), "missing meta => None (legacy install)");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tobii-config store 2>&1 | tail -25`
Expected: FAIL — `CalMeta` / `save_calibration_to` new signature not found.

- [ ] **Step 3: Implement `CalMeta` + sidecar save/load**

In `crates/tobii-config/src/store.rs` add:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct CalMeta {
    pub monitor_id: Option<String>,
    pub created_utc: i64,
    pub mode: String,
    pub display_fingerprint: u64,
}

impl CalMeta {
    fn to_toml(&self) -> String {
        format!(
            "# tobii-linux calibration metadata\nmonitor_id = \"{}\"\ncreated_utc = {}\nmode = \"{}\"\ndisplay_fingerprint = {}\n",
            self.monitor_id.as_deref().unwrap_or(""), self.created_utc, self.mode, self.display_fingerprint,
        )
    }
    fn from_toml(s: &str) -> Option<CalMeta> {
        let (mut mid, mut created, mut mode, mut fp) = (None, None, None, None);
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let Some((k, v)) = line.split_once('=') else { continue; };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "monitor_id" => mid = Some(if v.is_empty() { None } else { Some(v.to_string()) }),
                "created_utc" => created = v.parse::<i64>().ok(),
                "mode" => mode = Some(v.to_string()),
                "display_fingerprint" => fp = v.parse::<u64>().ok(),
                _ => {}
            }
        }
        Some(CalMeta { monitor_id: mid?, created_utc: created?, mode: mode?, display_fingerprint: fp? })
    }
}

fn calibration_meta_path() -> PathBuf {
    config_path().with_file_name("calibration.meta.toml")
}

pub fn save_calibration_to(bin: &Path, meta_path: &Path, blob: &[u8], meta: &CalMeta) -> io::Result<()> {
    write_atomic(bin, blob)?;                 // reuse the existing atomic writer used by save_calibration_to today
    write_atomic(meta_path, meta.to_toml().as_bytes())
}

pub fn load_calibration_from(bin: &Path, meta_path: &Path) -> io::Result<Option<(Vec<u8>, Option<CalMeta>)>> {
    let Some(blob) = read_opt(bin)? else { return Ok(None); };   // read_opt = the existing optional-read helper
    let meta = read_opt(meta_path)?.and_then(|b| String::from_utf8(b).ok()).and_then(|s| CalMeta::from_toml(&s));
    Ok(Some((blob, meta)))
}

pub fn save_calibration(blob: &[u8], meta: &CalMeta) -> io::Result<()> {
    save_calibration_to(&calibration_path(), &calibration_meta_path(), blob, meta)
}

pub fn load_calibration() -> io::Result<Option<(Vec<u8>, Option<CalMeta>)>> {
    load_calibration_from(&calibration_path(), &calibration_meta_path())
}
```

Notes for the implementer: the current `save_calibration_to(path,&[u8])` / `load_calibration_from(path)` (`store.rs:62-95`) take a single path — replace their signatures as above (bin + meta). Factor the existing atomic temp-write into `write_atomic(path,&[u8])` and the optional read into `read_opt(path)->io::Result<Option<Vec<u8>>>` if not already separate, so both blob and meta reuse them. Update `lib.rs` re-exports (`CalMeta`, `save_calibration`, `load_calibration`).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tobii-config store 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-config/src/store.rs crates/tobii-config/src/lib.rs
git commit -m "feat(config): bind calibration to a monitor via a calibration.meta.toml sidecar"
```

---

### Task 5: The calibration-state decision function

**Files:**
- Create: `crates/tobii-config/src/calibration_state.rs`
- Modify: `crates/tobii-config/src/lib.rs` (add `mod calibration_state; pub use ...`)

**Interfaces:**
- Consumes: `CalMeta` (Task 4).
- Produces:
  - `pub enum RecommendReason { OtherScreen, GeometryChanged }`
  - `pub enum CalAction { None, ForceSetup, ForceCalibration, RecommendCalibration(RecommendReason) }`
  - `pub fn decide(display_configured: bool, cal: Option<&CalMeta>, active_monitor: Option<&str>, current_fingerprint: u64) -> CalAction`

- [ ] **Step 1: Write the failing tests**

Create `crates/tobii-config/src/calibration_state.rs`:

```rust
//! Pure decision logic: given calibration/display state, what should the GUI do?

use crate::CalMeta;

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(monitor: &str, fp: u64) -> CalMeta {
        CalMeta { monitor_id: Some(monitor.into()), created_utc: 0, mode: "full".into(), display_fingerprint: fp }
    }

    #[test]
    fn no_display_setup_forces_setup() {
        assert_eq!(decide(false, None, Some("SAM7454"), 1), CalAction::ForceSetup);
    }
    #[test]
    fn display_but_no_calibration_forces_calibration() {
        assert_eq!(decide(true, None, Some("SAM7454"), 1), CalAction::ForceCalibration);
    }
    #[test]
    fn wrong_screen_recommends_recalibration() {
        let m = meta("DELA042", 1);
        assert_eq!(decide(true, Some(&m), Some("SAM7454"), 1),
                   CalAction::RecommendCalibration(RecommendReason::OtherScreen));
    }
    #[test]
    fn changed_geometry_recommends_recalibration() {
        let m = meta("SAM7454", 111);
        assert_eq!(decide(true, Some(&m), Some("SAM7454"), 222),
                   CalAction::RecommendCalibration(RecommendReason::GeometryChanged));
    }
    #[test]
    fn matching_calibration_is_silent() {
        let m = meta("SAM7454", 999);
        assert_eq!(decide(true, Some(&m), Some("SAM7454"), 999), CalAction::None);
    }
    #[test]
    fn unknown_active_monitor_skips_screen_check() {
        // Can't identify the screen → don't spuriously force/recommend on screen id;
        // still catch a geometry change.
        let m = meta("SAM7454", 5);
        assert_eq!(decide(true, Some(&m), None, 5), CalAction::None);
        assert_eq!(decide(true, Some(&m), None, 6),
                   CalAction::RecommendCalibration(RecommendReason::GeometryChanged));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p tobii-config calibration_state 2>&1 | tail -25`
Expected: FAIL — types/`decide` not found (after adding `mod calibration_state;` to `lib.rs`).

- [ ] **Step 3: Implement `decide`**

Add above the test module in `calibration_state.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendReason { OtherScreen, GeometryChanged }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalAction { None, ForceSetup, ForceCalibration, RecommendCalibration(RecommendReason) }

/// Decide the calibration action for the current state.
///
/// `active_monitor` is the id of the screen the tracker is on (None if it can't
/// be identified). `current_fingerprint` is `DisplaySetup::fingerprint()` of the
/// geometry currently configured.
pub fn decide(
    display_configured: bool,
    cal: Option<&CalMeta>,
    active_monitor: Option<&str>,
    current_fingerprint: u64,
) -> CalAction {
    if !display_configured {
        return CalAction::ForceSetup;
    }
    let Some(cal) = cal else {
        return CalAction::ForceCalibration;
    };
    // Screen mismatch only when we can identify both sides.
    if let (Some(active), Some(bound)) = (active_monitor, cal.monitor_id.as_deref()) {
        if active != bound {
            return CalAction::RecommendCalibration(RecommendReason::OtherScreen);
        }
    }
    if cal.display_fingerprint != current_fingerprint {
        return CalAction::RecommendCalibration(RecommendReason::GeometryChanged);
    }
    CalAction::None
}
```

Add to `crates/tobii-config/src/lib.rs`:

```rust
mod calibration_state;
pub use calibration_state::{decide, CalAction, RecommendReason};
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p tobii-config calibration_state 2>&1 | tail -25`
Expected: PASS (all six).

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-config/src/calibration_state.rs crates/tobii-config/src/lib.rs
git commit -m "feat(config): calibration-state decision (force/recommend/silent)"
```

---

### Task 6: Wire the state machine into the GUI (force / recommend)

**Files:**
- Modify: `crates/tobii-gtk/src/device.rs` (compute active monitor id + fingerprint on connect; update calibration load/save calls to the new signatures; persist `CalMeta` in `finish_calibration`)
- Modify: `crates/tobii-gtk/src/lib.rs` (on connect, evaluate `decide` and drive force/recommend UI)
- Modify: `crates/tobii-config/src/setup.rs` or config: persist the setup's `monitor_id` in `[display]` (soft-defaulted, like `curvature_radius_mm`)

> GTK is not unit-testable in this repo (audit: cairo/GTK "live-validated"). This task's gate is build + clippy + a manual on-hardware checklist. Keep *all* logic decisions in the Task 5 `decide` function; this task only maps its result to UI.

**Interfaces:**
- Consumes: `tobii_config::{decide, CalAction, RecommendReason, CalMeta, load_calibration, save_calibration, edid_monitor_id, detect_monitors, pick_monitor}`, `DisplaySetup::fingerprint`.

- [ ] **Step 1: Update the calibration load/save call sites to the new signatures**

In `crates/tobii-gtk/src/device.rs:275-283` (re-apply on connect):

```rust
    if let Ok(Some((blob, _meta))) = tobii_config::load_calibration() {
        if let Err(e) = conn.apply_calibration(&blob) {
            eprintln!("warning: could not apply saved calibration ({e})");
        }
    }
```

In `finish_calibration` (`device.rs:329-345`), persist metadata with the blob:

```rust
    let blob = conn.retrieve_calibration().map_err(|e| e.to_string())?;
    if blob.0.is_empty() {
        return Err("device returned an empty calibration".into());
    }
    let setup = tobii_config::load_setup().ok().flatten();           // whatever the existing load is
    let meta = tobii_config::CalMeta {
        monitor_id: active_monitor_id(),                             // helper from Step 2
        created_utc: now_unix_secs(),                                // SystemTime::now() → i64 seconds
        mode: cal_mode_label(),                                      // "quick" | "full" from the flow
        display_fingerprint: setup.map(|s| s.fingerprint()).unwrap_or(0),
    };
    tobii_config::save_calibration(&blob.0, &meta).map_err(|e| e.to_string())
```

(Restore-after-abort at `device.rs:213-221` uses the same `load_calibration` shape — update it to destructure `(blob, _meta)` too.)

- [ ] **Step 2: Add an active-monitor helper**

Add to `crates/tobii-gtk/src/device.rs` (or a small `screen.rs`):

```rust
/// Best-effort id of the screen the tracker is on: the configured setup monitor
/// id if present, else the EDID id of the largest detected monitor.
fn active_monitor_id() -> Option<String> {
    let monitors = tobii_config::detect_monitors();
    tobii_config::pick_monitor(&monitors).and_then(|m| m.id.clone())
}
```

- [ ] **Step 3: Evaluate `decide` on connect and drive the UI**

In `crates/tobii-gtk/src/lib.rs`, where the connect status becomes known (the per-tick closure `:405-443` already reacts to connection state), compute once per fresh connection:

```rust
    let display_configured = tobii_config::load_setup().ok().flatten().is_some();
    let fp = tobii_config::load_setup().ok().flatten().map(|s| s.fingerprint()).unwrap_or(0);
    let cal = tobii_config::load_calibration().ok().flatten().and_then(|(_, m)| m);
    match tobii_config::decide(display_configured, cal.as_ref(), active_monitor_id().as_deref(), fp) {
        tobii_config::CalAction::ForceSetup => setup_flow::launch(&app, cmd_tx.clone()), // modal; block hub
        tobii_config::CalAction::ForceCalibration => calibrate_flow::launch(&app, state.clone(), cmd_tx.clone()),
        tobii_config::CalAction::RecommendCalibration(reason) => show_recommend_banner(&hub, reason), // dismissible
        tobii_config::CalAction::None => {}
    }
```

Add `show_recommend_banner` (a dismissible `gtk::InfoBar`/banner at the top of the hub) with wording matched to the original: OtherScreen → "This calibration was made for a different screen. Recalibrate?"; GeometryChanged → "Display settings changed since calibration. Recalibrate?". "Force" launches must present full-screen and disable the hub until the flow completes (reuse the existing flow windows; gate re-entry with a flag so it fires once per connection).

- [ ] **Step 4: Persist the setup monitor id**

When setup completes, store the chosen monitor's id so future connects compare against it. Add `monitor_id` to the `[display]` TOML (soft-defaulted in `DisplaySetup::from_toml`, mirroring `curvature_radius_mm` at `setup.rs:131,158,169`), set from `active_monitor_id()` at setup save time.

- [ ] **Step 5: Build, lint, and manually verify on hardware**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets 2>&1 | tail -15 && cargo test --workspace 2>&1 | tail -15`
Expected: builds clean, clippy clean, all unit tests pass.

Manual hardware checklist (record results in the PR):
- Fresh profile (no `calibration.bin`) → app **forces** the full flow on connect.
- After calibrating, reconnect → **silent** (no prompt), gaze dot tracks.
- Change display geometry (`tobii setup`) → reconnect → **recommend** banner (GeometryChanged).
- Move tracker to a different monitor → **recommend** banner (OtherScreen).
- **Accuracy:** after the arc-width fix + recalibration, gaze at the screen **borders** is materially better than before (the original symptom). Compare against the captured `screenplane.setpm` geometry.

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-gtk/src/device.rs crates/tobii-gtk/src/lib.rs crates/tobii-config/src/setup.rs
git commit -m "feat(gtk): force calibration when missing, recommend when screen/geometry changed"
```

---

## Self-Review

**Spec coverage:** Component 1 (arc width) → Task 2; Component 2 (monitor id + binding) → Tasks 3-4; Component 3 (`decide` + wiring) → Tasks 5-6; Component 5 (golden test) → Task 1. Component 4 (full step UX parity + per-point retry) is **Phase 2**, intentionally deferred to its own plan. Migration (Component: legacy config) → handled by Task 6 Step 3 (missing meta ⇒ `None` ⇒ force/recommend) + Task 4's legacy-load test.

**Type consistency:** `CalMeta` fields (`monitor_id: Option<String>`, `created_utc: i64`, `mode: String`, `display_fingerprint: u64`) are identical in Tasks 4, 5, 6. `decide` signature identical in Tasks 5 and 6. `edid_monitor_id -> Option<String>` (Task 3) matches `active_monitor_id`'s use and `CalMeta.monitor_id` (Tasks 4/6). `plane_width_from_edid` / `fingerprint` (Task 2) match their uses in Tasks 2/6.

**Placeholder scan:** the GTK helpers referenced in Task 6 that the repo may name differently — `load_setup`, `cal_mode_label`, `now_unix_secs`, `show_recommend_banner` — are called out as "the existing load"/"helper from Step 2"/new functions with their exact behaviour described; the implementer confirms the real `load_setup` name against `store.rs` during Step 1 build. No code step is left as prose.

## Phase 2 (separate future plan — not in scope here)

Full step-UX parity with the screenshots (screen-picker "Diesen Bildschirm", posture step, exploding-dot animation, per-point retry replacing whole-session abort at `calibrate_flow.rs:434-438`, matched wording) and optional `stimulus_points_get` (`0x460`) per-point quality.

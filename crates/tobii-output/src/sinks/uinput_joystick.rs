//! A virtual joystick on `/dev/uinput` — head tracking with no Wine, no
//! second program, and no game-specific integration.
//!
//! # Why this sink exists
//!
//! Every other sink here needs something else to be installed: the opentrack
//! sink needs opentrack, the bridge sink needs a Wine prefix with our DLLs in
//! it. Both are fine for the games that want TrackIR, and useless for a native
//! Linux game that has never heard of head tracking but does have "bind an
//! axis".
//!
//! A uinput device is read by everything that reads a joystick: SDL, evdev,
//! the legacy `/dev/input/js*` interface, and — because Wine's `winebus`
//! enumerates evdev devices — DirectInput inside Wine and Proton. So one
//! ~200-line sink covers native games, Proton games, and emulators at once,
//! for the price of a udev rule.
//!
//! **DirectInput, not XInput.** Measured under wine 11.17 with a probe built
//! against `dinput8` and another against `xinput1_4`: DirectInput enumerates
//! this device as `DI8DEVTYPE_JOYSTICK`, and `XInputGetState` reports zero
//! controllers in all four slots, with and without the device present. XInput's
//! fixed two-stick layout has nowhere to put eight axes, so a game that speaks
//! only XInput cannot use this sink — it needs the Wine bridge or opentrack.
//!
//! # What was taken from opentrack, and what was not
//!
//! opentrack's `proto-libevdev` (ISC, Stanislaw Halik) is the established
//! working implementation, and two facts were taken from reading it because
//! they are not discoverable from the uinput documentation:
//!
//! * **A device that declares no key capability is an accelerometer.**
//!   opentrack's source says only "do not remove next 3 lines or udev scripts
//!   won't assign 0664 permissions", so the mechanism was measured here by
//!   building the device four ways and reading udev's verdict out of
//!   `/run/udev/data/`:
//!
//!   | built with | udev says |
//!   |---|---|
//!   | `EV_KEY` + [`BTN_TRIGGER`]/[`BTN_THUMB`] + all eight axes | `ID_INPUT_JOYSTICK=1` |
//!   | `EV_KEY` declared, **no** key codes | `ID_INPUT_JOYSTICK=1` |
//!   | `EV_KEY` + buttons, only `ABS_X`/`Y`/`Z` | `ID_INPUT_JOYSTICK=1` |
//!   | **no `EV_KEY` at all** | `ID_INPUT_ACCELEROMETER=1`, `IIO_SENSOR_PROXY_TYPE=input-accel`, `SYSTEMD_WANTS=iio-sensor-proxy.service` |
//!
//!   So the load-bearing thing is the bare `EV_KEY` capability, not the button
//!   codes: systemd's `input_id` reads `ABS_X`/`Y`/`Z` with no key capability
//!   as an accelerometer — and then hands it to `iio-sensor-proxy`, which is
//!   the service that rotates laptop screens. Given `EV_KEY`, either the button
//!   codes **or** `ABS_RX`/`RY`/`RZ` independently satisfy its joystick test,
//!   and this device has both.
//!
//!   The buttons are kept anyway, and are never pressed: SDL skips anything
//!   not tagged `ID_INPUT_JOYSTICK`, and Steam's container runtime has a
//!   fallback path that classifies from evdev capabilities directly when udev
//!   properties are unavailable, which does require a code in the
//!   `BTN_JOYSTICK..BTN_GAMEPAD` range. Cheap insurance for a case that cannot
//!   be reproduced outside a container.
//! * **Which axes.** Translation on `ABS_X`/`Y`/`Z` and rotation on
//!   `ABS_RX`/`RY`/`RZ` is the layout every head-tracking guide and game profile
//!   in circulation already assumes. Contiguity matters beyond convention:
//!   Wine's SDL backend builds its HID report descriptor by SDL *axis index*,
//!   so a gap in the ABS codes would silently shift every axis after it.
//!
//! The code is ours: opentrack calls `libevdev`, and this talks to the kernel
//! directly so the crate keeps its "no C dependencies" property. The full-scale
//! values are ours too, and differ deliberately — see [`TRANSLATION_FULL_SCALE_MM`].

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::rc::Rc;

use crate::{Sink, SinkError, TrackingFrame};

// ---------------------------------------------------------------- uapi

// From <linux/input-event-codes.h>. These are ABI: the kernel will never
// renumber them, which is why writing them out is safe and pulling in a
// bindgen-generated crate to learn that ABS_X is 0 would not be an improvement.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_THROTTLE: u16 = 0x06;
const ABS_RUDDER: u16 = 0x07;

/// Declared so `input_id` classifies this as a joystick — see the module docs.
const BTN_TRIGGER: u16 = 0x120;
/// The second button, for the same reason. Neither is ever pressed.
const BTN_THUMB: u16 = 0x121;

/// `BUS_USB`, not `BUS_VIRTUAL`.
///
/// Nothing about this device is a USB device. Bus 3 is what opentrack's
/// `proto-libevdev` sets and what every USB joystick reports.
///
/// The first version of this comment said SDL and Wine treat a virtual-bus
/// device as something other than a game controller. They do not — SDL
/// whitelists the virtual bus alongside real ones when parsing a joystick GUID,
/// and neither classifies by bus at all: SDL goes by `ID_INPUT_JOYSTICK`, and
/// Wine's `winebus` by vendor/product and axis and button counts.
///
/// The bus is still visible through both, which is the real reason to claim the
/// ordinary one: it is the leading field of the SDL joystick GUID that a
/// `gamecontrollerdb` entry or a saved in-game binding matches on, and it picks
/// Wine's device-instance-id prefix (`USB\VID_…` rather than `WINEBUS\VID_…`).
/// Looking like every other joystick costs nothing and is one fewer thing for a
/// game's device filter to trip over.
const BUS_USB: u16 = 0x03;

/// Vendor and product for the virtual device.
///
/// Deliberately **not** Tobii's real `0x2104`: this is a synthetic input device
/// rather than the tracker, and reusing the real vendor id would make the two
/// indistinguishable in `udevadm`-style tooling. A virtual device has no USB
/// allocation to be correct about, so these only need to be stable and unlikely
/// to collide.
const VENDOR: u16 = 0x7462;
const PRODUCT: u16 = 0x0501;

/// What the device calls itself. Games show this string in their bind lists.
const DEVICE_NAME: &str = "Tobii Eye Tracker 5 head tracking";

const UINPUT_IOCTL_BASE: u32 = b'U' as u32;

/// `_IO(UINPUT_IOCTL_BASE, nr)` — a request carrying no argument.
const fn io(nr: u32) -> u32 {
    (UINPUT_IOCTL_BASE << 8) | nr
}

/// `_IOW(UINPUT_IOCTL_BASE, nr, size)` — a request writing `size` bytes in.
const fn iow(nr: u32, size: usize) -> u32 {
    // _IOC_WRITE is 1, and the direction field starts at bit 30 on every
    // architecture this crate builds for.
    (1 << 30) | ((size as u32) << 16) | (UINPUT_IOCTL_BASE << 8) | nr
}

const UI_DEV_CREATE: u32 = io(1);
const UI_DEV_DESTROY: u32 = io(2);
const UI_DEV_SETUP: u32 = iow(3, size_of::<UinputSetup>());
const UI_ABS_SETUP: u32 = iow(4, size_of::<UinputAbsSetup>());
const UI_SET_EVBIT: u32 = iow(100, size_of::<i32>());
const UI_SET_KEYBIT: u32 = iow(101, size_of::<i32>());
const UI_SET_ABSBIT: u32 = iow(103, size_of::<i32>());

/// `struct input_id`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

/// `struct uinput_setup`. The name buffer is `UINPUT_MAX_NAME_SIZE`.
#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// `struct uinput_abs_setup`.
#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

/// `struct input_event`.
///
/// The timestamp is left zeroed: for a uinput device the input core stamps the
/// event itself, and what is written here is discarded.
#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    kind: u16,
    code: u16,
    value: i32,
}

// ------------------------------------------------------------- encoding

/// Lowest value any axis reports.
pub const AXIS_MIN: i32 = 0;
/// The resting value — a head looking straight ahead from the usual distance.
pub const AXIS_CENTRE: i32 = 32767;
/// Highest value any axis reports.
///
/// `65534`, not `65535`, because [`encode_axis`] emits
/// `AXIS_CENTRE ± AXIS_CENTRE`: an odd span would declare a maximum the encoder
/// can never actually reach.
///
/// It does **not** buy a centred axis in every consumer, and this was measured
/// rather than assumed — the first version of this comment claimed it avoided
/// "a permanent fractional offset in games that normalise to `[-1, 1]`", and
/// that is not what either consumer does:
///
/// * joydev rescales to `[-32767, 32767]` and reports exactly `0` at rest for
///   a 65534 span *and* for a 65535 one, so it does not discriminate.
/// * SDL2 maps the span onto its own asymmetric `[-32768, 32767]` and reports
///   `-1` at rest. With this span SDL's `0` is unreachable: raw 32767 reads
///   `-1` and raw 32768 reads `+1`. The span rejected here, paired with centre
///   32768, is the one that would give SDL an exact zero.
///
/// One step in 32767 is about 0.005° of a ±180° axis — below any deadzone a
/// game offers, and far below the tracker's own noise. So the choice is made on
/// the encoder's arithmetic, which is real, rather than on a centring benefit,
/// which is not.
pub const AXIS_MAX: i32 = AXIS_CENTRE * 2;

/// What one end of a rotation axis *means*, in degrees.
///
/// The physical limits of the quantity rather than the limits of a neck, so
/// that the axis carries the same meaning as every other head-tracking wire
/// format. How much head movement it takes to get there is a separate
/// question, answered by `joystick_*_full_deg` in the config and applied by
/// [`Response`] — without which a real head only ever reaches a third of this.
pub const YAW_FULL_SCALE_DEG: f64 = 180.0;
/// As [`YAW_FULL_SCALE_DEG`]. Pitch is half, because it is physically half.
pub const PITCH_FULL_SCALE_DEG: f64 = 90.0;
/// As [`YAW_FULL_SCALE_DEG`].
pub const ROLL_FULL_SCALE_DEG: f64 = 180.0;

/// The amplification stage, and where it does and does not belong.
///
/// # Why the joystick has one and the opentrack sink does not
///
/// The rule is: **shape it where we are the last stage, send it raw where
/// something downstream will shape it.**
///
/// * The **joystick** is the whole chain. Nothing between this and the game
///   will amplify anything, so if we do not, a 65° composed turn arrives as 36%
///   of the axis and the user is told to turn their in-game sensitivity up.
/// * The **opentrack sink** is not. opentrack receives us as a *tracker* and
///   applies its own mapping curves, which its users have already tuned.
///   Amplifying first would double-apply and silently break their profiles.
/// * The **Wine bridge** stands in for the TrackIR/FreeTrack software, which is
///   the thing that shapes the signal before a game sees it — so it arguably
///   wants this too. It is deliberately left raw for now: no game has yet
///   consumed that path, and changing its feel at the same time as bringing it
///   up would mean two untested changes at once.
#[derive(Debug, Clone, Copy)]
pub struct Response {
    /// Head angle reaching full deflection, per axis: yaw, pitch, roll.
    full_deg: [f64; 3],
}

impl Default for Response {
    fn default() -> Self {
        Response {
            full_deg: crate::games::OutputConfig::default().joystick_full_deg,
        }
    }
}

impl Response {
    /// The stage the user's settings ask for.
    pub fn from_config(cfg: &crate::games::OutputConfig) -> Response {
        Response {
            full_deg: cfg.joystick_full_deg,
        }
    }

    /// Map a composed head angle onto the axis's own scale.
    ///
    /// Linear with a hard clamp, not an eased curve. Smoothstep would flatten
    /// the response either side of centre as well as at the ends, and a soft
    /// centre is the one thing a head tracker must not have — it is a deadzone
    /// by another name, in the middle of where the user is looking. Games that
    /// want a curve have one; this stage exists only to make the range usable.
    fn shape(&self, raw_deg: f64, axis: usize, full_scale: f64) -> f64 {
        let head_full = self.full_deg.get(axis).copied().unwrap_or(full_scale);
        if !raw_deg.is_finite() || !head_full.is_finite() || head_full <= 0.0 {
            return 0.0;
        }
        (raw_deg / head_full * full_scale).clamp(-full_scale, full_scale)
    }
}

/// Head translation, in millimetres, that drives an axis to its limit.
///
/// **Displacement from where you normally sit**, not distance from the sensor.
/// [`FramePipeline`](crate::pipeline::FramePipeline) subtracts a neutral before
/// anything reaches here; without that, an ordinary 680 mm seating distance
/// pinned this axis at [`AXIS_MAX`] permanently, for every user.
///
/// The same ±500 mm span the TrackIR encoding uses — see
/// [`AXIS_LIMIT`](crate::trackir::AXIS_LIMIT) — so that a movement of a given
/// size means the same thing on every output this crate has. opentrack's
/// equivalent is ±1 m; matching *it* would have made our two outputs disagree
/// with each other, which is the worse of the two inconsistencies.
pub const TRANSLATION_FULL_SCALE_MM: f64 = 500.0;

/// Map a signed quantity onto the axis range, saturating at the ends.
///
/// A non-finite input reports centre rather than propagating a NaN into an
/// ioctl: the kernel would take it, and the game would see an axis pinned at
/// whatever `NaN as i32` happens to produce.
pub fn encode_axis(value: f64, full_scale: f64) -> i32 {
    // Written out rather than as `!(full_scale > 0.0)`: the negated form also
    // catches NaN, but only by accident of how NaN compares, and the next
    // person to simplify it to `full_scale <= 0.0` would silently drop that.
    if !value.is_finite() || !full_scale.is_finite() || full_scale <= 0.0 {
        return AXIS_CENTRE;
    }
    let n = (value / full_scale).clamp(-1.0, 1.0);
    AXIS_CENTRE + (n * AXIS_CENTRE as f64).round() as i32
}

/// Map a `0.0..=1.0` quantity — a normalised gaze coordinate — onto the axis.
pub fn encode_unit(value: f64) -> i32 {
    if !value.is_finite() {
        return AXIS_CENTRE;
    }
    (value.clamp(0.0, 1.0) * AXIS_MAX as f64).round() as i32
}

/// The gaze axes' value for this frame, holding the last one through a blink.
///
/// Gaze vanishes for 100-400 ms every few seconds. Reporting centre for those
/// frames would twitch both gaze axes several times a minute; holding the last
/// value makes a blink invisible, which is what the Extended View path does for
/// the same reason.
///
/// A free function rather than a branch inside `emit` so it can be tested
/// without `/dev/uinput` — it had no test at all, under a doc comment on a
/// different test that claimed otherwise.
fn hold_gaze(last: [i32; 2], gaze: Option<[f64; 2]>) -> [i32; 2] {
    match gaze {
        Some([gx, gy]) => [encode_unit(gx), encode_unit(gy)],
        None => last,
    }
}

/// The axes this device declares, in report order.
///
/// Gaze occupies `ABS_THROTTLE` and `ABS_RUDDER` because those two are already
/// in `input_id`'s joystick-classification set, so adding them cannot cost the
/// device its classification — and because no head tracker offers them, a game
/// profile that binds them cannot be inheriting a meaning from somewhere else.
const AXES: [u16; 8] = [
    ABS_X,
    ABS_Y,
    ABS_Z,
    ABS_RX,
    ABS_RY,
    ABS_RZ,
    ABS_THROTTLE,
    ABS_RUDDER,
];

// --------------------------------------------------------------- device

/// A virtual joystick fed by head pose and gaze.
pub struct UinputJoystick {
    fd: File,
    /// How much head movement reaches full deflection — see [`Response`].
    response: Response,
    /// Last gaze reported, held across blinks.
    ///
    /// Gaze vanishes for a fraction of a second every few seconds. Reporting
    /// centre for those frames would make the two gaze axes twitch on every
    /// blink; holding the last value makes a blink invisible, which is what the
    /// Extended View path already does for the same reason.
    last_gaze: [i32; 2],
}

/// Turn `errno` into an `io::Error`, tagging the failing operation.
fn last_error(what: &str) -> io::Error {
    // Captured once: `last_os_error` reads a thread-local `errno` that any
    // intervening call could overwrite, and reading it twice to build one
    // message is how an error ends up describing the wrong failure.
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

impl UinputJoystick {
    /// Create the device.
    ///
    /// Fails if `/dev/uinput` cannot be opened for writing, which is the
    /// ordinary case on a distribution that ships no rule for it — so the error
    /// says what to install rather than just reporting `EACCES` and leaving the
    /// user to guess.
    pub fn open() -> io::Result<Self> {
        let fd = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        // Deliberately NOT "re-plug", which the otherwise
                        // identical message in `tobii_usb::transport` does say.
                        // That is right for the tracker and wrong here:
                        // /dev/uinput is a virtual misc device with no USB
                        // parent, so unplugging the ET5 never re-events it and
                        // the uaccess ACL never lands. `tobii debug` already
                        // prints the correct remedy; these two must agree.
                        "/dev/uinput: {e}\n\
                         The virtual joystick needs write access to /dev/uinput. \
                         Install the packaged udev rule (60-tobii.rules) and log out \
                         and back in — the grant is a logind ACL applied at session \
                         start, so re-plugging the tracker does not apply it. \
                         If the node does not exist at all: sudo modprobe uinput"
                    ),
                )
            })?;
        let raw = fd.as_raw_fd();

        // SAFETY: `raw` is a live fd from the `File` above, and this block makes
        // three shapes of uinput request, not one. The `UI_SET_*BIT` calls take
        // an `int` by value, and each argument is a valid `i32`. `UI_ABS_SETUP`
        // and `UI_DEV_SETUP` take a POINTER to a `#[repr(C)]` struct matching
        // the kernel's `uinput_abs_setup` / `uinput_setup` — fully initialised
        // on this stack frame and live across the call — and the byte count the
        // kernel copies is the `size_of` baked into the request number where it
        // is defined above, so the two cannot drift apart. `UI_DEV_CREATE` is an
        // `_IO` and takes no argument at all.
        //
        // The pointer arguments are the ones that could actually corrupt
        // memory, and they were the ones the first version of this comment did
        // not mention: it said "every request is a uinput request taking an
        // `int`", which is true of `set_bit` alone and was never widened.
        unsafe {
            set_bit(raw, UI_SET_EVBIT, EV_ABS as i32)?;
            set_bit(raw, UI_SET_EVBIT, EV_KEY as i32)?;
            set_bit(raw, UI_SET_KEYBIT, BTN_TRIGGER as i32)?;
            set_bit(raw, UI_SET_KEYBIT, BTN_THUMB as i32)?;
            for code in AXES {
                set_bit(raw, UI_SET_ABSBIT, code as i32)?;
            }

            for code in AXES {
                let setup = UinputAbsSetup {
                    code,
                    absinfo: AbsInfo {
                        value: AXIS_CENTRE,
                        minimum: AXIS_MIN,
                        maximum: AXIS_MAX,
                        // No fuzz and no flat: this is a synthetic axis with no
                        // noise to filter and no mechanical dead spot to
                        // declare. A non-zero `flat` here would silently snap
                        // small head movements to centre — exactly the
                        // movements a head tracker exists to report.
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                    },
                };
                if libc::ioctl(raw, UI_ABS_SETUP as _, &setup) < 0 {
                    return Err(last_error("UI_ABS_SETUP"));
                }
            }

            let mut name = [0u8; 80];
            let bytes = DEVICE_NAME.as_bytes();
            let n = bytes.len().min(name.len() - 1);
            name[..n].copy_from_slice(&bytes[..n]);
            let setup = UinputSetup {
                id: InputId {
                    bustype: BUS_USB,
                    vendor: VENDOR,
                    product: PRODUCT,
                    version: 1,
                },
                name,
                ff_effects_max: 0,
            };
            if libc::ioctl(raw, UI_DEV_SETUP as _, &setup) < 0 {
                return Err(last_error("UI_DEV_SETUP"));
            }
            if libc::ioctl(raw, UI_DEV_CREATE as _) < 0 {
                return Err(last_error("UI_DEV_CREATE"));
            }
        }

        Ok(UinputJoystick {
            fd,
            response: Response::default(),
            last_gaze: [AXIS_CENTRE; 2],
        })
    }

    /// Re-tune without recreating the device.
    ///
    /// The device deliberately outlives any one tracking session, so a settings
    /// change has to reach the live one — recreating it to apply a number would
    /// take the controller out of every running game's bind list.
    pub fn set_response(&mut self, response: Response) {
        self.response = response;
    }

    /// Write one batch of axis values, terminated by the `SYN_REPORT` that
    /// makes the kernel deliver them as a single state change.
    fn report(&mut self, values: [i32; 8]) -> io::Result<()> {
        let mut events = [InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }; AXES.len() + 1];
        for (i, (code, value)) in AXES.iter().zip(values).enumerate() {
            events[i] = InputEvent {
                tv_sec: 0,
                tv_usec: 0,
                kind: EV_ABS,
                code: *code,
                value,
            };
        }
        // SAFETY: `events` is a live array of `#[repr(C)]` structs with no
        // padding bytes read as anything but bytes, and the length is exact.
        let bytes = unsafe {
            std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), std::mem::size_of_val(&events))
        };
        self.fd.write_all(bytes)
    }
}

/// One `UI_SET_*BIT` call.
///
/// # Safety
/// `fd` must be a live uinput file descriptor and `request` one of the
/// `UI_SET_*BIT` requests, all of which take an `int` by value.
unsafe fn set_bit(fd: i32, request: u32, bit: i32) -> io::Result<()> {
    if libc::ioctl(fd, request as _, bit) < 0 {
        return Err(last_error("UI_SET_*BIT"));
    }
    Ok(())
}

impl Drop for UinputJoystick {
    /// Remove the device.
    ///
    /// Without this the joystick outlives the sink until the process exits, so
    /// turning game output off in the hub would leave a phantom controller in
    /// every running game's bind list.
    fn drop(&mut self) {
        // SAFETY: the fd is live until this `File` is dropped, immediately
        // after this call. A failure here has nowhere to be reported and
        // nothing to do about it — the fd closing tears the device down anyway.
        unsafe {
            libc::ioctl(self.fd.as_raw_fd(), UI_DEV_DESTROY as _);
        }
    }
}

impl Sink for UinputJoystick {
    fn name(&self) -> &'static str {
        "joystick"
    }

    fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError> {
        let Some(pose) = frame.pose else {
            return Ok(());
        };
        self.last_gaze = hold_gaze(self.last_gaze, frame.gaze);
        let r = &self.response;
        self.report([
            encode_axis(pose.x_mm, TRANSLATION_FULL_SCALE_MM),
            encode_axis(pose.y_mm, TRANSLATION_FULL_SCALE_MM),
            encode_axis(pose.z_mm, TRANSLATION_FULL_SCALE_MM),
            encode_axis(
                r.shape(pose.yaw_deg, 0, YAW_FULL_SCALE_DEG),
                YAW_FULL_SCALE_DEG,
            ),
            encode_axis(
                r.shape(pose.pitch_deg, 1, PITCH_FULL_SCALE_DEG),
                PITCH_FULL_SCALE_DEG,
            ),
            encode_axis(
                r.shape(pose.roll_deg, 2, ROLL_FULL_SCALE_DEG),
                ROLL_FULL_SCALE_DEG,
            ),
            self.last_gaze[0],
            self.last_gaze[1],
        ])?;
        Ok(())
    }
}

/// Whether a virtual joystick made by this program already exists.
///
/// Two producers can want one: the hub, which keeps a device alive for as long
/// as the setting is on — including while the tracker is dark — and
/// `tobii headpose`. They would use the same [`DEVICE_NAME`], [`VENDOR`] and
/// [`PRODUCT`], so a second one is not a second input: it is two identical
/// entries in the game's bind list, of which only one moves, with nothing to
/// tell them apart. Worse, the frozen one is as likely to be picked as the
/// live one, since the hub's is usually enumerated first.
///
/// Scanned from sysfs rather than tracked in a process-wide flag, because the
/// two producers are different processes.
pub fn already_present() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/input") else {
        // No sysfs to consult is not evidence of absence, but reporting
        // "present" would disable the sink on a system where it might work.
        return false;
    };
    dir.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("device/name")).is_ok_and(|n| n.trim() == DEVICE_NAME)
    })
}

/// A shared reference to a joystick whose lifetime is owned elsewhere.
///
/// # Why the device must outlive the tracking session
///
/// Every other sink here is a UDP socket, and a socket that comes and goes is
/// invisible: nothing enumerates it. A joystick is enumerated, by name, and
/// twice over:
///
/// * A game builds its bind list when it launches. One that does not watch for
///   hot-plug simply never sees a device created afterwards — and the tracker
///   is off until something asks for it, so "afterwards" is the normal case.
/// * A game that *does* watch for hot-plug shows a "controller disconnected"
///   prompt every time the device goes away, which would be a few seconds
///   after the user stops looking at the screen.
///
/// So the device follows the **setting**, not the USB session: the hub creates
/// one while game output is on and hands each session a handle, rather than
/// each session creating its own and taking it down on the way out.
///
/// `Rc`, not `Arc`: the hub's device thread is the only thread that owns or
/// emits to it, and a mutex on the 60 Hz path would be a lock nobody ever
/// contends.
#[derive(Clone)]
pub struct JoystickHandle(Rc<RefCell<UinputJoystick>>);

impl JoystickHandle {
    /// Take ownership of a device and start sharing it.
    pub fn new(device: UinputJoystick) -> Self {
        JoystickHandle(Rc::new(RefCell::new(device)))
    }

    /// Re-tune the shared device — see [`UinputJoystick::set_response`].
    pub fn set_response(&self, response: Response) {
        self.0.borrow_mut().set_response(response);
    }
}

impl Sink for JoystickHandle {
    fn name(&self) -> &'static str {
        "joystick"
    }

    fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError> {
        self.0.borrow_mut().emit(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every structure here is passed to the kernel by size-encoded ioctl. A
    /// layout that does not match the uapi header is not a compile error and
    /// not a runtime error either — the ioctl number simply stops matching and
    /// the call fails with `ENOTTY`, or worse, matches and reads garbage.
    #[test]
    fn the_structures_are_the_sizes_the_kernel_expects() {
        assert_eq!(size_of::<InputId>(), 8);
        assert_eq!(size_of::<UinputSetup>(), 92);
        assert_eq!(size_of::<AbsInfo>(), 24);
        assert_eq!(size_of::<UinputAbsSetup>(), 28);
        assert_eq!(size_of::<InputEvent>(), 24);
    }

    /// The four request numbers, computed against the values in
    /// `<linux/uinput.h>`. Transcribing these by hand is the classic way to
    /// get a silently dead uinput device, so they are pinned.
    #[test]
    fn the_ioctl_numbers_match_the_uapi_header() {
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_DESTROY, 0x5502);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(UI_ABS_SETUP, 0x401c_5504);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_SET_ABSBIT, 0x4004_5567);
    }

    /// Without the response stage a real head only reaches a third of the
    /// axis, which is the difference between a usable bind and one the user
    /// concludes is broken.
    #[test]
    fn the_response_stage_is_what_makes_the_range_reachable() {
        let r = Response::default();
        // Extended View at "Normal" contributes up to 45 degrees of yaw; add a
        // 20 degree head turn, which is about as far as the ET5 can follow
        // before it loses an eye.
        let composed = 65.0;
        let raw_fraction = composed / YAW_FULL_SCALE_DEG;
        let shaped_fraction = r.shape(composed, 0, YAW_FULL_SCALE_DEG) / YAW_FULL_SCALE_DEG;
        assert!(
            raw_fraction < 0.4,
            "premise: unshaped, a full deliberate look is a third of the axis"
        );
        assert!(
            shaped_fraction > 0.85,
            "a full deliberate look should be most of the axis, got {:.0}%",
            shaped_fraction * 100.0
        );

        // Roll is the worst case: no Extended View contributes to it at all.
        let tilt = 15.0;
        assert!(
            r.shape(tilt, 2, ROLL_FULL_SCALE_DEG) / ROLL_FULL_SCALE_DEG > 0.6,
            "a 15 degree head tilt is 8% of the axis unshaped"
        );
    }

    /// Amplifying must not wrap or overshoot: past the configured head angle
    /// the axis saturates, and saturation must land exactly on the end stop.
    #[test]
    fn past_the_configured_head_angle_the_axis_saturates_exactly() {
        let r = Response::default();
        for (axis, full) in [
            (0, YAW_FULL_SCALE_DEG),
            (1, PITCH_FULL_SCALE_DEG),
            (2, ROLL_FULL_SCALE_DEG),
        ] {
            assert_eq!(r.shape(1e6, axis, full), full);
            assert_eq!(r.shape(-1e6, axis, full), -full);
            assert_eq!(encode_axis(r.shape(1e6, axis, full), full), AXIS_MAX);
            assert_eq!(encode_axis(r.shape(-1e6, axis, full), full), AXIS_MIN);
        }
        // Centre stays centre — a soft or offset centre is the one thing a
        // head tracker must not have.
        assert_eq!(
            encode_axis(r.shape(0.0, 0, YAW_FULL_SCALE_DEG), YAW_FULL_SCALE_DEG),
            AXIS_CENTRE
        );
    }

    /// A nonsensical setting must not divide by zero or emit a NaN into an
    /// ioctl — the kernel would take it and the axis would pin wherever
    /// `NaN as i32` lands.
    #[test]
    fn a_zero_or_non_finite_setting_reports_centre() {
        for bad in [0.0, -10.0, f64::NAN, f64::INFINITY] {
            let r = Response { full_deg: [bad; 3] };
            assert_eq!(r.shape(30.0, 0, YAW_FULL_SCALE_DEG), 0.0, "full_deg {bad}");
        }
    }

    /// An axis whose halves differ in length reads as a permanent offset in
    /// any game that normalises to `[-1, 1]`.
    #[test]
    fn the_axis_is_symmetric_about_centre() {
        assert_eq!(AXIS_CENTRE - AXIS_MIN, AXIS_MAX - AXIS_CENTRE);
        assert_eq!(encode_axis(0.0, 180.0), AXIS_CENTRE);
        assert_eq!(encode_axis(-180.0, 180.0), AXIS_MIN);
        assert_eq!(encode_axis(180.0, 180.0), AXIS_MAX);
    }

    /// Past full scale the axis has to stop, not wrap. A wrapped axis sends the
    /// view to the opposite extreme at the exact moment the head reaches the
    /// edge of its range.
    #[test]
    fn beyond_full_scale_saturates() {
        assert_eq!(encode_axis(1e9, 180.0), AXIS_MAX);
        assert_eq!(encode_axis(-1e9, 180.0), AXIS_MIN);
        assert_eq!(encode_unit(2.0), AXIS_MAX);
        assert_eq!(encode_unit(-1.0), AXIS_MIN);
    }

    /// The two encoders fail differently, and the difference is why this test
    /// has the assertions it does.
    ///
    /// It used to say only "`NaN as i32` is 0 in Rust, so an unguarded
    /// non-finite pose would pin every axis hard left". That is true of
    /// [`encode_unit`], which maps straight onto the range — with its guard
    /// gone, a NaN gaze pins hard at [`AXIS_MIN`]. It is *not* true of
    /// [`encode_axis`], which returns `AXIS_CENTRE + delta`: a NaN survives the
    /// clamp (`f64::clamp` propagates NaN rather than clamping it), `NaN as
    /// i32` saturates to 0, and centre plus zero is centre. So two of the four
    /// assertions passed with the guard deleted.
    ///
    /// What `encode_axis`'s guard actually saves is a non-finite **full scale**,
    /// which would make the ratio NaN by division — and that case does fail
    /// without it.
    #[test]
    fn a_non_finite_pose_reports_centre_rather_than_a_hard_deflection() {
        // Records intent; would pass without the guard, for the reason above.
        assert_eq!(encode_axis(f64::NAN, 180.0), AXIS_CENTRE);
        assert_eq!(encode_axis(f64::INFINITY, 180.0), AXIS_CENTRE);
        // These two are the ones that catch a regression.
        assert_eq!(encode_axis(1.0, f64::NAN), AXIS_CENTRE);
        assert_eq!(encode_unit(f64::NAN), AXIS_CENTRE);
    }

    /// Translation full scale is shared with the TrackIR encoder on purpose:
    /// the same head movement must mean the same thing on both outputs.
    #[test]
    fn translation_full_scale_agrees_with_the_trackir_encoder() {
        let trackir_span_mm = crate::trackir::AXIS_LIMIT / crate::trackir::TRANSLATION_SCALE;
        assert!(
            (trackir_span_mm as f64 - TRANSLATION_FULL_SCALE_MM).abs() < 1.0,
            "TrackIR saturates at {trackir_span_mm} mm but the joystick at \
             {TRANSLATION_FULL_SCALE_MM} mm"
        );
    }

    /// The axis set alone has to satisfy udev's joystick test, independently of
    /// the key bits — see the four-way experiment in the module docs.
    #[test]
    fn the_axis_set_is_classified_as_a_joystick() {
        // ABS_X and ABS_Y plus any of ABS_RX/RY/RZ or a key bit in the joystick
        // range is what systemd's input_id builtin looks for.
        assert!(AXES.contains(&ABS_X) && AXES.contains(&ABS_Y));
        assert!(AXES.contains(&ABS_RX));
    }

    /// Gaze is held through a blink. Reporting centre instead would twitch the
    /// two gaze axes several times a minute.
    ///
    /// This sentence used to sit above the test above it, which does not
    /// exercise the hold at all — so the behaviour had no test, under a comment
    /// claiming it did. Deleting the hold now fails here.
    #[test]
    fn a_blink_holds_the_last_gaze_rather_than_reporting_centre() {
        let looking = hold_gaze([AXIS_CENTRE; 2], Some([1.0, 0.0]));
        assert_eq!(
            looking,
            [AXIS_MAX, AXIS_MIN],
            "gaze at the top-right corner"
        );

        let blink = hold_gaze(looking, None);
        assert_eq!(blink, looking, "a blink must hold, not recentre");
        assert_ne!(
            blink, [AXIS_CENTRE; 2],
            "reporting centre through a blink is the twitch this prevents"
        );

        // And a real look back to the middle still moves — the hold applies
        // only when gaze is absent, never when it is present and central.
        assert_eq!(hold_gaze(looking, Some([0.5, 0.5])), [AXIS_CENTRE; 2]);
    }

    /// The only test that proves any of this works, end to end.
    ///
    /// Everything above checks arithmetic against numbers this file also
    /// chose. Three things it cannot reach: whether the kernel accepts this
    /// sequence of ioctls, whether udev then classifies the result as a
    /// joystick rather than as an accelerometer, and whether the values a
    /// reader sees are the values that were encoded. All three need a real
    /// device, which needs write access to `/dev/uinput` — hence `#[ignore]`
    /// rather than running by default.
    ///
    /// `cargo test -p tobii-output -- --ignored --nocapture`
    #[test]
    #[ignore = "needs write access to /dev/uinput"]
    fn a_real_device_is_a_joystick_and_reports_what_was_encoded() {
        use crate::TrackingFrame;
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        /// Every `/sys/class/input/*` entry whose device is ours.
        fn our_nodes() -> Vec<PathBuf> {
            let Ok(dir) = std::fs::read_dir("/sys/class/input") else {
                return Vec::new();
            };
            dir.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    std::fs::read_to_string(p.join("device/name"))
                        .is_ok_and(|n| n.trim() == DEVICE_NAME)
                })
                .collect()
        }

        // Stamped before the device exists, so the udev record read below can
        // be proved to be about THIS device. Minor numbers are reused: a record
        // left by a previous run of this very test sits at the same path and
        // reads as a pass, which is how the first version of this assertion
        // passed with the key bits deleted.
        let created_at = std::time::SystemTime::now();
        let mut js = UinputJoystick::open().expect("create the device");

        // udev has to run its rules before the nodes exist.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut nodes = our_nodes();
        while Instant::now() < deadline && nodes.len() < 2 {
            std::thread::sleep(Duration::from_millis(50));
            nodes = our_nodes();
        }
        let named = |prefix: &str| {
            nodes.iter().find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
            })
        };

        let event_node = named("event").expect("an event node").clone();

        // Classification, asserted where udev actually records it.
        //
        // This used to assert that a `/dev/input/js*` node existed and call
        // that the classification check. It is not one: `joydev` binds to any
        // device with absolute axes, so the js node appears even for a device
        // udev has decided is an accelerometer — which is exactly what the
        // no-buttons experiment in the module docs produced, js node and all.
        // The assertion could not fail, and its comment said the opposite of
        // the truth.
        //
        // `/run/udev/data/c<major>:<minor>` is where udev keeps the properties
        // it computed, world-readable, with no `udevadm` to shell out to.
        //
        // Retried, for the same reason the evdev open below is: the sysfs entry
        // exists the instant the kernel creates the device, and udev writes its
        // verdict afterwards.
        let dev = std::fs::read_to_string(event_node.join("dev")).expect("the dev node numbers");
        let record = format!("/run/udev/data/c{}", dev.trim());
        let deadline = Instant::now() + Duration::from_secs(3);
        let fresh = |path: &str| -> bool {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                // A record written in the same second as `created_at` can carry
                // a marginally earlier timestamp than the SystemTime read;
                // a second of slack keeps that from flaking without letting a
                // record from a previous run through.
                .map(|m| m + Duration::from_secs(1) >= created_at)
                .unwrap_or(false)
        };
        let props = loop {
            match std::fs::read_to_string(&record) {
                Ok(p) if p.contains("E:ID_INPUT") && fresh(&record) => break p,
                other => {
                    if Instant::now() >= deadline {
                        panic!(
                            "no fresh udev verdict at {record} after 3s \
                             (a stale record from a previous device with the same \
                             minor does not count): {other:?}"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        assert!(
            props.lines().any(|l| l == "E:ID_INPUT_JOYSTICK=1"),
            "udev did not classify this as a joystick, so SDL will skip it.\n{props}"
        );
        assert!(
            !props
                .lines()
                .any(|l| l.starts_with("E:ID_INPUT_ACCELEROMETER")),
            "classified as an accelerometer — the key bits are what prevent \
             that, see the module docs.\n{props}"
        );
        // Kept, but for what it actually shows: that joydev bound to the
        // device and the legacy interface works.
        let js_node = named("js").expect("a js* node — joydev did not bind");
        println!(
            "udev: ID_INPUT_JOYSTICK=1; joydev bound at {}",
            js_node.display()
        );

        // Read back through evdev. Opened before emitting: an evdev node
        // delivers nothing that happened before it was opened.
        // Retried, not opened once: the sysfs entry exists the instant the
        // kernel creates the device, but the `uaccess` ACL that makes it
        // readable is applied later by udev processing that event. Opening
        // immediately loses the race and gets EACCES — which is a test bug
        // rather than a permission-model problem, since a game starts long
        // after this.
        let dev = PathBuf::from("/dev/input").join(event_node.file_name().expect("a name"));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reader = loop {
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&dev)
            {
                Ok(f) => break f,
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("open {} after waiting for the udev ACL: {e}", dev.display()),
            }
        };

        let yaw_deg = 45.0;
        js.emit(&TrackingFrame {
            timestamp_us: 0,
            pose: Some(tobii_headpose::HeadPose {
                yaw_deg,
                ..Default::default()
            }),
            gaze: Some([1.0, 0.0]),
            presence: crate::Presence::BothEyes,
        })
        .expect("emit a frame");

        let mut seen: Vec<(u16, i32)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buf = [0u8; size_of::<InputEvent>() * 32];
        while Instant::now() < deadline && !seen.iter().any(|(c, _)| *c == SYN_REPORT) {
            match reader.read(&mut buf) {
                Ok(n) => {
                    for off in (0..n).step_by(size_of::<InputEvent>()) {
                        let Some(chunk) = buf.get(off..off + size_of::<InputEvent>()) else {
                            break;
                        };
                        let kind = u16::from_ne_bytes([chunk[16], chunk[17]]);
                        let code = u16::from_ne_bytes([chunk[18], chunk[19]]);
                        let value = i32::from_ne_bytes(chunk[20..24].try_into().expect("4 bytes"));
                        if kind == EV_ABS {
                            seen.push((code, value));
                        } else if kind == EV_SYN {
                            seen.push((SYN_REPORT, -1));
                        }
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }

        let value_of = |code: u16| {
            seen.iter()
                .find(|(c, _)| *c == code)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| panic!("no report for axis {code:#x} in {seen:?}"))
        };
        // Through the response stage, which is what the sink applies — the
        // round trip is only meaningful against what the sink actually encodes.
        let expected = encode_axis(
            Response::default().shape(yaw_deg, 0, YAW_FULL_SCALE_DEG),
            YAW_FULL_SCALE_DEG,
        );
        assert_eq!(
            value_of(ABS_RX),
            expected,
            "yaw did not survive the round trip"
        );
        assert_eq!(value_of(ABS_THROTTLE), AXIS_MAX, "gaze x at the right edge");
        assert_eq!(value_of(ABS_RUDDER), AXIS_MIN, "gaze y at the top");
        println!(
            "read back {} axis values through {}",
            seen.len(),
            dev.display()
        );
    }
}

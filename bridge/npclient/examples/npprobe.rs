//! A stand-in for a TrackIR game: load an `NPClient64.dll` by path, resolve its
//! exports, and call them — including `NP_GetData`, printing what comes back.
//!
//! Exists to answer two questions without launching a game. **Does a given
//! NPClient DLL read the mapping our provider writes?** Star Citizen rejects our
//! own DLL at `NP_GetSignature`, so the question becomes whether a
//! separately-installed client (opentrack ships one) can be pointed at our
//! provider instead. And **what does each export leave in the caller's
//! buffer?** Every out-parameter here arrives filled with [`POISON`], so a
//! buffer that comes back still holding it was never written — which is what a
//! game reads as its own uninitialised memory.
//!
//! Built on demand, not shipped:
//! `cargo build --release --target x86_64-pc-windows-gnu --example npprobe`

// The Windows types keep the header's spelling.
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

use tobii_output::trackir::TrackIrData;

type HMODULE = *mut c_void;
type FARPROC = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const u8) -> HMODULE;
    fn GetProcAddress(module: HMODULE, name: *const u8) -> FARPROC;
    fn GetLastError() -> u32;
}

/// A byte no signature and no version number would contain. Out-parameters go
/// in holding it, standing in for the uninitialised memory a game hands over.
const POISON: u8 = 0xAA;

/// What `NP_GetSignature` fills: two 200-byte strings.
#[repr(C)]
struct SignatureData {
    dll_signature: [u8; 200],
    app_signature: [u8; 200],
}

impl SignatureData {
    fn poisoned() -> Self {
        SignatureData {
            dll_signature: [POISON; 200],
            app_signature: [POISON; 200],
        }
    }
}

/// Render a fixed-size C string field for display, saying plainly when the DLL
/// wrote nothing at all.
fn show(bytes: &[u8]) -> String {
    let poisoned = bytes.iter().filter(|b| **b == POISON).count();
    if poisoned == bytes.len() {
        return format!("UNTOUCHED — all {poisoned} bytes still {POISON:#04x}");
    }
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let s = String::from_utf8_lossy(&bytes[..end]);
    match (s.is_empty(), poisoned) {
        (true, 0) => "empty string (zeroed)".to_string(),
        (true, n) => format!("empty string, {n} bytes still poisoned"),
        (false, n) => format!("{s:?} ({end} bytes, {n} still poisoned)"),
    }
}

/// Name a return code, from NaturalPoint's `NPClient.h`.
fn rc(code: i32) -> String {
    let name = match code {
        0 => "NP_OK",
        1 => "NP_ERR_DEVICE_NOT_PRESENT",
        2 => "NP_ERR_UNSUPPORTED_OS",
        3 => "NP_ERR_INVALID_ARG",
        4 => "NP_ERR_DLL_NOT_FOUND",
        5 => "NP_ERR_NO_DATA",
        6 => "NP_ERR_INTERNAL_DATA",
        _ => "unknown code",
    };
    format!("{code} ({name})")
}

fn main() {
    let dll = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\tobii-bridge\NPClient64.dll".to_string());
    let mut name = dll.clone().into_bytes();
    name.push(0);

    let module = unsafe { LoadLibraryA(name.as_ptr()) };
    if module.is_null() {
        println!("FAIL: LoadLibrary({dll}) failed, error {}", unsafe {
            GetLastError()
        });
        std::process::exit(1);
    }
    println!("loaded {dll}");

    let proc = |n: &str| {
        let mut b = n.as_bytes().to_vec();
        b.push(0);
        unsafe { GetProcAddress(module, b.as_ptr()) }
    };

    for export in [
        "NP_GetSignature",
        "NP_QueryVersion",
        "NP_RegisterWindowHandle",
        "NP_RegisterProgramProfileID",
        "NP_RequestData",
        "NP_StartDataTransmission",
        "NP_GetData",
    ] {
        println!(
            "  {export}: {}",
            if proc(export).is_null() {
                "MISSING"
            } else {
                "present"
            }
        );
    }

    // The signature is what Star Citizen checks before it will use a DLL, so
    // seeing whether this one returns a non-empty value is the whole point.
    let get_sig: extern "system" fn(*mut SignatureData) -> i32 =
        unsafe { std::mem::transmute(proc("NP_GetSignature")) };
    let mut sig = SignatureData::poisoned();
    let code = get_sig(&mut sig);
    println!("\nNP_GetSignature -> {}", rc(code));
    println!("  DllSignature: {}", show(&sig.dll_signature));
    println!("  AppSignature: {}", show(&sig.app_signature));

    // Drive the normal startup sequence a game performs.
    let version: extern "system" fn(*mut u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_QueryVersion")) };
    let mut v = u16::from_le_bytes([POISON, POISON]);
    let code = version(&mut v);
    println!("NP_QueryVersion -> {} (version {v:#06x})", rc(code));

    let reg_win: extern "system" fn(*mut c_void) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RegisterWindowHandle")) };
    println!(
        "NP_RegisterWindowHandle -> {}",
        rc(reg_win(std::ptr::null_mut()))
    );

    let reg_id: extern "system" fn(u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RegisterProgramProfileID")) };
    println!(
        "NP_RegisterProgramProfileID(13302) -> {}",
        rc(reg_id(13302))
    );

    let request: extern "system" fn(u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RequestData")) };
    println!("NP_RequestData(0x00ff) -> {}", rc(request(0x00ff)));

    let start: extern "system" fn() -> i32 =
        unsafe { std::mem::transmute(proc("NP_StartDataTransmission")) };
    println!("NP_StartDataTransmission -> {}", rc(start()));

    let get_data: extern "system" fn(*mut TrackIrData) -> i32 =
        unsafe { std::mem::transmute(proc("NP_GetData")) };

    println!("\nsampling NP_GetData:");
    let mut sigs = Vec::new();
    for i in 0..5 {
        // Poisoned rather than zeroed: a DLL that answers NP_OK without writing
        // leaves this as it arrived, and zeroes would read as a real frame at
        // dead centre.
        let mut d = TrackIrData::default();
        unsafe {
            std::ptr::write_bytes(
                std::ptr::from_mut(&mut d).cast::<u8>(),
                POISON,
                std::mem::size_of::<TrackIrData>(),
            );
        }
        let code = get_data(&mut d);
        println!(
            "  {i}: rc={} status={} frame={} yaw={:.1} pitch={:.1} roll={:.1} \
             x={:.1} y={:.1} z={:.1}",
            rc(code),
            d.wNPStatus,
            d.wPFrameSignature,
            d.fNPYaw,
            d.fNPPitch,
            d.fNPRoll,
            d.fNPX,
            d.fNPY,
            d.fNPZ
        );
        sigs.push((d.wPFrameSignature, d.fNPYaw));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    let moving = sigs.windows(2).any(|w| w[0] != w[1]);
    if moving {
        println!("\nPASS: values changed between samples — this DLL is reading our provider");
    } else {
        println!("\nNOTE: nothing changed; is the provider running and receiving frames?");
    }
}

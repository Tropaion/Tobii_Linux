//! A stand-in for a TrackIR game: load an `NPClient64.dll` by path, resolve its
//! exports, and call them — including `NP_GetData`, printing what comes back.
//!
//! Exists to answer one question without launching a game: **does a given
//! NPClient DLL read the mapping our provider writes?** Star Citizen rejects our
//! own DLL at `NP_GetSignature`, so the question becomes whether a
//! separately-installed client (opentrack ships one) can be pointed at our
//! provider instead.
//!
//! Built on demand, not shipped:
//! `cargo build --release --target x86_64-pc-windows-gnu --example npprobe`

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

/// What `NP_GetSignature` fills: two 200-byte strings.
#[repr(C)]
struct SignatureData {
    dll_signature: [u8; 200],
    app_signature: [u8; 200],
}

impl Default for SignatureData {
    fn default() -> Self {
        SignatureData {
            dll_signature: [0; 200],
            app_signature: [0; 200],
        }
    }
}

/// Render a fixed-size C string field for display.
fn show(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let s = String::from_utf8_lossy(&bytes[..end]);
    if s.is_empty() {
        "(empty)".to_string()
    } else {
        format!("{s:?} ({} bytes)", end)
    }
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
    let mut sig = SignatureData::default();
    let rc = get_sig(&mut sig);
    println!("\nNP_GetSignature -> {rc}");
    println!("  DllSignature: {}", show(&sig.dll_signature));
    println!("  AppSignature: {}", show(&sig.app_signature));

    // Drive the normal startup sequence a game performs.
    let version: extern "system" fn(*mut u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_QueryVersion")) };
    let mut v: u16 = 0;
    println!("NP_QueryVersion -> {} (version {v:#06x})", version(&mut v));

    let reg_win: extern "system" fn(*mut c_void) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RegisterWindowHandle")) };
    println!("NP_RegisterWindowHandle -> {}", reg_win(std::ptr::null_mut()));

    let reg_id: extern "system" fn(u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RegisterProgramProfileID")) };
    println!("NP_RegisterProgramProfileID(13302) -> {}", reg_id(13302));

    let request: extern "system" fn(u16) -> i32 =
        unsafe { std::mem::transmute(proc("NP_RequestData")) };
    println!("NP_RequestData(0x00ff) -> {}", request(0x00ff));

    let start: extern "system" fn() -> i32 =
        unsafe { std::mem::transmute(proc("NP_StartDataTransmission")) };
    println!("NP_StartDataTransmission -> {}", start());

    let get_data: extern "system" fn(*mut TrackIrData) -> i32 =
        unsafe { std::mem::transmute(proc("NP_GetData")) };

    println!("\nsampling NP_GetData:");
    let mut sigs = Vec::new();
    for i in 0..5 {
        let mut d = TrackIrData::default();
        let rc = get_data(&mut d);
        println!(
            "  {i}: rc={rc} status={} frame={} yaw={:.1} pitch={:.1} roll={:.1} \
             x={:.1} y={:.1} z={:.1}",
            d.wNPStatus, d.wPFrameSignature, d.fNPYaw, d.fNPPitch, d.fNPRoll, d.fNPX, d.fNPY, d.fNPZ
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

//! A stand-in for a game: load `freetrackclient64.dll` by path, resolve its
//! exports by name, and call them.
//!
//! Deliberately goes through `LoadLibrary` + `GetProcAddress` rather than
//! linking the crate, because that is the path a real game takes — it resolves
//! the DLL from a registry-supplied directory and looks its functions up by
//! name. Linking directly would test the Rust functions while saying nothing
//! about whether the export table a game reads is correct.
//!
//! Built on demand, not shipped:
//! `cargo build --release --target x86_64-pc-windows-gnu --example ftprobe`

use std::ffi::c_void;

type HMODULE = *mut c_void;
type FARPROC = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const u8) -> HMODULE;
    fn GetProcAddress(module: HMODULE, name: *const u8) -> FARPROC;
    fn GetLastError() -> u32;
}

/// Mirrors the published `FTData` layout.
#[repr(C)]
#[derive(Default, Debug)]
struct FtData {
    data_id: u32,
    cam_width: i32,
    cam_height: i32,
    yaw: f32,
    pitch: f32,
    roll: f32,
    x: f32,
    y: f32,
    z: f32,
    raw_yaw: f32,
    raw_pitch: f32,
    raw_roll: f32,
    raw_x: f32,
    raw_y: f32,
    raw_z: f32,
    points: [f32; 8],
}

fn main() {
    let dll = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\tobii-bridge\freetrackclient64.dll".to_string());
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

    let mut missing = 0;
    for export in [
        "FTGetData",
        "FTGetDllVersion",
        "FTProvider",
        "FTReportID",
        "FTReportName",
    ] {
        let mut n = export.as_bytes().to_vec();
        n.push(0);
        let addr = unsafe { GetProcAddress(module, n.as_ptr()) };
        if addr.is_null() {
            println!("  MISSING export {export}");
            missing += 1;
        } else {
            println!("  found {export}");
        }
    }
    if missing > 0 {
        println!("FAIL: {missing} export(s) missing");
        std::process::exit(1);
    }

    let cstr = |p: *const u8| unsafe {
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
    };

    let version: extern "system" fn() -> *const u8 = unsafe {
        std::mem::transmute(GetProcAddress(module, b"FTGetDllVersion\0".as_ptr()))
    };
    let provider: extern "system" fn() -> *const u8 =
        unsafe { std::mem::transmute(GetProcAddress(module, b"FTProvider\0".as_ptr())) };
    println!("version={} provider={}", cstr(version()), cstr(provider()));

    let get_data: extern "system" fn(*mut FtData) -> bool =
        unsafe { std::mem::transmute(GetProcAddress(module, b"FTGetData\0".as_ptr())) };

    // Sample a few times so a moving DataID is visible — that is what a game
    // watches to tell a live feed from a frozen one.
    let mut seen_ids = Vec::new();
    for i in 0..5 {
        let mut d = FtData::default();
        let ok = get_data(&mut d);
        if !ok {
            println!("sample {i}: FTGetData returned false (no provider running?)");
        } else {
            println!(
                "sample {i}: id={} yaw={:.4} pitch={:.4} roll={:.4} pos=({:.1}, {:.1}, {:.1})",
                d.data_id, d.yaw, d.pitch, d.roll, d.x, d.y, d.z
            );
            seen_ids.push(d.data_id);
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    if seen_ids.len() >= 2 && seen_ids.windows(2).any(|w| w[0] != w[1]) {
        println!("PASS: DataID advanced — the feed is live");
    } else if !seen_ids.is_empty() {
        println!("NOTE: DataID never changed; is anything sending to the provider?");
    }
}

//! The handful of Windows calls this bridge needs, declared by hand.
//!
//! No `windows-sys` or `winapi` crate: this workspace has two third-party
//! dependencies in total and a deliberate habit of not adding more, and the
//! surface used here is about fifteen functions that have been ABI-stable since
//! the 1990s. Declaring them costs less than the dependency does.
//!
//! Everything is `unsafe` at the boundary and wrapped in a safe API below, so
//! the rest of the bridge never writes an `unsafe` block.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type HKEY = *mut c_void;
pub type BOOL = i32;
pub type DWORD = u32;
pub type LONG = i32;
pub type LPCSTR = *const u8;

pub const INVALID_HANDLE_VALUE: HANDLE = usize::MAX as HANDLE;
pub const NULL: HANDLE = std::ptr::null_mut();

pub const PAGE_READWRITE: DWORD = 0x04;
pub const FILE_MAP_ALL_ACCESS: DWORD = 0x000F001F;
pub const FILE_MAP_READ: DWORD = 0x0004;

pub const WAIT_OBJECT_0: DWORD = 0;
pub const WAIT_ABANDONED: DWORD = 0x80;
pub const INFINITE: DWORD = 0xFFFF_FFFF;

pub const HKEY_CURRENT_USER: HKEY = 0x8000_0001u32 as usize as HKEY;
pub const KEY_WRITE: DWORD = 0x20006;
pub const REG_SZ: DWORD = 1;
pub const ERROR_SUCCESS: LONG = 0;

extern "system" {
    pub fn CreateFileMappingA(
        hFile: HANDLE,
        lpAttributes: *mut c_void,
        flProtect: DWORD,
        dwMaximumSizeHigh: DWORD,
        dwMaximumSizeLow: DWORD,
        lpName: LPCSTR,
    ) -> HANDLE;
    pub fn OpenFileMappingA(dwDesiredAccess: DWORD, bInheritHandle: BOOL, lpName: LPCSTR) -> HANDLE;
    pub fn MapViewOfFile(
        hFileMappingObject: HANDLE,
        dwDesiredAccess: DWORD,
        dwFileOffsetHigh: DWORD,
        dwFileOffsetLow: DWORD,
        dwNumberOfBytesToMap: usize,
    ) -> *mut c_void;
    pub fn UnmapViewOfFile(lpBaseAddress: *const c_void) -> BOOL;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;

    pub fn CreateMutexA(
        lpMutexAttributes: *mut c_void,
        bInitialOwner: BOOL,
        lpName: LPCSTR,
    ) -> HANDLE;
    pub fn OpenMutexA(dwDesiredAccess: DWORD, bInheritHandle: BOOL, lpName: LPCSTR) -> HANDLE;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
    pub fn ReleaseMutex(hMutex: HANDLE) -> BOOL;

    pub fn GetLastError() -> DWORD;
}

// The registry lives in advapi32, which — unlike kernel32 — is not in the
// default link set, so it has to be named explicitly.
#[link(name = "advapi32")]
extern "system" {
    pub fn RegCreateKeyExA(
        hKey: HKEY,
        lpSubKey: LPCSTR,
        Reserved: DWORD,
        lpClass: LPCSTR,
        dwOptions: DWORD,
        samDesired: DWORD,
        lpSecurityAttributes: *mut c_void,
        phkResult: *mut HKEY,
        lpdwDisposition: *mut DWORD,
    ) -> LONG;
    pub fn RegSetValueExA(
        hKey: HKEY,
        lpValueName: LPCSTR,
        Reserved: DWORD,
        dwType: DWORD,
        lpData: *const u8,
        cbData: DWORD,
    ) -> LONG;
    pub fn RegCloseKey(hKey: HKEY) -> LONG;
    pub fn RegDeleteKeyA(hKey: HKEY, lpSubKey: LPCSTR) -> LONG;
}

/// The `MUTEX_ALL_ACCESS` right, for opening an existing mutex.
pub const MUTEX_ALL_ACCESS: DWORD = 0x001F_0001;

/// A NUL-terminated copy of `s`, for the `*A` entry points.
pub fn cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// A Windows handle that closes itself.
pub struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

// The bridge is single-threaded, but a DLL's exports are called from whatever
// thread the game happens to be on. Windows handles are process-wide and these
// are only ever read after construction.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

/// Write a `REG_SZ` value under `HKEY_CURRENT_USER`.
///
/// Games find their tracking client by reading a path out of the registry, so
/// this is how a game is told where our DLLs live.
pub fn set_hkcu_string(subkey: &str, value_name: &str, data: &str) -> Result<(), DWORD> {
    let sub = cstr(subkey);
    let name = cstr(value_name);
    let payload = cstr(data); // REG_SZ includes its NUL in cbData
    let mut key: HKEY = std::ptr::null_mut();
    unsafe {
        let rc = RegCreateKeyExA(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null_mut(),
            &mut key,
            std::ptr::null_mut(),
        );
        if rc != ERROR_SUCCESS {
            return Err(rc as DWORD);
        }
        let rc = RegSetValueExA(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            payload.as_ptr(),
            payload.len() as DWORD,
        );
        RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return Err(rc as DWORD);
        }
    }
    Ok(())
}

/// Delete a key under `HKEY_CURRENT_USER`. Missing is not an error.
pub fn delete_hkcu_key(subkey: &str) {
    let sub = cstr(subkey);
    unsafe { RegDeleteKeyA(HKEY_CURRENT_USER, sub.as_ptr()) };
}

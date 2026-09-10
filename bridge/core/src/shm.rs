//! The `FT_SharedMem` mapping: one provider, several consumers.
//!
//! The **exe is the sole provider**. It creates the mapping, owns the mutex and
//! writes frames. The DLLs are pure consumers that open the mapping read-only.
//!
//! Deliberately *not* letting the DLLs host their own provider. It would double
//! the state space, and a stale in-process provider racing a real one is a nasty
//! failure mode to debug from inside a game — the view would half-work,
//! intermittently, with two writers disagreeing about `DataID`.

use std::sync::atomic::{AtomicU32, Ordering};

use tobii_output::freetrack::{FT_HEAP_LEN, FT_MUTEX_NAME, FT_SHARED_MEM_NAME};

use crate::winapi::*;

/// Writes tracking data into the shared mapping.
pub struct Provider {
    _mapping: OwnedHandle,
    view: *mut u8,
    mutex: OwnedHandle,
    data_id: AtomicU32,
}

// The view is process-wide and only written from the provider's own loop.
unsafe impl Send for Provider {}
unsafe impl Sync for Provider {}

impl Provider {
    /// Create the mapping and mutex a game's client DLL will look for.
    ///
    /// Reuses them if they already exist: `CreateFileMappingA` on an existing
    /// name returns a handle to it rather than failing, so restarting the
    /// bridge while a game holds the mapping open keeps working.
    pub fn create() -> Result<Provider, String> {
        let name = cstr(FT_SHARED_MEM_NAME);
        let mutex_name = cstr(FT_MUTEX_NAME);
        unsafe {
            let mapping = CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                std::ptr::null_mut(),
                PAGE_READWRITE,
                0,
                FT_HEAP_LEN as DWORD,
                name.as_ptr(),
            );
            if mapping.is_null() {
                return Err(format!(
                    "CreateFileMapping({FT_SHARED_MEM_NAME}) failed: {}",
                    GetLastError()
                ));
            }
            let mapping = OwnedHandle(mapping);
            let view = MapViewOfFile(mapping.0, FILE_MAP_ALL_ACCESS, 0, 0, FT_HEAP_LEN);
            if view.is_null() {
                return Err(format!("MapViewOfFile failed: {}", GetLastError()));
            }
            let mutex = CreateMutexA(std::ptr::null_mut(), 0, mutex_name.as_ptr());
            if mutex.is_null() {
                return Err(format!(
                    "CreateMutex({FT_MUTEX_NAME}) failed: {}",
                    GetLastError()
                ));
            }
            Ok(Provider {
                _mapping: mapping,
                view: view.cast::<u8>(),
                mutex: OwnedHandle(mutex),
                data_id: AtomicU32::new(0),
            })
        }
    }

    /// Publish one frame.
    ///
    /// `DataID` advances on every call, because that is what consumers watch to
    /// tell a new frame from a repeat — a counter that stuck would read to a
    /// game as "the tracker stopped" even while frames kept arriving.
    pub fn publish(&self, frame: &tobii_output::TrackingFrame, game_id: i32) {
        let next = tobii_output::freetrack::next_data_id(self.data_id.load(Ordering::Relaxed));
        self.data_id.store(next, Ordering::Relaxed);
        let bytes = tobii_output::freetrack::ft_heap_bytes(frame, next, game_id);
        unsafe {
            // Hold the mutex only for the memcpy. A consumer that blocks here
            // blocks the game's render thread, so this must stay the shortest
            // possible critical section.
            let waited = WaitForSingleObject(self.mutex.0, INFINITE);
            if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.view, FT_HEAP_LEN);
                ReleaseMutex(self.mutex.0);
            }
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        unsafe { UnmapViewOfFile(self.view.cast()) };
    }
}

/// Reads tracking data out of the shared mapping.
pub struct Consumer {
    _mapping: OwnedHandle,
    view: *const u8,
    mutex: OwnedHandle,
}

unsafe impl Send for Consumer {}
unsafe impl Sync for Consumer {}

impl Consumer {
    /// Open the provider's mapping, or `None` if no provider is running.
    ///
    /// Absent is the ordinary case — the game may well start before the
    /// bridge — so it is not an error, and the exports above simply report
    /// no data until it appears.
    pub fn open() -> Option<Consumer> {
        let name = cstr(FT_SHARED_MEM_NAME);
        let mutex_name = cstr(FT_MUTEX_NAME);
        unsafe {
            let mapping = OpenFileMappingA(FILE_MAP_READ, 0, name.as_ptr());
            if mapping.is_null() {
                return None;
            }
            let mapping = OwnedHandle(mapping);
            let view = MapViewOfFile(mapping.0, FILE_MAP_READ, 0, 0, FT_HEAP_LEN);
            if view.is_null() {
                return None;
            }
            // A missing mutex is survivable: reading a 108-byte structure that
            // is rewritten wholesale is at worst one torn frame, and refusing
            // to run at all would be the worse failure.
            let mutex = OpenMutexA(MUTEX_ALL_ACCESS, 0, mutex_name.as_ptr());
            Some(Consumer {
                _mapping: mapping,
                view: view.cast::<u8>(),
                mutex: OwnedHandle(mutex),
            })
        }
    }

    /// Take a copy of the current `FTHeap`.
    pub fn read(&self) -> [u8; FT_HEAP_LEN] {
        let mut out = [0u8; FT_HEAP_LEN];
        unsafe {
            let held = !self.mutex.0.is_null()
                && matches!(
                    WaitForSingleObject(self.mutex.0, 16),
                    WAIT_OBJECT_0 | WAIT_ABANDONED
                );
            std::ptr::copy_nonoverlapping(self.view, out.as_mut_ptr(), FT_HEAP_LEN);
            if held {
                ReleaseMutex(self.mutex.0);
            }
        }
        out
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        unsafe { UnmapViewOfFile(self.view.cast()) };
    }
}

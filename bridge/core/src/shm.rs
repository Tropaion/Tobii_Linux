//! The `FT_SharedMem` mapping: one provider, several consumers.
//!
//! Exactly one process per wineserver session creates the mapping, owns the
//! mutex and writes frames; everyone else opens it read-only.
//!
//! # The provider used to have to be the exe, and no longer is
//!
//! This said "deliberately *not* letting the DLLs host their own provider — a
//! stale in-process provider racing a real one is a nasty failure mode to debug
//! from inside a game". The concern was right; the conclusion did not survive
//! contact with Proton, where a separate exe cannot reach the game's wineserver
//! session at all. So the DLLs do host a provider now — see [`crate::feeder`] —
//! and the race the old comment feared is what that module's port rule exists
//! to prevent, having already been caught happening once.
//!
//! Two writers can still never disagree about `DataID`, because two writers
//! cannot exist: creating the mapping is gated on winning a host-wide UDP bind,
//! and ceding it is gated on being able to open somebody else's mapping — which
//! is only possible within one session.

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
    /// Open the mapping, or `None` if this session has none.
    ///
    /// Absent is not an error. It is also how [`crate::feeder`] tells a port
    /// holder in **this** wineserver session (whose mapping opens, so reading
    /// it is right) from one in another session (whose mapping is invisible
    /// here, so standing down would leave the game with nothing).
    ///
    /// Note that opening successfully is not the same as having data: the
    /// feeder creates the mapping before any frame arrives, so a fresh one is
    /// all zeroes. `DataID == 0` is reserved to mean exactly that — see
    /// [`tobii_output::freetrack::next_data_id`] — and the client exports check
    /// it before reporting a frame, or a game would see a live tracker sitting
    /// at dead centre.
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

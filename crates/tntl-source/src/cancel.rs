//! Cooperative cancellation for downloads.
//!
//! The desktop shell runs one install at a time and users press one "cancel"
//! button, so a process-wide flag is enough. Callers that never cancel (the
//! CLI) leave it untouched. Every install command in the shell calls
//! [`begin`] first and keeps the returned guard alive, so a cancel only
//! affects operations that can actually be cancelled: a request left over
//! from a finished install no longer fails the next search or skin download.
//!
//! The flag is only reset when the *last* operation finishes, so starting a
//! second job (a loader install next to a game install, or a repair started
//! from the CLI) cannot resurrect a job the user just cancelled. One cancel
//! button stops every operation that is running at that moment.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::SourceError;

static CANCELLED: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Marks one cancellable operation for as long as it is alive.
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        if ACTIVE.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last one out clears the flag, so a cancel pressed after the job
            // already stopped cannot leak into the next unrelated download.
            CANCELLED.store(false, Ordering::SeqCst);
        }
    }
}

/// Mark one cancellable operation. A cancel requested before the first one
/// starts is dropped; a cancel requested while any operation is live stays.
pub fn begin() -> Guard {
    if ACTIVE.fetch_add(1, Ordering::SeqCst) == 0 {
        CANCELLED.store(false, Ordering::SeqCst);
    }
    Guard(())
}

/// Ask every running operation to stop at the next chunk.
pub fn request() {
    CANCELLED.store(true, Ordering::SeqCst);
}

pub fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// True when a cancel was requested for an operation that is still running.
/// Long steps outside the download engine (Forge processors, unpacking) poll
/// this so one button stops the whole install.
pub fn cancelled() -> bool {
    is_cancelled() && ACTIVE.load(Ordering::SeqCst) > 0
}

pub(crate) fn check() -> Result<(), SourceError> {
    // Without a live cancellable operation, a stale flag must not turn
    // unrelated downloads (search, skins, imports) into "cancelled" errors.
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}

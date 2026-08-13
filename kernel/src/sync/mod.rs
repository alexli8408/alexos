//! Kernel synchronisation primitives.
//!
//! Two tiers, and picking the wrong one is a bug:
//!
//! * [`SpinLock`] masks interrupts and busy-waits. It is the only kind of lock
//!   an interrupt handler may take, and the only kind usable before the
//!   scheduler exists.
//! * [`Mutex`] and [`Semaphore`] park the calling task on a [`WaitQueue`].
//!   They must never be taken from interrupt context, because there is no task
//!   there to park.

pub mod spin;
pub mod wait;

pub use spin::{SpinLock, SpinLockGuard};
pub use wait::{Mutex, MutexGuard, Semaphore, WaitQueue};

/// A value initialised exactly once at boot and read freely thereafter.
///
/// This exists because a lot of kernel state -- the frame allocator's range,
/// the timer frequency, the virtio device list -- is written by hart 0 during
/// init and is immutable afterwards. Wrapping it in a lock would mean paying
/// for atomics on every read forever to protect against a race that only
/// exists for a few hundred microseconds at boot.
pub struct Once<T> {
    inner: SpinLock<Option<T>>,
}

impl<T> Once<T> {
    /// Create an empty cell.
    pub const fn new() -> Self {
        Self { inner: SpinLock::new(None) }
    }

    /// Store `value`, panicking if the cell was already populated.
    pub fn init(&self, value: T) {
        let mut slot = self.inner.lock();
        assert!(slot.is_none(), "Once::init called twice");
        *slot = Some(value);
    }

    /// Has this cell been initialised?
    pub fn is_initialized(&self) -> bool {
        self.inner.lock().is_some()
    }
}

impl<T: Copy> Once<T> {
    /// Read the value, panicking if `init` has not run yet.
    pub fn get(&self) -> T {
        self.inner.lock().expect("Once read before init")
    }
}

impl<T> Default for Once<T> {
    fn default() -> Self {
        Self::new()
    }
}

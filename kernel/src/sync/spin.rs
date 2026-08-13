//! Interrupt-safe spin lock.
//!
//! Kernel locks are taken from both thread context and interrupt handlers. If a
//! hart held a lock with interrupts enabled and the timer fired, the handler
//! could try to take the same lock and deadlock against itself. So acquiring a
//! `SpinLock` masks interrupts on the local hart for the whole critical
//! section, and the guard restores the previous state on drop.
//!
//! Masking must happen *before* the atomic swap and unmasking *after* the
//! release, otherwise there is a window where the lock is held with interrupts
//! live -- the exact race this type exists to prevent.

use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;

/// A mutual-exclusion primitive that spins rather than sleeping.
///
/// Correct for short critical sections and for anything reachable from an
/// interrupt handler. Code that may block while holding the lock wants
/// [`crate::sync::Mutex`] instead, which parks the task.
pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

/// RAII guard: derefs to the protected data, releases on drop.
pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    /// Whether interrupts were enabled before this guard masked them.
    restore_intr: bool,
}

// SAFETY: the lock serialises all access to `data`, so sharing a `&SpinLock<T>`
// across harts is sound whenever `T` can be moved between them.
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Create an unlocked spin lock. `const` so it can initialise a static.
    pub const fn new(data: T) -> Self {
        Self { locked: AtomicBool::new(false), data: UnsafeCell::new(data) }
    }

    /// Consume the lock and return the protected value.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Block until the lock is acquired.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let restore_intr = arch::intr_enabled();
        if restore_intr {
            // SAFETY: re-enabled by SpinLockGuard::drop on every path.
            unsafe { arch::intr_disable() };
        }

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin on a plain load first. `compare_exchange` needs exclusive
            // ownership of the cache line, so hammering it would bounce the
            // line between harts; a relaxed read lets us wait shared.
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }

        SpinLockGuard { lock: self, restore_intr }
    }

    /// Acquire the lock if it is free, otherwise return `None` immediately.
    ///
    /// Used by the panic path, which must not deadlock on a lock whose holder
    /// is the thread that just panicked.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let restore_intr = arch::intr_enabled();
        if restore_intr {
            // SAFETY: restored either by the guard or on the failure path below.
            unsafe { arch::intr_disable() };
        }

        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard { lock: self, restore_intr })
        } else {
            if restore_intr {
                // SAFETY: we never took the lock, so nothing is left inconsistent.
                unsafe { arch::intr_enable() };
            }
            None
        }
    }

    /// Is the lock currently held? Advisory only -- the answer may be stale
    /// before it is returned. Intended for assertions.
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// Get a mutable reference without locking, proven safe by `&mut self`.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Release the lock without going through a guard.
    ///
    /// # Safety
    /// Only sound when the caller holds the lock and has arranged for the
    /// guard not to also release it. Used by the scheduler, which drops a lock
    /// after switching onto a different stack.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard proves exclusive access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: holding the guard proves exclusive access.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        // Release ordering publishes every write made inside the section to the
        // next hart that acquires.
        self.lock.locked.store(false, Ordering::Release);
        if self.restore_intr {
            // SAFETY: restoring the interrupt state this guard masked.
            unsafe { arch::intr_enable() };
        }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for SpinLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("SpinLock").field("data", &&*guard).finish(),
            None => f.write_str("SpinLock { <locked> }"),
        }
    }
}

impl<T: Default> Default for SpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

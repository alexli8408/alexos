//! Wait queues and the blocking primitives built on them.
//!
//! A [`SpinLock`] busy-waits, which is right for a critical section a few
//! instructions long and wrong for anything that waits on a device or another
//! task -- burning a whole hart while a disk read completes is not a tradeoff,
//! it is a bug. These primitives park the caller instead.
//!
//! **None of these may be used from an interrupt handler.** There is no task to
//! park in that context, so blocking there would suspend whatever task happened
//! to be interrupted, which is not the one that wanted to wait.
//!
//! The classic hazard is the lost wakeup: a waiter that has queued itself but
//! not yet switched away receives a wakeup that goes nowhere, and sleeps
//! forever. That is closed in the scheduler, where `wake` records the wakeup on
//! the task and the next attempt to block consumes it. Callers still have to
//! re-check their condition in a loop, because a consumed wakeup surfaces as a
//! spurious one.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::SpinLock;
use crate::task::Task;
use crate::task::scheduler::{self, current_task};

/// A set of tasks waiting for an event.
pub struct WaitQueue {
    waiters: SpinLock<VecDeque<Arc<Task>>>,
}

impl WaitQueue {
    /// An empty queue.
    pub const fn new() -> Self {
        Self { waiters: SpinLock::new(VecDeque::new()) }
    }

    /// Block the calling task until someone wakes this queue.
    ///
    /// Returns on a spurious wakeup as well as a real one, so every caller must
    /// re-check its condition. [`Self::wait_until`] does that for you.
    pub fn wait(&self) {
        let task = current_task();
        self.waiters.lock().push_back(task.clone());

        scheduler::block_current();

        // Drop our entry if a wakeup left it behind -- otherwise a later
        // `wake_one` could spend itself on a task that is no longer waiting,
        // and the task that *is* waiting would never be woken.
        let mut waiters = self.waiters.lock();
        if let Some(pos) = waiters.iter().position(|t| Arc::ptr_eq(t, &task)) {
            waiters.remove(pos);
        }
    }

    /// Block until `condition` holds.
    ///
    /// The condition is checked before waiting at all, so a caller that is
    /// already satisfied never sleeps.
    pub fn wait_until(&self, mut condition: impl FnMut() -> bool) {
        while !condition() {
            self.wait();
        }
    }

    /// Wake the task at the head of the queue, if any.
    pub fn wake_one(&self) {
        let waiter = self.waiters.lock().pop_front();
        if let Some(task) = waiter {
            scheduler::wake(&task);
        }
    }

    /// Wake every waiter.
    ///
    /// The queue is drained under the lock and the wakeups happen after it is
    /// released, so a woken task that immediately re-waits does not deadlock
    /// against the waker.
    pub fn wake_all(&self) {
        let waiters: VecDeque<_> = core::mem::take(&mut *self.waiters.lock());
        for task in &waiters {
            scheduler::wake(task);
        }
    }

    /// Number of tasks currently waiting.
    pub fn len(&self) -> usize {
        self.waiters.lock().len()
    }

    /// Is nobody waiting?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A mutual-exclusion lock that parks the caller instead of spinning.
///
/// Use this whenever the critical section can block, take a page fault, or wait
/// on a device -- the filesystem and the block cache, mainly. For anything an
/// interrupt handler touches, use [`SpinLock`].
pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    waiters: WaitQueue,
    data: UnsafeCell<T>,
}

/// RAII guard for [`Mutex`].
pub struct MutexGuard<'a, T: ?Sized> {
    lock: &'a Mutex<T>,
}

// SAFETY: the lock serialises all access to `data`.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create an unlocked mutex.
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            waiters: WaitQueue::new(),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquire the lock, blocking the task until it is free.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return MutexGuard { lock: self };
            }
            // Re-checking inside `wait_until` is what makes the race between
            // this check and the wait harmless.
            self.waiters.wait_until(|| !self.locked.load(Ordering::Relaxed));
        }
    }

    /// Acquire the lock if it is free, without blocking.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { lock: self })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard proves exclusive access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: holding the guard proves exclusive access.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        // Wake one waiter rather than all: a broadcast would have every waiter
        // race for a lock only one can win, and the losers would go straight
        // back to sleep having accomplished nothing.
        self.lock.waiters.wake_one();
    }
}

/// A counting semaphore.
pub struct Semaphore {
    count: SpinLock<isize>,
    waiters: WaitQueue,
}

impl Semaphore {
    /// A semaphore with `initial` permits.
    pub const fn new(initial: isize) -> Self {
        Self { count: SpinLock::new(initial), waiters: WaitQueue::new() }
    }

    /// Take a permit, blocking until one is available.
    pub fn acquire(&self) {
        loop {
            {
                let mut count = self.count.lock();
                if *count > 0 {
                    *count -= 1;
                    return;
                }
            }
            self.waiters.wait_until(|| *self.count.lock() > 0);
        }
    }

    /// Return a permit and wake a waiter.
    pub fn release(&self) {
        *self.count.lock() += 1;
        self.waiters.wake_one();
    }

    /// Permits currently available.
    pub fn available(&self) -> isize {
        *self.count.lock()
    }
}

//! Timer interrupts: the heartbeat that makes scheduling preemptive.
//!
//! RISC-V gives S-mode a read-only `time` counter and no way to program the
//! comparator directly -- `stimecmp` lives in M-mode. So the kernel asks
//! firmware, via the SBI TIME extension, to deliver the next interrupt at an
//! absolute time. There is no periodic mode and no way to cancel, only "wake
//! me at T", which means each tick must arm the next one before returning.
//!
//! Arming from the *previous deadline* rather than from "now" matters: if the
//! handler is delayed, computing `now + interval` would let the period drift
//! outward every time. Advancing the deadline keeps the average rate exact.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch;
use crate::config::{TICK_MS, TIMER_FREQ};
use crate::sbi;

/// Timer ticks since boot.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Absolute `time` value the next interrupt is armed for.
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(0);

/// `time` units in one scheduling quantum.
const fn interval() -> u64 {
    TIMER_FREQ / 1000 * TICK_MS
}

/// Arm the first timer interrupt.
pub fn init() {
    let deadline = arch::read_time() + interval();
    NEXT_DEADLINE.store(deadline, Ordering::Relaxed);
    sbi::set_timer(deadline);
    crate::info!("timer: {TICK_MS} ms quantum ({} Hz)", 1000 / TICK_MS);
}

/// Called from the trap handler on every timer interrupt.
pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);

    // Account the quantum against the running task. This only sets a flag; the
    // switch happens on the way out of the trap, where the stack is safe to
    // leave. See scheduler::resched_if_needed.
    crate::task::scheduler::on_timer_tick();

    // Advance the deadline rather than recomputing from now, so a late handler
    // does not stretch the period permanently. If the kernel fell so far behind
    // that the new deadline is already in the past, skip forward to avoid a
    // storm of back-to-back interrupts.
    let now = arch::read_time();
    let mut next = NEXT_DEADLINE.load(Ordering::Relaxed) + interval();
    if next <= now {
        next = now + interval();
    }
    NEXT_DEADLINE.store(next, Ordering::Relaxed);
    sbi::set_timer(next);
}

/// Ticks since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Milliseconds since boot, derived from the hardware counter rather than the
/// tick count so it stays accurate even if interrupts were masked for a while.
pub fn uptime_ms() -> u64 {
    arch::read_time() * 1000 / TIMER_FREQ
}

/// Busy-wait for `ms` milliseconds.
///
/// Only for device bring-up, where there is no task to put to sleep yet.
pub fn spin_delay_ms(ms: u64) {
    let target = arch::read_time() + TIMER_FREQ / 1000 * ms;
    while arch::read_time() < target {
        core::hint::spin_loop();
    }
}

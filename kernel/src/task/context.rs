//! Saved state for a context switch, and the kernel stacks tasks run on.

use core::arch::global_asm;

use crate::config::{KERNEL_STACK_SIZE, PAGE_SIZE};
use crate::mm::frame::{Frame, order_for};

global_asm!(include_str!("switch.S"));

unsafe extern "C" {
    /// Save the current callee-saved registers into `old` and load `new`.
    ///
    /// Returns into `new`'s `ra`, so from the caller's perspective this
    /// function returns only when someone switches *back* to it.
    pub fn switch_context(old: *mut TaskContext, new: *const TaskContext);
}

/// Registers a context switch must preserve.
///
/// Just the callee-saved set. Everything else is dead at a call boundary by the
/// ABI, and a switch is a call as far as the compiler is concerned -- which is
/// why this is 14 registers and a trap frame is 34.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct TaskContext {
    /// Where execution resumes. Offset 0 in switch.S.
    pub ra: usize,
    /// Kernel stack pointer. Offset 8.
    pub sp: usize,
    /// Callee-saved `s0` through `s11`. Offsets 16 through 104.
    pub s: [usize; 12],
}

impl TaskContext {
    /// A zeroed context, for a hart's idle loop: the first `switch_context`
    /// into it will overwrite everything with real values.
    pub const fn zeroed() -> Self {
        Self { ra: 0, sp: 0, s: [0; 12] }
    }

    /// Build the context a never-yet-run task starts from.
    ///
    /// `ra` points at the entry trampoline rather than at a return address, so
    /// the `ret` at the end of `switch_context` lands there.
    pub fn new(entry: usize, stack_top: usize) -> Self {
        Self { ra: entry, sp: stack_top, s: [0; 12] }
    }
}

/// A task's kernel stack.
///
/// Taken from the buddy allocator as one contiguous run, so it is reachable
/// through the linear map with no extra mapping.
///
/// There is no guard page. The linear map is built from 2 MiB superpages, and
/// punching a 4 KiB hole in one would mean splitting it and giving up the TLB
/// benefit for every kernel stack. Instead the low end of the stack holds a
/// canary that the scheduler checks on every switch: that catches an overflow
/// one quantum late rather than instantly, but it catches it with a clear
/// message instead of silent corruption of whatever sits below.
pub struct KernelStack {
    frames: Frame,
}

/// Value written at the low end of every kernel stack.
const STACK_CANARY: usize = 0xC0DE_CAFE_DEAD_BEEF;

impl KernelStack {
    /// Allocate a stack.
    pub fn new() -> Option<Self> {
        let order = order_for(KERNEL_STACK_SIZE / PAGE_SIZE);
        let frames = Frame::alloc_order(order)?;
        let stack = Self { frames };
        // SAFETY: we own the run and the canary word is inside it.
        unsafe { (stack.bottom() as *mut usize).write(STACK_CANARY) };
        Some(stack)
    }

    /// Lowest address of the stack -- where the canary lives, and the point
    /// past which an overflow starts destroying other allocations.
    pub fn bottom(&self) -> usize {
        self.frames.ppn().as_ptr() as usize
    }

    /// Initial stack pointer: one past the highest byte, since the stack grows
    /// down and RISC-V pushes before storing.
    pub fn top(&self) -> usize {
        self.bottom() + self.frames.count() * PAGE_SIZE
    }

    /// Has the stack overflowed past its low end?
    pub fn canary_intact(&self) -> bool {
        // SAFETY: the canary address is inside a run this object owns.
        unsafe { (self.bottom() as *const usize).read() == STACK_CANARY }
    }

    /// Bytes of stack still unused below `sp`. Reported by `ps`.
    pub fn headroom(&self, sp: usize) -> usize {
        sp.saturating_sub(self.bottom())
    }
}

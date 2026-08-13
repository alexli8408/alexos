//! User processes: creation, `fork`, `exec`, and the program break.
//!
//! A user task is a kernel task with three extra things: an address space, a
//! trap frame that lives in the kernel heap rather than on the stack, and a
//! parent/child relationship so `wait` has something to wait for.
//!
//! The trap frame is heap-allocated deliberately. `sscratch` has to hold its
//! address the whole time user code runs, and a frame on the kernel stack would
//! move every time the task was rescheduled.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::config::{PAGE_SIZE, USER_HEAP_BASE, USER_MAX_ADDR, USER_STACK_SIZE, USER_STACK_TOP};
use crate::loader;
use crate::mm::addr::VirtAddr;
use crate::mm::page_table::PteFlags;
use crate::mm::space::{AddressSpace, Backing, Region};
use crate::task::scheduler::current_task;
use crate::task::{Task, TaskInner, TaskState, alloc_pid, enter_user_mode};
use crate::trap::TrapFrame;

/// Build the user stack region and return the initial stack pointer.
fn map_user_stack(space: &mut AddressSpace) -> Option<usize> {
    let top = VirtAddr(USER_STACK_TOP);
    let bottom = VirtAddr(USER_STACK_TOP - USER_STACK_SIZE);
    space.push(Region::new(bottom, top, Backing::Framed, PteFlags::RW | PteFlags::USER)).ok()?;
    Some(USER_STACK_TOP)
}

/// Copy `args` onto the user stack and return `(new_sp, argc, argv_ptr)`.
///
/// The layout is the conventional one: the strings themselves at the top of the
/// stack, then a NULL-terminated array of pointers to them, with everything
/// 16-byte aligned because the RISC-V ABI requires it at function entry.
fn push_args(
    space: &AddressSpace,
    mut sp: usize,
    args: &[String],
) -> Option<(usize, usize, usize)> {
    let mut pointers = Vec::with_capacity(args.len());

    for arg in args {
        let len = arg.len() + 1; // include the NUL
        sp -= len;
        sp &= !0x7;
        space.copy_to_user(VirtAddr(sp), arg.as_bytes())?;
        space.copy_to_user(VirtAddr(sp + arg.len()), &[0u8])?;
        pointers.push(sp);
    }

    // The pointer array, NULL-terminated.
    sp -= (pointers.len() + 1) * 8;
    sp &= !0xf;
    let argv = sp;
    for (i, &p) in pointers.iter().enumerate() {
        space.copy_to_user(VirtAddr(argv + i * 8), &p.to_le_bytes())?;
    }
    space.copy_to_user(VirtAddr(argv + pointers.len() * 8), &0usize.to_le_bytes())?;

    Some((sp, pointers.len(), argv))
}

impl Task {
    /// Create a user process from an ELF image.
    pub fn new_user(name: &str, image: &[u8], args: &[String]) -> Option<Arc<Self>> {
        let loaded = loader::load(image).ok()?;
        let mut space = loaded.space;

        let stack_top = map_user_stack(&mut space)?;
        let (sp, argc, argv) = push_args(&space, stack_top, args)?;

        let kstack = crate::task::KernelStack::new()?;
        let kernel_sp = kstack.top();
        let kernel_satp = crate::mm::space::kernel_token();

        let mut frame = Box::new(TrapFrame::new_user(loaded.entry, sp, kernel_sp, kernel_satp));
        frame.kernel_hartid = crate::arch::hart_id();
        // main(argc, argv) -- the user runtime forwards a0/a1 straight through.
        frame.x[10] = argc;
        frame.x[11] = argv;

        let task = Arc::new(Self {
            pid: alloc_pid(),
            name: crate::sync::SpinLock::new(String::from(name)),
            entry: enter_user_mode,
            child_exit: crate::sync::WaitQueue::new(),
            inner: crate::sync::SpinLock::new(TaskInner {
                state: TaskState::Ready,
                ctx: crate::task::TaskContext::new(crate::task::task_entry_addr(), kernel_sp),
                kstack,
                level: 0,
                ticks: 0,
                needs_resched: false,
                wakeup_pending: false,
                exit_code: 0,
                space: Some(space),
                frame: Some(frame),
                brk: loaded.brk.max(USER_HEAP_BASE),
                heap_top: loaded.brk.max(USER_HEAP_BASE),
                parent: None,
                children: Vec::new(),
            }),
        });
        Some(task)
    }

    /// Duplicate `parent` into a new process.
    ///
    /// The child's frame is a copy of the one the parent trapped with, except
    /// that `a0` is zeroed -- that difference is the entire mechanism by which
    /// the two halves of a fork discover which one they are.
    pub fn fork_from(parent: &Arc<Task>, frame: &TrapFrame) -> Option<Arc<Task>> {
        let inner = parent.inner.lock();
        let space = inner.space.as_ref()?.duplicate()?;
        let brk = inner.brk;
        let heap_top = inner.heap_top;
        drop(inner);

        let kstack = crate::task::KernelStack::new()?;
        let kernel_sp = kstack.top();

        let mut child_frame = Box::new(*frame);
        child_frame.kernel_sp = kernel_sp;
        child_frame.kernel_satp = crate::mm::space::kernel_token();
        child_frame.kernel_hartid = crate::arch::hart_id();
        child_frame.set_return(0);

        Some(Arc::new(Self {
            pid: alloc_pid(),
            name: crate::sync::SpinLock::new(parent.name.lock().clone()),
            entry: enter_user_mode,
            child_exit: crate::sync::WaitQueue::new(),
            inner: crate::sync::SpinLock::new(TaskInner {
                state: TaskState::Ready,
                ctx: crate::task::TaskContext::new(crate::task::task_entry_addr(), kernel_sp),
                kstack,
                level: 0,
                ticks: 0,
                needs_resched: false,
                wakeup_pending: false,
                exit_code: 0,
                space: Some(space),
                frame: Some(child_frame),
                brk,
                heap_top,
                parent: Some(Arc::downgrade(parent)),
                children: Vec::new(),
            }),
        }))
    }
}

/// Why an `exec` could not be completed.
///
/// Distinct from `LoadError` because the failures after a successful parse --
/// no memory for a stack, no memory for the argument vector -- are the
/// caller's problem in a different way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
    /// The image itself was rejected.
    BadImage(loader::LoadError),
    /// Ran out of memory building the new address space.
    OutOfMemory,
}

/// Replace `task`'s image with `image`, keeping its pid and its place in the
/// process tree.
///
/// On success the old address space is dropped, which frees every frame it
/// owned -- including the stack the caller's user-mode registers point into.
/// That is safe only because the caller is running on its kernel stack and its
/// user state is about to be overwritten wholesale.
pub fn exec_current(
    task: &Arc<Task>,
    name: &str,
    image: &[u8],
    args: &[String],
) -> Result<(), ExecError> {
    let loaded = loader::load(image).map_err(|e| {
        crate::warn!("exec {name}: {e:?}");
        ExecError::BadImage(e)
    })?;
    let mut space = loaded.space;

    let stack_top = map_user_stack(&mut space).ok_or(ExecError::OutOfMemory)?;
    let (sp, argc, argv) = push_args(&space, stack_top, args).ok_or(ExecError::OutOfMemory)?;

    // The process keeps its pid and its place in the tree, but it is a
    // different program now and `ps` should say so.
    *task.name.lock() = String::from(name);

    let mut inner = task.inner.lock();

    let kernel_sp = inner.kstack.top();
    let mut frame = Box::new(TrapFrame::new_user(
        loaded.entry,
        sp,
        kernel_sp,
        crate::mm::space::kernel_token(),
    ));
    frame.kernel_hartid = crate::arch::hart_id();
    frame.x[10] = argc;
    frame.x[11] = argv;

    // Install the new space before dropping the old one, so that at no point is
    // the hart running with an address space that has been freed.
    // SAFETY: the kernel half of the new space is mapped, and this task is
    // executing on its kernel stack, which lives in that half.
    unsafe { space.activate() };

    inner.brk = loaded.brk.max(USER_HEAP_BASE);
    inner.heap_top = inner.brk;
    inner.frame = Some(frame);
    let old = inner.space.replace(space);
    drop(inner);
    drop(old);

    Ok(())
}

/// Move `task`'s program break by `delta`, returning the previous break.
///
/// Growth is backed immediately rather than lazily. Demand paging would be the
/// better answer and the LAZY PTE bit is reserved for it, but a fault handler
/// that can distinguish "heap not yet backed" from "wild pointer" needs the
/// region list consulted on every fault, which is not wired up yet.
pub fn adjust_brk(task: &Arc<Task>, delta: isize) -> Option<usize> {
    let mut inner = task.inner.lock();
    let old = inner.brk;

    if delta == 0 {
        return Some(old);
    }

    let new = old.checked_add_signed(delta)?;
    if !(USER_HEAP_BASE..USER_MAX_ADDR).contains(&new) {
        return None;
    }

    if new > inner.heap_top {
        // Extend the heap region to cover the new break.
        let from = VirtAddr(inner.heap_top);
        let to = VirtAddr(new.next_multiple_of(PAGE_SIZE));
        let space = inner.space.as_mut()?;
        space.push(Region::new(from, to, Backing::Framed, PteFlags::RW | PteFlags::USER)).ok()?;
        inner.heap_top = to.0;
    }

    inner.brk = new;
    Some(old)
}

/// Reparent `task`'s children and wake its parent. Called as a process exits.
pub fn on_exit(task: &Arc<Task>) {
    let parent = {
        let inner = task.inner.lock();
        inner.parent.as_ref().and_then(|p| p.upgrade())
    };

    if let Some(parent) = parent {
        // The parent may be blocked in waitpid. It re-checks its child list on
        // wake, so a spurious wake costs nothing and a missed one hangs it.
        parent.child_exit.wake_all();
    }
}

/// The address space this task should run in, or the kernel's.
pub fn space_token(task: &Arc<Task>) -> usize {
    let inner = task.inner.lock();
    match inner.space.as_ref() {
        Some(space) => space.token(),
        None => crate::mm::space::kernel_token(),
    }
}

/// Pointer to `task`'s trap frame, for the user return path.
pub fn frame_ptr(task: &Arc<Task>) -> *mut TrapFrame {
    let mut inner = task.inner.lock();
    match inner.frame.as_mut() {
        Some(frame) => &raw mut **frame,
        None => panic!("task {} has no trap frame", task.pid),
    }
}

/// Enter user mode for the first time. Used as the entry point of a user task.
///
/// The `Arc<Task>` is scoped deliberately. `user_trap_return` never comes back,
/// so anything still live on this stack is never dropped -- and an `Arc` to the
/// task itself, leaked here, would keep the task, its kernel stack and its
/// address space alive forever after it exits. That is a slow leak that only
/// shows up as zombies that never disappear from `ps`.
pub fn user_entry() -> ! {
    crate::trap::set_user_vector();

    let frame = {
        let task = current_task();
        frame_ptr(&task)
    };

    // SAFETY: the frame was built by new_user or fork_from and describes a
    // valid user context; this never returns.
    unsafe { crate::trap::user_trap_return(frame) }
}

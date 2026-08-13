//! Tasks: the unit of scheduling.
//!
//! A task is a kernel stack, a saved context, and enough bookkeeping to put it
//! on a queue. Everything is behind `Arc<Task>` because a task is referenced
//! from several places at once -- the run queue, the current-task slot of some
//! hart, a wait queue it has blocked on, its parent's child list -- and the
//! last of those to let go is the one that frees it.
//!
//! Mutable state lives under a per-task `SpinLock` rather than a lock over the
//! whole table, so waking a task on one hart does not serialise against
//! scheduling on another.

pub mod context;
pub mod scheduler;

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

pub use context::{KernelStack, TaskContext, switch_context};
pub use scheduler::{
    block_current, current, exit_current, spawn, wake, yield_now,
};

/// A process identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(pub usize);

impl core::fmt::Display for Pid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Source of process ids.
///
/// Monotonic and never reused. Reuse is what makes a stale pid dangerous --
/// `kill(pid)` aimed at a process that exited can land on an unrelated one --
/// and 2^64 ids is enough that wrapping is not a real concern.
static NEXT_PID: AtomicUsize = AtomicUsize::new(1);

fn alloc_pid() -> Pid {
    Pid(NEXT_PID.fetch_add(1, Ordering::Relaxed))
}

/// Where a task is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Runnable and waiting on a queue.
    Ready,
    /// Currently executing on some hart.
    Running,
    /// Waiting for an event; not on any run queue. Something must call
    /// [`wake`] or this task never runs again.
    Blocked,
    /// Finished, waiting for its parent to collect the exit code.
    Zombie,
}

/// Scheduling priority levels, highest first.
///
/// The scheduler is a multi-level feedback queue: a task that uses a full
/// quantum without blocking drops a level, and one that blocks before its
/// quantum expires rises. The effect is that interactive work -- a shell
/// waiting on the console, a process waiting on disk -- keeps the top level
/// and gets scheduled promptly, while a compute loop settles at the bottom
/// without any of them being declared "interactive" up front.
pub const PRIORITY_LEVELS: usize = 3;

/// Mutable per-task state.
pub struct TaskInner {
    /// Lifecycle state.
    pub state: TaskState,
    /// Saved registers, valid whenever the task is not running.
    pub ctx: TaskContext,
    /// The stack `ctx.sp` points into.
    pub kstack: KernelStack,
    /// Current queue level, 0 being highest priority.
    pub level: usize,
    /// Quanta consumed since the last demotion.
    pub ticks: u64,
    /// Set by the timer when this task's quantum expires.
    pub needs_resched: bool,
    /// A wakeup arrived while this task was still running. Consumed by the
    /// next attempt to block, which turns a lost wakeup into a spurious one.
    pub wakeup_pending: bool,
    /// Value passed to `exit`.
    pub exit_code: i32,
}

/// A schedulable thread of control.
pub struct Task {
    /// Immutable identity.
    pub pid: Pid,
    /// Human-readable name, for `ps` and panic messages.
    pub name: String,
    /// What this task runs. Read by the entry trampoline.
    pub entry: fn(),
    /// Everything that changes.
    pub inner: SpinLock<TaskInner>,
}

impl Task {
    /// Create a kernel task that will begin at `entry`.
    ///
    /// The context is built so that the first `switch_context` into it returns
    /// straight into `task_entry`, which then calls `entry`.
    pub fn new_kernel(name: &str, entry: fn()) -> Option<Arc<Self>> {
        let kstack = KernelStack::new()?;
        let stack_top = kstack.top();

        // The trampoline learns what to run by reading it back off the task,
        // not from a register. Passing it in a callee-saved register looks
        // tempting -- switch_context restores those -- but the trampoline is a
        // Rust function, and its prologue overwrites s0 with a frame pointer
        // before a single line of its body executes.
        let ctx = TaskContext::new(task_entry as *const () as usize, stack_top);

        Some(Arc::new(Self {
            pid: alloc_pid(),
            name: String::from(name),
            entry,
            inner: SpinLock::new(TaskInner {
                state: TaskState::Ready,
                ctx,
                kstack,
                level: 0,
                ticks: 0,
                needs_resched: false,
                wakeup_pending: false,
                exit_code: 0,
            }),
        }))
    }

    /// Current lifecycle state.
    pub fn state(&self) -> TaskState {
        self.inner.lock().state
    }

    /// Set the lifecycle state.
    pub fn set_state(&self, state: TaskState) {
        self.inner.lock().state = state;
    }

    /// Exit code, meaningful once the task is a zombie.
    pub fn exit_code(&self) -> i32 {
        self.inner.lock().exit_code
    }
}

impl core::fmt::Debug for Task {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inner = self.inner.lock();
        write!(f, "Task({} {:?} L{} {:?})", self.pid, self.name, inner.level, inner.state)
    }
}

/// First thing a brand-new task executes.
///
/// Reached by `ret` from `switch_context`, not by a call, so there is no return
/// address on the stack and this must never return.
///
/// Interrupts are enabled here rather than in the scheduler because the switch
/// happens with them masked -- the run queue lock requires it -- and a task
/// that started with them masked could never be preempted.
extern "C" fn task_entry() -> ! {
    // The scheduler made this task current before switching in, so the entry
    // point is reachable without smuggling anything through a register.
    let entry = scheduler::current_task().entry;

    // SAFETY: we are on our own kernel stack with no locks held.
    unsafe { crate::arch::intr_enable() };

    entry();

    exit_current(0)
}

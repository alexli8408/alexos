//! The scheduler: a multi-level feedback queue with a per-hart idle context.
//!
//! # Why tasks switch *through* an idle context
//!
//! A task never switches directly to its successor. It switches to its hart's
//! idle context, which runs `run()`, which picks the next task and switches
//! into it. That indirection buys the thing that makes the whole design
//! race-free without holding a lock across a context switch:
//!
//! > A task is only put back on the run queue **after** it has finished
//! > switching off its stack.
//!
//! If a yielding task enqueued itself and then switched, there would be a
//! window in which another hart could pop it and start running it on a stack
//! whose registers had not been saved yet. Instead the outgoing task is parked
//! in `Cpu::previous`, and `run()` -- which by then is executing on the idle
//! context, not the task's -- decides what to do with it.
//!
//! # Why a feedback queue
//!
//! Round-robin gives a compute loop the same share of the CPU as a shell
//! waiting on a keystroke, so typing feels sluggish under load. Here a task
//! that burns a full quantum drops a level and one that blocks first rises,
//! which sorts interactive work to the top without anyone having to declare it
//! interactive.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::cell::UnsafeCell;

use crate::arch;
use crate::config::MAX_HARTS;
use crate::sync::SpinLock;
use crate::task::{PRIORITY_LEVELS, Task, TaskContext, TaskState, switch_context};

/// Quanta a task may consume at a level before it is demoted.
const QUANTA_PER_LEVEL: u64 = 4;

/// Runnable tasks, highest priority level first.
struct RunQueue {
    levels: [VecDeque<Arc<Task>>; PRIORITY_LEVELS],
    /// Total tasks ever created, for `ps`.
    spawned: usize,
}

impl RunQueue {
    const fn new() -> Self {
        Self { levels: [const { VecDeque::new() }; PRIORITY_LEVELS], spawned: 0 }
    }

    fn push(&mut self, task: Arc<Task>, level: usize) {
        self.levels[level.min(PRIORITY_LEVELS - 1)].push_back(task);
    }

    /// Take the highest-priority runnable task.
    fn pop(&mut self) -> Option<Arc<Task>> {
        self.levels.iter_mut().find_map(|q| q.pop_front())
    }

    fn len(&self) -> usize {
        self.levels.iter().map(|q| q.len()).sum()
    }
}

static RUN_QUEUE: SpinLock<RunQueue> = SpinLock::new(RunQueue::new());

/// Per-hart scheduling state.
struct Cpu {
    /// The task this hart is executing.
    current: SpinLock<Option<Arc<Task>>>,
    /// The task that just switched away, waiting to be requeued or reaped by
    /// `run()`. See the module comment for why this is not done by the task.
    previous: SpinLock<Option<Arc<Task>>>,
    /// Context of the idle loop. Only ever touched by its own hart.
    idle: UnsafeCell<TaskContext>,
}

// SAFETY: `current` and `previous` are lock-protected. `idle` is only accessed
// by the hart it belongs to, indexed by `hart_id()`, so no two harts can reach
// the same cell.
unsafe impl Sync for Cpu {}

impl Cpu {
    const fn new() -> Self {
        Self {
            current: SpinLock::new(None),
            previous: SpinLock::new(None),
            idle: UnsafeCell::new(TaskContext::zeroed()),
        }
    }
}

static CPUS: [Cpu; MAX_HARTS] = [const { Cpu::new() }; MAX_HARTS];

fn cpu() -> &'static Cpu {
    &CPUS[arch::hart_id()]
}

/// The task running on this hart, if any.
pub fn current() -> Option<Arc<Task>> {
    cpu().current.lock().clone()
}

/// The task running on this hart. Panics in contexts that have no task, which
/// is a bug in the caller rather than a condition to handle.
pub fn current_task() -> Arc<Task> {
    current().expect("no current task on this hart")
}

/// Create a kernel task and make it runnable.
pub fn spawn(name: &str, entry: fn()) -> Option<Arc<Task>> {
    let task = Task::new_kernel(name, entry)?;
    let mut queue = RUN_QUEUE.lock();
    queue.spawned += 1;
    queue.push(task.clone(), 0);
    Some(task)
}

/// Make a blocked task runnable again.
///
/// Safe to call from an interrupt handler: it only takes the run queue lock,
/// which masks interrupts, and never blocks. Waking a task twice is harmless,
/// so callers need not coordinate.
pub fn wake(task: &Arc<Task>) {
    let mut inner = task.inner.lock();
    if inner.state != TaskState::Blocked {
        // The target queued itself on a wait queue but has not switched away
        // yet. Dropping the wakeup here is the classic lost-wakeup bug, so
        // leave a note that `switch_to_idle` will find and honour.
        inner.wakeup_pending = true;
        return;
    }
    inner.state = TaskState::Ready;

    // Blocking means the task gave up the CPU before its quantum ran out, so
    // it is doing something interactive: promote it.
    inner.level = inner.level.saturating_sub(1);
    inner.ticks = 0;
    let level = inner.level;
    drop(inner);

    RUN_QUEUE.lock().push(task.clone(), level);
}

/// Give up the rest of this quantum.
pub fn yield_now() {
    switch_to_idle(TaskState::Ready);
}

/// Block the current task until someone calls [`wake`] on it.
///
/// The caller is responsible for having put the task on whatever wait queue
/// will eventually wake it, *before* calling this -- otherwise the wakeup can
/// arrive first and be dropped, and the task sleeps forever.
pub fn block_current() {
    switch_to_idle(TaskState::Blocked);
}

/// Terminate the current task. Never returns.
pub fn exit_current(code: i32) -> ! {
    if let Some(task) = current() {
        task.inner.lock().exit_code = code;
    }
    switch_to_idle(TaskState::Zombie);
    unreachable!("a zombie task was scheduled again");
}

/// Note that a quantum expired on this hart.
///
/// Called from the timer interrupt, which must not switch tasks itself: the
/// trap frame on the stack has to be unwound by the trap exit path first. So it
/// only sets a flag, and the trap handler acts on it once the frame is safe to
/// leave.
pub fn on_timer_tick() {
    let Some(task) = current() else { return };
    let mut inner = task.inner.lock();
    inner.ticks += 1;
    if inner.ticks >= QUANTA_PER_LEVEL {
        // Used a full allowance without blocking: this is compute-bound work,
        // so move it down and let interactive tasks past.
        inner.level = (inner.level + 1).min(PRIORITY_LEVELS - 1);
        inner.ticks = 0;
    }
    inner.needs_resched = true;
}

/// Yield if the timer asked for it. Called from the trap exit path, where the
/// stack is in a state that tolerates a switch.
pub fn resched_if_needed() {
    let Some(task) = current() else { return };
    let mut inner = task.inner.lock();
    if !core::mem::take(&mut inner.needs_resched) {
        return;
    }
    drop(inner);
    yield_now();
}

/// Switch from the current task to this hart's idle context, leaving the task
/// in `state`.
fn switch_to_idle(state: TaskState) {
    let cpu = cpu();

    // Interrupts stay masked from here until the idle loop has finished with
    // this task: an interrupt landing mid-switch would run on a stack that is
    // half-saved, and could try to schedule.
    let was_enabled = arch::intr_enabled();
    // SAFETY: restored below, or by the next task's own switch-in path.
    unsafe { arch::intr_disable() };

    // Scoped so that no `Arc<Task>` is alive on this stack across the switch.
    // A task that exits never comes back to drop one, and a strong count that
    // is never decremented means its kernel stack is never freed.
    let ctx_ptr = {
        let task = cpu.current.lock().take().expect("switch_to_idle with no current task");

        let ptr = {
            let mut inner = task.inner.lock();

            // A wakeup that arrived after the caller queued itself but before
            // it got here would otherwise be lost, and the task would sleep
            // forever. Consuming the flag turns that race into a spurious
            // wakeup, which every wait loop already tolerates.
            inner.state = if state == TaskState::Blocked && inner.wakeup_pending {
                inner.wakeup_pending = false;
                TaskState::Ready
            } else {
                state
            };

            if !inner.kstack.canary_intact() {
                panic!("kernel stack overflow in task {} ({})", task.pid, task.name);
            }
            &raw mut inner.ctx
        };

        // Hand the task to the idle loop, which requeues or reaps it once it is
        // running on a stack that is not this one.
        *cpu.previous.lock() = Some(task);
        ptr
    };

    // SAFETY: `ctx_ptr` points into a task the idle loop holds alive through
    // `cpu.previous`, and `idle` is this hart's own context. When this returns,
    // someone has switched back and our registers are restored.
    unsafe { switch_context(ctx_ptr, cpu.idle.get()) };

    if was_enabled {
        // SAFETY: restoring the interrupt state the task had on entry.
        unsafe { arch::intr_enable() };
    }
}

/// The idle loop. Never returns.
///
/// Every hart calls this once its bring-up is done; from then on the hart
/// alternates between here and whatever task it picks up.
pub fn run() -> ! {
    let cpu = cpu();
    loop {
        // Deal with whoever just switched away. This runs on the idle context,
        // so the task's own stack and registers are safely saved by now.
        if let Some(prev) = cpu.previous.lock().take() {
            let (state, level) = {
                let inner = prev.inner.lock();
                (inner.state, inner.level)
            };
            match state {
                TaskState::Ready => RUN_QUEUE.lock().push(prev, level),
                // A blocked task is kept alive by the wait queue holding it,
                // and a zombie by its parent's child list. Either way dropping
                // our reference is right: if it was the last one, the task and
                // its kernel stack are freed -- which is safe precisely because
                // we are running on the idle context, not on that stack.
                TaskState::Blocked | TaskState::Zombie => drop(prev),
                TaskState::Running => unreachable!("a running task reached the idle loop"),
            }
        }

        let next = RUN_QUEUE.lock().pop();
        let Some(task) = next else {
            // Nothing to run. Wait for an interrupt rather than spinning, so a
            // hart with no work does not burn power or fight for the bus.
            //
            // Interrupts must be enabled around the wfi or the timer that would
            // wake us can never arrive.
            // SAFETY: no locks are held here and the idle context has nothing
            // worth protecting.
            unsafe { arch::intr_enable() };
            arch::wait_for_interrupt();
            // SAFETY: back to the masked state the loop body expects.
            unsafe { arch::intr_disable() };
            continue;
        };

        let ctx = {
            let mut inner = task.inner.lock();
            inner.state = TaskState::Running;
            inner.needs_resched = false;
            &raw const inner.ctx
        };
        *cpu.current.lock() = Some(task);

        // SAFETY: `ctx` points into the task we just made current, and `idle`
        // is this hart's own context. Control comes back here when that task
        // switches away.
        unsafe { switch_context(cpu.idle.get(), ctx) };
    }
}

/// Number of runnable tasks, plus the total ever spawned.
pub fn stats() -> (usize, usize) {
    let queue = RUN_QUEUE.lock();
    (queue.len(), queue.spawned)
}

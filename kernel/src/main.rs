//! AlexOS -- a preemptive multitasking kernel for RISC-V 64, written from
//! scratch in stable Rust.
//!
//! Boot order, and why it is this order:
//!
//! 1. `entry.S` turns on Sv39 with a static two-window page table and jumps to
//!    the high half. Nothing before this point may touch a Rust static.
//! 2. The console comes up on SBI, so every later failure is reportable.
//! 3. Physical frames, then the kernel heap, then the real kernel page table.
//!    Each layer needs the one below it: the page table allocates frames, and
//!    address spaces allocate from the heap.
//! 4. Traps, so faults stop being fatal.
//! 5. Drivers, then the filesystem, then the scheduler and `init`.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

core::arch::global_asm!(include_str!("entry.S"));

#[macro_use]
pub mod console;

pub mod arch;
pub mod backtrace;
pub mod config;
pub mod drivers;
pub mod lang_items;
pub mod mm;
pub mod sbi;
pub mod sync;
pub mod task;
pub mod test_device;
pub mod timer;
pub mod trap;

/// Rust entry point, called from `entry.S` once paging is live and `.bss` is
/// clear.
///
/// `hart_id` is the boot hart; `dtb` is the *physical* address of the device
/// tree QEMU handed to OpenSBI.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(hart_id: usize, dtb: usize) -> ! {
    drivers::init_early();
    banner(hart_id, dtb);

    info!("kernel image  {} KiB", mm::kernel_image_size() / 1024);

    // SAFETY: boot hart, once, before anything allocates.
    unsafe { mm::init() };

    trap::init();
    drivers::init_interrupts(hart_id);
    timer::init();
    trap::enable_interrupts();

    task::spawn("spinner", demo::spinner).expect("spawn spinner");
    task::spawn("console", demo::console).expect("spawn console");
    task::spawn("heartbeat", demo::heartbeat).expect("spawn heartbeat");

    info!("boot complete -- scheduler taking over");

    // Becomes this hart's idle loop and never returns.
    task::scheduler::run()
}

/// Demonstration tasks, replaced by `init` once userspace exists.
///
/// The point is to show three things at once: that the timer preempts a task
/// that never yields, that a task which blocks on a wait queue is woken by an
/// interrupt handler, and that the feedback queue keeps the interactive task
/// responsive while the compute loop runs flat out.
mod demo {
    use crate::sync::WaitQueue;
    use crate::task::{self, scheduler};

    /// Woken by the console interrupt handler.
    pub static CONSOLE_WAIT: WaitQueue = WaitQueue::new();

    /// Never yields voluntarily. If this task can be interleaved with the
    /// others, preemption works.
    pub fn spinner() {
        let mut n: u64 = 0;
        loop {
            // Deliberately tight: no yield, no syscall, nothing that would give
            // the scheduler a cooperative opening.
            for _ in 0..8_000_000 {
                core::hint::spin_loop();
            }
            n += 1;
            let task = scheduler::current_task();
            let level = task.inner.lock().level;
            crate::info!("spinner: pass {n} (queue level {level})");
        }
    }

    /// Sleeps on a wait queue and reports what was typed.
    pub fn console() {
        loop {
            CONSOLE_WAIT.wait_until(|| crate::drivers::uart::has_input());
            while let Some(byte) = crate::drivers::uart::read_byte() {
                match byte {
                    b'\r' | b'\n' => crate::println!(),
                    0x7f | 0x08 => crate::print!("\x08 \x08"),
                    b => crate::print!("{}", b as char),
                }
            }
        }
    }

    /// Prints a heartbeat, yielding between beats.
    pub fn heartbeat() {
        let mut last = 0;
        loop {
            let seconds = crate::timer::uptime_ms() / 1000;
            if seconds != last {
                last = seconds;
                let (ready, spawned) = scheduler::stats();
                crate::debug!("uptime {seconds}s | {ready} ready, {spawned} spawned");
            }
            task::yield_now();
        }
    }
}

/// Wake anything blocked on console input. Called from the UART interrupt.
pub fn main_wake_console() {
    demo::CONSOLE_WAIT.wake_all();
}

/// Print the banner and the machine description.
fn banner(hart_id: usize, dtb: usize) {
    println!();
    println!("   _    _           ___  ____");
    println!("  / \\  | | _____  __/ _ \\/ ___|");
    println!(" / _ \\ | |/ _ \\ \\/ / | | \\___ \\");
    println!("/ ___ \\| |  __/>  <| |_| |___) |");
    println!("\\_/  \\_\\_|\\___/_/\\_\\\\___/|____/");
    println!();
    println!("  riscv64 · sv39 · stable rust");
    println!("  boot hart {hart_id} · dtb {dtb:#x}");
    println!();
}

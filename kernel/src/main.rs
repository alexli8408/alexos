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
pub mod test_device;

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

    info!("boot complete");
    sbi::shutdown(sbi::ResetType::Shutdown)
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

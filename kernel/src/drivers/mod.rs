//! Device drivers for the QEMU `virt` board.

pub mod plic;
pub mod uart;

/// Bring up the console before anything that might need to report a failure.
pub fn init_early() {
    uart::init();
}

/// Bring up the interrupt controller. Needs traps to be installed first.
pub fn init_interrupts(hart: usize) {
    plic::init(hart);
}

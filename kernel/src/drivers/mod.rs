//! Device drivers for the QEMU `virt` board.

pub mod uart;

/// Bring up the drivers that do not depend on the scheduler.
pub fn init_early() {
    uart::init();
}

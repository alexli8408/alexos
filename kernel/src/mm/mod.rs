//! Memory management.
//!
//! Layered bottom-up, and the order matters because each layer is built out of
//! the one below it:
//!
//! ```text
//!   addr        typed addresses and page numbers
//!   frame       buddy allocator over physical DRAM
//!   heap        size-class allocator backing `alloc`, refilled from frames
//!   page_table  Sv39 walks; needs `alloc` to own its interior tables
//!   space       address spaces composed of mapped regions
//! ```
//!
//! `init` follows exactly that order. Nothing above the frame allocator may
//! run before it, which is why the heap cannot simply be a static array: it
//! would have to be sized for the worst case and would waste the difference.

pub mod addr;
pub mod frame;
pub mod heap;
pub mod page_table;

pub use addr::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
pub use page_table::{MapError, PageTable, Pte, PteFlags};

use crate::config::{DRAM_BASE, DRAM_SIZE, VIRT_OFFSET};

// Symbols emitted by linker.ld. Reading their *addresses* is the point; the
// types are arbitrary because the objects have no Rust-level meaning.
#[allow(missing_docs)]
unsafe extern "C" {
    pub safe static __kernel_start: [u8; 0];
    pub safe static __kernel_end: [u8; 0];
    pub safe static __text_start: [u8; 0];
    pub safe static __text_end: [u8; 0];
    pub safe static __rodata_start: [u8; 0];
    pub safe static __rodata_end: [u8; 0];
    pub safe static __data_start: [u8; 0];
    pub safe static __data_end: [u8; 0];
    pub safe static __bss_start: [u8; 0];
    pub safe static __bss_end: [u8; 0];
}

/// Translate a kernel virtual address in the direct map to its physical address.
///
/// Only valid for direct-mapped addresses -- that is, the kernel image and the
/// physical-memory window. User addresses need a page-table walk instead; see
/// [`PageTable::translate_addr`].
#[inline(always)]
pub const fn virt_to_phys(vaddr: usize) -> usize {
    vaddr - VIRT_OFFSET
}

/// Translate a physical address to its direct-map virtual address.
#[inline(always)]
pub const fn phys_to_virt(paddr: usize) -> usize {
    paddr + VIRT_OFFSET
}

/// First physical address not occupied by the kernel image, page aligned.
/// Everything from here to the top of DRAM is fair game for the allocator.
pub fn kernel_end_phys() -> usize {
    virt_to_phys(&raw const __kernel_end as usize)
}

/// Size of the loaded kernel image in bytes.
pub fn kernel_image_size() -> usize {
    (&raw const __kernel_end as usize) - (&raw const __kernel_start as usize)
}

/// Bring up physical and virtual memory management.
///
/// # Safety
/// Call exactly once, on the boot hart, before any allocation.
pub unsafe fn init() {
    let dram_end = PhysAddr(DRAM_BASE + DRAM_SIZE);

    // SAFETY: everything from the end of the kernel image to the top of DRAM
    // is unused, and the boot page table direct-maps all of it.
    unsafe { frame::init(dram_end) };

    let allocator = frame::FRAME_ALLOCATOR.lock();
    let total = allocator.total_frames();
    drop(allocator);
    crate::info!(
        "frames: {} usable ({} MiB) from {:#x} to {:#x}",
        total,
        total * crate::config::PAGE_SIZE / 1024 / 1024,
        kernel_end_phys(),
        dram_end.0
    );

    heap::self_test();
    let (used, reserved) = heap::stats();
    crate::info!("heap: self-test passed, {used} B live / {reserved} B reserved");
}

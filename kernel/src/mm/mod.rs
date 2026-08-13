//! Memory management.
//!
//! Layered bottom-up: [`addr`] gives typed addresses and page numbers, `frame`
//! hands out physical frames, `page_table` walks and edits Sv39 tables,
//! `space` composes those into an address space, and `heap` backs `alloc`.

use crate::config::VIRT_OFFSET;

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
/// `space::AddressSpace::translate`.
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

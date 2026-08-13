//! Address spaces.
//!
//! An `AddressSpace` is a page table plus the list of regions mapped into it.
//! Keeping the region list -- rather than only the hardware table -- is what
//! makes `fork`, `exec` and demand paging tractable: the fault handler needs to
//! answer "is this address supposed to be valid?", and a page table can only
//! say what *is* mapped, never what *should* be.
//!
//! The kernel's own space replaces the boot table built in entry.S. That table
//! maps everything RWX with 1 GiB leaves, which is fine for the dozen
//! instructions between enabling paging and getting here, and unacceptable
//! afterwards: it leaves the kernel's own text writable and its data
//! executable. The space built here gives each section exactly what it needs.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::arch;
use crate::config::{
    CLINT_BASE, DRAM_BASE, DRAM_SIZE, PAGE_SIZE, PLIC_BASE, TEST_DEVICE_BASE, UART_BASE,
    VIRT_OFFSET, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_STRIDE,
};
use crate::mm::addr::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use crate::mm::frame::Frame;
use crate::mm::page_table::{MapError, PageTable, PteFlags};
use crate::mm::{
    __bss_end, __bss_start, __data_end, __data_start, __rodata_end, __rodata_start, __text_end,
    __text_start,
};
use crate::sync::SpinLock;

/// Size of a level-1 leaf: one entry covers 2 MiB.
const SUPERPAGE_SIZE: usize = 2 * 1024 * 1024;

/// How a region's virtual pages are backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backing {
    /// Linear: virtual page `v` maps to physical frame `v - VIRT_OFFSET`. Used
    /// for the kernel image and the physical-memory window, where the mapping
    /// is a fixed offset and the frames are not owned by anyone.
    Linear,
    /// Each page is backed by a frame from the allocator, owned by the region.
    /// Used for everything in user space.
    Framed,
}

/// A contiguous range of virtual pages sharing a backing and permissions.
pub struct Region {
    pub(crate) start: VirtAddr,
    pub(crate) end: VirtAddr,
    pub(crate) backing: Backing,
    pub(crate) perm: PteFlags,
    /// Frames owned by this region, for `Backing::Framed`. Indexed by page so
    /// that a partial unmap or a copy-on-write fault can find one page's frame
    /// without scanning.
    pub(crate) frames: BTreeMap<VirtPageNum, Frame>,
}

impl Region {
    /// Describe a region without mapping it yet.
    pub fn new(start: VirtAddr, end: VirtAddr, backing: Backing, perm: PteFlags) -> Self {
        Self {
            start: start.align_down(),
            end: end.align_up(),
            backing,
            perm,
            frames: BTreeMap::new(),
        }
    }

    /// First page of the region.
    pub fn start(&self) -> VirtAddr {
        self.start
    }

    /// One past the last page of the region.
    pub fn end(&self) -> VirtAddr {
        self.end
    }

    /// Permissions every page in the region carries.
    pub fn perm(&self) -> PteFlags {
        self.perm
    }

    /// Does `addr` fall inside this region?
    pub fn contains(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.end
    }

    /// Install every page of this region into `table`.
    pub(crate) fn map_into(&mut self, table: &mut PageTable) -> Result<(), MapError> {
        let mut vpn = self.start.vpn();
        let end = self.end.vpn();

        while vpn.0 < end.0 {
            match self.backing {
                Backing::Linear => {
                    let ppn = PhysPageNum(vpn.0 - (VIRT_OFFSET >> crate::config::PAGE_SHIFT));

                    // Use a 2 MiB leaf when both ends line up and the whole
                    // superpage is inside the region. One entry instead of 512
                    // is a real saving on the linear map, which spans all of
                    // DRAM: less page-table memory, and 512x fewer TLB entries
                    // to cover the same range.
                    let pages = SUPERPAGE_SIZE / PAGE_SIZE;
                    if vpn.0.is_multiple_of(pages)
                        && ppn.0.is_multiple_of(pages)
                        && vpn.0 + pages <= end.0
                    {
                        table.map_at_level(vpn, ppn, self.perm, 1)?;
                        vpn = VirtPageNum(vpn.0 + pages);
                        continue;
                    }
                    table.map(vpn, ppn, self.perm)?;
                }
                Backing::Framed => {
                    let frame = Frame::alloc().ok_or(MapError::OutOfMemory)?;
                    table.map(vpn, frame.ppn(), self.perm)?;
                    self.frames.insert(vpn, frame);
                }
            }
            vpn = vpn.next();
        }
        Ok(())
    }

    /// Remove every page of this region from `table`, releasing owned frames.
    fn unmap_from(&mut self, table: &mut PageTable) {
        let mut vpn = self.start.vpn();
        while vpn.0 < self.end.vpn().0 {
            let _ = table.unmap(vpn);
            vpn = vpn.next();
        }
        // Dropping the frames returns them to the buddy allocator.
        self.frames.clear();
    }

    /// Copy `data` into the region, starting at its base.
    ///
    /// Only meaningful for `Backing::Framed`; this is how `exec` gets an ELF
    /// segment into a space that is not currently active. Writing through the
    /// direct map avoids having to switch `satp` mid-load.
    pub fn write_data(&mut self, offset: usize, data: &[u8]) {
        debug_assert_eq!(self.backing, Backing::Framed);
        let mut written = 0;
        let mut pos = offset;

        while written < data.len() {
            let vpn = VirtAddr(self.start.0 + pos).vpn();
            let page_off = pos % PAGE_SIZE;
            let n = (PAGE_SIZE - page_off).min(data.len() - written);

            let frame = self.frames.get_mut(&vpn).expect("write_data outside the region");
            // SAFETY: the frame is owned by this region and `page_off + n` is
            // within one page by construction.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(written),
                    frame.ppn().as_ptr().add(page_off),
                    n,
                );
            }
            written += n;
            pos += n;
        }
    }
}

impl core::fmt::Debug for AddressSpace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AddressSpace {{ {} regions, {:?} }}", self.regions.len(), self.table)
    }
}

/// A page table plus the regions mapped into it.
pub struct AddressSpace {
    table: PageTable,
    regions: Vec<Region>,
}

impl AddressSpace {
    /// An empty space with a fresh root table.
    pub fn new() -> Option<Self> {
        Some(Self { table: PageTable::new()?, regions: Vec::new() })
    }

    /// Map `region` and take ownership of it.
    pub fn push(&mut self, mut region: Region) -> Result<(), MapError> {
        region.map_into(&mut self.table)?;
        self.regions.push(region);
        Ok(())
    }

    /// Map `region` and copy `data` into it starting `offset` bytes in.
    ///
    /// The offset exists because an ELF segment is page-aligned in memory but
    /// its contents start at whatever sub-page offset the file gives it.
    pub fn push_with_offset(
        &mut self,
        mut region: Region,
        offset: usize,
        data: &[u8],
    ) -> Result<(), MapError> {
        region.map_into(&mut self.table)?;
        region.write_data(offset, data);
        self.regions.push(region);
        Ok(())
    }

    /// Give this space the kernel's upper-half mappings.
    ///
    /// Every user space needs these: the trap handler runs with the faulting
    /// task's page table still installed, so kernel text, stacks and the linear
    /// map all have to be reachable from it.
    pub fn map_kernel_half(&mut self) -> Result<(), MapError> {
        let kernel = KERNEL_SPACE.lock();
        let kernel = kernel.as_ref().expect("kernel space not built yet");
        self.table.share_upper_half(kernel.table());
        Ok(())
    }

    /// Regions in this space, for fork and for diagnostics.
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Duplicate this space: same layout, freshly allocated frames with the
    /// same contents.
    ///
    /// This is the eager half of `fork`. Copy-on-write would avoid the copy for
    /// pages that are never written -- which, given fork is usually followed by
    /// exec, is most of them -- and the PTE bit for it is already defined. The
    /// eager copy is correct and simple; the optimisation is not yet wired up.
    pub fn duplicate(&self) -> Option<Self> {
        let mut child = Self::new()?;
        child.map_kernel_half().ok()?;

        for region in &self.regions {
            if region.backing != Backing::Framed {
                // Linear regions are the kernel's, already shared above.
                continue;
            }
            let mut copy = Region::new(region.start, region.end, Backing::Framed, region.perm);
            copy.map_into(&mut child.table).ok()?;

            for (&vpn, frame) in &region.frames {
                let dst = copy.frames.get(&vpn)?;
                // SAFETY: both frames are owned by their regions, are exactly
                // one page, and are distinct allocations.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        frame.ppn().as_ptr(),
                        dst.ppn().as_ptr(),
                        PAGE_SIZE,
                    );
                }
            }
            child.regions.push(copy);
        }
        Some(child)
    }

    /// Copy `len` bytes out of this space at `addr` into `dst`.
    ///
    /// Walks the page table and copies through the linear map rather than
    /// dereferencing the user pointer, so it works whether or not this space is
    /// the one currently installed, and a bad address returns None instead of
    /// faulting in the kernel.
    pub fn copy_from_user(&self, addr: VirtAddr, dst: &mut [u8]) -> Option<()> {
        let mut copied = 0;
        while copied < dst.len() {
            let va = VirtAddr(addr.0.checked_add(copied)?);
            if va.0 >= crate::config::USER_MAX_ADDR {
                return None;
            }
            let pa = self.table.translate_addr(va)?;
            let n = (PAGE_SIZE - va.page_offset()).min(dst.len() - copied);
            // SAFETY: `pa` came from a valid mapping, so the linear map covers
            // it, and `n` stays inside the page.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    pa.to_virt().0 as *const u8,
                    dst.as_mut_ptr().add(copied),
                    n,
                );
            }
            copied += n;
        }
        Some(())
    }

    /// Copy `src` into this space at `addr`.
    pub fn copy_to_user(&self, addr: VirtAddr, src: &[u8]) -> Option<()> {
        let mut copied = 0;
        while copied < src.len() {
            let va = VirtAddr(addr.0.checked_add(copied)?);
            if va.0 >= crate::config::USER_MAX_ADDR {
                return None;
            }
            let pte = self.table.translate(va.vpn())?;
            // Refuse to write through a mapping the user itself could not
            // write: a syscall must not be a way around page permissions.
            if !pte.flags().contains(PteFlags::WRITE | PteFlags::USER) {
                return None;
            }
            let pa = self.table.translate_addr(va)?;
            let n = (PAGE_SIZE - va.page_offset()).min(src.len() - copied);
            // SAFETY: as above, plus the write permission check.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr().add(copied),
                    pa.to_virt().0 as *mut u8,
                    n,
                );
            }
            copied += n;
        }
        Some(())
    }

    /// Read a NUL-terminated string of at most `max` bytes from user memory.
    pub fn read_cstr(&self, addr: VirtAddr, max: usize) -> Option<alloc::string::String> {
        let mut bytes = alloc::vec::Vec::new();
        for i in 0..max {
            let mut byte = [0u8; 1];
            self.copy_from_user(VirtAddr(addr.0.checked_add(i)?), &mut byte)?;
            if byte[0] == 0 {
                return alloc::string::String::from_utf8(bytes).ok();
            }
            bytes.push(byte[0]);
        }
        None
    }

    /// The `satp` value that activates this space.
    pub fn token(&self) -> usize {
        self.table.token()
    }

    /// The underlying page table.
    pub fn table(&self) -> &PageTable {
        &self.table
    }

    /// The underlying page table, mutably.
    pub fn table_mut(&mut self) -> &mut PageTable {
        &mut self.table
    }

    /// The region containing `addr`, if any.
    pub fn find_region(&self, addr: VirtAddr) -> Option<&Region> {
        self.regions.iter().find(|r| r.contains(addr))
    }

    /// Install this space in `satp` and flush the TLB.
    ///
    /// # Safety
    /// Every address the current instruction stream, stack and in-flight
    /// pointers depend on must be mapped in this space, or the hart faults on
    /// the next instruction.
    pub unsafe fn activate(&self) {
        let token = self.table.token();
        // SAFETY: contract delegated to the caller. The fence after the write
        // is mandatory: satp is not ordered against earlier page-table stores,
        // so without it the hart may translate through a stale TLB.
        unsafe {
            arch::satp::write(token);
        }
        arch::sfence_vma_all();
    }

    /// Resolve `addr` through this space's page table.
    pub fn translate(&self, addr: VirtAddr) -> Option<PhysAddr> {
        self.table.translate_addr(addr)
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Unmap explicitly so owned frames go back to the allocator before the
        // page table's own frames do.
        let mut regions = core::mem::take(&mut self.regions);
        for region in &mut regions {
            region.unmap_from(&mut self.table);
        }
    }
}

/// The kernel's address space, shared by every hart and mapped into the upper
/// half of every user space.
pub static KERNEL_SPACE: SpinLock<Option<AddressSpace>> = SpinLock::new(None);

/// MMIO windows the kernel needs after it stops using the boot table.
///
/// Device memory is mapped read/write and explicitly *not* executable; there is
/// no reason for the hart to fetch instructions from a UART, and leaving X set
/// would hand an attacker a place to land.
const MMIO_REGIONS: &[(usize, usize, &str)] = &[
    (TEST_DEVICE_BASE, PAGE_SIZE, "test finisher"),
    (CLINT_BASE, 0x10000, "clint"),
    (PLIC_BASE, 0x600000, "plic"),
    (UART_BASE, PAGE_SIZE, "uart"),
    (VIRTIO_MMIO_BASE, VIRTIO_MMIO_STRIDE * VIRTIO_MMIO_SLOTS, "virtio-mmio"),
];

/// Build the kernel address space and switch to it.
///
/// # Safety
/// Call once, on the boot hart, after the frame allocator and heap are up.
pub unsafe fn init_kernel_space() {
    let mut space = AddressSpace::new().expect("no memory for the kernel page table");

    let kernel = PteFlags::GLOBAL;
    let sections: [(usize, usize, PteFlags, &str); 4] = [
        (addr_of(&__text_start), addr_of(&__text_end), PteFlags::RX | kernel, ".text"),
        (addr_of(&__rodata_start), addr_of(&__rodata_end), PteFlags::READ | kernel, ".rodata"),
        (addr_of(&__data_start), addr_of(&__data_end), PteFlags::RW | kernel, ".data"),
        (addr_of(&__bss_start), addr_of(&__bss_end), PteFlags::RW | kernel, ".bss"),
    ];

    for (start, end, perm, name) in sections {
        if start == end {
            continue;
        }
        crate::debug!("map {name:<8} {start:#x}..{end:#x} {perm:?}");
        space
            .push(Region::new(VirtAddr(start), VirtAddr(end), Backing::Linear, perm))
            .expect("failed to map a kernel section");
    }

    // The physical-memory window: everything above the kernel image, so the
    // allocator's frames are reachable without a temporary mapping. Not
    // executable, and it deliberately excludes the firmware below the kernel.
    let heap_start = VirtAddr(crate::mm::phys_to_virt(crate::mm::kernel_end_phys()));
    let heap_end = VirtAddr(crate::mm::phys_to_virt(DRAM_BASE + DRAM_SIZE));
    crate::debug!("map linear   {:#x}..{:#x}", heap_start.0, heap_end.0);
    space
        .push(Region::new(heap_start, heap_end, Backing::Linear, PteFlags::RW | kernel))
        .expect("failed to map the physical memory window");

    for &(base, size, name) in MMIO_REGIONS {
        let start = VirtAddr(crate::mm::phys_to_virt(base));
        let end = VirtAddr(crate::mm::phys_to_virt(base + size));
        crate::debug!("map mmio     {start:?}..{end:?} {name}");
        space
            .push(Region::new(start, end, Backing::Linear, PteFlags::RW | kernel))
            .expect("failed to map an MMIO window");
    }

    let table_frames = space.table.table_frames();

    // SAFETY: every section of the kernel, the stack (in .bss), the physical
    // window and all MMIO are mapped above, so execution continues normally
    // across the switch.
    unsafe { space.activate() };

    *KERNEL_SPACE.lock() = Some(space);
    crate::info!("kernel space active, {table_frames} page-table frames, boot table retired");
}

/// `satp` value for the kernel address space.
pub fn kernel_token() -> usize {
    KERNEL_SPACE.lock().as_ref().expect("kernel space not built yet").token()
}

/// Install an arbitrary `satp` value and flush the TLB.
///
/// # Safety
/// The space must map the kernel half, or the next instruction faults.
pub unsafe fn activate_token(token: usize) {
    if arch::satp::read() == token {
        // Skip the fence when nothing changes. Rescheduling between two kernel
        // tasks is the common case, and a full TLB flush there would throw away
        // the whole working set for nothing.
        return;
    }
    // SAFETY: contract delegated to the caller.
    unsafe { arch::satp::write(token) };
    arch::sfence_vma_all();
}

/// Switch to the kernel address space.
///
/// # Safety
/// Only valid from kernel code running on a kernel stack.
pub unsafe fn activate_kernel() {
    let token = kernel_token();
    // SAFETY: the kernel space maps all kernel text, stacks and devices.
    unsafe { activate_token(token) };
}

/// Address of a linker-emitted symbol.
fn addr_of(sym: &[u8; 0]) -> usize {
    sym.as_ptr() as usize
}

//! Sv39 page tables.
//!
//! Sv39 splits a 39-bit virtual address into three 9-bit indices and a 12-bit
//! page offset. A walk starts at the frame in `satp` and follows non-leaf
//! entries down; an entry with any of R/W/X set is a leaf and terminates the
//! walk, which is what makes 2 MiB and 1 GiB superpages possible at levels 1
//! and 0. The kernel maps 4 KiB pages for everything it has to control at page
//! granularity, and 2 MiB leaves for the linear map, where one entry covering
//! 512 pages is a large saving in both table memory and TLB reach.
//!
//! A `PageTable` owns every frame it touches -- root and interior alike -- so
//! destroying an address space is just dropping the struct.

use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use crate::config::{PTE_PER_TABLE, SV39_LEVELS};
use crate::mm::addr::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use crate::mm::frame::Frame;

/// Permission and state bits of a page table entry.
///
/// Hand-rolled rather than pulled from `bitflags` because the kernel builds
/// with no external crates.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PteFlags(pub usize);

impl PteFlags {
    /// Entry is valid. Cleared means the walk faults here.
    pub const VALID: Self = Self(1 << 0);
    /// Readable.
    pub const READ: Self = Self(1 << 1);
    /// Writable. Meaningless without `READ` -- W-only is a reserved encoding.
    pub const WRITE: Self = Self(1 << 2);
    /// Executable.
    pub const EXEC: Self = Self(1 << 3);
    /// Reachable from user mode. Kernel pages must leave this clear or SUM
    /// stops being a meaningful guard.
    pub const USER: Self = Self(1 << 4);
    /// Global: the mapping is identical in every address space, so the TLB may
    /// keep it across an `satp` switch. Set on all kernel mappings.
    pub const GLOBAL: Self = Self(1 << 5);
    /// Accessed. The hardware sets this on first use if it implements Svadu;
    /// otherwise a zero A bit faults, so the kernel presets it.
    pub const ACCESSED: Self = Self(1 << 6);
    /// Dirty. Same story as `ACCESSED`, for writes.
    pub const DIRTY: Self = Self(1 << 7);

    /// Bit 8, reserved for supervisor use. The kernel marks copy-on-write
    /// pages here: the entry is read-only to the hardware, and this bit tells
    /// the fault handler the difference between "copy it" and "kill the task".
    pub const COW: Self = Self(1 << 8);

    /// Bit 9, also software-defined. Marks a page reserved by a VMA but not yet
    /// backed by a frame, so a fault means "allocate", not "segfault".
    pub const LAZY: Self = Self(1 << 9);

    /// No bits set.
    pub const EMPTY: Self = Self(0);

    /// Read + write, the usual data mapping.
    pub const RW: Self = Self(Self::READ.0 | Self::WRITE.0);
    /// Read + execute, for text.
    pub const RX: Self = Self(Self::READ.0 | Self::EXEC.0);
    /// Read + write + execute.
    pub const RWX: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::EXEC.0);

    /// Are all the bits in `other` present?
    #[inline(always)]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Is any bit in `other` present?
    #[inline(always)]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Union.
    #[inline(always)]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Everything in `self` that is not in `other`.
    #[inline(always)]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl core::ops::BitOr for PteFlags {
    type Output = Self;
    #[inline(always)]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for PteFlags {
    #[inline(always)]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for PteFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fixed-width so that a column of dumped entries lines up.
        for (bit, ch) in [
            (Self::VALID, 'V'),
            (Self::READ, 'R'),
            (Self::WRITE, 'W'),
            (Self::EXEC, 'X'),
            (Self::USER, 'U'),
            (Self::GLOBAL, 'G'),
            (Self::ACCESSED, 'A'),
            (Self::DIRTY, 'D'),
            (Self::COW, 'C'),
            (Self::LAZY, 'L'),
        ] {
            f.write_char(if self.contains(bit) { ch } else { '-' })?;
        }
        Ok(())
    }
}

/// One page table entry: `(ppn << 10) | flags`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Pte(pub usize);

impl Pte {
    /// Bits below the PPN field.
    const FLAG_MASK: usize = 0x3ff;

    /// An entry that faults on any access.
    pub const EMPTY: Self = Self(0);

    /// Build an entry pointing at `ppn` with `flags`.
    #[inline(always)]
    pub const fn new(ppn: PhysPageNum, flags: PteFlags) -> Self {
        Self((ppn.0 << 10) | flags.0)
    }

    /// Frame this entry points at -- the next table for an interior entry, the
    /// mapped page for a leaf.
    #[inline(always)]
    pub const fn ppn(self) -> PhysPageNum {
        PhysPageNum((self.0 >> 10) & ((1 << 44) - 1))
    }

    /// Permission and state bits.
    #[inline(always)]
    pub const fn flags(self) -> PteFlags {
        PteFlags(self.0 & Self::FLAG_MASK)
    }

    /// Replace the flags, keeping the frame.
    #[inline(always)]
    pub fn set_flags(&mut self, flags: PteFlags) {
        self.0 = (self.0 & !Self::FLAG_MASK) | flags.0;
    }

    /// Does the walk continue through this entry?
    #[inline(always)]
    pub const fn is_valid(self) -> bool {
        self.flags().contains(PteFlags::VALID)
    }

    /// Is this a leaf -- that is, does it map a page rather than name a table?
    ///
    /// The distinguishing feature is any of R/W/X being set; a valid entry with
    /// none of them is a pointer to the next level.
    #[inline(always)]
    pub const fn is_leaf(self) -> bool {
        self.is_valid() && self.flags().intersects(PteFlags::RWX)
    }
}

impl fmt::Debug for Pte {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pte({:?} -> {:?})", self.flags(), self.ppn())
    }
}

/// Borrow the frame `ppn` as this level's array of page table entries.
///
/// # Safety
/// The frame must actually hold a page table the caller owns; nothing else may
/// be mutating it for the lifetime of the returned slice.
#[inline]
unsafe fn table_at(ppn: PhysPageNum) -> &'static mut [Pte] {
    // SAFETY: a table is PTE_PER_TABLE 8-byte entries -- exactly one page --
    // and the direct map covers every frame.
    unsafe { core::slice::from_raw_parts_mut(ppn.as_ptr() as *mut Pte, PTE_PER_TABLE) }
}

/// Why a mapping operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// The virtual page already has a valid mapping.
    AlreadyMapped,
    /// The virtual page has no mapping to remove or modify.
    NotMapped,
    /// The frame allocator could not supply an interior table.
    OutOfMemory,
}

/// An Sv39 page table and every frame it owns.
pub struct PageTable {
    root: PhysPageNum,
    /// Root plus interior tables. Dropping these frees the whole tree; leaf
    /// *target* frames are owned by the caller, not by the table.
    frames: Vec<Frame>,
}

impl PageTable {
    /// Build an empty table: a single zeroed root frame.
    pub fn new() -> Option<Self> {
        let frame = Frame::alloc()?;
        let root = frame.ppn();
        Some(Self { root, frames: alloc::vec![frame] })
    }

    /// Frame number of the root table.
    pub fn root(&self) -> PhysPageNum {
        self.root
    }

    /// Value to load into `satp`: Sv39 mode in the top nibble plus the root PPN.
    pub fn token(&self) -> usize {
        (8usize << 60) | self.root.0
    }

    /// Walk to `vpn`'s slot at `stop_level`, allocating interior tables as
    /// needed. `stop_level` 2 yields a 4 KiB slot, 1 a 2 MiB slot, 0 a 1 GiB
    /// slot.
    fn walk_create_to(&mut self, vpn: VirtPageNum, stop_level: usize) -> Option<&mut Pte> {
        let indices = vpn.indices();
        let mut ppn = self.root;

        for (level, &index) in indices.iter().enumerate() {
            // SAFETY: `ppn` is a table frame owned by this object, so it holds
            // PTE_PER_TABLE entries reachable through the direct map.
            let entries = unsafe { table_at(ppn) };
            if level == stop_level {
                return Some(&mut entries[index]);
            }
            if !entries[index].is_valid() {
                let frame = Frame::alloc()?;
                // An interior entry carries no permission bits: R/W/X all clear
                // is exactly what marks it as a pointer rather than a leaf.
                entries[index] = Pte::new(frame.ppn(), PteFlags::VALID);
                self.frames.push(frame);
            }
            ppn = entries[index].ppn();
        }
        unreachable!("Sv39 walk ran past the leaf level")
    }

    /// Page-size granularity of a leaf installed at each level.
    const fn level_page_size(level: usize) -> usize {
        // level 0 -> 1 GiB, 1 -> 2 MiB, 2 -> 4 KiB
        1 << (crate::config::PAGE_SHIFT + 9 * (SV39_LEVELS - 1 - level))
    }

    /// Locate `vpn`'s leaf slot without allocating.
    ///
    /// Returns a raw pointer rather than a reference so that the read-only and
    /// mutable entry points below can each apply their own borrow rules; handing
    /// out a `&mut Pte` from `&self` would launder away the distinction.
    fn walk_ptr(&self, vpn: VirtPageNum) -> Option<*mut Pte> {
        let indices = vpn.indices();
        let mut ppn = self.root;

        for (level, &index) in indices.iter().enumerate() {
            // SAFETY: `ppn` names a table frame owned by this object.
            let entries = unsafe { table_at(ppn) };
            if level == SV39_LEVELS - 1 {
                return Some(&raw mut entries[index]);
            }
            let pte = entries[index];
            if !pte.is_valid() {
                return None;
            }
            if pte.is_leaf() {
                // A superpage. Nothing in the kernel creates one after boot, so
                // reaching here means the caller is walking the boot table.
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }

    /// Read `vpn`'s leaf entry.
    fn walk_read(&self, vpn: VirtPageNum) -> Option<Pte> {
        // SAFETY: the pointer, if any, addresses a live entry in a table this
        // object owns, and `&self` is enough to read it.
        self.walk_ptr(vpn).map(|p| unsafe { *p })
    }

    /// Borrow `vpn`'s leaf entry mutably.
    fn walk_mut(&mut self, vpn: VirtPageNum) -> Option<&mut Pte> {
        let ptr = self.walk_ptr(vpn)?;
        // SAFETY: `&mut self` proves nothing else is touching this table, and
        // the pointer addresses a live entry within it.
        Some(unsafe { &mut *ptr })
    }

    /// Map `vpn` to `ppn` with `flags`.
    ///
    /// `VALID` is implied. `ACCESSED`/`DIRTY` are set eagerly because not every
    /// implementation updates them in hardware, and a cleared bit faults.
    pub fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: PteFlags,
    ) -> Result<(), MapError> {
        self.map_at_level(vpn, ppn, flags, SV39_LEVELS - 1)
    }

    /// Map a leaf at an arbitrary level, creating a superpage when `level < 2`.
    ///
    /// A leaf above the bottom level covers 2 MiB (level 1) or 1 GiB (level 0)
    /// in a single entry. The kernel uses level-1 leaves for the linear map,
    /// where it cuts both page-table memory and TLB pressure by 512x; the
    /// hardware requires the physical frame to be aligned to the same size,
    /// which is asserted here rather than silently mis-mapped.
    pub fn map_at_level(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: PteFlags,
        level: usize,
    ) -> Result<(), MapError> {
        debug_assert!(level < SV39_LEVELS);
        let pages = Self::level_page_size(level) >> crate::config::PAGE_SHIFT;
        debug_assert!(vpn.0.is_multiple_of(pages), "misaligned virtual page for level {level}");
        debug_assert!(ppn.0.is_multiple_of(pages), "misaligned frame for level {level}");

        let pte = self.walk_create_to(vpn, level).ok_or(MapError::OutOfMemory)?;
        if pte.is_valid() {
            return Err(MapError::AlreadyMapped);
        }
        *pte = Pte::new(ppn, flags | PteFlags::VALID | PteFlags::ACCESSED | PteFlags::DIRTY);
        Ok(())
    }

    /// Remove `vpn`'s mapping and return the frame it pointed at.
    ///
    /// The caller owns that frame afterwards and is responsible for freeing it.
    pub fn unmap(&mut self, vpn: VirtPageNum) -> Result<PhysPageNum, MapError> {
        let pte = self.walk_mut(vpn).ok_or(MapError::NotMapped)?;
        if !pte.is_valid() {
            return Err(MapError::NotMapped);
        }
        let ppn = pte.ppn();
        *pte = Pte::EMPTY;
        Ok(ppn)
    }

    /// Look up `vpn` without modifying anything.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<Pte> {
        self.walk_read(vpn).filter(|pte| pte.is_valid())
    }

    /// Resolve a full virtual address, preserving the page offset.
    pub fn translate_addr(&self, va: VirtAddr) -> Option<PhysAddr> {
        let pte = self.translate(va.vpn())?;
        Some(PhysAddr(pte.ppn().base_addr().0 + va.page_offset()))
    }

    /// Change the permissions of an existing mapping.
    pub fn protect(&mut self, vpn: VirtPageNum, flags: PteFlags) -> Result<(), MapError> {
        let pte = self.walk_mut(vpn).ok_or(MapError::NotMapped)?;
        if !pte.is_valid() {
            return Err(MapError::NotMapped);
        }
        pte.set_flags(flags | PteFlags::VALID | PteFlags::ACCESSED | PteFlags::DIRTY);
        Ok(())
    }

    /// Borrow the leaf entry for `vpn`, for callers that need to edit it in
    /// place -- the copy-on-write fault handler, mainly.
    pub fn entry_mut(&mut self, vpn: VirtPageNum) -> Option<&mut Pte> {
        self.walk_mut(vpn).filter(|pte| pte.is_valid())
    }

    /// Copy the upper-half root entries of `kernel` into this table.
    ///
    /// Sv39 root entry *i* covers 1 GiB, and entries 256 and above are the
    /// upper half -- the kernel. Sharing those entries rather than duplicating
    /// the mappings means every address space sees the same kernel at the same
    /// addresses, which is what lets a trap from user mode run kernel code
    /// without first switching satp.
    ///
    /// The interior tables below these entries are shared, not owned, so this
    /// table's Drop will not free them.
    pub fn share_upper_half(&mut self, kernel: &PageTable) {
        // SAFETY: both frames are root tables owned by their respective
        // objects, and the halves being copied do not overlap this table's own
        // user mappings.
        unsafe {
            let dst = table_at(self.root);
            let src = table_at(kernel.root);
            dst[PTE_PER_TABLE / 2..].copy_from_slice(&src[PTE_PER_TABLE / 2..]);
        }
    }

    /// Number of frames this table occupies, root and interior included.
    pub fn table_frames(&self) -> usize {
        self.frames.len()
    }
}

impl fmt::Debug for PageTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PageTable {{ root: {:?}, {} frames }}", self.root, self.frames.len())
    }
}

//! Typed physical and virtual addresses.
//!
//! Mixing up a physical address, a virtual address and a page number is the
//! classic way to lose a weekend to a kernel bug, so they are four distinct
//! types here and conversions between them are explicit. All of it compiles
//! away -- every type is a `#[repr(transparent)]` newtype over `usize`.

use core::fmt;

use crate::config::{PAGE_SHIFT, PAGE_SIZE, SV39_LEVELS, VIRT_OFFSET};

/// A physical address: an index into DRAM or MMIO space.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct PhysAddr(pub usize);

/// A virtual address, subject to Sv39 translation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct VirtAddr(pub usize);

/// A physical address shifted right by `PAGE_SHIFT`: a frame number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct PhysPageNum(pub usize);

/// A virtual address shifted right by `PAGE_SHIFT`: a page number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct VirtPageNum(pub usize);

macro_rules! impl_addr_common {
    ($ty:ident) => {
        impl $ty {
            /// Offset within the containing page.
            #[inline(always)]
            pub const fn page_offset(self) -> usize {
                self.0 & (PAGE_SIZE - 1)
            }

            /// Is this address exactly on a page boundary?
            #[inline(always)]
            pub const fn is_aligned(self) -> bool {
                self.page_offset() == 0
            }

            /// Round down to the containing page boundary.
            #[inline(always)]
            pub const fn align_down(self) -> Self {
                Self(self.0 & !(PAGE_SIZE - 1))
            }

            /// Round up to the next page boundary.
            #[inline(always)]
            pub const fn align_up(self) -> Self {
                Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
            }

            /// Raw value.
            #[inline(always)]
            pub const fn as_usize(self) -> usize {
                self.0
            }
        }

        impl From<usize> for $ty {
            #[inline(always)]
            fn from(v: usize) -> Self {
                Self(v)
            }
        }

        impl From<$ty> for usize {
            #[inline(always)]
            fn from(v: $ty) -> usize {
                v.0
            }
        }
    };
}

impl_addr_common!(PhysAddr);
impl_addr_common!(VirtAddr);

impl PhysAddr {
    /// Frame number containing this address (rounds down).
    #[inline(always)]
    pub const fn ppn(self) -> PhysPageNum {
        PhysPageNum(self.0 >> PAGE_SHIFT)
    }

    /// Frame number, asserting the address is page aligned.
    #[inline(always)]
    pub const fn ppn_exact(self) -> PhysPageNum {
        debug_assert!(self.is_aligned());
        PhysPageNum(self.0 >> PAGE_SHIFT)
    }

    /// Where this physical address is readable in the kernel's direct map.
    #[inline(always)]
    pub const fn to_virt(self) -> VirtAddr {
        VirtAddr(self.0 + VIRT_OFFSET)
    }
}

impl VirtAddr {
    /// Page number containing this address (rounds down).
    #[inline(always)]
    pub const fn vpn(self) -> VirtPageNum {
        VirtPageNum(self.0 >> PAGE_SHIFT)
    }

    /// Page number, asserting the address is page aligned.
    #[inline(always)]
    pub const fn vpn_exact(self) -> VirtPageNum {
        debug_assert!(self.is_aligned());
        VirtPageNum(self.0 >> PAGE_SHIFT)
    }

    /// Reverse the direct map. Only meaningful for kernel addresses.
    #[inline(always)]
    pub const fn to_phys(self) -> PhysAddr {
        PhysAddr(self.0 - VIRT_OFFSET)
    }

    /// Does this address fall in the upper (kernel) half of the address space?
    #[inline(always)]
    pub const fn is_kernel(self) -> bool {
        self.0 >= VIRT_OFFSET
    }

    /// Sv39 requires bits 63:39 to replicate bit 38. An address that violates
    /// this faults with a page fault rather than being truncated, so syscall
    /// arguments are screened through here before use.
    #[inline(always)]
    pub const fn is_canonical(self) -> bool {
        let sign_extended = ((self.0 as i64) << 25 >> 25) as usize;
        sign_extended == self.0
    }
}

impl PhysPageNum {
    /// Address of the first byte of this frame.
    #[inline(always)]
    pub const fn base_addr(self) -> PhysAddr {
        PhysAddr(self.0 << PAGE_SHIFT)
    }

    /// Direct-map pointer to this frame's contents.
    #[inline(always)]
    pub const fn as_ptr(self) -> *mut u8 {
        (self.0 << PAGE_SHIFT).wrapping_add(VIRT_OFFSET) as *mut u8
    }

    /// Borrow the frame as a byte slice through the direct map.
    ///
    /// # Safety
    /// The caller must own the frame, or otherwise know that nothing else is
    /// writing it concurrently.
    #[inline]
    pub unsafe fn as_bytes(self) -> &'static mut [u8] {
        // SAFETY: contract delegated to the caller; the direct map guarantees
        // the whole frame is mapped and writable.
        unsafe { core::slice::from_raw_parts_mut(self.as_ptr(), PAGE_SIZE) }
    }

    /// Overwrite the whole frame with zeroes.
    ///
    /// # Safety
    /// The caller must own the frame.
    #[inline]
    pub unsafe fn zero(self) {
        // SAFETY: contract delegated; writes exactly one mapped page.
        unsafe { core::ptr::write_bytes(self.as_ptr(), 0, PAGE_SIZE) };
    }

    /// The next frame in physical order.
    #[inline(always)]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl VirtPageNum {
    /// Address of the first byte of this page.
    #[inline(always)]
    pub const fn base_addr(self) -> VirtAddr {
        VirtAddr(self.0 << PAGE_SHIFT)
    }

    /// Split the page number into its three 9-bit Sv39 table indices, from the
    /// root table down to the leaf.
    #[inline(always)]
    pub const fn indices(self) -> [usize; SV39_LEVELS] {
        let vpn = self.0;
        [(vpn >> 18) & 0x1ff, (vpn >> 9) & 0x1ff, vpn & 0x1ff]
    }

    /// The next page in virtual order.
    #[inline(always)]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Number of pages spanned by `[start, end)`, rounding the range outward.
    #[inline(always)]
    pub const fn range_len(start: VirtAddr, end: VirtAddr) -> usize {
        let s = start.align_down().0 >> PAGE_SHIFT;
        let e = end.align_up().0 >> PAGE_SHIFT;
        e - s
    }
}

// Hex formatting everywhere: kernel addresses are never meaningful in decimal.
impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PA({:#x})", self.0)
    }
}
impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VA({:#x})", self.0)
    }
}
impl fmt::Debug for PhysPageNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PPN({:#x})", self.0)
    }
}
impl fmt::Debug for VirtPageNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VPN({:#x})", self.0)
    }
}
impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}
impl fmt::Display for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}

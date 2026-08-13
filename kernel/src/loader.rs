//! Minimal ELF64 loader.
//!
//! Only what a static RISC-V executable needs: parse the header, walk the
//! program headers, and copy every `PT_LOAD` segment into a fresh address
//! space with the permissions the segment asks for. No dynamic linking, no
//! relocations -- user programs are linked at a fixed base.
//!
//! Every field is read with `from_le_bytes` out of a bounds-checked slice
//! rather than by casting the buffer to a struct pointer. The input is a file,
//! which means it is attacker-controlled the moment programs come off a disk;
//! a misaligned or truncated header must be an error, not a fault.

use crate::config::{PAGE_SIZE, USER_MAX_ADDR};
use crate::mm::addr::VirtAddr;
use crate::mm::page_table::PteFlags;
use crate::mm::space::{AddressSpace, Backing, Region};

/// ELF magic: 0x7f 'E' 'L' 'F'.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// 64-bit object.
const ELFCLASS64: u8 = 2;
/// Little-endian.
const ELFDATA2LSB: u8 = 1;
/// Executable file (as opposed to a shared object or relocatable).
const ET_EXEC: u16 = 2;
/// RISC-V.
const EM_RISCV: u16 = 243;
/// Program header type for a loadable segment.
const PT_LOAD: u32 = 1;

/// Segment permission flags.
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Why an image could not be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// Too short to contain the structure being read.
    Truncated,
    /// Not an ELF file.
    BadMagic,
    /// Not a 64-bit little-endian RISC-V executable.
    WrongFormat,
    /// A segment lies outside the user half of the address space.
    BadAddress,
    /// A segment's file size exceeds its memory size.
    BadSegment,
    /// Out of memory building the address space.
    OutOfMemory,
}

/// A loaded program: its address space and where to start executing.
#[derive(Debug)]
pub struct LoadedImage {
    /// The new address space, with every `PT_LOAD` segment mapped.
    pub space: AddressSpace,
    /// Value for `sepc`.
    pub entry: usize,
    /// First page-aligned address above every segment, where the heap starts.
    pub brk: usize,
}

/// Read a little-endian integer from `data` at `offset`.
macro_rules! read_int {
    ($ty:ty, $data:expr, $offset:expr) => {{
        const N: usize = core::mem::size_of::<$ty>();
        let off: usize = $offset;
        let end = off.checked_add(N).ok_or(LoadError::Truncated)?;
        let slice: &[u8] = $data;
        if end > slice.len() {
            return Err(LoadError::Truncated);
        }
        let mut buf = [0u8; N];
        buf.copy_from_slice(&slice[off..end]);
        <$ty>::from_le_bytes(buf)
    }};
}

/// Parse `image` and build an address space containing it.
pub fn load(image: &[u8]) -> Result<LoadedImage, LoadError> {
    if image.len() < 64 {
        return Err(LoadError::Truncated);
    }
    if image[0..4] != ELF_MAGIC {
        return Err(LoadError::BadMagic);
    }
    if image[4] != ELFCLASS64 || image[5] != ELFDATA2LSB {
        return Err(LoadError::WrongFormat);
    }

    let e_type = read_int!(u16, image, 16);
    let e_machine = read_int!(u16, image, 18);
    if e_type != ET_EXEC || e_machine != EM_RISCV {
        return Err(LoadError::WrongFormat);
    }

    let entry = read_int!(u64, image, 24) as usize;
    let phoff = read_int!(u64, image, 32) as usize;
    let phentsize = read_int!(u16, image, 54) as usize;
    let phnum = read_int!(u16, image, 56) as usize;

    let mut space = AddressSpace::new().ok_or(LoadError::OutOfMemory)?;
    // Kernel mappings must be present before the first trap out of this space,
    // and the trap can happen on the very first instruction.
    space.map_kernel_half().map_err(|_| LoadError::OutOfMemory)?;

    let mut brk = 0usize;

    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        let p_type = read_int!(u32, image, ph);
        if p_type != PT_LOAD {
            continue;
        }

        let p_flags = read_int!(u32, image, ph + 4);
        let p_offset = read_int!(u64, image, ph + 8) as usize;
        let p_vaddr = read_int!(u64, image, ph + 16) as usize;
        let p_filesz = read_int!(u64, image, ph + 32) as usize;
        let p_memsz = read_int!(u64, image, ph + 40) as usize;

        // A segment claiming more file bytes than memory bytes would have the
        // copy below run past the region it allocated.
        if p_filesz > p_memsz {
            return Err(LoadError::BadSegment);
        }

        let end = p_vaddr.checked_add(p_memsz).ok_or(LoadError::BadAddress)?;
        if end > USER_MAX_ADDR || p_vaddr < PAGE_SIZE {
            // Above the user half, or overlapping the guard page at zero that
            // makes a null dereference fault.
            return Err(LoadError::BadAddress);
        }

        let file_end = p_offset.checked_add(p_filesz).ok_or(LoadError::Truncated)?;
        if file_end > image.len() {
            return Err(LoadError::Truncated);
        }

        let mut perm = PteFlags::USER;
        if p_flags & PF_R != 0 {
            perm |= PteFlags::READ;
        }
        if p_flags & PF_W != 0 {
            perm |= PteFlags::WRITE;
        }
        if p_flags & PF_X != 0 {
            perm |= PteFlags::EXEC;
        }

        let region = Region::new(VirtAddr(p_vaddr), VirtAddr(end), Backing::Framed, perm);
        // Frames arrive zeroed, so the .bss tail of a segment -- memsz beyond
        // filesz -- needs no explicit clearing.
        space
            .push_with_offset(region, p_vaddr % PAGE_SIZE, &image[p_offset..file_end])
            .map_err(|_| LoadError::OutOfMemory)?;

        brk = brk.max(end.next_multiple_of(PAGE_SIZE));
    }

    if brk == 0 {
        return Err(LoadError::BadSegment);
    }

    Ok(LoadedImage { space, entry, brk })
}

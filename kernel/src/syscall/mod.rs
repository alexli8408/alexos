//! System call dispatch.
//!
//! Every syscall runs on the calling task's kernel stack with its address space
//! still installed, so user pointers are reachable -- but they are never
//! dereferenced directly. Each one is translated through the page table and
//! copied via the linear map, which turns a bad pointer into an error return
//! instead of a kernel page fault, and enforces that the user could have done
//! the access itself.
//!
//! `sepc` is advanced by 4 before dispatch. The `ecall` instruction leaves it
//! pointing *at* the `ecall`, so returning without advancing would re-execute
//! the syscall forever. Doing it up front rather than after means `fork`, whose
//! child resumes from a copy of this frame, does not need to remember to.

#[path = "../../../abi/syscall.rs"]
pub mod nr;

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::USER_MAX_ADDR;
use crate::mm::addr::VirtAddr;
use crate::task::scheduler::{self, current_task};
use crate::trap::TrapFrame;

/// Longest path or argument string a syscall will copy in.
const MAX_STR: usize = 256;
/// Largest single read or write.
const MAX_IO: usize = 64 * 1024;
/// Most arguments `exec` will accept.
const MAX_ARGS: usize = 32;

/// Dispatch one system call and return the value for `a0`.
pub fn dispatch(frame: &mut TrapFrame) -> isize {
    let id = frame.syscall_id();
    let a = [frame.arg(0), frame.arg(1), frame.arg(2)];

    match id {
        nr::SYS_EXIT => sys_exit(a[0] as i32),
        nr::SYS_WRITE => sys_write(a[0], a[1], a[2]),
        nr::SYS_READ => sys_read(a[0], a[1], a[2]),
        nr::SYS_YIELD => {
            scheduler::yield_now();
            0
        }
        nr::SYS_GETPID => current_task().pid.0 as isize,
        nr::SYS_GETPPID => sys_getppid(),
        nr::SYS_FORK => sys_fork(frame),
        nr::SYS_EXEC => sys_exec(a[0], a[1]),
        nr::SYS_WAITPID => sys_waitpid(a[0] as isize, a[1]),
        nr::SYS_SBRK => sys_sbrk(a[0] as isize),
        nr::SYS_UPTIME => crate::timer::uptime_ms() as isize,
        nr::SYS_PS => sys_ps(),
        _ => {
            crate::warn!("unknown syscall {id} from pid {}", current_task().pid);
            nr::ENOSYS
        }
    }
}

/// Terminate the caller.
fn sys_exit(code: i32) -> isize {
    scheduler::exit_current(code)
}

/// Copy a user buffer into the kernel, rejecting anything out of range.
fn read_user_buf(ptr: usize, len: usize) -> Option<Vec<u8>> {
    if len > MAX_IO || ptr.checked_add(len)? > USER_MAX_ADDR {
        return None;
    }
    let task = current_task();
    let inner = task.inner.lock();
    let space = inner.space.as_ref()?;

    let mut buf = alloc::vec![0u8; len];
    space.copy_from_user(VirtAddr(ptr), &mut buf)?;
    Some(buf)
}

fn sys_write(fd: usize, ptr: usize, len: usize) -> isize {
    if fd != nr::STDOUT && fd != nr::STDERR {
        return nr::EBADF;
    }
    let Some(buf) = read_user_buf(ptr, len) else {
        return nr::EFAULT;
    };
    // Write the bytes as-is. A user program's output is not the kernel's to
    // reinterpret, so no UTF-8 validation and no line-ending translation
    // beyond what the UART driver already does.
    crate::drivers::uart::write_bytes(&buf);
    len as isize
}

fn sys_read(fd: usize, ptr: usize, len: usize) -> isize {
    if fd != nr::STDIN {
        return nr::EBADF;
    }
    if len == 0 {
        return 0;
    }
    if len > MAX_IO || ptr.checked_add(len).is_none_or(|e| e > USER_MAX_ADDR) {
        return nr::EFAULT;
    }

    // Block until at least one byte is available, then take what is buffered.
    // Returning short is correct for a character device and is what makes a
    // line-at-a-time shell possible.
    crate::drivers::uart::READERS.wait_until(crate::drivers::uart::has_input);

    let mut buf = Vec::with_capacity(len);
    while buf.len() < len {
        match crate::drivers::uart::read_byte() {
            Some(b) => buf.push(b),
            None => break,
        }
    }

    let task = current_task();
    let inner = task.inner.lock();
    let Some(space) = inner.space.as_ref() else {
        return nr::EFAULT;
    };
    match space.copy_to_user(VirtAddr(ptr), &buf) {
        Some(()) => buf.len() as isize,
        None => nr::EFAULT,
    }
}

fn sys_getppid() -> isize {
    let task = current_task();
    let inner = task.inner.lock();
    match inner.parent.as_ref().and_then(|p| p.upgrade()) {
        Some(parent) => parent.pid.0 as isize,
        // Orphans are reparented to init conceptually; reporting 0 keeps the
        // convention that pid 0 is "no parent".
        None => 0,
    }
}

/// Duplicate the calling process.
///
/// The child gets a copy of the parent's address space and a copy of this trap
/// frame with `a0` forced to zero, which is how the two halves of a fork tell
/// themselves apart on return.
fn sys_fork(frame: &TrapFrame) -> isize {
    let parent = current_task();

    let child = match crate::task::Task::fork_from(&parent, frame) {
        Some(child) => child,
        None => return nr::ENOMEM,
    };
    let pid = child.pid.0 as isize;

    parent.inner.lock().children.push(child.clone());
    scheduler::admit(child);

    pid
}

/// Replace the current image.
fn sys_exec(path_ptr: usize, argv_ptr: usize) -> isize {
    let task = current_task();

    // Read the path and arguments out of the *old* space before it is torn
    // down -- every pointer here becomes meaningless the moment exec succeeds.
    let (path, args) = {
        let inner = task.inner.lock();
        let Some(space) = inner.space.as_ref() else {
            return nr::EFAULT;
        };
        let Some(path) = space.read_cstr(VirtAddr(path_ptr), MAX_STR) else {
            return nr::EFAULT;
        };
        let mut args: Vec<String> = Vec::new();
        if argv_ptr != 0 {
            for i in 0..MAX_ARGS {
                let mut slot = [0u8; 8];
                if space.copy_from_user(VirtAddr(argv_ptr + i * 8), &mut slot).is_none() {
                    return nr::EFAULT;
                }
                let p = usize::from_le_bytes(slot);
                if p == 0 {
                    break;
                }
                match space.read_cstr(VirtAddr(p), MAX_STR) {
                    Some(s) => args.push(s),
                    None => return nr::EFAULT,
                }
            }
        }
        (path, args)
    };

    let Some(image) = crate::programs::find(&path) else {
        return nr::ENOENT;
    };

    match crate::task::exec_current(&task, &path, image, &args) {
        Ok(()) => 0,
        Err(()) => nr::ENOMEM,
    }
}

/// Wait for a child to exit.
fn sys_waitpid(want: isize, status_ptr: usize) -> isize {
    let task = current_task();

    loop {
        // Scan for a zombie matching the request, and notice while we are here
        // whether any child could ever satisfy it.
        let (found, any_children) = {
            let mut inner = task.inner.lock();
            let mut found = None;
            for (i, child) in inner.children.iter().enumerate() {
                let matches = want < 0 || child.pid.0 as isize == want;
                if matches && child.state() == crate::task::TaskState::Zombie {
                    found = Some(i);
                    break;
                }
            }
            let any = if want < 0 {
                !inner.children.is_empty()
            } else {
                inner.children.iter().any(|c| c.pid.0 as isize == want)
            };
            (found.map(|i| inner.children.remove(i)), any)
        };

        if let Some(child) = found {
            let code = child.exit_code();
            if status_ptr != 0 {
                let inner = task.inner.lock();
                if let Some(space) = inner.space.as_ref() {
                    let _ = space.copy_to_user(VirtAddr(status_ptr), &code.to_le_bytes());
                }
            }
            // Last reference: dropping it here frees the child's kernel stack
            // and address space.
            return child.pid.0 as isize;
        }

        if !any_children {
            return nr::ECHILD;
        }

        // A child exists but has not exited. Sleep until one does; exit_current
        // wakes the parent.
        task.child_exit.wait();
    }
}

/// Move the program break.
fn sys_sbrk(delta: isize) -> isize {
    let task = current_task();
    match crate::task::adjust_brk(&task, delta) {
        Some(old) => old as isize,
        None => nr::ENOMEM,
    }
}

/// Dump the task table to the console.
fn sys_ps() -> isize {
    scheduler::dump_tasks();
    0
}

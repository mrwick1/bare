//! Memory pages straight from the OS, for the heap of the JavaScript engine (`bare-js2`).
//!
//! [`map`] gives `len` bytes of zeroed, read-write memory at an address aligned to [`GRANULE`]. [`unmap`] gives the whole region back
//! to the OS at once. The malloc of the C library keeps freed memory for later, so a heap that must return pages after churn maps its
//! blocks here.
//!
//! Unix maps `len + GRANULE` bytes with `mmap` and unmaps the unaligned ends. Windows reserves and commits with `VirtualAlloc`, whose
//! addresses are already aligned to 64 KiB (the allocation granularity). Under Miri, which runs no system calls, the memory comes from
//! `std::alloc` with the same alignment.

use std::io;
use std::ptr::NonNull;

/// The alignment of every region, and the unit of its length.
pub const GRANULE: usize = 64 * 1024;

/// Map `len` bytes (a multiple of [`GRANULE`], not 0): zeroed, read-write, aligned to [`GRANULE`].
pub fn map(len: usize) -> io::Result<NonNull<u8>> {
    if len == 0 || len % GRANULE != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the length is not a positive multiple of GRANULE"));
    }
    let p = os::map(len)?;
    debug_assert_eq!(p.as_ptr() as usize % GRANULE, 0);
    Ok(p)
}

/// Give a region of [`map`] back to the OS.
///
/// # Safety
/// `p` and `len` are exactly a region that [`map`] gave, and nothing uses it after this call.
pub unsafe fn unmap(p: NonNull<u8>, len: usize) -> io::Result<()> {
    // SAFETY: the caller gives a whole region of `map`.
    unsafe { os::unmap(p, len) }
}

#[cfg(miri)]
mod os {
    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    pub fn map(len: usize) -> io::Result<NonNull<u8>> {
        let l = Layout::from_size_align(len, GRANULE).map_err(io::Error::other)?;
        // SAFETY: `l` has a non-zero size.
        NonNull::new(unsafe { alloc_zeroed(l) }).ok_or_else(|| io::ErrorKind::OutOfMemory.into())
    }

    pub unsafe fn unmap(p: NonNull<u8>, len: usize) -> io::Result<()> {
        // SAFETY: `p` came from `map` with this layout.
        unsafe { dealloc(p.as_ptr(), Layout::from_size_align_unchecked(len, GRANULE)) };
        Ok(())
    }
}

#[cfg(all(unix, not(miri)))]
mod os {
    use super::*;
    use std::ffi::c_void;

    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const MAP_PRIVATE: i32 = 2;
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    const MAP_ANONYMOUS: i32 = 0x1000;
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")))]
    const MAP_ANONYMOUS: i32 = 0x20;

    unsafe extern "C" {
        fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }

    fn unmap_raw(a: usize, len: usize) -> io::Result<()> {
        // SAFETY: the caller owns the mapping `a..a + len`.
        if len == 0 || unsafe { munmap(a as *mut c_void, len) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }

    pub fn map(len: usize) -> io::Result<NonNull<u8>> {
        let total = len + GRANULE;
        // SAFETY: a new private anonymous mapping; no existing memory is touched.
        let p = unsafe { mmap(std::ptr::null_mut(), total, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
        if p as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        let a = p as usize;
        let start = a.next_multiple_of(GRANULE);
        unmap_raw(a, start - a)?;
        unmap_raw(start + len, a + total - (start + len))?;
        // The aligned part keeps the provenance of the mapping.
        Ok(NonNull::new(p.cast::<u8>().wrapping_add(start - a)).expect("mmap gave a non-null address"))
    }

    pub unsafe fn unmap(p: NonNull<u8>, len: usize) -> io::Result<()> {
        unmap_raw(p.as_ptr() as usize, len)
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use std::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 4;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(addr: *mut c_void, size: usize, kind: u32) -> i32;
    }

    pub fn map(len: usize) -> io::Result<NonNull<u8>> {
        // SAFETY: a new reservation; no existing memory is touched. The address is a multiple of the 64 KiB allocation granularity.
        let p = unsafe { VirtualAlloc(std::ptr::null_mut(), len, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        NonNull::new(p.cast::<u8>()).ok_or_else(io::Error::last_os_error)
    }

    pub unsafe fn unmap(p: NonNull<u8>, _len: usize) -> io::Result<()> {
        // SAFETY: `p` is the base of a whole reservation of `map`.
        if unsafe { VirtualFree(p.as_ptr().cast(), 0, MEM_RELEASE) } != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }
}

#[cfg(not(any(unix, windows, miri)))]
mod os {
    use super::*;

    pub fn map(_len: usize) -> io::Result<NonNull<u8>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub unsafe fn unmap(_p: NonNull<u8>, _len: usize) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_is_aligned_zeroed_writable_and_given_back() {
        let mut held = Vec::new();
        for n in [1, 2, 5] {
            let len = n * GRANULE;
            let p = map(len).unwrap();
            assert_eq!(p.as_ptr() as usize % GRANULE, 0);
            // SAFETY: `map` gave `len` read-write bytes.
            let s = unsafe { std::slice::from_raw_parts_mut(p.as_ptr(), len) };
            assert!(s.iter().all(|&b| b == 0));
            s[0] = 1;
            s[len - 1] = 2;
            held.push((p, len));
        }
        for (p, len) in held {
            // SAFETY: each region came from `map` and is no longer used.
            unsafe { unmap(p, len) }.unwrap();
        }
        assert!(map(0).is_err() && map(GRANULE + 1).is_err());
    }
}

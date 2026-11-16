//! The trust roots of the OS. `bare-tls` parses and checks them.
//!
//! Unix has PEM bundle files. Windows has the "ROOT" system certificate store.

use std::io;

/// PEM bundle paths on Unix, in order of use. (Only `/etc/ssl/certs/ca-certificates.crt` and
/// `/etc/ssl/cert.pem` were seen on the Arch machine of 2026-11-04. The Red Hat path is the
/// documented location. openSUSE has only `/etc/ssl/ca-bundle.pem`.)
pub const UNIX_PEM_PATHS: [&str; 4] =
    ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/cert.pem", "/etc/ssl/ca-bundle.pem"];

/// Where the OS keeps its roots.
#[derive(Debug)]
pub enum Roots {
    /// Candidate PEM files. The caller uses the first that exists.
    PemFiles(&'static [&'static str]),
    /// DER certificates, copied from the OS store.
    Der(Vec<Vec<u8>>),
}

/// Label for roots that come from the Windows store.
pub const WINDOWS_LABEL: &str = "Windows ROOT store";

#[cfg(unix)]
pub fn roots() -> io::Result<Roots> {
    Ok(Roots::PemFiles(&UNIX_PEM_PATHS))
}

#[cfg(windows)]
pub fn roots() -> io::Result<Roots> {
    use std::ffi::c_void;
    use std::ptr::null;

    #[repr(C)]
    struct CertContext {
        encoding: u32,
        encoded: *const u8,
        len: u32,
        info: *const c_void,
        store: *const c_void,
    }
    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CertOpenSystemStoreW(prov: usize, protocol: *const u16) -> *mut c_void;
        fn CertEnumCertificatesInStore(store: *mut c_void, prev: *const CertContext) -> *const CertContext;
        fn CertCloseStore(store: *mut c_void, flags: u32) -> i32;
    }

    let name: Vec<u16> = "ROOT\0".encode_utf16().collect();
    // SAFETY: `name` is NUL-terminated. A null result means failure.
    let store = unsafe { CertOpenSystemStoreW(0, name.as_ptr()) };
    if store.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    let mut prev = null();
    loop {
        // SAFETY: `prev` is null or the context the last call returned. The call frees it.
        let c = unsafe { CertEnumCertificatesInStore(store, prev) };
        if c.is_null() {
            break;
        }
        // SAFETY: `c` is valid until the next call. The encoded bytes are `len` long.
        out.push(unsafe { std::slice::from_raw_parts((*c).encoded, (*c).len as usize) }.to_vec());
        prev = c;
    }
    // SAFETY: `store` is open and no context is held.
    unsafe { CertCloseStore(store, 0) };
    Ok(Roots::Der(out))
}

#[cfg(not(any(unix, windows)))]
pub fn roots() -> io::Result<Roots> {
    Err(io::ErrorKind::Unsupported.into())
}

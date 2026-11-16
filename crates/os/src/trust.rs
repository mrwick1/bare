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

/// Ask the OS to build a chain for this peer chain (`chain[0]` is the leaf). Windows downloads a root
/// that it does not have yet when an application asks for a chain that needs it (automatic root update).
/// The new root lands in the store that [`roots`] reads. The caller reads the roots again and verifies
/// the chain itself. The answer of the OS about the chain is not used: only the fetch matters.
/// This does network I/O and can block for up to 10 s. Other OSes have no such fetch: `Unsupported`.
#[cfg(windows)]
pub fn fetch_missing_root(chain: &[&[u8]]) -> io::Result<()> {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    #[repr(C)]
    struct EnhKeyUsage {
        count: u32,
        ids: *const *const u8,
    }
    #[repr(C)]
    struct UsageMatch {
        kind: u32,
        usage: EnhKeyUsage,
    }
    // `CERT_CHAIN_PARA` without the fields that Windows 8 added: `cb_size` tells the OS which it is.
    #[repr(C)]
    struct ChainPara {
        cb_size: u32,
        usage: UsageMatch,
        policy: UsageMatch,
        url_timeout_ms: u32,
        check_freshness: i32,
        freshness_secs: u32,
    }
    // CERT_CHAIN_PARA up to `dwRevocationFreshnessTime` is 72 bytes on x64 and 40 on x86.
    const _: () = assert!(size_of::<ChainPara>() == if size_of::<usize>() == 8 { 72 } else { 40 });
    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CertOpenStore(prov: usize, enc: u32, crypt_prov: usize, flags: u32, para: *const c_void) -> *mut c_void;
        fn CertAddEncodedCertificateToStore(store: *mut c_void, enc: u32, der: *const u8, len: u32, disp: u32, out: *mut *const c_void) -> i32;
        fn CertGetCertificateChain(engine: *mut c_void, cert: *const c_void, time: *const c_void, extra: *mut c_void, para: *const ChainPara, flags: u32, reserved: *const c_void, out: *mut *const c_void) -> i32;
        fn CertFreeCertificateChain(chain: *const c_void);
        fn CertFreeCertificateContext(cert: *const c_void) -> i32;
        fn CertCloseStore(store: *mut c_void, flags: u32) -> i32;
    }
    const CERT_STORE_PROV_MEMORY: usize = 2;
    const X509_ASN_ENCODING: u32 = 1;
    const CERT_STORE_ADD_USE_EXISTING: u32 = 2;

    if chain.is_empty() || chain.iter().any(|c| u32::try_from(c.len()).is_err()) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: the memory provider takes no parameters. A null result means failure.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, null()) };
    if store.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut leaf = null();
    let mut result = Ok(());
    for (i, der) in chain.iter().enumerate() {
        let out: *mut *const c_void = if i == 0 { &mut leaf } else { null_mut() };
        // SAFETY: `der` is `len` bytes, and `len` fits `u32` (checked above). `out` is null or points to `leaf`, which lives to the end.
        if unsafe { CertAddEncodedCertificateToStore(store, X509_ASN_ENCODING, der.as_ptr(), der.len() as u32, CERT_STORE_ADD_USE_EXISTING, out) } == 0 {
            result = Err(io::Error::last_os_error());
            break;
        }
    }
    if result.is_ok() {
        // No `CERT_CHAIN_DISABLE_AUTH_ROOT_AUTO_UPDATE`, so Windows may download the root.
        // No revocation flag: no CRL or OCSP traffic.
        let para = ChainPara {
            cb_size: size_of::<ChainPara>() as u32,
            usage: UsageMatch { kind: 0, usage: EnhKeyUsage { count: 0, ids: null() } },
            policy: UsageMatch { kind: 0, usage: EnhKeyUsage { count: 0, ids: null() } },
            url_timeout_ms: 10_000,
            check_freshness: 0,
            freshness_secs: 0,
        };
        let mut ctx = null();
        // SAFETY: `leaf` is a context of the open store. `para` is valid for the call. `store` is the extra store.
        let ok = unsafe { CertGetCertificateChain(null_mut(), leaf, null(), store, &para, 0, null(), &mut ctx) };
        if ok == 0 {
            result = Err(io::Error::last_os_error());
        } else {
            // SAFETY: the call gave a chain context that nobody else holds.
            unsafe { CertFreeCertificateChain(ctx) };
        }
    }
    // SAFETY: `leaf` is null or a context that we own. The store is open and the chain is freed.
    unsafe {
        if !leaf.is_null() {
            CertFreeCertificateContext(leaf);
        }
        CertCloseStore(store, 0);
    }
    result
}

#[cfg(not(windows))]
pub fn fetch_missing_root(_chain: &[&[u8]]) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, windows))]
mod tests {
    /// The example.com chain of 2026-11-16: leaf, two issuing CAs and the cross-signed root.
    /// The leaf expires 2026-12-25. The test needs no valid date: the fetch only builds a chain.
    #[test]
    fn fetch_for_the_example_com_chain() {
        let chain: [&[u8]; 4] = [
            include_bytes!("../tests/data/example-leaf.der"),
            include_bytes!("../tests/data/example-ca1.der"),
            include_bytes!("../tests/data/example-ca2.der"),
            include_bytes!("../tests/data/example-ca3.der"),
        ];
        super::fetch_missing_root(&chain).unwrap();
        assert!(super::fetch_missing_root(&[]).is_err());
    }
}

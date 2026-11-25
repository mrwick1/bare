//! The OS layer for services other than the window. `docs/internal/decisions.md` ("Cross-platform core") asks for it.
//!
//! The core crates call these functions and never the OS. Each module has one function per service,
//! with one implementation per OS behind `cfg`.

pub mod dns;
pub mod fs;
pub mod metrics;
pub mod pages;
pub mod random;
pub mod trust;
pub mod tz;

/// The browser cache root. An empty BARE_CACHE_DIR disables disk caching.
pub fn cache_dir() -> Option<std::path::PathBuf> {
    use std::{env, path::PathBuf};

    if let Some(dir) = env::var_os("BARE_CACHE_DIR") {
        return (!dir.is_empty()).then(|| PathBuf::from(dir));
    }
    let path = |name| env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "windows") {
        path("LOCALAPPDATA").map(|dir| dir.join("bare").join("cache"))
    } else if cfg!(target_os = "macos") {
        path("HOME").map(|dir| dir.join("Library").join("Caches").join("bare"))
    } else if cfg!(target_os = "linux") {
        path("XDG_CACHE_HOME")
            .or_else(|| path("HOME").map(|dir| dir.join(".cache"))).map(|dir| dir.join("bare"))
    } else {
        None
    }
}

/// The root of the data that the browser keeps (the profiles). An empty BARE_DATA_DIR turns it off: no profile is saved.
pub fn data_dir() -> Option<std::path::PathBuf> {
    use std::{env, path::PathBuf};

    if let Some(dir) = env::var_os("BARE_DATA_DIR") {
        return (!dir.is_empty()).then(|| PathBuf::from(dir));
    }
    let path = |name| env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "windows") {
        path("LOCALAPPDATA").map(|dir| dir.join("bare"))
    } else if cfg!(target_os = "macos") {
        path("HOME").map(|dir| dir.join("Library").join("Application Support").join("bare"))
    } else if cfg!(target_os = "linux") {
        path("XDG_DATA_HOME")
            .or_else(|| path("HOME").map(|dir| dir.join(".local").join("share"))).map(|dir| dir.join("bare"))
    } else {
        None
    }
}

//! The OS layer for services other than the window. `docs/decisions.md` ("Cross-platform core") asks for it.
//!
//! The core crates call these functions and never the OS. Each module has one function per service,
//! with one implementation per OS behind `cfg`.

pub mod dns;
pub mod fs;
pub mod metrics;
pub mod random;
pub mod trust;

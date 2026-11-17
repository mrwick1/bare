//! Files that hold private data.

use std::{fs::File, io, path::Path};

/// Create a new file for writing. Fail if the path exists. On Unix only the owner can read or write the file (mode 0600).
/// Windows uses the default access of the directory.
pub fn create_private(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_private_file_is_new_and_owner_only() {
        let path = std::env::temp_dir().join(format!("bare-os-private-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        drop(super::create_private(&path).unwrap());
        assert!(super::create_private(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_file(path).unwrap();
    }
}

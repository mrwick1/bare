//! Files that hold private data.

use std::{fs::File, io, path::Path};

/// Create a new file for writing. Fail if the path exists. On Unix only the owner can read or write the file (mode 0600).
/// Windows uses the default access of the directory.
pub fn create_private(path: &Path) -> io::Result<File> {
    private(std::fs::OpenOptions::new().write(true).create_new(true)).open(path)
}

/// Open a file for reading and writing, and create it when it is missing (mode 0600 on Unix, as `create_private`).
pub fn open_private(path: &Path) -> io::Result<File> {
    private(std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)).open(path)
}

fn private(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Create a directory and its missing parents. On Unix only the owner can enter or list the new ones (mode 0700). Windows uses the
/// default access of the parent. A directory that exists keeps its mode.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
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

    #[test]
    fn a_private_dir_is_owner_only() {
        let root = std::env::temp_dir().join(format!("bare-os-private-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        super::create_private_dir(&root.join("a")).unwrap();
        super::create_private_dir(&root.join("a")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(root.join("a")).unwrap().permissions().mode() & 0o777, 0o700);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

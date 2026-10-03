use std::io::Read;
use std::path::Path;

/// Read a credential file only when it is a regular file that other users
/// cannot read or write. The checks run on the opened handle, not on the path.
#[cfg(unix)]
pub(crate) fn read_secret_file(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;

    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    // SAFETY: geteuid takes no pointers and has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() != euid {
        return Err(std::io::Error::other("owned by another user"));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(
            "readable or writable by other users; run chmod 600",
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

/// Windows has no mode bits; the profile directory's ACLs protect the file.
#[cfg(not(unix))]
pub(crate) fn read_secret_file(path: &Path) -> std::io::Result<String> {
    let mut contents = String::new();
    std::fs::File::open(path)?.read_to_string(&mut contents)?;
    Ok(contents)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn reads_a_private_file_and_refuses_a_shared_one() {
        let path = std::env::temp_dir().join(format!("herdr-secret-file-{}", std::process::id()));
        std::fs::write(&path, "secret\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_secret_file(&path).unwrap(), "secret\n");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let refused = read_secret_file(&path);
        let _ = std::fs::remove_file(&path);
        assert!(refused.is_err());
    }
}

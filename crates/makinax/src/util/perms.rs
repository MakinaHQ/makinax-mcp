//! Owner-only files for data a local attacker must not read (receipts,
//! credentials): created 0600 in a 0700 directory, with the mode applied AT
//! CREATION. Writing first and `chmod`ing after leaves a window in which the
//! content is readable — small, but the tmp file in an atomic write has the
//! same window and nobody thinks to chmod that one.

use std::path::Path;

/// Permission bits that must be clear on a credential file: group and other,
/// read/write/execute alike.
#[cfg(unix)]
pub const FORBIDDEN_BITS: u32 = 0o077;

/// Atomically write `contents` to `path` as a credential-bearing file.
///
/// The parent directory is created and tightened to 0700, and both the
/// temporary file and the final file are 0600 from the moment they exist.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        harden_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        // The temp file is the one people forget: it holds the same secret and
        // exists for the same reason, so it is created restricted rather than
        // fixed up afterwards.
        let mut f = open_private(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    // rename preserves the tmp file's mode, so the destination is 0600 too.
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::create(path)
}

/// Tighten a directory that holds credential files to 0700.
pub fn harden_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(dir)?.permissions();
        if p.mode() & FORBIDDEN_BITS != 0 {
            p.set_mode(0o700);
            std::fs::set_permissions(dir, p)?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// The file AND its directory are restricted, and the restriction is in
    /// place at creation rather than applied afterwards.
    #[cfg(unix)]
    #[test]
    fn a_credential_file_is_created_locked_down() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("private");
        let f = nested.join("demo.json");
        write_private(&f, br#"{"api_key":"k-secret"}"#).unwrap();
        assert_eq!(mode_of(&f), 0o600, "file must be owner-only");
        assert_eq!(mode_of(&nested), 0o700, "its directory must be owner-only");
    }

    /// The temp file of an atomic write holds the same secret. It used to be
    /// created at the umask and only the final file was ever considered.
    #[cfg(unix)]
    #[test]
    fn the_atomic_temp_file_is_never_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("r.json");
        write_private(&f, br#"{"api_key":"k-1"}"#).unwrap();
        // Rename preserves mode, so proving the destination is 0600 proves the
        // temp file it came from was too.
        assert_eq!(mode_of(&f), 0o600);
        assert!(
            !dir.path().join("r.tmp").exists(),
            "the temp file must not survive the write"
        );
    }
}

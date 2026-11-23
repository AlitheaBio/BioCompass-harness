//! Retain a host-owned upload after checking that the pinned validator approved the same bytes.
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};

pub const MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;

pub fn file_digest(path: &Path) -> io::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or oversized version archive",
        ));
    }
    let mut sha = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut sha)?;
    Ok(hex::encode(sha.finalize()))
}

fn check(path: &Path, sha: &str) -> io::Result<()> {
    if file_digest(path)? != sha {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive differs from its validated digest",
        ));
    }
    Ok(())
}

fn copy_checked(upload: &Path, directory: &Path, destination: &Path, sha: &str) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::copy(&mut fs::File::open(upload)?, temporary.as_file_mut())?;
    check(temporary.path(), sha)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(destination) {
        Ok(_) => Ok(()),
        Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => check(destination, sha),
        Err(err) => Err(err.error),
    }
}

pub fn retain_validated(
    upload: &Path,
    rendered: &Path,
    directory: &Path,
    sha: &str,
) -> io::Result<()> {
    // Equality to the computed digests also ensures the name is exactly 64 lowercase hex characters.
    check(upload, sha)?;
    check(rendered, sha)?;
    fs::create_dir_all(directory)?;
    let destination = directory.join(sha);
    // Linux protects worker-owned files against hard links. Only link the app's upload;
    // the worker saw it through its read-only input mount.
    match fs::hard_link(upload, &destination) {
        Ok(()) => fs::File::open(&destination)?.sync_all()?,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => check(&destination, sha)?,
        Err(_) => copy_checked(upload, directory, &destination, sha)?,
    }
    fs::File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_local_copy_preserves_bytes_and_existing_objects() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("upload");
        fs::write(&source, b"validated bytes").unwrap();
        let sha = file_digest(&source).unwrap();
        let target = root.path().join(&sha);
        copy_checked(&source, root.path(), &target, &sha).unwrap();
        copy_checked(&source, root.path(), &target, &sha).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"validated bytes");
        fs::write(&target, b"different").unwrap();
        assert!(copy_checked(&source, root.path(), &target, &sha).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"different");
    }

    #[test]
    fn both_archives_must_match_before_any_file_is_retained() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let rendered = root.path().join("validated");
        let dst = root.path().join("objects");
        fs::write(&original, b"original").unwrap();
        fs::write(&rendered, b"different").unwrap();
        for digest in [
            file_digest(&original).unwrap(),
            file_digest(&rendered).unwrap(),
        ] {
            assert!(retain_validated(&original, &rendered, &dst, &digest).is_err());
            assert!(!dst.exists());
        }
    }
}

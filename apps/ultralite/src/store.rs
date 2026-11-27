use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn config_dir() -> PathBuf {
    directories::BaseDirs::new().expect("User data directory is required")
        .data_local_dir().join("SteamUltraLite")
}

fn prepare_parent(path: &Path) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing parent"))?;
    for ancestor in parent.ancestors().filter(|item| !item.as_os_str().is_empty()) {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if local::is_symlink_or_reparse(&meta) || !meta.is_dir() => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsafe data directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    std::fs::create_dir_all(parent)?;
    for ancestor in parent.ancestors().filter(|item| !item.as_os_str().is_empty()) {
        let meta = std::fs::symlink_metadata(ancestor)?;
        if local::is_symlink_or_reparse(&meta) || !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsafe data directory"));
        }
    }
    Ok(parent)
}

pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = prepare_parent(path)?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if local::is_symlink_or_reparse(&meta) || !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsafe data file"));
        }
    }
    let tmp = parent.join(format!(".cloud-{}-{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(tmp); }
    result
}

pub fn copy_private(src: &Path, dst: &Path) -> io::Result<()> {
    prepare_parent(dst)?;
    let mut input = std::fs::File::open(src)?;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_replaces_and_backup_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let snapshot = root.path().join("cloud/record.json");
        write_private(&snapshot, b"first").unwrap();
        write_private(&snapshot, b"second").unwrap();
        assert_eq!(std::fs::read(&snapshot).unwrap(), b"second");
        let backup = root.path().join("backup/record.json");
        copy_private(&snapshot, &backup).unwrap();
        assert!(copy_private(&snapshot, &backup).is_err());
        assert_eq!(std::fs::read(&backup).unwrap(), b"second");
    }
}

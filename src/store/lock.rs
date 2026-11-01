use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Advisory exclusive lock on a card. The lock lives as long as this value: the `File`
/// must stay open, because closing the descriptor releases the `flock`.
#[cfg(unix)]
pub struct CardLock {
    file: std::fs::File,
    lock_path: PathBuf,
}

#[cfg(unix)]
impl CardLock {
    pub fn lock_path_for(path: &Path) -> PathBuf {
        if path.is_file() || path.extension().is_some() {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("card");
            path.with_file_name(format!(".{}.lock", stem))
        } else {
            path.join(".lock")
        }
    }

    pub fn acquire_exclusive(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;
        const LOCK_EX: i32 = 2;
        const LOCK_UN: i32 = 8;

        // Hidden sibling (`.tk-001-x.lock`) so lock files stay out of Obsidian and `ls`.
        let lock_path = Self::lock_path_for(path);

        for _ in 0..64 {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .with_context(|| format!("Failed to open lock file {:?}", lock_path))?;

            let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
            if rc != 0 {
                bail!("Failed to acquire exclusive lock on {:?}", lock_path);
            }

            // Verify that the locked descriptor still matches the path on disk
            // (a prior holder unlinks `lock_path` inside `LOCK_EX` right before `LOCK_UN`).
            if let (Ok(fd_meta), Ok(path_meta)) = (file.metadata(), std::fs::metadata(&lock_path))
                && fd_meta.dev() == path_meta.dev()
                && fd_meta.ino() == path_meta.ino()
            {
                return Ok(Self { file, lock_path });
            }

            unsafe { flock(file.as_raw_fd(), LOCK_UN) };
        }

        bail!("Failed to stabilize exclusive lock on {:?}", lock_path)
    }
}

#[cfg(unix)]
impl Drop for CardLock {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;
        const LOCK_UN: i32 = 8;

        // Unlink while still holding `LOCK_EX` so 0-byte `.lock` files never accumulate in `tasks/`.
        if let (Ok(fd_meta), Ok(path_meta)) =
            (self.file.metadata(), std::fs::metadata(&self.lock_path))
            && fd_meta.dev() == path_meta.dev()
            && fd_meta.ino() == path_meta.ino()
        {
            let _ = std::fs::remove_file(&self.lock_path);
        }

        unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
    }
}

/// No-op on non-Unix platforms: concurrent agents are NOT serialized on Windows.
#[cfg(not(unix))]
pub struct CardLock;

#[cfg(not(unix))]
impl CardLock {
    pub fn lock_path_for(path: &Path) -> PathBuf {
        path.with_extension("lock")
    }

    pub fn acquire_exclusive(_path: &Path) -> Result<Self> {
        Ok(Self)
    }
}

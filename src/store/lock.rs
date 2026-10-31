use anyhow::{Context, Result, bail};
use std::path::Path;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Advisory exclusive lock on a card. The lock lives as long as this value: the `File`
/// must stay open, because closing the descriptor releases the `flock`.
#[cfg(unix)]
pub struct CardLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl CardLock {
    pub fn acquire_exclusive(path: &Path) -> Result<Self> {
        use std::os::unix::io::AsRawFd;
        const LOCK_EX: i32 = 2;

        // Hidden sibling (`.tk-001-x.lock`) so lock files stay out of Obsidian and `ls`.
        let lock_path = if path.is_file() || path.extension().is_some() {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("card");
            path.with_file_name(format!(".{}.lock", stem))
        } else {
            path.join(".lock")
        };

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
        Ok(Self { file })
    }
}

#[cfg(unix)]
impl Drop for CardLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        const LOCK_UN: i32 = 8;
        unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
    }
}

/// No-op on non-Unix platforms: concurrent agents are NOT serialized on Windows.
#[cfg(not(unix))]
pub struct CardLock;

#[cfg(not(unix))]
impl CardLock {
    pub fn acquire_exclusive(_path: &Path) -> Result<Self> {
        Ok(Self)
    }
}

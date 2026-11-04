//! The journal on disk. The writer holds both files open without sharing write access, so the kernel
//! enforces a single writer and releases the lock if the process dies.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use augeo_ports::{JournalFile, PortError, Store};

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// The journal files in a directory.
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
    primary: Option<File>,
    mirror: Option<File>,
}

impl FileStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            primary: None,
            mirror: None,
        }
    }

    fn path(&self, file: JournalFile) -> PathBuf {
        self.dir.join(match file {
            JournalFile::Primary => "journal.log",
            JournalFile::Mirror => "journal.mirror.log",
        })
    }

    fn writer(&mut self, file: JournalFile) -> Result<&mut File, PortError> {
        let slot = match file {
            JournalFile::Primary => &mut self.primary,
            JournalFile::Mirror => &mut self.mirror,
        };
        slot.as_mut()
            .ok_or_else(|| PortError::Failed("the journal isn't open for writing".into()))
    }

    fn open_writer(&self, file: JournalFile) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ)
            .open(self.path(file))
    }
}

fn io_error(context: &str, error: &std::io::Error) -> PortError {
    let text = format!("{context}: {error}");
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        PortError::Denied(text)
    } else {
        PortError::Failed(text)
    }
}

impl Store for FileStore {
    fn try_lock(&mut self) -> Result<bool, PortError> {
        if self.primary.is_some() {
            return Ok(true);
        }
        let primary = match self.open_writer(JournalFile::Primary) {
            Ok(f) => f,
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => return Ok(false),
            Err(e) => return Err(io_error("opening the journal", &e)),
        };
        let mirror = match self.open_writer(JournalFile::Mirror) {
            Ok(f) => f,
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => return Ok(false),
            Err(e) => return Err(io_error("opening the journal mirror", &e)),
        };
        self.primary = Some(primary);
        self.mirror = Some(mirror);
        Ok(true)
    }

    fn load(&self, file: JournalFile) -> Result<Vec<u8>, PortError> {
        let opened = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(self.path(file));
        let mut handle = match opened {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error("reading the journal", &e)),
        };
        let mut bytes = Vec::new();
        handle
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("reading the journal", &e))?;
        Ok(bytes)
    }

    fn append(&mut self, file: JournalFile, data: &[u8]) -> Result<(), PortError> {
        let handle = self.writer(file)?;
        handle
            .seek(SeekFrom::End(0))
            .and_then(|_| handle.write_all(data))
            .map_err(|e| io_error("writing the journal", &e))
    }

    fn flush(&mut self, file: JournalFile) -> Result<(), PortError> {
        // std implements this with FlushFileBuffers.
        self.writer(file)?
            .sync_all()
            .map_err(|e| io_error("flushing the journal", &e))
    }

    fn truncate(&mut self, file: JournalFile, len: u64) -> Result<(), PortError> {
        self.writer(file)?
            .set_len(len)
            .map_err(|e| io_error("repairing the journal", &e))
    }

    fn preserve(&mut self, file: JournalFile) -> Result<(), PortError> {
        let source = self.path(file);
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let target = (1u32..10_000)
            .map(|n| self.dir.join(format!("{name}.damaged-{n}")))
            .find(|p| !p.exists())
            .ok_or_else(|| PortError::Failed("too many preserved journal copies".into()))?;
        fs::copy(&source, target)
            .map(|_| ())
            .map_err(|e| io_error("keeping a copy of the damaged journal", &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("augeo-test-store-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn appended_bytes_survive_reopening() {
        let scratch = Scratch::new("reopen");
        let mut store = FileStore::new(&scratch.0);
        assert!(store.try_lock().unwrap());
        store.append(JournalFile::Primary, b"hello ").unwrap();
        store.flush(JournalFile::Primary).unwrap();
        store.append(JournalFile::Primary, b"world").unwrap();
        store.flush(JournalFile::Primary).unwrap();
        drop(store);

        let store = FileStore::new(&scratch.0);
        assert_eq!(store.load(JournalFile::Primary).unwrap(), b"hello world");
        assert_eq!(store.load(JournalFile::Mirror).unwrap(), b"");
    }

    #[test]
    fn missing_files_load_as_empty() {
        let scratch = Scratch::new("missing");
        let store = FileStore::new(&scratch.0);
        assert_eq!(store.load(JournalFile::Primary).unwrap(), b"");
    }

    #[test]
    fn only_one_writer_at_a_time_and_readers_still_work() {
        let scratch = Scratch::new("lock");
        let mut first = FileStore::new(&scratch.0);
        assert!(first.try_lock().unwrap());
        first.append(JournalFile::Primary, b"data").unwrap();
        first.flush(JournalFile::Primary).unwrap();

        let mut second = FileStore::new(&scratch.0);
        assert!(!second.try_lock().unwrap());
        assert!(second.append(JournalFile::Primary, b"x").is_err());
        // A reader sees the data while the writer holds the lock.
        assert_eq!(second.load(JournalFile::Primary).unwrap(), b"data");

        // The lock goes away with the holder.
        drop(first);
        assert!(second.try_lock().unwrap());
    }

    #[test]
    fn locking_is_idempotent() {
        let scratch = Scratch::new("idempotent");
        let mut store = FileStore::new(&scratch.0);
        assert!(store.try_lock().unwrap());
        assert!(store.try_lock().unwrap());
    }

    #[test]
    fn truncate_and_preserve() {
        let scratch = Scratch::new("truncate");
        let mut store = FileStore::new(&scratch.0);
        store.try_lock().unwrap();
        store.append(JournalFile::Mirror, b"0123456789").unwrap();
        store.flush(JournalFile::Mirror).unwrap();

        store.preserve(JournalFile::Mirror).unwrap();
        store.preserve(JournalFile::Mirror).unwrap();
        store.truncate(JournalFile::Mirror, 4).unwrap();
        assert_eq!(store.load(JournalFile::Mirror).unwrap(), b"0123");

        store.append(JournalFile::Mirror, b"X").unwrap();
        store.flush(JournalFile::Mirror).unwrap();
        assert_eq!(store.load(JournalFile::Mirror).unwrap(), b"0123X");

        let copy = scratch.0.join("journal.mirror.log.damaged-2");
        assert_eq!(fs::read(copy).unwrap(), b"0123456789");
    }
}

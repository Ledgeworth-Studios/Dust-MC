//! Where a region file's bytes actually live.
//!
//! [`RegionFile`](crate::region::RegionFile) is written against this trait
//! rather than against `std::fs::File` for one reason that is worth the extra
//! type parameter: the corruption tests. Every failure this crate is supposed
//! to name — an offset past the end, two chunks over each other, a declared
//! length longer than the file — is a specific arrangement of bytes, and the
//! honest way to test the handling of it is to build those bytes and hand them
//! over. Doing that through the filesystem means a temporary directory, a
//! cleanup path, and a test that fails differently on a full disk.
//!
//! The external `.mcc` files are part of the trait rather than sitting outside
//! it because they are part of the format. A chunk whose payload is in a
//! sibling file is still one chunk with one location entry, and a store that
//! could not answer for it would push the filesystem back into
//! [`RegionFile`](crate::region::RegionFile) through the side door.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use crate::coords::{ChunkPos, RegionPos};

/// Random-access bytes, plus the sibling files an oversized chunk uses.
pub trait RegionStore {
    /// The length of the region file in bytes.
    fn length(&mut self) -> io::Result<u64>;

    /// Fill `buf` from `offset`, or fail.
    ///
    /// A short read is an error and not a partial success. Every caller here is
    /// reading a structure of known size, and a caller that got half of one
    /// would carry on with a zeroed remainder.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Write `data` at `offset`, extending the file if it ends before then.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()>;

    /// The contents of the `.mcc` file for a chunk, or `None` if there is none.
    fn read_external(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>>;

    /// Write a chunk's `.mcc` file.
    fn write_external(&mut self, pos: ChunkPos, data: &[u8]) -> io::Result<()>;

    /// Delete a chunk's `.mcc` file if it has one.
    fn remove_external(&mut self, pos: ChunkPos) -> io::Result<()>;
}

/// A region file on disk, and the directory its `.mcc` siblings live in.
#[derive(Debug)]
pub struct FileStore {
    file: File,
    directory: PathBuf,
}

impl FileStore {
    /// Open a region file, creating it if it is not there.
    pub fn open(directory: impl AsRef<Path>, region: RegionPos) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        std::fs::create_dir_all(&directory)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(region.file_name()))?;
        Ok(Self { file, directory })
    }

    /// Open an existing region file for reading and writing, failing if it is
    /// not there.
    pub fn open_existing(directory: impl AsRef<Path>, region: RegionPos) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        let file = File::options()
            .read(true)
            .write(true)
            .open(directory.join(region.file_name()))?;
        Ok(Self { file, directory })
    }

    fn external_path(&self, pos: ChunkPos) -> PathBuf {
        self.directory.join(pos.external_file_name())
    }
}

impl RegionStore for FileStore {
    fn length(&mut self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)
    }

    fn read_external(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.external_path(pos)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_external(&mut self, pos: ChunkPos, data: &[u8]) -> io::Result<()> {
        // Written beside the region file and moved into place, so that a crash
        // during the write leaves the old payload rather than half a new one.
        // The region header still points at the old sectors until the caller
        // updates it, so the window in which the two disagree is this move.
        let final_path = self.external_path(pos);
        let temporary = final_path.with_extension("mcc.tmp");
        std::fs::write(&temporary, data)?;
        std::fs::rename(&temporary, &final_path)
    }

    fn remove_external(&mut self, pos: ChunkPos) -> io::Result<()> {
        match std::fs::remove_file(self.external_path(pos)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// A region file in memory, for tests that need to arrange exact bytes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MemoryStore {
    bytes: Vec<u8>,
    external: BTreeMap<(i32, i32), Vec<u8>>,
}

impl MemoryStore {
    /// An empty file that exists only in memory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store over bytes that already exist.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            external: BTreeMap::new(),
        }
    }

    /// The whole file's contents.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The whole file's contents, taken.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Cut the file short, as a crash mid-write does.
    pub fn truncate(&mut self, length: usize) {
        self.bytes.truncate(length);
    }
}

impl RegionStore for MemoryStore {
    fn length(&mut self) -> io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset does not fit in memory")
        })?;
        let end = start.checked_add(buf.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "read runs past the address space",
            )
        })?;
        if end > self.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "wanted bytes {start}..{end} of a {}-byte region file",
                    self.bytes.len()
                ),
            ));
        }
        buf.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset does not fit in memory")
        })?;
        let end = start + data.len();
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[start..end].copy_from_slice(data);
        Ok(())
    }

    fn read_external(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        Ok(self.external.get(&(pos.x, pos.z)).cloned())
    }

    fn write_external(&mut self, pos: ChunkPos, data: &[u8]) -> io::Result<()> {
        self.external.insert((pos.x, pos.z), data.to_vec());
        Ok(())
    }

    fn remove_external(&mut self, pos: ChunkPos) -> io::Result<()> {
        self.external.remove(&(pos.x, pos.z));
        Ok(())
    }
}

//! Where a pack's bytes come from: a directory or a zip.
//!
//! A datapack ships as either, and an operator moves one to the other by
//! zipping it, so the two must load identically. That is enforced by shape:
//! [`PackSource`] answers only "what files are in you" and "give me this one",
//! and every rule about what a file *means* lives above it, in one place, run
//! over both. A test loads the same synthetic pack as a directory and as a zip
//! and asserts the two results are equal.
//!
//! # The base layer has no `pack.mcmeta`
//!
//! Vanilla's data is not a datapack an operator installed; it is what the
//! server is. The extracted tree at `.dust-extract/data-<version>/` has no
//! `pack.mcmeta` at its root because Minecraft's own built-in pack describes
//! itself from inside the jar. So [`DirectoryPack::builtin`] exists: it takes
//! the format as a parameter instead of reading one. The distinction is kept in
//! the type rather than by making `pack.mcmeta` optional for everybody,
//! because for an operator's pack a missing `pack.mcmeta` is a real mistake —
//! Minecraft will not load such a pack at all — and quietly assuming a format
//! for it would hide that.

use std::path::{Path, PathBuf};

use crate::zip::{ZipArchive, ZipEntry, ZipError};

/// The deepest a pack's directory tree may nest.
///
/// A symlink loop inside a pack directory is otherwise an infinite walk. The
/// real vanilla tree is six deep at `data/minecraft/tags/worldgen/biome/
/// has_structure/x.json`; thirty-two leaves room for anything sane and turns a
/// loop into a message.
pub const MAX_DEPTH: usize = 32;

/// Why a pack's bytes could not be got at.
#[derive(Debug)]
pub enum PackError {
    Io {
        path: String,
        source: std::io::Error,
    },
    Zip {
        archive: String,
        source: ZipError,
    },
    TooDeep {
        path: String,
    },
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "could not read {path}: {source}"),
            Self::Zip { archive, source } => write!(f, "{archive} {source}"),
            Self::TooDeep { path } => write!(
                f,
                "{path} nests more than {MAX_DEPTH} directories deep. If that is \
                 not a mistake it is a symlink pointing back at itself."
            ),
        }
    }
}

impl std::error::Error for PackError {}

/// Somewhere a pack's files can be read from.
pub trait PackSource: std::fmt::Debug {
    /// A short name for the pack, used in every finding. The directory or file
    /// name, not the full path, because it appears on every line.
    fn id(&self) -> &str;

    /// The full path, said once in the load summary.
    fn origin(&self) -> String;

    /// Every file in the pack, `/`-separated and relative to the pack root.
    /// Directories are not listed.
    fn list(&self) -> Result<Vec<String>, PackError>;

    /// One file, or `Ok(None)` when there is no such file.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError>;

    /// The pack format to assume when the pack has no `pack.mcmeta`.
    ///
    /// `None` — the default, and the right answer for anything an operator
    /// installed — makes a missing `pack.mcmeta` a finding. Only the built-in
    /// base layer overrides it; see the module documentation.
    fn assumed_format(&self) -> Option<u32> {
        None
    }
}

/// A pack that is a directory on disk.
#[derive(Debug)]
pub struct DirectoryPack {
    root: PathBuf,
    id: String,
    /// `Some` for the built-in base layer, whose format is not in a file.
    assumed_format: Option<u32>,
}

impl DirectoryPack {
    /// An operator's pack. Its `pack.mcmeta` is expected to exist.
    pub fn open(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            id: directory_id(&root),
            root,
            assumed_format: None,
        }
    }

    /// The server's own base layer, whose format comes from the build rather
    /// than from a file. See the module documentation.
    pub fn builtin(root: impl Into<PathBuf>, id: impl Into<String>, format: u32) -> Self {
        Self {
            root: root.into(),
            id: id.into(),
            assumed_format: Some(format),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn walk(&self, relative: &str, depth: usize, out: &mut Vec<String>) -> Result<(), PackError> {
        if depth > MAX_DEPTH {
            return Err(PackError::TooDeep {
                path: format!("{}/{relative}", self.root.display()),
            });
        }
        let directory = if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        };
        let entries = std::fs::read_dir(&directory).map_err(|source| PackError::Io {
            path: directory.display().to_string(),
            source,
        })?;
        // Read the whole directory before recursing, and sort it. The order the
        // filesystem hands entries back is not stable between machines, and a
        // load that reports its findings in a different order on every runner
        // is a load whose output cannot be diffed.
        let mut names: Vec<(String, bool)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| PackError::Io {
                path: directory.display().to_string(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| PackError::Io {
                path: entry.path().display().to_string(),
                source,
            })?;
            // `file_type` does not follow symlinks; `metadata` does. A pack
            // whose contents are symlinked in is a real thing operators do, so
            // links are followed and MAX_DEPTH is what stops a loop.
            let is_directory = if file_type.is_symlink() {
                std::fs::metadata(entry.path())
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            names.push((
                entry.file_name().to_string_lossy().into_owned(),
                is_directory,
            ));
        }
        names.sort();

        for (name, is_directory) in names {
            let child = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if is_directory {
                self.walk(&child, depth + 1, out)?;
            } else {
                out.push(child);
            }
        }
        Ok(())
    }
}

impl PackSource for DirectoryPack {
    fn id(&self) -> &str {
        &self.id
    }

    fn assumed_format(&self) -> Option<u32> {
        self.assumed_format
    }

    fn origin(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self) -> Result<Vec<String>, PackError> {
        let mut out = Vec::new();
        self.walk("", 0, &mut out)?;
        Ok(out)
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError> {
        let full = self.root.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PackError::Io {
                path: full.display().to_string(),
                source,
            }),
        }
    }
}

/// A pack that is a `.zip`.
#[derive(Debug)]
pub struct ZipPack {
    archive: ZipArchive,
    id: String,
    origin: String,
}

impl ZipPack {
    /// Read the archive's directory. Entry contents are read on demand.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| PackError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_bytes(bytes, directory_id(path), path.display().to_string())
    }

    /// The same, from bytes already in hand — which is what the tests use, and
    /// what a pack downloaded rather than installed would use.
    pub fn from_bytes(
        bytes: Vec<u8>,
        id: impl Into<String>,
        origin: impl Into<String>,
    ) -> Result<Self, PackError> {
        let id = id.into();
        let archive = ZipArchive::open(bytes).map_err(|source| PackError::Zip {
            archive: id.clone(),
            source,
        })?;
        Ok(Self {
            archive,
            id,
            origin: origin.into(),
        })
    }

    fn entry(&self, path: &str) -> Option<&ZipEntry> {
        self.archive
            .entries()
            .iter()
            .find(|entry| entry.name == path)
    }
}

impl PackSource for ZipPack {
    fn id(&self) -> &str {
        &self.id
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }

    fn list(&self) -> Result<Vec<String>, PackError> {
        let mut names: Vec<String> = self
            .archive
            .entries()
            .iter()
            .filter(|entry| !entry.is_directory())
            .map(|entry| entry.name.clone())
            .collect();
        // Sorted for the same reason the directory walk is sorted: a zip's
        // entry order is whatever the writer felt like.
        names.sort();
        Ok(names)
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError> {
        match self.entry(path) {
            None => Ok(None),
            Some(entry) => self
                .archive
                .read(entry)
                .map(Some)
                .map_err(|source| PackError::Zip {
                    archive: self.id.clone(),
                    source,
                }),
        }
    }
}

/// Open whichever kind of pack is at `path`.
///
/// A `.zip` by extension is read as an archive; anything else as a directory.
/// By extension rather than by sniffing the first bytes, because a directory
/// has no first bytes and an operator who names a directory `foo.zip` has made
/// a mistake worth being told about rather than worked around.
pub fn open(path: impl AsRef<Path>) -> Result<Box<dyn PackSource>, PackError> {
    let path = path.as_ref();
    let is_zip = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    if is_zip {
        Ok(Box::new(ZipPack::open(path)?))
    } else {
        Ok(Box::new(DirectoryPack::open(path)))
    }
}

fn directory_id(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_is_named_after_its_directory_and_not_its_whole_path() {
        let pack = DirectoryPack::open("/somewhere/deep/my_pack");
        assert_eq!(pack.id(), "my_pack");
        assert!(pack.origin().contains("/somewhere/deep/"));
    }

    #[test]
    fn a_missing_directory_is_an_error_that_names_it() {
        let pack = DirectoryPack::open("/nowhere/at/all/definitely_not_here");
        let error = pack.list().expect_err("no such directory");
        assert!(error.to_string().contains("definitely_not_here"), "{error}");
    }

    #[test]
    fn reading_a_file_that_is_not_there_is_not_an_error() {
        // A pack with no `pack.mcmeta` is a finding decided above this layer,
        // not an io failure. Folding the two together would make "the disk is
        // broken" and "the author forgot a file" the same message.
        let pack = DirectoryPack::open(std::env::temp_dir());
        assert!(pack
            .read("definitely_not_a_real_file.mcmeta")
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_builtin_layer_carries_its_format_instead_of_reading_one() {
        let pack = DirectoryPack::builtin("/data", "vanilla", 48);
        assert_eq!(pack.assumed_format(), Some(48));
        assert_eq!(DirectoryPack::open("/data").assumed_format(), None);
    }

    #[test]
    fn the_extension_decides_which_reader_is_used() {
        let temp = std::env::temp_dir();
        let opened = open(&temp).expect("a directory opens");
        assert_eq!(opened.origin(), temp.display().to_string());
    }
}

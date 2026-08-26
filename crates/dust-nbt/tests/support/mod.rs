//! Shared plumbing for the tests that use real Minecraft data.
//!
//! # Why there is a zip reader in here
//!
//! The external corpus this crate is checked against is the 1.21.1 server jar
//! in `.dust-extract/`. Mojang's files may not be committed — see
//! `Code Provenance.md` — so the tests read them out of the cache at run time
//! instead, which means reading a jar, which means reading a zip.
//!
//! It is a zip inside a zip: since 1.18 the published server jar is a *bundler*
//! whose real server, and therefore all of its data, lives at
//! `META-INF/versions/<version>/server-<version>.jar`. Both layers are deflate,
//! and `flate2` is already a dependency of the crate under test, so the only
//! part missing was a central-directory walk. That is what this is: about a
//! hundred lines, no new dependency, and it does not have to be a general zip
//! implementation because it has exactly one job.
//!
//! What it does **not** support: ZIP64, encryption, data descriptors, and any
//! compression method but stored and deflate. Every one of those would be a
//! reason to reach for a real zip crate; none of them appears in a Mojang jar.
//!
//! # The skip has to be visible
//!
//! A test that quietly passes when its fixture is missing is worse than no
//! test: it reports the same green as one that actually ran. The cache is
//! gitignored and will not exist on a fresh clone or in CI, so the tests here
//! must not fail — but they must not be silent either. [`skip`] writes to the
//! process's real stderr, which `libtest` does not capture, so the notice lands
//! in the output of a plain `cargo test` and not only under `--nocapture`.
//! `tests/vanilla.rs` has a check that this is still true.

#![allow(dead_code)] // Each test binary uses a different part of this module.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The Minecraft version the cache is expected to hold.
pub const VERSION: &str = "1.21.1";

/// Say, visibly, that a test could not run.
///
/// Writes to the process's stderr through `std::io::stderr`, which libtest
/// leaves alone — unlike `println!`, which it captures and discards for a
/// passing test.
pub fn skip(what: &str, why: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "SKIPPED {what}: {why}");
    let _ = stderr.flush();
}

/// The workspace root, found by walking up from this crate.
pub fn workspace_root() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/dust-nbt -> crates -> root
    path.pop();
    path.pop();
    path
}

/// The cached server jar, if the cache is there.
pub fn server_jar() -> Option<PathBuf> {
    let path = workspace_root()
        .join(".dust-extract")
        .join(format!("server-{VERSION}.jar"));
    path.is_file().then_some(path)
}

/// The inner jar's bytes, extracted from the bundler.
pub fn inner_jar(bundler: &Path) -> std::io::Result<Vec<u8>> {
    let outer = std::fs::read(bundler)?;
    let archive = Zip::open(&outer).expect("the server jar is a readable zip");
    let name = format!("META-INF/versions/{VERSION}/server-{VERSION}.jar");
    Ok(archive
        .read(&name)
        .unwrap_or_else(|| panic!("the bundler jar should contain {name}")))
}

/// A zip archive, read from a slice already in memory.
pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

/// One file in the archive.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    method: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    local_header_offset: usize,
}

impl<'a> Zip<'a> {
    /// Parse the central directory. `None` if this is not a zip we understand.
    pub fn open(data: &'a [u8]) -> Option<Self> {
        // The end-of-central-directory record is at the end, but a zip may
        // carry a comment after it, so it is found by scanning backwards for
        // its signature. The comment is at most 65,535 bytes.
        let signature = [0x50, 0x4b, 0x05, 0x06];
        let search_from = data.len().saturating_sub(65_557);
        let eocd = (search_from..data.len().checked_sub(22)? + 1)
            .rev()
            .find(|&i| data[i..i + 4] == signature)?;

        let entry_count = u16::from_le_bytes([data[eocd + 10], data[eocd + 11]]) as usize;
        let directory_offset = u32::from_le_bytes([
            data[eocd + 16],
            data[eocd + 17],
            data[eocd + 18],
            data[eocd + 19],
        ]) as usize;

        let mut entries = Vec::with_capacity(entry_count);
        let mut cursor = directory_offset;
        for _ in 0..entry_count {
            // Bounds-checked by the `?`, so a truncated directory gives
            // `None` rather than a panic.
            if data.get(cursor..cursor + 46)?[..4] != [0x50, 0x4b, 0x01, 0x02] {
                return None;
            }
            let method = u16::from_le_bytes([data[cursor + 10], data[cursor + 11]]);
            let compressed_size = read_u32(data, cursor + 20)? as usize;
            let uncompressed_size = read_u32(data, cursor + 24)? as usize;
            let name_len = read_u16(data, cursor + 28)? as usize;
            let extra_len = read_u16(data, cursor + 30)? as usize;
            let comment_len = read_u16(data, cursor + 32)? as usize;
            let local_header_offset = read_u32(data, cursor + 42)? as usize;
            let name = String::from_utf8_lossy(data.get(cursor + 46..cursor + 46 + name_len)?)
                .into_owned();
            entries.push(Entry {
                name,
                method,
                compressed_size,
                uncompressed_size,
                local_header_offset,
            });
            cursor += 46 + name_len + extra_len + comment_len;
        }
        Some(Self { data, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every entry whose name ends with `suffix`, in central-directory order.
    pub fn entries_ending_with(&self, suffix: &str) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.name.ends_with(suffix))
            .collect()
    }

    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self.entries.iter().find(|entry| entry.name == name)?;
        self.read_entry(entry)
    }

    pub fn read_entry(&self, entry: &Entry) -> Option<Vec<u8>> {
        // The local header repeats the name and extra fields, with lengths of
        // its own that need not match the central directory's; the payload
        // starts after them.
        let header = entry.local_header_offset;
        if self.data.get(header..header + 30)?[..4] != [0x50, 0x4b, 0x03, 0x04] {
            return None;
        }
        let name_len = read_u16(self.data, header + 26)? as usize;
        let extra_len = read_u16(self.data, header + 28)? as usize;
        let start = header + 30 + name_len + extra_len;
        let payload = self.data.get(start..start + entry.compressed_size)?;
        match entry.method {
            0 => Some(payload.to_vec()),
            8 => {
                use std::io::Read as _;
                let mut out = Vec::with_capacity(entry.uncompressed_size);
                flate2::read::DeflateDecoder::new(payload)
                    .read_to_end(&mut out)
                    .ok()?;
                Some(out)
            }
            _ => None,
        }
    }
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
    ]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
        *data.get(at + 2)?,
        *data.get(at + 3)?,
    ]))
}

/// Every string constant in a Java class file, as raw bytes.
///
/// A class file's constant pool stores strings in **modified UTF-8** — the same
/// encoding NBT uses, and for the same historical reason. That makes Mojang's
/// own jar an external corpus for `dust_nbt::mutf8`: thousands of strings
/// produced by `javac`, decoded here by the decoder under test.
///
/// This walks the pool by tag, which requires knowing each constant's size.
/// `Long` and `Double` take two pool slots, which is the famous mistake in the
/// class file format and the one thing that makes this walk longer than a
/// table lookup.
pub fn class_constant_strings(class: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if class.len() < 10 || class[..4] != [0xca, 0xfe, 0xba, 0xbe] {
        return out;
    }
    let count = u16::from_be_bytes([class[8], class[9]]) as usize;
    let mut cursor = 10;
    let mut index = 1;
    while index < count {
        let Some(&tag) = class.get(cursor) else {
            return out;
        };
        cursor += 1;
        match tag {
            1 => {
                let Some(len) = read_be_u16(class, cursor) else {
                    return out;
                };
                cursor += 2;
                let Some(bytes) = class.get(cursor..cursor + len as usize) else {
                    return out;
                };
                out.push(bytes);
                cursor += len as usize;
            }
            7 | 8 | 16 | 19 | 20 => cursor += 2,
            15 => cursor += 3,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => cursor += 4,
            5 | 6 => {
                cursor += 8;
                // A Long or Double occupies two entries. The second is unusable
                // and is skipped here rather than being walked into.
                index += 1;
            }
            _ => return out,
        }
        index += 1;
    }
    out
}

fn read_be_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

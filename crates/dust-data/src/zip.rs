//! Reading a zipped datapack.
//!
//! Only what a datapack needs: the central directory, stored and deflated
//! entries, and nothing else. Everything left out is [refused by
//! name](ZipError) rather than skipped, because an entry that vanishes from a
//! pack without a word is a recipe that quietly stopped existing.
//!
//! # The guards, and what each one is for
//!
//! A zip in `datapacks/` came from wherever the operator got it, so this is an
//! untrusted parser and the limits are part of the design rather than tidying:
//!
//! * **[`MAX_FILE_BYTES`]** caps one entry's decompressed size, and the
//!   declared size from the central directory is passed to the decompressor as
//!   well, so a header that lies about how small an entry is fails on the
//!   *first* byte past the limit instead of on the last byte of memory.
//! * **[`MAX_ENTRIES`]** caps how many there are, so a directory of millions of
//!   empty files cannot be turned into millions of allocations.
//! * **Entry names are checked** for `..`, absolute paths, backslashes and NUL.
//!   Dust never writes a pack's contents back out, so there is no file to
//!   escape to today — the check is here because the name becomes a resource
//!   path and a log line, and because the day somebody adds an "extract this
//!   pack" command is not the day to start thinking about it.
//! * **CRC-32 is verified on every read.** This is the outside check on
//!   [`crate::inflate`]: the number was computed by whatever compressor wrote
//!   the archive, so it cannot agree with a decompressor bug the way a
//!   round-trip against our own encoder would.
//! * **The local header is checked against the directory.** A zip records
//!   every entry twice — once in the streaming local header next to the
//!   bytes, once in the central directory at the end — and writers may fill
//!   the local copy in late (the data-descriptor convention), but when both
//!   copies *are* filled in they must agree. Where they disagree the archive
//!   has been edited by something that did not finish the job, and every
//!   field that disagrees is named rather than silently resolved in
//!   whichever direction happened to be read second.
//! * **The Unicode-name flag is honoured.** Bit 11 of the general-purpose
//!   flags promises the entry name is UTF-8; a name that fails to decode
//!   under that promise is corruption and is refused, because silently
//!   substituting U+FFFD would produce resource paths no pack could have
//!   meant. Without the flag the name decodes lossily — nominally CP437,
//!   in practice usually UTF-8 a writer declined to promise — and ASCII
//!   paths, which is what a datapack uses, come through either way.
//!
//! # What this does not support, and therefore refuses
//!
//! Zip64 (archives past 4 GiB or 65,535 entries), encryption, and every
//! compression method other than stored and deflate. A datapack is none of
//! those things, and a loader that pretended to read one would be worse than
//! one that says it cannot.

use crate::inflate::{inflate, InflateError};
use crate::pack::MAX_FILE_BYTES;

/// [`MAX_FILE_BYTES`] as the `usize` the entry sizes are parsed into.
///
/// The cap is a file size and lives as `u64` where file sizes live; the entry
/// fields are 32-bit, so everything they can hold compares cleanly against a
/// 64 MiB limit on any platform that can run this server at all.
const ENTRY_LIMIT: usize = MAX_FILE_BYTES as usize;

/// The largest number of entries a pack archive may hold.
///
/// Vanilla's own data tree is about 5,600 files. Ten times that leaves room for
/// a large modpack's worth of data and still bounds the work done before
/// anything has been read.
pub const MAX_ENTRIES: usize = 65_535;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const EOCD_MIN_SIZE: usize = 22;
/// The end-of-central-directory record is last, followed only by a comment of
/// at most 65,535 bytes, so it is within this far of the end.
const EOCD_SEARCH_SPAN: usize = EOCD_MIN_SIZE + 0xffff;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// General-purpose bit 0: the entry is encrypted.
const FLAG_ENCRYPTED: u16 = 1 << 0;
/// General-purpose bit 3: sizes and checksum live in a data descriptor after
/// the entry, because the writer was streaming and did not know them yet.
///
/// Under this flag the local header's copies of those fields are legitimately
/// zero or garbage, so the disagreement check compares the name and method
/// only — those are known before compression starts and are always filled in.
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
/// General-purpose bit 11: the entry name is UTF-8 rather than CP437.
const FLAG_UTF8: u16 = 1 << 11;

/// Why an archive could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// No end-of-central-directory record. Usually not a zip at all.
    NotAnArchive,
    /// The archive ends before something it points at.
    Truncated {
        at: usize,
    },
    /// A record with the wrong signature where one was expected.
    CorruptRecord {
        at: usize,
    },
    /// Zip64. See the module documentation.
    Zip64,
    TooManyEntries {
        count: usize,
    },
    Encrypted {
        name: String,
    },
    UnsupportedMethod {
        name: String,
        method: u16,
    },
    UnsafeName {
        name: String,
        reason: &'static str,
    },
    /// The decompressed bytes are not what the compressor recorded.
    ChecksumMismatch {
        name: String,
        expected: u32,
        actual: u32,
    },
    Inflate {
        name: String,
        source: InflateError,
    },
    /// The entry's data is not the length the central directory claims.
    SizeMismatch {
        name: String,
        expected: usize,
        actual: usize,
    },
    /// The streaming local header and the central directory disagree about a
    /// field both have filled in. `detail` names the field.
    LocalHeaderMismatch {
        name: String,
        detail: &'static str,
    },
    /// The entry declares the Unicode-name flag, and its name is not UTF-8.
    NameNotUtf8 {
        name: String,
        at: usize,
    },
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnArchive => f.write_str(
                "has no zip end-of-directory record, so it is not a zip archive. \
                 A zipped datapack must be a zip, not a `.tar.gz` renamed",
            ),
            Self::Truncated { at } => {
                write!(
                    f,
                    "ends at byte {at}, part-way through a record it points at"
                )
            }
            Self::CorruptRecord { at } => {
                write!(f, "has a record at byte {at} with the wrong signature")
            }
            Self::Zip64 => f.write_str(
                "is a zip64 archive. Dust reads ordinary zips only; a datapack \
                 large enough to need zip64 should be a directory instead",
            ),
            Self::TooManyEntries { count } => write!(
                f,
                "holds {count} entries, past the {MAX_ENTRIES} Dust reads from one pack"
            ),
            Self::Encrypted { name } => {
                write!(f, "has an encrypted entry `{name}`, which Dust cannot read")
            }
            Self::UnsupportedMethod { name, method } => write!(
                f,
                "compresses `{name}` with method {method}. Dust reads stored (0) \
                 and deflate (8) only"
            ),
            Self::UnsafeName { name, reason } => {
                write!(f, "has an entry named `{name}`, which {reason}")
            }
            Self::ChecksumMismatch {
                name,
                expected,
                actual,
            } => write!(
                f,
                "has `{name}` decompressing to a checksum of {actual:#010x} where \
                 the archive records {expected:#010x}, so the entry is corrupt"
            ),
            Self::Inflate { name, source } => write!(f, "has an entry `{name}` that {source}"),
            Self::SizeMismatch {
                name,
                expected,
                actual,
            } => write!(
                f,
                "has `{name}` decompressing to {actual} bytes where the archive \
                 records {expected}"
            ),
            Self::LocalHeaderMismatch { name, detail } => write!(
                f,
                "has `{name}`, whose local file header {detail}. A zip writes \
                 every entry twice — beside the bytes and in the directory at \
                 the end — and the two copies disagree, so the archive was \
                 edited by something that did not finish the job"
            ),
            Self::NameNotUtf8 { name, at } => write!(
                f,
                "names an entry `{name}` whose bytes are not UTF-8 (they stop \
                 being decodable at byte {at}) while carrying the Unicode-name \
                 flag promising they are. The name cannot be read as written, \
                 so the entry has been refused rather than silently renamed"
            ),
        }
    }
}

impl std::error::Error for ZipError {}

/// One file in the archive, as the central directory describes it.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    method: u16,
    crc32: u32,
    compressed_size: usize,
    uncompressed_size: usize,
    local_header_offset: usize,
    /// General-purpose bit 11 as the directory records it. Kept because a
    /// diagnostic about an oddly-named entry wants to know whether the
    /// archive promised the name was UTF-8.
    utf8_flagged: bool,
}

impl ZipEntry {
    /// Whether this is a directory record rather than a file.
    ///
    /// Zip has no directories, only names; a trailing slash with no content is
    /// the convention every writer uses.
    pub fn is_directory(&self) -> bool {
        self.name.ends_with('/')
    }

    /// Whether the archive's Unicode-name flag promises this name is UTF-8.
    pub fn is_utf8_flagged(&self) -> bool {
        self.utf8_flagged
    }
}

/// A zip archive held in memory.
///
/// The whole file is read up front. A datapack is a few megabytes and is read
/// end to end on every `/reload`, so seeking around a file handle would buy
/// nothing and cost the ability to hand the same bytes to two threads.
#[derive(Debug)]
pub struct ZipArchive {
    data: Vec<u8>,
    entries: Vec<ZipEntry>,
}

impl ZipArchive {
    /// Read the central directory. Entry contents are read on demand.
    pub fn open(data: Vec<u8>) -> Result<Self, ZipError> {
        let eocd = find_eocd(&data).ok_or(ZipError::NotAnArchive)?;
        let entry_count = read_u16(&data, eocd + 10)? as usize;
        let directory_size = read_u32(&data, eocd + 12)? as usize;
        let directory_offset = read_u32(&data, eocd + 16)? as usize;

        // Zip64 announces itself by writing the "everything is maxed out"
        // sentinels into the 32-bit fields. Recognising them is what turns a
        // baffling parse failure into a sentence.
        if entry_count == 0xffff || directory_size == 0xffff_ffff || directory_offset == 0xffff_ffff
        {
            return Err(ZipError::Zip64);
        }
        if entry_count > MAX_ENTRIES {
            return Err(ZipError::TooManyEntries { count: entry_count });
        }

        let mut entries = Vec::with_capacity(entry_count);
        let mut at = directory_offset;
        for _ in 0..entry_count {
            if read_u32(&data, at)? != CENTRAL_SIGNATURE {
                return Err(ZipError::CorruptRecord { at });
            }
            let flags = read_u16(&data, at + 8)?;
            let method = read_u16(&data, at + 10)?;
            let crc32 = read_u32(&data, at + 16)?;
            let compressed_size = read_u32(&data, at + 20)? as usize;
            let uncompressed_size = read_u32(&data, at + 24)? as usize;
            let name_length = read_u16(&data, at + 28)? as usize;
            let extra_length = read_u16(&data, at + 30)? as usize;
            let comment_length = read_u16(&data, at + 32)? as usize;
            let local_header_offset = read_u32(&data, at + 42)? as usize;

            let name = read_name(&data, at + 46, name_length, flags & FLAG_UTF8 != 0)?;
            if compressed_size == 0xffff_ffff
                || uncompressed_size == 0xffff_ffff
                || local_header_offset == 0xffff_ffff
            {
                return Err(ZipError::Zip64);
            }
            if flags & FLAG_ENCRYPTED != 0 {
                return Err(ZipError::Encrypted { name });
            }
            if let Some(reason) = unsafe_name_reason(&name) {
                return Err(ZipError::UnsafeName { name, reason });
            }
            if method != METHOD_STORED && method != METHOD_DEFLATE {
                return Err(ZipError::UnsupportedMethod { name, method });
            }

            entries.push(ZipEntry {
                name,
                method,
                crc32,
                compressed_size,
                uncompressed_size,
                local_header_offset,
                utf8_flagged: flags & FLAG_UTF8 != 0,
            });
            at += 46 + name_length + extra_length + comment_length;
        }

        Ok(Self { data, entries })
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Decompress one entry and check it against the archive's own checksum.
    ///
    /// The entry is read through its **local** header, which is checked
    /// against the central directory's copy first: a zip records every entry
    /// twice and the two records agreeing is part of what "this archive is
    /// intact" means. See the module documentation.
    pub fn read(&self, entry: &ZipEntry) -> Result<Vec<u8>, ZipError> {
        let header = entry.local_header_offset;
        if read_u32(&self.data, header)? != LOCAL_SIGNATURE {
            return Err(ZipError::CorruptRecord { at: header });
        }
        let local_flags = read_u16(&self.data, header + 6)?;
        let local_method = read_u16(&self.data, header + 8)?;
        let local_crc = read_u32(&self.data, header + 14)?;
        let local_compressed = read_u32(&self.data, header + 18)? as usize;
        let local_uncompressed = read_u32(&self.data, header + 22)? as usize;
        // The local header's name and extra lengths are read rather than the
        // central directory's: writers are allowed to differ there — an extra
        // field can be padded differently in the two places — and taking the
        // central copy puts the read a few bytes into the wrong place with no
        // signature left to notice it.
        let name_length = read_u16(&self.data, header + 26)? as usize;
        let extra_length = read_u16(&self.data, header + 28)? as usize;

        check_local_agreement(
            entry,
            LocalHeaderFields {
                flags: local_flags,
                method: local_method,
                crc32: local_crc,
                compressed_size: local_compressed,
                uncompressed_size: local_uncompressed,
            },
        )?;
        let local_name = self.read_local_name(header + 30, name_length)?;
        if local_name != entry.name {
            return Err(ZipError::LocalHeaderMismatch {
                name: entry.name.clone(),
                detail: "names the entry differently than the central directory does",
            });
        }

        let start = header + 30 + name_length + extra_length;
        let end = start
            .checked_add(entry.compressed_size)
            .ok_or(ZipError::Truncated { at: start })?;
        let raw = self
            .data
            .get(start..end)
            .ok_or(ZipError::Truncated { at: start })?;

        if entry.uncompressed_size > ENTRY_LIMIT {
            return Err(ZipError::Inflate {
                name: entry.name.clone(),
                source: InflateError::TooLarge { limit: ENTRY_LIMIT },
            });
        }

        let bytes = match entry.method {
            METHOD_STORED => raw.to_vec(),
            _ => inflate(raw, entry.uncompressed_size.min(ENTRY_LIMIT)).map_err(|source| {
                ZipError::Inflate {
                    name: entry.name.clone(),
                    source,
                }
            })?,
        };

        if bytes.len() != entry.uncompressed_size {
            return Err(ZipError::SizeMismatch {
                name: entry.name.clone(),
                expected: entry.uncompressed_size,
                actual: bytes.len(),
            });
        }
        let actual = crc32(&bytes);
        if actual != entry.crc32 {
            return Err(ZipError::ChecksumMismatch {
                name: entry.name.clone(),
                expected: entry.crc32,
                actual,
            });
        }
        Ok(bytes)
    }

    /// The local header's own copy of the name, for the agreement check.
    fn read_local_name(&self, at: usize, length: usize) -> Result<String, ZipError> {
        let bytes = self
            .data
            .get(at..at + length)
            .ok_or(ZipError::Truncated { at })?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// The local header's own copies of the fields both records carry.
struct LocalHeaderFields {
    flags: u16,
    method: u16,
    crc32: u32,
    compressed_size: usize,
    uncompressed_size: usize,
}

/// Compare the local header against the central directory, refusing where
/// they disagree about something the local copy has actually filled in.
///
/// The data-descriptor flag is what makes this check honest: a streaming
/// writer genuinely does not know sizes or checksum until after the bytes are
/// out, so under that flag those fields are expected to be unfilled and are
/// not compared. The name and the method *are* known up front — every writer
/// fills them in — so they are compared regardless.
fn check_local_agreement(entry: &ZipEntry, local: LocalHeaderFields) -> Result<(), ZipError> {
    if local.method != entry.method {
        return Err(ZipError::LocalHeaderMismatch {
            name: entry.name.clone(),
            detail: "records a different compression method than the central \
                     directory does",
        });
    }
    if local.flags & FLAG_DATA_DESCRIPTOR != 0 {
        return Ok(());
    }
    if local.compressed_size != entry.compressed_size
        || local.uncompressed_size != entry.uncompressed_size
    {
        return Err(ZipError::LocalHeaderMismatch {
            name: entry.name.clone(),
            detail: "records different entry sizes than the central directory does",
        });
    }
    if local.crc32 != entry.crc32 {
        return Err(ZipError::LocalHeaderMismatch {
            name: entry.name.clone(),
            detail: "records a different checksum than the central directory does",
        });
    }
    Ok(())
}

/// Scan backwards for the end-of-central-directory signature.
///
/// Backwards, because the archive comment sits after the record and may itself
/// contain the signature; the last match is the real one.
fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < EOCD_MIN_SIZE {
        return None;
    }
    let earliest = data.len().saturating_sub(EOCD_SEARCH_SPAN);
    (earliest..=data.len() - EOCD_MIN_SIZE)
        .rev()
        .find(|&at| read_u32(data, at) == Ok(EOCD_SIGNATURE))
}

fn read_u16(data: &[u8], at: usize) -> Result<u16, ZipError> {
    let bytes = data
        .get(at..at + 2)
        .ok_or(ZipError::Truncated { at: data.len() })?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], at: usize) -> Result<u32, ZipError> {
    let bytes = data
        .get(at..at + 4)
        .ok_or(ZipError::Truncated { at: data.len() })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read an entry name, honouring the Unicode-name flag.
///
/// With the flag set the name is *promised* UTF-8, so a failed decode is
/// corruption and is refused — replacing bytes with U+FFFD would hand back a
/// resource path no pack could have written. Without the flag the name is
/// nominally CP437; it decodes lossily, which is exact for ASCII (every
/// datapack path) and honest about everything else in a log line, because
/// refusing whole archives over a code-page nobody meant would cost more than
/// it saved.
fn read_name(
    data: &[u8],
    at: usize,
    length: usize,
    require_utf8: bool,
) -> Result<String, ZipError> {
    let bytes = data
        .get(at..at + length)
        .ok_or(ZipError::Truncated { at: data.len() })?;
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_owned()),
        Err(error) if require_utf8 => Err(ZipError::NameNotUtf8 {
            name: String::from_utf8_lossy(bytes).into_owned(),
            at: error.valid_up_to(),
        }),
        Err(_) => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Why a name is not one Dust will accept, or `None`.
fn unsafe_name_reason(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("is empty");
    }
    if name.starts_with('/') || name.contains(':') {
        return Some("is an absolute path rather than a path inside the pack");
    }
    if name.contains('\\') {
        return Some(
            "contains a backslash. Zip paths use `/`; a backslash means the \
             archive was written by a tool that got this wrong",
        );
    }
    if name.split('/').any(|segment| segment == "..") {
        return Some("climbs out of the pack with `..`");
    }
    if name.contains('\0') {
        return Some("contains a NUL byte");
    }
    None
}

/// The CRC-32 table, built at compile time so there is no lazily-initialised
/// global and no `unsafe`.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

/// CRC-32 as zip uses it (IEEE, reflected, initial and final complement).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_matches_the_published_check_value() {
        // The check value in the CRC-32 specification: the string "123456789".
        // An outside number, which is the point — a CRC verified against
        // itself verifies nothing.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn something_that_is_not_a_zip_says_so() {
        assert_eq!(
            ZipArchive::open(b"not a zip at all".to_vec()).unwrap_err(),
            ZipError::NotAnArchive
        );
        assert_eq!(
            ZipArchive::open(Vec::new()).unwrap_err(),
            ZipError::NotAnArchive
        );
    }

    #[test]
    fn names_that_climb_out_of_the_pack_are_refused() {
        assert!(unsafe_name_reason("data/minecraft/recipe/x.json").is_none());
        assert!(unsafe_name_reason("../../etc/passwd").is_some());
        assert!(unsafe_name_reason("/etc/passwd").is_some());
        assert!(unsafe_name_reason("data\\minecraft\\x.json").is_some());
        assert!(unsafe_name_reason("C:/windows").is_some());
        assert!(unsafe_name_reason("").is_some());
    }

    #[test]
    fn a_name_that_merely_contains_two_dots_is_fine() {
        // `..` is only a climb when it is a whole segment. Rejecting the
        // substring would refuse `minecraft:tags/a..b`, which is a legal name.
        assert!(unsafe_name_reason("data/minecraft/recipe/a..b.json").is_none());
    }
}

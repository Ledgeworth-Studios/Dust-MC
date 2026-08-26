//! Support for the vanilla corpus tests: finding the corpus, skipping loudly
//! when it is not there, and reading just enough NBT to reach a long array.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

/// The command that produces the corpus, named in every skip message.
pub const REGENERATE: &str = "crates/dust-world/tools/generate-corpus.sh";

/// The corpus's `region` directory, or `None` if it has not been generated.
///
/// `DUST_WORLD_CORPUS` overrides the location, which is what a developer who
/// keeps a world somewhere else uses.
pub fn corpus_region_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("DUST_WORLD_CORPUS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".corpus"),
    };
    let region = root.join("world").join("region");
    region.is_dir().then_some(region)
}

/// Where a rewritten copy of the corpus is put, for the vanilla server to be
/// handed back.
pub fn rewritten_region_dir() -> PathBuf {
    let root = match std::env::var_os("DUST_WORLD_CORPUS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".corpus"),
    };
    root.join("rewritten").join("region")
}

/// Announce that a test did nothing, in a way the person running it sees.
///
/// Written straight to the process's stderr rather than through `eprintln!`,
/// because the test harness captures the macros and shows the capture only for
/// tests that *failed*. A test that quietly passes when its fixture is missing
/// is worse than no test: it reports a green suite for a check that never ran,
/// and the corpus is exactly the fixture that is missing on a fresh clone.
pub fn skipping(test: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(
        stderr,
        "\nSKIPPED {test}: the vanilla corpus is not present.\n  \
         Generate it with:  {REGENERATE}\n  \
         Until then nothing in this file is checking anything.\n"
    );
}

/// Every region file in the corpus, sorted, with the region each names.
pub fn corpus_regions() -> Vec<(PathBuf, dust_world::RegionPos)> {
    let Some(dir) = corpus_region_dir() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the corpus directory is readable") {
        let entry = entry.expect("a directory entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(region) = dust_world::RegionPos::from_file_name(&name) {
            found.push((entry.path(), region));
        }
    }
    found.sort();
    found
}

/// A minimal structural reader for NBT.
///
/// **This is not an NBT library and must not become one.** `dust-nbt` is being
/// built in parallel and owns that job; this exists because the corpus tests
/// need to reach the packed long arrays inside a chunk in order to check the
/// bit packing against arrays a real server wrote, and waiting for that crate
/// would mean the one test in this suite with any bite does not exist yet.
/// It reads the tree structurally and understands nothing about chunks.
///
/// Delete it when `dust-nbt` lands.
///
/// **What it does not do:** Java's modified UTF-8. Strings are decoded as plain
/// UTF-8 and lossily, which is right for every key and block id in a chunk and
/// wrong for a supplementary character or an embedded NUL in a player-supplied
/// name. Nothing here reads one.
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Nbt>),
    Compound(BTreeMap<String, Nbt>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    /// A named child, if this is a compound with one.
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&BTreeMap<String, Nbt>> {
        match self {
            Nbt::Compound(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Nbt]> {
        match self {
            Nbt::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Nbt::LongArray(items) => Some(items),
            _ => None,
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

/// Read a whole NBT document: a named root compound.
pub fn read_nbt(bytes: &[u8]) -> Result<Nbt, String> {
    let mut reader = Reader { bytes, at: 0 };
    let tag = reader.u8()?;
    if tag != 10 {
        return Err(format!("the root tag is {tag}, not a compound"));
    }
    let _name = reader.string()?;
    reader.payload(10)
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).ok_or("length overflow")?;
        if end > self.bytes.len() {
            return Err(format!(
                "wanted {n} bytes at offset {} of {}",
                self.at,
                self.bytes.len()
            ));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, String> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn length(&mut self) -> Result<usize, String> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| format!("a length of {n} is negative"))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.i16()?;
        let len = usize::try_from(len).map_err(|_| format!("a string length of {len}"))?;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn payload(&mut self, tag: u8) -> Result<Nbt, String> {
        Ok(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(f32::from_bits(self.i32()? as u32)),
            6 => Nbt::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let len = self.length()?;
                Nbt::ByteArray(self.take(len)?.iter().map(|b| *b as i8).collect())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let element = self.u8()?;
                let len = self.length()?;
                if element == 0 && len > 0 {
                    return Err(format!("a list of {len} end tags"));
                }
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.payload(element)?);
                }
                Nbt::List(items)
            }
            10 => {
                let mut map = BTreeMap::new();
                loop {
                    let child = self.u8()?;
                    if child == 0 {
                        break;
                    }
                    let name = self.string()?;
                    map.insert(name, self.payload(child)?);
                }
                Nbt::Compound(map)
            }
            11 => {
                let len = self.length()?;
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.i32()?);
                }
                Nbt::IntArray(items)
            }
            12 => {
                let len = self.length()?;
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.i64()?);
                }
                Nbt::LongArray(items)
            }
            other => return Err(format!("tag type {other} is not one NBT has")),
        })
    }
}

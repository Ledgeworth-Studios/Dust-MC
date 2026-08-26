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

/// Run `f` on a thread with a stack far larger than the default, and wait.
///
/// Documents just under the 512-level limit are legitimate inputs — vanilla
/// reads them inside the JVM — so the near-limit tests have to be able to run
/// them without aborting. Test threads get the platform default, which a
/// debug build's fat frames can exhaust two or three levels short of where a
/// release build fits easily; rather than weaken the test to shallower
/// nesting, it runs where the depth actually lives. The server gives its
/// reader threads their own generous stacks for exactly this reason, and the
/// depth limit exists so a *hostile* document cannot choose this depth — see
/// `tests/hostile.rs`, which asserts that anything past the limit is refused
/// long before the stack is asked about it.
pub fn on_a_large_stack<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .expect("spawns")
        .join()
        .expect("the test body does not panic unexpectedly")
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

// ---------------------------------------------------------------------------
// Generated documents
// ---------------------------------------------------------------------------

/// Strategies that produce [`Tag`] trees for the round-trip and differential
/// suites.
///
/// The shape of the generator is where the strength of a property suite is
/// decided, so the choices here are written down. Scalars mix arbitrary values
/// with hand-picked edges, because a codec is far more likely to be broken by
/// `i32::MIN` or a NaN with its payload set than by a typical number.
/// Containers are built homogeneous from the start — a `List` that disagrees
/// with its declared element type cannot be constructed through public API, so
/// generating one would only measure the constructor's error path. Empty lists
/// appear both as vanilla writes them (element type `TAG_End`) and with a
/// declared type preserved from some other tool, because those two encode
/// differently and must both survive.

use dust_nbt::Tag;
use proptest::prelude::*;

/// Numeric edges per width, mixed into every scalar strategy.
const I8_EDGES: &[i8] = &[0, 1, -1, i8::MAX, i8::MIN];
const I16_EDGES: &[i16] = &[0, 1, -1, i16::MAX, i16::MIN];
const I32_EDGES: &[i32] = &[0, 1, -1, i32::MAX, i32::MIN];
const I64_EDGES: &[i64] = &[0, 1, -1, i64::MAX, i64::MIN];

/// Float edges including the values only the binary format carries exactly:
/// negative zero, both infinities, and NaNs with distinct payloads and sign
/// bits. SNBT has no syntax for the last three kinds; see
/// [`any_tag_surviving_snbt`].
const F32_EDGES: &[f32] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    f32::MIN_POSITIVE,
    -f32::MIN_POSITIVE,
    f32::MAX,
    f32::MIN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::from_bits(0x7fc0_0000),
    f32::from_bits(0xffc0_0000),
    f32::from_bits(0x7f80_0001),
    f32::from_bits(0xffab_cdef),
];

const F64_EDGES: &[f64] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    f64::MIN_POSITIVE,
    -f64::MIN_POSITIVE,
    f64::MAX,
    f64::MIN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::from_bits(0x7ff8_0000_0000_0000),
    f64::from_bits(0xfff8_0000_0000_0000),
    f64::from_bits(0x7ff0_0000_0000_0001),
    f64::from_bits(0xffd0_1234_5678_9abc),
];

/// A scalar that is usually arbitrary and occasionally one of `edges`.
fn edged<T>(edges: &'static [T]) -> impl Strategy<Value = T>
where
    T: Copy + proptest::arbitrary::Arbitrary + std::fmt::Debug,
{
    prop_oneof![4 => any::<T>(), 1 => proptest::sample::select(edges.to_vec())]
}

fn byte_value() -> impl Strategy<Value = i8> {
    edged(I8_EDGES)
}

fn short_value() -> impl Strategy<Value = i16> {
    edged(I16_EDGES)
}

fn int_value() -> impl Strategy<Value = i32> {
    edged(I32_EDGES)
}

fn long_value() -> impl Strategy<Value = i64> {
    edged(I64_EDGES)
}

fn float_value() -> impl Strategy<Value = f32> {
    edged(F32_EDGES)
}

fn double_value() -> impl Strategy<Value = f64> {
    edged(F64_EDGES)
}

/// Characters an NBT string may meaningfully hold, weighted towards the ones
/// that have historically been someone's bug: the NUL this encoding writes as
/// two bytes, the section signs colour codes are made of, characters above the
/// BMP that become surrogate pairs, the quote characters and backslash the
/// SNBT printer escapes, control characters it deliberately does not, and the
/// last scalar of the BMP.
fn interesting_char() -> impl Strategy<Value = char> {
    prop_oneof![
        6 => prop::char::range('a', 'z'),
        2 => prop::char::range('A', 'Z'),
        2 => prop::char::range('0', '9'),
        3 => Just('\u{0000}'),
        3 => Just('\u{00a7}'),
        3 => Just('"'),
        3 => Just('\''),
        3 => Just('\\'),
        2 => Just('\n'),
        1 => Just('\t'),
        1 => Just('\r'),
        1 => Just('\u{0001}'),
        1 => Just('\u{007f}'),
        2 => prop::char::range('\u{4e00}', '\u{9fff}'),
        4 => prop::char::range('\u{1f300}', '\u{1f9ff}'),
        1 => Just('\u{ffff}'),
        1 => Just('\u{10000}'),
        1 => Just('\u{10ffff}'),
    ]
}

/// String values: empty, ordinary, awkward, and long enough to make the
/// length-prefix arithmetic work for its living. Nothing here exceeds the
/// `u16` prefix, which no generated string can survive past; that boundary is
/// a fixture in `tests/binary.rs` and `tests/mutf8.rs`.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        1 => Just(String::new()),
        6 => proptest::collection::vec(interesting_char(), 0..24)
            .prop_map(|chars| chars.into_iter().collect()),
        1 => (interesting_char(), 200..2000usize)
            .prop_map(|(c, n)| std::iter::repeat_n(c, n).collect()),
    ]
}

/// Field names, including the empty name binary NBT permits everywhere.
fn field_name() -> impl Strategy<Value = String> {
    prop_oneof![
        5 => proptest::collection::vec(interesting_char(), 1..10)
            .prop_map(|chars| chars.into_iter().collect()),
        1 => Just(String::new()),
    ]
}

/// The twelve value tags, as a choice for list elements: every element of a
/// generated list comes from *one* of these, which is what keeps the list
/// homogeneous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
    ByteArray,
    String,
    List,
    Compound,
    IntArray,
    LongArray,
}

const KINDS: &[Kind] = &[
    Kind::Byte,
    Kind::Short,
    Kind::Int,
    Kind::Long,
    Kind::Float,
    Kind::Double,
    Kind::ByteArray,
    Kind::String,
    Kind::List,
    Kind::Compound,
    Kind::IntArray,
    Kind::LongArray,
];

impl Kind {
    fn tag_type(self) -> dust_nbt::TagType {
        match self {
            Kind::Byte => dust_nbt::TagType::Byte,
            Kind::Short => dust_nbt::TagType::Short,
            Kind::Int => dust_nbt::TagType::Int,
            Kind::Long => dust_nbt::TagType::Long,
            Kind::Float => dust_nbt::TagType::Float,
            Kind::Double => dust_nbt::TagType::Double,
            Kind::ByteArray => dust_nbt::TagType::ByteArray,
            Kind::String => dust_nbt::TagType::String,
            Kind::List => dust_nbt::TagType::List,
            Kind::Compound => dust_nbt::TagType::Compound,
            Kind::IntArray => dust_nbt::TagType::IntArray,
            Kind::LongArray => dust_nbt::TagType::LongArray,
        }
    }
}

fn any_kind() -> impl Strategy<Value = Kind> {
    proptest::sample::select(KINDS.to_vec())
}

/// An element of exactly `kind`, recursing where the kind nests.
fn element_of(kind: Kind, depth: u32) -> BoxedStrategy<Tag> {
    match kind {
        Kind::Byte => byte_value().prop_map(Tag::Byte).boxed(),
        Kind::Short => short_value().prop_map(Tag::Short).boxed(),
        Kind::Int => int_value().prop_map(Tag::Int).boxed(),
        Kind::Long => long_value().prop_map(Tag::Long).boxed(),
        Kind::Float => float_value().prop_map(Tag::Float).boxed(),
        Kind::Double => double_value().prop_map(Tag::Double).boxed(),
        Kind::ByteArray => proptest::collection::vec(byte_value(), 0..48)
            .prop_map(Tag::ByteArray)
            .boxed(),
        Kind::String => text().prop_map(Tag::String).boxed(),
        Kind::List => list_tree(depth.saturating_sub(1)).boxed(),
        Kind::Compound => compound_tree(depth.saturating_sub(1)).boxed(),
        Kind::IntArray => proptest::collection::vec(int_value(), 0..48)
            .prop_map(Tag::IntArray)
            .boxed(),
        Kind::LongArray => proptest::collection::vec(long_value(), 0..48)
            .prop_map(Tag::LongArray)
            .boxed(),
    }
}

/// A list at `depth`: empty with `TAG_End` like vanilla writes, empty with a
/// declared type like other tools write, or filled with one chosen kind.
fn list_tree(depth: u32) -> BoxedStrategy<Tag> {
    prop_oneof![
        1 => Just(Tag::List(dust_nbt::List::new(dust_nbt::TagType::End))),
        1 => any_kind().prop_map(|kind| Tag::List(dust_nbt::List::new(kind.tag_type()))),
        3 => (any_kind(), 1usize..=5)
            .prop_flat_map(move |(kind, n)| (
                Just(kind),
                proptest::collection::vec(element_of(kind, depth), n)
            ))
            .prop_map(|(kind, elements)| Tag::List(
                dust_nbt::List::from_elements(kind.tag_type(), elements)
                    .expect("generated lists are homogeneous")
            )),
    ]
    .boxed()
}

/// A compound at `depth`, empty on occasion, names drawn from the awkward
/// alphabet too.
fn compound_tree(depth: u32) -> BoxedStrategy<Tag> {
    proptest::collection::vec((field_name(), tag_tree(depth.saturating_sub(1))), 0..=4)
        .prop_map(|fields| {
            let mut compound = dust_nbt::Compound::new();
            for (name, value) in fields {
                compound.insert(name, value);
            }
            Tag::Compound(compound)
        })
        .boxed()
}

/// The nine scalar-and-array leaves, without containers.
fn leaf() -> BoxedStrategy<Tag> {
    prop_oneof![
        2 => byte_value().prop_map(Tag::Byte),
        2 => short_value().prop_map(Tag::Short),
        2 => int_value().prop_map(Tag::Int),
        2 => long_value().prop_map(Tag::Long),
        2 => float_value().prop_map(Tag::Float),
        2 => double_value().prop_map(Tag::Double),
        2 => text().prop_map(Tag::String),
        1 => proptest::collection::vec(byte_value(), 0..48).prop_map(Tag::ByteArray),
        1 => proptest::collection::vec(int_value(), 0..48).prop_map(Tag::IntArray),
        1 => proptest::collection::vec(long_value(), 0..48).prop_map(Tag::LongArray),
    ]
    .boxed()
}

fn tag_tree(depth: u32) -> BoxedStrategy<Tag> {
    if depth == 0 {
        return leaf();
    }
    prop_oneof![
        10 => leaf(),
        2 => list_tree(depth),
        3 => compound_tree(depth),
    ]
    .boxed()
}

/// Any tag tree, moderate depth: every value tag, every edge value above, and
/// non-finite floats, which the binary dialects carry bit-exactly.
pub fn any_tag() -> BoxedStrategy<Tag> {
    tag_tree(3)
}

/// Whether a tree survives an SNBT round trip. Three things do not, and each
/// is pinned as documented lossiness in `tests/snbt.rs`: a non-finite float
/// prints as `NaN`/`Infinity` and reads back as a string, an empty list loses
/// its declared element type to the `[]` syntax, and an empty compound key
/// prints but the parser refuses to read it back.
pub fn survives_snbt(tag: &Tag) -> bool {
    match tag {
        Tag::Float(v) => v.is_finite(),
        Tag::Double(v) => v.is_finite(),
        Tag::List(list) => {
            (!list.is_empty() || list.element_type() == dust_nbt::TagType::End)
                && list.iter().all(survives_snbt)
        }
        Tag::Compound(compound) => compound
            .iter()
            .all(|(name, value)| !name.is_empty() && survives_snbt(value)),
        _ => true,
    }
}

/// [`any_tag`] restricted to what SNBT can carry losslessly, for the
/// printer/parser differential.
pub fn any_tag_surviving_snbt() -> BoxedStrategy<Tag> {
    any_tag().prop_filter("must survive SNBT", survives_snbt).boxed()
}

/// A root name for the file dialect: any string at all, empty most often in
/// practice but not privileged here.
pub fn any_root_name() -> impl Strategy<Value = String> {
    text()
}

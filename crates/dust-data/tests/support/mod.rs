//! Shared helpers for the integration tests.
//!
//! Three things live here: finding the extracted vanilla corpus and being loud
//! when it is not there; a **builder for synthetic packs**, so every pack a
//! test needs is constructed in code rather than kept as a binary fixture; and
//! a tiny seeded generator, so the mutation and property tests are random
//! without being unreproducible.
//!
//! # Why the fixtures are built, not stored
//!
//! A datapack fixture is a tree of small JSON files, which is exactly the shape
//! of thing that rots: one format change and forty committed fixtures are
//! subtly wrong together, all looking fine in a diff. Building them at the
//! point of use keeps each test's pack next to its assertions, and makes the
//! *interesting* part of the fixture — the part the test is about — impossible
//! to miss. The two committed files under `tests/fixtures/` are hand-written
//! text documents exercising spellings a builder would just be echoing.
//!
//! Every integration test binary compiles this whole module but uses part of
//! it, which dead code cannot see — hence the blanket allow. The alternative,
//! per-binary helper modules, would duplicate exactly the things this module
//! exists to share.
#![allow(dead_code)]

use std::io::Write as _;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// The vanilla corpus.
// ---------------------------------------------------------------------------

/// The command that produces the corpus these tests read.
pub const REGENERATE: &str = "cargo xtask extract --version 1.21.1";

/// The extracted vanilla data tree, if it has been generated on this machine.
///
/// It lives in `.dust-extract/`, which is gitignored: **no Mojang file is
/// committed**, per the Code Provenance rule that the extractor and the
/// generated code are the repository's and Mojang's files stay on the machine
/// that downloaded them.
pub fn corpus_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.dust-extract/data-1.21.1")
        .canonicalize()
        .ok()?;
    root.join("data").is_dir().then_some(root)
}

/// Say, in a way the test harness cannot swallow, that a test did not run.
///
/// A test that quietly passes when its fixture is missing is worse than no test
/// at all: it reports a green that means nothing, and it does so most reliably
/// on the machine that has never had the fixture. `println!` will not do — the
/// harness captures it and shows it only for tests that fail, which is exactly
/// the wrong way round. Writing to the real stderr handle bypasses the capture,
/// so this line appears on a green run.
pub fn skipped(test: &str, reason: &str) {
    let _ = std::io::stderr().write_all(
        format!("\nSKIPPED {test}: {reason}\n         regenerate it with: {REGENERATE}\n\n")
            .as_bytes(),
    );
}

/// Print a measured number so it ends up in the run's output rather than only
/// in the head of whoever ran it. Same reasoning as [`skipped`].
pub fn report(lines: &[String]) {
    let mut out = String::new();
    for line in lines {
        out.push_str("         ");
        out.push_str(line);
        out.push('\n');
    }
    let _ = std::io::stderr().write_all(out.as_bytes());
}

// ---------------------------------------------------------------------------
// Synthetic packs.
// ---------------------------------------------------------------------------

/// A synthetic pack under construction: named, filled with files, then
/// realised as an in-memory source, a real directory, or a zip — because a
/// rule about packs must hold for all three containers or not at all.
#[derive(Debug, Default)]
pub struct PackBuilder {
    id: String,
    meta: Option<String>,
    files: Vec<(String, String)>,
}

impl PackBuilder {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ..Self::default()
        }
    }

    /// Use a specific `pack.mcmeta`. Without this, [`Self::build`] writes a
    /// minimal format-48 one.
    pub fn mcmeta(mut self, body: &str) -> Self {
        self.meta = Some(body.to_owned());
        self
    }

    /// One file, by path relative to the pack root.
    pub fn file(mut self, path: &str, body: &str) -> Self {
        self.files.push((path.to_owned(), body.to_owned()));
        self
    }

    /// Shorthand for `data/<namespace>/<registry>/<name>.json`.
    pub fn resource(self, namespace: &str, registry: &str, name: &str, body: &str) -> Self {
        self.file(&format!("data/{namespace}/{registry}/{name}.json"), body)
    }

    fn assembled(&self) -> Vec<(String, String)> {
        let mut files = vec![(
            "pack.mcmeta".to_owned(),
            self.meta.clone().unwrap_or_else(|| {
                r#"{"pack":{"pack_format":48,"description":"synthetic"}}"#.to_owned()
            }),
        )];
        files.extend(self.files.clone());
        files
    }

    /// The pack as an in-memory source. Nothing touches the disk.
    pub fn build(&self) -> MemPack {
        MemPack::from_pairs(&self.assembled(), &self.id)
    }

    /// The pack as a real directory under the test's temp root.
    pub fn build_directory(&self, root: &TempDir) -> PathBuf {
        let path = root.path.join(&self.id);
        for (relative, body) in self.assembled() {
            let full = path.join(&relative);
            std::fs::create_dir_all(full.parent().expect("has a parent"))
                .expect("create pack directory");
            std::fs::write(full, body).expect("write pack file");
        }
        path
    }

    /// The pack as zip bytes, written stored (no compression), which is what
    /// the hand-rolled writer supports and all the reader rules need.
    pub fn build_zip_bytes(&self) -> Vec<u8> {
        write_stored_zip(
            &self
                .assembled()
                .iter()
                .map(|(name, body)| (name.as_str(), body.as_bytes()))
                .collect::<Vec<_>>(),
        )
    }

    /// The pack as a real `.zip` under the test's temp root, ready for
    /// `ZipPack::open`.
    pub fn build_zip(&self, root: &TempDir) -> PathBuf {
        let path = root.path.join(format!("{}.zip", self.id));
        std::fs::write(&path, self.build_zip_bytes()).expect("write zip");
        path
    }
}

/// An in-memory [`dust_data::PackSource`] for tests that want to drive the
/// loader directly.
#[derive(Debug)]
pub struct MemPack {
    id: String,
    files: std::collections::BTreeMap<String, Vec<u8>>,
}

impl MemPack {
    fn from_pairs(files: &[(String, String)], id: &str) -> Self {
        Self {
            id: id.to_owned(),
            files: files
                .iter()
                .map(|(path, body)| (path.clone(), body.as_bytes().to_vec()))
                .collect(),
        }
    }

    /// A pack whose files are given as raw bytes. The text builders cover
    /// everything JSON-shaped; this one exists for the tests that are
    /// precisely about bytes a text builder cannot write — invalid UTF-8,
    /// hostile headers, truncated anything.
    pub fn with_raw(id: &str, files: &[(&str, &[u8])]) -> Self {
        Self {
            id: id.to_owned(),
            files: files
                .iter()
                .map(|(path, body)| ((*path).to_owned(), body.to_vec()))
                .collect(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

impl dust_data::PackSource for MemPack {
    fn id(&self) -> &str {
        &self.id
    }

    fn origin(&self) -> String {
        format!("<memory:{}>", self.id)
    }

    fn list(&self) -> Result<Vec<String>, dust_data::PackError> {
        Ok(self.files.keys().cloned().collect())
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, dust_data::PackError> {
        Ok(self.files.get(path).cloned())
    }
}

// ---------------------------------------------------------------------------
// Writing zips.
// ---------------------------------------------------------------------------

/// Write a zip archive of stored (uncompressed) entries.
///
/// This is the other half of `crate::zip`: the reader has no compressor, so
/// the tests bring their own writer, checked against the reader's CRC and
/// length fields. Deflated entries are exercised against archives produced by
/// the system `zip` binary instead — see `the_system_zipper_and_dust_agree` —
/// and against [`write_deflate_fixed`] below, which needs no outside tool.
pub fn write_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    assert!(entries.len() <= u16::MAX as usize);
    let raw: Vec<RawZipEntry> = entries
        .iter()
        .map(|(name, body)| RawZipEntry {
            name: name.as_bytes().to_vec(),
            flags: 0,
            method: 0,
            crc: dust_data::zip::crc32(body),
            compressed_size: body.len() as u32,
            uncompressed_size: body.len() as u32,
            body: body.to_vec(),
        })
        .collect();
    write_layouted_zip(&raw).0
}

/// One entry for [`write_layouted_zip`]: every field both of the archive's
/// records carry, under the test's control.
///
/// The ordinary builder fills these in consistently; this one exists so the
/// hostile-crafting tests can make the two records *disagree*, or promise a
/// name is UTF-8 when it is not. Nothing here is reachable from a well-formed
/// pack, which is the point.
#[derive(Clone)]
pub struct RawZipEntry {
    /// Name bytes exactly as they sit in the archive — not necessarily UTF-8.
    pub name: Vec<u8>,
    pub flags: u16,
    pub method: u16,
    /// Checksum of the **uncompressed** payload, which is what a real writer
    /// records whatever the compression method is.
    pub crc: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    /// The bytes between the headers: a stored payload or a deflate stream.
    pub body: Vec<u8>,
}

/// Where each entry's two records start, plus the end-of-central-directory.
///
/// Returned so a test can patch one field of one record without doing
/// arithmetic against a layout it cannot see.
pub struct ZipLayout {
    pub entries: Vec<ZipEntryLayout>,
    pub eocd: usize,
}

pub struct ZipEntryLayout {
    pub local_header: usize,
    pub central_directory: usize,
}

/// Write a zip with full control over every recorded field, and hand back
/// where everything landed.
pub fn write_layouted_zip(entries: &[RawZipEntry]) -> (Vec<u8>, ZipLayout) {
    const LOCAL: u32 = 0x0403_4b50;
    const CENTRAL: u32 = 0x0201_4b50;
    const EOCD: u32 = 0x0605_4b50;

    let mut out = Vec::new();
    let mut layout = ZipLayout {
        entries: Vec::new(),
        eocd: 0,
    };

    // Pass one: every local record, back to back.
    let mut offsets = Vec::with_capacity(entries.len());
    for entry in entries {
        let local_header = out.len();
        out.extend_from_slice(&LOCAL.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // time
        out.extend_from_slice(&0x21_u16.to_le_bytes()); // date: 1980-01-01
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&entry.compressed_size.to_le_bytes());
        out.extend_from_slice(&entry.uncompressed_size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // extra
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.body);
        offsets.push(local_header);
    }

    // Pass two: the central directory, one record per entry, contiguous.
    for (entry, &local_header) in entries.iter().zip(&offsets) {
        let central_directory = out.len();
        out.extend_from_slice(&CENTRAL.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // time
        out.extend_from_slice(&0x21_u16.to_le_bytes()); // date
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&entry.compressed_size.to_le_bytes());
        out.extend_from_slice(&entry.uncompressed_size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // extra
        out.extend_from_slice(&0_u16.to_le_bytes()); // comment
        out.extend_from_slice(&0_u16.to_le_bytes()); // disk start
        out.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&(local_header as u32).to_le_bytes());
        out.extend_from_slice(&entry.name);
        layout.entries.push(ZipEntryLayout {
            local_header,
            central_directory,
        });
    }

    let directory_size = out.len()
        - layout
            .entries
            .first()
            .map_or(out.len(), |first| first.central_directory);
    let eocd = out.len();
    out.extend_from_slice(&EOCD.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0_u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory_size as u32).to_le_bytes());
    out.extend_from_slice(
        &(layout
            .entries
            .first()
            .map_or(0, |first| first.central_directory) as u32)
            .to_le_bytes(),
    );
    out.extend_from_slice(&0_u16.to_le_bytes()); // comment len
    layout.eocd = eocd;
    (out, layout)
}

/// A DEFLATE stream of one final, **fixed-Huffman** block holding `payload`
/// as literals — no back-references, nothing clever.
///
/// Exists because the deflated-entry paths must be exercisable on machines
/// with no system `zip`, and because a stream produced by code nobody in this
/// repository wrote would defeat itself; the encoder here is twenty lines of
/// RFC 1951 §3.2.6 and the decoder still has to earn the bytes through its
/// own Huffman tables. Every literal goes through the fixed literal table;
/// the CRC the caller records is what proves the round trip.
pub fn write_deflate_fixed(payload: &[u8]) -> Vec<u8> {
    let mut bits = DeflateBits::new();
    bits.header_final_fixed();
    for &byte in payload {
        if byte < 144 {
            bits.code(0x30 + u32::from(byte), 8);
        } else {
            bits.code(0x190 + u32::from(byte - 144), 9);
        }
    }
    bits.end_of_block();
    bits.finish()
}

/// A fixed-Huffman stream that expands `repeats` times via back-references:
/// three literals, then `length 258, distance 3` pairs.
///
/// This is the zip-bomb shape — roughly 1.7 bytes of stream per 258 bytes of
/// output — written out because the cap on expansion must be tested against
/// a stream that genuinely wants to blow past it, not against a stored entry
/// merely *claiming* to be large.
pub fn deflate_repeat_stream(repeats: usize) -> Vec<u8> {
    let mut bits = DeflateBits::new();
    bits.header_final_fixed();
    for &byte in b"abc" {
        bits.code(0x30 + u32::from(byte), 8);
    }
    for _ in 0..repeats {
        // Length symbol 285: base 258, no extra bits.
        bits.code(0b1100_0101, 8);
        // Distance symbol 2: base 3, no extra bits, five-bit code.
        bits.code(0b00010, 5);
    }
    bits.end_of_block();
    bits.finish()
}

/// Bit-level output for the two streams above: plain fields go in
/// least-significant-first, Huffman codes most-significant-first, per
/// RFC 1951 §3.1.1.
struct DeflateBits {
    out: Vec<u8>,
    held: u32,
    used: u32,
}

impl DeflateBits {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            held: 0,
            used: 0,
        }
    }

    fn push_lsb(&mut self, value: u32, count: u32) {
        self.held |= value << self.used;
        self.used += count;
        while self.used >= 8 {
            self.out.push((self.held & 0xff) as u8);
            self.held >>= 8;
            self.used -= 8;
        }
    }

    /// One final block with the fixed tables' type bits.
    fn header_final_fixed(&mut self) {
        self.push_lsb(1, 1); // BFINAL
        self.push_lsb(1, 2); // BTYPE = 01, fixed
    }

    fn code(&mut self, code: u32, count: u32) {
        for bit in (0..count).rev() {
            self.push_lsb((code >> bit) & 1, 1);
        }
    }

    fn end_of_block(&mut self) {
        // Symbol 256, the seven all-zero code.
        self.code(0, 7);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push((self.held & 0xff) as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// A private scratch directory per test.
// ---------------------------------------------------------------------------

/// A directory that exists only for one test, removed when the test ends.
pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("dust-data-tests-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Deterministic randomness.
// ---------------------------------------------------------------------------

/// xorshift64*, seeded from a caller-chosen number so a failing seed is
/// reproducible by typing the same number again.
///
/// No dependency, no global state, and nothing here pretends to be
/// cryptographically anything — it exists so "mutate this pack a thousand
/// ways" can run in a test without being the same thousand ways every run,
/// while still being *some* known thousand ways when one of them breaks.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Below `bound`, roughly uniformly; `bound` must be non-zero.
    pub fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    /// Pick a byte value biased toward ASCII, so mutations more often land on
    /// something the parsers have opinions about.
    pub fn mutated_byte(&mut self, original: u8) -> u8 {
        loop {
            let candidate = (self.next_u64() & 0xff) as u8;
            if candidate != original {
                return candidate;
            }
        }
    }
}

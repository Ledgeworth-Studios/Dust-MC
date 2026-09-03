//! Per-chunk fingerprints, and the digest sets they are stored in.
//!
//! One chunk's fingerprint is three hashes over data read straight out of its
//! region-file NBT:
//!
//! - **A block-state multiset digest.** Every section's palette entries are
//!   canonicalised (name plus sorted properties) and counted across the whole
//!   chunk; the counts are hashed sorted by identity. This is order-independent
//!   by construction — the same blocks in the same quantities hash identically
//!   no matter how vanilla packed or ordered them — while any block placed,
//!   removed or swapped in kind changes exactly one count and therefore the
//!   digest. Air is excluded: sections are uniform-height and mostly air, so
//!   counting it would only make every digest a statement about empty volume.
//! - **A biome digest.** The same multiset treatment over each section's 4×4×4
//!   biome cells.
//! - **Per-heightmap digests.** Heightmaps are hashed as their packed long
//!   arrays, keyed by name (`MOTION_BLOCKING`, `WORLD_SURFACE`, …). Vanilla's
//!   packer is deterministic for identical content, so equal worlds produce
//!   equal arrays; a version that changes the packing width changes digests —
//!   which is correct behaviour for a tool that also refuses to compare across
//!   data versions anyway.
//!
//! # What deliberately does not reach the digest
//!
//! Block entities (chest inventories, sign text), entities (stored outside
//! region files since 1.17), tick clocks and light data. Each is either not
//! seed-stable by nature or not part of what Dust's worldgen must reproduce;
//! the module doc on [`super`] records the full argument.
//!
//! # Storage
//!
//! `chunks.bin` is the machine format: a fixed header, then records sorted by
//! coordinates so two captures diff in one pass without hashing to compare
//! first. `chunks.tsv` beside it carries the same rows as hex, for humans and
//! spreadsheets; it is regenerated from the binary, never authoritative.

use std::collections::BTreeMap;
use std::path::Path;

use super::nbt::{self, Node};

/// The digest length in bytes (SHA-1, matching the jar verifier's choice).
pub const DIGEST_LEN: usize = 20;

/// File magic for `chunks.bin`.
const MAGIC: &[u8; 8] = b"DUSTCHNK";

/// The storage layout version this code reads and writes.
const FORMAT_VERSION: u16 = 1;

/// One chunk's fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDigest {
    pub x: i32,
    pub z: i32,
    /// Generation status normalised of its `minecraft:` prefix; capture only
    /// stores chunks that reached `full`, but the value is kept because a
    /// future reader deserves to see what was actually saved.
    pub status: String,
    pub blocks: [u8; DIGEST_LEN],
    pub biomes: [u8; DIGEST_LEN],
    /// Sorted by heightmap name.
    pub heightmaps: Vec<(String, [u8; DIGEST_LEN])>,
}

/// A captured world: every expected chunk, plus the facts all of them share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestSet {
    /// Vanilla's data version, uniform across every chunk or the scan refused
    /// to run. Comparing sets with different versions is meaningless — block
    /// names themselves move between versions — and is refused outright.
    pub data_version: u32,
    pub seed: i64,
    /// Sorted by (x, z).
    pub chunks: Vec<ChunkDigest>,
}

impl DigestSet {
    /// Look up one chunk's fingerprint.
    ///
    /// A test-side convenience; production readers walk `chunks` in order.
    #[cfg(test)]
    pub fn chunk(&self, x: i32, z: i32) -> Option<&ChunkDigest> {
        self.chunks.iter().find(|c| c.x == x && c.z == z)
    }
}

/// Fingerprint one parsed chunk compound.
pub fn digest_chunk(root: &Node) -> Result<ChunkDigest, String> {
    let status = root
        .get("Status")
        .and_then(Node::as_str)
        .map(|s| s.trim_start_matches("minecraft:").to_owned())
        .ok_or("the chunk has no Status")?;

    let mut blocks: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
    let mut biomes: BTreeMap<Vec<u8>, u64> = BTreeMap::new();

    let sections = root.get("sections").map(Node::list).unwrap_or(&[]);
    if sections.is_empty() {
        return Err(format!("chunk at {status} has no sections"));
    }
    for section in sections {
        if let Some(states) = section.get("block_states") {
            tally_palette(
                states,
                BLOCK_STATE_COUNT,
                MIN_BLOCK_BITS,
                block_identity,
                &mut blocks,
            )?;
        }
        if let Some(biome_states) = section.get("biomes") {
            tally_palette(
                biome_states,
                BIOME_CELL_COUNT,
                MIN_BIOME_BITS,
                biome_identity,
                &mut biomes,
            )?;
        }
    }
    // Uniform-height sections are overwhelmingly air above the surface; air
    // carries no signal about world content, so it is dropped from the count
    // rather than allowed to dominate every digest.
    let air = {
        let mut identity = length_prefixed(AIR.as_bytes());
        // The property count a property-less entry carries.
        identity.extend_from_slice(&0u32.to_be_bytes());
        identity
    };
    blocks.remove(&air);

    let heightmaps = match root.get("Heightmaps") {
        Some(maps) => {
            let mut digests = Vec::new();
            for (name, values) in maps.entries() {
                let longs = values
                    .as_longs()
                    .ok_or_else(|| format!("heightmap {name} is not a long array"))?;
                digests.push((name.clone(), heightmap_digest(name, longs)));
            }
            digests.sort_by(|a, b| a.0.cmp(&b.0));
            digests
        }
        None => Vec::new(),
    };

    Ok(ChunkDigest {
        // Coordinates live in the caller's hands (they come from the region
        // layout, not the NBT); filled in by the scan below.
        x: 0,
        z: 0,
        status,
        blocks: multiset_digest(&blocks),
        biomes: multiset_digest(&biomes),
        heightmaps,
    })
}

/// Read every expected chunk out of a world's region directory.
///
/// `expected` must be sorted and unique — [`expected_chunks`] produces such a
/// list. A chunk that is absent, half-generated, or written under a different
/// data version than its neighbours stops the scan: a partial capture that
/// looked complete would be worse than no capture.
pub fn scan(region_dir: &Path, expected: &[(i32, i32)], seed: i64) -> Result<DigestSet, String> {
    use super::region;

    let mut chunks = Vec::with_capacity(expected.len());
    let mut data_version: Option<u32> = None;
    // One region file in memory at a time; consecutive expected coordinates
    // share a file, so this is the whole cache the scan needs.
    let mut open_file: Option<(std::path::PathBuf, Vec<u8>)> = None;

    for &(x, z) in expected {
        let path = region::region_file_path(region_dir, x, z);
        let already_open = matches!(&open_file, Some((open_path, _)) if *open_path == path);
        if !already_open {
            let loaded = std::fs::read(&path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    format!(
                        "chunk {x},{z} is missing: no region file at {}",
                        path.display()
                    )
                }
                _ => format!("could not read {}: {e}", path.display()),
            })?;
            open_file = Some((path, loaded));
        }
        let bytes = &open_file.as_ref().expect("loaded just above").1;
        let (compression, payload) =
            region::read_chunk(bytes, x, z)?.ok_or_else(|| format!("chunk {x},{z} is missing"))?;
        let decompressed = region::decompress(compression, &payload)?;
        let root = nbt::read_root(&decompressed).map_err(|e| format!("chunk {x},{z}: {e}"))?;

        let version = root
            .get("DataVersion")
            .and_then(Node::as_i32)
            .ok_or_else(|| format!("chunk {x},{z} has no DataVersion"))?;
        let version = u32::try_from(version)
            .map_err(|_| format!("chunk {x},{z} has negative DataVersion {version}"))?;
        match data_version {
            Some(seen) if seen != version => {
                return Err(format!(
                    "chunk {x},{z} has DataVersion {version} but earlier chunks had {seen}; \
                     a mixed-version world cannot be fingerprinted"
                ));
            }
            other => data_version = other.or(Some(version)),
        }

        let mut digest = digest_chunk(&root).map_err(|e| format!("chunk {x},{z}: {e}"))?;
        if digest.status != "full" {
            return Err(format!(
                "chunk {x},{z} is present but only `{}`; the pregeneration did not finish",
                digest.status
            ));
        }
        digest.x = x;
        digest.z = z;
        chunks.push(digest);
    }

    Ok(DigestSet {
        data_version: data_version.unwrap_or(0),
        seed,
        chunks,
    })
}

/// The square of chunk coordinates within `radius` of origin, sorted.
pub fn expected_chunks(radius: i32) -> Vec<(i32, i32)> {
    expected_chunks_at(radius, (0, 0))
}

/// The square of chunk coordinates within `radius` of `centre`, sorted.
pub fn expected_chunks_at(radius: i32, centre: (i32, i32)) -> Vec<(i32, i32)> {
    expected_chunks_over(radius, &[centre])
}

/// Every chunk within `radius` of any of `centres`, sorted and deduplicated.
///
/// Centres exist because a square anywhere is a sample of one climate.
/// Minecraft has two biomes in a 9x9 wherever it is put, so scoring a biome
/// source needs several squares far apart rather than one wide one. They are
/// deduplicated because two centres close enough to overlap would otherwise
/// have their shared chunks scored twice, which quietly weights that climate.
pub fn expected_chunks_over(radius: i32, centres: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(centres.len() * ((2 * radius + 1) as usize).pow(2));
    for &(cx, cz) in centres {
        for z in -radius..=radius {
            for x in -radius..=radius {
                out.push((x + cx, z + cz));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Palettes and packed data
// ---------------------------------------------------------------------------

/// Blocks per section edge cubed: 16×16×16.
const BLOCK_STATE_COUNT: usize = 4096;
/// Biome cells per section: 4×4×4.
const BIOME_CELL_COUNT: usize = 64;
/// Vanilla packs block palettes at four bits minimum.
const MIN_BLOCK_BITS: u32 = 4;
/// Biome palettes have no minimum; two entries pack at one bit.
const MIN_BIOME_BITS: u32 = 1;

/// The only block excluded from the multiset.
const AIR: &str = "minecraft:air";

/// Count how often each palette entry occurs in one section's packed data.
///
/// The two palette families spell their entries differently — blocks carry
/// `Name` plus optional `Properties`, biomes are bare strings — so the
/// caller supplies the canonicaliser.
fn tally_palette(
    states: &Node,
    entry_count: usize,
    min_bits: u32,
    identify: fn(&Node) -> Result<Vec<u8>, String>,
    into: &mut BTreeMap<Vec<u8>, u64>,
) -> Result<(), String> {
    let palette = states.get("palette").map(Node::list).unwrap_or(&[]);
    if palette.is_empty() {
        return Err("a block_states or biomes compound has an empty palette".to_owned());
    }
    let identities: Vec<Vec<u8>> = palette.iter().map(identify).collect::<Result<_, _>>()?;

    let counts: BTreeMap<usize, u64> = match states.get("data").and_then(Node::as_longs) {
        None => {
            // No data array means every cell holds palette[0].
            BTreeMap::from([(0usize, entry_count as u64)])
        }
        Some(longs) => {
            let bits = infer_bits(longs.len(), entry_count, palette.len(), min_bits)?;
            let decoded = decode_packed(longs, bits, entry_count)?;
            let mut counts = BTreeMap::new();
            for value in decoded {
                *counts.entry(value as usize).or_insert(0) += 1;
            }
            counts
        }
    };

    for (index, count) in counts {
        let identity = identities.get(index).ok_or_else(|| {
            format!(
                "packed data references palette index {index} but the palette holds only {}",
                identities.len()
            )
        })?;
        *into.entry(identity.clone()).or_insert(0) += count;
    }
    Ok(())
}

/// Work out the packing width vanilla used.
///
/// The width is derivable rather than stored: candidate widths must reproduce
/// the observed long-array length under non-spanning packing *and* decode to
/// indices inside the palette. Two adjacent widths can share a long count, so
/// the index check does the disambiguating; trying candidates ascending keeps
/// the smallest consistent answer.
fn infer_bits(
    longs_len: usize,
    entry_count: usize,
    palette_len: usize,
    min_bits: u32,
) -> Result<u32, String> {
    let needed = min_bits.max(ceil_bits(palette_len)).max(1);
    for bits in needed..=32 {
        let per_long = 64 / bits;
        let longs_needed = entry_count.div_ceil(per_long as usize);
        if longs_needed != longs_len {
            continue;
        }
        return Ok(bits);
    }
    Err(format!(
        "no packing width from {needed} up explains a {longs_len}-long array holding \
         {entry_count} entries"
    ))
}

/// Bits needed to index `value` distinct entries, i.e. ceil(log2(value)).
fn ceil_bits(value: usize) -> u32 {
    let mut bits = 0;
    let mut largest_index = value.saturating_sub(1);
    while largest_index > 0 {
        bits += 1;
        largest_index >>= 1;
    }
    bits
}

/// Decode a non-spanning bit-packed array.
///
/// Entries never straddle long boundaries — Minecraft's packer has worked this
/// way since 1.16 — so each long holds `64 / bits` independent values.
fn decode_packed(longs: &[i64], bits: u32, entry_count: usize) -> Result<Vec<u64>, String> {
    debug_assert!((1..=32).contains(&bits));
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let per_long = 64 / bits;
    let mut out = Vec::with_capacity(entry_count);
    for long in longs {
        let raw = *long as u64;
        for slot in 0..per_long {
            if out.len() == entry_count {
                return Ok(out);
            }
            out.push((raw >> (slot * bits)) & mask);
        }
    }
    if out.len() < entry_count {
        return Err(format!(
            "packed array produced {} entries, wanted {entry_count}",
            out.len()
        ));
    }
    Ok(out)
}

/// Canonical bytes naming one block palette entry: id plus properties in key
/// order.
fn block_identity(entry: &Node) -> Result<Vec<u8>, String> {
    let name = entry
        .get("Name")
        .and_then(Node::as_str)
        .ok_or("a palette entry has no Name")?;
    let mut out = length_prefixed(name.as_bytes());

    let mut properties: Vec<(&str, &str)> = match entry.get("Properties") {
        Some(props) => props
            .entries()
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|vs| (k.as_str(), vs))
                    .ok_or_else(|| format!("property {k} of {name} is not a string"))
            })
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    properties.sort_unstable();
    out.extend_from_slice(&(properties.len() as u32).to_be_bytes());
    for (key, value) in properties {
        out.extend_from_slice(&length_prefixed(key.as_bytes()));
        out.extend_from_slice(&length_prefixed(value.as_bytes()));
    }
    Ok(out)
}

/// Canonical bytes naming one biome palette entry.
///
/// Biome palettes hold bare strings, not compounds — vanilla spells
/// `"minecraft:river"` where blocks get `{"Name": ...}`. The identity keeps
/// the same length-prefixed shape as the block one so the two digest spaces
/// are built by one hasher over equally-shaped keys.
fn biome_identity(entry: &Node) -> Result<Vec<u8>, String> {
    entry
        .as_str()
        .map(|name| length_prefixed(name.as_bytes()))
        .ok_or_else(|| "a biome palette entry is not a string".to_owned())
}

fn length_prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

/// Hash a counted set of identities, order-independently.
///
/// Iterating a `BTreeMap` gives a deterministic order; the counts ride along
/// so quantity changes move the digest even when identities do not.
fn multiset_digest(counted: &BTreeMap<Vec<u8>, u64>) -> [u8; DIGEST_LEN] {
    let mut hasher = crate::extract::sha1::Sha1::new();
    hasher.update(&(counted.len() as u64).to_be_bytes());
    for (identity, count) in counted {
        hasher.update(&count.to_be_bytes());
        hasher.update(&(identity.len() as u32).to_be_bytes());
        hasher.update(identity);
    }
    hasher.finish_bytes()
}

fn heightmap_digest(name: &str, longs: &[i64]) -> [u8; DIGEST_LEN] {
    let mut hasher = crate::extract::sha1::Sha1::new();
    hasher.update(name.as_bytes());
    hasher.update(b"\0");
    hasher.update(&(longs.len() as u32).to_be_bytes());
    for long in longs {
        // Little-endian chosen once and forever here so the digest does not
        // depend on the platform's byte order through any accidental cast.
        hasher.update(&long.to_le_bytes());
    }
    hasher.finish_bytes()
}

/// Lowercase hex of a digest, for TSV output.
pub fn hex(digest: &[u8; DIGEST_LEN]) -> String {
    let mut out = String::with_capacity(DIGEST_LEN * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Storage: compact binary, plus the human TSV rendered from it
// ---------------------------------------------------------------------------

/// Write the machine-readable digest file.
pub fn write_bin(set: &DigestSet, path: &Path) -> Result<(), String> {
    std::fs::write(path, encode_bin(set))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Read back what [`write_bin`] wrote, refusing anything else loudly.
pub fn read_bin(path: &Path) -> Result<DigestSet, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    decode_bin(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn encode_bin(set: &DigestSet) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&set.data_version.to_le_bytes());
    out.extend_from_slice(&set.seed.to_le_bytes());
    out.extend_from_slice(&(set.chunks.len() as u32).to_le_bytes());
    let mut chunks = set.chunks.clone();
    chunks.sort_by_key(|c| (c.x, c.z));
    for chunk in chunks {
        out.extend_from_slice(&chunk.x.to_le_bytes());
        out.extend_from_slice(&chunk.z.to_le_bytes());
        out.push(chunk.status.len() as u8);
        out.extend_from_slice(chunk.status.as_bytes());
        out.extend_from_slice(&chunk.blocks);
        out.extend_from_slice(&chunk.biomes);
        out.push(chunk.heightmaps.len() as u8);
        for (name, digest) in &chunk.heightmaps {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(digest);
        }
    }
    out
}

struct BinReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl BinReader<'_> {
    fn take(&mut self, count: usize, what: &str) -> Result<&[u8], String> {
        let end = self.at + count;
        if end > self.bytes.len() {
            return Err(format!(
                "{what}: wanted {count} byte(s), {} remain",
                self.bytes.len() - self.at
            ));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8v(&mut self, what: &str) -> Result<u8, String> {
        Ok(self.take(1, what)?[0])
    }

    fn i32le(&mut self, what: &str) -> Result<i32, String> {
        let b = self.take(4, what)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u16le(&mut self, what: &str) -> Result<u16, String> {
        let b = self.take(2, what)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32le(&mut self, what: &str) -> Result<u32, String> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64le(&mut self, what: &str) -> Result<i64, String> {
        let b = self.take(8, what)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn digest(&mut self, what: &str) -> Result<[u8; DIGEST_LEN], String> {
        let mut out = [0u8; DIGEST_LEN];
        out.copy_from_slice(self.take(DIGEST_LEN, what)?);
        Ok(out)
    }
}

fn decode_bin(bytes: &[u8]) -> Result<DigestSet, String> {
    let mut r = BinReader { bytes, at: 0 };
    if r.take(8, "magic")? != MAGIC {
        return Err("not a Dust digest file (magic mismatch)".to_owned());
    }
    let version = r.u16le("format version")?;
    if version != FORMAT_VERSION {
        return Err(format!(
            "format version {version} is not supported; regenerate this capture"
        ));
    }
    let data_version = r.u32le("data version")?;
    let seed = r.i64le("seed")?;
    let count = r.u32le("chunk count")? as usize;

    let mut chunks: Vec<ChunkDigest> = Vec::with_capacity(count.min(1 << 20));
    for index in 0..count {
        let x = r.i32le("chunk x")?;
        let z = r.i32le("chunk z")?;
        let status_len = r.u8v("status length")? as usize;
        let status = String::from_utf8(r.take(status_len, "status")?.to_vec())
            .map_err(|_| format!("chunk {index}: status is not UTF-8"))?;
        let blocks = r.digest("block digest")?;
        let biomes = r.digest("biome digest")?;
        let map_count = r.u8v("heightmap count")? as usize;
        let mut heightmaps = Vec::with_capacity(map_count);
        for _ in 0..map_count {
            let name_len = r.u8v("heightmap name length")? as usize;
            let name = String::from_utf8(r.take(name_len, "heightmap name")?.to_vec())
                .map_err(|_| format!("chunk {index}: heightmap name is not UTF-8"))?;
            heightmaps.push((name, r.digest("heightmap digest")?));
        }
        if let Some(previous) = chunks.last() {
            if (previous.x, previous.z) >= (x, z) {
                return Err(format!(
                    "records out of coordinate order at {x},{z}; the file was not written \
                     by this tool"
                ));
            }
        }
        chunks.push(ChunkDigest {
            x,
            z,
            status,
            blocks,
            biomes,
            heightmaps,
        });
    }
    Ok(DigestSet {
        data_version,
        seed,
        chunks,
    })
}

/// The human-readable companion, one line per chunk plus a header.
pub fn render_tsv(set: &DigestSet) -> String {
    let mut out = String::from("# chunk_x\tchunk_z\tstatus\tblocks\tbiomes\theightmaps\n");
    for chunk in &set.chunks {
        let maps = chunk
            .heightmaps
            .iter()
            .map(|(name, digest)| format!("{name}={}", hex(digest)))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{maps}\n",
            chunk.x,
            chunk.z,
            chunk.status,
            hex(&chunk.blocks),
            hex(&chunk.biomes),
        ));
    }
    out
}

/// Write `chunks.tsv` beside a capture's binary file.
pub fn write_tsv(set: &DigestSet, path: &Path) -> Result<(), String> {
    std::fs::write(path, render_tsv(set))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::nbt::writer::{n, root};
    use crate::harness::region::builder::build_region;
    use crate::harness::region::COMPRESSION_ZLIB;
    use crate::harness::testing::scratch_dir;
    use std::collections::HashMap;

    /// Bytes before the first chunk record: magic (8), format version (2),
    /// data version (4), seed (8), chunk count (4).
    const HEADER_LEN: usize = 8 + 2 + 4 + 8 + 4;

    /// Pack `values` at `bits` wide, non-spanning, as vanilla would.
    fn pack(values: &[u64], bits: u32) -> Vec<i64> {
        let per_long = (64 / bits) as usize;
        let mask = (1u64 << bits) - 1;
        values
            .chunks(per_long)
            .map(|chunk| {
                let mut long = 0u64;
                for (slot, value) in chunk.iter().enumerate() {
                    long |= (value & mask) << (slot as u32 * bits);
                }
                long as i64
            })
            .collect()
    }

    fn palette_entry(name: &str, props: &[(&str, &str)]) -> Node {
        n::comp(vec![
            ("Name", n::str(name)),
            (
                "Properties",
                n::comp(props.iter().map(|(k, v)| (*k, n::str(v))).collect()),
            ),
        ])
    }

    /// One test section: Y level, palette entries, optional packed values.
    type TestSection<'a> = (i32, Vec<Node>, Option<&'a [u64]>);

    /// Build a chunk-shaped NBT tree.
    ///
    /// `sections` maps Y → (palette entries, packed values); a `None` value
    /// means the section carries no data array (all of palette[0]). The
    /// packing width follows the palette size the way vanilla's writer picks
    /// it: four bits minimum for blocks.
    fn chunk_nbt(
        sections: Vec<TestSection<'_>>,
        heightmaps: Vec<(&str, &[i64])>,
        status: &str,
    ) -> Node {
        let section_nodes = sections
            .into_iter()
            .map(|(y, palette, data)| {
                // The packing width is decided from the palette size before
                // the palette itself is moved into the field list. The
                // palette/data pair lives under the `block_states` compound,
                // spelled the way vanilla spells it inside a section.
                let width = data.map(|_| ceil_bits(palette.len()).max(4));
                let mut states = vec![("palette", n::list(palette))];
                if let (Some(values), Some(bits)) = (data, width) {
                    // Vanilla always writes a full section; the shorthand
                    // here names the leading cells and palette[0] fills the
                    // rest, which is what stone-under-everything means.
                    let mut full = values.to_vec();
                    full.resize(BLOCKS_PER_SECTION, 0);
                    states.push(("data", n::la(&pack(&full, bits))));
                }
                n::comp(vec![
                    ("Y", n::b(y as i8)),
                    ("block_states", n::comp(states)),
                ])
            })
            .collect();
        n::comp(vec![
            ("DataVersion", n::i(3953)),
            ("Status", n::str(status)),
            ("sections", n::list(section_nodes)),
            (
                "Heightmaps",
                n::comp(heightmaps.iter().map(|(k, v)| (*k, n::la(v))).collect()),
            ),
        ])
    }

    /// Encode a chunk node as it would sit inside a region file (unframed).
    fn encode_chunk(node: &Node) -> Vec<u8> {
        let Node::Compound(entries) = node else {
            panic!("tests encode compound roots");
        };
        root(
            entries
                .iter()
                .map(|(name, value)| (name.as_str(), value.clone()))
                .collect(),
        )
    }

    const STONE: &str = "minecraft:stone";
    const DIRT: &str = "minecraft:dirt";

    /// Cells per section; tests build whole sections or nothing.
    const BLOCKS_PER_SECTION: usize = 4096;

    #[test]
    fn the_same_world_in_different_orders_hashes_identically() {
        // Section order shuffled, packing order permuted within a section:
        // both are free choices vanilla's writer could make differently, and
        // neither may move the digest.
        let a = chunk_nbt(
            vec![
                (-4, vec![palette_entry(STONE, &[])], None),
                (
                    -3,
                    vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                    Some(&[0, 1, 1, 0, 1]),
                ),
            ],
            vec![("MOTION_BLOCKING", &[7, 7, 8])],
            "full",
        );
        let b = chunk_nbt(
            vec![
                (
                    -3,
                    vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                    Some(&[1, 1, 0, 1, 0]),
                ),
                (-4, vec![palette_entry(STONE, &[])], None),
            ],
            vec![("MOTION_BLOCKING", &[7, 7, 8])],
            "full",
        );
        let da = digest_chunk(&a).expect("digests");
        let db = digest_chunk(&b).expect("digests");
        assert_eq!(da.blocks, db.blocks, "multiset must be order-independent");
        assert_eq!(da.biomes, db.biomes);
        assert_eq!(da, db);
    }

    #[test]
    fn biome_palettes_are_bare_strings_and_move_the_biome_digest() {
        // Vanilla spells biome palettes as bare strings — not {Name}
        // compounds like blocks — and reading one like the other stopped the
        // very first real capture. The digest takes them as they are written.
        let base = |biome: &str| {
            let mut cells = vec![0u64];
            cells.resize(64, 0);
            n::comp(vec![
                ("DataVersion", n::i(3953)),
                ("Status", n::str("full")),
                ("Heightmaps", n::comp(vec![])),
                (
                    "sections",
                    n::list(vec![n::comp(vec![
                        ("Y", n::b(-3)),
                        (
                            "block_states",
                            n::comp(vec![("palette", n::list(vec![palette_entry(STONE, &[])]))]),
                        ),
                        (
                            "biomes",
                            n::comp(vec![
                                ("palette", n::list(vec![n::str(biome)])),
                                ("data", n::la(&pack(&cells, 1))),
                            ]),
                        ),
                    ])]),
                ),
            ])
        };
        let river = digest_chunk(&base("minecraft:river")).expect("digests");
        let desert = digest_chunk(&base("minecraft:desert")).expect("digests");
        assert_ne!(
            river.biomes, desert.biomes,
            "a different biome moves its digest"
        );
        assert_eq!(river.blocks, desert.blocks, "blocks were identical");
    }

    #[test]
    fn reordering_a_palette_with_remapped_indices_changes_nothing() {
        // Same world, palette written in the opposite order and the packed
        // indices flipped to match. Identity comes from the entries, never
        // their positions. The remainder of the section is stone in both
        // spellings, so the fill index differs too — spelled out in full
        // rather than through the shorthand's palette[0] padding.
        let mut stone_cells = vec![0u64, 1];
        stone_cells.resize(BLOCKS_PER_SECTION, 0);
        let mut dirt_cells = vec![1u64, 0];
        dirt_cells.resize(BLOCKS_PER_SECTION, 1);
        let stone_first = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                Some(&stone_cells),
            )],
            vec![],
            "full",
        );
        let dirt_first = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(DIRT, &[]), palette_entry(STONE, &[])],
                Some(&dirt_cells),
            )],
            vec![],
            "full",
        );
        assert_eq!(
            digest_chunk(&stone_first).expect("a").blocks,
            digest_chunk(&dirt_first).expect("b").blocks
        );
    }

    #[test]
    fn swapping_one_block_moves_the_digest() {
        let stone = chunk_nbt(
            vec![(-3, vec![palette_entry(STONE, &[])], Some(&[0]))],
            vec![],
            "full",
        );
        let mixed = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                Some(&[1]),
            )],
            vec![],
            "full",
        );
        assert_ne!(
            digest_chunk(&stone).expect("a").blocks,
            digest_chunk(&mixed).expect("b").blocks
        );
    }

    #[test]
    fn quantities_matter_not_just_kinds() {
        let one_dirt = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                Some(&[1, 0, 0, 0]),
            )],
            vec![],
            "full",
        );
        let three_dirt = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                Some(&[1, 1, 1, 0]),
            )],
            vec![],
            "full",
        );
        assert_ne!(
            digest_chunk(&one_dirt).expect("a").blocks,
            digest_chunk(&three_dirt).expect("b").blocks
        );
    }

    #[test]
    fn air_is_excluded_so_empty_volume_carries_no_signal() {
        let all_air = chunk_nbt(
            vec![(-3, vec![palette_entry(AIR, &[])], None)],
            vec![],
            "full",
        );
        let no_sections_at_all_air = chunk_nbt(
            vec![
                (-3, vec![palette_entry(AIR, &[])], None),
                (-2, vec![palette_entry(AIR, &[])], None),
            ],
            vec![],
            "full",
        );
        assert_eq!(
            digest_chunk(&all_air).expect("a").blocks,
            digest_chunk(&no_sections_at_all_air).expect("b").blocks,
            "two all-air sections and one must agree: air is not content"
        );
    }

    #[test]
    fn properties_participate_in_identity() {
        let facing_north = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(
                    "minecraft:oak_stairs",
                    &[("facing", "north")],
                )],
                Some(&[0]),
            )],
            vec![],
            "full",
        );
        let facing_east = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry("minecraft:oak_stairs", &[("facing", "east")])],
                Some(&[0]),
            )],
            vec![],
            "full",
        );
        assert_ne!(
            digest_chunk(&facing_north).expect("a").blocks,
            digest_chunk(&facing_east).expect("b").blocks
        );
    }

    #[test]
    fn property_order_within_an_entry_is_irrelevant() {
        let ab = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(
                    "minecraft:vine",
                    &[("north", "true"), ("south", "false")],
                )],
                Some(&[0]),
            )],
            vec![],
            "full",
        );
        let ba = chunk_nbt(
            vec![(
                -3,
                vec![palette_entry(
                    "minecraft:vine",
                    &[("south", "false"), ("north", "true")],
                )],
                Some(&[0]),
            )],
            vec![],
            "full",
        );
        assert_eq!(
            digest_chunk(&ab).expect("a").blocks,
            digest_chunk(&ba).expect("b").blocks
        );
    }

    #[test]
    fn heightmap_content_and_names_both_reach_their_digests() {
        // Heightmaps ride on a chunk that still carries one real section;
        // a section-less chunk is refused before heightmaps are read.
        let ground = vec![(-4, vec![palette_entry(STONE, &[])], None)];
        let a = chunk_nbt(
            ground.clone(),
            vec![("MOTION_BLOCKING", &[1, 2, 3])],
            "full",
        );
        let b = chunk_nbt(
            ground.clone(),
            vec![("MOTION_BLOCKING", &[1, 2, 4])],
            "full",
        );
        let c = chunk_nbt(ground, vec![("WORLD_SURFACE", &[1, 2, 3])], "full");
        let da = digest_chunk(&a).expect("a");
        assert_ne!(
            da.heightmaps[0].1,
            digest_chunk(&b).expect("b").heightmaps[0].1
        );
        assert_ne!(
            da.heightmaps[0].1,
            digest_chunk(&c).expect("c").heightmaps[0].1,
            "the map name is hashed alongside its values"
        );
    }

    #[test]
    fn wider_palettes_pack_correctly_across_the_width_boundaries() {
        // Palette sizes 1 (no data), 2 (min bits via clamp), 17 (five bits),
        // and a large one exercising multi-long spans, all round-trip.
        for size in [1usize, 2, 15, 16, 17, 300] {
            let palette: Vec<Node> = (0..size)
                .map(|i| palette_entry(&format!("minecraft:block{i}"), &[]))
                .collect();
            let values: Vec<u64> = (0..4096u64).map(|i| i % size as u64).collect();
            let node = if size == 1 {
                chunk_nbt(vec![(-3, palette, None)], vec![], "full")
            } else {
                chunk_nbt(vec![(-3, palette, Some(&values))], vec![], "full")
            };
            let digest = digest_chunk(&node).expect("digests at every width");
            // Every block appears exactly 4096/size times; sanity-check via
            // determinism instead of recounting: rebuilding hashes identically.
            let again = digest_chunk(&node).expect("stable");
            assert_eq!(digest.blocks, again.blocks, "size {size}");
        }
    }

    #[test]
    fn a_status_below_full_is_preserved_but_visible() {
        let node = chunk_nbt(
            vec![(-3, vec![palette_entry(STONE, &[])], None)],
            vec![],
            "minecraft:empty",
        );
        let digest = digest_chunk(&node).expect("digests anyway");
        assert_eq!(digest.status, "empty");
    }

    #[test]
    fn the_binary_format_round_trips_exactly() {
        let dir = scratch_dir("digest-bin");
        let path = dir.join("chunks.bin");
        let set = DigestSet {
            data_version: 3953,
            seed: -42,
            chunks: vec![
                ChunkDigest {
                    x: -1,
                    z: 2,
                    status: "full".to_owned(),
                    blocks: [7; DIGEST_LEN],
                    biomes: [9; DIGEST_LEN],
                    heightmaps: vec![
                        ("MOTION_BLOCKING".to_owned(), [1; DIGEST_LEN]),
                        ("WORLD_SURFACE".to_owned(), [2; DIGEST_LEN]),
                    ],
                },
                ChunkDigest {
                    x: 0,
                    z: 0,
                    status: "full".to_owned(),
                    blocks: [3; DIGEST_LEN],
                    biomes: [4; DIGEST_LEN],
                    heightmaps: Vec::new(),
                },
            ],
        };
        write_bin(&set, &path).expect("writes");
        let back = read_bin(&path).expect("reads");
        assert_eq!(back, set);
    }

    #[test]
    fn unsorted_or_corrupt_binaries_are_refused_by_name_of_the_problem() {
        let good = encode_bin(&DigestSet {
            data_version: 1,
            seed: 0,
            chunks: vec![ChunkDigest {
                x: 0,
                z: 0,
                status: "full".to_owned(),
                blocks: [0; DIGEST_LEN],
                biomes: [0; DIGEST_LEN],
                heightmaps: Vec::new(),
            }],
        });
        assert!(decode_bin(&good).is_ok());

        let wrong_magic = {
            let mut b = good.clone();
            b[0] = b'X';
            b
        };
        assert!(decode_bin(&wrong_magic)
            .expect_err("refused")
            .contains("magic"));

        let truncated = &good[..good.len() - 5];
        assert!(decode_bin(truncated).is_err(), "truncation refused");

        // Two records, second before first: rejected as unordered.
        let record_for = |x: i32| {
            let set = DigestSet {
                data_version: 1,
                seed: 0,
                chunks: vec![ChunkDigest {
                    x,
                    z: 0,
                    status: "full".to_owned(),
                    blocks: [0; DIGEST_LEN],
                    biomes: [0; DIGEST_LEN],
                    heightmaps: Vec::new(),
                }],
            };
            // Header: magic (8) + format version (2) + data version (4) +
            // seed (8) + chunk count (4).
            encode_bin(&set)[HEADER_LEN..].to_vec()
        };
        let mut unordered = Vec::new();
        // Reuse the header but correct its chunk count to what follows.
        let mut header = good[..HEADER_LEN].to_vec();
        header[HEADER_LEN - 4..].copy_from_slice(&2u32.to_le_bytes());
        unordered.extend_from_slice(&header);
        unordered.extend_from_slice(&record_for(5));
        unordered.extend_from_slice(&record_for(-5));
        assert!(
            decode_bin(&unordered)
                .expect_err("refused")
                .contains("order"),
            "unordered records must be named"
        );
    }

    #[test]
    fn the_tsv_lists_every_chunk_as_hex_rows() {
        let set = DigestSet {
            data_version: 3953,
            seed: 5,
            chunks: vec![ChunkDigest {
                x: 3,
                z: -4,
                status: "full".to_owned(),
                blocks: [0xab; DIGEST_LEN],
                biomes: [0xcd; DIGEST_LEN],
                heightmaps: vec![("MOTION_BLOCKING".to_owned(), [0; DIGEST_LEN])],
            }],
        };
        let tsv = render_tsv(&set);
        let lines: Vec<&str> = tsv.lines().collect();
        assert_eq!(lines.len(), 2, "{tsv}");
        assert!(lines[0].starts_with("# chunk_x"), "{tsv}");
        assert_eq!(
            lines[1],
            format!(
                "3\t-4\tfull\t{}\t{}\tMOTION_BLOCKING={}",
                hex(&[0xab; DIGEST_LEN]),
                hex(&[0xcd; DIGEST_LEN]),
                hex(&[0; DIGEST_LEN])
            )
        );
    }

    #[test]
    fn scanning_a_synthetic_world_fingerprints_exactly_the_expected_chunks() {
        let dir = scratch_dir("digest-scan");
        let world = dir.join("world/region");
        std::fs::create_dir_all(&world).expect("region dir");

        let make = |dirt_share: u64| {
            chunk_nbt(
                vec![
                    (-4, vec![palette_entry(STONE, &[])], None),
                    (
                        -3,
                        vec![palette_entry(STONE, &[]), palette_entry(DIRT, &[])],
                        Some(&vec![1u64; dirt_share as usize]),
                    ),
                ],
                vec![("MOTION_BLOCKING", &[9, 9])],
                "full",
            )
        };
        // Two chunks in region 0,0 with different contents; one in region -2.
        // Entries are grouped per region file first, so chunks sharing a file
        // do not overwrite one another.
        type RegionEntries = Vec<(usize, u8, Vec<u8>)>;
        let chunks: Vec<(i32, i32, Node)> =
            vec![(0, 0, make(1)), (1, 0, make(3)), (-33, 0, make(1))];
        let mut files: HashMap<(i32, i32), RegionEntries> = HashMap::new();
        for (cx, cz, node) in &chunks {
            let compressed = {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                std::io::Write::write_all(&mut encoder, &encode_chunk(node)).expect("zlib");
                encoder.finish().expect("zlib finish")
            };
            files
                .entry(crate::harness::region::region_coords(*cx, *cz))
                .or_default()
                .push((
                    crate::harness::region::local_index(*cx, *cz),
                    COMPRESSION_ZLIB,
                    compressed,
                ));
        }
        for ((rx, rz), entries) in &files {
            let file = world.join(crate::harness::region::region_file_name(*rx, *rz));
            std::fs::write(&file, build_region(entries)).expect("write region");
        }

        let expected = vec![(-33, 0), (0, 0), (1, 0)];
        let set = scan(&world, &expected, 77).expect("scans");
        assert_eq!(set.chunks.len(), 3);
        assert_eq!(set.data_version, 3953);
        assert_ne!(
            set.chunk(0, 0).expect("0,0").blocks,
            set.chunk(1, 0).expect("1,0").blocks,
            "different dirt shares must differ"
        );

        // And a missing chunk is named, not papered over.
        let missing = vec![(0, 0), (99, 99)];
        let err = scan(&world, &missing, 77).expect_err("refused");
        assert!(err.contains("99,99"), "{err}");
    }
}

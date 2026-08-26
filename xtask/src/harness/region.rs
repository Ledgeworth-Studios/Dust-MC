//! The anvil region layout, read directly.
//!
//! A vanilla overworld saves its chunks into `region/r.X.Z.mca` files, 32×32
//! chunks each. The format has been stable for a decade and is small enough to
//! own: an 8 KiB header of 4-byte location records (three bytes of sector
//! offset, one byte of sector count) followed by the sectors themselves, where
//! a chunk begins with a big-endian length that includes a one-byte compression
//! tag. This module reads exactly that and nothing else; writing is not
//! supported because nothing here writes worlds.
//!
//! Like [`super::nbt`], this exists because `dust-nbt`/`dust-data` are not yet
//! implemented on this base. It is deliberately narrower than either will be:
//! no cache-file handling (`mcr`), no external-entity files, no writes, and
//! every assumption stated as a check — a sector run that would leave the file
//! is an error naming the chunk rather than a panic in the middle of a capture.

use std::io::Read;
use std::path::Path;

/// Chunks per region edge.
pub const REGION_CHUNKS: i32 = 32;

/// Bytes per sector, per the format.
const SECTOR: usize = 4096;

/// Compression tag: gzip (the original choice, rare now).
pub const COMPRESSION_GZIP: u8 = 1;
/// Compression tag: zlib, what current vanilla writes.
pub const COMPRESSION_ZLIB: u8 = 2;
/// Compression tag: stored uncompressed.
pub const COMPRESSION_NONE: u8 = 3;

/// Which region file holds `chunk_x`, `chunk_z`.
///
/// Arithmetic shift, so negative coordinates land in negative regions the way
/// the game lays them out: chunk -1 lives in region -1, at local index 31.
pub fn region_coords(chunk_x: i32, chunk_z: i32) -> (i32, i32) {
    (chunk_x >> 5, chunk_z >> 5)
}

/// The chunk's index within its region's 1024-entry tables.
pub fn local_index(chunk_x: i32, chunk_z: i32) -> usize {
    let x = (chunk_x & (REGION_CHUNKS - 1)) as usize;
    let z = (chunk_z & (REGION_CHUNKS - 1)) as usize;
    // Row-major with z as the row: the order the header itself uses.
    x + z * REGION_CHUNKS as usize
}

/// The conventional file name for a region.
pub fn region_file_name(region_x: i32, region_z: i32) -> String {
    format!("r.{region_x}.{region_z}.mca")
}

/// The path of the region file holding this chunk, under a `region/` dir.
pub fn region_file_path(region_dir: &Path, chunk_x: i32, chunk_z: i32) -> std::path::PathBuf {
    let (rx, rz) = region_coords(chunk_x, chunk_z);
    region_dir.join(region_file_name(rx, rz))
}

/// Read one chunk's compressed payload out of a loaded region file.
///
/// `Ok(None)` means the header lists no data for this chunk — a chunk that has
/// never been generated. Every other outcome is either bytes or an error
/// naming the coordinates involved.
pub fn read_chunk(
    file: &[u8],
    chunk_x: i32,
    chunk_z: i32,
) -> Result<Option<(u8, Vec<u8>)>, String> {
    let slot = local_index(chunk_x, chunk_z);
    let entry_at = slot * 4;
    if file.len() < 2 * SECTOR {
        return Err(format!(
            "{chunk_x},{chunk_z}: region file is {} bytes, smaller than the two-table header",
            file.len()
        ));
    }
    let raw = [
        file[entry_at],
        file[entry_at + 1],
        file[entry_at + 2],
        file[entry_at + 3],
    ];
    let offset_sectors = u32::from_be_bytes(raw) >> 8;
    let sector_count = (u32::from_be_bytes(raw) & 0xff) as usize;
    if offset_sectors == 0 {
        return Ok(None);
    }

    let start = offset_sectors as usize * SECTOR;
    // The length field sits inside the first sector; the declared run must fit
    // both the file and its own claim about how many sectors it occupies,
    // otherwise the writer truncated mid-chunk and the NBT beneath is suspect.
    if start + 4 > file.len() {
        return Err(format!(
            "{chunk_x},{chunk_z}: chunk starts at byte {start}, past the end of a {}-byte \
             file",
            file.len()
        ));
    }
    let length = u32::from_be_bytes([
        file[start],
        file[start + 1],
        file[start + 2],
        file[start + 3],
    ]) as usize;
    let compression = file[start + 4];
    let payload_end = start + 4 + length;
    if payload_end > file.len() {
        return Err(format!(
            "{chunk_x},{chunk_z}: payload claims {length} bytes but only {} follow",
            file.len() - start - 4
        ));
    }
    let minimum_sectors = (4 + length).div_ceil(SECTOR);
    if sector_count != 0 && minimum_sectors > sector_count {
        return Err(format!(
            "{chunk_x},{chunk_z}: payload needs {minimum_sectors} sectors but its header \
             reserves {sector_count}"
        ));
    }
    Ok(Some((compression, file[start + 5..payload_end].to_vec())))
}

/// Decompress a chunk payload by its tag.
pub fn decompress(compression: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    match compression {
        COMPRESSION_GZIP => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(payload)
                .read_to_end(&mut out)
                .map_err(|e| format!("gzip chunk did not decompress: {e}"))?;
            Ok(out)
        }
        COMPRESSION_ZLIB => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(payload)
                .read_to_end(&mut out)
                .map_err(|e| format!("zlib chunk did not decompress: {e}"))?;
            Ok(out)
        }
        COMPRESSION_NONE => Ok(payload.to_vec()),
        other => Err(format!("unknown chunk compression tag {other}")),
    }
}

/// The world-space coordinates of every chunk listed in a region file header.
///
/// Presence in the header is presence on disk as far as pregeneration goes;
/// callers still verify each chunk's `Status` before hashing it, which is
/// where a half-generated chunk would actually show up. Used by the smoke
/// tests that walk a real cached world; the capture itself always knows
/// which chunks it asked for.
#[cfg(test)]
pub fn listed_chunks(file: &[u8], region_x: i32, region_z: i32) -> Result<Vec<(i32, i32)>, String> {
    if file.len() < 2 * SECTOR {
        return Err("region file smaller than its header".to_owned());
    }
    let mut found = Vec::new();
    for z in 0..REGION_CHUNKS {
        for x in 0..REGION_CHUNKS {
            let entry = ((x + z * REGION_CHUNKS) as usize) * 4;
            let offset = u32::from_be_bytes([
                file[entry],
                file[entry + 1],
                file[entry + 2],
                file[entry + 3],
            ]) >> 8;
            if offset != 0 {
                found.push((region_x * REGION_CHUNKS + x, region_z * REGION_CHUNKS + z));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
pub(crate) mod builder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_math_agrees_with_the_layout_on_every_quadrant() {
        // Hand-checked against the format: region of a chunk is floor-div by
        // 32, the local index counts x fastest within a 32-wide row.
        assert_eq!(region_coords(0, 0), (0, 0));
        assert_eq!(region_coords(31, 31), (0, 0));
        assert_eq!(region_coords(32, 0), (1, 0));
        assert_eq!(region_coords(-1, -1), (-1, -1));
        assert_eq!(region_coords(-32, -32), (-1, -1));
        assert_eq!(region_coords(-33, 64), (-2, 2));

        assert_eq!(local_index(0, 0), 0);
        assert_eq!(local_index(31, 0), 31);
        assert_eq!(local_index(0, 1), 32);
        assert_eq!(local_index(31, 31), 1023);
        // Negative locals wrap to the far edge of their region.
        assert_eq!(local_index(-1, -1), 31 + 31 * 32);
        // And the pair is consistent: mapping back through the same maths.
        for &(cx, cz) in &[
            (0, 0),
            (1, 2),
            (31, 31),
            (32, 32),
            (-1, 0),
            (0, -1),
            (-33, -65),
            (500, -500),
        ] {
            let (rx, rz) = region_coords(cx, cz);
            let back = (rx * 32 + (cx & 31), rz * 32 + (cz & 31));
            assert_eq!(back, (cx, cz), "round trip for {cx},{cz}");
        }
    }

    #[test]
    fn region_files_name_their_region() {
        assert_eq!(region_file_name(0, 0), "r.0.0.mca");
        assert_eq!(region_file_name(-1, 2), "r.-1.2.mca");
    }

    #[test]
    fn a_written_chunk_reads_back_through_the_real_path() {
        let entries = vec![(local_index(0, 0), COMPRESSION_ZLIB, b"first".to_vec())];
        let file = builder::build_region(&entries);
        let read = read_chunk(&file, 0, 0).expect("reads").expect("present");
        assert_eq!(read, (COMPRESSION_ZLIB, b"first".to_vec()));
        // Its neighbours were never written.
        assert_eq!(read_chunk(&file, 1, 0).expect("reads"), None);
        assert_eq!(read_chunk(&file, -1, 0).expect("reads"), None);
    }

    #[test]
    fn chunks_far_apart_share_a_file_without_colliding() {
        let a = (local_index(5, 7), COMPRESSION_ZLIB, b"alpha".to_vec());
        let b = (local_index(-6, -8), COMPRESSION_ZLIB, b"omega".to_vec());
        let file = builder::build_region(&[a, b]);
        assert_eq!(
            read_chunk(&file, 5, 7).expect("reads").expect("a"),
            (COMPRESSION_ZLIB, b"alpha".to_vec())
        );
        assert_eq!(
            read_chunk(&file, -6, -8).expect("reads").expect("b"),
            (COMPRESSION_ZLIB, b"omega".to_vec())
        );
    }

    #[test]
    fn all_three_compression_tags_round_trip() {
        // The tag describes the payload, so each payload is really encoded
        // that way: gzip and zlib streams, and the raw bytes for `none`.
        use std::io::Write as _;
        let raw = b"payload".to_vec();
        for tag in [COMPRESSION_GZIP, COMPRESSION_ZLIB, COMPRESSION_NONE] {
            let encoded = match tag {
                COMPRESSION_GZIP => {
                    let mut encoder =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    encoder.write_all(&raw).expect("gzip");
                    encoder.finish().expect("gzip finish")
                }
                COMPRESSION_ZLIB => {
                    let mut encoder =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    encoder.write_all(&raw).expect("zlib");
                    encoder.finish().expect("zlib finish")
                }
                _ => raw.clone(),
            };
            let file = builder::build_region(&[(0, tag, encoded)]);
            let read = read_chunk(&file, 0, 0).expect("reads").expect("present");
            assert_eq!(read.0, tag);
            assert_eq!(decompress(read.0, &read.1).expect("inflates"), raw);
        }
    }

    #[test]
    fn a_truncated_sector_run_is_an_error_naming_the_chunk() {
        let mut file = builder::build_region(&[(0, COMPRESSION_ZLIB, b"payload".to_vec())]);
        file.truncate(file.len() - 100);
        let err = read_chunk(&file, 0, 0).expect_err("refused");
        assert!(err.contains("0,0"), "{err}");
    }

    #[test]
    fn a_header_slot_reserving_fewer_sectors_than_needed_is_refused() {
        // Builder writes honest headers; forge a lying one by hand.
        let mut file = builder::build_region(&[(0, COMPRESSION_ZLIB, vec![7u8; 9000])]);
        // 9001 framed bytes need three sectors; claim two.
        file[3] = 2;
        assert!(
            read_chunk(&file, 0, 0).is_err(),
            "under-reserved run refused"
        );
    }

    #[test]
    fn listed_chunks_reports_exactly_what_was_written_in_world_coordinates() {
        let entries = vec![
            (local_index(0, 0), COMPRESSION_ZLIB, b"a".to_vec()),
            (local_index(31, 31), COMPRESSION_ZLIB, b"b".to_vec()),
        ];
        let file = builder::build_region(&entries);
        let listed = listed_chunks(&file, 3, -4).expect("lists");
        assert_eq!(listed, vec![(96, -128), (127, -97)]);
    }

    #[test]
    fn an_empty_header_lists_nothing() {
        let file = builder::build_region(&[]);
        assert!(listed_chunks(&file, 0, 0).expect("lists").is_empty());
    }

    #[test]
    fn an_undersized_file_is_rejected_before_any_index_math_runs() {
        assert!(read_chunk(&[0u8; 16], 0, 0).is_err());
        assert!(listed_chunks(&[], 0, 0).is_err());
    }
}

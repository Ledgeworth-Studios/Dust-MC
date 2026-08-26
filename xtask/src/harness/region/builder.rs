//! Synthetic region files for the tests.
//!
//! The real writer of these files is the vanilla server, and the licensing
//! rule keeps any of its output out of the repository — so the tests construct
//! their own. Building the bytes by hand is also what makes the tests honest:
//! they exercise exactly what this module claims about the layout rather than
//! whatever a fixture happened to contain.

use super::SECTOR;

/// Assemble a region file from `(local slot, compression tag, payload)`.
///
/// Each payload is framed as the format frames it — big-endian length
/// including the compression byte — and given its own sectors; slots left out
/// stay zeroed, which is how "never generated" is spelled in a header.
pub(crate) fn build_region(entries: &[(usize, u8, Vec<u8>)]) -> Vec<u8> {
    let location_table = 2 * SECTOR;
    let mut file = vec![0u8; location_table];
    let mut next_sector = location_table / SECTOR;

    for &(slot, tag, ref payload) in entries {
        debug_assert!(slot < 1024, "test built an out-of-range slot");
        let framed_len = payload.len() + 1;
        let mut framed = Vec::with_capacity(4 + framed_len);
        framed.extend_from_slice(&(framed_len as u32).to_be_bytes());
        framed.push(tag);
        framed.extend_from_slice(payload);

        let sectors = framed.len().div_ceil(SECTOR);
        let offset = next_sector;
        let start = offset * SECTOR;
        // Grow exactly far enough for this run; `resize` never shrinks, so
        // runs written back-to-back keep their allocated sectors apart even
        // when a payload ends mid-sector.
        if file.len() < start + framed.len() {
            file.resize(start + framed.len(), 0);
        }
        file[start..start + framed.len()].copy_from_slice(&framed);

        let entry = slot * 4;
        file[entry] = (offset >> 16) as u8;
        file[entry + 1] = (offset >> 8) as u8;
        file[entry + 2] = offset as u8;
        file[entry + 3] = sectors as u8;
        // Touch the timestamp table too so it does not read as all-zero.
        file[SECTOR + entry] = 1;

        next_sector += sectors;
    }
    file
}

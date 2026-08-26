//! Chunk sections on the wire.
//!
//! `dust-protocol` owns the chunk packet's envelope and leaves the section
//! contents behind a trait, saying in its own documentation that this is the
//! crate that fills it in — because a paletted container's leading
//! bits-per-entry byte changes the meaning of everything after it, getting it
//! wrong reindexes every block in the chunk silently, and that is a format to
//! implement exactly once beside the storage that owns it.
//!
//! # The layout, as a real 1.21.1 server sends it
//!
//! Captured off the wire and decoded field by field until the section blob was
//! consumed exactly — 18,779 bytes claimed and 18,779 read, across
//! twenty-four sections. Every number below came from that, not from a wiki:
//!
//! ```text
//! per section, low y first:
//!   non_air_count : i16 big-endian
//!   block states  : paletted container, 16x16x16
//!   biomes        : paletted container, 4x4x4
//!
//! paletted container:
//!   bits_per_entry : u8
//!   0        -> VarInt value, then VarInt 0 (an empty long array)
//!   indirect -> VarInt palette length, that many VarInts, then the array
//!   direct   -> the array alone, no palette
//!   array    -> VarInt long count, then that many i64
//! ```
//!
//! Two details worth naming because a plausible implementation gets each
//! wrong. The empty long array after a single-valued palette **is written**,
//! as a zero count, rather than omitted. And the whole column is sent —
//! vanilla writes all twenty-four sections including the all-air ones above
//! the terrain, each costing five bytes.
//!
//! # Why this is not [`PalettedContainer::to_parts`]
//!
//! That method produces the *disk* form, and the disk and network forms differ
//! in a way that is invisible until a section gets large. On disk vanilla
//! re-palettes: it writes the values actually present and packs indices into
//! that list, so a global-tier section's array holds indices. On the network it
//! writes the container as it stands, palette and storage untouched, so a
//! global-tier section's array holds **registry ids** and carries no palette at
//! all.
//!
//! Sending the disk form would produce a chunk that decodes without error and
//! renders as the wrong blocks — every id read as a position in a list that was
//! never sent. So this writes the live container, which is also the cheaper of
//! the two: no re-palette per chunk per viewer.

use dust_protocol::packets::play::chunk::Section as WireSection;
use dust_protocol::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use dust_protocol::ProtocolVersion;

use crate::chunk::Section;
use crate::container::{PalettedContainer, Strategy};
use crate::palette::PaletteKind;

impl WireSection for Section {
    fn decode_wire<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        // The non-air count is read and dropped. It is a hint the client uses
        // to skip empty sections cheaply, and it is derivable from the blocks
        // that follow — so trusting it over them would let a sender's mistake
        // become this side's state. Recomputed on the way back out.
        let _non_air = read_short(input)?;
        let states = decode_container(input, Strategy::BLOCK_STATES, BLOCK_REGISTRY_HINT)?;
        let biomes = decode_container(input, Strategy::BIOMES, BIOME_REGISTRY_HINT)?;
        Ok(Section::new(
            states,
            biomes,
            crate::light::LightArray::filled(0),
            crate::light::LightArray::filled(0),
        ))
    }

    fn encode_wire<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let non_air = non_air_count(self.states());
        // A section cannot hold more than 4096 blocks, so this always fits;
        // written as a checked conversion anyway, because the alternative is a
        // silent wrap the client would read as a negative count.
        let non_air = i16::try_from(non_air).map_err(|_| EncodeError::TooManyElements {
            count: non_air as usize,
        })?;
        out.write_slice(&non_air.to_be_bytes());
        encode_container(self.states(), out);
        encode_container(self.biomes(), out);
        Ok(())
    }
}

/// How wide the block registry is assumed to be when decoding a container whose
/// palette is direct.
///
/// A direct container carries no palette, so its bits-per-entry is the only
/// statement of how wide the registry is — and the reader has to agree with the
/// sender about that or every cell lands at the wrong offset. Taking it from
/// the wire rather than from `dust-registry` is deliberate: this crate decodes
/// what it was sent, and a sender on a modded server has a wider registry than
/// this build knows about. The value below is only a floor for reconstruction;
/// see [`decode_container`].
const BLOCK_REGISTRY_HINT: u32 = 1 << 20;

/// The same, for biomes.
const BIOME_REGISTRY_HINT: u32 = 1 << 12;

/// How many of a section's cells are not air.
///
/// The client uses it to skip empty sections cheaply. It is recomputed on every
/// send rather than cached beside the section, because a cached count is a
/// second statement of what the blocks already say, and the two would drift the
/// first time anything set a block without going through the cache.
///
/// Air is registry id 0 in every version this server speaks, and that is a
/// claim about `dust-registry`'s table rather than about the protocol — see the
/// test that pins it.
fn non_air_count(states: &PalettedContainer) -> u32 {
    // Whole-container shortcuts first: a section that is entirely one value is
    // the common case above and below the terrain, and walking 4096 cells to
    // discover that is the difference between sending a chunk and sending a
    // chunk slowly.
    if states.palette().kind() == PaletteKind::Single {
        return if states.palette().value(0) == Some(AIR) {
            0
        } else {
            states.len() as u32
        };
    }
    (0..states.len())
        .filter(|cell| states.get(*cell) != AIR)
        .count() as u32
}

/// The block state id of air.
///
/// Zero in every version this server speaks. It is named rather than written
/// inline so the day it is not, there is one line to change and a test that
/// fails first.
pub const AIR: u32 = 0;

fn read_short<R: WireRead + ?Sized>(input: &mut R) -> Result<i16, DecodeError> {
    let bytes = input.read_slice(2)?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn decode_container<R: WireRead + ?Sized>(
    input: &mut R,
    strategy: Strategy,
    registry_hint: u32,
) -> Result<PalettedContainer, DecodeError> {
    let bits = input.read_slice(1)?[0] as u32;

    // Which tier the sender used is decided by the byte it sent, not by what
    // this side would have chosen for the same contents. A sender is allowed to
    // be less efficient than necessary — vanilla itself sends four bits for a
    // two-state section — and second-guessing it here would misread a legal
    // chunk.
    let indirect =
        bits == 0 || strategy.palette_for(bits, registry_hint).kind() != PaletteKind::Global;

    let entries: Vec<u32> = if bits == 0 {
        vec![read_var_u32(input, "single palette value")?]
    } else if indirect {
        let count = read_var_usize(input, "palette length")?;
        if count > strategy.len() {
            return Err(DecodeError::NegativeLength {
                field: "palette length",
                value: count as i32,
            });
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(read_var_u32(input, "palette entry")?);
        }
        entries
    } else {
        Vec::new()
    };

    let longs = read_long_array(input, strategy.len(), bits)?;

    if bits == 0 {
        return PalettedContainer::from_parts(strategy, registry_hint, &entries, None)
            .map_err(|e| container_error("single-valued section", e));
    }
    if indirect {
        return PalettedContainer::from_parts(strategy, registry_hint, &entries, Some(longs))
            .map_err(|e| container_error("paletted section", e));
    }

    // Direct: the array holds registry ids. Rebuilt through the same
    // constructor by handing it the identity palette the wire implies, so
    // there is one path that produces a container and not two.
    let storage =
        crate::bits::BitStorage::from_longs(bits, strategy.len(), longs).map_err(|_| {
            DecodeError::Nbt {
                why: "a direct section's long array is the wrong length for its width",
            }
        })?;
    let mut container = PalettedContainer::filled(strategy, registry_hint, 0);
    for cell in 0..strategy.len() {
        container.set(cell, storage.get(cell));
    }
    Ok(container)
}

fn encode_container<W: WireWrite + ?Sized>(container: &PalettedContainer, out: &mut W) {
    let palette = container.palette();
    let storage = container.storage();
    match palette.kind() {
        PaletteKind::Single => {
            out.write_slice(&[0]);
            out.write_var_int(palette.value(0).unwrap_or(0) as i32);
            // The empty array is written as a zero count rather than omitted.
            // Vanilla writes it; a reader that expected it and did not get it
            // would take the next section's non-air count as this section's
            // long count.
            out.write_var_int(0);
        }
        PaletteKind::Linear | PaletteKind::Hashed => {
            out.write_slice(&[storage.bits() as u8]);
            let entries = palette.entries().unwrap_or(&[]);
            out.write_var_int(entries.len() as i32);
            for entry in entries {
                out.write_var_int(*entry as i32);
            }
            write_longs(storage.as_longs(), out);
        }
        PaletteKind::Global => {
            // No palette at all: the array holds registry ids, and the width
            // is how the far end knows how many bits each one occupies.
            out.write_slice(&[storage.bits() as u8]);
            write_longs(storage.as_longs(), out);
        }
    }
}

fn write_longs<W: WireWrite + ?Sized>(longs: &[i64], out: &mut W) {
    out.write_var_int(longs.len() as i32);
    for long in longs {
        out.write_slice(&long.to_be_bytes());
    }
}

fn read_var_u32<R: WireRead + ?Sized>(
    input: &mut R,
    field: &'static str,
) -> Result<u32, DecodeError> {
    let raw = input.read_var_int()?;
    u32::try_from(raw).map_err(|_| DecodeError::NegativeLength { field, value: raw })
}

fn read_var_usize<R: WireRead + ?Sized>(
    input: &mut R,
    field: &'static str,
) -> Result<usize, DecodeError> {
    Ok(read_var_u32(input, field)? as usize)
}

/// Read the long array, refusing a count that does not match the width.
///
/// The count is on the wire and the correct count is computable, so the two are
/// compared rather than the wire's being trusted. A sender that is wrong about
/// this is a sender whose chunk cannot be read at all, and saying so beats
/// allocating what it asked for.
fn read_long_array<R: WireRead + ?Sized>(
    input: &mut R,
    cells: usize,
    bits: u32,
) -> Result<Vec<i64>, DecodeError> {
    let count = read_var_usize(input, "section long count")?;
    let expected = if bits == 0 {
        0
    } else {
        crate::bits::long_count(cells, bits)
    };
    if count != expected {
        return Err(DecodeError::Nbt {
            why: "a section's long count does not match its bits per entry",
        });
    }
    let mut longs = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = input.read_slice(8)?;
        longs.push(i64::from_be_bytes(
            bytes.try_into().expect("read_slice(8) yields eight bytes"),
        ));
    }
    Ok(longs)
}

fn container_error(_what: &'static str, _e: crate::container::ContainerError) -> DecodeError {
    DecodeError::Nbt {
        why: "a section's palette is not one this container can hold",
    }
}

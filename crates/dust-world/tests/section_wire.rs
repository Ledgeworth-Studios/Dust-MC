//! The section codec, against bytes a real server sent.
//!
//! # Why a captured fixture and not a round trip
//!
//! A round trip through this crate's own encoder and decoder passes under any
//! self-consistent reading of the format. The paletted container is the worst
//! case for that: its leading byte decides whether what follows is a value, a
//! palette or neither, and a pair of halves that agreed on the wrong split
//! would round-trip perfectly and render every block in the chunk as something
//! else. So the assertions here are against bytes this crate did not produce.
//!
//! Two sections, chosen for the two shapes the format actually has: an indirect
//! one with a seven-entry palette, and a single-valued all-air one. Between
//! them they cover every branch of the encoder except the direct tier, which is
//! covered by construction below because no vanilla overworld section reaches
//! it.

/// Bytes a real 1.21.1 server sent; see the file's own header.
mod fixtures {
    include!("fixtures/sections.rs");
}

use dust_protocol::packets::play::chunk::Section as WireSection;
use dust_protocol::version::V1_21_1;
use dust_protocol::wire::{Reader, WireRead as _, Writer};
use dust_world::chunk::Section;
use dust_world::container::{PalettedContainer, Strategy};
use dust_world::light::LightArray;
use dust_world::palette::PaletteKind;

/// Decode a captured section, re-encode it, and require the same bytes back.
///
/// Byte-for-byte, not "equivalent": the format has choices in it — which tier,
/// how wide, what order the palette is in — and re-encoding to a *different*
/// legal spelling would mean this server's chunks differ from vanilla's for
/// reasons no diff would explain. Where Dust deviates it should be because
/// somebody decided to.
fn round_trips_the_captured_bytes(captured: &[u8]) -> Section {
    let mut reader = Reader::new(captured);
    let section = Section::decode_wire(&mut reader, V1_21_1).expect("the capture decodes");
    assert_eq!(reader.remaining(), 0, "the section is the whole fixture");

    let mut out = Writer::default();
    section.encode_wire(&mut out, V1_21_1).expect("re-encodes");
    let produced = out.into_bytes();
    assert_eq!(
        produced.len(),
        captured.len(),
        "re-encoding changed the length"
    );
    assert_eq!(produced, captured, "re-encoding changed the bytes");
    section
}

#[test]
fn a_captured_terrain_section_survives_a_decode_and_re_encode() {
    let section = round_trips_the_captured_bytes(fixtures::SOLID);

    // What the capture says about this section, read off the decode above:
    // four bits per entry, a seven-entry palette, and a full 4096 non-air
    // blocks — it is the bottom section of an overworld chunk, all stone and
    // bedrock and deepslate.
    assert_eq!(section.states().storage().bits(), 4);
    assert_eq!(section.states().palette().kind(), PaletteKind::Linear);
    assert_eq!(
        section.states().palette().entries().map(<[u32]>::len),
        Some(7)
    );

    // And the biomes are a single value, which is the other container's
    // single-valued branch travelling in the same fixture.
    assert_eq!(section.biomes().palette().kind(), PaletteKind::Single);
}

#[test]
fn a_captured_all_air_section_is_eight_bytes_and_stays_eight_bytes() {
    // The whole section above the terrain: a zero non-air count, a
    // single-valued block palette holding air, an empty long array written as
    // a zero count, then the same for biomes. Eight bytes, and the empty
    // arrays are two of them — omitting either would make the next section's
    // count read as this one's long count.
    assert_eq!(fixtures::AIR.len(), 8);
    let section = round_trips_the_captured_bytes(fixtures::AIR);
    assert_eq!(section.states().palette().kind(), PaletteKind::Single);
    assert_eq!(section.states().palette().value(0), Some(0), "air");
}

#[test]
fn the_non_air_count_is_recomputed_rather_than_carried() {
    // Decode the all-air section, put one block in it, and the count on the
    // wire must follow. A count cached at decode time would still say zero,
    // and a client told a section is empty does not render it.
    let mut reader = Reader::new(fixtures::AIR);
    let mut section = Section::decode_wire(&mut reader, V1_21_1).expect("decodes");
    section.states_mut().set_at(0, 0, 0, 1);

    let mut out = Writer::default();
    section.encode_wire(&mut out, V1_21_1).expect("encodes");
    let bytes = out.into_bytes();
    assert_eq!(
        i16::from_be_bytes([bytes[0], bytes[1]]),
        1,
        "one non-air block must be counted"
    );
}

#[test]
fn a_direct_tier_section_carries_its_ids_and_no_palette() {
    // No vanilla overworld section reaches the direct tier, so this one is
    // built rather than captured — but the branch exists on the wire and a
    // modded or heavily varied section takes it, so it is exercised here
    // instead of being the one path nothing runs.
    //
    // The disk form of this container would write indices into a palette; the
    // wire form writes registry ids and sends no palette. Sending the disk
    // form would decode cleanly and render the wrong blocks everywhere, which
    // is why the two forms are separate code.
    let mut states = PalettedContainer::filled(Strategy::BLOCK_STATES, 1 << 20, 0);
    for cell in 0..states.len() {
        states.set(cell, cell as u32 + 1);
    }
    assert_eq!(
        states.palette().kind(),
        PaletteKind::Global,
        "4096 distinct states must leave the hashed tier"
    );

    let section = Section::new(
        states,
        PalettedContainer::filled(Strategy::BIOMES, 1 << 12, 0),
        LightArray::filled(0),
        LightArray::filled(0),
    );

    let mut out = Writer::default();
    section.encode_wire(&mut out, V1_21_1).expect("encodes");
    let bytes = out.into_bytes();

    let mut reader = Reader::new(&bytes);
    let back = Section::decode_wire(&mut reader, V1_21_1).expect("decodes");
    assert_eq!(reader.remaining(), 0);
    for cell in 0..back.states().len() {
        assert_eq!(
            back.states().get(cell),
            cell as u32 + 1,
            "cell {cell} came back as a different id"
        );
    }
}

#[test]
fn a_long_count_that_disagrees_with_the_width_is_refused() {
    // The count is on the wire and the right count is computable, so a
    // mismatch is a sender this side cannot read — and allocating what it
    // asked for first would be the mistake.
    let mut broken = fixtures::AIR.to_vec();
    // The block palette's empty long array: bump its count without supplying
    // the longs.
    broken[4] = 0x7f;
    let mut reader = Reader::new(&broken);
    Section::decode_wire(&mut reader, V1_21_1)
        .expect_err("a lying long count must be refused, not trusted");
}

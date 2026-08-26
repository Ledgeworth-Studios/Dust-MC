//! Behaviour tests for the pieces of Play that are more than a field list.
//!
//! The corpus in [`common`] proves every Play packet round-trips. This file
//! proves the *policies*: which metadata serializers are refused and why, how
//! a chunk blob is walked and when that walk fails, which nibble of a
//! multi-block entry is which coordinate, what an acknowledged message with id
//! zero promises, and the handful of flag semantics game code will lean on.
//! Each test names the behaviour it pins, so a red line reads as a sentence.

mod common;

use common::{corpus, v};
use dust_protocol::packets::play::chat::{
    AcknowledgedMessage, ChatFilter, MessageAcknowledgement, SignatureBytes,
};
use dust_protocol::packets::play::chunk::{
    ChunkData, LightArray, LightData, Section, CHUNK_DATA_MAX_BYTES, LIGHT_SECTION_BYTES,
};
use dust_protocol::packets::play::metadata::{MetadataEntries, MetadataEntry, MetadataValue, Pose};
use dust_protocol::packets::play::player_info::{
    PlayerInfoActions, PlayerInfoBody, PlayerInfoEntry,
};
use dust_protocol::packets::play::serverbound as sb;
use dust_protocol::packets::play::{
    Abilities, BlockChangeEntry, ChunkSectionPosition, EntityVelocity, GameModeByte, Gamemode,
    PreviousGameMode, TeleportFlags,
};
use dust_protocol::types::{BitSet, Decode, Encode, FixedBitSet, PrefixedBytes, VarInt};
use dust_protocol::wire::{DecodeError, EncodeError, Reader, WireRead, WireWrite, Writer};

// ---------------------------------------------------------------------------
// Packed coordinates: the sign-extension traps, again, at different widths
// ---------------------------------------------------------------------------

#[test]
fn a_chunk_section_position_sign_extends_its_three_fields() {
    // 22/20/22 bits this time, negative y included, because sections below y=0
    // are where everyone digs first.
    for (x, y, z) in [
        (-4, 3, 12),
        (-1, -5, -1),
        (262_143, -524_288, -262_144),
        (0, 0, 0),
    ] {
        let packed = ChunkSectionPosition::pack(x, y, z);
        assert_eq!(packed.x(), x, "{packed:?}");
        assert_eq!(packed.y(), y, "{packed:?}");
        assert_eq!(packed.z(), z, "{packed:?}");
    }

    // And it travels as a plain long.
    let mut writer = Writer::new();
    ChunkSectionPosition::pack(-2, -1, 3)
        .encode(&mut writer, v())
        .expect("encodes");
    let back =
        ChunkSectionPosition::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!((back.x(), back.y(), back.z()), (-2, -1, 3));
}

#[test]
fn a_block_change_entry_knows_which_nibble_is_which() {
    // The published example order: state above x above z above **y**. The
    // known-answer rows were computed from the format description by hand;
    // they are here because a swap of two nibbles still round-trips.
    let entry = BlockChangeEntry::pack(4095, 15, 14, 13);
    assert_eq!(entry.state_id(), 4095);
    assert_eq!(entry.local_x(), 15);
    assert_eq!(entry.local_y(), 14);
    assert_eq!(entry.local_z(), 13);

    let mut writer = Writer::new();
    entry.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        BlockChangeEntry::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        entry
    );
}

// ---------------------------------------------------------------------------
// Entity metadata: the closed enum over an open set
// ---------------------------------------------------------------------------

fn one_entry(index: u8, serializer: i32, payload: &[u8]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.write_u8(index);
    writer.write_var_int(serializer);
    writer.write_slice(payload);
    writer.write_u8(0xFF);
    writer.into_bytes()
}

#[test]
fn metadata_entries_run_until_the_terminator_and_stop_there() {
    // Two entries then the terminator, followed by another field's bytes. A
    // reader that kept going past 0xFF would eat them.
    let mut bytes = one_entry(2, 0, &[7]);
    bytes.push(0xAA);
    let mut reader = Reader::new(&bytes);
    let entries = MetadataEntries::decode(&mut reader, v()).expect("decodes");
    assert_eq!(
        entries.0,
        vec![MetadataEntry {
            index: 2,
            value: MetadataValue::Byte(7),
        }]
    );
    assert_eq!(reader.read_u8(), Ok(0xAA));

    // No entries at all is just the terminator, and encodes as exactly it.
    let empty = MetadataEntries(vec![]);
    let mut writer = Writer::new();
    empty.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.into_bytes(), vec![0xFF]);
}

#[test]
fn metadata_offsets_distinguish_absent_from_zero() {
    // Optional entity id: zero means absent; a real entity zero would be a 1
    // on the wire. Same shape for optional block state.
    let value = MetadataValue::OptionalEntityId(None);
    let mut writer = Writer::new();
    value.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[20, 0], "serializer 20, absent");

    let present = MetadataValue::OptionalEntityId(Some(VarInt(0)));
    let mut writer = Writer::new();
    present.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[20, 1], "entity zero is wire 1");
    let back = MetadataValue::read(20, &mut Reader::new(&[1]), v()).expect("decodes");
    assert_eq!(back, present);

    let block_state = MetadataValue::OptionalBlockState(Some(VarInt(9)));
    let mut writer = Writer::new();
    block_state.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[15, 10]);
    let back = MetadataValue::read(15, &mut Reader::new(&[10]), v()).expect("decodes");
    assert_eq!(back, block_state);
}

#[test]
fn unknown_metadata_serializers_are_refused_by_id_and_never_guessed_at() {
    // The open-set policy in one line each: a modelled-but-unimplementable
    // serializer names its reason; an unmodelled id says why no guess is
    // possible. Neither may panic, default, or skip.
    for (serializer, payload) in [
        (7i32, &[][..]), // item stack: the Slot seam
        (17, &[0][..]),  // particle: options have no length
        (18, &[][..]),   // particles
        (23, &[0][..]),  // wolf variant, inline form
        (26, &[0][..]),  // painting variant, inline form
        (31, &[0][..]),  // not modelled at all on this version
        (-3, &[][..]),   // hostile id
    ] {
        let bytes = one_entry(0, serializer, payload);
        match MetadataEntries::decode(&mut Reader::new(&bytes), v()) {
            Err(DecodeError::Unsupported { field, .. }) => {
                assert_eq!(field, "entity metadata", "{serializer}")
            }
            other => panic!("serializer {serializer} was accepted: {other:?}"),
        }
    }

    // The registry-id family shares one variant precisely because resolving
    // ids belongs to dust-registry; cat variant (22) decodes raw.
    let bytes = one_entry(4, 22, &[5]);
    let entries = MetadataEntries::decode(&mut Reader::new(&bytes), v()).expect("decodes");
    assert_eq!(entries.0[0].value, MetadataValue::RegistryId(VarInt(5)));
}

#[test]
fn pose_refuses_a_discriminant_this_version_does_not_define() {
    let bytes = [99];
    assert_eq!(
        Pose::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "Pose",
            value: 99
        })
    );
}

// ---------------------------------------------------------------------------
// Chunks: the envelope is ours, the contents are the world crate's
// ---------------------------------------------------------------------------

/// A stand-in for dust-world's future section type: consumes a fixed number of
/// bytes per section, enough to prove the walk without implementing palettes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeSection(u32);

impl Section for FakeSection {
    fn decode_wire<R: dust_protocol::wire::WireRead + ?Sized>(
        input: &mut R,
        _version: dust_protocol::ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let bytes = input.read_slice(4)?;
        Ok(Self(u32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }

    fn encode_wire<W: dust_protocol::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: dust_protocol::ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0.to_be_bytes());
        Ok(())
    }
}

#[test]
fn chunk_sections_are_walked_bottom_up_and_the_rest_of_the_column_is_air() {
    // Two four-byte sections, big-endian as the fake decoder reads them.
    let data = ChunkData(PrefixedBytes(vec![0, 0, 0, 1, 0, 0, 0, 2]));
    let sections = data
        .parse_sections::<FakeSection>(5, v())
        .expect("two sections fit a five-tall column");
    assert_eq!(
        sections,
        vec![Some(FakeSection(1)), Some(FakeSection(2)), None, None, None]
    );
}

#[test]
fn a_section_that_overreads_fails_the_walk_rather_than_shifting_the_rest() {
    // The blob holds three bytes; the section decoder wants four. The failure
    // must surface here, at this section, instead of leaving the next section
    // (or the trailing check) to discover it.
    let data = ChunkData(PrefixedBytes(vec![1, 0, 0]));
    assert!(data.parse_sections::<FakeSection>(1, v()).is_err());
}

#[test]
fn more_sections_than_the_column_is_tall_are_an_error() {
    // One section of data, a column declared one tall, and a whole second
    // section's bytes still sitting there: the walk refuses rather than
    // guessing whether they were padding.
    let data = ChunkData(PrefixedBytes(vec![
        0, 0, 0, 1, // section zero
        9, 9, 9, 9, // nobody claimed this
    ]));
    assert!(matches!(
        data.parse_sections::<FakeSection>(1, v()),
        Err(DecodeError::Nbt { .. })
    ));
}

#[test]
fn a_hostile_chunk_length_is_bounded_before_allocation() {
    // Four mebibytes is the bound; a prefix claiming more is refused by name
    // before any allocation, which is the same discipline strings follow.
    let mut writer = Writer::new();
    writer.write_var_int(CHUNK_DATA_MAX_BYTES as i32 + 1);
    writer.write_slice(b"tiny");
    assert!(matches!(
        ChunkData::decode(&mut Reader::new(writer.as_bytes()), v()),
        Err(DecodeError::StringTooLong { .. })
    ));
}

#[test]
fn light_arrays_carry_exactly_two_kilobytes_each() {
    let light = LightData {
        sky_mask: bitset(&[1]),
        block_mask: bitset(&[]),
        empty_sky_mask: bitset(&[]),
        empty_block_mask: bitset(&[]),
        sky_arrays: vec![LightArray(vec![0; LIGHT_SECTION_BYTES])],
        block_arrays: vec![],
    };
    let mut writer = Writer::new();
    light.encode(&mut writer, v()).expect("encodes");
    let back = LightData::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!(back.sky_arrays.len(), 1);

    // The length is a constant of the format, so an array claiming any other
    // size is refused at the byte, not read as a shorter truth.
    let mut hostile = Writer::new();
    hostile.write_var_int(100);
    assert!(matches!(
        LightArray::decode(&mut Reader::new(hostile.as_bytes()), v()),
        Err(DecodeError::StringTooLong {
            limit: 2048,
            actual: 100
        })
    ));
}

fn bitset(bits: &[usize]) -> BitSet {
    let mut set = BitSet::default();
    for &bit in bits {
        set.set(bit, true);
    }
    set
}

// ---------------------------------------------------------------------------
// Chat fields: the signing seam's layout rules
// ---------------------------------------------------------------------------

#[test]
fn an_acknowledged_message_with_id_zero_carries_its_signature_inline() {
    let signature: SignatureBytes = core::array::from_fn(|i| i as u8);
    let referenced = AcknowledgedMessage {
        id: VarInt(5),
        signature: None,
    };
    let inline = AcknowledgedMessage {
        id: VarInt(0),
        signature: Some(signature),
    };

    for message in [&referenced, &inline] {
        let mut writer = Writer::new();
        message.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        assert_eq!(
            AcknowledgedMessage::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
            *message
        );
    }

    // The two states the type can hold but the wire cannot spell: both are
    // refusals, because writing either would desynchronise the peer.
    let mut writer = Writer::new();
    let lies = [
        AcknowledgedMessage {
            id: VarInt(0),
            signature: None,
        },
        AcknowledgedMessage {
            id: VarInt(2),
            signature: Some(signature),
        },
    ];
    for lie in &lies {
        assert!(matches!(
            lie.encode(&mut writer, v()),
            Err(EncodeError::Unsupported { .. })
        ));
    }
}

#[test]
fn the_chat_filter_mask_exists_only_when_filtering_was_partial() {
    for (filter, extra_bytes) in [
        (ChatFilter::PassThrough, 0),
        (ChatFilter::FullyFiltered, 0),
        (ChatFilter::PartiallyFiltered(bitset(&[3])), 1 + 8),
    ] {
        let mut writer = Writer::new();
        filter.encode(&mut writer, v()).expect("encodes");
        assert_eq!(writer.len(), 1 + extra_bytes, "{filter:?}");
        let back = ChatFilter::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, filter);
    }
}

#[test]
fn the_acknowledgement_bit_set_is_twenty_bits_with_no_prefix() {
    let ack = MessageAcknowledgement {
        offset: VarInt(9),
        acknowledged: FixedBitSet::<20>::new(),
    };
    let mut writer = Writer::new();
    ack.encode(&mut writer, v()).expect("encodes");
    // One byte of VarInt offset plus ceil(20/8) = 3 fixed bytes.
    assert_eq!(writer.len(), 4);
}

// ---------------------------------------------------------------------------
// Small semantics game code will lean on
// ---------------------------------------------------------------------------

#[test]
fn teleport_flags_ask_whether_an_axis_is_relative() {
    let flags = TeleportFlags(TeleportFlags::X | TeleportFlags::YAW);
    assert!(flags.is_relative(TeleportFlags::X));
    assert!(flags.is_relative(TeleportFlags::YAW));
    assert!(!flags.is_relative(TeleportFlags::PITCH));
    assert!(!flags.is_relative(TeleportFlags::Y));

    // All clear means everything absolute — the ordinary hard teleport.
    assert!(!TeleportFlags(0).is_relative(0xFF));
}

#[test]
fn flying_without_permission_is_a_player_who_cannot_land() {
    assert!(Abilities(Abilities::FLYING).can_stop_flying().eq(&false));
    assert!(Abilities(Abilities::FLYING | Abilities::ALLOW_FLYING).can_stop_flying());
    assert!(Abilities(0).can_stop_flying());
}

#[test]
fn the_previous_gamemode_sentinel_survives_a_round_trip() {
    for mode in [
        PreviousGameMode(None),
        PreviousGameMode(Some(Gamemode::Spectator)),
    ] {
        let mut writer = Writer::new();
        mode.encode(&mut writer, v()).expect("encodes");
        assert_eq!(
            PreviousGameMode::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
            mode
        );
    }
    // None is the -1 byte, which is the whole point of the wrapper.
    let mut writer = Writer::new();
    PreviousGameMode(None)
        .encode(&mut writer, v())
        .expect("encodes");
    assert_eq!(writer.as_bytes(), &[0xFF]);

    // And an out-of-range byte is refused by naming the variant table.
    assert_eq!(
        GameModeByte::decode(&mut Reader::new(&[9]), v()),
        Err(DecodeError::UnknownVariant {
            name: "Gamemode",
            value: 9
        })
    );
}

#[test]
fn velocity_and_delta_units_are_distinct_types_that_do_not_convert() {
    // Nothing here can accidentally assign one to the other; the test only
    // pins that both survive the wire intact, since their layouts coincide.
    let velocity = EntityVelocity {
        x: i16::MIN,
        y: 0,
        z: i16::MAX,
    };
    let mut writer = Writer::new();
    velocity.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 6);
    assert_eq!(
        EntityVelocity::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        velocity
    );
}

// ---------------------------------------------------------------------------
// Player info update: the bitmask is law
// ---------------------------------------------------------------------------

#[test]
fn player_info_actions_outside_the_version_are_refused() {
    // An action bit a peer invented would leave every entry's length unknown.
    // Refusing the packet is the only safe answer, and it names the byte. The
    // bytes here are hand-written because the encoder refuses to produce them.
    let bytes = [0xFF, 0x00];
    assert_eq!(
        PlayerInfoBody::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "PlayerInfoActions",
            value: 255
        })
    );
}

#[test]
fn each_action_selects_its_field_per_entry_without_prefixes() {
    // Update latency only: uuid, then the varint, nothing else. If presence
    // ever grew a boolean prefix, this byte count is where it shows.
    let body = PlayerInfoBody {
        actions: PlayerInfoActions(PlayerInfoActions::UPDATE_LATENCY),
        entries: vec![PlayerInfoEntry {
            uuid: dust_protocol::types::Uuid(1),
            latency: Some(VarInt(300)),
            ..PlayerInfoEntry::default()
        }],
    };
    let mut writer = Writer::new();
    body.encode(&mut writer, v()).expect("encodes");
    // actions(1) + count(1) + uuid(16) + varint(2)
    assert_eq!(writer.len(), 20);
    let back = PlayerInfoBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!(back, body);

    // An enabled action with no data behind it is an encoding refusal, not a
    // silent default row on someone's tab list.
    let hollow = PlayerInfoBody {
        actions: PlayerInfoActions(PlayerInfoActions::UPDATE_GAME_MODE),
        entries: vec![PlayerInfoEntry::default()],
    };
    assert!(matches!(
        hollow.encode(&mut Writer::new(), v()),
        Err(EncodeError::Unsupported { .. })
    ));

    // And decoding enforces the mirror image: the entry must consume exactly
    // its selected fields, so a truncated body errors instead of inventing
    // values.
    let mut bytes = writer.into_bytes();
    bytes.pop();
    assert!(PlayerInfoBody::decode(&mut Reader::new(&bytes), v()).is_err());
}

// ---------------------------------------------------------------------------
// The corpus itself
// ---------------------------------------------------------------------------

#[test]
fn every_play_frame_still_decodes_through_its_group_dispatch() {
    // Cheap insurance that the shared corpus stays valid as definitions move:
    // if a layout changes under a stale sample, this is the first line to go
    // red, pointing at the packet by name.
    for frame in corpus() {
        if frame.state != dust_protocol::ConnectionState::Play {
            continue;
        }
        assert!(
            (frame.decodes)(&frame.bytes).is_ok(),
            "{} no longer decodes",
            frame.name
        );
    }
}

#[test]
fn play_definitions_claim_the_only_version_there_is() {
    // D3's dimension in miniature: nothing in a definition may assume the
    // version list will always be length one, so the claim is explicit and
    // this merely checks nobody typo'd the version string.
    use dust_protocol::packets::PacketBody;
    assert_eq!(
        sb::TeleportConfirm::protocol_id(v()),
        Some(
            v().protocol_id(
                dust_protocol::ConnectionState::Play,
                dust_protocol::Direction::Serverbound,
                "minecraft:accept_teleportation"
            )
            .expect("in table")
        )
    );
}

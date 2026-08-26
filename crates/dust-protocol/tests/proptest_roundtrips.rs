//! Property tests: generated values, boundary lengths, and the round trip
//! that must survive all of them.
//!
//! The corpus tests prove particular values travel intact. Properties prove
//! the *shape* is total: every representable string at its exact limit, every
//! bit pattern a float can hold, coordinates at the edges of their fields.
//! When one of these goes red it names the shrunk input, which is the other
//! reason they exist — a minimal failing value turns "the chunk packet
//! sometimes fails" into an afternoon off.

use dust_protocol::packets::play::chat::{
    AcknowledgedMessage, ChatFilter, MessageAcknowledgement,
};
use dust_protocol::packets::play::chunk::{ChunkData, LightArray, LIGHT_SECTION_BYTES};
use dust_protocol::packets::play::metadata::{
    MetadataEntries, MetadataEntry, MetadataValue, Pose,
};
use dust_protocol::packets::play::serverbound as sb;
use dust_protocol::packets::play::{
    Abilities, BlockChangeEntry, ChunkSectionPosition, EntityDelta, EntityVelocity, Gamemode,
    GameModeByte, PreviousGameMode, TeleportFlags,
};
use dust_protocol::text::{Body, Color, Component, NamedColor, Style};
use dust_protocol::types::{
    Angle, BitSet, BoundedString, Decode, Encode, FixedBitSet, Identifier, Position,
    PrefixedBytes, RestOfPacket, Uuid, VarInt, VarLong,
};
use dust_protocol::version;
use dust_protocol::wire::{DecodeError, Reader, WireWrite, Writer};

use proptest::prelude::*;

use dust_protocol::packets::play::chunk::CHUNK_DATA_MAX_BYTES;

fn v() -> dust_protocol::ProtocolVersion {
    version::V1_21_1
}

/// Encode, decode back, demand equality. The one assertion every property
/// below leans on, lifted out so its failure message names the type.
fn assert_round_trip<T>(value: &T)
where
    T: Encode + Decode + PartialEq + std::fmt::Debug,
{
    let mut writer = Writer::new();
    value.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();
    assert_eq!(
        &T::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        value,
        "{value:?} changed on the way round"
    );
}

/// A metadata value cannot travel alone — its serializer id is part of its
/// encoding — so value-level properties ride a one-entry list.
fn round_trip_value(value: MetadataValue) {
    let expected = value.clone();
    let mut writer = Writer::new();
    MetadataEntries(vec![MetadataEntry { index: 0, value }])
        .encode(&mut writer, v())
        .expect("encodes");
    let bytes = writer.into_bytes();
    let back = MetadataEntries::decode(&mut Reader::new(&bytes), v()).expect("decodes");
    assert_eq!(back.0.len(), 1);
    assert_eq!(back.0[0].value, expected);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_048))]

    #[test]
    fn var_ints_round_trip_across_the_whole_range(value in any::<i32>()) {
        assert_round_trip(&VarInt(value));
        // The five extremes are where encoders break; finding them by search
        // alone would take long enough to matter.
        for edge in [i32::MIN, -1, 0, 1, i32::MAX] {
            assert_round_trip(&VarInt(edge));
        }
    }

    #[test]
    fn var_longs_round_trip_across_the_whole_range(value in any::<i64>()) {
        assert_round_trip(&VarLong(value));
        for edge in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_round_trip(&VarLong(edge));
        }
    }

    #[test]
    fn fixed_width_integers_keep_every_bit(value in any::<(i8, u8, i16, u16, i32, i64)>()) {
        assert_round_trip(&value.0);
        assert_round_trip(&value.1);
        assert_round_trip(&value.2);
        assert_round_trip(&value.3);
        assert_round_trip(&value.4);
        assert_round_trip(&value.5);
    }

    #[test]
    fn floats_round_trip_bit_for_bit_including_nans(bits in any::<[u8; 4]>()) {
        let value = f32::from_be_bytes(bits);
        let mut writer = Writer::new();
        value.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        let back = f32::decode(&mut Reader::new(&bytes), v()).expect("decodes");
        prop_assert_eq!(back.to_bits(), value.to_bits(), "a float changed bits");
    }

    #[test]
    fn positions_survive_wherever_they_are_representable(
        x in -(1i32 << 25)..(1 << 25),
        y in -(1i32 << 11)..(1 << 11),
        z in -(1i32 << 25)..(1 << 25),
    ) {
        let position = Position::new(x, y, z);
        prop_assert!(position.is_representable());
        assert_round_trip(&position);
    }

    #[test]
    fn identifiers_round_trip_when_their_characters_are_legal(
        namespace in "[a-z0-9_.-]{1,16}",
        path in "[a-z0-9_./-]{1,48}",
    ) {
        let identifier = Identifier { namespace, path };
        assert_round_trip(&identifier);
    }

    #[test]
    fn uuids_and_angles_are_exact(uuid in any::<u128>(), angle in any::<u8>()) {
        assert_round_trip(&Uuid(uuid));
        assert_round_trip(&Angle(angle));
        prop_assert_eq!(Angle::from_degrees(Angle(angle).to_degrees()), Angle(angle));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_024))]

    #[test]
    fn strings_at_their_utf16_limit_round_trip(
        ascii_len in 0..=100usize,
        emoji_units in 0..=8usize,
    ) {
        // Two ways to sit exactly at a limit: plain ASCII, and text padded
        // with two-unit characters — where byte length and UTF-16 length
        // diverge furthest, and where a byte-counted limit would lie.
        let ascii: String = "a".repeat(ascii_len);
        let bounded = BoundedString::<128>::new(&ascii).expect("fits");
        assert_round_trip(&bounded);

        let mut padded = "e".repeat(ascii_len.min(90));
        for _ in 0..emoji_units {
            padded.push('\u{1F600}');
        }
        let units = dust_protocol::types::utf16_len(&padded);
        if units <= 128 {
            let bounded = BoundedString::<128>::new(&padded).expect("fits");
            assert_round_trip(&bounded);
            prop_assert_eq!(dust_protocol::types::utf16_len(bounded.as_str()), units);
        } else {
            prop_assert!(BoundedString::<128>::new(&padded).is_err());
        }
    }

    #[test]
    fn a_string_over_its_limit_is_refused_in_units_not_bytes(
        units in 1usize..=64,
    ) {
        let text = "x".repeat(units);
        let mut writer = Writer::new();
        writer.write_var_int(text.len() as i32);
        writer.write_slice(text.as_bytes());
        let bytes = writer.into_bytes();

        // Exactly at its own length it fits...
        assert!(dust_protocol::types::read_string(&mut Reader::new(&bytes), units).is_ok());
        // ...and one unit under, it does not — including the unit-0 limit,
        // which must refuse a one-character string rather than an empty one.
        let refused = dust_protocol::types::read_string(&mut Reader::new(&bytes), units - 1);
        assert!(matches!(refused, Err(DecodeError::StringTooLong { .. })));
    }

    #[test]
    fn bit_sets_round_trip_through_their_words(words in any::<Vec<u64>>()) {
        assert_round_trip(&BitSet(words.clone()));
        let set = BitSet(words);
        for index in 0..set.0.len().min(4) * 64 + 7 {
            let word = set.0.get(index / 64).copied().unwrap_or(0);
            prop_assert_eq!(set.get(index), word >> (index % 64) & 1 == 1);
        }
    }

    #[test]
    fn prefixed_byte_arrays_survive_any_content(
        content in proptest::collection::vec(any::<u8>(), 0..300),
    ) {
        assert_round_trip(&PrefixedBytes::<300>(content));
    }

    #[test]
    fn rest_of_packet_is_exact(content in proptest::collection::vec(any::<u8>(), 0..120)) {
        assert_round_trip(&RestOfPacket(content));
    }

    #[test]
    fn packed_section_coordinates_sign_extend(
        x in -(1i32 << 21)..(1 << 21),
        y in -(1i32 << 19)..(1 << 19),
        z in -(1i32 << 21)..(1 << 21),
    ) {
        let packed = ChunkSectionPosition::pack(x, y, z);
        prop_assert_eq!(packed.x(), x);
        prop_assert_eq!(packed.y(), y);
        prop_assert_eq!(packed.z(), z);
    }

    #[test]
    fn block_change_entries_keep_state_and_nibbles_apart(
        state in 0u32..(1 << 12),
        lx in 0u8..16,
        ly in 0u8..16,
        lz in 0u8..16,
    ) {
        let entry = BlockChangeEntry::pack(state, lx, ly, lz);
        prop_assert_eq!(entry.state_id(), state);
        prop_assert_eq!(entry.local_x(), lx);
        prop_assert_eq!(entry.local_y(), ly);
        prop_assert_eq!(entry.local_z(), lz);
        assert_round_trip(&entry);
    }

    #[test]
    fn velocities_and_deltas_carry_every_short(
        x in any::<i16>(),
        y in any::<i16>(),
        z in any::<i16>(),
    ) {
        assert_round_trip(&EntityVelocity { x, y, z });
        assert_round_trip(&EntityDelta { x, y, z });
    }

    #[test]
    fn gamemode_wrappers_accept_every_representable_mode(
        mode in 0i32..4,
        previous in -1i32..4,
    ) {
        let mode = Gamemode::from_discriminant(mode).expect("modelled");
        assert_round_trip(&GameModeByte(mode));

        let previous = PreviousGameMode(Gamemode::from_discriminant(previous));
        assert_round_trip(&previous);
    }

    #[test]
    fn ability_flags_are_a_byte_and_mean_what_they_say(flags in any::<u8>()) {
        let abilities = Abilities(flags);
        assert_round_trip(&abilities);
        prop_assert_eq!(
            abilities.has(Abilities::FLYING),
            flags & Abilities::FLYING != 0
        );
        prop_assert_eq!(
            abilities.can_stop_flying(),
            !abilities.has(Abilities::FLYING) || abilities.has(Abilities::ALLOW_FLYING)
        );
    }

    #[test]
    fn teleport_flag_bits_mean_what_they_say(flags in any::<u8>(), probe in 1u8..=0x10) {
        let flags = TeleportFlags(flags);
        prop_assert_eq!(flags.is_relative(probe), flags.0 & probe != 0);
    }

    #[test]
    fn acknowledged_messages_match_their_wire_promise(
        id in any::<i32>(),
        with_signature in any::<bool>(),
    ) {
        let candidate: Option<[u8; 256]> =
            with_signature.then(|| core::array::from_fn(|i| (i % 251) as u8));
        let message = AcknowledgedMessage {
            id: VarInt(id),
            signature: if id == 0 { candidate } else { None },
        };
        assert_round_trip(&message);
    }

    #[test]
    fn chat_filters_survive_every_shape(mask_words in any::<Vec<u64>>(), kind in 0u8..3) {
        let filter = match kind {
            0 => ChatFilter::PassThrough,
            1 => ChatFilter::FullyFiltered,
            _ => ChatFilter::PartiallyFiltered(BitSet(mask_words)),
        };
        assert_round_trip(&filter);
    }

    #[test]
    fn acknowledgements_keep_their_twenty_bits(
        offset in any::<i32>(),
        words in proptest::collection::vec(any::<u8>(), 3),
    ) {
        let mut acknowledged = FixedBitSet::<20>::new();
        acknowledged.0.copy_from_slice(&words);
        let ack = MessageAcknowledgement {
            offset: VarInt(offset),
            acknowledged,
        };
        assert_round_trip(&ack);
    }

    #[test]
    fn metadata_values_survive_every_modelled_serializer(
        byte in any::<i8>(),
        int in any::<i32>(),
        long in any::<i64>(),
        float in any::<f32>(),
        xyz in (any::<f32>(), any::<f32>(), any::<f32>()),
        pose in 0i32..15,
    ) {
        round_trip_value(MetadataValue::Byte(byte));
        round_trip_value(MetadataValue::VarInt(VarInt(int)));
        round_trip_value(MetadataValue::VarLong(VarLong(long)));
        round_trip_value(MetadataValue::Float(float));
        round_trip_value(MetadataValue::Boolean(float.to_bits() & 1 == 1));

        let pose = Pose::from_discriminant(pose).expect("modelled");
        round_trip_value(MetadataValue::Pose(pose));

        // Rotations and vectors are three floats that must not rotate into
        // each other's places; distinct values make a swap visible.
        let (pitch, yaw, roll) = xyz;
        round_trip_value(MetadataValue::Rotations(pitch, yaw, roll));
        round_trip_value(MetadataValue::Vector(yaw, roll, pitch));
    }

    #[test]
    fn metadata_lists_survive_mixed_entries(count in 0usize..12) {
        let entries: Vec<MetadataEntry> = (0..count)
            .map(|index| MetadataEntry {
                index: (index * 17 % 250) as u8,
                value: match index % 3 {
                    0 => MetadataValue::Byte(index as i8),
                    1 => MetadataValue::VarInt(VarInt(index as i32 * 7919)),
                    _ => MetadataValue::OptionalBlockState(Some(VarInt(index as i32))),
                },
            })
            .collect();
        assert_round_trip(&MetadataEntries(entries));
    }

    #[test]
    fn light_arrays_insist_on_two_kilobytes(len in 0usize..2_100usize) {
        let array = LightArray(vec![0xAA; len]);
        let mut writer = Writer::new();
        array.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        let decoded = LightArray::decode(&mut Reader::new(&bytes), v());
        if len == LIGHT_SECTION_BYTES {
            decoded.expect("the exact length decodes");
        } else {
            assert!(decoded.is_err(), "{len} bytes was accepted");
        }
    }

    #[test]
    fn chunk_blobs_pass_through_up_to_a_megabyte(size in 0usize..(1 << 20)) {
        let data = ChunkData(PrefixedBytes::<CHUNK_DATA_MAX_BYTES>(vec![0x5A; size]));
        assert_round_trip(&data);
    }

    #[test]
    fn movement_packets_survive_hostile_floats(
        coords in (any::<f64>(), any::<f64>(), any::<f64>()),
        angles in (any::<f32>(), any::<f32>()),
        on_ground in any::<bool>(),
    ) {
        let (x, y, z) = coords;
        let (yaw, pitch) = angles;
        assert_round_trip(&sb::MovePlayerPos { x, y, z, on_ground });
        assert_round_trip(&sb::MovePlayerPosRot { x, y, z, yaw, pitch, on_ground });
        assert_round_trip(&sb::MovePlayerRot { yaw, pitch, on_ground });
        assert_round_trip(&sb::MovePlayerStatusOnly { on_ground });
    }

    #[test]
    fn text_components_survive_generated_content(
        parts in proptest::collection::vec("[a-z \u{1F600}\u{00E9}]{0,24}", 0..6),
        bold in any::<Option<bool>>(),
        italic in any::<Option<bool>>(),
        color_index in 0u16..18,
    ) {
        const PALETTE: [NamedColor; 16] = [
            NamedColor::Black,
            NamedColor::DarkBlue,
            NamedColor::DarkGreen,
            NamedColor::DarkAqua,
            NamedColor::DarkRed,
            NamedColor::DarkPurple,
            NamedColor::Gold,
            NamedColor::Gray,
            NamedColor::DarkGray,
            NamedColor::Blue,
            NamedColor::Green,
            NamedColor::Aqua,
            NamedColor::Red,
            NamedColor::LightPurple,
            NamedColor::Yellow,
            NamedColor::White,
        ];
        let color = match color_index {
            0..=15 => Some(Color::Named(PALETTE[color_index as usize])),
            16 => None,
            _ => Some(Color::Rgb(color_index as u32)),
        };
        let build = |text: &str| Component {
            body: Body::Text(text.to_owned()),
            style: Style { color, bold, italic },
            extra: vec![],
        };

        // A single node exercises every shortcut the encoder may take...
        assert_round_trip(&build(&parts.join("")));

        // ...and an extra chain forces the list encoding, whose element kind
        // flips between strings and compounds depending on styling.
        let mut root = build("");
        root.extra = parts.iter().map(|text| build(text)).collect();
        assert_round_trip(&root);
    }
}

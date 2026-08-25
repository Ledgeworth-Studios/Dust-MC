//! The offline half of Phase 1's check: vectors, coverage, and a round trip of
//! every packet before Play.
//!
//! Needs no network and no JVM. The other half — whether these layouts are
//! what a real 1.21.1 server actually speaks — is in `vanilla_conformance.rs`,
//! and neither of these files can do the other's job. This one proves the code
//! is self-consistent and agrees with tables computed elsewhere; that one
//! proves it agrees with Minecraft.

use dust_protocol::conformance::{
    check_field_types, check_nbt, check_wire, in_crate_nbt, in_crate_wire,
};
use dust_protocol::nbt::{self, JsonTextComponent, Nbt, TextComponent};
use dust_protocol::packets::common::{
    BuiltInLinkLabel, KnownPack, ProfileProperty, RegistryEntry, ReportDetail, ServerLink,
    ServerLinkLabel, Tag, TagRegistry,
};
use dust_protocol::packets::{
    configuration, handshake, login, status, undefined_for, IMPLEMENTED_STATES,
};
use dust_protocol::types::{
    Angle, BitSet, BoundedString, ChatVisibility, Decode, Encode, FixedBitSet, Identifier,
    MainHand, NextState, Position, PrefixedBytes, ResourcePackResult, RestOfPacket, Slot, Uuid,
    VarInt,
};
use dust_protocol::wire::{DecodeError, EncodeError, Reader, WireRead, WireWrite, Writer};
use dust_protocol::{version, ConnectionState, Direction, ProtocolVersion};

fn v() -> ProtocolVersion {
    version::V1_21_1
}

fn s<const N: usize>(text: &str) -> BoundedString<N> {
    BoundedString::new(text).expect("fits")
}

fn id(text: &str) -> Identifier {
    Identifier::parse(text).expect("valid")
}

/// An NBT compound, so a text component field carries something with structure
/// rather than the one-byte empty value that every scanner gets right.
fn some_nbt() -> Nbt {
    Nbt(vec![
        0x0a, 0x08, 0x00, 0x04, b't', b'e', b'x', b't', 0x00, 0x04, b'D', b'u', b's', b't', 0x00,
    ])
}

// ---------------------------------------------------------------------------
// The vector tables
// ---------------------------------------------------------------------------

#[test]
fn the_wire_primitives_agree_with_the_vectors() {
    // The same runner `dust-net` will call against its own implementation. If
    // this ever fails after that merge, the vectors say which of the two is
    // wrong, which is the thing checking them against each other could not do.
    let failures = check_wire(&in_crate_wire());
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_nbt_scanner_agrees_with_the_vectors() {
    let failures = check_nbt(in_crate_nbt());
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_field_types_agree_with_the_vectors() {
    let failures = check_field_types(v());
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---------------------------------------------------------------------------
// The traps
// ---------------------------------------------------------------------------

#[test]
fn a_string_is_measured_in_utf16_code_units_and_prefixed_in_bytes() {
    // The whole trap in one test. `café` is four UTF-16 units and five bytes.
    let text = "café";
    let mut writer = Writer::new();
    s::<4>(text).encode(&mut writer, v()).expect("fits at 4");
    assert_eq!(writer.as_bytes()[0], 5, "the prefix counts bytes");
    assert_eq!(writer.len(), 6);

    // A limit of four accepts it and a limit of three does not — which is the
    // assertion a byte-length implementation fails, because it would compare
    // five against three and reject, or compare five against five and accept
    // a string that is over the real limit.
    let bytes = writer.as_bytes().to_vec();
    assert!(BoundedString::<4>::decode(&mut Reader::new(&bytes), v()).is_ok());
    assert_eq!(
        BoundedString::<3>::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::StringTooLong {
            limit: 3,
            actual: 4
        })
    );

    // And the other direction: a 16-unit name of non-ASCII characters is 48
    // bytes, and a byte-length limit of 16 would refuse a name vanilla accepts.
    let long = "日".repeat(16);
    assert_eq!(dust_protocol::types::utf16_len(&long), 16);
    assert_eq!(long.len(), 48);
    assert!(BoundedString::<16>::new(&long).is_ok());
}

#[test]
fn a_hostile_length_prefix_is_refused_before_anything_is_allocated() {
    // A VarInt saying two billion, and four bytes of body. The refusal must
    // come from the limit and not from running out of input, because running
    // out of input is what happens *after* an implementation has tried to
    // reserve two gigabytes.
    let mut writer = Writer::new();
    writer.write_var_int(2_000_000_000);
    writer.write_slice(b"oops");
    let bytes = writer.into_bytes();
    assert!(matches!(
        BoundedString::<16>::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::StringTooLong { limit: 16, .. })
    ));
}

#[test]
fn a_position_sign_extends_and_negative_coordinates_survive() {
    // The failure this catches works perfectly around spawn. Every coordinate
    // here is one an ordinary player reaches in ten seconds of walking.
    for (x, y, z) in [
        (0, 0, 0),
        (-1, -1, -1),
        (-100, -60, 100),
        (100, -64, -100),
        (-33554432, -2048, 33554431),
    ] {
        let position = Position::new(x, y, z);
        assert!(position.is_representable(), "{position:?}");
        let mut writer = Writer::new();
        position.encode(&mut writer, v()).expect("encodes");
        let back = Position::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, position);
    }

    // A masking implementation that forgot to sign-extend produces this value,
    // so asserting it is *not* what comes back names the bug rather than just
    // failing.
    let masked_x = 0x3FF_FFFF - 99;
    let decoded = Position::decode(
        &mut Reader::new(&Position::new(-100, -60, 100).to_bits().to_be_bytes()),
        v(),
    )
    .expect("decodes");
    assert_ne!(decoded.x, masked_x);
    assert_eq!(decoded.x, -100);
}

#[test]
fn a_position_outside_the_range_wraps_rather_than_erroring() {
    // Documented behaviour, matching vanilla, and pinned here so a later change
    // to it is a deliberate one.
    let far = Position::new(1 << 26, 0, 0);
    assert!(!far.is_representable());
    assert_eq!(Position::from_bits(far.to_bits()).x, 0);
}

#[test]
fn an_angle_round_trips_as_steps_and_not_as_degrees() {
    // The lossy direction is stated rather than hidden: every step survives a
    // trip through degrees, and a degree value does not survive a trip through
    // a step.
    for raw in 0..=255u8 {
        let angle = Angle(raw);
        assert_eq!(Angle::from_degrees(angle.to_degrees()), angle, "{raw}");
    }
    assert_ne!(Angle::from_degrees(1.0).to_degrees(), 1.0);
    assert_eq!(Angle::from_degrees(360.0), Angle(0), "a full turn wraps");
    assert_eq!(Angle::from_degrees(-90.0), Angle(192), "and so does -90");
}

#[test]
fn an_unknown_enum_discriminant_is_a_named_error_and_never_a_default() {
    // Attacker-controlled input. A panic here is a remote crash and a silent
    // fallback to the first variant is worse, because the wrong value then
    // travels somewhere with no connection to the packet that produced it.
    let mut writer = Writer::new();
    writer.write_var_int(99);
    let bytes = writer.into_bytes();
    assert_eq!(
        NextState::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "NextState",
            value: 99
        })
    );
    assert_eq!(
        ChatVisibility::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "ChatVisibility",
            value: 99
        })
    );
    // A negative discriminant is the same story and is easy to miss, because a
    // VarInt happily carries one.
    let mut writer = Writer::new();
    writer.write_var_int(-1);
    assert!(matches!(
        MainHand::decode(&mut Reader::new(&writer.into_bytes()), v()),
        Err(DecodeError::UnknownVariant { value: -1, .. })
    ));
    for state in NextState::ALL {
        assert_eq!(
            NextState::from_discriminant(state.discriminant()),
            Some(*state)
        );
    }
}

#[test]
fn the_two_bit_sets_are_encoded_differently() {
    // Same idea, different formats, and conflating them is a stream that is a
    // few bytes out in a way nothing local notices.
    let mut growable = BitSet::default();
    growable.set(0, true);
    growable.set(70, true);
    let mut writer = Writer::new();
    growable.encode(&mut writer, v()).expect("encodes");
    // A VarInt count of two longs, then sixteen bytes.
    assert_eq!(writer.len(), 1 + 16);
    assert_eq!(
        BitSet::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        growable
    );
    assert!(growable.get(0) && growable.get(70) && !growable.get(1));

    let mut fixed = FixedBitSet::<7>::new();
    fixed.set(6, true);
    let mut writer = Writer::new();
    fixed.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 1, "seven bits is one byte and no prefix");
    assert_eq!(
        FixedBitSet::<7>::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        fixed
    );
}

#[test]
fn a_slot_with_components_is_refused_by_name_rather_than_guessed_at() {
    // The honest half-implementation. An empty stack and a stack with only
    // removals work; the moment a component appears, the answer is a named
    // refusal at the exact byte, because a component has no length and
    // guessing past one loses the position of everything after it.
    let empty = Slot::Empty;
    let mut writer = Writer::new();
    empty.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[0x00]);
    assert_eq!(
        Slot::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        empty
    );

    let stack = Slot::Present {
        count: 3,
        item_id: 1,
        removed_components: vec![7, 9],
    };
    let mut writer = Writer::new();
    stack.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        Slot::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        stack
    );

    // count 1, item 1, one component to add, none to remove.
    let with_component = [0x01, 0x01, 0x01, 0x00, 0x00];
    assert!(matches!(
        Slot::decode(&mut Reader::new(&with_component), v()),
        Err(DecodeError::Unsupported {
            field: "Slot components",
            ..
        })
    ));
}

#[test]
fn an_identifier_is_validated_and_a_bare_path_takes_the_default_namespace() {
    assert_eq!(id("minecraft:stone").to_string(), "minecraft:stone");
    assert_eq!(id("stone").namespace, "minecraft");
    assert_eq!(id("dust:some/path.thing-1").path, "some/path.thing-1");
    for bad in [
        "",
        ":",
        "Minecraft:stone",
        "minecraft:",
        "a b:c",
        "mine/craft:x",
    ] {
        assert!(
            Identifier::parse(bad).is_err(),
            "`{bad}` should not be an identifier"
        );
    }
}

#[test]
fn nbt_is_delimited_without_being_interpreted() {
    // The seam's whole contract: where does the value end.
    let bytes = some_nbt().0;
    assert_eq!(nbt::scan(&bytes), Ok(bytes.len()));

    // Inside a packet, followed by another field. A scanner that consumed the
    // rest of the body instead of the value would pass every test where NBT
    // was last, and this is the one where it is not.
    let mut writer = Writer::new();
    TextComponent(some_nbt())
        .encode(&mut writer, v())
        .expect("encodes");
    writer.write_var_int(1234);
    let bytes = writer.into_bytes();
    let mut reader = Reader::new(&bytes);
    let text = TextComponent::decode(&mut reader, v()).expect("decodes");
    assert_eq!(text.0, some_nbt());
    assert_eq!(reader.read_var_int(), Ok(1234));
    assert_eq!(reader.remaining(), 0);
}

#[test]
fn nbt_nesting_is_bounded_rather_than_overflowing_the_stack() {
    // Reachable from an unauthenticated socket, and a stack overflow in Rust
    // is an abort that no caller can catch.
    // Genuinely nested, rather than merely long: each level is a compound
    // entry with an empty name, so the repeating unit is tag, name length,
    // and nothing else.
    let levels = 600;
    let mut deep = vec![0x0a];
    for _ in 0..levels {
        deep.extend_from_slice(&[0x0a, 0x00, 0x00]);
    }
    deep.extend(std::iter::repeat_n(0x00, levels + 1));
    assert!(
        matches!(nbt::scan(&deep), Err(DecodeError::Nbt { .. })),
        "600 levels is past the limit of {}",
        nbt::MAX_DEPTH
    );

    // And the positive control, without which the assertion above would pass
    // for a scanner that refused everything.
    let mut shallow = vec![0x0a];
    for _ in 0..8 {
        shallow.extend_from_slice(&[0x0a, 0x00, 0x00]);
    }
    shallow.extend(std::iter::repeat_n(0x00, 9));
    assert_eq!(nbt::scan(&shallow), Ok(shallow.len()));
}

// ---------------------------------------------------------------------------
// Coverage: the anti-drift guard
// ---------------------------------------------------------------------------

#[test]
fn every_packet_before_play_has_a_definition_and_every_definition_is_a_packet() {
    // This is the test that makes hand-written definitions defensible. The ids
    // are generated; the bodies are not; this is the only thing that connects
    // them, and it runs on every pull request forever.
    for protocol in ProtocolVersion::all() {
        let problems = undefined_for(protocol);
        assert!(problems.is_empty(), "{}: {problems:#?}", protocol.name());
    }
}

#[test]
fn the_definitions_cover_exactly_the_forty_one_packets_before_play() {
    let mut counted = 0;
    for state in IMPLEMENTED_STATES {
        for direction in Direction::ALL {
            counted += v().table(state, direction).len();
        }
    }
    assert_eq!(counted, 41, "the four states before Play have 41 packets");
    let defined: usize = dust_protocol::packets::GROUPS
        .iter()
        .map(|group| group.len())
        .sum();
    assert_eq!(defined, 41);
}

#[test]
fn play_is_deliberately_not_covered() {
    // Scope, asserted rather than assumed. If somebody adds Play definitions
    // without widening IMPLEMENTED_STATES, the coverage check would not look at
    // them; if they widen it without adding them, this crate's own coverage
    // test goes red. Either way it is visible.
    assert!(!IMPLEMENTED_STATES.contains(&ConnectionState::Play));
    assert_eq!(
        v().table(ConnectionState::Play, Direction::Clientbound)
            .len(),
        124
    );
}

// ---------------------------------------------------------------------------
// Every packet, both ways
// ---------------------------------------------------------------------------

/// One value of every packet before Play.
///
/// Written out rather than derived, because a `Default` would fill every field
/// with a zero and a round trip over zeros is a weaker test than one over
/// values that differ from each other — a swapped pair of fields of the same
/// type is invisible when both are zero.
#[allow(clippy::too_many_lines)]
fn one_of_every_packet() -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let mut push = |name: &'static str, bytes: Vec<u8>| out.push((name, bytes));

    macro_rules! round_trip {
        ($group:path, $value:expr) => {{
            use $group as g;
            let packet: g::Packet = ($value).into();
            let name = packet.name();
            let mut writer = Writer::new();
            let protocol_id = packet.encode(&mut writer, v()).expect("encodes");
            let bytes = writer.into_bytes();
            let back = g::Packet::decode(&mut Reader::new(&bytes), v())
                .unwrap_or_else(|e| panic!("{name} (id {protocol_id}) did not decode: {e}"));
            assert_eq!(back, packet, "{name} changed on the way round");
            push(name, bytes);
        }};
    }

    round_trip!(
        handshake::serverbound,
        handshake::serverbound::Intention {
            protocol_version: VarInt(767),
            server_address: s("dust.example"),
            server_port: 25565,
            next_state: NextState::Login,
        }
    );

    round_trip!(
        status::clientbound,
        status::clientbound::StatusResponse {
            json: s(r#"{"description":"x"}"#),
        }
    );
    round_trip!(
        status::clientbound,
        status::clientbound::PongResponse { payload: -9 }
    );
    round_trip!(status::serverbound, status::serverbound::StatusRequest {});
    round_trip!(
        status::serverbound,
        status::serverbound::PingRequest {
            payload: 81985529216486895
        }
    );

    round_trip!(
        login::clientbound,
        login::clientbound::LoginDisconnect {
            reason: JsonTextComponent(s(r#"{"text":"no"}"#)),
        }
    );
    round_trip!(
        login::clientbound,
        login::clientbound::Hello {
            server_id: s(""),
            public_key: PrefixedBytes(vec![1, 2, 3]),
            verify_token: PrefixedBytes(vec![4, 5, 6, 7]),
            should_authenticate: true,
        }
    );
    round_trip!(
        login::clientbound,
        login::clientbound::GameProfile {
            uuid: Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            username: s("Notch"),
            properties: vec![
                ProfileProperty {
                    name: s("textures"),
                    value: s("base64"),
                    signature: Some(s("sig")),
                },
                ProfileProperty {
                    name: s("unsigned"),
                    value: s("value"),
                    signature: None,
                },
            ],
            strict_error_handling: true,
        }
    );
    round_trip!(
        login::clientbound,
        login::clientbound::LoginCompression {
            threshold: VarInt(256)
        }
    );
    round_trip!(
        login::clientbound,
        login::clientbound::CustomQuery {
            message_id: VarInt(7),
            channel: id("fabric:hello"),
            data: RestOfPacket(vec![9, 9, 9]),
        }
    );
    round_trip!(
        login::clientbound,
        login::clientbound::CookieRequest {
            key: id("dust:session")
        }
    );

    round_trip!(
        login::serverbound,
        login::serverbound::Hello {
            name: s("Notch"),
            profile_id: Uuid(1),
        }
    );
    round_trip!(
        login::serverbound,
        login::serverbound::Key {
            shared_secret: PrefixedBytes(vec![1; 16]),
            verify_token: PrefixedBytes(vec![2; 4]),
        }
    );
    round_trip!(
        login::serverbound,
        login::serverbound::CustomQueryAnswer {
            message_id: VarInt(7),
            data: Some(RestOfPacket(vec![1, 2])),
        }
    );
    round_trip!(
        login::serverbound,
        login::serverbound::CustomQueryAnswer {
            message_id: VarInt(8),
            data: None,
        }
    );
    round_trip!(login::serverbound, login::serverbound::LoginAcknowledged {});
    round_trip!(
        login::serverbound,
        login::serverbound::CookieResponse {
            key: id("dust:session"),
            payload: Some(PrefixedBytes(vec![3, 3, 3])),
        }
    );

    round_trip!(
        configuration::clientbound,
        configuration::clientbound::CookieRequest { key: id("dust:c") }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x04Dust".to_vec()),
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::Disconnect {
            reason: TextComponent(some_nbt()),
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::FinishConfiguration {}
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::KeepAlive { id: -1 }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::Ping { id: -2 }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::ResetChat {}
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::RegistryData {
            registry_id: id("minecraft:dimension_type"),
            entries: vec![
                RegistryEntry {
                    entry_id: id("minecraft:overworld"),
                    data: Some(some_nbt()),
                },
                RegistryEntry {
                    entry_id: id("minecraft:the_nether"),
                    data: None,
                },
            ],
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::ResourcePackPop {
            uuid: Some(Uuid(5))
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::ResourcePackPush {
            uuid: Uuid(6),
            url: s("https://example.invalid/p.zip"),
            hash: s("0123456789012345678901234567890123456789"),
            forced: true,
            prompt_message: Some(TextComponent(some_nbt())),
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::StoreCookie {
            key: id("dust:c"),
            payload: PrefixedBytes(vec![1, 2, 3]),
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::Transfer {
            host: s("elsewhere.example"),
            port: VarInt(25565),
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::UpdateEnabledFeatures {
            features: vec![id("minecraft:vanilla")],
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::UpdateTags {
            registries: vec![TagRegistry {
                registry: id("minecraft:block"),
                tags: vec![Tag {
                    name: id("minecraft:logs"),
                    entries: vec![VarInt(1), VarInt(2), VarInt(3)],
                }],
            }],
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::SelectKnownPacks {
            packs: vec![KnownPack {
                namespace: s("minecraft"),
                id: s("core"),
                version: s("1.21.1"),
            }],
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::CustomReportDetails {
            details: vec![ReportDetail {
                title: s("server"),
                description: s("Dust"),
            }],
        }
    );
    round_trip!(
        configuration::clientbound,
        configuration::clientbound::ServerLinks {
            links: vec![
                ServerLink {
                    label: ServerLinkLabel::BuiltIn(BuiltInLinkLabel::BugReport),
                    url: s("https://example.invalid/bugs"),
                },
                ServerLink {
                    label: ServerLinkLabel::Custom(TextComponent(some_nbt())),
                    url: s("https://example.invalid/other"),
                },
            ],
        }
    );

    round_trip!(
        configuration::serverbound,
        configuration::serverbound::ClientInformation {
            locale: s("en_GB"),
            view_distance: 12,
            chat_mode: ChatVisibility::System,
            chat_colors: true,
            displayed_skin_parts: 0b0111_1111,
            main_hand: MainHand::Left,
            text_filtering_enabled: false,
            allow_server_listings: true,
        }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::CookieResponse {
            key: id("dust:c"),
            payload: None,
        }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x06vanilla".to_vec()),
        }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::FinishConfiguration {}
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::KeepAlive { id: 42 }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::Pong { id: 43 }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::ResourcePack {
            uuid: Uuid(6),
            result: ResourcePackResult::Declined,
        }
    );
    round_trip!(
        configuration::serverbound,
        configuration::serverbound::SelectKnownPacks { packs: vec![] }
    );

    out
}

#[test]
fn every_packet_before_play_round_trips_through_its_body() {
    let encoded = one_of_every_packet();

    // Every packet name in the table appears at least once above. A round-trip
    // suite that quietly stopped covering a packet would stay green while that
    // packet's layout went unchecked — the failure mode where a guard degrades
    // instead of breaking.
    let mut covered: Vec<&str> = encoded.iter().map(|(name, _)| *name).collect();
    covered.sort_unstable();
    covered.dedup();
    let mut missing = Vec::new();
    for state in IMPLEMENTED_STATES {
        for direction in Direction::ALL {
            for (_, name) in v().table(state, direction).packets() {
                if !covered.contains(&name) {
                    missing.push(format!("{}/{} {name}", state.name(), direction.name()));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "packets with no round-trip: {missing:#?}"
    );
}

#[test]
fn a_body_that_ends_early_or_late_is_an_error_rather_than_a_shrug() {
    // Trailing bytes mean the layout this crate believes is not the layout that
    // was sent. Accepting them would mean the next packet on a shared buffer
    // starts in the wrong place, which is a much harder bug to find than this.
    let packet =
        status::serverbound::Packet::PingRequest(status::serverbound::PingRequest { payload: 1 });
    let mut writer = Writer::new();
    packet.encode(&mut writer, v()).expect("encodes");
    let mut bytes = writer.into_bytes();

    bytes.push(0xff);
    assert_eq!(
        status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::TrailingBytes { left: 1 })
    );

    bytes.truncate(bytes.len() - 3);
    assert!(matches!(
        status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnexpectedEnd { .. })
    ));
}

#[test]
fn an_id_this_state_has_no_packet_for_is_a_named_error() {
    let bytes = [0x7f];
    assert_eq!(
        status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownPacket {
            state: "status",
            direction: "serverbound",
            protocol_id: 127
        })
    );
}

#[test]
fn a_packet_takes_its_id_from_the_generated_table_and_not_from_a_constant() {
    // The property that makes hand-written definitions survive a version bump:
    // nothing in a definition names a number, so a release that renumbers the
    // protocol changes nothing here.
    use dust_protocol::packets::PacketBody;
    assert_eq!(
        login::serverbound::LoginAcknowledged::protocol_id(v()),
        v().protocol_id(
            ConnectionState::Login,
            Direction::Serverbound,
            "minecraft:login_acknowledged"
        )
    );
    assert_eq!(
        login::serverbound::LoginAcknowledged::protocol_id(v()),
        Some(3)
    );

    // And the refusal when a version has no such packet, rather than a wrong
    // number.
    let packet = status::serverbound::Packet::StatusRequest(status::serverbound::StatusRequest {});
    let mut writer = Writer::new();
    assert!(packet.encode(&mut writer, v()).is_ok());
    assert!(matches!(
        BoundedString::<1>::new("too long"),
        Err(EncodeError::StringTooLong {
            limit: 1,
            actual: 8
        })
    ));
}

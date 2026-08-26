//! Helpers shared by the protocol test binaries.
//!
//! # Why the corpus is built once, here
//!
//! Every offline guarantee this crate makes is a statement about *every*
//! packet: coverage, round trips, hostile-input robustness. A statement about
//! everything is worthless if the list of everything lives in four files that
//! drift apart, so the list lives once, below, and each guarantee consumes
//! it. Building a frame also validates it — encode, decode back, insist the
//! value survived — so a corpus entry is a frame that has already passed its
//! round trip, which means the fuzz loop mutates real layouts rather than
//! accidents.
//!
//! Each test binary links this module on its own and uses a different slice
//! of it — the fuzzer wants the corpus, the body tests want the constructors
//! — so a helper unused in one binary is load-bearing in another, which is
//! why the whole module tolerates dead code rather than each item.
#![allow(dead_code)]

use dust_protocol::nbt::{JsonTextComponent, Nbt, TextComponent};
use dust_protocol::packets::common::{
    BuiltInLinkLabel, KnownPack, ProfileProperty, RegistryEntry, ReportDetail, ServerLink,
    ServerLinkLabel, Tag, TagRegistry,
};
use dust_protocol::packets::{configuration, handshake, login, status};
use dust_protocol::types::{
    Angle, BitSet, BoundedString, ChatVisibility, FixedBitSet, Identifier, MainHand, NextState,
    Position, PrefixedBytes, ResourcePackResult, RestOfPacket, Uuid, VarInt,
};
use dust_protocol::version;
use dust_protocol::wire::{DecodeError, Reader, Writer};
use dust_protocol::{ConnectionState, Direction, ProtocolVersion};

/// The one protocol version these tables describe.
pub fn v() -> ProtocolVersion {
    version::V1_21_1
}

/// A bounded string that fits, failing the test if the limit was miscounted.
pub fn s<const N: usize>(text: &str) -> BoundedString<N> {
    BoundedString::new(text).expect("fits")
}

/// An identifier, failing the test if it was malformed.
pub fn id(text: &str) -> Identifier {
    Identifier::parse(text).expect("valid")
}

/// An NBT compound, so component fields carry structure rather than the
/// one-byte empty value every scanner gets right.
pub fn some_nbt() -> Nbt {
    Nbt(vec![
        0x0a, 0x08, 0x00, 0x04, b't', b'e', b't', b'x', 0x00, 0x04, b'D', b'u', b's', b't', 0x00,
    ])
}

/// One encoded packet, plus what it takes to throw hostile input at it again.
///
/// Each test binary links this module independently and uses a different
/// subset of these fields — the fuzzer wants the bytes and the decoder, the
/// coverage checks want the names — so a field unused in one binary is load-
/// bearing in another.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Frame {
    /// The packet's namespaced name, for coverage checks.
    pub name: &'static str,
    pub state: ConnectionState,
    pub direction: Direction,
    /// The full wire form: VarInt id, then body.
    pub bytes: Vec<u8>,
    /// Whether a full-frame decode of `bytes` succeeds. Almost always true;
    /// false marks an intentionally ambiguous sample, of which there are
    /// none today.
    pub decodes: fn(&[u8]) -> Result<(), DecodeError>,
}

macro_rules! frame {
    ($group:path, $state:ident, $direction:ident, $value:expr) => {{
        use $group as g;
        let packet: g::Packet = ($value).into();
        let name = packet.name();
        let mut writer = Writer::new();
        let protocol_id = packet.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        let back = g::Packet::decode(&mut Reader::new(&bytes), v())
            .unwrap_or_else(|e| panic!("{name} (id {protocol_id}) did not decode: {e}"));
        assert_eq!(back, packet, "{name} changed on the way round");
        fn decodes(bytes: &[u8]) -> Result<(), DecodeError> {
            g::Packet::decode(&mut Reader::new(bytes), v()).map(drop)
        }
        Frame {
            name,
            state: ConnectionState::$state,
            direction: Direction::$direction,
            bytes,
            decodes,
        }
    }};
}

/// One value of every packet this crate defines.
///
/// Written out rather than derived, because a `Default` would fill every field
/// with a zero and a round trip over zeros is a weaker test than one over
/// values that differ from each other — a swapped pair of fields of the same
/// type is invisible when both are zero.
#[allow(clippy::too_many_lines)]
pub fn corpus() -> Vec<Frame> {
    let mut out = Vec::new();

    macro_rules! push {
        ($($t:tt)*) => {
            out.push(frame!($($t)*));
        };
    }

    push!(
        handshake::serverbound,
        Handshake,
        Serverbound,
        handshake::serverbound::Intention {
            protocol_version: VarInt(767),
            server_address: s("dust.example"),
            server_port: 25565,
            next_state: NextState::Login,
        }
    );

    push!(
        status::clientbound,
        Status,
        Clientbound,
        status::clientbound::StatusResponse {
            json: s(r#"{"description":"x"}"#),
        }
    );
    push!(
        status::clientbound,
        Status,
        Clientbound,
        status::clientbound::PongResponse { payload: -9 }
    );
    push!(
        status::serverbound,
        Status,
        Serverbound,
        status::serverbound::StatusRequest {}
    );
    push!(
        status::serverbound,
        Status,
        Serverbound,
        status::serverbound::PingRequest {
            payload: 81985529216486895
        }
    );

    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::LoginDisconnect {
            reason: JsonTextComponent(s(r#"{"text":"no"}"#)),
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::Hello {
            server_id: s(""),
            public_key: PrefixedBytes(vec![1, 2, 3]),
            verify_token: PrefixedBytes(vec![4, 5, 6, 7]),
            should_authenticate: true,
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::GameProfile {
            uuid: Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            username: s("Notch"),
            properties: vec![
                ProfileProperty {
                    name: s("textures"),
                    value: s("base64"),
                    signature: Some(s("sig"))
                },
                ProfileProperty {
                    name: s("unsigned"),
                    value: s("value"),
                    signature: None
                },
            ],
            strict_error_handling: true,
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::LoginCompression {
            threshold: VarInt(256)
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::CustomQuery {
            message_id: VarInt(7),
            channel: id("fabric:hello"),
            data: RestOfPacket(vec![9, 9, 9]),
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::CookieRequest {
            key: id("dust:session")
        }
    );

    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::Hello {
            name: s("Notch"),
            profile_id: Uuid(1),
        }
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::Key {
            shared_secret: PrefixedBytes(vec![1; 16]),
            verify_token: PrefixedBytes(vec![2; 4]),
        }
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::CustomQueryAnswer {
            message_id: VarInt(7),
            data: Some(RestOfPacket(vec![1, 2])),
        }
    );
    // The `data: None` half of this packet is exercised by the proptest
    // suite rather than the corpus, which keeps one frame per definition.
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::LoginAcknowledged {}
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::CookieResponse {
            key: id("dust:session"),
            payload: Some(PrefixedBytes(vec![3, 3, 3])),
        }
    );

    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CookieRequest { key: id("dust:c") }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x04Dust".to_vec()),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Disconnect {
            reason: TextComponent(some_nbt()),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::FinishConfiguration {}
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::KeepAlive { id: -1 }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Ping { id: -2 }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResetChat {}
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::RegistryData {
            registry_id: id("minecraft:dimension_type"),
            entries: vec![
                RegistryEntry {
                    entry_id: id("minecraft:overworld"),
                    data: Some(some_nbt())
                },
                RegistryEntry {
                    entry_id: id("minecraft:the_nether"),
                    data: None
                },
            ],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResourcePackPop {
            uuid: Some(Uuid(5))
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResourcePackPush {
            uuid: Uuid(6),
            url: s("https://example.invalid/p.zip"),
            hash: s("0123456789012345678901234567890123456789"),
            forced: true,
            prompt_message: Some(dust_protocol::nbt::TextComponent(some_nbt())),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::StoreCookie {
            key: id("dust:c"),
            payload: PrefixedBytes(vec![1, 2, 3]),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Transfer {
            host: s("elsewhere.example"),
            port: VarInt(25565),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::UpdateEnabledFeatures {
            features: vec![id("minecraft:vanilla")],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::UpdateTags {
            registries: vec![TagRegistry {
                registry: id("minecraft:block"),
                tags: vec![Tag {
                    name: id("minecraft:logs"),
                    entries: vec![VarInt(1), VarInt(2), VarInt(3)]
                }],
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::SelectKnownPacks {
            packs: vec![KnownPack {
                namespace: s("minecraft"),
                id: s("core"),
                version: s("1.21.1")
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CustomReportDetails {
            details: vec![ReportDetail {
                title: s("server"),
                description: s("Dust")
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ServerLinks {
            links: vec![
                ServerLink {
                    label: ServerLinkLabel::BuiltIn(BuiltInLinkLabel::BugReport),
                    url: s("https://example.invalid/bugs"),
                },
                ServerLink {
                    label: ServerLinkLabel::Custom(dust_protocol::nbt::TextComponent(some_nbt())),
                    url: s("https://example.invalid/other"),
                },
            ],
        }
    );

    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
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
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::CookieResponse {
            key: id("dust:c"),
            payload: None,
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x06vanilla".to_vec()),
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::FinishConfiguration {}
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::KeepAlive { id: 42 }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::Pong { id: 43 }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::ResourcePack {
            uuid: Uuid(6),
            result: ResourcePackResult::Declined,
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::SelectKnownPacks { packs: vec![] }
    );

    play_frames(&mut out);
    out
}

#[allow(clippy::too_many_lines)]
fn play_frames(out: &mut Vec<Frame>) {
    use dust_protocol::packets::play::chat::{
        AcknowledgedMessage, ChatFilter, MessageAcknowledgement, SignatureBytes,
    };
    use dust_protocol::packets::play::chunk::{BlockEntity, ChunkData, LightArray, LightData};
    use dust_protocol::packets::play::clientbound as cb;
    use dust_protocol::packets::play::metadata::{MetadataEntries, MetadataValue};
    use dust_protocol::packets::play::player_info::{
        ChatSession, PlayerInfoActions, PlayerInfoBody, PlayerInfoEntry, ProfileAddition,
    };
    use dust_protocol::packets::play::serverbound as sb;
    use dust_protocol::packets::play::{
        Abilities, BlockChangeEntry, ChunkSectionPosition, DeathLocation, EntityDelta,
        EntityVelocity, GameModeByte, PreviousGameMode, TeleportFlags,
    };

    let signature: SignatureBytes = core::array::from_fn(|i| (i * 7 + 3) as u8);

    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::AddEntity {
            entity_id: VarInt(17),
            uuid: Uuid(0x11),
            kind: VarInt(120),
            x: 8.5,
            y: -64.0,
            z: 4096.25,
            pitch: Angle(200),
            yaw: Angle(10),
            head_yaw: Angle(12),
            data: VarInt(-1),
            velocity: EntityVelocity {
                x: 100,
                y: -40,
                z: 8000
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockUpdate {
            location: Position::new(-100, -60, 100),
            block_id: VarInt(4044),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Disconnect {
            reason: dust_protocol::text::Component::text("bye").bold(true),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LevelChunkWithLight {
            chunk_x: -3,
            chunk_z: 19,
            heightmaps: some_nbt(),
            data: ChunkData(PrefixedBytes(vec![0xAA; 64])),
            block_entities: vec![
                BlockEntity {
                    packed_xz: 0x5A,
                    y: -20,
                    kind: VarInt(7),
                    data: some_nbt()
                },
                BlockEntity {
                    packed_xz: 0x01,
                    y: 90,
                    kind: VarInt(2),
                    data: Nbt::empty()
                },
            ],
            light: LightData {
                sky_mask: bitset(&[0, 5, 27]),
                block_mask: bitset(&[5]),
                empty_sky_mask: bitset(&[]),
                empty_block_mask: bitset(&[0, 1, 2, 3]),
                sky_arrays: vec![LightArray(vec![0xFF; 2048]); 3],
                block_arrays: vec![LightArray(vec![0x00; 2048])],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::KeepAlive { id: i64::MIN + 7 }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Login {
            entity_id: 1,
            hardcore: true,
            dimensions: vec![id("minecraft:overworld"), id("dust:pocket")],
            max_players: VarInt(100),
            view_distance: VarInt(12),
            simulation_distance: VarInt(10),
            reduced_debug_info: false,
            respawn_screen: true,
            limited_crafting: false,
            dimension_type: VarInt(0),
            dimension_name: id("minecraft:overworld"),
            hashed_seed: 0x1234_5678_9abc_def0,
            game_mode: GameModeByte(dust_protocol::packets::play::Gamemode::Survival),
            previous_game_mode: PreviousGameMode(None),
            debug: false,
            flat: true,
            death_location: Some(DeathLocation {
                dimension: id("minecraft:the_end"),
                position: Position::new(99, 2047, -99),
            }),
            portal_cooldown: VarInt(40),
            secure_chat: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityPosRot {
            entity_id: VarInt(17),
            delta: EntityDelta {
                x: -300,
                y: 5,
                z: 300
            },
            yaw: Angle(255),
            pitch: Angle(1),
            on_ground: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityPos {
            entity_id: VarInt(18),
            delta: EntityDelta {
                x: 1,
                y: -1,
                z: 4095
            },
            on_ground: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityRot {
            entity_id: VarInt(19),
            yaw: Angle(128),
            pitch: Angle(254),
            on_ground: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerChatMessage {
            sender: Uuid(0x22),
            index: VarInt(4),
            signature: Some(signature),
            message: s::<256>("hello there"),
            timestamp: 1_700_000_000_000,
            salt: -55,
            previous_messages: vec![
                AcknowledgedMessage {
                    id: VarInt(3),
                    signature: None
                },
                AcknowledgedMessage {
                    id: VarInt(0),
                    signature: Some(signature)
                },
            ],
            unsigned_content: None,
            filter: ChatFilter::PartiallyFiltered(bitset(&[2, 9])),
            chat_type: VarInt(1),
            network_name: dust_protocol::text::Component::translate("chat.type.text", None)
                .italic(false),
            network_target_name: Some(dust_protocol::text::Component::text("<notch>")),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerAbilities {
            flags: Abilities(Abilities::FLYING | Abilities::ALLOW_FLYING),
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerPosition {
            x: 1.5,
            y: -59.99,
            z: 2.5,
            yaw: 179.5,
            pitch: -12.25,
            flags: TeleportFlags(TeleportFlags::X | TeleportFlags::Z | TeleportFlags::YAW),
            teleport_id: VarInt(77),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerInfoUpdate {
            body: PlayerInfoBody {
                actions: PlayerInfoActions(
                    PlayerInfoActions::ADD_PLAYER
                        | PlayerInfoActions::INITIALIZE_CHAT
                        | PlayerInfoActions::UPDATE_LISTED
                        | PlayerInfoActions::UPDATE_LATENCY,
                ),
                entries: vec![
                    PlayerInfoEntry {
                        uuid: Uuid(0x33),
                        profile: Some(ProfileAddition {
                            name: s("Notch"),
                            properties: vec![ProfileProperty {
                                name: s("textures"),
                                value: s("base64"),
                                signature: None,
                            }],
                        }),
                        chat_session: Some(None),
                        game_mode: None,
                        listed: Some(true),
                        latency: Some(VarInt(-1)),
                        display_name: None,
                    },
                    PlayerInfoEntry {
                        uuid: Uuid(0x34),
                        profile: Some(ProfileAddition {
                            name: s("Dinnerbone"),
                            properties: vec![]
                        }),
                        chat_session: Some(Some(ChatSession {
                            session_id: Uuid(0x35),
                            expires_at: 1_800_000_000_000,
                            public_key: PrefixedBytes(vec![9; 494]),
                            key_signature: PrefixedBytes(vec![8; 512]),
                        })),
                        game_mode: None,
                        listed: Some(false),
                        latency: Some(VarInt(42)),
                        display_name: None,
                    },
                ],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerInfoRemove {
            uuids: vec![Uuid(0x33), Uuid(0x34)],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x04Dust".to_vec()),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RemoveEntities {
            entity_ids: vec![VarInt(1), VarInt(2), VarInt(3), VarInt(-9)],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RotateHead {
            entity_id: VarInt(17),
            head_yaw: Angle(64),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SectionBlocksUpdate {
            section: ChunkSectionPosition::pack(-4, 3, 12),
            entries: vec![
                BlockChangeEntry::pack(1, 15, 0, 8),
                BlockChangeEntry::pack(9000, 3, 15, 2),
            ],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetCarriedItem { slot: 8 }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetEntityData {
            entity_id: VarInt(17),
            entries: MetadataEntries(vec![
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 0,
                    value: MetadataValue::Byte(0x01),
                },
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 6,
                    value: MetadataValue::VarInt(VarInt(300)),
                },
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 9,
                    value: MetadataValue::TextComponent(
                        dust_protocol::text::Component::text("custom name").colored(
                            dust_protocol::text::Color::Named(
                                dust_protocol::text::NamedColor::Gold
                            ),
                        ),
                    ),
                },
            ]),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SystemChat {
            content: dust_protocol::text::Component::text("welcome").italic(true),
            overlay: false,
        }
    ));
    out.push(frame!(cb, Play, Clientbound, cb::Ping { id: -3 }));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PongResponse { payload: 987654321 }
    ));

    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::TeleportConfirm {
            teleport_id: VarInt(77)
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::Chat {
            message: s::<256>("a message"),
            timestamp: 1_700_000_000_000,
            salt: 12,
            signature: Some(signature),
            acknowledgement: MessageAcknowledgement {
                offset: VarInt(30),
                acknowledged: fixed_bits(),
            },
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::CustomPayload {
            channel: id("dust:hello"),
            data: RestOfPacket(vec![1, 2, 3]),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::KeepAlive { id: i64::MIN + 7 }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerPos {
            x: -30_000_000.0,
            y: -64.0,
            z: 29_999_999.75,
            on_ground: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerPosRot {
            x: 0.125,
            y: 320.0,
            z: 0.25,
            yaw: -359.5,
            pitch: 89.75,
            on_ground: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerRot {
            yaw: 359.5,
            pitch: -89.75,
            on_ground: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerStatusOnly { on_ground: true }
    ));
    out.push(frame!(sb, Play, Serverbound, sb::Pong { id: i32::MIN }));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlayerAbilities {
            flags: Abilities(Abilities::FLYING),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PingRequest { payload: -2 }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SetCarriedItem { slot: 5 }
    ));
}

fn bitset(bits: &[usize]) -> BitSet {
    let mut set = BitSet::default();
    for &bit in bits {
        set.set(bit, true);
    }
    set
}

fn fixed_bits() -> FixedBitSet<20> {
    let mut set = FixedBitSet::<20>::new();
    set.set(0, true);
    set.set(19, true);
    set
}

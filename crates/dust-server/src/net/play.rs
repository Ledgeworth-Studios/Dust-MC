//! Play: the join sequence, and the chunks that make it a world.
//!
//! # The order, and why every packet in it is here
//!
//! ```text
//! S->C  login                  who you are, which world, how far you see
//! S->C  player_position        where you are, with a teleport id
//! S->C  set_chunk_cache_center the column the streaming is centred on
//! S->C  level_chunk_with_light  x (2r+1)^2
//! S->C  game_event 13          "start waiting for level chunks"
//! ```
//!
//! Each one is load-bearing and the omissions are visible:
//!
//! * Without **player_position** the client never leaves the loading screen —
//!   it is waiting to be told where it is, and it will wait forever.
//! * Without **set_chunk_cache_center** the client files arriving chunks
//!   against the wrong centre and unloads them again.
//! * Without **game_event 13** the terrain arrives and the loading screen stays
//!   up: that event is what tells the client the world is ready to render.
//!
//! The position is sent *before* the chunks rather than after. Vanilla does it
//! this way too, and the reason is worth keeping: the client uses its position
//! to decide which chunks it wants, and one that is told about chunks before it
//! knows where it is discards them.
//!
//! # Light
//!
//! Sky light comes from `dust-world`'s propagation engine, computed per column
//! against the terrain's own heightmaps. It is not fifteen everywhere: under
//! the grass it is dark, which is what a cave will need and what a constant
//! could never give.
//!
//! Light crosses a chunk boundary: a column is lit with the sky floors of the
//! four columns around it as sources, so a terrain step at an edge no longer
//! leaves a seam. What it does not yet do is carry light that has to travel
//! *through* a neighbour — around the mouth of a cave three blocks into the
//! next chunk — which under-lights rather than mis-lights. There is still
//! **no block light at all**, because nothing in this world emits any. Both
//! are stated in `dust_world::column_light`, and neither is the kind of gap
//! that renders as a broken packet.

use dust_protocol::nbt::Nbt;
use dust_protocol::packets::play;
use dust_protocol::packets::play::chunk::{
    ChunkData, LightArray, Section as WireSection, LIGHT_SECTION_BYTES,
};
use dust_protocol::packets::play::metadata;
use dust_protocol::packets::play::{GameModeByte, Gamemode, PreviousGameMode, TeleportFlags};
use dust_protocol::types::{BitSet, Identifier, PrefixedBytes, VarInt};
use dust_protocol::wire::Writer;
use dust_protocol::ProtocolVersion;
use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::HeightmapKind;

use super::world::FlatWorld;

/// The game event that ends the loading screen.
///
/// Vanilla's `ClientboundGameEventPacket.LEVEL_CHUNKS_LOAD_START`. Named
/// because `13` beside a `0.0` in a packet call is a magic number nobody
/// reading this file could check.
pub const LEVEL_CHUNKS_LOAD_START: u8 = 13;

/// A light byte holding two nibbles at full: the value for a cell in open sky.
const FULL_BRIGHT: u8 = 0xff;

/// Build the join packet for a player entering `world`.
pub fn login_packet(
    entity_id: i32,
    max_players: u32,
    view_distance: u32,
    dimension_type: u32,
) -> Result<play::clientbound::Login, dust_protocol::wire::DecodeError> {
    Ok(play::clientbound::Login {
        entity_id,
        hardcore: false,
        // The dimensions this player may be sent to. Names, not contents: the
        // contents were synced during configuration, and this packet carries
        // ids into that sync — which is why `dimension_type` below is a number
        // and `dimension_name` is not.
        dimensions: vec![
            Identifier::parse("minecraft:overworld")?,
            Identifier::parse("minecraft:the_nether")?,
            Identifier::parse("minecraft:the_end")?,
        ],
        max_players: VarInt(max_players as i32),
        view_distance: VarInt(view_distance as i32),
        simulation_distance: VarInt(view_distance as i32),
        reduced_debug_info: false,
        respawn_screen: true,
        limited_crafting: false,
        dimension_type: VarInt(dimension_type as i32),
        dimension_name: Identifier::parse("minecraft:overworld")?,
        // The seed the client hashes for its own biome noise. Zero because
        // this world has no seed to hash — a flat world is the same
        // everywhere — and a random number here would make two joins to the
        // same server disagree about nothing.
        hashed_seed: 0,
        game_mode: GameModeByte(Gamemode::Creative),
        // `None`, meaning "no previous mode", which is what a fresh join is.
        // Encoded as -1 rather than as a mode nobody was in.
        previous_game_mode: PreviousGameMode(None),
        debug: false,
        // `flat` is what makes the client render a flat horizon and skip the
        // void fog. It is true here because the world *is* flat, and saying so
        // is the difference between a world that looks deliberate and one that
        // looks broken.
        flat: true,
        death_location: None,
        portal_cooldown: VarInt(0),
        secure_chat: false,
    })
}

/// Where the player is, which way they face, and the teleport to acknowledge.
///
/// `yaw` is a parameter rather than a zero here because a world states the
/// direction its spawn faces (`level.dat`'s `SpawnAngle`) and a client dropped
/// facing south into a world built to be seen the other way sees the back of
/// it. Pitch stays level: no world states one.
pub fn position_packet(
    spawn: (f64, f64, f64),
    yaw: f32,
    teleport_id: i32,
) -> play::clientbound::PlayerPosition {
    play::clientbound::PlayerPosition {
        x: spawn.0,
        y: spawn.1,
        z: spawn.2,
        yaw,
        pitch: 0.0,
        // Zero flags means every field is absolute. The bits mark *relative*
        // axes, so a set bit would have the client add these numbers to where
        // it thinks it already is — which on a join is nowhere in particular.
        flags: TeleportFlags(0),
        teleport_id: VarInt(teleport_id),
    }
}

/// Encode one column into a chunk packet.
/// `pos` is passed rather than read off the chunk, because the chunk may be a
/// template shared by every column in the world — see `world::FlatWorld`. A
/// column's contents and a column's coordinates are separable here and the
/// packet is where they meet.
pub fn chunk_packet(
    chunk: &Chunk,
    pos: ChunkPos,
    version: ProtocolVersion,
) -> Result<play::clientbound::LevelChunkWithLight, dust_protocol::wire::EncodeError> {
    let mut data = Writer::default();
    for section in chunk.sections() {
        section.encode_wire(&mut data, version)?;
    }

    let section_count = chunk.sections().len();
    Ok(play::clientbound::LevelChunkWithLight {
        chunk_x: pos.x,
        chunk_z: pos.z,
        heightmaps: heightmaps_nbt(chunk),
        data: ChunkData(PrefixedBytes(data.into_bytes())),
        // No chests, no signs, no spawners in a flat world.
        block_entities: Vec::new(),
        light: column_light(chunk, section_count),
    })
}

/// The heightmap compound the chunk packet carries.
///
/// Network NBT: a root compound with no name, holding one long array per map,
/// each *named*. Two maps rather than four — see
/// [`FlatWorld::NETWORK_HEIGHTMAPS`].
///
/// Written by hand here rather than through `dust-nbt` because `dust-protocol`
/// carries this field as opaque bytes on purpose, and the compound is four
/// tags long. The day a third caller needs to build NBT, that is the day it
/// moves.
fn heightmaps_nbt(chunk: &Chunk) -> Nbt {
    const TAG_END: u8 = 0;
    const TAG_LONG_ARRAY: u8 = 12;

    let mut out = Vec::new();
    out.push(10); // TAG_Compound, the unnamed root
    for kind in FlatWorld::NETWORK_HEIGHTMAPS {
        let name = network_name(kind);
        out.push(TAG_LONG_ARRAY);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        let longs = chunk.heightmaps().get(kind).as_longs();
        out.extend_from_slice(&(longs.len() as i32).to_be_bytes());
        for long in longs {
            out.extend_from_slice(&long.to_be_bytes());
        }
    }
    out.push(TAG_END);
    Nbt(out)
}

/// The key a heightmap travels under.
///
/// Screaming snake case, which is Minecraft's own spelling for these and not a
/// transformation of the Rust name — a `to_uppercase` on a variant name would
/// be right today and wrong the moment a kind's name has two words in a
/// different arrangement.
fn network_name(kind: HeightmapKind) -> &'static str {
    match kind {
        HeightmapKind::MotionBlocking => "MOTION_BLOCKING",
        HeightmapKind::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
        HeightmapKind::OceanFloor => "OCEAN_FLOOR",
        HeightmapKind::WorldSurface => "WORLD_SURFACE",
        HeightmapKind::OceanFloorWg => "OCEAN_FLOOR_WG",
        HeightmapKind::WorldSurfaceWg => "WORLD_SURFACE_WG",
    }
}

/// The light packet for a column, from the column's own arrays.
///
/// The masks cover the sections **plus one above and one below**. The client
/// lights the boundary between a chunk and the void from those, and a mask
/// sized to the sections alone leaves a dark seam at the world's floor and
/// another at its ceiling. The two extra arrays are the world's outside: the
/// one below is dark, the one above is open sky.
///
/// Sky light goes out for every section and block light only for the sections
/// that have any, which is the difference between a light that is everywhere
/// by default and one that comes from cells. Both are the same 2,048-byte
/// nibble arrays and the client reads them the same way.
fn column_light(chunk: &Chunk, section_count: usize) -> play::chunk::LightData {
    let mut sky_mask = BitSet(Vec::new());
    let mut sky_arrays = Vec::with_capacity(section_count + 2);

    // Below the world: no sky reaches under bedrock.
    sky_mask.set(0, true);
    sky_arrays.push(LightArray(vec![0; LIGHT_SECTION_BYTES]));

    for (index, section) in chunk.sections().iter().enumerate() {
        sky_mask.set(index + 1, true);
        sky_arrays.push(LightArray(section.sky_light().as_bytes().to_vec()));
    }

    // Above the world: open sky, so the top of a column is not shadowed by the
    // absence of anything.
    sky_mask.set(section_count + 1, true);
    sky_arrays.push(LightArray(vec![FULL_BRIGHT; LIGHT_SECTION_BYTES]));

    // Block light, and only where there is any. **Absent and "present but
    // zero" mean the same thing to a renderer**, so a section with no light in
    // it is named in the empty mask instead of carrying 2,048 zero bytes —
    // which is what vanilla does, and on a surface chunk it is every section.
    //
    // The two sections outside the world are always empty: there is nothing
    // under bedrock to hold a torch, and nothing above the sky either.
    let mut block_mask = BitSet(Vec::new());
    let mut empty_block_mask = BitSet(Vec::new());
    let mut block_arrays = Vec::new();
    empty_block_mask.set(0, true);
    for (index, section) in chunk.sections().iter().enumerate() {
        let bytes = section.block_light().as_bytes();
        if bytes.iter().all(|byte| *byte == 0) {
            empty_block_mask.set(index + 1, true);
        } else {
            block_mask.set(index + 1, true);
            block_arrays.push(LightArray(bytes.to_vec()));
        }
    }
    empty_block_mask.set(section_count + 1, true);

    play::chunk::LightData {
        sky_mask,
        block_mask,
        empty_sky_mask: BitSet(Vec::new()),
        empty_block_mask,
        sky_arrays,
        block_arrays,
    }
}

/// Every column within `radius` of `centre`, nearest first.
///
/// Nearest first because a client renders what it has: a player standing on
/// the column under their feet while the far corner of the view distance
/// arrives is a player who is already playing, and one waiting for a spiral to
/// reach the middle is a player looking at the void.
pub fn columns_around(centre: ChunkPos, radius: i32) -> Vec<ChunkPos> {
    let mut columns = Vec::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            columns.push(ChunkPos::new(centre.x + dx, centre.z + dz));
        }
    }
    columns.sort_by_key(|pos| {
        let dx = pos.x - centre.x;
        let dz = pos.z - centre.z;
        dx * dx + dz * dz
    });
    columns
}

// ---------------------------------------------------------------------------
// Other players
// ---------------------------------------------------------------------------

use dust_protocol::packets::play::player_info::{
    PlayerInfoActions, PlayerInfoBody, PlayerInfoEntry, ProfileAddition,
};
use dust_protocol::types::{Angle, BoundedString, Uuid};

use super::players::Player;

/// The tab-list entry that says a player exists.
///
/// Sent alongside the entity and never instead of it: a client shown the
/// entity without an entry renders a body with no name plate and no skin,
/// because the skin is looked up from the profile this carries.
///
/// `ADD_PLAYER | UPDATE_LISTED`, and no more. The actions byte selects which
/// fields each entry carries, so a bit set here is a field the encoder must
/// write and a bit unset is one it must not — there is no way to send a field
/// "just in case", and a mismatched bit desynchronises every entry after it.
pub fn player_info_add(
    player: &Player,
) -> Result<play::clientbound::PlayerInfoUpdate, dust_protocol::wire::EncodeError> {
    Ok(play::clientbound::PlayerInfoUpdate {
        body: PlayerInfoBody {
            actions: PlayerInfoActions(
                PlayerInfoActions::ADD_PLAYER | PlayerInfoActions::UPDATE_LISTED,
            ),
            entries: vec![PlayerInfoEntry {
                uuid: Uuid(u128::from_be_bytes(player.uuid)),
                profile: Some(ProfileAddition {
                    name: BoundedString::new(player.name.clone())?,
                    // Empty in offline mode: the signed skin lives in the
                    // profile Mojang returns, and offline mode never asks. An
                    // online-mode server puts the real properties here and the
                    // player has their own face.
                    properties: Vec::new(),
                }),
                chat_session: None,
                game_mode: None,
                listed: Some(true),
                latency: None,
                display_name: None,
            }],
        },
    })
}

/// The tab-list removal. Keyed by uuid, where the entity removal is keyed by
/// entity id — two namespaces for one player, and sending one without the
/// other leaves either a ghost body or a ghost name.
pub fn player_info_remove(uuid: [u8; 16]) -> play::clientbound::PlayerInfoRemove {
    play::clientbound::PlayerInfoRemove {
        uuids: vec![Uuid(u128::from_be_bytes(uuid))],
    }
}

/// The entity that gives a player a body.
pub fn spawn_player(player: &Player, player_type: i32) -> play::clientbound::AddEntity {
    play::clientbound::AddEntity {
        entity_id: VarInt(player.entity_id),
        uuid: Uuid(u128::from_be_bytes(player.uuid)),
        kind: VarInt(player_type),
        x: player.x,
        y: player.y,
        z: player.z,
        pitch: Angle::from_degrees(player.pitch),
        yaw: Angle::from_degrees(player.yaw),
        // The head is sent at the body's yaw on spawn. They diverge as soon as
        // the player turns, and `rotate_head` carries that — but a spawn with
        // a head at zero while the body faces west renders as a player looking
        // over their own shoulder.
        head_yaw: Angle::from_degrees(player.yaw),
        data: VarInt(0),
        velocity: play::EntityVelocity { x: 0, y: 0, z: 0 },
    }
}

/// Move an entity that already exists.
///
/// A teleport rather than a delta. The delta packets carry position as
/// 1/4096-block shorts, which is smaller and only legal for moves under eight
/// blocks — so using them means tracking each viewer's idea of where each
/// entity is and falling back when it drifts too far. That bookkeeping is
/// worth having and is not free, and doing it wrong sends a delta that the
/// client applies to the wrong origin, which is a player sliding away into the
/// distance. Absolute coordinates cannot be wrong about where somebody is.
pub fn move_player(
    entity_id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
) -> play::clientbound::TeleportEntity {
    play::clientbound::TeleportEntity {
        entity_id: VarInt(entity_id),
        x,
        y,
        z,
        yaw: Angle::from_degrees(yaw),
        pitch: Angle::from_degrees(pitch),
        on_ground: true,
    }
}

/// Somebody else broke a block: the particles and the sound.
///
/// The `data` field is the **broken** block's state id, not the air left
/// behind — that is what the client makes the particle texture and the dig
/// sound out of, and sending the air's id gives a silent puff of nothing.
/// Captured from a real 1.21.1 server, which sends
/// `world_event effectId=2001 data=<state> global=false` to every player
/// except the one who dug.
///
/// `global` is false: this is a local effect with distance falloff, unlike the
/// handful of events — a wither spawning, the dragon dying — that every player
/// on the server hears wherever they are.
pub fn block_broken(
    position: dust_protocol::types::Position,
    previous: u32,
) -> play::clientbound::LevelEvent {
    play::clientbound::LevelEvent {
        event: PARTICLES_DESTROY_BLOCK,
        position,
        data: previous as i32,
        global: false,
    }
}

/// Vanilla's `LevelEvent.PARTICLES_DESTROY_BLOCK`. Named because 2001 beside a
/// state id is a number nobody can check.
const PARTICLES_DESTROY_BLOCK: i32 = 2001;

/// Somebody else swung an arm.
///
/// The animation table is the protocol's: 0 is the main hand, 3 the off hand.
/// A client sends `swing` with a *hand* and expects everybody else to be sent
/// an `animate` with the matching animation, and mapping one to the other is
/// this function's whole job — a server that relayed the hand number would
/// make an off-hand swing look like leaving a nest egg.
pub fn swing(entity_id: i32, off_hand: bool) -> play::clientbound::Animate {
    play::clientbound::Animate {
        entity_id: VarInt(entity_id),
        animation: if off_hand {
            SWING_OFF_HAND
        } else {
            SWING_MAIN_HAND
        },
    }
}

/// `ClientboundAnimatePacket.SWING_MAIN_HAND`.
const SWING_MAIN_HAND: u8 = 0;
/// `ClientboundAnimatePacket.SWING_OFF_HAND`. Not 1 — 1 is taking damage and
/// 2 is waking up, which is why these are named rather than counted.
const SWING_OFF_HAND: u8 = 3;

/// Somebody else started or stopped crouching or running.
///
/// **Two slots, not one, and both are needed.** Index 0 is the shared entity
/// flag byte, where bit 1 is crouching and bit 3 is sprinting; index 6 is the
/// pose, which is what actually shortens the model and the hitbox. A client
/// told only the flag renders a full-height player with a dimmed name tag, and
/// one told only the pose renders a crouch that does not sneak.
pub fn posture(
    entity_id: i32,
    sneaking: bool,
    sprinting: bool,
) -> play::clientbound::SetEntityData {
    let mut flags = 0u8;
    if sneaking {
        flags |= ENTITY_FLAG_CROUCHING;
    }
    if sprinting {
        flags |= ENTITY_FLAG_SPRINTING;
    }
    play::clientbound::SetEntityData {
        entity_id: VarInt(entity_id),
        entries: metadata::MetadataEntries(vec![
            metadata::MetadataEntry {
                index: ENTITY_FLAGS_INDEX,
                value: metadata::MetadataValue::Byte(flags as i8),
            },
            metadata::MetadataEntry {
                index: POSE_INDEX,
                value: metadata::MetadataValue::Pose(if sneaking {
                    metadata::Pose::Sneaking
                } else {
                    metadata::Pose::Standing
                }),
            },
        ]),
    }
}

/// `Entity.DATA_SHARED_FLAGS_ID`, slot 0 on every entity there is.
const ENTITY_FLAGS_INDEX: u8 = 0;
/// `Entity.DATA_POSE`, slot 6 on 1.21.1.
const POSE_INDEX: u8 = 6;
/// Bit 1 of the shared flags.
const ENTITY_FLAG_CROUCHING: u8 = 0x02;
/// Bit 3 of the shared flags.
const ENTITY_FLAG_SPRINTING: u8 = 0x08;

/// The head's yaw, which the body's does not imply.
///
/// Living entities carry both: the head leads a turn and the body follows, and
/// a client sent only the body's yaw renders a player whose head never moves.
pub fn turn_head(entity_id: i32, yaw: f32) -> play::clientbound::RotateHead {
    play::clientbound::RotateHead {
        entity_id: VarInt(entity_id),
        head_yaw: Angle::from_degrees(yaw),
    }
}

/// Take a player's body away.
pub fn despawn(entity_id: i32) -> play::clientbound::RemoveEntities {
    play::clientbound::RemoveEntities {
        entity_ids: vec![VarInt(entity_id)],
    }
}

/// `minecraft:player`'s id in the entity-type registry.
///
/// Resolved from the generated table rather than written down: it is a
/// position in a list that is regenerated per version, and a constant would be
/// right until a bump and then spawn something else entirely.
pub fn player_entity_type() -> Option<i32> {
    dust_registry::EntityType::from_name("minecraft:player")
        .and_then(|t| i32::try_from(t.protocol_id()).ok())
}

// ---------------------------------------------------------------------------
// The rest of what a joining client is told
// ---------------------------------------------------------------------------

use dust_protocol::packets::play::Abilities;
use dust_protocol::types::Position;

/// What the player may do with their own body.
///
/// **The one packet on this list whose absence is felt immediately.** A client
/// in creative mode that is never sent it cannot fly: the flags are where
/// flight is *granted*, and the client's own movement prediction runs from
/// them, not from the game mode in the join packet. Dust puts everybody in
/// creative and did not send this, which is a creative player who walks.
///
/// Compared against a real 1.21.1 server, which sends it as the third packet
/// after login — before the position, and long before the chunks.
pub fn abilities(creative: bool) -> play::clientbound::PlayerAbilities {
    let flags = if creative {
        Abilities::INVULNERABLE | Abilities::ALLOW_FLYING | Abilities::INSTANT_BREAK
    } else {
        0
    };
    play::clientbound::PlayerAbilities {
        flags: Abilities(flags),
        // Vanilla's own defaults. They are the values the client assumes when
        // it has never been told, which is exactly why sending the same ones
        // is not a no-op: the client that was never told is also the client
        // that was never granted flight.
        flying_speed: 0.05,
        fov_modifier: 0.1,
    }
}

/// Where compasses point and where a player respawns.
///
/// Not the same as where they currently are — a returning player is put back
/// where they left, and the compass still points here.
pub fn default_spawn(at: (f64, f64, f64)) -> play::clientbound::SetDefaultSpawnPosition {
    play::clientbound::SetDefaultSpawnPosition {
        location: Position {
            x: at.0.floor() as i32,
            y: at.1.floor() as i32,
            z: at.2.floor() as i32,
        },
        angle: 0.0,
    }
}

/// Full health, a full hunger bar, and vanilla's starting saturation.
///
/// Nothing in this server damages anybody yet, so this is a constant rather
/// than a reading — and it is sent anyway, because it is not only decoration.
/// A vanilla client that is never told its health assumes it is alive and
/// renders a full bar; `mineflayer` waits for this packet before it considers
/// itself in the world at all, and without it a bot connects, receives its
/// position and every chunk around it, and then sits in the loading state
/// forever. Vanilla sends it on join, so Dust does.
///
/// Saturation is 5.0 and not 20.0: a fresh vanilla player has five, which is
/// why sprinting starts eating into the hunger bar as soon as it does.
pub fn full_health() -> play::clientbound::SetHealth {
    play::clientbound::SetHealth {
        health: 20.0,
        food: VarInt(20),
        food_saturation: 5.0,
    }
}

/// The world clock.
///
/// Two numbers, and they mean different things. `world_age` only ever counts
/// up and is what scoreboards and some redstone read; `time_of_day` is the
/// position of the sun within a 24,000-tick day. A **negative** `time_of_day`
/// tells the client the cycle is frozen at its absolute value, which is what
/// this server sends: nothing here ticks a clock, and a sun that never moves is
/// better than one that jumps back to dawn every time somebody joins.
pub fn frozen_at_noon() -> play::clientbound::SetTime {
    /// Midday, when a superflat looks like anything at all.
    const NOON: i64 = 6_000;
    play::clientbound::SetTime {
        world_age: 0,
        time_of_day: -NOON,
    }
}

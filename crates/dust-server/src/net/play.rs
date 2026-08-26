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
//! Two things it is not. Light does **not cross a chunk boundary** — each
//! column is lit alone, so a terrain step at an edge leaves a seam — and there
//! is **no block light at all**, because nothing in this world emits any. Both
//! are stated in `dust_world::column_light`, and neither is the kind of gap
//! that renders as a broken packet.

use dust_protocol::nbt::Nbt;
use dust_protocol::packets::play;
use dust_protocol::packets::play::chunk::{
    ChunkData, LightArray, Section as WireSection, LIGHT_SECTION_BYTES,
};
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

/// Where the player is, and the teleport it must acknowledge.
pub fn position_packet(
    spawn: (f64, f64, f64),
    teleport_id: i32,
) -> play::clientbound::PlayerPosition {
    play::clientbound::PlayerPosition {
        x: spawn.0,
        y: spawn.1,
        z: spawn.2,
        yaw: 0.0,
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

    let mut empty_block_mask = BitSet(Vec::new());
    for index in 0..section_count + 2 {
        empty_block_mask.set(index, true);
    }

    play::chunk::LightData {
        sky_mask,
        // Nothing in this world emits light, so no block-light array is sent
        // and every section is named in the empty mask. Absent and
        // "present but zero" mean the same thing to a renderer; absent is
        // fewer bytes and is what vanilla sends for a chunk with no torches.
        block_mask: BitSet(Vec::new()),
        empty_sky_mask: BitSet(Vec::new()),
        empty_block_mask,
        sky_arrays,
        block_arrays: Vec::new(),
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

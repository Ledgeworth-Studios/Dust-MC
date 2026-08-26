//! Play, server to client: the packets that keep a world in front of a
//! player.
//!
//! Field layouts here were written against the protocol documentation for 767
//! and are checked three ways: round trips over every packet, property tests
//! over generated values, and the mutation loop that insists a hostile body
//! can fail but never panic. What no offline test can check is whether
//! Mojang's client agrees with all of it; that is what pointing a real
//! 1.21.1 client at this crate is for.

use crate::packet_group;
use crate::packets::play::chunk::{BlockEntity, ChunkData, LightData};
use crate::packets::play::metadata::MetadataEntries;
use crate::packets::play::player_info::PlayerInfoBody;
use crate::packets::play::{Abilities, BlockChangeEntry, ChunkSectionPosition, DeathLocation};
use crate::packets::play::{EntityDelta, EntityVelocity, GameModeByte, PreviousGameMode};
use crate::packets::play::{TeleportFlags, chat as chat_fields};
use crate::text::Component;
use crate::types::{Angle, BoundedString, Identifier, Position, RestOfPacket, Uuid, VarInt};
use crate::nbt::Nbt;

packet_group! {
    state: Play,
    direction: Clientbound,
    versions: ["1.21.1"],

    /// A player or object becomes visible: identity, kind, where and how it
    /// moves.
    ///
    /// `data` means something different per entity kind — a launcher's item
    /// for arrows, nothing for most — and is carried raw because resolving it
    /// needs the entity table, not the protocol.
    "minecraft:add_entity" => AddEntity {
        entity_id: VarInt,
        uuid: Uuid,
        kind: VarInt,
        x: f64,
        y: f64,
        z: f64,
        pitch: Angle,
        yaw: Angle,
        head_yaw: Angle,
        data: VarInt,
        velocity: EntityVelocity,
    },

    /// One block changed. The chunk it sits in must already be loaded, which
    /// is why this cannot replace [`SectionBlocksUpdate`] for world edits:
    /// two blocks in one tick go out together or the client animates them
    /// apart.
    "minecraft:block_update" => BlockUpdate {
        location: Position,
        block_id: VarInt,
    },

    /// Kicks the player to the server list with a reason. **NBT** component,
    /// same as every play-state text field.
    "minecraft:disconnect" => Disconnect {
        reason: Component,
    },

    /// The whole chunk column plus its light, on first sight.
    ///
    /// The sections themselves are bytes behind a trait; see
    /// [`crate::packets::play::chunk::Section`] for where dust-world plugs
    /// in. Heightmaps ride along as NBT this layer delimits without opening.
    "minecraft:level_chunk_with_light" => LevelChunkWithLight {
        chunk_x: i32,
        chunk_z: i32,
        heightmaps: Nbt,
        data: ChunkData,
        block_entities: Vec<BlockEntity>,
        light: LightData,
    },

    /// Liveness: eight opaque bytes the client must hand straight back.
    "minecraft:keep_alive" => KeepAlive {
        id: i64,
    },

    /// You have joined: who you are, which world, how much of it to load.
    ///
    /// The registries themselves were synced during configuration; this
    /// packet carries only the *names* of the dimensions and the id of the
    /// one being spawned into, which is the shape 1.20.5+ settled on.
    /// `dimension_type` is an id into the dimension-type registry — a VarInt
    /// here where older versions spelled an identifier, and exactly the kind
    /// of change the version parameter exists for.
    "minecraft:login" => Login {
        entity_id: i32,
        hardcore: bool,
        dimensions: Vec<Identifier>,
        max_players: VarInt,
        view_distance: VarInt,
        simulation_distance: VarInt,
        reduced_debug_info: bool,
        respawn_screen: bool,
        limited_crafting: bool,
        dimension_type: VarInt,
        dimension_name: Identifier,
        hashed_seed: i64,
        game_mode: GameModeByte,
        previous_game_mode: PreviousGameMode,
        debug: bool,
        flat: bool,
        death_location: Option<DeathLocation>,
        portal_cooldown: VarInt,
        secure_chat: bool,
    },

    /// Entities moved and turned at once. Deltas are 1/4096-block shorts so
    /// ordinary motion costs eleven bytes instead of twenty-seven.
    "minecraft:move_entity_pos_rot" => MoveEntityPosRot {
        entity_id: VarInt,
        delta: EntityDelta,
        yaw: Angle,
        pitch: Angle,
        on_ground: bool,
    },

    /// An entity moved without turning.
    "minecraft:move_entity_pos" => MoveEntityPos {
        entity_id: VarInt,
        delta: EntityDelta,
        on_ground: bool,
    },

    /// An entity turned without moving.
    "minecraft:move_entity_rot" => MoveEntityRot {
        entity_id: VarInt,
        yaw: Angle,
        pitch: Angle,
        on_ground: bool,
    },

    /// Another player's chat message, with its signing envelope.
    ///
    /// Dust relays messages unsigned; the fields still decode fully because a
    /// decoder that skipped them could not find the end of the packet. See
    /// [`crate::packets::play::chat`] for the seam.
    "minecraft:player_chat" => PlayerChatMessage {
        sender: Uuid,
        index: VarInt,
        signature: Option<chat_fields::SignatureBytes>,
        message: BoundedString<256>,
        timestamp: i64,
        salt: i64,
        previous_messages: Vec<chat_fields::AcknowledgedMessage>,
        unsigned_content: Option<Component>,
        filter: chat_fields::ChatFilter,
        chat_type: VarInt,
        network_name: Component,
        network_target_name: Option<Component>,
    },

    /// Grants or revokes flight, invulnerability and their friends, with the
    /// speeds the client should feel them at.
    "minecraft:player_abilities" => PlayerAbilities {
        flags: Abilities,
        flying_speed: f32,
        fov_modifier: f32,
    },

    /// Where the player now is, and whether any of that is relative. The
    /// teleport id exists to be echoed back by the serverbound
    /// `accept_teleportation`, which is how both sides agree the move
    /// happened.
    "minecraft:player_position" => PlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        flags: TeleportFlags,
        teleport_id: VarInt,
    },

    /// Which players exist and what the tab list says about them.
    ///
    /// One bitmask selects up to six layouts per entry, which is why the body
    /// after the id is one type; see [`PlayerInfoBody`]'s module for how to
    /// read that.
    "minecraft:player_info_update" => PlayerInfoUpdate {
        body: PlayerInfoBody,
    },

    /// Players gone from the tab list. Removals carry no actions: the uuids
    /// are the whole message.
    "minecraft:player_info_remove" => PlayerInfoRemove {
        uuids: Vec<Uuid>,
    },

    /// A plugin channel message toward the client.
    "minecraft:custom_payload" => CustomPayload {
        channel: Identifier,
        data: RestOfPacket,
    },

    /// Entities ceased to exist. A list rather than one-per-packet because
    /// deaths and chunk unloads take many at once.
    "minecraft:remove_entities" => RemoveEntities {
        entity_ids: Vec<VarInt>,
    },

    /// The head's yaw, separately from the body's. Living entities need both;
    /// the head leads turns and the body follows, which is why this is not
    /// just another angle on the movement packets.
    "minecraft:rotate_head" => RotateHead {
        entity_id: VarInt,
        head_yaw: Angle,
    },

    /// Several blocks changed in one section, in one tick.
    "minecraft:section_blocks_update" => SectionBlocksUpdate {
        section: ChunkSectionPosition,
        entries: Vec<BlockChangeEntry>,
    },

    /// Which hotbar slot is held. A byte here and a short in the serverbound
    /// direction — same name, different widths, four packets apart.
    "minecraft:set_carried_item" => SetCarriedItem {
        slot: u8,
    },

    /// A named slot on an entity changed value: pose, health, fire, name.
    ///
    /// The entries run until a terminator byte, so they are a type of their
    /// own; unknown serializers refuse rather than guess. See
    /// [`crate::packets::play::metadata`].
    "minecraft:set_entity_data" => SetEntityData {
        entity_id: VarInt,
        entries: MetadataEntries,
    },

    /// Server-to-player text that is nobody's speech: motds, game messages,
    /// action-bar lines. `overlay` picks the action bar over the chat log.
    "minecraft:system_chat" => SystemChat {
        content: Component,
        overlay: bool,
    },

    /// Round-trip latency probe: an int the client echoes through pong.
    "minecraft:ping" => Ping {
        id: i32,
    },

    /// The answer to the client's own ping request, payload untouched.
    "minecraft:pong_response" => PongResponse {
        payload: i64,
    },
}

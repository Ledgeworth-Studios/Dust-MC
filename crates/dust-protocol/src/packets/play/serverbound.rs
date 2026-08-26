//! Play, client to server: what a player does, and how the server learns it.
//!
//! The movement family is four packets that differ only in which fields made
//! it onto the wire — position, position plus rotation, rotation alone, or
//! nothing but the on-ground flag. They are four definitions and not one
//! clever type because the wire has no tag saying which is which: the packet
//! id *is* the tag, and collapsing them would put the id back inside a body,
//! which this crate never does.

use crate::packet_group;
use crate::packets::play::advancements::SeenAdvancementsBody;
use crate::packets::play::chat::MessageAcknowledgement;
use crate::packets::play::containers::{ChangedSlot, ClickType};
use crate::packets::play::Abilities;
use crate::types::{BoundedString, Identifier, RestOfPacket, Slot, VarInt};

packet_group! {
    state: Play,
    direction: Serverbound,
    versions: ["1.21.1"],

    /// "The teleport you sent me happened." Carries back the id from
    /// the clientbound `player_position`; until this arrives the server treats
    /// the client's position as provisional.
    "minecraft:accept_teleportation" => TeleportConfirm {
        teleport_id: VarInt,
    },

    /// A player chat message, with whatever signing artifacts the client
    /// attached.
    ///
    /// Dust neither verifies nor requires signatures; the acknowledgement
    /// travels so the layout stays exact, and the message content is what
    /// gameplay sees. See [`crate::packets::play::chat`].
    "minecraft:chat" => Chat {
        message: BoundedString<256>,
        timestamp: i64,
        salt: i64,
        signature: Option<crate::packets::play::chat::SignatureBytes>,
        acknowledgement: MessageAcknowledgement,
    },

    /// A plugin channel message toward the server.
    "minecraft:custom_payload" => CustomPayload {
        channel: Identifier,
        data: RestOfPacket,
    },

    /// The echo of a keep-alive, same eight bytes back.
    "minecraft:keep_alive" => KeepAlive {
        id: i64,
    },

    /// Movement with position only; the client sends whichever of these
    /// family members covers the change it has to report.
    "minecraft:move_player_pos" => MovePlayerPos {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },

    /// Movement plus look direction, the ordinary walking packet.
    "minecraft:move_player_pos_rot" => MovePlayerPosRot {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },

    /// Look direction without movement.
    "minecraft:move_player_rot" => MovePlayerRot {
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },

    /// Nothing but the on-ground flag, for a player who did not move and did
    /// not turn but landed or jumped in place. Sent on a timer even when
    /// nothing happened, which is why its arrival is not evidence of motion.
    "minecraft:move_player_status_only" => MovePlayerStatusOnly {
        on_ground: bool,
    },

    /// The answer to the clientbound ping: the same int, straight back.
    "minecraft:pong" => Pong {
        id: i32,
    },

    /// Flight toggles. The flags byte is the whole packet; the speeds live
    /// only in the clientbound direction, because a client tells the server
    /// what it wants to do and the server tells it how fast that may happen.
    "minecraft:player_abilities" => PlayerAbilities {
        flags: Abilities,
    },

    /// A latency probe from the client side; answered with pong_response.
    "minecraft:ping_request" => PingRequest {
        payload: i64,
    },

    /// Which hotbar slot the player switched to, as a short here where the
    /// clientbound twin uses a byte. Same name, different widths.
    "minecraft:set_carried_item" => SetCarriedItem {
        slot: i16,
    },

    /// The player clicked a slot: what they did, and the container as they
    /// believe it now stands.
    ///
    /// The server replays the click over its own state and pushes back only
    /// the slots that disagree, which is why this packet carries the client's
    /// opinion rather than a request. `state_id` ties it to the last full or
    /// partial sync; see [`ChangedSlot`] and [`ClickType`].
    "minecraft:container_click" => ClickContainer {
        window_id: u8,
        state_id: VarInt,
        slot: i16,
        button: i8,
        mode: ClickType,
        changed_slots: Vec<ChangedSlot>,
        cursor_item: Slot,
    },

    /// The advancement screen opened onto one tab, or closed.
    ///
    /// The action picks whether the tab id follows — see
    /// [`SeenAdvancementsBody`].
    "minecraft:seen_advancements" => SeenAdvancements {
        body: SeenAdvancementsBody,
    },
}

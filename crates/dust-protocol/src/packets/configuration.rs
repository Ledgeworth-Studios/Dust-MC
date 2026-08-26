//! Configuration: registries, tags, resource packs, and the handoff to Play.
//!
//! The state that did not exist before 1.20.2, and the reason a client can be
//! told what the world is made of before it is put in one. It is also where
//! most of the bytes are: a vanilla server sends several hundred kilobytes of
//! registry data here, or almost none if the client says it already has the
//! packs — see [`SelectKnownPacks`](clientbound::SelectKnownPacks).

/// Server to client.
pub mod clientbound {
    use crate::nbt::TextComponent;
    use crate::packet_group;
    use crate::packets::common::{KnownPack, RegistryEntry, ReportDetail, ServerLink, TagRegistry};
    use crate::types::{
        BoundedString, Identifier, PrefixedBytes, ProtocolString, RestOfPacket, Uuid, VarInt,
    };

    packet_group! {
        state: Configuration,
        direction: Clientbound,
        versions: ["1.21.1"],

        /// Asks the client for a stored cookie.
        "minecraft:cookie_request" => CookieRequest {
            key: Identifier,
        },

        /// A channel message. `minecraft:brand` travels this way, which is how
        /// a client learns the server is not vanilla.
        "minecraft:custom_payload" => CustomPayload {
            channel: Identifier,
            data: RestOfPacket,
        },

        /// Refusal, with a reason. **NBT**, unlike the login-state disconnect
        /// one packet group away. See `crate::packets::login`.
        "minecraft:disconnect" => Disconnect {
            reason: TextComponent,
        },

        /// Configuration is over; the client acknowledges and both move to
        /// Play.
        "minecraft:finish_configuration" => FinishConfiguration {},

        /// A liveness check the client must echo with the same value.
        "minecraft:keep_alive" => KeepAlive {
            id: i64,
        },

        /// A different liveness check with a different width, kept separate
        /// from [`KeepAlive`] because they are different packets that do nearly
        /// the same thing — this one is an `i32` and is answered by `pong`.
        "minecraft:ping" => Ping {
            id: i32,
        },

        /// Clears the client's chat history. No fields.
        "minecraft:reset_chat" => ResetChat {},

        /// One registry's contents.
        ///
        /// Sent once per registry, so a real connection sees a run of these.
        /// The entries' payloads are NBT this crate delimits and does not open;
        /// see [`RegistryEntry`] for that seam.
        "minecraft:registry_data" => RegistryData {
            registry_id: Identifier,
            entries: Vec<RegistryEntry>,
        },

        /// Remove a resource pack, or all of them if no id is given.
        "minecraft:resource_pack_pop" => ResourcePackPop {
            uuid: Option<Uuid>,
        },

        /// Offer a resource pack.
        ///
        /// `hash` is a hex SHA-1 and is bounded at 40 because that is how many
        /// characters a hex SHA-1 has. A vanilla server sends an empty string
        /// when it has no hash, which is why the field is not an `Option`.
        "minecraft:resource_pack_push" => ResourcePackPush {
            uuid: Uuid,
            url: ProtocolString,
            hash: BoundedString<40>,
            forced: bool,
            prompt_message: Option<TextComponent>,
        },

        /// Ask the client to keep a value and hand it back on a later
        /// connection, including to a different server.
        "minecraft:store_cookie" => StoreCookie {
            key: Identifier,
            payload: PrefixedBytes<5120>,
        },

        /// Send the client to another server, keeping its cookies.
        ///
        /// The port is a `VarInt` here and a `u16` in the handshake. Same
        /// quantity, two encodings, four packets apart — and the type system is
        /// the only thing that will remember.
        "minecraft:transfer" => Transfer {
            host: ProtocolString,
            port: VarInt,
        },

        /// Which experimental features are on.
        "minecraft:update_enabled_features" => UpdateEnabledFeatures {
            features: Vec<Identifier>,
        },

        /// Every tag of every registry, in one packet.
        "minecraft:update_tags" => UpdateTags {
            registries: Vec<TagRegistry>,
        },

        /// Which data packs the server has, so the client can say which of them
        /// it already has and be spared the registry dump.
        ///
        /// The client answers with the subset it recognises, and the server
        /// then omits every registry that pack would have defined. This is why
        /// a real configuration exchange can be small.
        "minecraft:select_known_packs" => SelectKnownPacks {
            packs: Vec<KnownPack>,
        },

        /// Extra lines for the client to put in a crash report.
        "minecraft:custom_report_details" => CustomReportDetails {
            details: Vec<ReportDetail>,
        },

        /// Links to show in the pause menu.
        "minecraft:server_links" => ServerLinks {
            links: Vec<ServerLink>,
        },
    }
}

/// Client to server.
pub mod serverbound {
    use crate::packet_group;
    use crate::packets::common::KnownPack;
    use crate::types::{
        BoundedString, ChatVisibility, Identifier, MainHand, PrefixedBytes, ResourcePackResult,
        RestOfPacket, Uuid,
    };

    packet_group! {
        state: Configuration,
        direction: Serverbound,
        versions: ["1.21.1"],

        /// Everything the client wants the server to know about its settings.
        ///
        /// `view_distance` is a signed byte and is the client's *request*, not
        /// a fact. `displayed_skin_parts` is a bit field of the seven skin
        /// layers the player has switched on, and is a raw `u8` here rather
        /// than a [`FixedBitSet`](crate::types::FixedBitSet) because it is
        /// vanilla's own single byte with no length prefix — modelling it as a
        /// bit set would be truer to what it means and wrong about what it is.
        "minecraft:client_information" => ClientInformation {
            locale: BoundedString<16>,
            view_distance: i8,
            chat_mode: ChatVisibility,
            chat_colors: bool,
            displayed_skin_parts: u8,
            main_hand: MainHand,
            text_filtering_enabled: bool,
            allow_server_listings: bool,
        },

        /// The cookie the server asked for, or nothing.
        "minecraft:cookie_response" => CookieResponse {
            key: Identifier,
            payload: Option<PrefixedBytes<5120>>,
        },

        /// A channel message from the client.
        "minecraft:custom_payload" => CustomPayload {
            channel: Identifier,
            data: RestOfPacket,
        },

        /// The client agrees configuration is over.
        "minecraft:finish_configuration" => FinishConfiguration {},

        /// The echo of a keep-alive.
        "minecraft:keep_alive" => KeepAlive {
            id: i64,
        },

        /// The echo of a ping.
        "minecraft:pong" => Pong {
            id: i32,
        },

        /// What happened to a resource pack the server pushed.
        "minecraft:resource_pack" => ResourcePack {
            uuid: Uuid,
            result: ResourcePackResult,
        },

        /// The subset of the server's packs the client already has.
        "minecraft:select_known_packs" => SelectKnownPacks {
            packs: Vec<KnownPack>,
        },
    }
}

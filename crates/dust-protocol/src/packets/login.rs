//! Login: authentication, encryption, compression, and the handoff to
//! configuration.
//!
//! # The one that catches people
//!
//! `login_disconnect` carries a **JSON** text component, and
//! `configuration/disconnect` carries an **NBT** one. Same word, adjacent in
//! the id table, opposite encodings. 1.20.3 moved text components to NBT and
//! left this packet alone, because it is sent to a client that may not have
//! got as far as agreeing on much. A decoder that "modernised" it is wrong on
//! 1.21.1 today — see the live-server test, which triggers this packet by
//! claiming an outdated protocol version and checks which encoding actually
//! arrives.

/// Server to client.
pub mod clientbound {
    use crate::nbt::JsonTextComponent;
    use crate::packet_group;
    use crate::packets::common::ProfileProperty;
    use crate::types::{BoundedString, Identifier, PrefixedBytes, RestOfPacket, Uuid, VarInt};

    packet_group! {
        state: Login,
        direction: Clientbound,
        versions: ["1.21.1"],

        /// Refusal, with a reason. **JSON**, not NBT; see the module docs.
        "minecraft:login_disconnect" => LoginDisconnect {
            reason: JsonTextComponent,
        },

        /// Start encryption, and say whether Mojang will be asked about this
        /// player.
        ///
        /// `should_authenticate` arrived in 1.20.5 and is the field a decoder
        /// written against an older wiki page leaves off — which costs one
        /// byte and desynchronises everything after it. An offline-mode server
        /// never sends this packet at all, so a test suite that only ever ran
        /// against an offline server would never notice either way.
        "minecraft:hello" => Hello {
            server_id: BoundedString<20>,
            public_key: PrefixedBytes<1024>,
            verify_token: PrefixedBytes<1024>,
            should_authenticate: bool,
        },

        /// Login succeeded: here is who you are.
        ///
        /// `properties` is empty in offline mode and carries the signed skin in
        /// online mode, which is the difference between the two that most
        /// affects this layer.
        "minecraft:game_profile" => GameProfile {
            uuid: Uuid,
            username: BoundedString<16>,
            properties: Vec<ProfileProperty>,
            /// 1.20.5. Asks the client to treat a malformed packet as fatal
            /// rather than ignoring it — a debugging aid that is nevertheless a
            /// field on the wire.
            strict_error_handling: bool,
        },

        /// Compress everything above this size from now on.
        ///
        /// A threshold of zero or less turns compression off. This changes the
        /// frame format for every later packet, which makes it the one packet
        /// in this crate with an effect `dust-net` has to know about — the
        /// value belongs to framing even though the packet belongs here.
        "minecraft:login_compression" => LoginCompression {
            threshold: VarInt,
        },

        /// A modded server asking a modded client something during login.
        "minecraft:custom_query" => CustomQuery {
            message_id: VarInt,
            channel: Identifier,
            /// Everything left. Must be last, and is.
            data: RestOfPacket,
        },

        /// Asks the client for a cookie this server stored earlier, possibly on
        /// a different server it transferred the player from.
        "minecraft:cookie_request" => CookieRequest {
            key: Identifier,
        },
    }
}

/// Client to server.
pub mod serverbound {
    use crate::packet_group;
    use crate::types::{BoundedString, Identifier, PrefixedBytes, RestOfPacket, Uuid, VarInt};

    packet_group! {
        state: Login,
        direction: Serverbound,
        versions: ["1.21.1"],

        /// Who the client says it is.
        ///
        /// `profile_id` has been mandatory since 1.20.2 — it was optional
        /// before, and a decoder carrying that memory reads a boolean that is
        /// not there. In offline mode the server ignores this and derives its
        /// own id from the name, so a wrong value here is invisible until it
        /// meets an online-mode server.
        "minecraft:hello" => Hello {
            name: BoundedString<16>,
            profile_id: Uuid,
        },

        /// The client's answer to [`Hello`](super::clientbound::Hello): a
        /// shared secret and the verify token, both under the server's public
        /// key.
        "minecraft:key" => Key {
            shared_secret: PrefixedBytes<1024>,
            verify_token: PrefixedBytes<1024>,
        },

        /// The answer to a custom query, or a refusal to answer.
        ///
        /// `None` is how a vanilla client answers a channel it does not know,
        /// and it is the common case: a plain client answers every modded
        /// query this way.
        "minecraft:custom_query_answer" => CustomQueryAnswer {
            message_id: VarInt,
            data: Option<RestOfPacket>,
        },

        /// The client has processed the game profile and is ready for
        /// configuration. No fields; the packet *is* the message.
        "minecraft:login_acknowledged" => LoginAcknowledged {},

        /// The cookie a server asked for, or nothing if the client has none.
        "minecraft:cookie_response" => CookieResponse {
            key: Identifier,
            payload: Option<PrefixedBytes<5120>>,
        },
    }
}

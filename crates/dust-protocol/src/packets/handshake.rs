//! The handshake: one packet, in one direction.
//!
//! There is no clientbound handshake because the server says nothing until it
//! knows which state the connection is asking for. The generated id table says
//! the same thing by having an empty `handshake`/`clientbound` pair.

/// Client to server.
pub mod serverbound {
    use crate::packet_group;
    use crate::types::{BoundedString, NextState, VarInt};

    packet_group! {
        state: Handshake,
        direction: Serverbound,
        versions: ["1.21.1"],

        /// The first packet of every connection: which protocol the client
        /// speaks, where it thinks it connected, and what it wants next.
        ///
        /// `protocol_version` is the number that decides everything after this
        /// point, and it is deliberately a plain [`VarInt`] rather than a
        /// [`ProtocolVersion`](crate::ProtocolVersion). A client may send any
        /// number, including one this server has never heard of — that is the
        /// entire purpose of the field — so it arrives as data and is resolved
        /// to a version afterwards, by a caller that can answer "no" politely.
        ///
        /// `server_address` and `server_port` are what the client believes it
        /// dialled, not what it reached, and a server behind a proxy sees the
        /// proxy's opinion. They are informational: nothing here should route
        /// on them without knowing that.
        "minecraft:intention" => Intention {
            protocol_version: VarInt,
            server_address: BoundedString<255>,
            server_port: u16,
            next_state: NextState,
        },
    }
}

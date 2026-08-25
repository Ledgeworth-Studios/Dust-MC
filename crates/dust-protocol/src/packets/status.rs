//! The server list ping: four packets, no authentication, no state kept.
//!
//! The whole exchange is two round trips and it is the smallest complete
//! conversation the protocol has, which makes it the natural first thing to
//! get right — and the natural thing to check against a real server, because
//! a vanilla server answers it without any credentials at all.

/// Server to client.
///
/// The JSON body of a status response is deliberately not modelled here.
/// Building it needs a JSON serialiser, and the shape it serialises — version,
/// players, description, favicon — is a server policy decision rather than a
/// protocol one. This layer's job ends at "a length-prefixed string travels
/// here", and the string's contents are the caller's.
pub mod clientbound {
    use crate::packet_group;
    use crate::types::ProtocolString;

    packet_group! {
        state: Status,
        direction: Clientbound,
        versions: ["1.21.1"],

        /// The server list entry, as JSON.
        ///
        /// Still JSON on 1.21.1, and worth being explicit about because
        /// everything else that carries text stopped being JSON in 1.20.3.
        /// This is not a text component in a status response — it is a JSON
        /// *document* with a text component inside it, under `description` —
        /// so it stays a string all the way down.
        "minecraft:status_response" => StatusResponse {
            json: ProtocolString,
        },

        /// The same eight bytes the client sent, returned unexamined.
        ///
        /// Not a timestamp as far as this server is concerned. The client
        /// chooses the payload and uses it to measure a round trip; treating it
        /// as a time would be reading meaning into a number that belongs to
        /// somebody else.
        "minecraft:pong_response" => PongResponse {
            payload: i64,
        },
    }
}

/// Client to server.
pub mod serverbound {
    use crate::packet_group;

    packet_group! {
        state: Status,
        direction: Serverbound,
        versions: ["1.21.1"],

        /// Asks for the server list entry. No fields at all.
        "minecraft:status_request" => StatusRequest {},

        /// Eight bytes the server must hand straight back.
        "minecraft:ping_request" => PingRequest {
            payload: i64,
        },
    }
}

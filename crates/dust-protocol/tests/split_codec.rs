//! The two halves of a packet codec must agree with the whole.
//!
//! [`Packet::encode`] writes an id and then a body; [`Packet::protocol_id`]
//! and [`Packet::encode_body`] produce those two things separately, for a
//! framed transport that keeps them apart. Decoding has the same pair.
//!
//! The split exists so `dust-net`'s `Frame { id, body }` can cross into this
//! crate without a copy, and its whole risk is that the halves drift from the
//! joined version — an id written by one path and not the other, a body that
//! starts one byte later. Neither drift is visible in a round trip through
//! *one* path: split-encode into split-decode agrees with itself perfectly
//! while disagreeing with every other server on the network.
//!
//! So the assertion here is never "it round-trips". It is that the joined form
//! and the split form are the *same bytes*, and that each form decodes what
//! the other wrote. That is a claim about the two implementations agreeing
//! with each other, and it is only worth making because the joined form is the
//! one the corpus tests already pinned against real captured packets.

use dust_protocol::packets::handshake::serverbound as hs;
use dust_protocol::packets::status::clientbound as sc;
use dust_protocol::packets::status::serverbound as ss;
use dust_protocol::types::{BoundedString, NextState, ProtocolString, VarInt};
use dust_protocol::version;
use dust_protocol::wire::{Reader, WireRead as _, WireWrite as _, Writer};

/// Assert that the joined and split encodings of `$packet` are byte-identical,
/// and that each decode path reads what either encode path wrote.
macro_rules! agrees {
    ($module:ident, $value:expr) => {{
        let v = version::V1_21_1;
        let packet: $module::Packet = $value.into();

        // Joined: id and body in one buffer.
        let mut joined = Writer::default();
        let id_from_joined = packet.encode(&mut joined, v).expect("joined encode");
        let joined = joined.into_bytes();

        // Split: the id as a number, the body as bytes that do not contain it.
        let id_from_split = packet.protocol_id(v).expect("id");
        let mut body = Writer::default();
        packet.encode_body(&mut body, v).expect("split encode");
        let body = body.into_bytes();

        assert_eq!(id_from_joined, id_from_split, "the two paths' ids");

        // The joined buffer must be exactly the id's VarInt followed by the
        // body — not merely decodable to the same thing. A body that began one
        // byte late would still decode if the field before it absorbed the
        // difference.
        let mut expected = Writer::default();
        expected.write_var_int(id_from_split as i32);
        expected.write_slice(&body);
        assert_eq!(joined, expected.into_bytes(), "joined bytes vs id + body");

        // Cross the paths: split-decode the joined bytes' body, joined-decode
        // the split bytes with an id put back on the front.
        let mut reader = Reader::new(&body);
        let from_split = $module::Packet::decode_body(id_from_split as i32, &mut reader, v)
            .expect("decode_body");
        assert_eq!(from_split, packet, "decode_body of encode_body");

        let mut rejoined = Writer::default();
        rejoined.write_var_int(id_from_split as i32);
        rejoined.write_slice(&body);
        let rejoined = rejoined.into_bytes();
        let mut reader = Reader::new(&rejoined);
        let from_joined = $module::Packet::decode(&mut reader, v).expect("decode");
        assert_eq!(from_joined, packet, "decode of a rejoined split encode");
    }};
}

#[test]
fn the_handshake_encodes_the_same_bytes_either_way() {
    agrees!(
        hs,
        hs::Intention {
            protocol_version: VarInt(767),
            server_address: BoundedString::new("dust.example.com").expect("fits"),
            server_port: 25565,
            next_state: NextState::Status,
        }
    );
}

#[test]
fn a_status_request_with_no_fields_at_all_still_agrees() {
    // The zero-field case is the one where a body-length mistake hides: there
    // is no field for a stray byte to land in, so only a byte-for-byte
    // comparison notices.
    agrees!(ss, ss::StatusRequest {});
}

#[test]
fn a_ping_and_its_pong_agree_in_both_directions() {
    agrees!(ss, ss::PingRequest { payload: -1 });
    agrees!(sc, sc::PongResponse { payload: i64::MIN });
}

#[test]
fn a_status_response_body_carries_no_id_of_its_own() {
    agrees!(
        sc,
        sc::StatusResponse {
            json: ProtocolString::new(r#"{"description":{"text":"Dust"}}"#).expect("fits"),
        }
    );
}

#[test]
fn a_negative_id_is_refused_rather_than_wrapped() {
    // decode_body takes the id as a signed VarInt because that is what the
    // wire type is. Reading it as unsigned would turn -1 into 4294967295 and
    // then report "unknown packet 4294967295", which names a number nobody
    // sent.
    let mut reader = Reader::new(&[]);
    let err = ss::Packet::decode_body(-1, &mut reader, version::V1_21_1)
        .expect_err("a negative id is malformed");
    assert!(
        format!("{err}").contains("packet id"),
        "the error must name the field: {err}"
    );
    assert_eq!(reader.remaining(), 0, "nothing was consumed to find out");
}

//! `dust-net`'s hand-parsed login packets, checked against `dust-protocol`'s
//! definitions of the same packets.
//!
//! # The bug this file exists because of
//!
//! `login_flow` reads and writes the login conversation's packets by hand, with
//! its own string reader and its own ids. `dust-protocol` also defines those
//! packets, from Mojang's own report and a decompile. Nothing tied the two
//! together, and they disagreed: this crate believed Login Start carried an
//! optional profile id behind a presence flag — true in 1.20.2 through 1.20.4,
//! and wrong since 1.20.5 — while `dust-protocol`'s definition had the field
//! mandatory and unprefixed, with a comment beside it warning about exactly
//! that memory.
//!
//! Both crates' test suites were green. This crate's tests built Login Start
//! the way this crate parsed it, so they agreed with the code rather than with
//! Minecraft, and no real client could have completed a login.
//!
//! # What is asserted, and what is not
//!
//! Not "these round-trip". A round trip through one implementation passes under
//! any self-consistent convention including a wrong one, which is the whole
//! reason the disagreement survived. What is asserted is that bytes built the
//! way this crate builds them are read by *the other crate's* decoder as the
//! same values, and vice versa — a claim about two implementations agreeing,
//! which neither can satisfy alone.
//!
//! `dust-protocol`'s definitions are the reference half because they are
//! generated ids plus layouts read off a decompile, and because the packet ids
//! in them are checked against Mojang's report by that crate's own tests.

use dust_net::login_flow::{
    ENCRYPTION_REQUEST_ID, ENCRYPTION_RESPONSE_ID, LOGIN_ACKNOWLEDGED_ID, LOGIN_DISCONNECT_ID,
    LOGIN_START_ID, LOGIN_SUCCESS_ID, PROFILE_ID_BYTES, SET_COMPRESSION_ID,
};
use dust_protocol::packets::login::{clientbound as cb, serverbound as sb};
use dust_protocol::version::V1_21_1;
use dust_protocol::{ConnectionState, Direction};

/// Every id this crate hard-codes must be the id the generated table gives that
/// packet in 1.21.1.
///
/// The constants are hard-coded because the login conversation is written
/// against one version's numbering, and that is defensible — but only while
/// somebody checks. A renumbered packet in a future version turns this red
/// instead of turning a login into a decode error nobody can read.
#[test]
fn the_hard_coded_ids_are_the_ids_the_generated_table_gives() {
    let expect = |direction: Direction, name: &str, id: i32| {
        let actual = V1_21_1
            .protocol_id(ConnectionState::Login, direction, name)
            .unwrap_or_else(|| panic!("{name} is not in the 1.21.1 login table"));
        assert_eq!(
            actual as i32, id,
            "{name} is id {actual} in the table and {id} in login_flow"
        );
    };

    expect(Direction::Serverbound, "minecraft:hello", LOGIN_START_ID);
    expect(
        Direction::Serverbound,
        "minecraft:key",
        ENCRYPTION_RESPONSE_ID,
    );
    expect(
        Direction::Serverbound,
        "minecraft:login_acknowledged",
        LOGIN_ACKNOWLEDGED_ID,
    );
    expect(
        Direction::Clientbound,
        "minecraft:login_disconnect",
        LOGIN_DISCONNECT_ID,
    );
    expect(
        Direction::Clientbound,
        "minecraft:hello",
        ENCRYPTION_REQUEST_ID,
    );
    expect(
        Direction::Clientbound,
        "minecraft:game_profile",
        LOGIN_SUCCESS_ID,
    );
    expect(
        Direction::Clientbound,
        "minecraft:login_compression",
        SET_COMPRESSION_ID,
    );
}

/// Login Start's body, as this crate expects to receive it, decoded by the
/// other crate's definition.
///
/// This is the assertion that would have caught the bug. The body below is
/// built the way a client builds one — a name, then sixteen raw bytes — and if
/// `dust-net` went back to expecting a presence flag, the body it expected
/// would fail to decode here.
#[test]
fn login_start_is_a_name_and_a_mandatory_unprefixed_profile_id() {
    let name = "Steve";
    let profile_id = [0x42u8; PROFILE_ID_BYTES];

    let mut body = Vec::new();
    body.push(name.len() as u8); // a VarInt, and five characters fits in one byte
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&profile_id);

    let mut reader = dust_protocol::wire::Reader::new(&body);
    let packet = sb::Packet::decode_body(LOGIN_START_ID, &mut reader, V1_21_1)
        .expect("dust-protocol must read the body dust-net expects");
    let sb::Packet::Hello(hello) = packet else {
        panic!("id {LOGIN_START_ID} must be Login Start");
    };
    assert_eq!(hello.name.as_str(), name);
    assert_eq!(hello.profile_id.0, u128::from_be_bytes(profile_id));

    // And the shapes vanilla refuses must be refused here too. Each of these
    // was accepted by this crate before the fix, and each was checked against a
    // running 1.21.1 server, which answered with a decode error.
    let mut without_id = Vec::new();
    without_id.push(name.len() as u8);
    without_id.extend_from_slice(name.as_bytes());
    let mut reader = dust_protocol::wire::Reader::new(&without_id);
    sb::Packet::decode_body(LOGIN_START_ID, &mut reader, V1_21_1)
        .expect_err("a bare name has been malformed since 1.20.5");

    let mut with_flag = Vec::new();
    with_flag.push(name.len() as u8);
    with_flag.extend_from_slice(name.as_bytes());
    with_flag.push(1); // the presence flag that no longer exists
    with_flag.extend_from_slice(&profile_id);
    let mut reader = dust_protocol::wire::Reader::new(&with_flag);
    sb::Packet::decode_body(LOGIN_START_ID, &mut reader, V1_21_1)
        .expect_err("the pre-1.20.5 flagged shape is one byte too long");
}

/// Login Acknowledged carries nothing, and this crate refuses a body on it.
#[test]
fn login_acknowledged_has_no_body_at_all() {
    let mut reader = dust_protocol::wire::Reader::new(&[]);
    let packet = sb::Packet::decode_body(LOGIN_ACKNOWLEDGED_ID, &mut reader, V1_21_1)
        .expect("an empty body is the whole packet");
    assert!(matches!(packet, sb::Packet::LoginAcknowledged(_)));

    let mut reader = dust_protocol::wire::Reader::new(&[0x00]);
    sb::Packet::decode_body(LOGIN_ACKNOWLEDGED_ID, &mut reader, V1_21_1)
        .expect_err("a trailing byte means the layout is not what was sent");
}

/// Set Compression is one VarInt, and Login Success is not.
///
/// The two are adjacent ids sent one after the other, which is the arrangement
/// where an off-by-one in either direction still produces a packet that decodes.
#[test]
fn set_compression_and_login_success_are_not_interchangeable() {
    let threshold = vec![0x80, 0x02]; // 256 as a VarInt
    let mut reader = dust_protocol::wire::Reader::new(&threshold);
    let packet = cb::Packet::decode_body(SET_COMPRESSION_ID, &mut reader, V1_21_1)
        .expect("a lone VarInt is the whole packet");
    let cb::Packet::LoginCompression(body) = packet else {
        panic!("id {SET_COMPRESSION_ID} must be Set Compression");
    };
    assert_eq!(body.threshold.0, 256);

    // The same bytes read as Login Success must fail: that packet starts with
    // sixteen bytes of uuid and these are two.
    let mut reader = dust_protocol::wire::Reader::new(&threshold);
    cb::Packet::decode_body(LOGIN_SUCCESS_ID, &mut reader, V1_21_1)
        .expect_err("two bytes cannot be a game profile");
}

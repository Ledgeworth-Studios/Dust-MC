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

/// The bodies this crate *writes* must decode under `dust-protocol`'s
/// definitions.
///
/// The reading half of this file was written after a defect in how `dust-net`
/// parsed Login Start. It did not cover the other direction, and a defect
/// promptly appeared there too: Login Success went out without its final
/// `strict_error_handling` byte, so the packet ended one byte early and a real
/// client could not log in. Nothing in this crate noticed, because every test
/// here read that packet the same way this crate wrote it.
///
/// So this drives a real login over a duplex and decodes every clientbound
/// frame it produces with the other crate's decoder. It is the same argument as
/// the reading half: neither implementation can satisfy it alone.
mod written {
    use dust_net::frame::Frame;
    use dust_net::io::{Conn, ConnConfig};
    use dust_net::login_flow::{
        canonical_username, offline_profile_id, LoginConfig, LoginHandler, PROFILE_ID_BYTES,
    };
    use dust_net::session::{JoinRequest, Profile, SessionError, SessionServer};
    use dust_protocol::packets::login::clientbound as cb;
    use dust_protocol::version::V1_21_1;
    use dust_protocol::wire::{Reader, WireRead as _};
    use std::time::Duration;

    struct NoMojang;

    impl SessionServer for NoMojang {
        async fn join(&self, _request: JoinRequest<'_>) -> Result<(), SessionError> {
            unreachable!("offline mode never asks")
        }
        async fn has_joined(
            &self,
            _username: &str,
            _hash: &str,
        ) -> Result<Option<Profile>, SessionError> {
            unreachable!("offline mode never asks")
        }
    }

    fn short_clocks() -> ConnConfig {
        ConnConfig {
            timeouts: dust_net::io::Timeouts {
                idle: Some(Duration::from_secs(10)),
                pre_auth_budget: Some(Duration::from_secs(10)),
            },
            ..ConnConfig::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_clientbound_login_frame_decodes_under_the_definitions() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());

        // Handshake: next state 2, login.
        let mut intention = Vec::new();
        intention.push(0x8f); // protocol 767 as a VarInt
        intention.push(0x06);
        let host = b"localhost";
        intention.push(host.len() as u8);
        intention.extend_from_slice(host);
        intention.extend_from_slice(&25565u16.to_be_bytes());
        intention.push(2);
        client
            .send(Frame::new(0x00, intention))
            .await
            .expect("send the handshake");
        let handshake = server
            .next_frame()
            .await
            .expect("read")
            .expect("a handshake");
        server.handshake(2).expect("apply");
        assert_eq!(handshake.id, 0x00);

        // Login Start.
        let mut start = Vec::new();
        let name = "Steve";
        start.push(name.len() as u8);
        start.extend_from_slice(name.as_bytes());
        start.extend_from_slice(&[0x11; PROFILE_ID_BYTES]);
        client.send(Frame::new(0x00, start)).await.expect("send");

        let driver = tokio::spawn(async move {
            let outcome = LoginHandler::new(&mut server, LoginConfig::offline(), &NoMojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        // Set Compression, then Login Success — every frame decoded by the
        // other crate, which is the whole point.
        let compression = client.next_frame().await.expect("read").expect("frame");
        let mut reader = Reader::new(&compression.body);
        let packet = cb::Packet::decode_body(compression.id, &mut reader, V1_21_1)
            .expect("set_compression decodes");
        assert!(matches!(packet, cb::Packet::LoginCompression(_)));
        assert_eq!(reader.remaining(), 0, "and is the whole body");
        client.set_compression(dust_net::frame::Compress::At { threshold: 256 });

        let success = client.next_frame().await.expect("read").expect("frame");
        let mut reader = Reader::new(&success.body);
        let packet = cb::Packet::decode_body(success.id, &mut reader, V1_21_1)
            .expect("login_finished decodes; a missing trailing field fails here");
        assert_eq!(
            reader.remaining(),
            0,
            "and is the whole body — a field written twice would leave bytes"
        );
        let cb::Packet::GameProfile(profile) = packet else {
            panic!("id {} must be Login Success", success.id);
        };
        assert_eq!(profile.username.as_str(), name);
        assert_eq!(
            profile.uuid.0.to_be_bytes(),
            offline_profile_id(&canonical_username(name).expect("legal"))
        );

        client
            .send(Frame::new(0x03, Vec::new()))
            .await
            .expect("acknowledge");
        let (outcome, _server) = driver.await.expect("no panic");
        outcome.expect("the login completed");
    }
}

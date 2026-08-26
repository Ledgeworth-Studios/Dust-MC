//! The session-server client, against recorded wire bytes.
//!
//! [`dust_net::session`] speaks two protocols at once — HTTP/1.1 and
//! Mojang's JSON shapes — and both are pinned here the same way the frame
//! codec is pinned to published vectors: by feeding it what the real peer
//! actually sends and asserting exactly what comes back. The transport seam
//! hands over raw bytes, so a fixture *is* a recording: status line, headers,
//! body, byte for byte, captured from responses shaped like Mojang's. No test
//! in this file opens a socket, which is the property that keeps them exact
//! and CI-friendly; the TLS transport behind the `tls` feature is the one
//! piece these fixtures cannot reach, by construction.
//!
//! The negative recordings matter as much as the positive ones. A captive
//! portal answering every host with an HTML page, a proxy speaking chunked
//! encoding, a body cut short mid-flight — each has a recording here, and each
//! must land in [`SessionError::Malformed`] or [`SessionError::Transport`],
//! never in "no such player", which would silently reject an innocent login.

use std::sync::{LockResult, Mutex, MutexGuard};

use dust_net::session::{
    HttpSessionServer, JoinRequest, ProfileId, RawTransport, SessionError, SessionServer,
    SESSION_HOST, SESSION_PORT,
};

/// Plays back recorded answers, in order, and keeps every request it was
/// handed.
#[derive(Debug, Default)]
struct Scripted(Mutex<Inner>);

#[derive(Debug, Default)]
struct Inner {
    /// One entry per expected exchange, handed back verbatim.
    answers: Vec<Result<Vec<u8>, SessionError>>,
    /// Requests as they arrived, so tests can assert the wire shape.
    requests: Vec<Vec<u8>>,
}

impl Scripted {
    /// A script with a single answer.
    fn playing(answer: &[u8]) -> Self {
        Self::scripted_with(vec![Ok(answer.to_vec())])
    }

    /// A script whose only exchange fails at the transport level.
    fn failing(reason: &str) -> Self {
        Self::scripted_with(vec![Err(SessionError::Transport {
            reason: reason.to_owned(),
        })])
    }

    fn scripted_with(answers: Vec<Result<Vec<u8>, SessionError>>) -> Self {
        Self(Mutex::new(Inner {
            answers,
            requests: Vec::new(),
        }))
    }

    /// The most recent request, as text. HTTP requests are ASCII by contract
    /// here, so a decode failure is itself worth panicking about.
    fn last_request(&self) -> String {
        let inner = self.lock();
        String::from_utf8(inner.requests.last().expect("a request was made").clone())
            .expect("requests are text")
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.0.lock() {
            LockResult::Ok(guard) => guard,
            LockResult::Err(poisoned) => poisoned.into_inner(),
        }
    }
}

// The client owns its transport, so the tests hand over a shared reference
// and keep the recording half themselves. This forwarding impl is what makes
// `HttpSessionServer<Scripted>` and `HttpSessionServer<&Scripted>` the same
// conversation.
impl RawTransport for Scripted {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, SessionError> {
        let mut inner = self.lock();
        inner.requests.push(request.to_vec());
        inner.answers.remove(0)
    }
}

// A shared reference plays the same script, so a test can hold the recording
// half while the client owns its transport.
impl RawTransport for &Scripted {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, SessionError> {
        (**self).exchange(request).await
    }
}

/// A valid-looking profile id for fixtures: 32 lowercase hex digits.
const PROFILE_HEX: &str = "853c80ef3c3749fdaa49938b674adae6";

/// The login digest of the published `jeb_` vector, as a realistic value.
const DIGEST: &str = "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1";

/// A `hasJoined` success, shaped like the real answer: id, name, and a signed
/// textures property whose values are base64 blobs of plausible size.
const HAS_JOINED_OK: &[u8] = b"HTTP/1.1 200 OK\r\n\
Content-Type: application/json; charset=utf-8\r\n\
Content-Length: 429\r\n\
Connection: keep-alive\r\n\
\r\n\
{\"id\":\"853c80ef3c3749fdaa49938b674adae6\",\"name\":\"Notch\",\"properties\":\
[{\"name\":\"textures\",\"value\":\"ew0KICAgICJ0aW1lc3RhbXAiOjE2NzAwMDAwMDAsDQogICAgInByb2ZpbGVJZCI6Ijg1M2M4MGVmM2MzNzQ5ZmRhYTQ5OTM4YjY3NGFkYWU2IiwNCiAgICAicHJvZmlsZU5hbWUiOiJOb3RjaCIsDQogICAgInRleHR1cmVzIjp7DQogICAgICAgICJTUElOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L2pvaG5kb2UucG5nIn19fQ==\",\"signature\":\"c2lnbmF0dXJlIG92ZXIgYSBsb25nIGJhc2U2NCBibG9i\"}]}\r\n";

/// A join success is famously empty: 204, no body, nothing else promised.
const JOIN_NO_CONTENT: &[u8] = b"HTTP/1.1 204 No Content\r\nServer: Jetty\r\n\r\n";

/// What a captive portal or an intercepting proxy looks like from inside.
const PORTAL_HTML: &[u8] =
    b"HTTP/1.1 302 Found\r\nLocation: https://portal.example/\r\n\r\n<html>sign in</html>";

// ---------------------------------------------------------------------------
// The wire shape of the requests themselves.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_join_request_is_exactly_the_documented_shape() {
    let script = Scripted::playing(JOIN_NO_CONTENT);
    let id = ProfileId::parse(PROFILE_HEX).expect("fixture id");
    let server = HttpSessionServer::new(&script);
    let joined = server
        .join(JoinRequest {
            access_token: "token.abc123",
            profile_id: &id,
            server_id_hash: DIGEST,
        })
        .await;
    assert!(joined.is_ok(), "204 means recorded: {joined:?}");

    let sent = script.last_request();
    // Line by line, because the whole point is the exact shape: method and
    // target, the authority, the JSON content type, an honest length, and a
    // connection this client does not try to reuse.
    assert!(
        sent.starts_with("POST /session/minecraft/join HTTP/1.1\r\n"),
        "{sent}"
    );
    assert!(
        sent.contains(&format!("Host: {SESSION_HOST}\r\n")),
        "{sent}"
    );
    assert!(
        sent.contains("Content-Type: application/json\r\n"),
        "{sent}"
    );
    assert!(sent.contains("Connection: close\r\n"), "{sent}");

    // The three parameters, spelled as Mojang documents them, in the order
    // first-party launchers send them.
    let expected_body = format!(
        "{{\"accessToken\":\"token.abc123\",\
         \"selectedProfile\":\"{PROFILE_HEX}\",\
         \"serverId\":\"{DIGEST}\"}}"
    );
    assert!(
        sent.contains(&format!("Content-Length: {}\r\n", expected_body.len())),
        "{sent}"
    );
    let body = sent.split("\r\n\r\n").nth(1).expect("body after headers");
    assert_eq!(body, expected_body);
}

#[tokio::test]
async fn the_hasjoined_query_encodes_its_parameters() {
    let script = Scripted::playing(b"HTTP/1.1 204 No Content\r\n\r\n");
    let server = HttpSessionServer::new(&script);
    // A name with a space and non-ASCII, and a digest that happens to start
    // with its minus sign: neither may travel raw in a query string.
    server
        .has_joined("not ch\u{e9}", "-abc")
        .await
        .expect("answered");

    let sent = script.last_request();
    assert!(
        sent.starts_with(
            "GET /session/minecraft/hasJoined?username=not%20ch%C3%A9&serverId=-abc HTTP/1.1\r\n"
        ),
        "{sent}"
    );
    assert!(
        sent.contains(&format!("Host: {SESSION_HOST}\r\n")),
        "{sent}"
    );
}

#[tokio::test]
async fn the_endpoint_is_https_and_nothing_else_is_offered() {
    // Spelled out because "no plaintext variant" is a security claim, and a
    // claim someone can check beats one they must take on faith.
    assert_eq!(SESSION_PORT, 443);
}

// ---------------------------------------------------------------------------
// The recorded answers, good and bad.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recorded_hasjoined_success_parses_into_a_profile() {
    let server = HttpSessionServer::new(Scripted::playing(HAS_JOINED_OK));
    let profile = server
        .has_joined("Notch", DIGEST)
        .await
        .expect("recorded answer parses")
        .expect("200 with a body is a profile");

    assert_eq!(profile.id.as_str(), PROFILE_HEX);
    assert_eq!(profile.name, "Notch");
    assert_eq!(profile.properties.len(), 1);
    let textures = &profile.properties[0];
    assert_eq!(textures.name, "textures");
    // Pass-through means byte-exact: the value is base64 this layer must not
    // decode, so the assertion is equality with what was recorded, not any
    // interpretation of what it decodes to.
    assert_eq!(
        textures.value,
        "ew0KICAgICJ0aW1lc3RhbXAiOjE2NzAwMDAwMDAsDQogICAgInByb2ZpbGVJZCI6Ijg1M2M4MGVmM2MzNzQ5ZmRhYTQ5OTM4YjY3NGFkYWU2IiwNCiAgICAicHJvZmlsZU5hbWUiOiJOb3RjaCIsDQogICAgInRleHR1cmVzIjp7DQogICAgICAgICJTUElOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L2pvaG5kb2UucG5nIn19fQ=="
    );
    assert!(textures.signature.is_some(), "the signature rides along");
}

#[tokio::test]
async fn a_204_from_hasjoined_means_nobody_and_is_an_answer() {
    let server = HttpSessionServer::new(Scripted::playing(b"HTTP/1.1 204 No Content\r\n\r\n"));
    let outcome = server
        .has_joined("impostor", "0000")
        .await
        .expect("answered");
    assert_eq!(outcome, None, "absence is not an error; it is the verdict");
}

#[tokio::test]
async fn a_refusal_reports_the_status_instead_of_inventing_one() {
    let refused = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 39\r\n\r\n{\"error\":\"ForbiddenOperationException\"}";
    let server = HttpSessionServer::new(Scripted::playing(refused));
    let outcome = server.has_joined("x", "y").await;
    assert!(
        matches!(outcome, Err(SessionError::Rejected { status: 403 })),
        "{outcome:?}"
    );

    // Join refuses through the same door: an expired token is a 403 there.
    let script = Scripted::playing(b"HTTP/1.1 403 Forbidden\r\n\r\n");
    let id = ProfileId::parse(PROFILE_HEX).expect("fixture id");
    let server = HttpSessionServer::new(&script);
    let outcome = server
        .join(JoinRequest {
            access_token: "expired",
            profile_id: &id,
            server_id_hash: "digest",
        })
        .await;
    assert!(
        matches!(outcome, Err(SessionError::Rejected { status: 403 })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_broken_session_server_is_its_own_kind_of_failure() {
    let down = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
    let server = HttpSessionServer::new(Scripted::playing(down));
    let outcome = server.has_joined("Notch", "digest").await;
    assert!(
        matches!(outcome, Err(SessionError::Unavailable { status: 503 })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_portal_or_proxy_answer_is_malformed_not_a_verdict_on_the_player() {
    // The redirect carries a body but no usable contract: following it is how
    // credentials leak to whoever redirected us, and reporting "no such
    // player" would fail an honest login quietly.
    let server = HttpSessionServer::new(Scripted::playing(PORTAL_HTML));
    let outcome = server.has_joined("Notch", "digest").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn garbage_json_names_what_was_wrong() {
    let answer =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"id\":true}";
    let server = HttpSessionServer::new(Scripted::playing(answer));
    let message = match server.has_joined("Notch", "digest").await {
        Err(error @ SessionError::Malformed { .. }) => error.to_string(),
        other => panic!("expected malformed, got {other:?}"),
    };
    assert!(message.contains("hasJoined"), "{message}");
}

// ---------------------------------------------------------------------------
// Profile parsing, field by field.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_profile_id_that_the_wire_cannot_carry_is_refused() {
    for bad in [
        "069a79f4-44e9-4726-a5be-fca90e38aaf5", // dashed display form
        "853C80EF3C3749FDAA49938B674ADAE6",     // uppercase hex
        "853c80ef3c3749fdaa49938b674adae",      // 31 digits
        "853c80ef3c3749fdaa49938b674adaeg",     // g is not hex
        "",                                     // nothing at all
    ] {
        assert!(ProfileId::parse(bad).is_err(), "{bad:?} should not parse");
    }
    assert!(ProfileId::parse(PROFILE_HEX).is_ok());
}

#[tokio::test]
async fn a_profile_without_an_id_or_a_name_is_not_a_profile() {
    for body in [
        r#"{"name":"Notch"}"#,                          // no id
        r#"{"id":"853c80ef3c3749fdaa49938b674adae6"}"#, // no name
        r#"{"id":853,"name":"Notch"}"#,                 // id is not a string
        r#"[]"#,                                        // not even an object
    ] {
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let server = HttpSessionServer::new(Scripted::playing(answer.as_bytes()));
        let outcome = server.has_joined("Notch", "digest").await;
        assert!(
            matches!(outcome, Err(SessionError::Malformed { .. })),
            "{body} parsed as {outcome:?}"
        );
    }
}

#[tokio::test]
async fn properties_pass_through_signed_or_unsigned() {
    // An unsigned profile: no properties key at all, which older accounts and
    // some proxy setups produce. Headers folded into the fixture bytes rather
    // than escaped line continuations, so the lengths stay countable.
    let unsigned: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Length: 60\r\n\r\n{\"id\":\"853c80ef3c3749fdaa49938b674adae6\",\"name\":\"Herobrine\"}";
    let server = HttpSessionServer::new(Scripted::playing(unsigned));
    let profile = server
        .has_joined("Herobrine", "d")
        .await
        .expect("parses")
        .expect("profile");
    assert!(profile.properties.is_empty(), "absent means none");

    // And null, which is what some gateways serialise an absent array into.
    let nulled: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 78\r\n\r\n\
{\"id\":\"853c80ef3c3749fdaa49938b674adae6\",\"name\":\"Herobrine\",\"properties\":null}";
    let server = HttpSessionServer::new(Scripted::playing(nulled));
    let profile = server
        .has_joined("Herobrine", "d")
        .await
        .expect("parses")
        .expect("profile");
    assert!(profile.properties.is_empty());

    // A property without its value half is a broken answer, whatever a
    // lenient reader could salvage from it.
    let half: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 87\r\n\r\n\
{\"id\":\"853c80ef3c3749fdaa49938b674adae6\",\"name\":\"X\",\"properties\":[{\"name\":\"textures\"}]}";
    let server = HttpSessionServer::new(Scripted::playing(half));
    let outcome = server.has_joined("X", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// HTTP-level defences.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn framing_this_client_does_not_speak_is_refused_by_name() {
    let chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n21\r\n{\"id\":...}\r\n0\r\n\r\n";
    let server = HttpSessionServer::new(Scripted::playing(chunked));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn conflicting_content_lengths_are_smuggling_and_are_refused() {
    let split = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 999\r\n\r\nhello";
    let server = HttpSessionServer::new(Scripted::playing(split));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn an_answer_promising_more_than_the_ceiling_stops_at_the_door() {
    // Declared length beyond the cap: refused without the body mattering.
    let lying = b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\nsmall but lying about it";
    let server = HttpSessionServer::new(Scripted::playing(lying));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );

    // And an actual oversized body with an honest length is refused too:
    // the ceiling bounds the answer, not merely the claim.
    let big_body = vec![b'a'; dust_net::session::MAX_RESPONSE_BODY + 1];
    let mut honest = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        big_body.len()
    )
    .into_bytes();
    honest.extend_from_slice(&big_body);
    let server = HttpSessionServer::new(Scripted::playing(&honest));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn an_answer_without_http_headers_is_not_negotiated_with() {
    // HTTP/0.9 replies with bare bodies; accepting one would accept whatever
    // text a middlebox felt like emitting.
    let server = HttpSessionServer::new(Scripted::playing(b"<html>blocked</html>\r\n"));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_transport_failure_arrives_as_itself_and_nothing_else() {
    // Whatever went wrong below the HTTP layer travels verbatim: the caller
    // sees the transport's own words, not a reinterpretation.
    let server = HttpSessionServer::new(Scripted::failing("connect failed: timed out"));
    let outcome = server.has_joined("Notch", "digest").await;
    assert!(
        matches!(&outcome,
            Err(SessionError::Transport { reason }) if reason.contains("timed out")),
        "{outcome:?}"
    );

    // A truncated stream — headers promising more than arrived — is the same
    // kind of broken, even though something did come back.
    let short = b"HTTP/1.1 200 OK\r\nContent-Length: 400\r\n\r\nshort";
    let server = HttpSessionServer::new(Scripted::playing(short));
    let outcome = server.has_joined("Notch", "d").await;
    assert!(
        matches!(outcome, Err(SessionError::Malformed { .. })),
        "{outcome:?}"
    );
}

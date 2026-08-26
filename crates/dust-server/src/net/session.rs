//! One connection's conversation, from its first byte to its last.
//!
//! # Where this sits
//!
//! `dust-net` owns the socket, the framing, the compression, the encryption and
//! the state machine's *legality*; `dust-protocol` owns what the bytes inside a
//! frame mean. Neither knows what this server wants to say. This module is that
//! third thing, and it is the first code in the workspace that holds both
//! halves at once — which is why the Frame-to-Packet bridge below is written
//! here and not in either crate.
//!
//! # Every path through this function ends the connection
//!
//! A status ping is two round trips and then the client hangs up; a refused
//! login is one packet and a close. Nothing here loops waiting for more work,
//! and that is deliberate for as long as Play is unimplemented: a connection
//! that this server cannot serve should stop costing it something. The moment
//! Play lands, this becomes the pre-Play half and hands a live connection on.
//!
//! # A stranger is on the other end
//!
//! Everything reachable here is reachable before anybody has authenticated.
//! The frame limits, the idle timeout and the pre-authentication budget are
//! `dust-net`'s and are already applied underneath; what this module adds is
//! the rule that it never allocates anything sized by what the peer said, and
//! never answers a packet that does not belong in the state the connection is
//! actually in.

use std::sync::Arc;
use std::time::Duration;

use dust_net::frame::Frame;
use dust_net::io::{Conn, ConnConfig, ConnError};
use dust_net::login::ServerKey;
use dust_net::login_flow::{LoginConfig, LoginHandler};
use dust_net::session::{
    HttpSessionServer, JoinRequest, Profile, SessionError as NetSessionError, SessionServer,
    TlsTransport,
};
use dust_net::state::{Intent, State};
use dust_protocol::packets::{configuration, handshake, status};
use dust_protocol::text::{Color, Component, NamedColor};
use dust_protocol::types::ProtocolString;
use dust_protocol::wire::{Reader, Writer};
use dust_protocol::ProtocolVersion;
use tokio::io::{AsyncRead, AsyncWrite};

use super::status::StatusPolicy;

/// What one connection turned out to be, once it ended.
///
/// Returned rather than logged from inside, so the caller decides the log level
/// — a status ping is `debug` on a busy server and `info` on a quiet one, and
/// that is not this function's call to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    /// A server-list ping, answered. Whether the client also asked for the
    /// round-trip measurement is recorded, because a scanner asks for the JSON
    /// and hangs up while a real client always pings.
    Status { pinged: bool },
    /// A player logged in successfully and was then told the world is not
    /// ready. The identity is carried out because it is the thing the login
    /// existed to establish, and a log line that omitted it would leave an
    /// operator unable to tell one attempt from another.
    LoggedIn {
        username: String,
        profile_id: [u8; 16],
    },
    /// A login attempt that did not produce an identity. The reason is the one
    /// the client was given, so the log and the player's screen agree.
    LoginFailed { reason: String },
    /// The peer disconnected before saying what it wanted.
    NothingAsked,
}

/// Everything a connection needs that is the same for all of them.
///
/// One allocation per server rather than per connection, shared by `Arc`
/// because a connection outlives the accept loop's stack frame.
#[derive(Debug)]
pub struct SessionContext {
    pub version: ProtocolVersion,
    pub status: StatusPolicy,
    pub conn: ConnConfig,
    pub auth: Authority,
}

/// How this server decides who a joining player is.
///
/// The two arms hold different things because they need different things, and
/// that is the point of the enum over a bool beside two options: an online-mode
/// server without a key or without a session server is not a configuration, it
/// is a bug, and this type makes it one that cannot be built. `[server]
/// online_mode = true` with no way to reach Mojang would otherwise become a
/// server that quietly admitted anybody under any name — the failure the
/// setting exists to prevent.
pub enum Authority {
    /// Verify nothing. Anyone may claim any name, and the profile id is
    /// derived from it.
    Offline,
    /// Verify against Mojang. The key is generated once at boot, because RSA
    /// key generation has no upper bound on its running time and does not
    /// belong on a login path.
    Online {
        session: Arc<HttpSessionServer<TlsTransport>>,
        key: Arc<ServerKey>,
    },
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Neither the key nor the trust store belongs in a log line, and the
        // only thing a reader wants here is which regime is running.
        f.write_str(match self {
            Self::Offline => "Offline",
            Self::Online { .. } => "Online",
        })
    }
}

/// Errors this layer adds on top of `dust-net`'s.
#[derive(Debug)]
pub enum SessionError {
    /// The transport failed or the peer was refused by the codec.
    Conn(ConnError),
    /// A frame decoded as no packet this state and direction has.
    Protocol(dust_protocol::wire::DecodeError),
    /// A packet this server has to send would not encode.
    Encode(dust_protocol::wire::EncodeError),
    /// A well-formed packet arrived where the protocol does not allow it.
    ///
    /// Distinct from `Protocol` on purpose: that one means "these bytes are not
    /// a packet", this one means "this packet, here, is a protocol violation",
    /// and the two want different responses from an operator reading a log.
    OutOfTurn {
        state: &'static str,
        packet: &'static str,
    },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conn(e) => write!(f, "connection: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
            Self::OutOfTurn { state, packet } => {
                write!(f, "{packet} is not allowed in the {state} state")
            }
        }
    }
}

impl std::error::Error for SessionError {}

impl From<dust_net::state::HandshakeError> for SessionError {
    /// A handshake naming a state that does not exist is `dust-net`'s to
    /// refuse, and it arrives here as a connection-level failure because that
    /// is what it is: the connection has no legal next state and there is
    /// nothing to answer with.
    fn from(e: dust_net::state::HandshakeError) -> Self {
        Self::Conn(ConnError::from(e))
    }
}

impl From<ConnError> for SessionError {
    fn from(e: ConnError) -> Self {
        Self::Conn(e)
    }
}

impl From<dust_protocol::wire::DecodeError> for SessionError {
    fn from(e: dust_protocol::wire::DecodeError) -> Self {
        Self::Protocol(e)
    }
}

impl From<dust_protocol::wire::EncodeError> for SessionError {
    fn from(e: dust_protocol::wire::EncodeError) -> Self {
        Self::Encode(e)
    }
}

/// Turn a packet into a frame without writing its id into its own body.
///
/// The two crates keep the id in different places on purpose — `dust-net`'s
/// `Frame` holds it as a number because the framer has to read it to find the
/// body, and `dust-protocol` holds it as a lookup because the number depends on
/// the version. Writing it twice would give the framer two ids and no rule
/// about which one wins.
macro_rules! to_frame {
    ($packet:expr, $version:expr) => {{
        let packet = $packet;
        let mut body = Writer::default();
        packet.encode_body(&mut body, $version)?;
        Frame::new(packet.protocol_id($version)? as i32, body.into_bytes())
    }};
}

/// How long a graceful close may spend flushing before it is abandoned.
///
/// `Conn::close` is *willing to wait*, which is the right default and the wrong
/// one here: a peer that stops reading can hold a flush open for as long as it
/// likes, and every one of these connections belongs to a stranger. Five
/// seconds is far beyond any real client's need for a few hundred bytes on a
/// socket it is actively reading, and short enough that a peer cannot pin a
/// task by going quiet.
const CLOSE_LINGER: Duration = Duration::from_secs(5);

/// End a connection, flushing what is queued if the peer will take it.
///
/// Dropping a `Conn` **aborts** it — that is `dust-net`'s documented contract,
/// and it is the right default, because a caller that wanted the flush
/// guarantee had `close` for it. It also means that returning from this module
/// with a reply still in the outbound queue sends the peer nothing at all,
/// which is exactly the bug this function exists to make unrepresentable: every
/// path that sent something goes through here.
///
/// On timeout the close future is dropped, which drops the `Conn`, which sets
/// the abort flag. The fallback needs no code of its own.
async fn finish<W>(conn: Conn<W>, outcome: Served) -> Result<Served, SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match tokio::time::timeout(CLOSE_LINGER, conn.close()).await {
        // A peer that hung up mid-flush is the ordinary end of a status ping,
        // not a failure worth reporting: the client got what it asked for and
        // stopped listening. The outcome stands.
        Ok(Ok(())) | Err(_) => Ok(outcome),
        Ok(Err(ConnError::Closed)) => Ok(outcome),
        Ok(Err(e)) => Err(SessionError::Conn(e)),
    }
}

/// Serve one connection to its end.
///
/// Generic over the stream so the tests can run the whole conversation over a
/// duplex pipe in microseconds, and the real listener can hand it a socket. The
/// same code path serves both — a test that exercised a different one would be
/// testing a different server.
pub async fn serve<W>(io: W, ctx: Arc<SessionContext>) -> Result<Served, SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut conn = Conn::new(io, ctx.conn.clone());

    // ---- Handshake -------------------------------------------------------
    let Some(frame) = conn.next_frame().await? else {
        // A TCP connection that opened and closed without a byte. Common
        // enough — health checks and port scanners both do it — that it is a
        // named outcome rather than an error.
        return Ok(Served::NothingAsked);
    };
    let intention = {
        let mut reader = Reader::new(&frame.body);
        match handshake::serverbound::Packet::decode_body(frame.id, &mut reader, ctx.version)? {
            handshake::serverbound::Packet::Intention(i) => i,
        }
    };

    // The intent decides the next state, and `dust-net` decides whether that
    // transition is legal. Asking it rather than assigning the state directly
    // is what keeps one state machine in this process instead of two.
    let intent = conn.handshake(intention.next_state.discriminant())?;

    match intent {
        Intent::Status => serve_status(conn, &ctx).await,
        Intent::Login | Intent::Transfer => {
            // The client's protocol number is deliberately *not* checked here.
            // A mismatched client that asked for status has already been told
            // the server's version in the document it received, which is the
            // channel the protocol provides for saying so; a mismatched client
            // that asked to log in gets the same refusal as a matched one for
            // as long as this server has nowhere to put either. Checking it
            // here would make this look like version enforcement when the real
            // enforcement does not exist yet.
            let _ = intention.protocol_version;
            serve_login(conn, &ctx).await
        }
    }
}

/// Log a player in, then tell them the world is not ready.
///
/// # How far this goes
///
/// All the way through login — Login Start, the encryption exchange and
/// Mojang's verdict in online mode, Set Compression, Login Success, Login
/// Acknowledged — and then one step into configuration, where it stops. The
/// player's identity is established and the connection is in the state where a
/// server would send its registries; Dust has no registries to send yet, so it
/// says so and disconnects.
///
/// That is worth doing rather than skipping, because the half that exists is
/// the half with the cryptography in it. A login that is never exercised end to
/// end against a real client is a login nobody has tested.
///
/// # Why the request is read even when the answer is no
///
/// The obvious shortcut — refuse the instant the intent is known, without
/// waiting for Login Start — leaves the client's bytes unread in this end's
/// receive buffer when the socket closes. A TCP stack that closes with unread
/// data sends **RST rather than FIN**, and an RST is entitled to discard
/// whatever is still in the peer's receive buffer: on a bad day that is the
/// refusal itself, and the player sees "connection reset" — precisely the
/// message the packet existed to replace. It reproduced here on the first run,
/// as a reset where the test expected a clean end of stream. `LoginHandler`
/// reads the request before answering, which is one more reason to go through
/// it rather than around it.
async fn serve_login<W>(mut conn: Conn<W>, ctx: &SessionContext) -> Result<Served, SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let authenticated = match &ctx.auth {
        Authority::Offline => {
            LoginHandler::new(&mut conn, LoginConfig::offline(), &NoSessionServer, None)
                .authenticate()
                .await
        }
        Authority::Online { session, key } => {
            LoginHandler::new(
                &mut conn,
                LoginConfig::online(),
                session.as_ref(),
                Some(key.as_ref()),
            )
            .authenticate()
            .await
        }
    };

    let authenticated = match authenticated {
        Ok(authenticated) => authenticated,
        Err(error) => {
            // `LoginHandler` has already put the reason on the wire for every
            // failure that had a wire to put it on; what is left here is to
            // flush it and say what happened. Returning the error instead
            // would abort the connection and take the explanation with it.
            let reason = error.to_string();
            let _ = finish(
                conn,
                Served::LoginFailed {
                    reason: reason.clone(),
                },
            )
            .await;
            return Ok(Served::LoginFailed { reason });
        }
    };

    // Login Acknowledged has been received, so the connection is in
    // configuration. The disconnect below is therefore a *configuration*
    // disconnect and carries an NBT component — not the JSON one login uses.
    // The two states spell the same idea differently, which is exactly the
    // sort of thing that is invisible until a client renders nothing.
    let reason = Component::text("Dust cannot serve a world yet.")
        .colored(Color::Named(NamedColor::Red))
        .to_nbt(ctx.version)?;
    let packet =
        configuration::clientbound::Packet::from(configuration::clientbound::Disconnect { reason });
    let frame = to_frame!(packet, ctx.version);
    conn.send(frame).await?;
    conn.disconnect();

    let outcome = Served::LoggedIn {
        username: authenticated.username,
        profile_id: authenticated.profile_id,
    };
    finish(conn, outcome).await
}

/// The session server for offline mode: there isn't one.
///
/// `LoginHandler` takes a session server unconditionally because the type it
/// needs is decided at compile time, and offline mode never calls either
/// method. Both bodies are unreachable rather than unimplemented, and they say
/// so by failing loudly instead of returning something plausible — a stub that
/// answered "yes, that player is who they say" would turn a wiring mistake into
/// an authentication bypass.
struct NoSessionServer;

impl SessionServer for NoSessionServer {
    async fn join(&self, _request: JoinRequest<'_>) -> Result<(), NetSessionError> {
        Err(NetSessionError::Transport {
            reason: "offline mode never contacts the session server".to_owned(),
        })
    }

    async fn has_joined(
        &self,
        _username: &str,
        _server_id_hash: &str,
    ) -> Result<Option<Profile>, NetSessionError> {
        Err(NetSessionError::Transport {
            reason: "offline mode never contacts the session server".to_owned(),
        })
    }
}

/// The two round trips of a server-list ping.
///
/// Takes the connection by value because every exit from here ends it, and one
/// of those exits has to flush. A `&mut` would leave the ending to the caller,
/// and the caller's ending is a drop, and a drop is an abort.
async fn serve_status<W>(mut conn: Conn<W>, ctx: &SessionContext) -> Result<Served, SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut answered = false;
    loop {
        let Some(frame) = conn.next_frame().await? else {
            // The client hung up. Normal after either round trip: a client that
            // only wanted the JSON never sends a ping, and one that pinged has
            // no reason to stay.
            return Ok(Served::Status { pinged: answered });
        };
        let mut reader = Reader::new(&frame.body);
        let packet = status::serverbound::Packet::decode_body(frame.id, &mut reader, ctx.version)?;
        match packet {
            status::serverbound::Packet::StatusRequest(_) => {
                if answered {
                    // Vanilla's own server closes on a second request. Serving
                    // it again would let an unauthenticated peer ask for the
                    // document as many times as it liked on one connection,
                    // which is a cost with no matching benefit.
                    return Err(SessionError::OutOfTurn {
                        state: State::Status.name(),
                        packet: "minecraft:status_request",
                    });
                }
                // A MOTD long enough to overflow a protocol string is a
                // configuration error, and this is the wrong place to learn
                // about one — but it is checked rather than unwrapped, because
                // the alternative is a panic on a path an unauthenticated
                // stranger reaches.
                let json = ProtocolString::new(ctx.status.render(0))?;
                let response = status::clientbound::StatusResponse { json };
                let frame = to_frame!(status::clientbound::Packet::from(response), ctx.version);
                conn.send(frame).await?;
                answered = true;
            }
            status::serverbound::Packet::PingRequest(ping) => {
                // Returned unexamined. The eight bytes are the client's, used
                // to measure a round trip; reading meaning into them would be
                // reading meaning into somebody else's number.
                let pong = status::clientbound::PongResponse {
                    payload: ping.payload,
                };
                let frame = to_frame!(status::clientbound::Packet::from(pong), ctx.version);
                conn.send(frame).await?;
                // The protocol says the server closes after the pong and the
                // client expects it, which is why this close is part of the
                // message rather than the end of it — and why it has to be a
                // *graceful* one. An abort here truncates the very packet the
                // whole exchange existed to deliver.
                return finish(conn, Served::Status { pinged: true }).await;
            }
        }
    }
}

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

use dust_net::io::{Conn, ConnConfig, ConnError};
use dust_net::login::ServerKey;
use dust_net::login_flow::{LoginConfig, LoginHandler};
use dust_net::session::{
    HttpSessionServer, JoinRequest, Profile, SessionError as NetSessionError, SessionServer,
    TlsTransport,
};
use dust_net::state::{Intent, State};
use dust_protocol::packets::{configuration, handshake, play, status};
use dust_protocol::text::{Color, Component, NamedColor};
use dust_protocol::types::{ProtocolString, VarInt};
use dust_protocol::wire::Reader;
use dust_protocol::ProtocolVersion;
use dust_world::coords::ChunkPos;
use tokio::io::{AsyncRead, AsyncWrite};

use super::configure::{configure, Configured};
use super::edits::{Edit, SharedWorld};
use super::finish;
use super::play as play_mod;
use super::status::StatusPolicy;
use super::view::{self, View};
use super::world;
use crate::to_frame;

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
    /// The world a joining player is put into, with whatever players have
    /// changed in it.
    pub world: SharedWorld,
    /// How many columns out from the player are streamed on join.
    pub view_distance: u32,
    /// `minecraft:overworld`'s id in the dimension-type registry, as it was
    /// synced during configuration.
    ///
    /// Resolved at boot from the same table the sync is built from, rather
    /// than written as a number: the id is a *position* in that table, and a
    /// constant here would be a second answer to a question the sync already
    /// answers.
    pub overworld_dimension_type: u32,
    /// Shared with the accept loop. A session counts the player inside it,
    /// because only a session knows when one has arrived and when it has left.
    pub counters: std::sync::Arc<super::listen::Counters>,
    /// Air, and the one block a player can place.
    ///
    /// Resolved at boot alongside the world's own palette rather than looked
    /// up per click: `Block::from_name` is a scan over the whole block table,
    /// and a right-click is not the place for one.
    pub blocks: PlaceableBlocks,
    /// For the one thing this layer has to say that is not a packet: a session
    /// that fell behind on block changes.
    pub logger: crate::logging::Logger,
    /// Everybody currently connected, and the channel their comings and
    /// goings travel on.
    pub roster: super::players::SharedRoster,
    /// `minecraft:player`'s id in the entity-type registry, resolved at boot.
    pub player_entity_type: i32,
    /// Where each player was when they last left, by profile id.
    ///
    /// Read on join and written on leave, so a reconnecting player lands where
    /// they stood rather than at spawn. Shared rather than per-session for the
    /// obvious reason: the session that knows the position is the one that is
    /// ending.
    pub positions: super::save::SharedPositions,
}

/// The block states a session can put into the world.
#[derive(Debug, Clone, Copy)]
pub struct PlaceableBlocks {
    /// What breaking a block leaves behind.
    pub air: u32,
    /// What placing one puts down.
    ///
    /// One block, because there is no inventory and what a player is holding
    /// is not knowable here yet. Stated rather than dressed up as a choice.
    pub placeable: u32,
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

impl From<dust_net::state::IllegalTransition> for SessionError {
    /// A transition this connection was not allowed to make. Reachable only
    /// from a bug here rather than from anything a peer sent — `dust-net` owns
    /// the legality of transitions precisely so this code cannot invent one —
    /// which is why it is an error and not an assertion: the loop that catches
    /// it logs and drops one connection instead of ending the process.
    fn from(e: dust_net::state::IllegalTransition) -> Self {
        Self::Conn(ConnError::from(e))
    }
}

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
    let profile_id = authenticated.profile_id;
    let username = authenticated.username.clone();

    // Login Acknowledged has been received, so the connection is in
    // configuration. Every disconnect from here carries an NBT component, not
    // login's JSON: the two states spell one idea differently, and the wrong
    // one is a packet that travels and renders nothing.
    let outcome = match configure(&mut conn, ctx).await? {
        Configured::Ready => Served::LoggedIn {
            username: authenticated.username,
            profile_id: authenticated.profile_id,
        },
        Configured::UnknownContent => {
            // The client did not acknowledge the vanilla pack, so its
            // registries would have to carry their own contents, and Dust has
            // none to send. Saying so beats sending three hundred entries with
            // no definitions and leaving it in a world with no dimension types.
            refuse_in_configuration(
                &mut conn,
                ctx,
                "This server can only serve clients that already have \
                 Minecraft 1.21.1's own data.",
            )
            .await?;
            return finish(
                conn,
                Served::LoginFailed {
                    reason: "the client did not acknowledge minecraft:core 1.21.1".to_owned(),
                },
            )
            .await;
        }
    };

    // Configuration finished, so both ends are in Play.
    conn.transition(State::Play)?;
    serve_play(&mut conn, ctx, profile_id, &username).await?;
    conn.disconnect();
    finish(conn, outcome).await
}

/// Put the player in the world and keep them there.
///
/// Returns when the connection ends, which today means when the client
/// disconnects or a keep-alive goes unanswered.
async fn serve_play<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    profile_id: [u8; 16],
    username: &str,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let version = ctx.version;

    // Where this player was when they last left, or spawn if this is the
    // first time. Read before the join packet, because the join packet is not
    // what carries a position — the teleport below is — and a player told the
    // wrong one first would see themselves move.
    let start = ctx
        .positions
        .lock()
        .expect("the position map is never poisoned")
        .get(&profile_id)
        .copied()
        .unwrap_or(world::SPAWN);

    // Counted here rather than when the session ends, and held by a guard so
    // that every way out of this function — a disconnect, a timeout, a decode
    // error, a panic in the task — puts the number back.
    let _player = ctx.counters.player_joined();

    send_play(
        conn,
        play_mod::login_packet(
            ENTITY_ID,
            ctx.status.max_players(),
            ctx.view_distance,
            ctx.overworld_dimension_type,
        )?,
        version,
    )
    .await?;

    // Abilities before the position, as a real server sends them. This is
    // where creative flight is *granted*: the game mode in the join packet
    // does not grant it, and a client that is never sent this walks.
    send_play(conn, play_mod::abilities(true), version).await?;
    send_play(conn, play_mod::frozen_at_noon(), version).await?;
    send_play(conn, play_mod::default_spawn(world::SPAWN), version).await?;

    // Before the chunks, not after: a client uses its position to decide which
    // columns it wants, and one told about columns before it knows where it is
    // throws them away.
    send_play(
        conn,
        play_mod::position_packet(start, FIRST_TELEPORT_ID),
        version,
    )
    .await?;

    // Subscribed *before* the first column is generated, so an edit made in
    // the window between generating a chunk and starting to listen is heard
    // rather than missed. A duplicate is harmless — setting a block to the
    // state it already holds is not observable — and a miss is a wrong world.
    let mut edits = ctx.world.subscribe();

    // The view is the server's record of what this client holds. Every column
    // it sends and every one it forgets goes through it, so the record cannot
    // drift from the client's actual contents.
    let mut view = View::default();
    let centre = view::column_of(start.0, start.2);
    stream(conn, ctx, &mut view, centre).await?;

    // The terrain is there; this is what tells the client to stop looking at
    // the loading screen and start rendering it.
    send_play(
        conn,
        play::clientbound::GameEvent {
            event: play_mod::LEVEL_CHUNKS_LOAD_START,
            value: 0.0,
        },
        version,
    )
    .await?;

    // Join the roster *after* the world is on screen. The order matters the
    // same way the position does: an entity announced before the client has
    // the column it stands in is an entity the client files against nothing.
    //
    // The roster hands back the players already here and a subscription that
    // begins before this player was added, both taken under one lock — so
    // nobody who joins in between falls between the two.
    let joined = ctx.roster.join(profile_id, username.to_owned(), start);
    let me = joined.player.clone();
    let mut roster_changes = joined.listener;
    ctx.roster
        .say(SERVER_SPEAKING, super::chat::joined(&me.name));

    // Everybody already here, both halves each: the tab-list row and the body.
    for other in &joined.existing {
        send_play(conn, play_mod::player_info_add(other)?, version).await?;
        send_play(
            conn,
            play_mod::spawn_player(other, ctx.player_entity_type),
            version,
        )
        .await?;
    }

    // The loop records the position into the shared map as the player moves,
    // not when the session ends. Recording it only at the end lost it in two
    // real cases: a server stopped while somebody was connected read the map
    // before that session had finished tearing down, and a process that died
    // never wrote it at all. Twenty locks a second per player is the price of
    // the map always being right.
    let result = play_loop(
        conn,
        ctx,
        view,
        &mut edits,
        &mut roster_changes,
        &me,
        profile_id,
        start,
    )
    .await;

    // Left however the loop ended, so a session that failed does not leave a
    // body standing in the world forever. The announcement goes first, while
    // this player is still on the roster — a leave line for somebody the
    // roster no longer has is a line nobody can attribute.
    ctx.roster.say(SERVER_SPEAKING, super::chat::left(&me.name));
    ctx.roster.leave(me.entity_id);
    result
}

/// Bring the client's loaded columns in line with a new centre.
///
/// Recentre first, then send, then forget. The order matters at both ends: a
/// client filing columns against a stale centre may discard them, and one told
/// to forget a column before its replacement arrives has a hole in the world
/// for as long as the round trip takes.
async fn stream<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    centre: ChunkPos,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let change = view.move_to(centre, ctx.view_distance as i32);
    if change.recentre {
        send_play(
            conn,
            play::clientbound::SetCenterChunk {
                chunk_x: VarInt(centre.x),
                chunk_z: VarInt(centre.z),
            },
            ctx.version,
        )
        .await?;
    }
    for pos in &change.send {
        // An untouched column is the template, sent without a clone; an edited
        // one is built. Almost every column is untouched, and the branch is
        // here rather than inside `chunk` so the common case does not allocate
        // a megabyte to change nothing.
        let packet = if ctx.world.is_edited(*pos) {
            play_mod::chunk_packet(&ctx.world.chunk(*pos), *pos, ctx.version)?
        } else {
            play_mod::chunk_packet(ctx.world.template(), *pos, ctx.version)?
        };
        send_play(conn, packet, ctx.version).await?;
    }
    for pos in &change.forget {
        send_play(
            conn,
            play::clientbound::ForgetLevelChunk {
                chunk_x: pos.x,
                chunk_z: pos.z,
            },
            ctx.version,
        )
        .await?;
    }
    Ok(())
}

async fn play_loop<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    mut view: View,
    edits: &mut tokio::sync::broadcast::Receiver<Edit>,
    roster: &mut tokio::sync::broadcast::Receiver<super::players::RosterChange>,
    me: &super::players::Player,
    profile_id: [u8; 16],
    start: (f64, f64, f64),
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut position = start;
    // Recorded once on arrival too, so a player who joins and never moves is
    // still somewhere the next boot knows about.
    ctx.positions
        .lock()
        .expect("the position map is never poisoned")
        .insert(profile_id, position);

    let mut next_id: i64 = 1;
    // `interval`'s first tick fires immediately, and that is kept rather than
    // skipped: one keep-alive right after the chunks proves the round trip
    // works while the join is still the thing being debugged, instead of ten
    // seconds later when it is not.
    let mut ticker = tokio::time::interval(KEEP_ALIVE_PERIOD);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                send_play(
                    conn,
                    play::clientbound::KeepAlive { id: next_id },
                    ctx.version,
                )
                .await?;
                next_id = next_id.wrapping_add(1);
            }
            edit = edits.recv() => {
                match edit {
                    Ok(edit) => {
                        // Only if the client is holding the column. Sending an
                        // update for a chunk it does not have makes it apply
                        // the change to nothing and then receive the column
                        // again later with the change already in it — harmless
                        // twice over, and a packet for no reason.
                        if view.holds(view::column_of(
                            f64::from(edit.position.x),
                            f64::from(edit.position.z),
                        )) {
                            send_play(
                                conn,
                                play::clientbound::BlockUpdate {
                                    location: edit.position,
                                    block_id: VarInt(edit.state as i32),
                                },
                                ctx.version,
                            )
                            .await?;
                        }
                    }
                    // The sender is gone: the server is stopping, and this
                    // session is about to be too.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    // This session fell far enough behind that edits were
                    // dropped. Its world is now wrong in a way it cannot know
                    // about, so the columns it holds are resent rather than
                    // patched — which is the only repair available, since what
                    // was missed is exactly what is not known.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        ctx.logger.warn(
                            "dust::net",
                            format!("a session missed {missed} block change(s); resending its columns"),
                        );
                        let centre = view.centre().unwrap_or_else(|| ChunkPos::new(0, 0));
                        view = View::default();
                        stream(conn, ctx, &mut view, centre).await?;
                    }
                }
            }
            change = roster.recv() => {
                use super::players::RosterChange;
                match change {
                    // A session hears about its own join and its own movement,
                    // because its subscription starts before either. Filtered
                    // here rather than by the roster, because a channel that
                    // knew who each receiver was would be a channel per
                    // receiver — and showing a player their own body standing
                    // where they are is the one entity that must not exist.
                    Ok(RosterChange::Joined(player)) if player.entity_id != me.entity_id => {
                        send_play(conn, play_mod::player_info_add(&player)?, ctx.version).await?;
                        send_play(
                            conn,
                            play_mod::spawn_player(&player, ctx.player_entity_type),
                            ctx.version,
                        )
                        .await?;
                    }
                    Ok(RosterChange::Left { entity_id, uuid }) if entity_id != me.entity_id => {
                        // Both halves, again: the entity id takes the body away
                        // and the uuid takes the tab-list row.
                        send_play(conn, play_mod::despawn(entity_id), ctx.version).await?;
                        send_play(conn, play_mod::player_info_remove(uuid), ctx.version).await?;
                    }
                    Ok(RosterChange::Moved {
                        entity_id,
                        x,
                        y,
                        z,
                        yaw,
                        pitch,
                    }) if entity_id != me.entity_id => {
                        send_play(
                            conn,
                            play_mod::move_player(entity_id, x, y, z, yaw, pitch),
                            ctx.version,
                        )
                        .await?;
                        send_play(conn, play_mod::turn_head(entity_id, yaw), ctx.version).await?;
                    }
                    // Chat reaches everybody, the speaker included — a player
                    // has to see their own words, and filtering them here
                    // would mean every session adding them back locally.
                    Ok(RosterChange::Said { text, .. }) => {
                        send_play(
                            conn,
                            play::clientbound::SystemChat {
                                content: text,
                                // The log, not the action bar. `overlay`
                                // sends a line to the strip above the hotbar,
                                // where it replaces whatever is there and
                                // vanishes — which is right for a status
                                // readout and wrong for anything anybody said.
                                overlay: false,
                            },
                            ctx.version,
                        )
                        .await?;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    // Behind on the roster means showing players who left and
                    // missing players who arrived. Rebuilt from the current
                    // roster rather than patched, for the same reason as the
                    // edits: what was missed is what is not known.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        ctx.logger.warn(
                            "dust::net",
                            format!("a session missed {missed} roster change(s); resending it"),
                        );
                        for other in ctx.roster.snapshot() {
                            if other.entity_id == me.entity_id {
                                continue;
                            }
                            send_play(conn, play_mod::player_info_add(&other)?, ctx.version)
                                .await?;
                            send_play(
                                conn,
                                play_mod::spawn_player(&other, ctx.player_entity_type),
                                ctx.version,
                            )
                            .await?;
                        }
                    }
                }
            }
            frame = conn.next_frame() => {
                let Some(frame) = frame? else { return Ok(()) };
                let mut reader = Reader::new(&frame.body);
                // Decoded and dropped. Decoding it anyway is the point: this
                // is the only place in the project where a real client's
                // serverbound Play packets meet the generated definitions.
                match play::serverbound::Packet::decode_body(frame.id, &mut reader, ctx.version) {
                    // The three movement packets that carry a position. The
                    // fourth carries only a rotation, and a player turning on
                    // the spot has not changed which columns they can see.
                    Ok(play::serverbound::Packet::MovePlayerPos(m)) => {
                        position = (m.x, m.y, m.z);
                        record(ctx, profile_id, position);
                        ctx.roster
                            .moved(me.entity_id, m.x, m.y, m.z, me.yaw, me.pitch);
                        moved(conn, ctx, &mut view, m.x, m.z).await?;
                    }
                    Ok(play::serverbound::Packet::MovePlayerPosRot(m)) => {
                        position = (m.x, m.y, m.z);
                        record(ctx, profile_id, position);
                        ctx.roster
                            .moved(me.entity_id, m.x, m.y, m.z, m.yaw, m.pitch);
                        moved(conn, ctx, &mut view, m.x, m.z).await?;
                    }
                    Ok(play::serverbound::Packet::Chat(said)) => {
                        // The signature and the acknowledgement chain are
                        // decoded and dropped. Relaying the signature would be
                        // worse than not signing: it covers the message *and*
                        // the chain it was sent in, so a server that reorders
                        // or delays anything produces a signature the client
                        // rejects.
                        let message = said.message.as_str();
                        if super::chat::is_sendable(message) {
                            ctx.roster.say(
                                me.entity_id,
                                super::chat::player_said(&me.name, message),
                            );
                        }
                    }
                    // Turning on the spot. It changes no column, so it does not
                    // stream — but it is what everybody else sees, so it does
                    // reach the roster.
                    Ok(play::serverbound::Packet::MovePlayerRot(m)) => {
                        ctx.roster.moved(
                            me.entity_id,
                            position.0,
                            position.1,
                            position.2,
                            m.yaw,
                            m.pitch,
                        );
                    }
                    // Digging. Only the finish counts: the start and the
                    // cancel are a client telling the server what its
                    // animation is doing, and a server that broke a block on
                    // the start would break blocks the player let go of.
                    //
                    // In creative mode the client sends only StartDigging and
                    // expects the block gone, which is why that is honoured
                    // too — the join packet says creative, so this is the case
                    // that actually happens.
                    Ok(play::serverbound::Packet::PlayerAction(action)) => {
                        use play::serverbound::PlayerActionKind::{FinishDigging, StartDigging};
                        if matches!(action.status, StartDigging | FinishDigging) {
                            ctx.world.set_block(action.location, ctx.blocks.air);
                        }
                        // The sequence number is acknowledged whatever
                        // happened. The client predicted the change locally
                        // and holds that prediction until it is told the
                        // server has caught up; an unacknowledged sequence
                        // leaves the block flickering back and forth.
                        send_play(
                            conn,
                            play::clientbound::BlockChangedAck {
                                sequence: action.sequence,
                            },
                            ctx.version,
                        )
                        .await?;
                    }
                    // Placing. The block goes on the *face* that was clicked,
                    // not in the block that was clicked — a right-click on the
                    // top of the ground puts a block above it, and putting it
                    // in the clicked cell would replace the ground instead.
                    Ok(play::serverbound::Packet::UseItemOnBlock(use_on)) => {
                        let target = offset(use_on.hit.location, use_on.hit.face);
                        // There is no inventory, so there is nothing to place
                        // but the world's own surface block. Stated rather
                        // than dressed up: what a player is holding is not
                        // knowable here yet.
                        ctx.world.set_block(target, ctx.blocks.placeable);
                        send_play(
                            conn,
                            play::clientbound::BlockChangedAck {
                                sequence: use_on.sequence,
                            },
                            ctx.version,
                        )
                        .await?;
                    }
                    Ok(_) => {}
                    // A packet this server has no definition for is not a
                    // reason to drop a player. The list of unclaimed Play
                    // packets is published in dust-protocol, and meeting one
                    // is expected rather than exceptional.
                    Err(dust_protocol::wire::DecodeError::UnknownPacket { .. }) => {}
                    Err(e) => return Err(SessionError::Protocol(e)),
                }
            }
        }
    }
}

/// Stream whatever a move to `(x, z)` requires.
///
/// Called for every position packet, which arrive twenty times a second, and
/// almost all of them land in the column the player was already in — so the
/// common path is one comparison inside [`View::move_to`] and no packets at
/// all. The position is trusted as sent: nothing here validates movement, and
/// an anti-cheat that did would live between this and the world rather than
/// inside it.
async fn moved<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    x: f64,
    z: f64,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    stream(conn, ctx, view, view::column_of(x, z)).await
}

/// The block one step off `face` from `location`.
///
/// The face numbering is the protocol's: 0 down, 1 up, 2 north, 3 south,
/// 4 west, 5 east. An unknown face returns the clicked block itself, which
/// places into the block that was clicked — wrong, and the least wrong of the
/// available answers for a number this server does not recognise.
fn offset(location: dust_protocol::types::Position, face: u8) -> dust_protocol::types::Position {
    let (dx, dy, dz) = match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        5 => (1, 0, 0),
        _ => (0, 0, 0),
    };
    dust_protocol::types::Position {
        x: location.x + dx,
        y: location.y + dy,
        z: location.z + dz,
    }
}

/// Put a player's position where a shutdown can find it.
///
/// Called on every movement packet. The lock is held for a hash lookup and a
/// three-float write; anything longer here would be a lock every player
/// contends for on every step.
fn record(ctx: &SessionContext, profile_id: [u8; 16], position: (f64, f64, f64)) {
    ctx.positions
        .lock()
        .expect("the position map is never poisoned")
        .insert(profile_id, position);
}

/// The entity id chat carries when the server itself is speaking.
///
/// Zero rather than an `Option`, because every reader of a chat line either
/// wants to know who said it or does not, and none of them wants to handle a
/// case that never has an answer. No player is entity 0.
const SERVER_SPEAKING: i32 = 0;

/// The entity id the player is given.
///
/// A constant because there is one player at a time and no entities beside
/// them. It becomes an allocator the moment there are two.
const ENTITY_ID: i32 = 1;

/// The id of the teleport that places a joining player.
const FIRST_TELEPORT_ID: i32 = 1;

/// How often a keep-alive goes out. Vanilla's cadence.
const KEEP_ALIVE_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);

/// Encode one clientbound Play packet and queue it.
async fn send_play<W, P>(
    conn: &mut Conn<W>,
    body: P,
    version: dust_protocol::ProtocolVersion,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    P: Into<play::clientbound::Packet>,
{
    let frame = to_frame!(body.into(), version);
    conn.send(frame).await?;
    Ok(())
}

/// Send a configuration-state disconnect carrying `message`.
async fn refuse_in_configuration<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    message: &str,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let reason = Component::text(message)
        .colored(Color::Named(NamedColor::Red))
        .to_nbt(ctx.version)?;
    let packet =
        configuration::clientbound::Packet::from(configuration::clientbound::Disconnect { reason });
    let frame = to_frame!(packet, ctx.version);
    conn.send(frame).await?;
    conn.disconnect();
    Ok(())
}

/// The session server for offline mode: there isn't one.
///
/// `LoginHandler` takes a session server unconditionally because the type it
/// needs is decided at compile time, and offline mode never calls either
/// method. Both bodies are unreachable rather than unimplemented, and they say
/// so by failing loudly instead of returning something plausible — a stub that
/// answered "yes, that player is who they say" would turn a wiring mistake
/// into an authentication bypass.
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
                let json = ProtocolString::new(ctx.status.render(ctx.counters.online()))?;
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

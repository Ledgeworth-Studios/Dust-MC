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
use dust_protocol::packets::play::Gamemode;
use dust_protocol::packets::{configuration, handshake, play, status};
use dust_protocol::text::{Color, Component, NamedColor};
use dust_protocol::types::{ProtocolString, VarInt};
use dust_protocol::wire::Reader;
use dust_protocol::ProtocolVersion;
use dust_world::coords::ChunkPos;
use tokio::io::{AsyncRead, AsyncWrite};

use super::configure::{configure, Configured};
use super::edits::{Edit, Player, SharedWorld};
use super::finish;
use super::inventory::{ClickMode, Inventory};
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
    /// The world's own spawn point, read from `level.dat` at boot, or `None`
    /// for a world that has no such file — a flat world, or a directory of
    /// region files with nothing beside them.
    ///
    /// Read once at boot rather than per join: it is a property of the world
    /// on disk, and re-reading it on every connection would make a join depend
    /// on a file read that can fail.
    pub world_spawn: Option<super::level::WorldSpawn>,
    /// How far a player may reach to break or place a block.
    ///
    /// The check itself is `dust_guard`'s, and it lives there rather than here
    /// for the reason that crate's own documentation gives: a rule that can
    /// only be run from inside a session can only be tested by running one.
    pub reach: dust_guard::Reach,
    /// How far a player may move in one tick before the server stops believing
    /// them.
    ///
    /// In `dust_guard` for the same reason the reach limit is, and measured
    /// rather than chosen: `tools/bot/movement.js` counts what a real client's
    /// movement packets actually contain, and decision record 0017 says what it
    /// found and what the number is set to because of it.
    pub speed: dust_guard::SpeedLimit,

    /// Whether a player may walk into a block. `[server] movement_collision`.
    /// The check also needs a block table that says which states are solid, so
    /// this being true is necessary and not sufficient — see
    /// [`super::collide::Ground::of`].
    pub collision: bool,
    /// The furthest this server will stream, in columns.
    ///
    /// A ceiling and not the answer: a client asks for a distance of its own
    /// during configuration and is served the smaller of the two, so what a
    /// given session actually uses is decided per connection and lives in its
    /// [`View`](super::view::View).
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
    /// Air, and the block a right-click falls back to.
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
    /// `minecraft:item`'s id in the same table.
    pub item_entity_type: i32,
    /// Every item lying on the ground, and the channel their comings and
    /// goings travel on.
    pub items: Arc<super::items::ItemWorld>,
    /// What a broken block yields, compiled from the operator's own loot
    /// tables at boot.
    ///
    /// An empty set is the ordinary state of a server without a `[data] path`,
    /// and it means breaking a block drops nothing rather than dropping a
    /// guess. Which of those a player would rather have is not a close call:
    /// a guess is a survival game that gives you the wrong thing, and there is
    /// no way to tell from inside the world that it was a guess.
    pub drops: Arc<dust_sim::drops::Tables>,
    /// What a grid of items makes. Read at boot out of the operator's own
    /// recipe files and never written again, so every session shares one copy.
    /// Empty when there is no `[data] path`, and an empty table crafts
    /// nothing rather than crafting wrongly.
    pub recipes: Arc<dust_sim::crafting::Recipes>,
    /// Minecraft's own per-state constants, if the operator put a table beside
    /// their data.
    ///
    /// The light engine has had these since decision record 0008; what a
    /// session wants from them is the sound a block makes going down. `None`
    /// is the ordinary state of a server without a `[data] path`, and it means
    /// a placement is silent rather than guessed at.
    pub constants: Option<Arc<dust_registry::BlockConstants>>,
    /// Where `requires_tool` sits in that table, resolved once at boot.
    ///
    /// A flag column is addressed by name and the name is compared against
    /// every column in the header, so resolving it per break would be a
    /// handful of string comparisons on the interaction path for an answer
    /// that cannot change while the server runs. `None` is a table extracted
    /// before the column existed, and it means no block asks for a tool —
    /// which is the server an operator had before decision record 0027.
    pub requires_tool: Option<dust_registry::constants::Flag>,
    /// The game mode every joining player is put in, from `[server] game_mode`.
    ///
    /// Read on the interaction path, where it decides whether a break is timed
    /// at all. Creative is the mode where it is not: the client has already
    /// removed the block and is only telling the server, so a server that made
    /// it wait would put a block back on a screen that had moved on.
    pub game_mode: dust_config::model::GameMode,
    /// Which block each item puts down, if the operator put a table beside
    /// their data.
    ///
    /// `None` is a server where a right-click puts down
    /// [`PlaceableBlocks::placeable`] whatever the player is holding — which is
    /// what every server did before this existed, and is a refusal to guess
    /// rather than a gap: matching item names to block names is right about
    /// nine hundred items and wrong about sixteen.
    pub item_blocks: Option<Arc<dust_registry::ItemBlocks>>,
    /// The contents of the registries Dust can serve, read at boot from
    /// `[data] path`.
    ///
    /// Empty when no such path is set, which is the state that makes a client
    /// acknowledging no data packs unservable. Held by value rather than
    /// behind a lock: it is read on every join and written never — a reload
    /// that changed it would be changing what a *joining* client is told, so
    /// it is a restart-scoped setting and the type says so by not being
    /// mutable.
    pub registry_contents: crate::registries::Loaded,

    /// Where each player was when they last left, by profile id.
    ///
    /// Read on join and written on leave, so a reconnecting player lands where
    /// they stood rather than at spawn. Shared rather than per-session for the
    /// obvious reason: the session that knows the position is the one that is
    /// ending.
    pub positions: super::save::SharedPositions,

    /// What each player was carrying when they last left, by profile id.
    ///
    /// The same shape and the same reason as [`positions`](Self::positions),
    /// with one difference that is the whole point of this cycle: this is what
    /// makes a relog give a player their things back. See
    /// [`super::inventory`].
    pub inventories: super::save::SharedInventories,
}

/// The block states a session can put into the world.
#[derive(Debug, Clone, Copy)]
pub struct PlaceableBlocks {
    /// What breaking a block leaves behind.
    pub air: u32,
    /// What a right-click puts down when nothing better is known.
    ///
    /// It was the *only* thing a player could place until the item table
    /// arrived; now it is the fallback for the three ways that lookup comes up
    /// empty — no `dust-items.tsv` beside the data, an empty hand, or an item
    /// that places no block. See [`held_block`].
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
    /// A registry entry loaded from the operator's data would not encode as
    /// NBT. Separate from `Encode` because the cause is a file rather than a
    /// packet definition, and the operator can do something about it.
    RegistryContents(String),
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
            Self::RegistryContents(detail) => {
                write!(f, "a registry entry would not encode as NBT: {detail}")
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
    let mut view_distance = ctx.view_distance;
    let outcome = match configure(&mut conn, ctx).await? {
        Configured::Ready {
            view_distance: asked,
        } => {
            // The smaller of what the client asked for and what this server
            // will serve. A request and not a demand in either direction: a
            // client asking for thirty-two on a server set to eight gets
            // eight, and one asking for two is spared the columns it would
            // throw away.
            if let Some(asked) = asked.filter(|asked| *asked > 0) {
                view_distance = view_distance.min(asked);
            }
            Served::LoggedIn {
                username: authenticated.username,
                profile_id: authenticated.profile_id,
            }
        }
        Configured::UnknownContent => {
            // The client did not acknowledge the vanilla pack, so its
            // registries have to carry their contents and no `[data] path`
            // supplied any. Checked rather than assumed: serving the names
            // anyway was tried against a client that acknowledges nothing, and
            // it fails inside its own registry loader without ever reaching
            // the world. See `configure`'s module documentation.
            //
            // The message names the setting. Whoever reads it is running a bot
            // or a proxy against a server they control, and "this server
            // cannot serve you" without saying what would is a dead end for
            // somebody who is one line of configuration away.
            refuse_in_configuration(
                &mut conn,
                ctx,
                "This server has no copy of Minecraft's data, so it can only \
                 serve clients that already have their own. Set [data] path in \
                 dust.toml to admit clients that acknowledge no data packs.",
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
    serve_play(&mut conn, ctx, profile_id, &username, view_distance).await?;
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
    view_distance: u32,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let version = ctx.version;

    // Where this player was when they last left, or spawn if this is the
    // first time. Read before the join packet, because the join packet is not
    // what carries a position — the teleport below is — and a player told the
    // wrong one first would see themselves move.
    let spawn = match ctx.world_spawn {
        Some(point) => world::spawn_at(&ctx.world, point.x, point.z),
        None => world::spawn_in(&ctx.world),
    };
    let saved = ctx
        .positions
        .lock()
        .expect("the position map is never poisoned")
        .get(&profile_id)
        .copied();
    let start = saved.unwrap_or(spawn);
    // Which way they face. The world's spawn angle is for somebody arriving at
    // the spawn; a returning player is put back where they stood, and the save
    // records that and not which way they were looking. Facing south is what
    // vanilla does with no angle, so it is a behaviour rather than a stand-in.
    let yaw = match (saved, ctx.world_spawn) {
        (None, Some(point)) => point.angle,
        _ => 0.0,
    };

    // Counted here rather than when the session ends, and held by a guard so
    // that every way out of this function — a disconnect, a timeout, a decode
    // error, a panic in the task — puts the number back.
    let _player = ctx.counters.player_joined();

    send_play(
        conn,
        play_mod::login_packet(
            ENTITY_ID,
            ctx.status.max_players(),
            view_distance,
            ctx.overworld_dimension_type,
            match ctx.game_mode {
                dust_config::model::GameMode::Survival => Gamemode::Survival,
                dust_config::model::GameMode::Creative => Gamemode::Creative,
            },
        )?,
        version,
    )
    .await?;

    // Abilities before the position, as a real server sends them. This is
    // where creative flight is *granted*: the game mode in the join packet
    // does not grant it, and a client that is never sent this walks.
    // The abilities follow the mode, and `INSTANT_BREAK` is the flag that
    // matters here: with it the client does not animate a break at all, and
    // without it the client runs its own break timer against the same numbers
    // the server is about to run against.
    let creative = ctx.game_mode == dust_config::model::GameMode::Creative;
    send_play(conn, play_mod::abilities(creative), version).await?;
    send_play(conn, play_mod::frozen_at_noon(), version).await?;
    send_play(conn, play_mod::default_spawn(spawn), version).await?;

    // Before the chunks, not after: a client uses its position to decide which
    // columns it wants, and one told about columns before it knows where it is
    // throws them away.
    send_play(
        conn,
        play_mod::position_packet(start, yaw, FIRST_TELEPORT_ID),
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
    let mut view = View::with_radius(view_distance);
    let centre = view::column_of(start.0, start.2);

    // The columns this player's movement will be checked against, claimed
    // before anything is sent and given up however this session ends. See
    // `residency::Residence`.
    //
    // Warmed here rather than on a blocking thread, which is the one place
    // that is right: this is a join, there is no movement packet waiting on
    // it, and the nine columns it builds are nine of the first twenty-five the
    // stream below is about to send — so the stream finds them resident and
    // the warm costs the join nothing it was not already paying.
    let mut residence = super::residency::Residence::new(ctx.world.residency());
    if residence.move_to(centre) {
        ctx.world.warm(centre);
    }

    // The near square first, then the loading screen ends, then the rest.
    //
    // A player used to spend the whole burst looking at a progress bar with the
    // ground under their feet already sent. `View::move_to` returns columns
    // nearest first, so the first twenty-five are exactly what somebody
    // standing there can see — and the event that ends the loading screen needs
    // the world to be *there*, not complete.
    //
    // **Measured, A/B on one binary at the default view distance: the loading
    // screen ends at 668 ms with the limit and 1,757 ms without it**, and the
    // last of the 289 columns arrives at the same moment either way. This
    // shortens the wait rather than the work; a per-tick streaming budget is a
    // different change and is Phase 17's.
    stream_up_to(conn, ctx, &mut view, centre, JOIN_FIRST_COLUMNS).await?;

    // The ground is there; this is what tells the client to stop looking at
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

    // Everything else in view is left to the play loop, which sends it a batch
    // at a time between keep-alives and whatever the client is saying. See
    // `STREAM_BATCH`.

    // Health, last, exactly where vanilla puts it — and the position in the
    // order is load-bearing rather than tidy. `mineflayer` treats this packet
    // as the moment it is in the world, so a server that sends it before the
    // position has a bot that believes it spawned at the origin. Captured
    // from vanilla's join burst, where it is the last packet sent.
    send_play(conn, play_mod::full_health(), version).await?;

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
        // And how they are standing. A spawned player is standing upright
        // until something says otherwise, so somebody who started crouching
        // before this player arrived would be upright to them and crouching to
        // everybody else — two clients rendering the same player differently,
        // which is the reason the roster keeps this rather than the session
        // that owns them.
        //
        // Only when it is not the default: a metadata packet per player per
        // join, saying nothing, is a packet per player per join.
        if other.sneaking || other.sprinting {
            send_play(
                conn,
                play_mod::posture(other.entity_id, other.sneaking, other.sprinting),
                version,
            )
            .await?;
        }
        // And what they are wearing and holding. Same reasoning again, and it
        // is the failure mode this half of the feature exists for: a player
        // who logs in would otherwise see everybody bare-headed and
        // empty-handed until each of them happened to change a slot. One
        // packet per dressed player, and none at all for an undressed one.
        if let Some(packet) = play_mod::equipment_on_sight(other) {
            send_play(conn, packet, version).await?;
        }
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
        &mut residence,
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
/// How many columns the play loop sends per batch, and how often.
///
/// Eight every twenty milliseconds is four hundred a second — comfortably more
/// than a walking player generates, and enough to clear a full view distance's
/// backlog in under a second. The numbers are a pair and only mean anything
/// together, which is why they sit together.
const STREAM_BATCH: usize = 8;
const STREAM_BATCH_PERIOD: std::time::Duration = std::time::Duration::from_millis(20);

/// How many columns go out before the loading screen is allowed to end.
///
/// Twenty-five, which is the five-by-five around the player — what somebody
/// standing still can actually see, and what this server sent in total before
/// the view distance became a setting. Enough that the world is under their
/// feet and out to the horizon they will look at first; small enough that it
/// is a fraction of the wait.
const JOIN_FIRST_COLUMNS: usize = 25;

/// Stream at most `limit` of the columns a move to `centre` requires.
///
/// The remainder is left for the next call, which is what makes this composable
/// with [`stream`] rather than a second implementation of it: `View` records
/// only what was actually sent, so a partial pass leaves the rest wanted.
async fn stream_up_to<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    centre: ChunkPos,
    limit: usize,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    stream_inner(conn, ctx, view, centre, Some(limit)).await
}

async fn stream<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    centre: ChunkPos,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    stream_inner(conn, ctx, view, centre, None).await
}

async fn stream_inner<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    centre: ChunkPos,
    limit: Option<usize>,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let change = view.move_to_limited(centre, limit);
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
            play_mod::chunk_packet(ctx.world.template(*pos).as_chunk(), *pos, ctx.version)?
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

// Nine, and the ninth is the residence. It is a guard whose whole job is to
// outlive every way this loop can end, so it is created before the join
// streams its first column — a hold taken inside the loop would leak the ring
// for a session whose stream failed on the way in.
#[allow(clippy::too_many_arguments)]
async fn play_loop<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    mut view: View,
    edits: &mut tokio::sync::broadcast::Receiver<Edit>,
    roster: &mut tokio::sync::broadcast::Receiver<super::players::RosterChange>,
    me: &super::players::Player,
    profile_id: [u8; 16],
    start: (f64, f64, f64),
    residence: &mut super::residency::Residence,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut position = start;
    // Where the server believes this player is, as opposed to where their last
    // packet said. Until this existed they were the same thing, and the README
    // said so in its "Not yet" list.
    let mut movement = dust_guard::Movement::new(ctx.speed, start);
    // What shape this player is, as far as anything they have sent says. Read
    // by both checks: the collision half measures this much of them and the
    // reach half measures from this pose's eye. Everything in it comes off a
    // packet — three `player_command` actions, the abilities flags, and the
    // on-ground flag every movement packet carries — and nothing in it costs a
    // world lookup. See `dust_guard::Posture`.
    let mut posture = dust_guard::Posture::default();
    // The world this player's movement is checked against, and the four
    // columns it keeps. `None` where the block table cannot say what is solid,
    // which is a server running without the operator's extracted constants or
    // with a table extracted before the `full_collision` column existed.
    let mut ground = ctx
        .collision
        .then(|| super::collide::Ground::of(&ctx.world, ctx.constants.as_deref()))
        .flatten();
    // When the last movement packet was judged. A movement budget has to be
    // per *tick* and not per packet, or a connection that stalls and then
    // delivers fourteen queued packets at once refuses thirteen of them.
    let mut last_move = std::time::Instant::now();
    // Teleport ids this session has issued. The join used the first one; every
    // correction takes the next, so a client's acknowledgement names which
    // teleport it is answering.
    let mut last_teleport_id = FIRST_TELEPORT_ID;
    // Recorded once on arrival too, so a player who joins and never moves is
    // still somewhere the next boot knows about.
    ctx.positions
        .lock()
        .expect("the position map is never poisoned")
        .insert(profile_id, position);

    // Where this player is looking, in the protocol's own degrees. Kept
    // because a placement reads it: a stair faces the way the player was
    // standing. Starts at the spawn's rotation and is replaced by the first
    // packet that carries one, which arrives within a tick of joining.
    let mut rotation = (me.yaw, me.pitch);

    // What this player is carrying. Restored from the last time they were
    // here, which is the difference between a server people play on and one
    // that empties their pockets at the door.
    let mut inventory = ctx
        .inventories
        .lock()
        .expect("the inventory map is never poisoned")
        .get(&profile_id)
        .map(|carried| Inventory::restored(carried.slots.clone(), carried.selected))
        .unwrap_or_default()
        .crafting_with(Arc::clone(&ctx.recipes));

    // And told to the client, all forty-six slots at once. This is the one
    // place the whole container goes out: a join has nothing to compare
    // against, so there is nothing to send a difference of. Every change after
    // this one is a single slot.
    send_container(conn, ctx, &mut inventory).await?;
    send_play(
        conn,
        play::clientbound::SetCarriedItem {
            slot: inventory.selected(),
        },
        ctx.version,
    )
    .await?;

    // What this player is wearing, to everybody who can already see them.
    // After the restore and not before it: the roster took this player with
    // nothing on, because the container is loaded here and the join is up
    // there, and a player who logged out in full armour would otherwise be
    // naked to everyone until their first click.
    ctx.roster.equipped(me.entity_id, inventory.equipment());

    // Every item lying in the world from now on. Subscribed *before* the
    // items already there are sent, for the reason the edit channel is: an
    // item that appears in the window between the two is better sent twice
    // than not at all, because a client told about an entity it already has
    // replaces it and a client never told has a drop it cannot see.
    let mut item_changes = ctx.items.subscribe();
    if !ctx.items.is_empty() {
        let mut already = Vec::new();
        ctx.items
            .visible_from(position, f64::from(view.radius() * 16), &mut already);
        for change in &already {
            relay_item(conn, ctx, change).await?;
        }
    }
    // Picking things up is asked at the tick rate rather than on every
    // movement packet: a client sends those faster than the world moves, and a
    // player walking over a stack does not need to be told twice in one tick
    // that they have it. The atomic read at the top of the arm is what makes
    // this free on a server with nothing on the floor.
    let mut pickups = tokio::time::interval(PICKUP_PERIOD);
    let mut collected: Vec<(dust_registry::Item, u8)> = Vec::new();

    // The stream every break's loot roll comes out of. Seeded from the
    // player's entity id and the moment they joined, so two players mining at
    // once do not get the same sequence of leaves and saplings and so a
    // restart does not repeat one.
    let mut drop_seed = (me.entity_id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);

    // The break in flight, if any. One per player and at most one: a client
    // that starts a second break has abandoned the first, which is what
    // replacing this says.
    let mut digging: Option<Digging> = None;

    let mut next_id: i64 = 1;
    // `interval`'s first tick fires immediately, and that is kept rather than
    // skipped: one keep-alive right after the chunks proves the round trip
    // works while the join is still the thing being debugged, instead of ten
    // seconds later when it is not.
    let mut ticker = tokio::time::interval(KEEP_ALIVE_PERIOD);
    // The backlog drains here, a batch at a time, rather than in one burst
    // before this loop starts. **What that buys is the loop itself.** A player
    // who joined and walked immediately used to have their movement packets sit
    // in the socket for as long as the far columns took, and the keep-alive
    // with them.
    //
    // Measured A/B on one binary at the default view distance, timing the first
    // keep-alive *after* the loading screen ends:
    //
    // ```text
    //            screen ends   first keep-alive after it   all 289 columns
    //   burst        648 ms       1,733 ms (1.1 s later)        1,731 ms
    //   batched      411 ms          428 ms (17 ms later)       1,768 ms
    // ```
    //
    // The same work in the same order, finishing at the same moment, with the
    // session answering throughout instead of at the end.
    //
    // It ticks whether or not there is a backlog, because a tick with nothing
    // to send costs one comparison inside `View::move_to`. Making the branch
    // conditional would mean a `select!` arm that is sometimes absent, which
    // is a lot of shape for a comparison.
    let mut streaming = tokio::time::interval(STREAM_BATCH_PERIOD);
    loop {
        tokio::select! {
            _ = streaming.tick() => {
                stream_up_to(
                    conn,
                    ctx,
                    &mut view,
                    view::column_of(position.0, position.2),
                    STREAM_BATCH,
                )
                .await?;
            }
            _ = ticker.tick() => {
                send_play(
                    conn,
                    play::clientbound::KeepAlive { id: next_id },
                    ctx.version,
                )
                .await?;
                next_id = next_id.wrapping_add(1);
            }
            _ = pickups.tick() => {
                // A break whose stop came in too early finishes here, on the
                // server's own count. This arm already runs once a tick for
                // the pickups, so the whole cost of the delayed path is one
                // `Option` test on a player who is not mining — which is why
                // it lives here rather than in a timer of its own.
                if let Some(dig) = digging.filter(|dig| dig.delayed) {
                    let elapsed = dust_sim::mining::Progress::ticks_elapsed(
                        dig.started.elapsed().as_millis(),
                    );
                    if dig.progress.server_done(elapsed) {
                        digging = None;
                        let previous = ctx.world.block_at(dig.at);
                        let neighbours = [
                            (-1i8, ctx.world.block_at(below(dig.at))),
                            (1i8, ctx.world.block_at(above(dig.at))),
                        ];
                        let broke =
                            ctx.world.break_block(dig.at, ctx.blocks.air, me.entity_id);
                        if broke {
                            drop_seed = drop_seed
                                .wrapping_mul(6_364_136_223_846_793_005)
                                .wrapping_add(1_442_695_040_888_963_407);
                            spill(
                                ctx,
                                dig.at,
                                previous,
                                &neighbours,
                                inventory.held(),
                                drop_seed,
                            );
                        }
                    }
                }
                if !ctx.items.is_empty() {
                    collected.clear();
                    ctx.items.claim_near(me.entity_id, position, &mut collected);
                    for (item, count) in collected.drain(..) {
                        let (changed, left) = inventory
                            .collect(super::inventory::Stack::new(item, count));
                        for index in 0..super::inventory::SLOTS {
                            if changed.has(index) {
                                send_slot(conn, ctx, &mut inventory, index).await?;
                            }
                        }
                        // A pickup is a container change like any other, and
                        // it is one this loop used not to record: an item
                        // walked over and then a crash put it nowhere. It is
                        // also the one change that can arm a player without a
                        // click — a sword collected into the selected hotbar
                        // slot is a sword everybody else has to see.
                        record_inventory(ctx, profile_id, me.entity_id, &inventory);
                        // A full inventory puts it straight back on the floor
                        // where the player is standing, so nothing is deleted
                        // and they can see what would not fit.
                        if let Some(over) = left {
                            ctx.items.pop(
                                &ctx.roster,
                                dust_protocol::types::Position {
                                    x: position.0.floor() as i32,
                                    y: position.1.floor() as i32,
                                    z: position.2.floor() as i32,
                                },
                                over.item,
                                over.count,
                                me.entity_id as u64,
                            );
                        }
                    }
                }
            }
            change = item_changes.recv() => {
                match change {
                    Ok(change) => {
                        let (x, z) = change.at();
                        if view.holds(view::column_of(x, z)) {
                            relay_item(conn, ctx, &change).await?;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        // Unlike a missed block change, a missed item change
                        // cannot be repaired by resending a column: entities
                        // are not in the chunk packet. The honest repair is to
                        // say so; the visible cost is an item the player walks
                        // through, and it is picked up anyway because pickup
                        // is decided by the server and not by what was drawn.
                        ctx.logger.warn(
                            "dust::net",
                            format!("{} missed {missed} item change(s)", me.name),
                        );
                    }
                    // The world is going away, and a closed broadcast
                    // receiver is ready for ever after. Ending the session is
                    // what the edit channel does for the same reason: the
                    // alternative is a `select!` arm that resolves instantly
                    // on every pass and spins a core.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
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
                            // And what a player doing it looked and sounded
                            // like. Never to the player who did it: their own
                            // client played the effect before the server heard
                            // about the click, and telling them again plays it
                            // twice. Vanilla leaves them out for the same
                            // reason.
                            //
                            // The two arms are not symmetrical and the asymmetry
                            // is vanilla's: a break is one level event carrying
                            // the broken state, out of which the client makes
                            // both the particles and the sound, while a
                            // placement has no particles and a sound that has to
                            // be named.
                            match edit.by {
                                Some(cause) if cause.entity_id() != me.entity_id => match cause {
                                    Player::Broke { previous, .. } => {
                                        send_play(
                                            conn,
                                            play_mod::block_broken(edit.position, previous),
                                            ctx.version,
                                        )
                                        .await?;
                                    }
                                    Player::Placed { placed, seed, .. } => {
                                        if let Some(sound) = play_mod::block_placed(
                                            edit.position,
                                            placed,
                                            seed,
                                            ctx.constants.as_deref(),
                                        ) {
                                            send_play(conn, sound, ctx.version).await?;
                                        }
                                    }
                                },
                                _ => {}
                            }
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
                        // Almost always nothing: a player joins the roster
                        // before their container is loaded, and the equipment
                        // that follows arrives as its own change a moment
                        // later. Sent anyway, because "almost always" is not
                        // a reason to leave the one case that is not.
                        if let Some(packet) = play_mod::equipment_on_sight(&player) {
                            send_play(conn, packet, ctx.version).await?;
                        }
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
                    // Swings and postures go to everybody but the player
                    // who did them, like movement: their own client has
                    // already animated the swing and is already crouching,
                    // and being told again would fight its own prediction.
                    Ok(RosterChange::Swung {
                        entity_id,
                        animation,
                    }) if entity_id != me.entity_id => {
                        send_play(
                            conn,
                            play::clientbound::Animate {
                                entity_id: dust_protocol::types::VarInt(entity_id),
                                animation,
                            },
                            ctx.version,
                        )
                        .await?;
                    }
                    // Equipment goes to everybody but its wearer, like a
                    // swing and for a sharper reason: a player's own armour
                    // and hand are drawn from the container they already
                    // have, and vanilla's own tracker does not send an entity
                    // its own equipment either.
                    Ok(RosterChange::Equipped { entity_id, slots }) if entity_id != me.entity_id => {
                        if let Some(packet) = play_mod::set_equipment(entity_id, &slots) {
                            send_play(conn, packet, ctx.version).await?;
                        }
                    }
                    Ok(RosterChange::Posture {
                        entity_id,
                        sneaking,
                        sprinting,
                    }) if entity_id != me.entity_id => {
                        send_play(
                            conn,
                            play_mod::posture(entity_id, sneaking, sprinting),
                            ctx.version,
                        )
                        .await?;
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
                            if let Some(packet) = play_mod::equipment_on_sight(&other) {
                                send_play(conn, packet, ctx.version).await?;
                            }
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
                        let ticks = ticks_since(&mut last_move);
                        posture.on_ground = m.on_ground;
                        movement.posture(posture);
                        match judge(&mut movement, (m.x, m.y, m.z), ticks, &mut ground) {
                            dust_guard::Claim::Accepted => {
                                position = movement.at();
                                record(ctx, profile_id, position);
                                ctx.roster.moved(
                                    me.entity_id,
                                    position.0,
                                    position.1,
                                    position.2,
                                    me.yaw,
                                    me.pitch,
                                );
                                moved(conn, ctx, &mut view, residence, position.0, position.2)
                                    .await?;
                            }
                            dust_guard::Claim::Ignored => {}
                            dust_guard::Claim::Refused(why) => {
                                put_back(
                                    conn,
                                    ctx,
                                    &mut movement,
                                    &mut last_teleport_id,
                                    (m.x, m.y, m.z),
                                    why,
                                )
                                .await?;
                            }
                        }
                    }
                    Ok(play::serverbound::Packet::MovePlayerPosRot(m)) => {
                        // The rotation is kept whatever happens to the
                        // position. A player who is refused for moving too far
                        // was still looking somewhere, and a placement reads
                        // that; refusing a look because a position was wrong
                        // would leave the two out of step for no gain.
                        rotation = (m.yaw, m.pitch);
                        let ticks = ticks_since(&mut last_move);
                        posture.on_ground = m.on_ground;
                        movement.posture(posture);
                        match judge(&mut movement, (m.x, m.y, m.z), ticks, &mut ground) {
                            dust_guard::Claim::Accepted => {
                                position = movement.at();
                                record(ctx, profile_id, position);
                                ctx.roster.moved(
                                    me.entity_id,
                                    position.0,
                                    position.1,
                                    position.2,
                                    m.yaw,
                                    m.pitch,
                                );
                                moved(conn, ctx, &mut view, residence, position.0, position.2)
                                    .await?;
                            }
                            dust_guard::Claim::Ignored => {}
                            dust_guard::Claim::Refused(why) => {
                                put_back(
                                    conn,
                                    ctx,
                                    &mut movement,
                                    &mut last_teleport_id,
                                    (m.x, m.y, m.z),
                                    why,
                                )
                                .await?;
                            }
                        }
                    }
                    // A client answering a teleport. Most of these are about
                    // nothing — every client acknowledges the one that placed
                    // it on join — and the one that is not is what ends the
                    // silence after a correction.
                    Ok(play::serverbound::Packet::TeleportConfirm(confirm)) => {
                        movement.confirmed(confirm.teleport_id.0);
                    }
                    // An arm swing. Sent on every click, hit and miss
                    // alike, and it is the only thing that makes another
                    // player look like they are doing something rather than
                    // sliding around with their arms down.
                    Ok(play::serverbound::Packet::SwingArm(swing)) => {
                        let packet = play_mod::swing(
                            me.entity_id,
                            swing.hand == dust_protocol::packets::play::Hand::Off,
                        );
                        ctx.roster.swung(me.entity_id, packet.animation);
                    }
                    // Crouching, running and gliding — which is everything a
                    // 1.21.1 client ever tells a server about its own shape.
                    // Two readers: the roster, so that everybody else sees the
                    // animation, and `dust_guard`, which measures a crouching
                    // player 0.3 shorter and their eyes 0.35 lower than a
                    // standing one. The remaining actions are about horses and
                    // beds, neither of which exists here.
                    Ok(play::serverbound::Packet::PlayerCommand(command)) => {
                        use play::serverbound::PlayerCommandAction as Action;
                        let (sneaking, sprinting) = match command.body.action_id {
                            Action::StartSneaking => (Some(true), None),
                            Action::StopSneaking => (Some(false), None),
                            Action::StartSprinting => (None, Some(true)),
                            Action::StopSprinting => (None, Some(false)),
                            _ => (None, None),
                        };
                        // A client says when a glide starts and never when it
                        // ends; vanilla's server works the landing out for
                        // itself and this one cannot yet. A stale `true` makes
                        // a player shorter than they are, which believes them
                        // rather than refusing them, and the first accepted
                        // move after they land clears it.
                        if command.body.action_id == Action::StartFlyingWithElytra {
                            posture.gliding = true;
                        }
                        if let Some(sneaking) = sneaking {
                            posture.sneaking = sneaking;
                        }
                        if let Some(sprinting) = sprinting {
                            posture.sprinting = sprinting;
                        }
                        movement.posture(posture);
                        if sneaking.is_some() || sprinting.is_some() {
                            ctx.roster.posture(me.entity_id, sneaking, sprinting);
                        }
                    }
                    // A player toggling flight. The only thing read out of it
                    // is whether they are flying, because vanilla's crouch
                    // condition is `isShiftKeyDown() && !abilities.flying`: a
                    // creative player who holds shift while flying descends
                    // and stays their full height.
                    Ok(play::serverbound::Packet::PlayerAbilities(abilities)) => {
                        posture.flying = abilities
                            .flags
                            .has(dust_protocol::packets::play::Abilities::FLYING);
                        movement.posture(posture);
                    }
                    // A player changing their render distance in options.
                    // The server's setting is still the ceiling, and the view
                    // sends or forgets the difference on its next move — which
                    // is now paced by the loop rather than sent in a burst,
                    // which is what makes this affordable at all.
                    Ok(play::serverbound::Packet::ClientInformation(info)) => {
                        if let Ok(asked) = u32::try_from(info.view_distance) {
                            if asked > 0 {
                                view.set_radius(ctx.view_distance.min(asked));
                                stream_up_to(
                                    conn,
                                    ctx,
                                    &mut view,
                                    view::column_of(position.0, position.2),
                                    STREAM_BATCH,
                                )
                                .await?;
                            }
                        }
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
                    // Neither a move nor a turn: a player who landed or
                    // jumped where they stood, and the timer this packet is
                    // also sent on. It carries one bit and that bit is read —
                    // a player who lands without moving has stopped being
                    // airborne, and `Posture::on_ground` is what says so.
                    Ok(play::serverbound::Packet::MovePlayerStatusOnly(m)) => {
                        posture.on_ground = m.on_ground;
                        movement.posture(posture);
                    }
                    // Turning on the spot. It changes no column, so it does not
                    // stream — but it is what everybody else sees, so it does
                    // reach the roster.
                    Ok(play::serverbound::Packet::MovePlayerRot(m)) => {
                        rotation = (m.yaw, m.pitch);
                        posture.on_ground = m.on_ground;
                        movement.posture(posture);
                        ctx.roster.moved(
                            me.entity_id,
                            position.0,
                            position.1,
                            position.2,
                            m.yaw,
                            m.pitch,
                        );
                    }
                    // Digging, and which packet finishes it depends on the
                    // mode the player was told they are in.
                    //
                    // **In creative the start is the whole of it.** The client
                    // removes the block locally the instant it is clicked and
                    // never sends a stop, so the server that made it wait
                    // would be answering a screen that had moved on. The
                    // finish is honoured too, because a client that mines
                    // through sends both and setting air twice is idempotent.
                    //
                    // **In survival the start only starts a clock.** How long
                    // that clock runs is `dust_sim::mining`, against the
                    // block's own hardness and the tool in the hand, and the
                    // stop the client sends when its animation ends is
                    // believed at 70% — see decision record 0028 and the
                    // header of [`dust_sim::mining`] for why the two
                    // thresholds are different numbers.
                    Ok(play::serverbound::Packet::PlayerAction(action)) => {
                        use play::serverbound::PlayerActionKind::{
                            CancelDigging, FinishDigging, StartDigging,
                        };
                        let creative =
                            ctx.game_mode == dust_config::model::GameMode::Creative;
                        let reachable = matches!(
                            action.status,
                            StartDigging | CancelDigging | FinishDigging
                        ) && within_reach(
                            ctx,
                            position,
                            movement.pose(),
                            action.location,
                        );
                        // Whether this packet is the one that takes the block
                        // away. Everything above decides it; one place below
                        // acts on it, so the destroy path is written once.
                        let destroy = match action.status {
                            StartDigging if reachable && creative => true,
                            FinishDigging if reachable && creative => true,
                            StartDigging if reachable => {
                                // **One read of the world, here.** The state
                                // is what carries the hardness and what the
                                // tool is judged correct against, and both
                                // come out of the same lookup. The break
                                // itself reads again when it happens, because
                                // by then the cell may hold something else —
                                // that is a second interaction, not a second
                                // read of this one.
                                let state = ctx.world.block_at(action.location);
                                let progress = break_progress(
                                    ctx,
                                    state,
                                    inventory.held(),
                                    posture.on_ground,
                                );
                                if progress.instant() {
                                    digging = None;
                                    true
                                } else {
                                    // Kept even when the block cannot be
                                    // broken at all: `possible()` is false for
                                    // bedrock, and neither threshold is ever
                                    // reached, so the entry expires by never
                                    // firing rather than by a second branch.
                                    digging = Some(Digging {
                                        at: action.location,
                                        started: std::time::Instant::now(),
                                        progress,
                                        delayed: false,
                                    });
                                    false
                                }
                            }
                            FinishDigging if reachable => match digging.as_mut() {
                                Some(dig) if dig.at == action.location => {
                                    let elapsed = dust_sim::mining::Progress::ticks_elapsed(
                                        dig.started.elapsed().as_millis(),
                                    );
                                    if dig.progress.stop_accepted(elapsed) {
                                        digging = None;
                                        true
                                    } else {
                                        // Too early to believe, and **not a
                                        // refusal**: the block still goes, on
                                        // the server's own count, from the
                                        // tick loop below. A client that
                                        // spams stops gets one armed entry and
                                        // not a queue of them.
                                        dig.delayed = true;
                                        false
                                    }
                                }
                                // A stop for a block this player never started
                                // on. Acknowledged and otherwise ignored.
                                _ => false,
                            },
                            // Letting go. The client has stopped animating and
                            // so does the server; an armed delayed destroy is
                            // dropped with it, which is what a player who
                            // changed their mind means.
                            CancelDigging => {
                                digging = None;
                                false
                            }
                            _ => false,
                        };
                        if destroy {
                            // Read before the break, because after it the
                            // cell is air and a loot table asks what *was*
                            // there. The two cells above and below come with
                            // it: two tables read them, and both are a
                            // double-tall plant deciding which half of itself
                            // is the one that drops.
                            let previous = ctx.world.block_at(action.location);
                            let neighbours = [
                                (-1i8, ctx.world.block_at(below(action.location))),
                                (1i8, ctx.world.block_at(above(action.location))),
                            ];
                            // Through `break_block` and not `set_block`: the
                            // other players are shown the block breaking, and
                            // the particles and the sound come from what was
                            // there rather than from the air left behind.
                            //
                            // **The return value is the whole of the
                            // difference between a break that yielded nothing
                            // and a break that never happened.** They leave
                            // the same air behind and look identical from
                            // outside; only this says which it was, and a
                            // refused break that dropped a stone would be a
                            // duplication bug nobody could see.
                            let broke = ctx.world
                                .break_block(action.location, ctx.blocks.air, me.entity_id);
                            if broke {
                                drop_seed = drop_seed.wrapping_mul(6_364_136_223_846_793_005)
                                    .wrapping_add(1_442_695_040_888_963_407);
                                spill(
                                    ctx,
                                    action.location,
                                    previous,
                                    &neighbours,
                                    inventory.held(),
                                    drop_seed,
                                );
                            }
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
                    // Placing. Where it lands is [`placement`]; whether it
                    // lands at all is the same question asked of the cell it
                    // chose.
                    Ok(play::serverbound::Packet::UseItemOnBlock(use_on)) => {
                        // **Where it lands is decided before what it is**, and
                        // that order is the whole of this change: three of the
                        // placement rules read the cell the block is going
                        // into — water makes it wet, snow makes it deeper, a
                        // slab makes it double — and none of them can be asked
                        // until the cell is known.
                        //
                        // Reach is checked against the block that was
                        // *clicked* and not the one the placement lands in.
                        // They differ by a block, and the clicked one is the
                        // one the player actually touched — checking the target
                        // would refuse a legitimate click at the edge of range
                        // and allow one aimed back towards the player from just
                        // outside it.
                        let holding = held_place(ctx.item_blocks.as_deref(), inventory.held());
                        let target = placement(
                            &ctx.world,
                            ctx.constants.as_deref(),
                            use_on.hit.location,
                            &use_on.hit,
                            holding,
                            rotation,
                        )
                        .filter(|_| within_reach(ctx, position, movement.pose(), use_on.hit.location));
                        if let Some(target) = target {
                            let state = held_block(
                                holding,
                                ctx.blocks.placeable,
                                &use_on.hit,
                                rotation,
                                cell(&ctx.world, target),
                            );
                            ctx.world.place_block(target, state, me.entity_id);
                        }
                        send_play(
                            conn,
                            play::clientbound::BlockChangedAck {
                                sequence: use_on.sequence,
                            },
                            ctx.version,
                        )
                        .await?;
                    }
                    // Which hotbar slot is in hand. It changes no block and
                    // no position, so nothing goes out — but the next
                    // right-click is a different block because of it.
                    Ok(play::serverbound::Packet::SetCarriedItem(carried)) => {
                        if inventory.select(carried.slot) {
                            record_inventory(ctx, profile_id, me.entity_id, &inventory);
                        }
                    }
                    // A creative client writing a slot directly, which is the
                    // one inventory write that needs no container open. Every
                    // player here is in creative, so it is how most things get
                    // into a hand — but no longer the only way: a click is the
                    // other, and both land in the same container.
                    Ok(play::serverbound::Packet::SetCreativeModeSlot(set)) => {
                        match inventory.set_creative(set.slot, &set.item) {
                            Ok(changed) if !changed.is_empty() => {
                                record_inventory(ctx, profile_id, me.entity_id, &inventory);
                                // The slot the client named is not echoed
                                // back: it wrote that one itself and already
                                // draws it, and a set-slot here would be a
                                // packet per creative-menu click that changes
                                // nothing on screen. The crafting output is
                                // the exception — a write into the grid moves
                                // a slot the client did not touch.
                                for index in changed.iter() {
                                    if index != usize::try_from(set.slot).unwrap_or(usize::MAX) {
                                        send_slot(conn, ctx, &mut inventory, index).await?;
                                    }
                                }
                            }
                            Ok(_) => {}
                            // Refused — a count above the item's own maximum,
                            // or an item this build has no entry for. The
                            // client believes it put something there, so it has
                            // to be told what is actually in that slot.
                            Err(index) => {
                                send_slot(conn, ctx, &mut inventory, index).await?;
                            }
                        }
                    }
                    // A survival client's click: left, right, shift, a number
                    // key, the drags, all of it. Replayed over the server's own
                    // container, and only the slots the client is now wrong
                    // about are sent back.
                    Ok(play::serverbound::Packet::ClickContainer(click)) => {
                        if click.window_id == PLAYER_WINDOW {
                            // Which window the client thought it was clicking
                            // on, read *before* the click moves anything. A
                            // `state_id` that is not the current one means the
                            // client acted on a container that has since
                            // changed under it, so the corrections below would
                            // be differences against a picture it no longer
                            // holds.
                            let stale = click.state_id.0 != inventory.state_id();
                            let changed =
                                inventory.click(ClickMode::from(click.mode), click.slot, click.button);
                            if !changed.is_empty() {
                                record_inventory(ctx, profile_id, me.entity_id, &inventory);
                            }
                            if stale {
                                // The whole container, which is what Minecraft
                                // answers the same thing with. The click still
                                // happens — a stale number says the client is
                                // out of date, not that it clicked on nothing —
                                // and then the container is *stated* rather
                                // than corrected, because a difference is only
                                // meaningful against a picture both ends agree
                                // on and this client has just said it does not
                                // have one.
                                //
                                // Forty-seven stacks is the expensive answer
                                // and it is the one that ends the
                                // disagreement in a single packet. The
                                // per-slot corrections cover only the slots
                                // this click touched, so every *other* slot
                                // the client is wrong about stays wrong until
                                // something happens to move it — which is a
                                // player looking at an inventory that is not
                                // theirs, for as long as they leave it alone.
                                send_container(conn, ctx, &mut inventory).await?;
                            } else {
                                push_back(conn, ctx, &mut inventory, changed, &click).await?;
                            }
                        }
                    }
                    // The player closed their own inventory. Whatever was on
                    // the cursor and in the crafting grid goes back into the
                    // inventory rather than nowhere; see `Inventory::closed`.
                    Ok(play::serverbound::Packet::CloseContainer(closed)) => {
                        if closed.window_id == PLAYER_WINDOW && !inventory.closed().is_empty() {
                            record_inventory(ctx, profile_id, me.entity_id, &inventory);
                            send_container(conn, ctx, &mut inventory).await?;
                        }
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

/// Judge a claimed position against the world, or against nothing.
///
/// The two arms are the same call to the same generic method and differ only in
/// what they hand it, which is the point: a server with no answer for what is
/// solid runs the identical rule against [`dust_guard::Open`], where nothing
/// is, rather than running a different rule or a branch inside the hot one.
fn judge(
    movement: &mut dust_guard::Movement,
    to: (f64, f64, f64),
    ticks: u32,
    ground: &mut Option<super::collide::Ground<'_>>,
) -> dust_guard::Claim {
    match ground {
        Some(ground) => movement.claimed(to, ticks, ground),
        None => movement.claimed(to, ticks, &mut dust_guard::Open),
    }
}

/// Ticks since the last movement packet was judged, and reset the clock.
///
/// Wall time rather than a tick counter, because a session has no tick counter
/// and adding one to reach this number would tie the packet path to the game
/// loop for no gain: what the budget is about is how long the player had, and
/// that is what a clock measures. `Instant::now` is a vDSO read on every
/// platform this runs on, which is the cheapest thing in this function.
///
/// The cap is a thousand ticks and it is not the real bound — `dust_guard`
/// clamps this to its own maximum and owns that number. This one exists only so
/// that a session resumed after a laptop was shut for an hour produces a `u32`
/// rather than a wrap.
fn ticks_since(last: &mut std::time::Instant) -> u32 {
    let now = std::time::Instant::now();
    let millis = now.duration_since(*last).as_millis();
    *last = now;
    (millis / 50).min(1_000) as u32
}

/// Refuse a claimed position and teleport the player back to the last one the
/// server believed.
///
/// A correction and not a log line: the client honours a `player_position` by
/// moving, which is the whole difference between a server that notices a cheat
/// and one that stops it. Until the client acknowledges the teleport id, its
/// movement packets are ignored rather than refused — see
/// [`dust_guard::Claim::Ignored`] for why answering each of them with another
/// teleport is how a correction becomes a loop.
///
/// The log line is per correction and not per packet for the same reason.
async fn put_back<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    movement: &mut dust_guard::Movement,
    last_teleport_id: &mut i32,
    claimed: (f64, f64, f64),
    why: dust_guard::Refusal,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    *last_teleport_id = last_teleport_id.wrapping_add(1);
    let back = movement.correct(*last_teleport_id);
    let reason = match why {
        dust_guard::Refusal::NotFinite => "a coordinate that is not a number".to_owned(),
        dust_guard::Refusal::OutOfWorld => "a position outside every world".to_owned(),
        dust_guard::Refusal::IntoSolid { block } => format!(
            "a step into the solid block at {}, {}, {}",
            block.0, block.1, block.2
        ),
        dust_guard::Refusal::TooFast {
            moved_squared,
            allowed_squared,
        } => format!(
            "{:.1} blocks in the time for {:.1}",
            moved_squared.sqrt(),
            allowed_squared.sqrt()
        ),
    };
    ctx.logger.warn(
        "dust::net",
        format!(
            "a player claimed {:.1}, {:.1}, {:.1} — {reason}; put back to {:.1}, {:.1}, {:.1}",
            claimed.0, claimed.1, claimed.2, back.0, back.1, back.2
        ),
    );
    send_play(
        conn,
        play_mod::correction(back, *last_teleport_id),
        ctx.version,
    )
    .await
}

/// Stream whatever a move to `(x, z)` requires.
///
/// Called for every position packet, which arrive twenty times a second, and
/// almost all of them land in the column the player was already in — so the
/// common path is one comparison inside [`View::move_to`] and no packets at
/// all. Only reached for a position `dust_guard::Movement` accepted — a
/// refused one streams nothing, because the columns a player who is not there
/// can see are not columns anybody needs.
async fn moved<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    view: &mut View,
    residence: &mut super::residency::Residence,
    x: f64,
    z: f64,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let centre = view::column_of(x, z);
    // The claim is taken here, on this task, because it is nine hash lookups
    // and no file. The *building* goes to a blocking thread and is not waited
    // for: a movement check that had to wait for a region file would be the
    // 0.9 ms stall this exists to remove, moved rather than deleted.
    //
    // Nothing depends on the warm having finished. A player who somehow
    // reached a column before it was built finds it missing and builds it
    // themselves, which is what every column cost before this — the floor is
    // the old behaviour, not a hole. `dust_guard::SpeedLimit` is what makes
    // that not happen: sixteen blocks of walking is 1.6 seconds at the fastest
    // speed this server will believe and the warm is about five milliseconds.
    if residence.move_to(centre) {
        ctx.world.want_ring(centre);
    }
    // A batch, not the whole difference. A player crossing a chunk boundary at
    // a large view distance wants dozens of columns at once, and sending them
    // all here would put the same stall back that the join just lost — the
    // loop's own ticker takes the rest.
    stream_up_to(conn, ctx, view, centre, STREAM_BATCH).await
}

/// The block state a right-click puts down.
///
/// The held item's block, in the state `dust_sim::placement` computes for the
/// click — the face, where on it the cursor was, and which way the player is
/// looking. What that crate has no rule for keeps its default state, and
/// `cargo xtask harness placement` is what says how many of those there are.
///
/// Falls back to [`PlaceableBlocks::placeable`] when there is nothing to look
/// up with: no table beside the data, an empty hand, or an item that places no
/// block. All three used to be the only case, and the fallback is what keeps a
/// server with no `[data] path` behaving the way it always has rather than
/// refusing right-clicks.
fn held_block(
    holding: Option<(dust_registry::Block, Option<dust_registry::WallForm>)>,
    fallback: u32,
    hit: &play::serverbound::BlockHit,
    rotation: (f32, f32),
    into: dust_registry::BlockState,
) -> u32 {
    let Some((block, wall)) = holding else {
        return fallback;
    };
    // A face the protocol does not have is one this server has no answer for;
    // vanilla refuses the packet outright and `offset` has already decided to
    // treat it as the clicked block, so the least surprising thing left is to
    // place the block's own default and let the rest of the click be as wrong
    // as it already is.
    let Some(click) = click(hit, rotation, into) else {
        return block.default_state().id();
    };
    dust_sim::placement::state_for_item(block, wall, click).id()
}

/// The block a held item puts down, if there is a table to ask and an item to
/// ask about.
///
/// Split out of [`held_block`] because the answer is needed *twice* and a
/// second lookup is a second chance to disagree: once to choose where the
/// placement lands — snow stacks and a slab doubles only under their own item
/// — and once to choose the state that goes there.
fn held_place(
    table: Option<&dust_registry::ItemBlocks>,
    held: Option<dust_registry::Item>,
) -> Option<(dust_registry::Block, Option<dust_registry::WallForm>)> {
    let (table, item) = table.zip(held)?;
    let block = table.places(item)?;
    // The wall form, for the fifty-three items that have two blocks. A table
    // written before those columns says nothing rather than *no*, and
    // `has_walls` is the question that tells the two apart — the same trap the
    // `replaceable` column already paid for once.
    let wall = table.has_walls().then(|| table.on_wall(item)).flatten();
    Some((block, wall))
}

/// A right-click, as `dust_sim::placement` reads one.
fn click(
    hit: &play::serverbound::BlockHit,
    rotation: (f32, f32),
    into: dust_registry::BlockState,
) -> Option<dust_sim::placement::Click> {
    Some(dust_sim::placement::Click {
        face: dust_sim::placement::Face::from_protocol(hit.face)?,
        cursor_y: hit.cursor_y,
        yaw: rotation.0,
        pitch: rotation.1,
        into,
    })
}

/// What a cell holds, as a state rather than an id.
///
/// An id the world holds that this build has no state for is read as air. The
/// alternative is a panic on a world saved by a different version, and the
/// rules that read this cell all treat air as "nothing to say" — which is the
/// right answer for a block nobody here can describe.
fn cell(
    world: &super::edits::EditedWorld,
    at: dust_protocol::types::Position,
) -> dust_registry::BlockState {
    dust_registry::BlockState::from_id(world.block_at(at)).unwrap_or_else(|| {
        dust_registry::Block::from_name("minecraft:air")
            .expect("every version of the game has air")
            .default_state()
    })
}

/// Whether a player at `feet` may act on the block at `location`.
///
/// The position is the last one `dust_guard::Movement` accepted, which is
/// where the player could have walked to and not merely where they said they
/// were. Between the two checks the cheat the README names is closed from both
/// ends: this refuses acting far from where the player is, and the movement
/// check refuses being somewhere they could not have got to.
///
/// The eye height is the one that belongs to the pose the player's own packets
/// describe: 1.62 standing, 1.27 crouching, 0.4 gliding. It used to be 1.62
/// whatever they were doing, which measured a crouching player **0.35 too
/// high** — the wrong direction at a ledge edge, where crouching is the single
/// most common thing a player does deliberately.
fn within_reach(
    ctx: &SessionContext,
    feet: (f64, f64, f64),
    pose: dust_guard::Pose,
    location: dust_protocol::types::Position,
) -> bool {
    ctx.reach.allows(
        dust_guard::eye_of(feet, pose),
        (location.x, location.y, location.z),
    )
}

/// Where a right-click on `face` of `clicked` actually puts a block, if
/// anywhere.
///
/// Two questions, in vanilla's order, and both are the same question asked of a
/// different cell.
///
/// **Is the clicked block itself replaceable?** Then the placement goes *into*
/// it. Right-clicking tall grass puts the block where the grass was rather than
/// on top of it, and the same for snow, water and fire. Getting this wrong is
/// the difference between building a wall through a meadow and building a wall
/// one block above it.
///
/// **Otherwise, is the cell on that face replaceable?** Then the placement goes
/// there, and if it is not, nothing happens. That second refusal is what stops
/// a player right-clicking into the side of a wall and replacing the block
/// behind it — which is what this did before, silently, for every solid cell.
///
/// # What this is not
///
/// Minecraft's own answer is `canBeReplaced(state, BlockPlaceContext)`, which
/// is this property *and* a question about what the player is holding — and
/// that second half is now asked. `dust_sim::placement::replaces_clicked` and
/// `replaces_beside` are the two shapes of it: **deep snow may only be
/// replaced by more snow, and a slab only by its own other half.** They are
/// two functions rather than one because the clicked cell and the cell behind
/// it are different questions: a bottom slab clicked on its *top* face doubles,
/// and the same slab clicked from underneath does not — the placement goes
/// below it instead.
///
/// Nothing here validates *reach*, either — a player may still place a block
/// from across the map, which is stated with the rest of the missing rules in
/// [`super::edits`].
///
/// A server with no table, or with one written before the column, keeps the
/// rule it had: always the face, never a refusal. An operator who has not
/// copied a file should have the server they had rather than one that ignores
/// right-clicks.
fn placement(
    world: &super::edits::EditedWorld,
    constants: Option<&dust_registry::BlockConstants>,
    clicked: dust_protocol::types::Position,
    hit: &play::serverbound::BlockHit,
    holding: Option<(dust_registry::Block, Option<dust_registry::WallForm>)>,
    rotation: (f32, f32),
) -> Option<dust_protocol::types::Position> {
    let beside = offset(clicked, hit.face);
    // With nothing to ask, the old rule stands: the face, always, and never a
    // refusal. **Not `BlockConstants::replaceable`'s own default**, which
    // answers true and would send every placement into the block that was
    // clicked — that default is for a caller reading one state, and this caller
    // is choosing between two. The question here is whether the table *knows*,
    // and `has_replaceable` is the one that asks it.
    let Some(table) = constants.filter(|table| table.has_replaceable()) else {
        return Some(beside);
    };
    // And with no block in hand there is nothing for the item-aware half of
    // the rule to be about, so it is the plain column on its own — exactly
    // what this did before that half existed.
    let (Some((block, _)), Some(click)) = (holding, click(hit, rotation, cell(world, clicked)))
    else {
        return if table.replaceable(world.block_at(clicked)) {
            Some(clicked)
        } else {
            table.replaceable(world.block_at(beside)).then_some(beside)
        };
    };
    let there = cell(world, clicked);
    if dust_sim::placement::replaces_clicked(block, there, table.replaceable(there.id()), click) {
        return Some(clicked);
    }
    let past = cell(world, beside);
    dust_sim::placement::replaces_beside(block, past, table.replaceable(past.id()))
        .then_some(beside)
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

/// The window id a player's own inventory always has.
///
/// Zero, and it is a constant rather than a literal because every container
/// packet carries one and the day a chest opens there will be a second answer.
/// A click naming any other window is a click on a container this server never
/// opened, and is ignored rather than replayed against the player's own.
const PLAYER_WINDOW: u8 = 0;

/// The window id a slot correction carries.
///
/// **Zero, and it was -2 until a second client said otherwise.** The protocol
/// gives `container_set_slot` a signed window id so that `-2` can mean "the
/// player's own inventory, and do not check the state id", which reads like
/// exactly the right thing for a correction — Mojang's client honours it. It is
/// still the wrong choice, and the reason is that it is the *only* thing that
/// honours it: pointed at a server that corrected on `-2`, mineflayer dropped
/// every correction on the floor, silently, because its handler resolves a
/// window by id and there is no window `-2`. Four checks failed and nothing
/// anywhere said why.
///
/// Zero is the id vanilla's own `ContainerSynchronizer` sends for a player's
/// own menu, both clients honour it, and it is what this server has already
/// told the client the window is. A correction one client cannot see is not a
/// correction, and the fact that the other one can see it is not a defence.
const CORRECTION_WINDOW: i8 = PLAYER_WINDOW as i8;

/// Send the whole container, plus whatever is on the cursor.
///
/// Forty-seven stacks on the wire. Only on a join and after a close, because
/// every other change knows which slot it was — see [`send_slot`].
async fn send_container<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    inventory: &mut super::inventory::Inventory,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state_id = inventory.next_state_id();
    let slots = inventory
        .slots()
        .iter()
        .map(|stack| super::inventory::to_wire(stack.as_ref()))
        .collect();
    send_play(
        conn,
        play::clientbound::ContainerSetContent {
            window_id: PLAYER_WINDOW,
            state_id: VarInt(state_id),
            slots,
            carried_item: super::inventory::to_wire(inventory.cursor()),
        },
        ctx.version,
    )
    .await
}

/// Send one slot.
///
/// The packet that exists so a pickup does not cost a whole container. One
/// stack instead of forty-seven, for every player on every change.
async fn send_slot<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    inventory: &mut super::inventory::Inventory,
    index: usize,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state_id = inventory.next_state_id();
    let item = super::inventory::to_wire(inventory.slot(index));
    send_play(
        conn,
        play::clientbound::ContainerSetSlot {
            window_id: CORRECTION_WINDOW,
            state_id: VarInt(state_id),
            slot: index as i16,
            item,
        },
        ctx.version,
    )
    .await
}

/// Send the cursor.
///
/// Slot -1 of window -1, which is how the protocol addresses the thing that is
/// not in the container at all — and unlike the correction above there is no
/// second spelling to prefer, so this one stays negative. mineflayer ignores it
/// too and keeps its own cursor; Mojang's client applies it. That asymmetry is
/// why the bot check reads slots and not the cursor.
async fn send_cursor<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    inventory: &mut super::inventory::Inventory,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state_id = inventory.next_state_id();
    let item = super::inventory::to_wire(inventory.cursor());
    send_play(
        conn,
        play::clientbound::ContainerSetSlot {
            window_id: -1,
            state_id: VarInt(state_id),
            slot: -1,
            item,
        },
        ctx.version,
    )
    .await
}

/// Tell the client the slots it is now wrong about, and only those.
///
/// A click packet carries the client's own opinion of what it changed, which is
/// the whole design of `Click Container`: the client predicts, the server
/// replays, and the disagreement — not the result — is what goes back. So the
/// set to correct is the union of two things, and leaving either one out is a
/// desynchronised inventory:
///
/// - **slots the server moved**, because a shift-click can move a stack the
///   client put somewhere else, and
/// - **slots the client says it moved**, because a click the server declined
///   entirely moves nothing and would otherwise send nothing back.
///
/// A slot the client already agrees about is not sent. In the ordinary case —
/// a left click the client predicted correctly — that is zero packets.
async fn push_back<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    inventory: &mut super::inventory::Inventory,
    changed: super::inventory::Changed,
    click: &play::serverbound::ClickContainer,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    for index in changed.iter() {
        let claimed = click
            .changed_slots
            .iter()
            .find(|slot| usize::try_from(slot.number) == Ok(index));
        let agrees = claimed.is_some_and(|slot| {
            super::inventory::from_wire(&slot.item).as_ref() == inventory.slot(index)
        });
        if !agrees {
            send_slot(conn, ctx, inventory, index).await?;
        }
    }
    for slot in &click.changed_slots {
        let Ok(index) = usize::try_from(slot.number) else {
            continue;
        };
        if index >= super::inventory::SLOTS || changed.has(index) {
            continue;
        }
        if super::inventory::from_wire(&slot.item).as_ref() != inventory.slot(index) {
            send_slot(conn, ctx, inventory, index).await?;
        }
    }
    if super::inventory::from_wire(&click.cursor_item).as_ref() != inventory.cursor() {
        send_cursor(conn, ctx, inventory).await?;
    }
    Ok(())
}

/// The container moved: put it where a shutdown and the next session can find
/// it, and tell everybody what of it shows.
///
/// The two travel together on purpose. There are five places a player's
/// container changes — a pickup, a hotbar key, a creative write, a click and
/// a close — and a rule spelled at five call sites is a rule that is wrong at
/// one of them. Both halves are wanted at all five, so both live here and a
/// caller cannot remember one and forget the other.
///
/// Called when a slot moves rather than on a timer: a click is a few times a
/// minute where a movement packet is twenty a second, so this can afford to
/// copy the container while [`record`] cannot afford to copy three floats
/// twice. The roster call is a six-slot comparison and sends nothing at all
/// when nothing visible moved, which is every click in the main inventory.
fn record_inventory(
    ctx: &SessionContext,
    profile_id: [u8; 16],
    entity_id: i32,
    inventory: &super::inventory::Inventory,
) {
    ctx.inventories
        .lock()
        .expect("the inventory map is never poisoned")
        .insert(
            profile_id,
            super::save::Carried {
                slots: inventory.slots().clone(),
                selected: inventory.selected(),
            },
        );
    ctx.roster.equipped(entity_id, inventory.equipment());
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

/// How often a session asks whether its player is standing on anything.
///
/// One tick. A player walks 0.28 blocks in one, and the pickup reach is 1.4,
/// so nothing can be walked past between two asks.
const PICKUP_PERIOD: std::time::Duration = std::time::Duration::from_millis(50);

/// The cell above one.
fn above(position: dust_protocol::types::Position) -> dust_protocol::types::Position {
    dust_protocol::types::Position {
        y: position.y.saturating_add(1),
        ..position
    }
}

/// The cell below one.
fn below(position: dust_protocol::types::Position) -> dust_protocol::types::Position {
    dust_protocol::types::Position {
        y: position.y.saturating_sub(1),
        ..position
    }
}

/// Roll a broken block's loot table and put what came out on the ground.
///
/// Called only when the break actually changed the world. Everything it needs
/// is read before the break, because after it the cell is air.
///
/// **The count is split into stacks here and not in `dust-sim`**, because only
/// this layer knows what a stack is: a loot table's `set_count` of eight and a
/// fortune-multiplied ore are both one number that has nothing to do with the
/// sixty-four a chest slot holds.
/// A break a survival player has started and the server has not finished.
///
/// Four fields and no allocation, held for the length of one break by the
/// session that is running it. **The progress is computed once, when the click
/// arrives, and never again** — the hardness of the block and the speed of the
/// tool cannot change under a player who is holding the button down, and
/// asking the world again every tick would put a chunk lookup on the tick loop
/// of every mining player for an answer that is already known.
#[derive(Debug, Clone, Copy)]
struct Digging {
    /// The cell. A start on a different cell replaces this one, which is what
    /// a player who looked away and clicked something else means.
    at: dust_protocol::types::Position,
    /// When the start packet arrived. Wall time rather than a server tick
    /// count: see [`dust_sim::mining::Progress::ticks_elapsed`].
    started: std::time::Instant,
    /// How much of the break one tick is worth.
    progress: dust_sim::mining::Progress,
    /// Set by a stop that arrived before the 70% the server believes. The
    /// break then finishes on the server's own count instead of being refused,
    /// which is the difference between a block that goes late and a block that
    /// does not go at all.
    delayed: bool,
}

/// How fast this player breaks this block, from one state and one held item.
///
/// The hardness comes out of `dust-constants.tsv` and the tool speed out of
/// the item's own `minecraft:tool` component, so **both numbers are the
/// operator's jar's and neither is Dust's guess**. A table with no hardness
/// column at all is a table extracted before decision record 0027, and the
/// answer for it is an instant break: a server that mines fast looks generous
/// and a server that will not let a player mine looks broken.
fn break_progress(
    ctx: &SessionContext,
    state: u32,
    held: Option<dust_registry::Item>,
    on_ground: bool,
) -> dust_sim::mining::Progress {
    let Some(constants) = ctx.constants.as_deref() else {
        return dust_sim::mining::Progress::of(0.0, &dust_sim::mining::Digger::bare());
    };
    let Some(hardness) = constants.destroy_speed(state) else {
        return dust_sim::mining::Progress::of(0.0, &dust_sim::mining::Digger::bare());
    };
    let Some(block) = dust_registry::BlockState::from_id(state).map(|s| s.block()) else {
        return dust_sim::mining::Progress::of(0.0, &dust_sim::mining::Digger::bare());
    };
    let digger = dust_sim::mining::Digger {
        speed: dust_registry::mining::speed(held, block),
        // A stack carries its components as bytes and nothing decodes
        // `minecraft:enchantments` out of them yet, so every efficiency
        // branch takes its unenchanted side. That is a gap in the stack and
        // not in the rule: the day a stack knows, this line is where it
        // starts working, and it is the same seam `spill` names for silk
        // touch and fortune.
        efficiency: 0,
        // **Whether the tool is right for the drops, not whether the block
        // asks for one.** The wrong tool costs time on every block, including
        // the ones that drop themselves to a bare hand — an axe on oak planks
        // is three times faster than a pickaxe of the same speed, and a server
        // that read `requires_tool` here would make them the same.
        correct: dust_registry::mining::correct_for_drops(held, block),
        on_ground,
    };
    dust_sim::mining::Progress::of(hardness, &digger)
}

fn spill(
    ctx: &SessionContext,
    at: dust_protocol::types::Position,
    previous: u32,
    neighbours: &[(i8, u32)],
    held: Option<dust_registry::Item>,
    seed: u64,
) {
    let Some(state) = dust_registry::BlockState::from_id(previous) else {
        return;
    };
    let Some(table) = ctx.drops.table(state.block()) else {
        // No table for this block, which is not "drops nothing" — see
        // `dust_sim::drops::Tables::table`. Nothing is dropped either way, and
        // the difference is why this branch is written down rather than being
        // the same `return` as an empty roll.
        return;
    };
    let around: Vec<(i8, dust_registry::BlockState)> = neighbours
        .iter()
        .filter_map(|(offset, state)| {
            dust_registry::BlockState::from_id(*state).map(|state| (*offset, state))
        })
        .collect();
    let context = dust_sim::drops::Break {
        state,
        // Whether this state yields anything to the wrong tool. Read off the
        // same table the sound comes from, with the column resolved at boot.
        // Whether the tool in the hand is the *right* one is not asked here:
        // that is the item's own `minecraft:tool` component, and `dust-sim`
        // reads it, so the server and `cargo xtask harness drops` are asking
        // one implementation rather than agreeing with each other.
        requires_tool: match (ctx.constants.as_deref(), ctx.requires_tool) {
            (Some(constants), Some(flag)) => constants.is_set(flag, previous),
            _ => false,
        },
        tool: dust_sim::drops::Tool {
            item: held,
            // A stack carries no data components yet, so it carries no
            // enchantments and every silk-touch and fortune branch in every
            // table takes its unenchanted side. That is a gap in the stack and
            // not in the table: the day a stack knows, this line is where it
            // starts working.
            enchantments: &[],
        },
        broken_by_entity: true,
        neighbours: &around,
    };
    let mut rolled = Vec::new();
    let mut rng = dust_sim::drops::Rng::from_seed(seed);
    table.roll(&context, &mut rng, &mut rolled);
    for drop in rolled {
        let limit = u32::from(drop.item.max_stack_size().max(1));
        let mut left = drop.count;
        while left > 0 {
            let taken = left.min(limit);
            left -= taken;
            ctx.items.pop(
                &ctx.roster,
                at,
                drop.item,
                u8::try_from(taken).unwrap_or(u8::MAX),
                seed ^ u64::from(left),
            );
        }
    }
}

/// Put one item change on the wire.
///
/// A spawn is two packets and not one: `AddEntity` says an item entity is
/// there and `SetEntityData` says which item, and an item entity without the
/// second renders as nothing at all.
async fn relay_item<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
    change: &super::items::ItemChange,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use super::items::ItemChange;
    match change {
        ItemChange::Spawned {
            id, item, count, ..
        } => {
            if let Some(packet) = super::items::spawn(change, ctx.item_entity_type) {
                send_play(conn, packet, ctx.version).await?;
            }
            send_play(
                conn,
                super::items::contents(*id, *item, *count),
                ctx.version,
            )
            .await?;
        }
        ItemChange::Settled { id, x, y, z } => {
            send_play(
                conn,
                play::clientbound::TeleportEntity {
                    entity_id: VarInt(*id),
                    x: *x,
                    y: *y,
                    z: *z,
                    yaw: dust_protocol::types::Angle::from_degrees(0.0),
                    pitch: dust_protocol::types::Angle::from_degrees(0.0),
                    on_ground: true,
                },
                ctx.version,
            )
            .await?;
        }
        ItemChange::Collected { id, by, count, .. } => {
            // The animation first and the removal second. The other order is
            // an item that vanishes and then is told to fly somewhere.
            send_play(
                conn,
                play::clientbound::TakeItemEntity {
                    collected_entity_id: VarInt(*id),
                    collector_entity_id: VarInt(*by),
                    pickup_item_count: VarInt(i32::from(*count)),
                },
                ctx.version,
            )
            .await?;
            send_play(conn, super::play::despawn(*id), ctx.version).await?;
        }
        ItemChange::Removed { id, .. } => {
            send_play(conn, super::play::despawn(*id), ctx.version).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_protocol::types::Position;
    use dust_registry::{Block, Item, ItemBlocks};

    /// A table where every item places the block of its own name if there is
    /// one, plus the one row that says a table is not a name match.
    ///
    /// Not Minecraft's answers — none of those are in this repository, which is
    /// decision record 0008. What it can carry is the *shape* of the case:
    /// `minecraft:wheat_seeds` placing something other than a block called
    /// `minecraft:wheat_seeds`.
    fn table() -> ItemBlocks {
        let mut text = String::from("# item_id\titem\tplaces\n");
        for item in Item::all() {
            let places = if item.name() == "minecraft:wheat_seeds" {
                "minecraft:wheat"
            } else {
                Block::from_name(item.name()).map_or("-", Block::name)
            };
            text.push_str(&format!(
                "{}\t{}\t{places}\n",
                item.protocol_id(),
                item.name()
            ));
        }
        ItemBlocks::parse(&text).expect("a complete table")
    }

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("this build has that item")
    }

    /// A state id no block has, so a test that got the fallback back can say so
    /// rather than matching some real block by accident.
    const FALLBACK: u32 = u32::MAX;

    /// A click on the top of a block, looking south, low on the face.
    ///
    /// The situation the placement rules answer most plainly, so a test about
    /// *which block* goes down is not also a test about which state.
    /// A state id read back as a state, so a test can name the property it is
    /// about rather than an integer.
    trait PipeState {
        fn pipe_state(self) -> dust_registry::BlockState;
    }

    impl PipeState for u32 {
        fn pipe_state(self) -> dust_registry::BlockState {
            dust_registry::BlockState::from_id(self).expect("a real state")
        }
    }

    /// An empty cell, which is what a placement landed in before this server
    /// asked what was already there.
    fn nothing() -> dust_registry::BlockState {
        dust_registry::Block::from_name("minecraft:air")
            .expect("this build has air")
            .default_state()
    }

    /// A click on one face of the block at the origin.
    ///
    /// The location is ignored — every caller passes the position it means
    /// separately — so what this carries is the face and the cursor, which is
    /// all the two rules under test read.
    fn hit(face: u8, cursor_y: f32) -> play::serverbound::BlockHit {
        play::serverbound::BlockHit {
            location: Position { x: 0, y: 0, z: 0 },
            face,
            cursor_x: 0.5,
            cursor_y,
            cursor_z: 0.5,
            inside_block: false,
        }
    }

    fn on_top() -> play::serverbound::BlockHit {
        play::serverbound::BlockHit {
            location: Position { x: 0, y: 0, z: 0 },
            face: 1,
            cursor_x: 0.5,
            cursor_y: 0.5,
            cursor_z: 0.5,
            inside_block: false,
        }
    }

    /// A flat world to click at, and what is in it.
    fn world() -> super::super::edits::EditedWorld {
        let palette = super::world::Palette::resolve().expect("the block table");
        super::super::edits::EditedWorld::new(super::super::source::Source::Flat(Box::new(
            super::world::FlatWorld::new(palette, 0, 64),
        )))
    }

    /// A constants table where exactly the states of `replaceable` are.
    ///
    /// Named blocks rather than parity, because the whole question is about
    /// specific blocks: the grass a wall is built through, and the stone it is
    /// built against.
    fn replaceable(names: &[&str]) -> dust_registry::BlockConstants {
        let states: std::collections::HashSet<u32> = names
            .iter()
            .flat_map(|name| {
                Block::from_name(name)
                    .expect("this build has that block")
                    .states()
                    .map(|state| state.id())
            })
            .collect();
        let mut text = String::from(
            "# state_id	opacity	emission	occlude	replaceable
",
        );
        for state in 0..dust_registry::STATE_COUNT {
            text.push_str(&format!(
                "{state}	0	0	1	{}
",
                u32::from(states.contains(&state))
            ));
        }
        dust_registry::BlockConstants::parse(&text).expect("a complete table")
    }

    /// The cell the flat world's surface block is in, and the air above it.
    fn surface() -> Position {
        Position {
            x: 6,
            y: super::world::SURFACE_Y,
            z: 6,
        }
    }

    #[test]
    fn a_click_on_solid_ground_puts_the_block_on_the_face() {
        // Face 1 is up. The block goes above the ground rather than into it,
        // which is the case that has always worked and the one everything else
        // here is a departure from.
        let world = world();
        let table = replaceable(&["minecraft:air"]);
        let ground = surface();
        assert_eq!(
            placement(&world, Some(&table), ground, &hit(1, 0.5), None, (0.0, 0.0)),
            Some(Position {
                y: ground.y + 1,
                ..ground
            })
        );
    }

    #[test]
    fn a_click_on_something_replaceable_puts_the_block_into_it() {
        // Right-clicking tall grass puts the block *where the grass was*. A
        // server that always went to the face builds a wall one block above
        // the meadow it was meant to cross.
        let world = world();
        let grass = Block::from_name("minecraft:short_grass").expect("this build has it");
        let table = replaceable(&["minecraft:air", "minecraft:short_grass"]);
        let at = Position {
            y: surface().y + 1,
            ..surface()
        };
        assert!(world.set_block(at, grass.default_state().id()));
        assert_eq!(
            placement(&world, Some(&table), at, &hit(1, 0.5), None, (0.0, 0.0)),
            Some(at),
            "into the grass, not above it"
        );
    }

    #[test]
    fn a_click_into_a_solid_neighbour_places_nothing() {
        // The refusal this rule exists for. Clicking the side of a block whose
        // neighbour is also solid used to replace that neighbour, silently, for
        // every solid cell in the world — a player could hollow a wall out from
        // the outside without breaking anything.
        let world = world();
        let table = replaceable(&["minecraft:air"]);
        let buried = Position {
            y: surface().y - 1,
            ..surface()
        };
        // Face 1 is up, and the cell above a buried block is more ground.
        assert_eq!(
            placement(&world, Some(&table), buried, &hit(1, 0.5), None, (0.0, 0.0)),
            None
        );
    }

    #[test]
    fn a_ninth_layer_of_snow_goes_on_top_of_the_drift_and_a_fourth_goes_into_it() {
        // The item-aware half of `canBeReplaced`, which the plain column
        // cannot express: the *same state* is replaceable by a shovel-load of
        // anything else and not by more snow. Eight layers is a full block a
        // player walks on, and a ninth used to flatten it back to one.
        let world = world();
        let snow = Block::from_name("minecraft:snow").expect("this build has snow");
        let table = replaceable(&["minecraft:air", "minecraft:snow"]);
        let at = Position {
            y: surface().y + 1,
            ..surface()
        };
        let deep = snow
            .default_state()
            .with("layers", "8")
            .expect("snow has layers");
        assert!(world.set_block(at, deep.id()));
        assert_eq!(world.block_at(at), deep.id(), "the drift is really there");
        assert_eq!(
            placement(
                &world,
                Some(&table),
                at,
                &hit(1, 0.5),
                Some((snow, None)),
                (0.0, 0.0)
            ),
            Some(Position { y: at.y + 1, ..at }),
            "a ninth layer goes on top of the drift and does not flatten it"
        );
        // A shallower drift takes the layer instead, which is the same cell
        // answering the opposite way to the same click.
        let shallow = snow
            .default_state()
            .with("layers", "3")
            .expect("snow has layers");
        assert!(world.set_block(at, shallow.id()));
        assert_eq!(
            placement(
                &world,
                Some(&table),
                at,
                &hit(1, 0.5),
                Some((snow, None)),
                (0.0, 0.0)
            ),
            Some(at),
            "into the drift, which then gets a fourth layer"
        );
        assert_eq!(
            held_block(
                Some((snow, None)),
                FALLBACK,
                &hit(1, 0.5),
                (0.0, 0.0),
                shallow
            )
            .pipe_state()
            .property("layers"),
            Some("4")
        );
    }

    #[test]
    fn a_block_placed_into_water_comes_out_waterlogged() {
        // The wiring, end to end: the cell is read, it reaches the rule, and
        // the state that comes back is wet. Before this, a fence post in a
        // river replaced the water and left a dry hole in it.
        let world = world();
        let water = Block::from_name("minecraft:water").expect("this build has water");
        let fence = Block::from_name("minecraft:oak_fence").expect("this build has fences");
        let table = replaceable(&["minecraft:air", "minecraft:water"]);
        let at = Position {
            y: surface().y + 1,
            ..surface()
        };
        assert!(world.set_block(at, water.default_state().id()));
        let target = placement(
            &world,
            Some(&table),
            at,
            &hit(1, 0.5),
            Some((fence, None)),
            (0.0, 0.0),
        );
        assert_eq!(target, Some(at), "into the water and not above it");
        assert_eq!(
            held_block(
                Some((fence, None)),
                FALLBACK,
                &hit(1, 0.5),
                (0.0, 0.0),
                cell(&world, at)
            )
            .pipe_state()
            .property("waterlogged"),
            Some("true")
        );
    }

    #[test]
    fn a_table_written_before_the_column_also_places_on_the_face() {
        // The trap this rule nearly walked into. `BlockConstants::replaceable`
        // answers *true* for an absent column — the right default for a caller
        // reading one state — and a chooser between two cells that took it at
        // face value would send every placement into the block that was
        // clicked, which is neither the old behaviour nor Minecraft's.
        let world = world();
        let mut text = String::from(
            "# state_id	opacity	emission	occlude
",
        );
        for state in 0..dust_registry::STATE_COUNT {
            text.push_str(&format!(
                "{state}	0	0	1
"
            ));
        }
        let old = dust_registry::BlockConstants::parse(&text).expect("a complete table");
        assert!(!old.has_replaceable());
        let ground = surface();
        assert_eq!(
            placement(&world, Some(&old), ground, &hit(1, 0.5), None, (0.0, 0.0)),
            Some(Position {
                y: ground.y + 1,
                ..ground
            }),
            "the face, not the block that was clicked"
        );
    }

    #[test]
    fn a_server_with_no_table_places_on_the_face_the_way_it_always_did() {
        // Absent is the old behaviour and not a refusal, by the same argument
        // the light table's absence is made with: an operator who has not
        // copied a file should have the server they had, not one that ignores
        // right-clicks.
        let world = world();
        let buried = Position {
            y: surface().y - 1,
            ..surface()
        };
        assert_eq!(
            placement(&world, None, buried, &hit(1, 0.5), None, (0.0, 0.0)),
            Some(Position {
                y: buried.y + 1,
                ..buried
            })
        );
    }

    #[test]
    fn a_stair_goes_down_facing_the_way_the_player_stood() {
        // The rules crate has its own tests; this one is about the wiring —
        // that the click and the rotation reach it at all. Placed with the
        // player looking west, a stair faces west, and the same click with a
        // default state would have faced north.
        let table = table();
        let stairs = Block::from_name("minecraft:oak_stairs").expect("this build has stairs");
        let placed = held_block(
            held_place(Some(&table), Some(item("minecraft:oak_stairs"))),
            FALLBACK,
            &on_top(),
            (90.0, 0.0),
            nothing(),
        );
        assert_ne!(placed, stairs.default_state().id(), "not the default state");
        let state = dust_registry::BlockState::from_id(placed).expect("a real state");
        assert_eq!(state.property("facing"), Some("west"));
        assert_eq!(state.property("half"), Some("bottom"));
    }

    #[test]
    fn a_held_block_item_places_its_own_block() {
        let table = table();
        let expected = Block::from_name("minecraft:cobblestone")
            .expect("this build has cobblestone")
            .default_state()
            .id();
        assert_eq!(
            held_block(
                held_place(Some(&table), Some(item("minecraft:cobblestone"))),
                FALLBACK,
                &on_top(),
                (0.0, 0.0),
                nothing()
            ),
            expected
        );
    }

    #[test]
    fn an_item_whose_block_has_another_name_places_that_block() {
        // The row the table exists for. A server matching item names against
        // block names would look for a block called `minecraft:wheat_seeds`,
        // find none, and fall back — silently placing the wrong thing.
        let table = table();
        let wheat = Block::from_name("minecraft:wheat")
            .expect("this build has wheat")
            .default_state()
            .id();
        assert_eq!(
            held_block(
                held_place(Some(&table), Some(item("minecraft:wheat_seeds"))),
                FALLBACK,
                &on_top(),
                (0.0, 0.0),
                nothing()
            ),
            wheat
        );
        assert_ne!(wheat, FALLBACK, "and it is not the fallback wearing a hat");
    }

    #[test]
    fn an_empty_hand_and_an_item_that_places_nothing_both_fall_back() {
        let table = table();
        assert_eq!(
            held_block(
                held_place(Some(&table), None),
                FALLBACK,
                &on_top(),
                (0.0, 0.0),
                nothing()
            ),
            FALLBACK
        );
        assert_eq!(
            held_block(
                held_place(Some(&table), Some(item("minecraft:diamond_sword"))),
                FALLBACK,
                &on_top(),
                (0.0, 0.0),
                nothing()
            ),
            FALLBACK,
            "a sword places nothing, and nothing is the fallback and not air"
        );
    }

    #[test]
    fn a_server_with_no_table_places_what_it_always_did() {
        // Every server did this before the table existed, and one whose
        // operator has not copied the file still does. It is the fallback and
        // not a refusal: a right-click that did nothing would read as a
        // dropped packet.
        assert_eq!(
            held_block(
                held_place(None, Some(item("minecraft:cobblestone"))),
                FALLBACK,
                &on_top(),
                (0.0, 0.0),
                nothing()
            ),
            FALLBACK
        );
    }
}

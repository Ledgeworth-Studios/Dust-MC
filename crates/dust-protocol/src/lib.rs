//! Minecraft's wire protocol: which packet id means what, and what is in one.
//!
//! # What this crate is, and what it is not
//!
//! Every Minecraft frame starts with a packet id, and that id is meaningless on
//! its own. It is an index within one (connection state, direction) pair: id 0
//! is `minecraft:intention` from a client mid-handshake, `minecraft:status_response`
//! from a server answering a ping, and `minecraft:accept_teleportation` from a
//! client in play. [`generated::packets`] holds the table that turns the pair
//! and the number into a name and back.
//!
//! [`packets`] holds the **bodies** for every packet of handshake, status,
//! login and configuration — everything a connection uses before it reaches
//! Play — plus the core of Play itself: join and movement, the world and
//! entity families, chat and the tab list. What is missing from Play is a
//! list, not a secret: see [`packets::unclaimed_for`]. [`types`] holds the
//! field types those bodies are built from.
//!
//! What is *not* here, and is not missing:
//!
//! - **Framing, compression, encryption, sockets.** `dust-net` owns those. The
//!   contract between the two is [`wire`], and a body reaches this crate with
//!   its length prefix stripped and its compression and encryption undone.
//! - **NBT.** `dust-nbt` owns it. This crate delimits an NBT value and does not
//!   interpret it; see [`nbt`].
//! - **Registry contents.** `dust-registry` owns those. The configuration state
//!   carries registry sync, and this crate carries the entry ids and the blobs;
//!   see [`packets::common::RegistryEntry`]. The same holds inside Play: chunk
//!   sections are a blob plus a trait, and the world crate fills both in —
//!   see [`packets::play`].
//!
//! Those first three are **seams**, not gaps. Each one is a trait or an opaque
//! type that another crate will implement or replace, and each one has a
//! differential test in [`conformance`] written *now*, so that the day two
//! implementations of one format exist in this workspace, a disagreement
//! between them is a red test rather than a bug in production.
//!
//! # Why a version dimension exists before there is a second version
//!
//! Decision D3 targets 1.21.1 first and commits the protocol layer to being
//! multi-version from the first commit, because retrofitting that dimension
//! later is a rewrite and is the most common architectural regret in this
//! space. Packet ids are the sharpest case — they are renumbered nearly every
//! release — and field layouts are not far behind.
//!
//! So there is only one 1.21.1 today, and it is still reached as
//! [`version::V1_21_1`] — a row in a generated table, not a global. Every
//! lookup and every codec takes a [`ProtocolVersion`]. Adding 1.21.4 is
//! `cargo xtask extract --version 1.21.4` plus a triage pass over the packet
//! definitions, which the coverage test in [`packets`] will not let anybody
//! skip.
//!
//! [`ProtocolVersion`] is deliberately not an enum. An enum would make adding a
//! version a change to a type, and every `match` on it a thing to revisit; a
//! version here is data, and the only hand-written vocabulary is
//! [`ConnectionState`] and [`Direction`], which are the protocol's own fixed
//! shape rather than anything that varies per release.
//!
//! # What the tests do and do not prove
//!
//! `tests/packet_ids.rs` round-trips every packet id and `tests/packet_bodies.rs`
//! round-trips every packet body. Neither proves the format is right: a round
//! trip agrees with itself under any self-consistent convention, including a
//! wrong one. Three things outside this crate are what make the difference —
//! [`ProtocolVersion::samples`], taken from Mojang's report at extraction time;
//! the vector tables in [`conformance`], computed by a separate implementation
//! written from the format description; and `tests/vanilla_conformance.rs`,
//! which speaks to a real vanilla 1.21.1 server and is the only one of the
//! three that can catch a field list that is self-consistent, agrees with every
//! table here, and is not what Minecraft sends.

pub mod conformance;
pub mod generated;
pub mod nbt;
pub mod packets;
pub mod text;
pub mod types;
pub mod varint;
pub mod wire;

use generated::packets::VERSIONS;

pub use generated::packets::version;

/// The state a connection is in, which decides what a packet id means.
///
/// Hand-written rather than generated: these five are the protocol's fixed
/// shape, not a list that varies per release, and a version that added a sixth
/// would be a change this crate should be forced to think about rather than
/// absorb. The extractor holds the same five and refuses a report that has any
/// other, so a new state stops the extraction instead of quietly dropping every
/// packet in it.
///
/// The declaration order is load-bearing: it is the order the generated tables
/// are laid out in, and `self as usize` indexes them. That is checked rather
/// than trusted — each table carries the state it is for, and the test reads it
/// back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConnectionState {
    /// The first frame of any connection, which says where it is going next.
    Handshake,
    /// The server list ping.
    Status,
    /// Authentication, encryption and compression negotiation.
    Login,
    /// Registries and settings, before the world exists.
    Configuration,
    /// Everything else, for as long as the player is on the server.
    Play,
}

impl ConnectionState {
    /// Every state, in the order a connection moves through them.
    pub const ALL: [Self; 5] = [
        Self::Handshake,
        Self::Status,
        Self::Login,
        Self::Configuration,
        Self::Play,
    ];

    /// The name Mojang's report uses, which is also the one in the wiki and in
    /// every packet-capture tool.
    pub fn name(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Play => "play",
        }
    }

    /// A `match` rather than a scan of [`ALL`](Self::ALL), so that this answer
    /// does not move when the declaration order does. The golden sample rows
    /// carry state names and are resolved through here for exactly that reason.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "handshake" => Some(Self::Handshake),
            "status" => Some(Self::Status),
            "login" => Some(Self::Login),
            "configuration" => Some(Self::Configuration),
            "play" => Some(Self::Play),
            _ => None,
        }
    }
}

/// Who sent a packet. Also part of what a packet id means: the client and the
/// server number their packets independently, so id 0 in a state is two
/// different packets depending on which way it was travelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    /// Server to client.
    Clientbound,
    /// Client to server.
    Serverbound,
}

impl Direction {
    pub const ALL: [Self; 2] = [Self::Clientbound, Self::Serverbound];

    pub fn name(self) -> &'static str {
        match self {
            Self::Clientbound => "clientbound",
            Self::Serverbound => "serverbound",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "clientbound" => Some(Self::Clientbound),
            "serverbound" => Some(Self::Serverbound),
            _ => None,
        }
    }
}

/// One protocol version's packet tables, as the generated table holds it.
#[derive(Debug)]
pub struct VersionDef {
    /// The Minecraft version id, e.g. `1.21.1`.
    pub name: &'static str,
    /// The number a client sends in its handshake to ask for this version.
    pub protocol: i32,
    /// Ten tables, at `state as usize * 2 + direction as usize`.
    pub tables: &'static [PacketTable],
    /// `(state, direction, protocol id, packet name)` straight from Mojang's
    /// report. See [`ProtocolVersion::samples`].
    pub samples: &'static [(&'static str, &'static str, u32, &'static str)],
}

/// One (connection state, direction) pair's packets, for one version.
#[derive(Debug)]
pub struct PacketTable {
    /// The state these ids are meaningful in.
    pub state: ConnectionState,
    /// The direction these ids are meaningful in.
    pub direction: Direction,
    /// Packet names indexed by protocol id.
    ///
    /// An empty entry is an id the report defined no packet for. There are none
    /// on 1.21.1 — every pair numbers its packets `0..n` — and the hole is
    /// representable anyway, because a version that skipped an id would
    /// otherwise be generated with everything after the gap shifted down by
    /// one, which compiles and round-trips and puts the wrong packet on the
    /// wire.
    pub by_id: &'static [&'static str],
    /// Indices into [`by_id`](Self::by_id), ordered by the name at each index,
    /// so the name-to-id direction is a binary search. Holes are absent.
    pub by_name: &'static [u16],
}

impl PacketTable {
    /// The packet with this protocol id, or `None` if this pair has no such id.
    ///
    /// A direct index: an id is a position, which is what the extractor checks
    /// when it builds the table.
    pub fn name(&self, protocol_id: u32) -> Option<&'static str> {
        let name = *self.by_id.get(protocol_id as usize)?;
        (!name.is_empty()).then_some(name)
    }

    /// The protocol id of a packet, by its namespaced name.
    ///
    /// A bare name is not accepted, for the reason `dust-registry` gives: the
    /// two spellings are the same packet to a person and different strings to a
    /// lookup, and accepting both leaves every caller unsure which it holds.
    pub fn protocol_id(&self, name: &str) -> Option<u32> {
        self.by_name
            .binary_search_by(|&index| self.by_id[index as usize].cmp(name))
            .ok()
            .map(|position| u32::from(self.by_name[position]))
    }

    /// Every packet in this pair, in protocol id order, as id and name.
    pub fn packets(&self) -> impl Iterator<Item = (u32, &'static str)> + '_ {
        self.by_id
            .iter()
            .enumerate()
            .filter(|(_, name)| !name.is_empty())
            .map(|(id, name)| (id as u32, *name))
    }

    /// How many packets this pair defines, not counting ids nothing claimed.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// One protocol version, as an index into the generated version table.
///
/// Opaque and `Copy`. Call sites name a version through [`version`], hold this,
/// and pass it down; nothing outside this crate needs to know it is a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    /// Construct from a position in [`VERSIONS`]. Only the generated table may
    /// do this, because only it knows which position is which version.
    pub(crate) const fn at(index: u16) -> Self {
        Self(index)
    }

    /// Look a version up by its Minecraft version id, e.g. `1.21.1`.
    ///
    /// A linear scan, unlike the packet lookups: this runs once when a
    /// connection picks a version, over a table with as many rows as Dust
    /// supports Minecraft releases. The packet lookups run per frame, and are
    /// an index and a binary search for that reason.
    pub fn from_name(name: &str) -> Option<Self> {
        VERSIONS
            .iter()
            .position(|version| version.name == name)
            .map(|index| Self(index as u16))
    }

    /// Every version with a generated table.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..VERSIONS.len() as u16).map(Self)
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    /// The number a client puts in its handshake to ask for this version.
    ///
    /// Signed, because the field on the wire is a VarInt and a client may send
    /// any number at all including a negative one — resolving it is exactly
    /// where "no" gets said, so the type has to be able to hold the values that
    /// get refused.
    pub fn number(self) -> i32 {
        self.def().protocol
    }

    /// Resolve the number out of a client's handshake.
    ///
    /// `None` is the ordinary answer, not an error condition: it is what an
    /// unsupported client looks like, and the caller's job is to say so
    /// politely rather than to drop the connection. This is why the handshake
    /// packet carries a plain VarInt and not one of these.
    pub fn from_protocol_number(number: i32) -> Option<Self> {
        VERSIONS
            .iter()
            .position(|version| version.protocol == number)
            .map(|index| Self(index as u16))
    }

    /// The packets of one (state, direction) pair.
    ///
    /// Infallible: all ten pairs exist for every version, and a pair Mojang's
    /// report does not have — `handshake` / `clientbound`, because the server
    /// says nothing during a handshake — is present and empty rather than
    /// missing. A caller decoding a frame should not have to distinguish
    /// "no such pair" from "no such id in this pair"; both are a frame that
    /// does not belong on this connection.
    pub fn table(self, state: ConnectionState, direction: Direction) -> &'static PacketTable {
        &self.def().tables[state as usize * Direction::ALL.len() + direction as usize]
    }

    /// All ten tables, in the order [`table`](Self::table) indexes them.
    pub fn tables(self) -> &'static [PacketTable] {
        self.def().tables
    }

    /// Every packet as Mojang's report states it, as
    /// `(state, direction, protocol id, name)`.
    ///
    /// This is not a second copy of the table, and the difference is the point.
    /// It is rendered from the report rather than from anything the extractor
    /// derived, its state and direction are strings rather than the enums the
    /// tables are indexed by, and its rows are in the report's own name order.
    /// A table that is internally consistent and wrong — off by one, sorted by
    /// name, or in the wrong slot because the enum was reordered — round-trips
    /// perfectly and fails this.
    pub fn samples(self) -> &'static [(&'static str, &'static str, u32, &'static str)] {
        self.def().samples
    }

    /// The packet this id names, in this state and direction.
    pub fn packet_name(
        self,
        state: ConnectionState,
        direction: Direction,
        protocol_id: u32,
    ) -> Option<&'static str> {
        self.table(state, direction).name(protocol_id)
    }

    /// The id this packet is sent under, in this state and direction.
    pub fn protocol_id(
        self,
        state: ConnectionState,
        direction: Direction,
        name: &str,
    ) -> Option<u32> {
        self.table(state, direction).protocol_id(name)
    }

    fn def(self) -> &'static VersionDef {
        &VERSIONS[self.0 as usize]
    }
}

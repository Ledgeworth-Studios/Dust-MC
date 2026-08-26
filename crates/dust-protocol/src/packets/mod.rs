//! The packet bodies, state by state and direction by direction.
//!
//! # Scope
//!
//! Handshake, status, login and configuration are complete: that is what a
//! client has to get through before it reaches Play, and every packet those
//! four states can carry is defined. Play is defined family by family — join
//! and movement first, then the world and entity families, then chat — and is
//! deliberately not finished; [`unclaimed_for`] is the worklist of what is
//! left.
//!
//! Do not confuse this module with [`crate::generated::packets`], which is the
//! id table. This one is bodies; that one is names and numbers.
//!
//! # Hand-written definitions, and the argument for them
//!
//! The Build Plan asks for packet definitions generated from the version data,
//! so that a version bump is a regeneration. That is the right instinct and it
//! cannot be followed here, for a reason that is a fact about Mojang's data
//! rather than a preference:
//!
//! **`packets.json` contains names and numbers and no field layouts at all.**
//! Nothing in any report Minecraft's data generators emit describes the body of
//! a packet. The layouts exist in the server jar as `StreamCodec` values —
//! composed at class-initialisation time out of combinators, not laid out as a
//! readable sequence of reads — so extracting them means either running the JVM
//! and reflecting over obfuscated fields with a mappings file, or writing a
//! bytecode interpreter that evaluates static initialisers. Both are a project
//! rather than a task, and neither produces anything usable until it is
//! finished: there is no half-extracted layout that decodes half a packet.
//!
//! So the definitions are hand-written. The cost of that is drift — the ids are
//! generated and the bodies are not, and nothing would normally connect them.
//! Three things connect them here, and they are the reason this is a
//! declarative macro rather than forty hand-rolled `impl` blocks:
//!
//! 1. **A definition never writes an id.** [`packet_group!`] takes the packet's
//!    namespaced *name* and looks the number up in the generated table at
//!    dispatch time, per version. A release that renumbers every packet in the
//!    protocol needs no edit here at all — which is most of what a version bump
//!    actually does to packet definitions.
//! 2. **A definition is data.** The macro body is a table of field names and
//!    field types. It is authored by hand and it is still machine-readable, so
//!    the day the layouts do become extractable, what changes is who writes the
//!    table and not what reads it.
//! 3. **Coverage is checked, in both directions.** [`undefined_in`] compares
//!    the generated table against the definitions. A packet in the table with
//!    no definition is a failure, and a definition naming a packet that is not
//!    in the table is a failure. When 1.21.4's table is generated, every packet
//!    whose definition does not claim 1.21.4 turns the suite red, and somebody
//!    has to look at each one and say whether the layout moved.
//!
//! Point 3 is the one that answers D3. A definition that is right for 1.21.1
//! and silently wrong for 1.21.4 is exactly the failure D3 exists to prevent,
//! and the guard against it is a version list on every definition plus a test
//! that no packet in a version's table is unclaimed. The forward half of the
//! guard applies per pair — see [`COMPLETE_PAIRS`] for why Play is held to it
//! only once its last packet is written. The guard is tested with a version
//! that does not exist, so that it is known to bite rather than assumed
//! to — see the tests at the bottom of this file.
//!
//! # What the definitions do not prove
//!
//! That a layout is right. A hand-written field list can be wrong in a way
//! every test in this crate agrees with, because every test in this crate reads
//! the same list. The check that a layout is right is a live vanilla server:
//! decoding what a real 1.21.1 server actually sends and insisting that every
//! packet ends exactly where the definition says it does. Nothing here can do
//! that job from inside the crate.

pub mod common;
pub mod configuration;
pub mod handshake;
pub mod login;
pub mod play;
pub mod status;

use crate::{ConnectionState, Direction, ProtocolVersion};

/// What a packet definition claims about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketMeta {
    /// The namespaced name, which is the key into the generated id table.
    pub name: &'static str,
    pub state: ConnectionState,
    pub direction: Direction,
    /// The Minecraft versions this layout has been checked against.
    ///
    /// A list rather than a range: "1.21.1 and 1.21.4" is a claim somebody
    /// made about two specific releases, and a range would be a claim about
    /// releases nobody has looked at.
    pub versions: &'static [&'static str],
}

/// A packet body, and the three constants that tie it to the generated table.
pub trait PacketBody: crate::types::Encode + crate::types::Decode {
    const NAME: &'static str;
    const STATE: ConnectionState;
    const DIRECTION: Direction;

    /// The number this packet travels under in `version`, or `None` if that
    /// version's table has no such packet.
    fn protocol_id(version: ProtocolVersion) -> Option<u32> {
        version.protocol_id(Self::STATE, Self::DIRECTION, Self::NAME)
    }
}

/// The (state, direction) pairs whose definitions are **complete**: every
/// packet the version table lists for the pair is defined here, and the
/// coverage check refuses to let that drift.
///
/// Play is absent on purpose and is not a promise about the future. Its
/// definitions grow family by family — movement, then entities, then chat —
/// and a half-covered pair held to the complete-pair rule would turn the suite
/// red for months while providing no information beyond what this constant
/// already states. The pair graduates to this list the day its last packet is
/// written, and [`undefined_for`] turns into its guard from that moment. A
/// test in `tests/packet_bodies.rs` pins the fact that Play is *not* here, so
/// nobody mistakes a growing definition list for a finished state.
pub static COMPLETE_PAIRS: &[(ConnectionState, Direction)] = &[
    (ConnectionState::Handshake, Direction::Serverbound),
    (ConnectionState::Status, Direction::Clientbound),
    (ConnectionState::Status, Direction::Serverbound),
    (ConnectionState::Login, Direction::Clientbound),
    (ConnectionState::Login, Direction::Serverbound),
    (ConnectionState::Configuration, Direction::Clientbound),
    (ConnectionState::Configuration, Direction::Serverbound),
];

/// Every group of definitions.
///
/// One row per (state, direction) that has packets. A group left out of this
/// list would leave its packets looking undefined, which [`undefined_in`]
/// reports for complete pairs and which the duplicate-name test catches for
/// all of them — so forgetting to add a group here fails either way.
pub static GROUPS: &[&[PacketMeta]] = &[
    handshake::serverbound::DEFINED,
    status::clientbound::DEFINED,
    status::serverbound::DEFINED,
    login::clientbound::DEFINED,
    login::serverbound::DEFINED,
    configuration::clientbound::DEFINED,
    configuration::serverbound::DEFINED,
    play::clientbound::DEFINED,
    play::serverbound::DEFINED,
];

/// What is wrong with the coverage of `version_name` by `groups`, given the
/// packets `table` says that version has.
///
/// Pure, and taking the table rather than reading it, for one reason: a guard
/// that can only be run against the data it was written for is a guard nobody
/// can prove bites. Passing a version name that no definition claims must
/// produce a complaint per packet, and there is a test below that does exactly
/// that with a version that does not exist.
///
/// Checked in both directions. A packet in the table with no definition is the
/// obvious failure; a definition naming a packet the table does not have is
/// the one that catches a typo in a name, which would otherwise sit there
/// decoding nothing forever.
///
/// The forward check runs only for pairs in `complete_pairs`. A pair still
/// being written gets its definitions checked against the table — a name is
/// either right or reported — but is not required to have every packet yet,
/// because "not finished" must be representable without lying about it.
pub fn undefined_in(
    version_name: &str,
    table: &[(ConnectionState, Direction, &str)],
    groups: &[&[PacketMeta]],
) -> Vec<String> {
    undefined_in_partial(version_name, table, groups, COMPLETE_PAIRS)
}

/// [`undefined_in`] with the complete pairs supplied by the caller.
///
/// The split exists so the guard's sensitivity stays provable: tests pass
/// their own pair lists, including ones that make Play look complete, and can
/// then assert exactly what bites. A guard whose inputs cannot vary is a guard
/// whose behaviour on new input is folklore.
pub fn undefined_in_partial(
    version_name: &str,
    table: &[(ConnectionState, Direction, &str)],
    groups: &[&[PacketMeta]],
    complete_pairs: &[(ConnectionState, Direction)],
) -> Vec<String> {
    let mut problems = Vec::new();
    let defined: Vec<&PacketMeta> = groups.iter().flat_map(|group| group.iter()).collect();

    for (state, direction, name) in table {
        if !complete_pairs.contains(&(*state, *direction)) {
            continue;
        }
        let found = defined.iter().find(|meta| {
            meta.state == *state && meta.direction == *direction && meta.name == *name
        });
        match found {
            None => problems.push(format!(
                "{}/{} {name} is in {version_name}'s table and has no definition",
                state.name(),
                direction.name()
            )),
            Some(meta) if !meta.versions.contains(&version_name) => problems.push(format!(
                "{}/{} {name} has a definition, and it claims {:?} rather than {version_name}. \
                 Check whether the layout moved before adding it.",
                state.name(),
                direction.name(),
                meta.versions
            )),
            Some(_) => {}
        }
    }

    for meta in &defined {
        let in_table = table.iter().any(|(state, direction, name)| {
            *state == meta.state && *direction == meta.direction && *name == meta.name
        });
        if !in_table && meta.versions.contains(&version_name) {
            problems.push(format!(
                "{}/{} {} is defined and claims {version_name}, and {version_name}'s table has no \
                 such packet",
                meta.state.name(),
                meta.direction.name(),
                meta.name
            ));
        }
    }
    problems
}

/// [`undefined_in`], against a version this workspace actually has a table for.
pub fn undefined_for(version: ProtocolVersion) -> Vec<String> {
    let mut table = Vec::new();
    for state in ConnectionState::ALL {
        for direction in Direction::ALL {
            for (_, name) in version.table(state, direction).packets() {
                table.push((state, direction, name));
            }
        }
    }
    undefined_in(version.name(), &table, GROUPS)
}

/// The packets a version's table lists for pairs **outside**
/// [`COMPLETE_PAIRS`] that no definition claims, as
/// `(state, direction, name)`.
///
/// The forward half of the coverage check deliberately does not look at these —
/// that is what makes an unfinished state representable. What keeps the
/// unfinishedness honest is this function: it is the worklist, in the table's
/// own order, and the tests read it rather than trusting the list to shrink by
/// itself.
pub fn unclaimed_for(version: ProtocolVersion) -> Vec<(ConnectionState, Direction, &'static str)> {
    let defined: Vec<&PacketMeta> = GROUPS.iter().flat_map(|group| group.iter()).collect();
    let mut unclaimed = Vec::new();
    for state in ConnectionState::ALL {
        for direction in Direction::ALL {
            if COMPLETE_PAIRS.contains(&(state, direction)) {
                continue;
            }
            for (_, name) in version.table(state, direction).packets() {
                let claimed = defined.iter().any(|meta| {
                    meta.state == state && meta.direction == direction && meta.name == name
                });
                if !claimed {
                    unclaimed.push((state, direction, name));
                }
            }
        }
    }
    unclaimed
}

/// Define a group of packets: one (state, direction) pair's bodies, the enum
/// over them, and the dispatch in both directions.
///
/// A definition is a name, a Rust type name and a list of fields **in wire
/// order**. It carries no packet id; see the module docs for why that matters
/// more than it looks like it does.
#[macro_export]
macro_rules! packet_group {
    (
        state: $state:ident,
        direction: $direction:ident,
        versions: [$($version:literal),* $(,)?],
        $(
            $(#[$packet_meta:meta])*
            $name:literal => $packet:ident {
                $(
                    $(#[$field_meta:meta])*
                    $field:ident : $ty:ty
                ),* $(,)?
            }
        ),* $(,)?
    ) => {
        $(
            $crate::wire_struct! {
                $(#[$packet_meta])*
                pub struct $packet {
                    $($(#[$field_meta])* $field: $ty),*
                }
            }

            impl $crate::packets::PacketBody for $packet {
                const NAME: &'static str = $name;
                const STATE: $crate::ConnectionState = $crate::ConnectionState::$state;
                const DIRECTION: $crate::Direction = $crate::Direction::$direction;
            }
        )*

        /// Every packet this pair can carry.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Packet {
            $($packet($packet),)*
        }

        /// The Minecraft versions these layouts have been checked against.
        ///
        /// Hoisted out of [`DEFINED`] rather than repeated per packet because a
        /// group's definitions were all written by reading the same version of
        /// the protocol, and a per-packet list would invite them to drift into
        /// claiming different things without anybody deciding to.
        pub static VERSIONS: &[&str] = &[$($version),*];

        /// What these definitions claim, for the coverage check.
        pub static DEFINED: &[$crate::packets::PacketMeta] = &[
            $($crate::packets::PacketMeta {
                name: $name,
                state: $crate::ConnectionState::$state,
                direction: $crate::Direction::$direction,
                versions: VERSIONS,
            },)*
        ];

        impl Packet {
            pub const STATE: $crate::ConnectionState = $crate::ConnectionState::$state;
            pub const DIRECTION: $crate::Direction = $crate::Direction::$direction;

            /// The namespaced name, which is what the id table is keyed by.
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$packet(_) => $name,)*
                }
            }

            /// Read a whole packet: the VarInt id, then the body.
            ///
            /// The id is looked up in `version`'s generated table rather than
            /// matched against a constant, so this dispatch is correct for any
            /// version whose table exists and whose layouts these definitions
            /// claim.
            ///
            /// Insists the body ends exactly where the definition says. A
            /// packet that decoded with bytes left over means the layout here
            /// is not the layout that was sent, and continuing would put every
            /// later field of every later packet in the wrong place — so it is
            /// an error, not a warning.
            pub fn decode<R: $crate::wire::WireRead + ?Sized>(
                input: &mut R,
                version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<Self, $crate::wire::DecodeError> {
                let protocol_id = input.read_var_int()?;
                let protocol_id = u32::try_from(protocol_id).map_err(|_| {
                    $crate::wire::DecodeError::NegativeLength {
                        field: "packet id",
                        value: protocol_id,
                    }
                })?;
                let unknown = || $crate::wire::DecodeError::UnknownPacket {
                    state: Self::STATE.name(),
                    direction: Self::DIRECTION.name(),
                    protocol_id,
                };
                let name = version
                    .packet_name(Self::STATE, Self::DIRECTION, protocol_id)
                    .ok_or_else(unknown)?;
                let decoded = match name {
                    $($name => Self::$packet(
                        <$packet as $crate::types::Decode>::decode(input, version)?
                    ),)*
                    _ => return ::core::result::Result::Err(unknown()),
                };
                let left = input.remaining();
                if left != 0 {
                    return ::core::result::Result::Err(
                        $crate::wire::DecodeError::TrailingBytes { left }
                    );
                }
                ::core::result::Result::Ok(decoded)
            }

            /// Write a whole packet: the VarInt id, then the body.
            ///
            /// Framing, compression and encryption wrap what this produces and
            /// are `dust-net`'s; the id belongs to the packet, not the frame.
            pub fn encode<W: $crate::wire::WireWrite + ?Sized>(
                &self,
                out: &mut W,
                version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<u32, $crate::wire::EncodeError> {
                let name = self.name();
                let protocol_id = version
                    .protocol_id(Self::STATE, Self::DIRECTION, name)
                    .ok_or($crate::wire::EncodeError::NotInVersion {
                        name,
                        version: version.name(),
                    })?;
                out.write_var_int(protocol_id as i32);
                match self {
                    $(Self::$packet(body) =>
                        <$packet as $crate::types::Encode>::encode(body, out, version)?,)*
                }
                ::core::result::Result::Ok(protocol_id)
            }
        }

        $(
            impl ::core::convert::From<$packet> for Packet {
                fn from(body: $packet) -> Self {
                    Self::$packet(body)
                }
            }
        )*
    };
}

/// A struct whose fields are read and written in declaration order.
///
/// Declaration order **is** wire order. Rust evaluates a struct expression's
/// fields in the order they are written, so the generated decode reads them in
/// the order they appear here; moving two lines in a definition changes the
/// format. That is the property that makes a definition readable as a layout,
/// and it is also the one that makes a careless reordering a protocol change,
/// which is why the live-server test exists.
#[macro_export]
macro_rules! wire_struct {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($(#[$field_meta:meta])* $field:ident : $ty:ty),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        $vis struct $name {
            $($(#[$field_meta])* pub $field: $ty,)*
        }

        impl $crate::types::Decode for $name {
            fn decode<R: $crate::wire::WireRead + ?Sized>(
                input: &mut R,
                version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<Self, $crate::wire::DecodeError> {
                // Four packets in this crate have no fields at all — a packet
                // whose whole message is that it arrived. Their generated
                // bodies touch neither argument, so both are consumed here to
                // keep a zero-field definition from being a warning that
                // teaches people to stop reading warnings.
                let _ = (&mut *input, version);
                ::core::result::Result::Ok(Self {
                    $($field: <$ty as $crate::types::Decode>::decode(input, version)?,)*
                })
            }
        }

        impl $crate::types::Encode for $name {
            fn encode<W: $crate::wire::WireWrite + ?Sized>(
                &self,
                out: &mut W,
                version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<(), $crate::wire::EncodeError> {
                let _ = (&mut *out, version);
                $(<$ty as $crate::types::Encode>::encode(&self.$field, out, version)?;)*
                ::core::result::Result::Ok(())
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard's positive control.
    ///
    /// A coverage check that has only ever been run against the data it passes
    /// on is a check nobody knows the sensitivity of. This runs it against a
    /// version no definition claims and insists it complains about every
    /// packet — which is what will happen the day 1.21.4's table is generated,
    /// and is the whole reason the version list exists.
    #[test]
    fn a_version_no_definition_claims_is_reported_packet_by_packet() {
        let version = ProtocolVersion::from_name("1.21.1").expect("the table exists");
        let mut table = Vec::new();
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                for (_, name) in version.table(state, direction).packets() {
                    table.push((state, direction, name));
                }
            }
        }
        // Every pair is held complete here, including Play, which is how the
        // day 1.21.4's table lands will actually look.
        let problems = undefined_in_partial("1.21.4", &table, GROUPS, &[
            (ConnectionState::Handshake, Direction::Serverbound),
            (ConnectionState::Status, Direction::Clientbound),
            (ConnectionState::Status, Direction::Serverbound),
            (ConnectionState::Login, Direction::Clientbound),
            (ConnectionState::Login, Direction::Serverbound),
            (ConnectionState::Configuration, Direction::Clientbound),
            (ConnectionState::Configuration, Direction::Serverbound),
            (ConnectionState::Play, Direction::Clientbound),
            (ConnectionState::Play, Direction::Serverbound),
        ]);
        assert_eq!(
            problems.len(),
            table.len(),
            "every packet in the table should be unclaimed by a version nothing claims"
        );
        assert!(
            problems.iter().all(|p| p.contains("1.21.4")),
            "{problems:#?}"
        );
    }

    #[test]
    fn an_incomplete_pair_is_not_held_to_the_forward_check() {
        // The mechanism the growing Play definitions rely on: a pair outside
        // the complete list gets its definitions validated but is not required
        // to cover its table yet.
        let table = [(
            ConnectionState::Play,
            Direction::Clientbound,
            "minecraft:explode",
        )];
        static CLAIMED: &[PacketMeta] = &[PacketMeta {
            name: "minecraft:login",
            state: ConnectionState::Play,
            direction: Direction::Clientbound,
            versions: &["1.21.1"],
        }];
        let all_complete: Vec<(ConnectionState, Direction)> = ConnectionState::ALL
            .iter()
            .flat_map(|&s| Direction::ALL.map(move |d| (s, d)))
            .collect();
        let problems = undefined_in("1.21.1", &table, &[CLAIMED]);
        assert!(!problems.is_empty(), "`minecraft:explode` is undefined");
        let problems =
            undefined_in_partial("1.21.1", &table, &[CLAIMED], &all_complete);
        assert!(
            !problems.is_empty(),
            "holding every pair complete must report the gap"
        );
        let problems = undefined_in_partial("1.21.1", &table, &[CLAIMED], &[]);
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn a_pair_graduating_to_complete_is_guarded_from_that_moment() {
        // What happens when Play/clientbound is added to COMPLETE_PAIRS while
        // packets are still missing: one complaint per missing packet. This is
        // the check that makes graduation irreversible without finishing.
        let mut table = Vec::new();
        for (_, name) in ProtocolVersion::from_name("1.21.1")
            .expect("the table exists")
            .table(ConnectionState::Play, Direction::Clientbound)
            .packets()
        {
            table.push((ConnectionState::Play, Direction::Clientbound, name));
        }
        let problems = undefined_in_partial(
            "1.21.1",
            &table,
            GROUPS,
            &[(ConnectionState::Play, Direction::Clientbound)],
        );
        assert_eq!(
            problems.len(),
            unclaimed_for(ProtocolVersion::from_name("1.21.1").unwrap())
                .iter()
                .filter(|(s, d, _)| (*s, *d) == (ConnectionState::Play, Direction::Clientbound))
                .count(),
            "every unclaimed packet of a graduated pair is a complaint"
        );
    }

    #[test]
    fn no_two_groups_define_the_same_packet() {
        // The dispatch `match` would silently prefer the first arm over a
        // duplicate, so a copy-pasted definition would compile and decode as
        // whichever group came first. The coverage check cannot see it — both
        // rows claim the same name — so it is caught here instead.
        let mut defined: Vec<&PacketMeta> = GROUPS.iter().flat_map(|g| g.iter()).collect();
        defined.sort_by_key(|meta| (meta.state, meta.direction, meta.name));
        let unique = defined.len();
        defined.dedup_by_key(|meta| (meta.state, meta.direction, meta.name));
        assert_eq!(unique, defined.len(), "a packet is defined twice");
    }

    #[test]
    fn a_definition_naming_a_packet_that_does_not_exist_is_reported() {
        // The other direction, which catches a typo in a name. Without this a
        // misspelled definition would sit in the tree decoding nothing, and the
        // packet it was meant to be would look undefined somewhere else.
        static INVENTED: &[PacketMeta] = &[PacketMeta {
            name: "minecraft:not_a_packet",
            state: ConnectionState::Login,
            direction: Direction::Clientbound,
            versions: &["1.21.1"],
        }];
        let problems = undefined_in("1.21.1", &[], &[INVENTED]);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].contains("no such packet"), "{problems:#?}");
    }

    #[test]
    fn an_empty_definition_set_reports_every_packet_in_the_table() {
        // The check must not be satisfiable by having nothing to check.
        let table = [(
            ConnectionState::Login,
            Direction::Clientbound,
            "minecraft:hello",
        )];
        let problems = undefined_in("1.21.1", &table, &[]);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].contains("no definition"), "{problems:#?}");
    }
}

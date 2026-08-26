//! Reading `reports/packets.json` into something that can be generated from.
//!
//! # The thing this module exists to get right
//!
//! The report is `state -> direction -> packet name -> {"protocol_id": n}`, and
//! that id is the number at the head of every frame on the wire. Three things
//! about it are worth checking rather than assuming.
//!
//! - **The ids are per (state, direction) pair, and nothing wider.** Id 0 is
//!   `minecraft:intention` in the handshake, `minecraft:status_response` from
//!   the server in status, and `minecraft:accept_teleportation` from the client
//!   in play. A table keyed by anything less than the full pair does not fail —
//!   it decodes a different packet, which is worse.
//! - **Not every pair exists.** On 1.21.1 `handshake/clientbound` is absent
//!   from the report entirely: the server says nothing during the handshake.
//!   An extractor that walked the product of states and directions expecting to
//!   find all ten would either stop or, if it were tolerant, shift every
//!   later pair by one slot. So the pairs are built by name and the absent ones
//!   are recorded, not skipped.
//! - **The ids are contiguous from 0.** They are on 1.21.1, for all nine pairs
//!   that exist, which is what makes an id an index into an array. That is a
//!   fact about this data and not a promise in the format, so it is measured:
//!   if a version ever leaves a gap, the gap is emitted as an id that decodes
//!   to nothing and the extraction says so on the way past, rather than the
//!   table silently closing up and shifting everything after it.
//!
//! Duplicates are the one shape that is refused outright. Two names on one id
//! in one pair cannot both be true — a decoder could not tell them apart, and
//! there is no honest thing to generate.
//!
//! What this module does not check: that the names mean what Dust will assume
//! they mean. It reads a name and an id; the body behind that name is Phase 1's
//! problem, and nothing here would notice if Mojang reused a name for a
//! different packet.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// One packet, as `reports/packets.json` describes it.
#[derive(Debug, Deserialize)]
pub struct ReportedPacket {
    pub protocol_id: u32,
}

/// The report as it stands: state, then direction, then packet name.
pub type Report = BTreeMap<String, BTreeMap<String, BTreeMap<String, ReportedPacket>>>;

/// The connection states, in the order a connection moves through them.
///
/// This is the vocabulary the generated table is built against, and it is
/// deliberately a list here rather than a read of the report's keys: a version
/// that adds a state must turn this extractor red, not quietly produce a table
/// that is missing a fifth of the protocol. The matching enum in `dust-protocol`
/// is hand-written for the same reason, and the two are held together by the
/// tables carrying their own state and direction — see
/// `crates/dust-protocol/tests/packet_ids.rs`.
///
/// `xtask` deliberately does not depend on `dust-protocol` to share that
/// vocabulary. The extractor has to be runnable exactly when the generated code
/// is broken, which is when a dependency on it would stop it building.
pub const STATES: [&str; 5] = ["handshake", "status", "login", "configuration", "play"];

/// Who is sending. Both spellings appear in the report as keys.
pub const DIRECTIONS: [&str; 2] = ["clientbound", "serverbound"];

/// One (state, direction) pair's packets.
#[derive(Debug)]
pub struct Group {
    pub state: &'static str,
    pub direction: &'static str,
    /// Packet names indexed by protocol id. An empty string is an id no packet
    /// claimed — impossible on 1.21.1, and represented rather than closed up so
    /// that a version with a gap generates a hole instead of a shift.
    pub by_id: Vec<String>,
    /// Whether the report had this pair at all. `handshake/clientbound` does
    /// not, and an absent pair is not the same as an empty one.
    pub present: bool,
    /// Ids in `0..by_id.len()` that no packet claimed.
    pub holes: Vec<u32>,
}

impl Group {
    /// How many packets this pair actually defines, not counting holes.
    pub fn count(&self) -> usize {
        self.by_id.iter().filter(|name| !name.is_empty()).count()
    }
}

/// Everything the packet report says, once it has been checked.
#[derive(Debug)]
pub struct Packets {
    /// All ten pairs, at `state_index * 2 + direction_index`, whether or not
    /// the report had them. The generated table is indexed the same way, so
    /// this order is load-bearing.
    pub groups: Vec<Group>,
    pub total: usize,
    /// The report as it was read, kept so the golden sample can be taken from
    /// it rather than from anything this module derived.
    pub reported: Report,
}

pub fn parse(json: &[u8]) -> Result<Packets, String> {
    let reported: Report =
        serde_json::from_slice(json).map_err(|e| format!("could not read packets.json: {e}"))?;

    check_vocabulary(&reported)?;

    let mut groups = Vec::with_capacity(STATES.len() * DIRECTIONS.len());
    for state in STATES {
        for direction in DIRECTIONS {
            groups.push(Group::from_report(state, direction, &reported)?);
        }
    }

    let total = groups.iter().map(Group::count).sum();
    Ok(Packets {
        groups,
        total,
        reported,
    })
}

/// The report's states and directions are exactly the ones this extractor knows
/// how to name.
///
/// A state Dust has never heard of is the failure worth being loud about: the
/// generated table would be complete, compile, pass its round-trip, and be
/// missing every packet of whatever 1.22 adds.
fn check_vocabulary(reported: &Report) -> Result<(), String> {
    let known: BTreeSet<&str> = STATES.into_iter().collect();
    let found: BTreeSet<&str> = reported.keys().map(String::as_str).collect();

    let unknown: Vec<&str> = found.difference(&known).copied().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "the packet report has connection state(s) {unknown:?}, which this extractor does \
             not know. Add them to STATES here and to ConnectionState in dust-protocol, or the \
             generated table silently drops every packet in them."
        ));
    }
    let missing: Vec<&str> = known.difference(&found).copied().collect();
    if !missing.is_empty() {
        return Err(format!(
            "the packet report has no connection state(s) {missing:?}. Either the report is not \
             what this extractor thinks it is, or the protocol changed shape."
        ));
    }

    for (state, directions) in reported {
        for direction in directions.keys() {
            if !DIRECTIONS.contains(&direction.as_str()) {
                return Err(format!(
                    "{state} has a direction `{direction}`, and the only two are {DIRECTIONS:?}"
                ));
            }
        }
    }
    Ok(())
}

impl Group {
    fn from_report(
        state: &'static str,
        direction: &'static str,
        reported: &Report,
    ) -> Result<Self, String> {
        let Some(packets) = reported.get(state).and_then(|s| s.get(direction)) else {
            return Ok(Self {
                state,
                direction,
                by_id: Vec::new(),
                present: false,
                holes: Vec::new(),
            });
        };

        let mut by_id: Vec<String> = Vec::new();
        for (name, packet) in packets {
            let id = packet.protocol_id as usize;
            // The ids index a table, and that table is indexed with a u16, so
            // an id past that is a shape this cannot generate rather than a
            // number to truncate.
            if id > u16::MAX as usize {
                return Err(format!(
                    "{state}/{direction} has {name} at protocol id {id}, which does not fit the \
                     index type the generated table uses"
                ));
            }
            if by_id.len() <= id {
                by_id.resize(id + 1, String::new());
            }
            if !by_id[id].is_empty() {
                return Err(format!(
                    "{state}/{direction} has both {} and {name} at protocol id {id}. A decoder \
                     could not tell them apart, so there is nothing honest to generate.",
                    by_id[id]
                ));
            }
            by_id[id] = name.clone();
        }

        let holes = by_id
            .iter()
            .enumerate()
            .filter(|(_, name)| name.is_empty())
            .map(|(id, _)| id as u32)
            .collect();

        Ok(Self {
            state,
            direction,
            by_id,
            present: true,
            holes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal report with every state present, so a test can perturb one
    /// thing at a time without tripping the vocabulary check first.
    fn minimal() -> String {
        let mut states = Vec::new();
        for state in STATES {
            states.push(format!(
                "\"{state}\": {{ \"serverbound\": {{ \"minecraft:a\": {{\"protocol_id\": 0}}, \
                 \"minecraft:b\": {{\"protocol_id\": 1}} }} }}"
            ));
        }
        format!("{{{}}}", states.join(", "))
    }

    #[test]
    fn ids_become_the_index_of_the_name() {
        let parsed = parse(minimal().as_bytes()).expect("parses");
        assert_eq!(parsed.total, STATES.len() * 2);
        let play = parsed
            .groups
            .iter()
            .find(|g| g.state == "play" && g.direction == "serverbound")
            .expect("present");
        assert_eq!(play.by_id, ["minecraft:a", "minecraft:b"]);
        assert!(play.holes.is_empty());
    }

    #[test]
    fn an_absent_pair_is_recorded_rather_than_skipped() {
        // handshake/clientbound is genuinely absent on 1.21.1. The pair still
        // has to occupy its slot, because the generated table is indexed by
        // position and a missing slot shifts every pair after it.
        let parsed = parse(minimal().as_bytes()).expect("parses");
        assert_eq!(parsed.groups.len(), STATES.len() * DIRECTIONS.len());
        let clientbound = parsed
            .groups
            .iter()
            .find(|g| g.state == "handshake" && g.direction == "clientbound")
            .expect("occupies its slot");
        assert!(!clientbound.present);
        assert_eq!(clientbound.count(), 0);
    }

    #[test]
    fn an_unknown_state_fails_the_extraction() {
        // The failure worth being loudest about: everything else about the
        // generated table would be right, and a fifth of the protocol missing.
        let json = minimal().replace("\"play\":", "\"transfer\": {}, \"play\":");
        let err = parse(json.as_bytes()).expect_err("must not be accepted");
        assert!(err.contains("transfer"), "{err}");
    }

    #[test]
    fn a_missing_state_fails_the_extraction() {
        let json = r#"{"handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}}}"#;
        let err = parse(json.as_bytes()).expect_err("must not be accepted");
        assert!(err.contains("no connection state"), "{err}");
    }

    #[test]
    fn an_unknown_direction_fails_the_extraction() {
        let json = minimal().replace(
            "\"handshake\": { \"serverbound\"",
            "\"handshake\": { \"both\"",
        );
        let err = parse(json.as_bytes()).expect_err("must not be accepted");
        assert!(err.contains("both"), "{err}");
    }

    #[test]
    fn two_packets_on_one_id_fail_the_extraction() {
        let json = minimal().replace(
            "\"minecraft:b\": {\"protocol_id\": 1}",
            "\"minecraft:b\": {\"protocol_id\": 0}",
        );
        let err = parse(json.as_bytes()).expect_err("must not be accepted");
        assert!(err.contains("could not tell them apart"), "{err}");
    }

    #[test]
    fn a_gap_becomes_a_hole_and_not_a_shift() {
        // Closing the gap would renumber every packet after it, which is the
        // defect that compiles, round-trips and puts the wrong packet on the
        // wire. The id that nothing claims is kept as an id that decodes to
        // nothing, and the caller is told.
        let json = minimal().replace(
            "\"minecraft:b\": {\"protocol_id\": 1}",
            "\"minecraft:b\": {\"protocol_id\": 2}",
        );
        let parsed = parse(json.as_bytes()).expect("parses");
        let group = parsed
            .groups
            .iter()
            .find(|g| g.state == "play" && g.direction == "serverbound")
            .expect("present");
        assert_eq!(group.holes, [1]);
        assert_eq!(group.by_id, ["minecraft:a", "", "minecraft:b"]);
        assert_eq!(group.count(), 2);
    }

    // What these tests do not catch: whether Mojang's report means what this
    // module reads it as meaning. The checks for that are the extraction
    // refusing to emit what it cannot verify, run against the real 1.21.1
    // report, and the golden sample in dust-protocol's tests — which is taken
    // from the report rather than from anything derived here.
}

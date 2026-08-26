//! Phase 0.5's exit criterion for the protocol layer: the generated packet
//! tables compile and round-trip every packet, in every state and direction, in
//! every version.
//!
//! The extractor verifies its reading of Mojang's report as it goes, but it
//! verifies it against the report. This checks the code that came out, which is
//! a different thing and the one that runs on every pull request forever.

use dust_protocol::{version, ConnectionState, Direction, ProtocolVersion};

#[test]
fn every_packet_round_trips_through_its_id() {
    let mut seen = 0usize;
    for protocol in ProtocolVersion::all() {
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let table = protocol.table(state, direction);
                for (id, name) in table.packets() {
                    assert_eq!(
                        table.name(id),
                        Some(name),
                        "{} {}/{} id {id}",
                        protocol.name(),
                        state.name(),
                        direction.name()
                    );
                    assert_eq!(
                        table.protocol_id(name),
                        Some(id),
                        "{} {}/{} {name} does not find its way back to its id",
                        protocol.name(),
                        state.name(),
                        direction.name()
                    );
                    seen += 1;
                }
                assert_eq!(table.len(), table.packets().count());
            }
        }
    }
    assert!(seen > 0, "the generated tables carry no packets");
}

#[test]
fn the_table_agrees_with_mojang_and_not_merely_with_itself() {
    // This is the test the round-trip above cannot be. Looking a name up by its
    // id and the id back up by its name goes through the same two tables, and
    // those two agree with each other under *any* consistent numbering —
    // including one that is off by one, or sorted by name instead of by id, or
    // correct but filed under the wrong state. A round-trip proves the encoder
    // and the decoder agree with each other. Whether they agree with Minecraft
    // is a different question and needs something that did not come from the
    // table.
    //
    // The samples are that something: taken from Mojang's report at extraction
    // time, carrying their state and direction as the report's own strings so
    // that a table in the wrong slot cannot move with them.
    for protocol in ProtocolVersion::all() {
        assert!(
            !protocol.samples().is_empty(),
            "{} carries no samples",
            protocol.name()
        );
        for &(state, direction, id, name) in protocol.samples() {
            let state = ConnectionState::from_name(state)
                .unwrap_or_else(|| panic!("the report's state `{state}` has no variant"));
            let direction = Direction::from_name(direction)
                .unwrap_or_else(|| panic!("the report's direction `{direction}` has no variant"));
            assert_eq!(
                protocol.packet_name(state, direction, id),
                Some(name),
                "{} {}/{} id {id} decodes to something other than what Mojang's report says",
                protocol.name(),
                state.name(),
                direction.name()
            );
            assert_eq!(
                protocol.protocol_id(state, direction, name),
                Some(id),
                "{} {}/{} {name} is sent under an id other than the one Mojang's report gives",
                protocol.name(),
                state.name(),
                direction.name()
            );
        }
    }
}

#[test]
fn the_samples_and_the_tables_cover_each_other() {
    // A sample set that quietly stopped covering a pair would leave that pair
    // unchecked while the suite stayed green — the failure mode where a guard
    // degrades instead of breaking. The check runs both ways: a sample with no
    // packet is as wrong as a packet with no sample.
    for protocol in ProtocolVersion::all() {
        let mut sampled: Vec<(&str, &str, u32)> = protocol
            .samples()
            .iter()
            .map(|&(state, direction, id, _)| (state, direction, id))
            .collect();
        sampled.sort_unstable();
        let unique = sampled.len();
        sampled.dedup();
        assert_eq!(
            unique,
            sampled.len(),
            "{} samples a packet twice",
            protocol.name()
        );

        let mut tabled: Vec<(&str, &str, u32)> = Vec::new();
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                for (id, _) in protocol.table(state, direction).packets() {
                    tabled.push((state.name(), direction.name(), id));
                }
            }
        }
        tabled.sort_unstable();
        assert_eq!(
            sampled,
            tabled,
            "{}: the samples and the tables describe different sets of packets",
            protocol.name()
        );
    }
}

#[test]
fn every_table_is_in_the_slot_the_lookup_expects() {
    // The lookup indexes the tables by `state as usize * 2 + direction as
    // usize`, which is only right if the generated layout and the hand-written
    // enums agree about the order. They are produced by different files, so
    // each table carries the pair it is for and this reads it back. Without
    // this, reordering ConnectionState would silently swap two states' packets.
    for protocol in ProtocolVersion::all() {
        assert_eq!(
            protocol.tables().len(),
            ConnectionState::ALL.len() * Direction::ALL.len(),
            "{} does not have every pair",
            protocol.name()
        );
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let table = protocol.table(state, direction);
                assert_eq!(table.state, state, "{}", protocol.name());
                assert_eq!(table.direction, direction, "{}", protocol.name());
            }
        }
    }
}

#[test]
fn only_the_clientbound_handshake_is_empty() {
    // A fact about Minecraft rather than a quirk of the report: the server says
    // nothing between accepting a connection and the state it is handed off to.
    // Every other pair has packets, and a version where one of them empties is
    // something somebody should look at rather than a table that quietly
    // decodes nothing.
    for protocol in ProtocolVersion::all() {
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let empty = protocol.table(state, direction).is_empty();
                let expected =
                    state == ConnectionState::Handshake && direction == Direction::Clientbound;
                assert_eq!(
                    empty,
                    expected,
                    "{} {}/{} is {}",
                    protocol.name(),
                    state.name(),
                    direction.name(),
                    if empty { "empty" } else { "not empty" }
                );
            }
        }
    }
}

#[test]
fn the_ids_of_a_pair_are_contiguous_from_zero() {
    // Not a promise the format makes, which is why the extractor can represent
    // a gap. It holds for every pair on 1.21.1, and if a version ever breaks it
    // this is where that becomes visible rather than a hole nobody noticed.
    for protocol in ProtocolVersion::all() {
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let table = protocol.table(state, direction);
                let ids: Vec<u32> = table.packets().map(|(id, _)| id).collect();
                let contiguous: Vec<u32> = (0..table.len() as u32).collect();
                assert_eq!(
                    ids,
                    contiguous,
                    "{} {}/{} leaves an id unclaimed",
                    protocol.name(),
                    state.name(),
                    direction.name()
                );
            }
        }
    }
}

#[test]
fn the_name_index_is_sorted_and_every_name_is_namespaced() {
    // `protocol_id` is a binary search, which returns nonsense rather than
    // failing over an unsorted index — and it would still round-trip for
    // whichever names it happened to find.
    for protocol in ProtocolVersion::all() {
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let table = protocol.table(state, direction);
                let names: Vec<&str> = table
                    .by_name
                    .iter()
                    .map(|&index| table.by_id[index as usize])
                    .collect();
                let mut sorted = names.clone();
                sorted.sort_unstable();
                assert_eq!(names, sorted, "{}/{}", state.name(), direction.name());

                for (_, name) in table.packets() {
                    assert!(
                        name.starts_with("minecraft:"),
                        "{name} is not namespaced, and the lookup takes namespaced names"
                    );
                }
            }
        }
    }
}

#[test]
fn nothing_outside_a_table_resolves() {
    for protocol in ProtocolVersion::all() {
        for state in ConnectionState::ALL {
            for direction in Direction::ALL {
                let table = protocol.table(state, direction);
                assert_eq!(table.name(table.by_id.len() as u32), None);
                assert_eq!(table.name(u32::MAX), None);
                assert_eq!(table.protocol_id("minecraft:not_a_packet"), None);
                // A bare name is deliberately not accepted; see `protocol_id`.
                assert_eq!(table.protocol_id("intention"), None);
                assert_eq!(table.protocol_id(""), None);
            }
        }
    }
}

#[test]
fn a_packet_id_means_nothing_without_its_pair() {
    // The reason every lookup takes all four coordinates. Id 0 is a different
    // packet in each of these, and a table keyed by less than the whole pair
    // would not fail — it would decode the wrong thing.
    let v = version::V1_21_1;
    assert_eq!(
        v.packet_name(ConnectionState::Handshake, Direction::Serverbound, 0),
        Some("minecraft:intention")
    );
    assert_eq!(
        v.packet_name(ConnectionState::Status, Direction::Clientbound, 0),
        Some("minecraft:status_response")
    );
    assert_eq!(
        v.packet_name(ConnectionState::Play, Direction::Serverbound, 0),
        Some("minecraft:accept_teleportation")
    );
    assert_eq!(
        v.packet_name(ConnectionState::Handshake, Direction::Clientbound, 0),
        None,
        "the server says nothing during a handshake"
    );
}

#[test]
fn the_states_and_directions_name_themselves_both_ways() {
    for state in ConnectionState::ALL {
        assert_eq!(ConnectionState::from_name(state.name()), Some(state));
    }
    for direction in Direction::ALL {
        assert_eq!(Direction::from_name(direction.name()), Some(direction));
    }
    assert_eq!(ConnectionState::from_name("transfer"), None);
    assert_eq!(Direction::from_name("both"), None);
}

#[test]
fn every_version_is_findable_by_name_and_names_itself() {
    // D3 commits this project to more than one protocol version, and the point
    // of the dimension is that a call site names one rather than assuming it.
    for protocol in ProtocolVersion::all() {
        assert_eq!(ProtocolVersion::from_name(protocol.name()), Some(protocol));
    }
    assert_eq!(ProtocolVersion::from_name("1.7.10"), None);
    assert_eq!(version::V1_21_1.name(), "1.21.1");
    assert!(ProtocolVersion::all().any(|v| v == version::V1_21_1));
}

/// The handshake's number resolves to the version whose table it selects, and
/// nothing else does.
///
/// The two directions are asserted against each other rather than against a
/// literal 767 alone, because a table that agreed with itself about a wrong
/// number would satisfy either check on its own. The literal is here too, from
/// the jar's `version.json` — it is the one number in this crate that a
/// generator cannot derive and a test cannot recompute.
#[test]
fn a_protocol_number_selects_exactly_one_version() {
    assert_eq!(version::V1_21_1.number(), 767);
    assert_eq!(
        ProtocolVersion::from_protocol_number(767),
        Some(version::V1_21_1)
    );

    for v in ProtocolVersion::all() {
        assert_eq!(
            ProtocolVersion::from_protocol_number(v.number()),
            Some(v),
            "{} must resolve back to itself",
            v.name()
        );
    }

    // An unsupported client is a `None`, not a panic and not a nearest match:
    // this is the point at which a server has to be able to say no politely.
    for absent in [-1, 0, 1, 766, 768, i32::MAX] {
        if ProtocolVersion::all().any(|v| v.number() == absent) {
            continue;
        }
        assert_eq!(
            ProtocolVersion::from_protocol_number(absent),
            None,
            "{absent} is not a version this server has a table for"
        );
    }
}

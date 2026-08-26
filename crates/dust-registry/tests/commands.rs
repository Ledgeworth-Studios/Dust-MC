//! The command graph: the table against the report, not against itself.
//!
//! A flat array of nodes has no round-trip in the usual sense — resolving a
//! path and walking children both read the same table, so they would agree with
//! each other under any internally consistent numbering of the nodes, including
//! one where two subtrees are swapped. What says the table agrees with
//! *Minecraft* is [`PATH_SAMPLES`] and [`REDIRECT_SAMPLES`], walked out of the
//! report's own tree at extraction time by a pass that shares nothing with the
//! flattener.
//!
//! The rows are paths, and that is what makes them evidence: the generated
//! table stores no paths at all, so the test reconstructs every path from the
//! root through the child indices. A wrong child link, a mis-sorted sibling
//! list or a swapped redirect lands the walk on a node whose name spells a
//! different command.
//!
//! That claim was tested by breaking the table on purpose. Shifting the `xp`
//! node's redirect target by one left nine of these tests green — the table
//! still resolved, walked, terminated and counted correctly — and failed
//! [`every_redirect_goes_where_the_report_says`] and
//! [`cycles_terminate_and_the_execute_loop_is_the_one_that_loops`], which is
//! the one worth remembering: `/xp` would have dispatched as `/experience`
//! with everything else in this file agreeing about it.

use dust_registry::{ArgumentProperties, CommandDef, CommandGraph, NodeKind, DATA_VERSION};

use dust_registry::generated::commands::{
    EXECUTABLE_COUNT, MAX_DEPTH, NODES, NODE_COUNT, PARSER_USES, PATH_SAMPLES, REDIRECT_SAMPLES,
    UNREACHABLE,
};

#[test]
fn every_node_is_reachable_from_the_root_exactly_once() {
    // The flattener emitted nodes in its own DFS order; this walk is an
    // independent traversal over what came out. If a child index were wrong,
    // some node would be visited twice or never.
    let mut visited = vec![0usize; NODE_COUNT];
    let mut count = 0;
    for index in CommandGraph::walk() {
        visited[index] += 1;
        count += 1;
    }
    assert_eq!(count, NODE_COUNT, "the walk did not reach every node once");
    assert!(
        visited.iter().all(|&n| n == 1),
        "some node was reached {} times",
        visited.iter().max().unwrap()
    );
}

#[test]
fn every_child_and_redirect_index_names_a_real_node() {
    // Binary-searching children is undefined over dangling indices, and a
    // redirect into nowhere would be followed at dispatch time. Checked rather
    // than assumed because the extractor checks its own arithmetic and not the
    // bytes that came out.
    for (index, def) in NODES.iter().enumerate() {
        assert!(def.name.len() < 64, "node {index} has an absurd name");
        if index > 0 {
            assert!(!def.name.is_empty(), "node {index} is unnamed");
        }
        for &child in def.children {
            assert!(
                (child as usize) < NODE_COUNT,
                "node {index} names child {child}"
            );
        }
        if let Some(target) = def.redirect {
            assert!(
                (target as usize) < NODE_COUNT,
                "node {index} redirects to {target}"
            );
        }
    }
}

#[test]
fn children_are_sorted_by_name_so_resolution_can_binary_search() {
    for (index, def) in NODES.iter().enumerate() {
        assert!(
            def.children
                .windows(2)
                .all(|pair| NODES[pair[0] as usize].name <= NODES[pair[1] as usize].name),
            "node {index} ({}) has children out of name order",
            def.name
        );
    }
}

#[test]
fn the_table_agrees_with_mojang_and_not_merely_with_itself() {
    // Every non-root node, as the report's own tree states it. Resolving the
    // row's path goes through the table's child indices, so this fails if any
    // link is wrong anywhere along it — and there is a row for every node, so
    // no corner of the graph is unchecked.
    assert!(
        !PATH_SAMPLES.is_empty(),
        "the generated table carries no samples"
    );
    assert_eq!(
        PATH_SAMPLES.len(),
        NODE_COUNT - 1,
        "one row per node except the root"
    );
    for &(path, executable, parser) in PATH_SAMPLES {
        let index =
            CommandGraph::resolve(path).unwrap_or_else(|| panic!("{path} does not resolve"));
        let def = CommandGraph::def(index).expect("in range");
        assert_eq!(
            parser,
            def.parser.unwrap_or_default(),
            "{path}: the report says the parser is {parser:?}"
        );
        assert_eq!(executable, def.executable, "{path}");
    }
}

#[test]
fn every_redirect_goes_where_the_report_says() {
    // The aliases (`xp` to `experience`) and the 103 `execute` cycles are all
    // here. A swapped target resolves to a real node with the wrong name,
    // which only these rows can catch.
    assert_eq!(REDIRECT_SAMPLES.len(), 108);
    for &(from, to) in REDIRECT_SAMPLES {
        let source = CommandGraph::resolve(from).unwrap_or_else(|| panic!("{from}"));
        let def = CommandGraph::def(source).expect("in range");
        let target = def
            .redirect
            .unwrap_or_else(|| panic!("{from} has no redirect in the table"))
            as usize;
        let resolved_to = CommandGraph::resolve(to).unwrap_or_else(|| {
            panic!("the report redirects {from} to {to}, which does not resolve")
        });
        assert_eq!(
            target, resolved_to,
            "{from} redirects somewhere other than {to}"
        );
    }
}

#[test]
fn the_two_dead_nodes_are_exactly_execute_run_and_return_run() {
    // Named values, not counts: if a third node ever went dead, or one of
    // these came back to life, this list is where somebody meets it. The paths
    // are rebuilt from the table rather than stored — which is also the proof
    // that reconstruction works for exactly the nodes whose paths matter.
    let dead: Vec<String> = CommandGraph::unreachable_nodes().map(path_of).collect();
    assert_eq!(dead, ["execute/run", "return/run"]);
    assert_eq!(UNREACHABLE.len(), 2);
}

/// The slash-joined path of a node, rebuilt from the table alone.
///
/// This is the reconstruction the golden rows depend on, exercised directly so
/// it cannot quietly stop working while the sample loop keeps passing on rows
/// it happens to agree with.
fn path_of(index: usize) -> String {
    let mut parts = Vec::new();
    let mut current = index;
    while current != CommandGraph::ROOT {
        parts.push(NODES[current].name);
        // Walk back up by finding the parent: linear, but this runs twice in
        // tests, not in a dispatcher.
        current = NODES
            .iter()
            .position(|def| def.children.contains(&(current as u16)))
            .expect("every non-root node has a parent");
    }
    parts.reverse();
    parts.join("/")
}

#[test]
fn cycles_terminate_and_the_execute_loop_is_the_one_that_loops() {
    // Following redirects from inside `execute` reaches `execute` again. The
    // chain stops on the first repeat — the honest answer to "where does this
    // go" — and the alias redirects end on their targets.
    let execute = CommandGraph::resolve("execute").expect("execute exists");
    let chain = CommandGraph::redirect_chain(execute);
    assert_eq!(*chain.last().expect("non-empty"), execute, "it loops");

    let xp = CommandGraph::resolve("xp").expect("xp exists");
    let aliased = CommandGraph::redirect_chain(xp);
    assert_eq!(aliased.len(), 2, "one hop");
    assert_eq!(
        CommandGraph::def(aliased[1]).expect("in range").name,
        "experience"
    );

    // And a command with no redirect is a chain of itself.
    let give = CommandGraph::resolve("give").expect("give exists");
    assert_eq!(CommandGraph::redirect_chain(give), vec![give]);
}

#[test]
fn the_named_facts_about_this_graph_are_still_true() {
    // Numbers from the 1.21.1 report, written down so a change has to explain
    // itself. Nothing here derives them from the table — that would make this
    // test unable to fail.
    assert_eq!(NODE_COUNT, 1763);
    assert_eq!(EXECUTABLE_COUNT, 1007);
    assert_eq!(MAX_DEPTH, 13);
    assert_eq!(PARSER_USES.len(), 51);
    assert_eq!(
        PARSER_USES.iter().map(|(_, uses)| uses).sum::<u16>() as usize,
        NODES.iter().filter(|d| d.parser.is_some()).count()
    );
}

#[test]
fn argument_properties_survive_the_trip_through_a_table_of_constants() {
    // Rows resolved through the table the way a dispatcher would, with the
    // values Mojang's report states: `/time set` takes a time argument whose
    // report carries `min: 0`, and `/advancement grant` takes entities —
    // multiple of them, players only.
    let entity = CommandGraph::resolve("advancement/grant/targets").expect("resolves");
    assert_eq!(
        CommandGraph::def(entity).and_then(|d: &CommandDef| d.properties),
        Some(ArgumentProperties::Entity {
            single: false,
            players_only: true
        })
    );

    let time = CommandGraph::resolve("time/set/time").expect("resolves");
    assert_eq!(
        CommandGraph::def(time).and_then(|d| d.properties),
        Some(ArgumentProperties::Time { min: 0 })
    );
    assert_eq!(
        CommandGraph::def(time).and_then(|d| d.parser),
        Some("minecraft:time")
    );

    // And a literal carries neither parser nor properties.
    let everything =
        CommandGraph::resolve("advancement/grant/targets/everything").expect("a literal");
    assert_eq!(
        CommandGraph::def(everything).map(|d| d.kind),
        Some(NodeKind::Literal)
    );
    assert_eq!(CommandGraph::def(everything).and_then(|d| d.parser), None);
}

#[test]
fn every_parser_used_is_an_entry_of_the_argument_type_registry() {
    // The extraction checked this against the registry report; this re-checks
    // it against the registry *table* that came out of that same report, so
    // the two generated files cannot drift apart after the fact.
    use dust_registry::Registry;

    let registry = Registry::from_name("minecraft:command_argument_type")
        .expect("the registry is extracted beside this table");
    for (parser, _) in PARSER_USES {
        assert!(
            registry.entry_id(parser).is_some(),
            "{parser} is used as a parser and is missing from the registry"
        );
    }
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(
        dust_registry::generated::commands::DATA_VERSION,
        DATA_VERSION
    );
}

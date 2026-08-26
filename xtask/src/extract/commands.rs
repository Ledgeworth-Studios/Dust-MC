//! Reading `reports/commands.json` — the complete brigadier command graph.
//!
//! 1,763 nodes: one root, 816 literals and 946 arguments, 83 commands at the
//! top, 13 levels deep at the deepest, 1,007 of them executable. Phase 1 sends
//! this graph to the client as `declare_commands`, which is what gives a
//! vanilla client its tab completion and its client-side syntax colouring, and
//! Phase 3 needs it again for a dispatcher.
//!
//! # It is not a tree, and the report only looks like one
//!
//! The JSON is a tree — every node written out inside its parent — with a
//! `redirect` field that names a path back into it. Following those turns it
//! into a graph with cycles, and a walker written against the JSON's shape
//! rather than the graph's will run forever on `/execute`.
//!
//! Measured on 1.21.1 rather than assumed:
//!
//! - **108 redirect edges to 5 distinct targets.** 103 of them point at
//!   `execute`, and every one of those 103 is a descendant of `execute`. Those
//!   are the cycles. The shortest is three edges (`execute → as → targets →
//!   execute`); the longest is eight.
//! - **The other five are aliases and are not cycles**: `tell` and `w` redirect
//!   to `msg`, `tm` to `teammsg`, `tp` to `teleport`, `xp` to `experience`.
//!   Every one is a root command pointing at another root command, and none of
//!   the targets points back.
//! - **One non-trivial strongly connected component, of 268 nodes**, and it is
//!   the `execute` subtree. Everything else is acyclic.
//!
//! So the generated table is a flat array of nodes addressed by index, and a
//! redirect is an index like any other. Cycles are representable rather than
//! survivable: nothing recursive is built at generation time, and the walkers
//! in `dust-registry` carry a visited set rather than a depth limit. A depth
//! limit would be a number somebody guessed, and the graph would still be
//! cyclic underneath it.
//!
//! # Two nodes the report cannot describe
//!
//! `execute/run` and `return/run` are `{"type": "literal"}` and nothing else:
//! no children, not executable, no redirect. A node like that can never be the
//! end of a command and can never continue one, so as written they are dead.
//!
//! In the game they redirect to the *root* — `/execute run <any command>` is
//! the whole point of `/execute`. The report cannot say so, because a redirect
//! is a path and the root's path is empty. This extraction does not invent the
//! edge: it emits what the report says and names the anomaly, in the generated
//! table and in a test, so that whoever builds `declare_commands` meets it
//! deliberately instead of discovering it from a client that will not run
//! `/execute run`. Inventing the edge here would be encoding knowledge from
//! outside the report into a table whose whole value is that it came from the
//! report.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value as Json;

use super::numbers::check_every_number_reprints;
use super::registries::Registries;

/// One node, as `reports/commands.json` describes it, with the report's own
/// child order preserved.
///
/// The order matters enough to check. `blocks.rs` exists because a report's
/// serialisation order turned out to disagree with the order the data actually
/// used for four blocks, and a `BTreeMap` here would have thrown away the
/// evidence before anybody could look at it. So children are read as a list of
/// pairs, in document order, and [`check_child_order_is_name_order`] compares
/// the two.
#[derive(Debug)]
pub struct ReportedNode {
    pub kind: String,
    pub children: Vec<(String, ReportedNode)>,
    pub executable: bool,
    pub redirect: Option<Vec<String>>,
    pub parser: Option<String>,
    pub properties: Option<BTreeMap<String, Json>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Root,
    Literal,
    Argument,
}

/// A node's parser properties, typed.
///
/// Typed here and generic for item components, which is the same rule reaching
/// two different answers: type what the data can check. Eleven of the 51
/// parsers carry properties, between them in four shapes, and every field of
/// every shape appears in the report — so a struct here is a description of
/// data rather than a guess at it. An unrecognised key stops the extraction
/// instead of being dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum Properties {
    Integer {
        min: Option<i32>,
        max: Option<i32>,
    },
    Float {
        min: Option<f32>,
        max: Option<f32>,
    },
    Double {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// `word`, `phrase` or `greedy`.
    StringKind(String),
    Entity {
        single: bool,
        players_only: bool,
    },
    ScoreHolder {
        single: bool,
    },
    /// The registry the argument names something in.
    Resource {
        registry: String,
    },
    Time {
        min: i32,
    },
}

/// A node, flattened out of the tree and given an index.
#[derive(Debug, Clone)]
pub struct Node {
    /// Slash-joined path from the root, for the generated file's comments and
    /// for the golden samples. Not an identity: two nodes can be reached by
    /// more than one path once redirects are followed.
    pub path: String,
    pub kind: Kind,
    /// Empty for the root.
    pub name: String,
    /// Indices into [`Commands::nodes`], sorted by the child's name.
    pub children: Vec<usize>,
    pub executable: bool,
    pub redirect: Option<usize>,
    pub parser: Option<String>,
    pub properties: Option<Properties>,
}

#[derive(Debug)]
pub struct Commands {
    /// Every node. Index 0 is the root; the rest are in depth-first order with
    /// children in name order, so a command's subtree is contiguous and the
    /// generated file reads as one command after another.
    pub nodes: Vec<Node>,
    /// The distinct parsers, name-sorted, with how many argument nodes use each.
    pub parsers: Vec<(String, usize)>,
    /// Indices of nodes that can neither end a command nor continue one.
    pub unreachable: Vec<usize>,
    /// `(source, target)` node indices, for reporting.
    pub redirects: Vec<(usize, usize)>,
    pub max_depth: usize,
    pub executable_count: usize,
    pub number_count: usize,
    /// Registries named by `resource`-family parsers that are not in the
    /// registry report, because they come from the data pack instead.
    pub unchecked_registries: BTreeSet<String>,
    /// The report as it was read, kept so the golden sample can be taken from
    /// its own tree rather than from anything [`flatten`] derived. Same rule as
    /// `Blocks::reported`: the rows have to be able to disagree with the table
    /// for them to be worth asserting.
    pub reported: ReportedNode,
}

pub fn parse(json: &[u8], registries: &Registries) -> Result<Commands, String> {
    let reported: ReportedNode =
        serde_json::from_slice(json).map_err(|e| format!("could not read commands.json: {e}"))?;
    let number_count = check_every_number_reprints(json, "commands.json")?;
    check_child_order_is_name_order(&reported, "")?;

    let mut nodes = Vec::new();
    let mut by_path = BTreeMap::new();
    flatten(&reported, String::new(), &mut nodes, &mut by_path)?;

    // Redirects are resolved after every node has an index, which is the whole
    // reason the table is flat: a redirect from inside `execute` back to
    // `execute` is a backward index, and there is no order in which a tree of
    // owned values could hold it.
    let mut redirects = Vec::new();
    for (index, flattened) in nodes.iter_mut().enumerate() {
        let Some(path) = flattened.redirect_path.clone() else {
            continue;
        };
        let joined = path.join("/");
        let target = *by_path.get(&joined).ok_or_else(|| {
            format!(
                "{} redirects to {joined}, which is not a node in this report",
                flattened.node.path
            )
        })?;
        flattened.node.redirect = Some(target);
        redirects.push((index, target));
    }

    let mut nodes: Vec<Node> = nodes.into_iter().map(|n| n.node).collect();
    // Children were collected in the report's order; they are stored in name
    // order because the crate binary-searches them. The two agree on 1.21.1,
    // which is what `check_child_order_is_name_order` above just insisted on.
    // Names are read through a copy because a child of one node is another
    // node the loop is about to visit.
    let names: Vec<String> = nodes.iter().map(|n| n.name.clone()).collect();
    for node in &mut nodes {
        node.children
            .sort_by(|a, b| names[*a].cmp(&names[*b]));
    }

    let mut parsers: BTreeMap<String, usize> = BTreeMap::new();
    for node in &nodes {
        if let Some(parser) = &node.parser {
            *parsers.entry(parser.clone()).or_default() += 1;
        }
    }

    let unreachable = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.kind != Kind::Root
                && node.children.is_empty()
                && !node.executable
                && node.redirect.is_none()
        })
        .map(|(index, _)| index)
        .collect();

    let max_depth = nodes
        .iter()
        .map(|node| node.path.split('/').filter(|s| !s.is_empty()).count())
        .max()
        .unwrap_or(0);
    let executable_count = nodes.iter().filter(|node| node.executable).count();

    let commands = Commands {
        parsers: parsers.into_iter().collect(),
        unreachable,
        redirects,
        max_depth,
        executable_count,
        number_count,
        unchecked_registries: BTreeSet::new(),
        reported,
        nodes,
    };
    check_kinds_and_parsers(&commands)?;
    check_parsers_are_argument_types(&commands, registries)?;
    let unchecked = check_named_registries_exist(&commands, registries)?;
    Ok(Commands {
        unchecked_registries: unchecked,
        ..commands
    })
}

/// Every parser an argument uses is an entry of the `command_argument_type`
/// registry.
///
/// Two reports agreeing again, and the check earns its place the way the others
/// do: brigadier spells the parser exactly as the registry names it today, and
/// if that ever stops being true — a new parser shipping before its registry
/// entry, or a rename reaching one file and not the other — the extraction has
/// to stop while the disagreement is still a puzzle somebody can look at,
/// rather than generate a table whose parser strings decode against nothing.
///
/// The reverse is reported rather than required: three registered argument
/// types are never used as a parser in vanilla's graph (`brigadier:long`,
/// `minecraft:float_range`, `minecraft:uuid`). They exist for map makers and
/// are left alone.
fn check_parsers_are_argument_types(
    commands: &Commands,
    registries: &Registries,
) -> Result<(), String> {
    let registry = registries
        .registries
        .iter()
        .find(|r| r.name == "minecraft:command_argument_type")
        .ok_or("the registry report has no minecraft:command_argument_type")?;
    for node in &commands.nodes {
        let Some(parser) = &node.parser else {
            continue;
        };
        if !registry.entries.iter().any(|e| &e.name == parser) {
            return Err(format!(
                "{} uses the parser {parser}, which is not an entry of the \
                 command_argument_type registry",
                node.path
            ));
        }
    }
    Ok(())
}

/// A node on its way out of the tree, still holding the redirect as a path.
struct Flattened {
    node: Node,
    redirect_path: Option<Vec<String>>,
}

fn flatten(
    reported: &ReportedNode,
    path: String,
    out: &mut Vec<Flattened>,
    by_path: &mut BTreeMap<String, usize>,
) -> Result<usize, String> {
    let kind = match reported.kind.as_str() {
        "root" => Kind::Root,
        "literal" => Kind::Literal,
        "argument" => Kind::Argument,
        other => {
            return Err(format!(
                "{path} has type {other:?}, which is not a node type"
            ))
        }
    };
    let name = path.rsplit('/').next().unwrap_or_default().to_owned();
    let properties = match (&reported.parser, &reported.properties) {
        (Some(parser), Some(properties)) => Some(convert_properties(&path, parser, properties)?),
        (None, Some(_)) => {
            return Err(format!(
                "{path} has properties and no parser, so nothing says how to read them"
            ))
        }
        _ => None,
    };

    let index = out.len();
    out.push(Flattened {
        node: Node {
            path: path.clone(),
            kind,
            name,
            children: Vec::new(),
            executable: reported.executable,
            redirect: None,
            parser: reported.parser.clone(),
            properties,
        },
        redirect_path: reported.redirect.clone(),
    });
    if by_path.insert(path.clone(), index).is_some() {
        return Err(format!("two nodes have the path {path}"));
    }

    // Depth-first, children in name order, so a command's subtree is contiguous
    // in the generated table and the file reads as one command after another.
    let mut children: Vec<&(String, ReportedNode)> = reported.children.iter().collect();
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (child_name, child) in children {
        let child_path = if path.is_empty() {
            child_name.clone()
        } else {
            format!("{path}/{child_name}")
        };
        let child_index = flatten(child, child_path, out, by_path)?;
        out[index].node.children.push(child_index);
    }
    Ok(index)
}

fn convert_properties(
    path: &str,
    parser: &str,
    properties: &BTreeMap<String, Json>,
) -> Result<Properties, String> {
    let known = |keys: &[&str]| -> Result<(), String> {
        for key in properties.keys() {
            if !keys.contains(&key.as_str()) {
                return Err(format!(
                    "{path}: {parser} has a property {key:?}, which this extraction does not \
                     know how to read. Dropping it would be information disappearing quietly, \
                     so it stops here instead."
                ));
            }
        }
        Ok(())
    };
    let int = |key: &str| -> Result<Option<i32>, String> {
        match properties.get(key) {
            None => Ok(None),
            Some(Json::Number(n)) => n
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .map(Some)
                .ok_or_else(|| format!("{path}: {parser}'s {key} is {n}, which is not an i32")),
            Some(other) => Err(format!("{path}: {parser}'s {key} is {other}, not a number")),
        }
    };
    let double = |key: &str| -> Result<Option<f64>, String> {
        match properties.get(key) {
            None => Ok(None),
            Some(Json::Number(n)) => n
                .as_f64()
                .map(Some)
                .ok_or_else(|| format!("{path}: {parser}'s {key} is {n}, which is not an f64")),
            Some(other) => Err(format!("{path}: {parser}'s {key} is {other}, not a number")),
        }
    };
    // A `brigadier:float` bound is an f32 on the wire, and every one in this
    // report is exactly representable as one — checked here rather than
    // assumed, because narrowing a bound that is not would move it, and a
    // moved bound accepts or rejects an input the real server would not.
    let single = |key: &str| -> Result<Option<f32>, String> {
        let Some(value) = double(key)? else {
            return Ok(None);
        };
        let narrowed = value as f32;
        if f64::from(narrowed) != value {
            return Err(format!(
                "{path}: {parser}'s {key} is {value:?}, which is not exactly an f32, and the \
                 wire sends this bound as one"
            ));
        }
        Ok(Some(narrowed))
    };
    let string = |key: &str, allowed: &[&str]| -> Result<String, String> {
        match properties.get(key) {
            Some(Json::String(s)) if allowed.contains(&s.as_str()) => Ok(s.clone()),
            other => Err(format!(
                "{path}: {parser}'s {key} is {other:?}, and the values seen are {allowed:?}"
            )),
        }
    };

    Ok(match parser {
        "brigadier:integer" => {
            known(&["min", "max"])?;
            Properties::Integer {
                min: int("min")?,
                max: int("max")?,
            }
        }
        "brigadier:float" => {
            known(&["min", "max"])?;
            Properties::Float {
                min: single("min")?,
                max: single("max")?,
            }
        }
        "brigadier:double" => {
            known(&["min", "max"])?;
            Properties::Double {
                min: double("min")?,
                max: double("max")?,
            }
        }
        "brigadier:string" => {
            known(&["type"])?;
            Properties::StringKind(string("type", &["word", "phrase", "greedy"])?)
        }
        "minecraft:entity" => {
            known(&["amount", "type"])?;
            Properties::Entity {
                single: string("amount", &["single", "multiple"])? == "single",
                players_only: string("type", &["players", "entities"])? == "players",
            }
        }
        "minecraft:score_holder" => {
            known(&["amount"])?;
            Properties::ScoreHolder {
                single: string("amount", &["single", "multiple"])? == "single",
            }
        }
        "minecraft:time" => {
            known(&["min"])?;
            Properties::Time {
                min: int("min")?.ok_or_else(|| format!("{path}: time has no min"))?,
            }
        }
        "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key" => {
            known(&["registry"])?;
            match properties.get("registry") {
                Some(Json::String(registry)) => Properties::Resource {
                    registry: registry.clone(),
                },
                other => return Err(format!("{path}: {parser}'s registry is {other:?}")),
            }
        }
        other => {
            return Err(format!(
                "{path}: {other} carries properties {properties:?}, and this extraction has \
                 no shape for them. Eleven parsers had properties on 1.21.1; a twelfth is a \
                 thing to look at rather than to drop."
            ))
        }
    })
}

/// The report's child order is the children's name order.
///
/// Checked because the generated table stores one order and not two, and a
/// version where the report's order carried information — brigadier tries
/// argument children in insertion order, and two nodes here have more than one
/// — would make that a lossy choice. It agrees for all 209 multi-child nodes on
/// 1.21.1. If it ever stops, the table needs both orders, and this says so
/// rather than sorting over the top of the evidence.
fn check_child_order_is_name_order(node: &ReportedNode, path: &str) -> Result<(), String> {
    let reported: Vec<&String> = node.children.iter().map(|(name, _)| name).collect();
    let mut sorted = reported.clone();
    sorted.sort();
    if reported != sorted {
        return Err(format!(
            "the children of {:?} are serialised as {reported:?} and sort as {sorted:?}. The \
             generated table keeps one order; this one carries information, so it needs both.",
            if path.is_empty() { "<root>" } else { path }
        ));
    }
    for (name, child) in &node.children {
        let child_path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        check_child_order_is_name_order(child, &child_path)?;
    }
    Ok(())
}

/// An argument has a parser, a literal does not, and the root is the root.
fn check_kinds_and_parsers(commands: &Commands) -> Result<(), String> {
    for (index, node) in commands.nodes.iter().enumerate() {
        match node.kind {
            Kind::Root if index != 0 => {
                return Err(format!("{} is a second root node", node.path));
            }
            Kind::Root => {
                if !node.name.is_empty() {
                    return Err(format!("the root is named {:?}", node.name));
                }
            }
            Kind::Argument if node.parser.is_none() => {
                return Err(format!("{} is an argument with no parser", node.path));
            }
            Kind::Literal if node.parser.is_some() => {
                return Err(format!(
                    "{} is a literal with a parser, which is a node this table cannot describe",
                    node.path
                ));
            }
            _ => {}
        }
        if node.kind != Kind::Root && node.name.is_empty() {
            return Err(format!("the node at {} has no name", node.path));
        }
    }
    if commands.nodes.first().map(|n| n.kind) != Some(Kind::Root) {
        return Err("node 0 is not the root".to_owned());
    }
    Ok(())
}

/// Registries named by `resource`-family parsers exist, where this extraction
/// has anything to check them against.
///
/// Six of the ten are data pack registries — `minecraft:enchantment`,
/// `minecraft:damage_type`, `minecraft:worldgen/biome` and friends — which live
/// in the generated *data*, not in the registry report, and so cannot be
/// checked here. They are returned rather than ignored, so "not checked" is
/// something the extraction says out loud on every run.
fn check_named_registries_exist(
    commands: &Commands,
    registries: &Registries,
) -> Result<BTreeSet<String>, String> {
    let known: BTreeSet<&str> = registries
        .registries
        .iter()
        .map(|r| r.name.as_str())
        .chain(std::iter::once(registries.block.name.as_str()))
        .collect();
    let mut unchecked = BTreeSet::new();
    for node in &commands.nodes {
        let Some(Properties::Resource { registry }) = &node.properties else {
            continue;
        };
        if !known.contains(registry.as_str()) {
            unchecked.insert(registry.clone());
        }
    }
    Ok(unchecked)
}

// The report's child order is evidence, and a BTreeMap would throw it away
// before anything could look at it — so children are read as a list of pairs in
// document order. serde's MapAccess streams entries in the order the file has
// them, which is the only reason this is possible without a dependency that
// keeps insertion order.
impl<'de> Deserialize<'de> for ReportedNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NodeVisitor;

        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = ReportedNode;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a command node")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ReportedNode, A::Error> {
                let mut kind = None;
                let mut children = Vec::new();
                let mut executable = false;
                let mut redirect = None;
                let mut parser = None;
                let mut properties = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => kind = Some(map.next_value()?),
                        "children" => children = map.next_value::<OrderedChildren>()?.0,
                        "executable" => executable = map.next_value()?,
                        "redirect" => redirect = Some(map.next_value()?),
                        "parser" => parser = Some(map.next_value()?),
                        "properties" => properties = Some(map.next_value()?),
                        other => {
                            // A field nobody read is a field nobody knows was
                            // there, and this report gains one every few
                            // versions.
                            return Err(de::Error::custom(format!(
                                "a command node has a field {other:?} that this extraction \
                                 does not read"
                            )));
                        }
                    }
                }
                Ok(ReportedNode {
                    kind: kind.ok_or_else(|| de::Error::missing_field("type"))?,
                    children,
                    executable,
                    redirect,
                    parser,
                    properties,
                })
            }
        }

        deserializer.deserialize_map(NodeVisitor)
    }
}

struct OrderedChildren(Vec<(String, ReportedNode)>);

impl<'de> Deserialize<'de> for OrderedChildren {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ChildrenVisitor;

        impl<'de> Visitor<'de> for ChildrenVisitor {
            type Value = OrderedChildren;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of command nodes")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedChildren, A::Error> {
                let mut children = Vec::new();
                while let Some((name, node)) = map.next_entry()? {
                    children.push((name, node));
                }
                Ok(OrderedChildren(children))
            }
        }

        deserializer.deserialize_map(ChildrenVisitor)
    }
}

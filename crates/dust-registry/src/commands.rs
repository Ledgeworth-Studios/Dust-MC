//! The vanilla command graph: brigadier's nodes, as data.
//!
//! 1,763 nodes — one root, 816 literals, 946 arguments — 83 commands at the
//! top, 13 levels at the deepest. Phase 1 sends this to the client as
//! `declare_commands`, which is what gives a vanilla client tab completion and
//! client-side syntax colouring for commands the server has not implemented
//! yet, and Phase 3 needs it again.
//!
//! # This is the graph and not a dispatcher, deliberately
//!
//! Nothing here parses user input, checks a permission, or runs anything. There
//! is no `Command::execute`, no argument parsing behind
//! [`ParserProperties::Integer`], and no suggestion provider. Phase 3 owns
//! that, and a half-built dispatcher would be worse than none: it would be
//! reached for, would work for `/say`, and would be discovered to be a
//! pretence somewhere around `/execute if score`. What this crate offers is the
//! shape of the graph, exactly as Minecraft's own generator described it.
//!
//! # It is a graph, not a tree, and the difference is `/execute`
//!
//! 108 nodes redirect. 103 of them point at `execute` and every one is a
//! descendant of `execute`, which makes one strongly connected component of 268
//! nodes and cycles from three to eight edges long. The other five are aliases:
//! `tell` and `w` to `msg`, `tm` to `teammsg`, `tp` to `teleport`, `xp` to
//! `experience`.
//!
//! So the table is flat and a redirect is an index like any other — cycles are
//! representable by construction, rather than being something the shape has to
//! survive. Every walker here carries a visited set: [`Node::reachable`] will
//! not loop, and [`Node::resolve`] will not loop on a redirect chain. Neither
//! has a depth limit, because a depth limit is a number somebody guessed and
//! the graph is still cyclic underneath it.
//!
//! # `execute run` and `return run` are dead in the report
//!
//! Both are a literal with no children, not executable, no redirect — a node
//! that can neither end a command nor continue one. In the game they redirect
//! to the *root*, which is what makes `/execute run <anything>` work, and the
//! report cannot say so because a redirect is a path and the root's path is
//! empty.
//!
//! The extractor does not invent the edge, because the value of this table is
//! that it came from the report. It names them instead: [`UNREACHABLE`] holds
//! them, and a test in `tests/commands.rs` asserts there are exactly two and
//! that both are called `run`. Whoever builds `declare_commands` will meet this
//! deliberately rather than discovering it from a client that will not run
//! `/execute run`.

use crate::generated::commands::{NODES, UNREACHABLE};

/// What kind of node this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// The one node everything hangs off. Not a command.
    Root,
    /// A word typed literally: the `if` in `/execute if`.
    Literal,
    /// A value read by a parser: the `targets` in `/execute as <targets>`.
    Argument,
}

/// How much of the input a `brigadier:string` argument takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StringKind {
    /// One unquoted word.
    Word,
    /// One word, or several inside quotes.
    Phrase,
    /// The rest of the line, quotes and all.
    Greedy,
}

/// The properties an argument's parser was configured with.
///
/// Typed, unlike the item components in [`crate::items`], and for the reason
/// that decided those: type what the data can check. Eleven of the 51 parsers
/// carry properties, between them in these shapes, and every field of every
/// shape appears in the report — so this is a description of data rather than a
/// guess at it. A twelfth parser with properties, or a new key on one of these,
/// stops `cargo xtask extract` rather than being dropped on the way in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParserProperties {
    /// `brigadier:integer`. Absent bounds are brigadier's own, which are the
    /// full range of the type.
    Integer {
        min: Option<i32>,
        max: Option<i32>,
    },
    /// `brigadier:float`. `f32` because that is the width the wire sends these
    /// bounds at, and the extractor checks every one is exactly representable
    /// as one before narrowing it.
    Float {
        min: Option<f32>,
        max: Option<f32>,
    },
    /// `brigadier:double`.
    Double {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// `brigadier:string`.
    Str(StringKind),
    /// `minecraft:entity`: how many, and whether only players.
    Entity { single: bool, players_only: bool },
    /// `minecraft:score_holder`: how many.
    ScoreHolder { single: bool },
    /// `minecraft:resource` and its three relatives: which registry the
    /// argument names something in.
    ///
    /// Six of the ten registries named this way are data pack registries —
    /// `minecraft:enchantment`, `minecraft:worldgen/biome` and friends — which
    /// are not in the registry report and so are not among [`crate::Registry`].
    /// The extractor says which on every run rather than leaving "unchecked"
    /// looking like "checked".
    Resource { registry: &'static str },
    /// `minecraft:time`, whose minimum is in ticks.
    Time { min: i32 },
}

/// One node, as the generated table holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandNode {
    pub kind: Kind,
    /// The literal word, or the argument's name. Empty for the root.
    pub name: &'static str,
    /// Indices into the node table, sorted by the child's name.
    pub children: &'static [u16],
    /// Whether a command may end here.
    pub executable: bool,
    /// Where parsing continues instead of at this node's children.
    pub redirect: Option<u16>,
    /// The parser id for an argument, e.g. `minecraft:entity`.
    pub parser: Option<&'static str>,
    pub properties: Option<ParserProperties>,
}

/// A node in the command graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node(u16);

impl Node {
    /// The root. Every command hangs off it and it is not one itself.
    pub const fn root() -> Self {
        Self(0)
    }

    /// The node at an index in the generated table, or `None` if there is none.
    pub fn at(index: u16) -> Option<Self> {
        ((index as usize) < NODES.len()).then_some(Self(index))
    }

    /// This node's index, which is what a redirect holds and what
    /// `declare_commands` will send.
    pub fn index(self) -> u16 {
        self.0
    }

    pub fn kind(self) -> Kind {
        self.def().kind
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    pub fn is_executable(self) -> bool {
        self.def().executable
    }

    pub fn parser(self) -> Option<&'static str> {
        self.def().parser
    }

    pub fn properties(self) -> Option<ParserProperties> {
        self.def().properties
    }

    /// Where parsing continues instead of at this node's children.
    pub fn redirect(self) -> Option<Self> {
        self.def().redirect.map(Self)
    }

    /// This node's children, in name order.
    pub fn children(self) -> impl Iterator<Item = Self> {
        self.def().children.iter().copied().map(Self)
    }

    /// The child with this name.
    ///
    /// A binary search, which is sound because the extractor stores children in
    /// name order and refuses a report whose own order disagrees.
    pub fn child(self, name: &str) -> Option<Self> {
        let children = self.def().children;
        let position = children
            .binary_search_by(|index| Self(*index).name().cmp(name))
            .ok()?;
        Some(Self(children[position]))
    }

    /// Walk a path of node names from here, following redirects.
    ///
    /// `Node::root().resolve(&["execute", "as"])` is the `as` of `/execute as`.
    /// A redirect is followed when this node has no child of the wanted name
    /// and does redirect — which is what makes
    /// `["execute", "as", "targets", "at"]` resolve at all, since `targets`
    /// redirects to `execute` and `at` is one of `execute`'s children.
    ///
    /// Termination comes from a visited set and not from a depth limit: 103
    /// nodes redirect into `execute`, so a redirect chain could be made to loop
    /// and a guessed limit would be both arbitrary and wrong somewhere.
    ///
    /// This resolves *names*. It is not a parser: it does not read arguments,
    /// does not check that a value fits a parser's properties, and does not
    /// know what a command does.
    pub fn resolve(self, path: &[&str]) -> Option<Self> {
        let mut node = self;
        for segment in path {
            let mut hops = Vec::new();
            loop {
                if let Some(child) = node.child(segment) {
                    node = child;
                    break;
                }
                let Some(next) = node.redirect() else {
                    return None;
                };
                if hops.contains(&next) {
                    // A redirect cycle with no matching child anywhere in it.
                    return None;
                }
                hops.push(next);
                node = next;
            }
        }
        Some(node)
    }

    /// Every node reachable from here, breadth-first, including this one.
    ///
    /// A `Vec` and not an iterator because reachability needs a visited set to
    /// terminate, and a lazy iterator that owns one is a worse thing to read
    /// than a list. `Node::root().reachable()` is 1,763 nodes and takes
    /// microseconds.
    pub fn reachable(self) -> Vec<Self> {
        let mut seen = vec![false; NODES.len()];
        let mut queue = std::collections::VecDeque::from([self]);
        let mut out = Vec::new();
        seen[self.0 as usize] = true;
        while let Some(node) = queue.pop_front() {
            out.push(node);
            for next in node.children().chain(node.redirect()) {
                if !std::mem::replace(&mut seen[next.0 as usize], true) {
                    queue.push_back(next);
                }
            }
        }
        out
    }

    /// Every node in the table, in the order it is stored: the root, then each
    /// command's subtree in turn.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..NODES.len() as u16).map(Self)
    }

    /// The nodes the report describes as unable to end a command or continue
    /// one — `execute/run` and `return/run` on 1.21.1. See this module's
    /// documentation for why they are like that and why nothing here fixes it.
    pub fn unreachable() -> impl Iterator<Item = Self> {
        UNREACHABLE.iter().copied().map(Self)
    }

    fn def(self) -> &'static CommandNode {
        &NODES[self.0 as usize]
    }
}

//! The brigadier command graph, and how to walk it without falling in.
//!
//! `cargo xtask extract` reads Minecraft's own command report and commits this
//! table: every literal and argument the vanilla client is told about, as
//! [`CommandDef`] nodes addressed by index, with children and redirects as
//! indices into the same array.
//!
//! # Why a flat array and not a tree
//!
//! The report looks like a tree — every node written inside its parent — but
//! carries a `redirect` field naming a path back into it. Following those turns
//! the shape into a graph with cycles: on 1.21.1 there are 108 redirects and
//! 103 of them point at `execute`, most from inside `execute` itself. An owned
//! tree cannot hold a child that is also an ancestor; an index can. Nothing in
//! the table resolves a cycle, and nothing here does either — that is what
//! [`CommandGraph::walk`] carrying a visited set is for.
//!
//! A depth limit would have been the other option, and it was not taken: a
//! limit is a number somebody guessed, and the graph stays cyclic underneath
//! whatever number is picked. A visited set terminates on any shape the data
//! can actually have.
//!
//! # What a redirect means
//!
//! `/tp` is not a command of its own; its node redirects to `teleport`. Five of
//! the 108 are those aliases (`tell`/`w` to `msg`, `tm` to `teammsg`, `tp` to
//! `teleport`, `xp` to `experience`). The rest are the `execute` recursion. At
//! dispatch time a redirect means "this node has no meaning of its own; keep
//! walking from there".
//!
//! # Two dead nodes, named rather than fixed
//!
//! `execute/run` and `return/run` are `{"type": "literal"}` and nothing else:
//! no children, not executable, no redirect. As written they can neither end a
//! command nor continue one, which makes them unreachable by construction. In
//! the game they redirect to the *root* — `/execute run <anything>` is the
//! entire point of `/execute` — but the report cannot say so, because a
//! redirect is a path and the root's path is empty.
//!
//! This extraction does not invent the edge. Inventing it would put knowledge
//! from outside the report into a table whose whole value is that it came from
//! the report. The two nodes are listed in [`generated::commands::UNREACHABLE`]
//! instead, and whoever builds `declare_commands` meets them deliberately,
//! with this paragraph beside them, rather than from a client that will not run
//! `/execute run`.
//!
//! # What this table is not
//!
//! It is the graph's shape, not its semantics: no argument parsing, no suggestion
//! providers (the report does not carry them), no permission checks, and no
//! execution. Phase 3's dispatcher reads this for tab completion and syntax;
//! what an argument *accepts* is that phase's problem.

use crate::generated::commands::{NODES, NODE_COUNT, UNREACHABLE};

/// Which of the three node shapes this is.
///
/// Hand-written rather than generated because it is the protocol's fixed
/// vocabulary — brigadier has exactly these three — and a fourth would be a
/// change to think about rather than absorb. The extractor holds the same three
/// and refuses anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    /// The unnamed node every path starts from.
    Root,
    /// A fixed word: `give`, `execute`, `as`.
    Literal,
    /// A value read with a parser: `minecraft:entity`, `brigadier:integer`.
    Argument,
}

impl NodeKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Literal => "literal",
            Self::Argument => "argument",
        }
    }
}

/// The properties an argument's parser constrains itself with, typed.
///
/// Eleven parsers carry properties, in these eight shapes, and every field of
/// every shape appears in the 1.21.1 report — so this is a description of data
/// rather than a guess at it. The extractor refuses an unrecognised key or a
/// twelfth shape rather than dropping either.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArgumentProperties {
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
    /// How much of the input one read takes: `word`, `phrase` or `greedy`.
    StringKind(&'static str),
    Entity {
        single: bool,
        players_only: bool,
    },
    ScoreHolder {
        single: bool,
    },
    /// The registry the argument names something in, e.g. `minecraft:function`.
    Resource {
        registry: &'static str,
    },
    Time {
        min: i32,
    },
}

/// One node of the command graph, as the generated table holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandDef {
    /// The word or argument name; empty only for the root.
    pub name: &'static str,
    pub kind: NodeKind,
    /// Indices into [`NODES`], sorted by name — which is what makes
    /// [`CommandGraph::resolve`] a chain of binary searches rather than scans.
    pub children: &'static [u16],
    /// Whether reaching this node ends a runnable command.
    pub executable: bool,
    /// Where control continues when this node is reached as an alias or an
    /// `execute` clause. Backward indices are normal: most point into
    /// `execute`.
    pub redirect: Option<u16>,
    /// The parser an argument reads its input with; every value here is an
    /// entry of the `command_argument_type` registry, checked at extraction.
    pub parser: Option<&'static str>,
    pub properties: Option<ArgumentProperties>,
}

/// The whole command graph, and the ways it is safe to move through it.
///
/// A node is an index into [`NODES`]; everything here hands out indices rather
/// than references-with-lifetimes so callers can store them freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandGraph;

impl CommandGraph {
    /// The index of the root, where every path starts.
    pub const ROOT: usize = 0;

    /// How many nodes the graph holds.
    pub fn len() -> usize {
        NODE_COUNT
    }

    pub fn is_empty() -> bool {
        false
    }

    /// The node at an index, or `None` past the end.
    ///
    /// Indices arrive from children and redirect arrays that were checked
    /// against this length at generation time, but they also arrive from
    /// callers, and a caller's index is exactly the kind of number to check.
    pub fn def(index: usize) -> Option<&'static CommandDef> {
        NODES.get(index)
    }

    /// Resolve a slash-joined path from the root, e.g. `execute/if/block`.
    ///
    /// Each step binary-searches the current node's children, so a miss stops
    /// early. Redirects are *not* followed: the path names the node the report
    /// filed it under, and `xp` resolving to the `xp` node rather than to
    /// `experience` is the difference between the two spellings.
    pub fn resolve(path: &str) -> Option<usize> {
        let mut current = Self::ROOT;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let def = Self::def(current)?;
            let position = def
                .children
                .binary_search_by(|&child| NODES[child as usize].name.cmp(part))
                .ok()?;
            current = def.children[position] as usize;
        }
        Some(current)
    }

    /// Every node reachable from the root, once each, depth-first.
    ///
    /// Cycles are followed around, not into: a visited set ends the walk
    /// wherever the graph loops, which is why this iterator terminates on
    /// `execute` while a naive recursive walk would not terminate at all.
    pub fn walk() -> impl Iterator<Item = usize> {
        let mut seen = vec![false; NODE_COUNT];
        let mut stack = vec![Self::ROOT];
        std::iter::from_fn(move || loop {
            let index = stack.pop()?;
            if seen[index] {
                continue;
            }
            seen[index] = true;
            // Pushed in reverse so the pop order visits children by name.
            stack.extend(NODES[index].children.iter().rev().map(|&c| c as usize));
            if let Some(redirect) = NODES[index].redirect {
                stack.push(redirect as usize);
            }
            return Some(index);
        })
    }

    /// The nodes that can neither end a command nor continue one.
    ///
    /// Two on 1.21.1 — `execute/run` and `return/run`. See the module header
    /// before deciding this list is a bug.
    pub fn unreachable_nodes() -> impl Iterator<Item = usize> {
        UNREACHABLE.iter().map(|&i| i as usize)
    }

    /// Follow redirects from a node until it reaches one with no redirect, or
    /// one already seen on this chain.
    ///
    /// Returns every node along the way, starting with `start`. The visited
    /// check is per call: `execute` redirects to itself, so "follow to the end"
    /// needs a stopping rule that comes from the walk and not from a guess
    /// about the data.
    pub fn redirect_chain(start: usize) -> Vec<usize> {
        let mut chain = vec![start];
        let mut current = start;
        while let Some(def) = Self::def(current) {
            let Some(next) = def.redirect else {
                break;
            };
            let next = next as usize;
            if chain.contains(&next) {
                break;
            }
            chain.push(next);
            current = next;
        }
        chain
    }
}

/// Re-exported for the tests that quote them, and for callers that want the
/// raw golden rows rather than going through [`CommandGraph`].
pub use crate::generated::commands::{EXECUTABLE_COUNT, MAX_DEPTH};

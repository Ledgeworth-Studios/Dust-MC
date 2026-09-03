//! The loot tables vanilla ships, as an inventory and a vocabulary.
//!
//! 1,178 tables on 1.21.1: one per block that drops anything, one per entity,
//! the chests, shearing, fishing, barter. What lives in this crate is which
//! tables exist, how they group, and which condition, function and pool-entry
//! types they are written with — the grammar of loot, ahead of any need to
//! speak it. No drop amount, roll or result survives extraction; those are
//! Mojang's data and stay on the machine that read them.
//!
//! # Two readings of one tree
//!
//! [`VOCABULARY`] comes from a walk that knows the format's positions: an
//! entry type is the `type` key of an object inside `entries` or `children`,
//! not the `type` of a number-provider argument buried in a function.
//! [`SOURCE_COUNTS`] comes from a pass with no position rules at all — every
//! string under `"condition"` or `"function"`, counted wherever it sits. The
//! two must agree exactly for those kinds; where they differ, one reading of
//! the tree misread it, and `tests/loot.rs` names the disagreement.

use crate::generated::loot::{CATEGORIES, SOURCE_COUNTS, TABLES, VOCABULARY};

/// Which kind of loot vocabulary a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Condition,
    Function,
    Entry,
}

impl Kind {
    /// The name the generated table spells this kind with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Condition => "condition",
            Self::Function => "function",
            Self::Entry => "entry",
        }
    }
}

/// Whether a loot table with that id exists in the vanilla set.
///
/// The full name is required — `minecraft:blocks/stone`, not
/// `blocks/stone` — because every id in Dust is namespaced and a lookup that
/// accepted two spellings would be two lookups wearing one signature.
pub fn table_exists(id: &str) -> bool {
    TABLES.binary_search(&id).is_ok()
}

/// Every table id, sorted.
pub fn tables() -> impl Iterator<Item = &'static str> {
    TABLES.iter().copied()
}

/// Tables per top-level directory, sorted by directory name.
pub fn categories() -> &'static [(&'static str, u32)] {
    CATEGORIES
}

/// How many times the vanilla tables use a given vocabulary item.
///
/// `None` when the name is unknown *or* belongs to another kind; asking how
/// many times `minecraft:set_count` appears as a condition is a question with
/// no answer rather than one with zero.
pub fn uses(kind: Kind, name: &str) -> Option<u32> {
    let index = VOCABULARY
        .binary_search_by(|(k, n, _)| (*k).cmp(kind.name()).then_with(|| (*n).cmp(name)))
        .ok()?;
    Some(VOCABULARY[index].2)
}

/// The same tally from the structureless second pass, for the checks that
/// compare them.
pub fn source_uses(kind: Kind, name: &str) -> Option<u32> {
    let index = SOURCE_COUNTS
        .binary_search_by(|(k, n, _)| (*k).cmp(kind.name()).then_with(|| (*n).cmp(name)))
        .ok()?;
    Some(SOURCE_COUNTS[index].2)
}

/// Every vocabulary item of one kind, as `(name, uses)`, in name order.
pub fn vocabulary(kind: Kind) -> impl Iterator<Item = (&'static str, u32)> {
    VOCABULARY
        .iter()
        .filter(move |(k, _, _)| *k == kind.name())
        .map(|(_, n, u)| (*n, *u))
}

/// Which loot table each block draws from.
///
/// `Block.getLootTable()` is Java. It is in no `--reports` output and in no
/// data pack, and it is the one thing standing between an operator's own loot
/// files and a server that knows what a broken block yields: 982 of the 1,060
/// blocks on 1.21.1 draw from a table of their own name and 78 do not.
/// `minecraft:bedrock` draws `minecraft:empty`, and about sixty wall forms
/// draw another block's — `minecraft:oak_wall_sign` yields an `oak_sign` out
/// of `blocks/oak_sign.json`.
///
/// **There is no rule about names that gets there.** `oak_wall_sign` drops the
/// `oak_sign` prefix, `oak_wall_hanging_sign` drops a `wall_` from the middle,
/// `dead_tube_coral_wall_fan` swaps `wall_fan` for `fan`, and `potted_cactus`
/// follows none of the three. Decision record 0022 said the fix was one more
/// oracle column rather than a rule, and this is the reader for it.
///
/// The table is `dust-blocks.tsv`, written by `cargo xtask extract --only
/// constants` and copied beside the operator's data. Nothing in it is
/// committed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockLoot {
    /// One entry per distinct table id, sorted by id.
    entries: Box<[Entry]>,
    /// Per block, which entry it draws from.
    by_block: Box<[u32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: Box<str>,
    blocks: Box<[crate::Block]>,
}

impl BlockLoot {
    /// The table `block` draws from, e.g. `minecraft:blocks/stone`.
    ///
    /// Every block has one — a parsed table describes all of them or it was
    /// refused — and `minecraft:empty` is the answer for the ones that yield
    /// nothing. That is a table id and not an absence, which is the whole
    /// distinction this type exists to keep: a block with no *file* is a
    /// question nobody answered, and a block pointed at `minecraft:empty` has
    /// been answered "nothing".
    #[must_use]
    pub fn table_of(&self, block: crate::Block) -> &str {
        &self.entries[self.by_block[block.protocol_id() as usize] as usize].id
    }

    /// Every block that draws from `id`, in block order.
    ///
    /// The direction a loader reads it in: it holds a file and needs to know
    /// which blocks it serves, and for `blocks/oak_sign.json` that is two.
    #[must_use]
    pub fn drawing_from(&self, id: &str) -> &[crate::Block] {
        match self.entries.binary_search_by(|e| (*e.id).cmp(id)) {
            Ok(at) => &self.entries[at].blocks,
            Err(_) => &[],
        }
    }

    /// How many blocks this table describes, which is all of them.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_block.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_block.is_empty()
    }

    /// How many distinct tables are drawn from. 900 on 1.21.1.
    #[must_use]
    pub fn tables(&self) -> usize {
        self.entries.len()
    }

    /// How many blocks draw from a table that is not named after them.
    ///
    /// The number worth printing at boot, because both ends of its range are a
    /// recognisable failure: zero is a table that could have been a rule about
    /// names, and all of them is a column that resolved to something else.
    #[must_use]
    pub fn elsewhere(&self) -> usize {
        crate::Block::all()
            .filter(|block| {
                let name = block.name();
                let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
                self.table_of(*block) != format!("{namespace}:blocks/{path}")
            })
            .count()
    }

    /// Read the table the oracle wrote.
    ///
    /// The first `#` line names the columns; `block` and `loot_table` are
    /// required and `block_id` is checked where it is present. Every block
    /// this build knows appears exactly once or the table is refused, by the
    /// same argument [`crate::BlockConstants::parse`] makes: the failure being
    /// guarded against is not a corrupt file but a file extracted from a
    /// *different version*, where every row parses and every name means
    /// something else.
    ///
    /// # Errors
    ///
    /// [`BlockLootError`], naming the line and what was wrong with it.
    pub fn parse(text: &str) -> Result<Self, BlockLootError> {
        let header = text
            .lines()
            .find(|line| line.starts_with('#'))
            .ok_or(BlockLootError::NoHeader)?;
        let names: Vec<&str> = header
            .trim_start_matches('#')
            .split('\t')
            .map(str::trim)
            .collect();
        let column = |wanted: &str| names.iter().position(|name| *name == wanted);
        let block_at = column("block").ok_or(BlockLootError::MissingColumn { column: "block" })?;
        let table_at = column("loot_table").ok_or(BlockLootError::MissingColumn {
            column: "loot_table",
        })?;
        let id_at = column("block_id");

        let count = crate::Block::all().count();
        let mut drawn: Vec<Option<Box<str>>> = vec![None; count];
        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != names.len() {
                return Err(BlockLootError::Malformed {
                    line: at,
                    detail: format!(
                        "{} field(s) where the header names {}",
                        fields.len(),
                        names.len()
                    ),
                });
            }
            let name = fields[block_at];
            let block =
                crate::Block::from_name(name).ok_or_else(|| BlockLootError::UnknownBlock {
                    line: at,
                    block: name.to_owned(),
                })?;
            // The id is checked and not used. It is a second opinion about a
            // relation this build already holds, and the point of reading it
            // is that a table from another version disagrees here rather than
            // silently describing somebody else's blocks.
            if let Some(id_at) = id_at {
                let stated: u32 = fields[id_at]
                    .parse()
                    .map_err(|_| BlockLootError::Malformed {
                        line: at,
                        detail: format!("block_id is {:?}, which is not a number", fields[id_at]),
                    })?;
                if stated != block.protocol_id() {
                    return Err(BlockLootError::WrongId {
                        line: at,
                        block: name.to_owned(),
                        stated,
                        here: block.protocol_id(),
                    });
                }
            }
            let slot = &mut drawn[block.protocol_id() as usize];
            if slot.is_some() {
                return Err(BlockLootError::DuplicateBlock {
                    line: at,
                    block: name.to_owned(),
                });
            }
            *slot = Some(fields[table_at].into());
        }

        let present = drawn.iter().filter(|d| d.is_some()).count();
        if present != count {
            return Err(BlockLootError::Incomplete {
                present,
                expected: count,
            });
        }

        let mut ids: Vec<&str> = drawn
            .iter()
            .map(|d| d.as_deref().expect("just counted"))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let entries: Vec<Entry> = ids
            .iter()
            .map(|id| Entry {
                id: (*id).into(),
                blocks: crate::Block::all()
                    .filter(|block| drawn[block.protocol_id() as usize].as_deref() == Some(*id))
                    .collect(),
            })
            .collect();
        let by_block: Vec<u32> = drawn
            .iter()
            .map(|d| {
                let id = d.as_deref().expect("just counted");
                entries
                    .binary_search_by(|e| (*e.id).cmp(id))
                    .expect("every id came from this list") as u32
            })
            .collect();
        Ok(Self {
            entries: entries.into_boxed_slice(),
            by_block: by_block.into_boxed_slice(),
        })
    }
}

/// Why a `dust-blocks.tsv` could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockLootError {
    NoHeader,
    MissingColumn {
        column: &'static str,
    },
    Malformed {
        line: usize,
        detail: String,
    },
    UnknownBlock {
        line: usize,
        block: String,
    },
    DuplicateBlock {
        line: usize,
        block: String,
    },
    WrongId {
        line: usize,
        block: String,
        stated: u32,
        here: u32,
    },
    Incomplete {
        present: usize,
        expected: usize,
    },
}

impl std::fmt::Display for BlockLootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHeader => write!(
                f,
                "the table has no `#` header line, so nothing says which column is which"
            ),
            Self::MissingColumn { column } => {
                write!(f, "the header names no `{column}` column")
            }
            Self::Malformed { line, detail } => write!(f, "line {line}: {detail}"),
            Self::UnknownBlock { line, block } => write!(
                f,
                "line {line}: `{block}` is not a block this build knows, so the table \
                 was extracted from a different version of Minecraft"
            ),
            Self::DuplicateBlock { line, block } => {
                write!(f, "line {line}: `{block}` appears twice")
            }
            Self::WrongId {
                line,
                block,
                stated,
                here,
            } => write!(
                f,
                "line {line}: the table numbers `{block}` {stated} and this build \
                 numbers it {here}, so the two are different versions of Minecraft"
            ),
            Self::Incomplete { present, expected } => write!(
                f,
                "the table describes {present} of {expected} blocks; a partial table \
                 would leave the rest drawing from nothing"
            ),
        }
    }
}

impl std::error::Error for BlockLootError {}

#[cfg(test)]
mod block_loot_tests {
    use super::*;
    use crate::Block;

    /// A table where every block draws from its own name, plus whatever
    /// overrides the caller wants. Built rather than pasted: the reader
    /// insists on all 1,060 blocks, and a fixture that listed three would be
    /// testing the refusal rather than the reading.
    fn table(overrides: &[(&str, &str)]) -> String {
        let mut out = String::from("# block_id\tblock\tloot_table\n");
        for block in Block::all() {
            let name = block.name();
            let (namespace, path) = name.split_once(':').expect("namespaced");
            let drawn = overrides.iter().find(|(who, _)| *who == name).map_or_else(
                || format!("{namespace}:blocks/{path}"),
                |(_, to)| (*to).to_owned(),
            );
            out.push_str(&format!("{}\t{name}\t{drawn}\n", block.protocol_id()));
        }
        out
    }

    #[test]
    fn a_wall_sign_can_be_pointed_at_the_sign_it_yields() {
        let loot = BlockLoot::parse(&table(&[(
            "minecraft:oak_wall_sign",
            "minecraft:blocks/oak_sign",
        )]))
        .expect("a complete table");
        assert_eq!(loot.len(), Block::all().count());
        assert_eq!(loot.elsewhere(), 1);
        let sign = Block::from_name("minecraft:oak_sign").expect("a vanilla block");
        let wall = Block::from_name("minecraft:oak_wall_sign").expect("a vanilla block");
        assert_eq!(loot.table_of(wall), "minecraft:blocks/oak_sign");
        assert_eq!(
            loot.drawing_from("minecraft:blocks/oak_sign"),
            &[sign, wall]
        );
    }

    #[test]
    fn a_block_pointed_at_nothing_is_not_a_block_nobody_answered_for() {
        let loot = BlockLoot::parse(&table(&[("minecraft:bedrock", "minecraft:empty")]))
            .expect("a complete table");
        let bedrock = Block::from_name("minecraft:bedrock").expect("a vanilla block");
        assert_eq!(loot.table_of(bedrock), "minecraft:empty");
        assert!(loot.drawing_from("minecraft:blocks/bedrock").is_empty());
    }

    #[test]
    fn a_table_missing_a_block_is_refused() {
        let full = table(&[]);
        let short: String = full
            .lines()
            .filter(|line| !line.ends_with("\tminecraft:stone\tminecraft:blocks/stone"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            BlockLoot::parse(&short),
            Err(BlockLootError::Incomplete { .. })
        ));
    }

    #[test]
    fn a_table_numbering_a_block_differently_is_refused() {
        let full = table(&[]);
        let bent = full.replacen("0\tminecraft:air\t", "9999\tminecraft:air\t", 1);
        assert!(matches!(
            BlockLoot::parse(&bent),
            Err(BlockLootError::WrongId { .. })
        ));
    }

    #[test]
    fn a_table_naming_a_block_this_build_never_heard_of_is_refused() {
        let full = table(&[]);
        let alien = full.replacen("\tminecraft:stone\t", "\tmodded:unobtainium\t", 1);
        assert!(matches!(
            BlockLoot::parse(&alien),
            Err(BlockLootError::UnknownBlock { .. })
        ));
    }
}

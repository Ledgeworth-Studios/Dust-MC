//! Which block each item puts down.
//!
//! `minecraft:stone` the item places `minecraft:stone` the block, and 910 more
//! do the same — which is exactly why this is a table and not a rule.
//! `minecraft:wheat_seeds` places `minecraft:wheat`, `minecraft:redstone`
//! places `minecraft:redstone_wire`, and **`minecraft:wheat` the item places
//! nothing at all**: it is the thing bread is made of, and the crop of the same
//! name is what the seeds put down. A server that matched names would be right
//! about 909 items, wrong about sixteen, and wrong in a direction nobody would
//! think to test.
//!
//! It is `BlockItem.block` in Java — a field, in no report and no data pack —
//! so it arrives the way decision record 0008 already decided this kind of
//! value arrives: asked of the operator's own jar by
//! `cargo xtask extract --only constants`, written to their own disk, and read
//! from `[data] path` at boot. Nothing here is generated and no row of it is
//! committed.
//!
//! # What this is not
//!
//! **A block, not a block state.** A stair placed by a player faces the way
//! they were standing and a slab lands in the half they clicked; that is
//! `getStateForPlacement`, it needs a placement context this table has never
//! seen, and it belongs wherever placement rules end up living. What is here is
//! the block, and [`Block::default_state`] is what a caller with no context
//! can honestly do with it.
//!
//! # The format, and the check it makes possible
//!
//! ```text
//! # item_id | item                  | places
//! 0           minecraft:air           -
//! 1           minecraft:stone         minecraft:stone
//! 853         minecraft:wheat_seeds   minecraft:wheat
//! ```
//!
//! The item's own name is in the file beside its id, and it is not decoration:
//! this reader checks every row's name against the name *this build* gives that
//! id. The light table can only count its rows, so it catches a version with a
//! different number of block states and nothing finer; this catches a version
//! that renumbered a single item, on the row where it happened, by name.

use std::fmt;

use crate::{Block, Item};

/// Which block every item places, indexed by the item's protocol id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemBlocks {
    places: Box<[Option<Block>]>,
}

impl ItemBlocks {
    /// The block `item` puts down, or `None` for an item that places nothing.
    #[must_use]
    pub fn places(&self, item: Item) -> Option<Block> {
        *self.places.get(item.protocol_id() as usize)?
    }

    /// How many items this table describes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.places.len()
    }

    /// Whether it describes none, which a parsed table never does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// How many of them place a block.
    ///
    /// The number worth logging: 925 of 1,333 on 1.21.1, and a table reporting
    /// none is one whose `places` column resolved to nothing anywhere.
    #[must_use]
    pub fn placing(&self) -> usize {
        self.places.iter().filter(|b| b.is_some()).count()
    }

    /// Read a table the oracle wrote.
    ///
    /// The first `#` line is the header and names the columns; blank lines and
    /// any further `#` lines are skipped. `item_id`, `item` and `places` must
    /// all be there. A `places` of `-` is an item that places nothing, which is
    /// most of a table and is not an error.
    ///
    /// # Errors
    ///
    /// [`PlacementError`], naming the line and what was wrong with it. Every
    /// item in this build appears exactly once under the name this build gives
    /// it, or the table is refused — see the module docs for why the name is
    /// worth checking when the light table only counts.
    pub fn parse(text: &str) -> Result<Self, PlacementError> {
        let expected = Item::all().count();
        let header = Header::read(text)?;
        let mut places: Vec<Option<Option<Block>>> = vec![None; expected];

        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != header.width {
                return Err(PlacementError::Malformed {
                    line: at,
                    detail: format!(
                        "{} field(s) where the header names {}",
                        fields.len(),
                        header.width
                    ),
                });
            }

            let id: u32 =
                fields[header.item_id]
                    .parse()
                    .map_err(|_| PlacementError::Malformed {
                        line: at,
                        detail: format!(
                            "item_id is {:?}, which is not a whole number",
                            fields[header.item_id]
                        ),
                    })?;
            let item = Item::from_protocol_id(id).ok_or(PlacementError::UnknownItem {
                line: at,
                id,
                items: expected,
            })?;

            // The row says which item it is about, and this build says which
            // item that id is. Two answers to one question, compared — which
            // is the whole reason the name is in the file.
            let named = fields[header.item];
            if named != item.name() {
                return Err(PlacementError::Renamed {
                    line: at,
                    id,
                    table: named.to_owned(),
                    build: item.name(),
                });
            }

            let places_what =
                match fields[header.places] {
                    NOTHING => None,
                    name => Some(Block::from_name(name).ok_or_else(|| {
                        PlacementError::UnknownBlock {
                            line: at,
                            name: name.to_owned(),
                        }
                    })?),
                };

            let slot = &mut places[id as usize];
            if slot.is_some() {
                return Err(PlacementError::DuplicateItem { line: at, id });
            }
            *slot = Some(places_what);
        }

        let present = places.iter().filter(|p| p.is_some()).count();
        if present != expected {
            return Err(PlacementError::Incomplete { present, expected });
        }
        Ok(Self {
            places: places
                .into_iter()
                .map(|p| p.expect("every slot was just counted as present"))
                .collect(),
        })
    }
}

/// What the `places` column holds for an item that places nothing.
///
/// A dash and not an empty field: an empty last column is a trailing tab, which
/// is invisible in a diff and in an editor, and a row whose last field vanished
/// would read as one that was never written.
const NOTHING: &str = "-";

/// Which column is which, read out of the table's own header.
struct Header {
    width: usize,
    item_id: usize,
    item: usize,
    places: usize,
}

impl Header {
    fn read(text: &str) -> Result<Self, PlacementError> {
        let (at, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.starts_with('#'))
            .ok_or(PlacementError::NoHeader)?;
        let names: Vec<&str> = line
            .trim_start_matches('#')
            .split('\t')
            .map(str::trim)
            .collect();
        let required = |wanted: &'static str| {
            names
                .iter()
                .position(|name| *name == wanted)
                .ok_or(PlacementError::MissingColumn {
                    line: at + 1,
                    column: wanted,
                })
        };
        Ok(Self {
            width: names.len(),
            item_id: required("item_id")?,
            item: required("item")?,
            places: required("places")?,
        })
    }
}

/// Why a placement table could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// No `#` line, so nothing says what the columns are.
    NoHeader,
    /// The header does not name a column the reader needs.
    MissingColumn {
        /// One-based line number of the header.
        line: usize,
        /// The column that is not there.
        column: &'static str,
    },
    /// A line was not the columns the header promised.
    Malformed {
        /// One-based line number in the file.
        line: usize,
        /// What did not read.
        detail: String,
    },
    /// An item id this build has no item for.
    UnknownItem {
        /// One-based line number in the file.
        line: usize,
        /// The id the row was for.
        id: u32,
        /// How many items this build has.
        items: usize,
    },
    /// An id this build gives to a different item than the table does.
    ///
    /// The version-skew case, caught by name on the row where it happened.
    Renamed {
        /// One-based line number in the file.
        line: usize,
        /// The id both sides agree the row is about.
        id: u32,
        /// What the table calls it.
        table: String,
        /// What this build calls it.
        build: &'static str,
    },
    /// A block this build has no entry for.
    UnknownBlock {
        /// One-based line number in the file.
        line: usize,
        /// The name that did not resolve.
        name: String,
    },
    /// One item described twice, which is two answers to one question.
    DuplicateItem {
        /// One-based line number in the file.
        line: usize,
        /// The item described a second time.
        id: u32,
    },
    /// The table stopped short.
    Incomplete {
        /// How many distinct items were described.
        present: usize,
        /// How many this build has.
        expected: usize,
    },
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHeader => write!(
                f,
                "no `#` header line, so nothing says which column is which — \
                 this is not a table the oracle wrote"
            ),
            Self::MissingColumn { line, column } => write!(
                f,
                "line {line}: the header does not name an `{column}` column"
            ),
            Self::Malformed { line, detail } => write!(f, "line {line}: {detail}"),
            Self::UnknownItem { line, id, items } => write!(
                f,
                "line {line}: item {id}, and this build has {items} of them — \
                 the table is from a different Minecraft version"
            ),
            Self::Renamed {
                line,
                id,
                table,
                build,
            } => write!(
                f,
                "line {line}: the table calls item {id} `{table}` and this build \
                 calls it `{build}` — the table is from a different Minecraft version"
            ),
            Self::UnknownBlock { line, name } => write!(
                f,
                "line {line}: no block is called `{name}` in this build's table — \
                 the table is from a different Minecraft version"
            ),
            Self::DuplicateItem { line, id } => {
                write!(f, "line {line}: item {id} is described twice")
            }
            Self::Incomplete { present, expected } => write!(
                f,
                "{present} of {expected} items are described — the table is from \
                 a different Minecraft version, or was truncated"
            ),
        }
    }
}

impl std::error::Error for PlacementError {}

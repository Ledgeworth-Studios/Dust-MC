//! The per-block-state values Minecraft keeps in Java code rather than in data.
//!
//! How much light entering a state costs, how much it gives off, whether it
//! occludes, and which of the six heightmaps count it. None of it is in any
//! `--reports` output or any data pack: it is all code. Decision record 0008 is
//! the account for the light values and 0010 for the heightmap predicates, and
//! `cargo xtask extract --only constants` is the oracle that asks the game —
//! it boots Minecraft's static initialisation against the operator's own jar
//! and reads the answers off the block-state registry.
//!
//! This module is the reader for what that oracle writes. **Nothing here is
//! generated and no value in it is committed**, which is the point D6, D7 and
//! D8 all make: Minecraft's numbers arrive from the operator's copy of the
//! game, not from this repository. What the repository holds is the question
//! and the parser for the answer.
//!
//! # Why this crate
//!
//! `dust-world` walks light across a graph and packs heightmaps, and says in
//! its own documentation that *meaning* — which block state attenuates what,
//! which one a heightmap counts — belongs here. A table keyed by block-state id
//! is meaningless without the thing that says how many block states there are,
//! and that is [`STATE_COUNT`](crate::STATE_COUNT).
//!
//! # The format is header-driven, and that is load bearing
//!
//! ```text
//! # state_id | opacity | emission | occlude | WORLD_SURFACE | MOTION_BLOCKING | …
//! 0            0         0          0         0               0                 air
//! 1            15        0          1         1               1                 stone
//! ```
//!
//! Tab-separated in the file. The first four columns are named values; every
//! other column is a **flag column** holding `0` or `1`, addressed by the name
//! in the header — which for the heightmaps is the same string a chunk's NBT
//! uses and [`HeightmapKind::nbt_key`] returns, so the two sides match on
//! something they each know independently rather than on a position.
//!
//! A reader that took columns by position would silently change meaning the day
//! one was inserted. This one can also answer *which columns a table it has been
//! handed does not have*, which is what lets an older table keep working.
//!
//! [`HeightmapKind::nbt_key`]: https://docs.rs/dust-world
//!
//! # What the reader refuses
//!
//! A table is either complete or refused. Every state in `0..STATE_COUNT`
//! appears exactly once, or [`ConstantsError`] names what was wrong — because
//! the failure this is really guarding against is not a corrupt file. It is a
//! table extracted from **a different version of Minecraft than the generated
//! tables were**, where every row parses, every number is in range, and every
//! state id means a different block. A row count that has to match is the one
//! check that catches it.

use std::fmt;

use crate::STATE_COUNT;

/// One flag column of a constants table, resolved from its name once.
///
/// Held rather than looked up per cell because the callers ask per *block*:
/// recomputing a chunk's heightmaps asks six questions of every state in
/// 98,304 cells, and a string comparison in that loop is a string comparison
/// several million times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag(usize);

/// Everything the oracle read out of Minecraft, for every block state.
///
/// Dense and indexed by state id: the oracle reads Minecraft's own `IdMapper`,
/// so the ids are the ids `dust-registry`'s generated tables are numbered by
/// and there is no name-matching step for a mistake to hide in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockConstants {
    /// How much light is lost entering each state, `0..=15`.
    opacity: Box<[u8]>,
    /// How much light each state gives off, `0..=15`.
    emission: Box<[u8]>,
    /// Whether each state occludes — Minecraft's `canOcclude()`.
    occludes: Box<[bool]>,
    /// The flag columns, in header order.
    flags: Vec<FlagColumn>,
}

/// One named boolean column: what it is called, and one bit per state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FlagColumn {
    name: String,
    set: Box<[bool]>,
}

impl BlockConstants {
    /// How much light is lost entering `state`.
    ///
    /// Out-of-range ids answer 15. A state id this table has never heard of
    /// cannot be described, and the two ways to be wrong about it are to let
    /// light through something that may be a wall or to stop it at something
    /// that may be air. The first is visible as light in a sealed room and the
    /// second as a dark patch, and only the second is the direction every
    /// other known gap already errs in.
    #[must_use]
    pub fn opacity(&self, state: u32) -> u8 {
        self.opacity.get(state as usize).copied().unwrap_or(15)
    }

    /// How much light `state` gives off, `0..=15`. Out-of-range ids emit
    /// nothing, by the same argument as [`BlockConstants::opacity`].
    #[must_use]
    pub fn emission(&self, state: u32) -> u8 {
        self.emission.get(state as usize).copied().unwrap_or(0)
    }

    /// Whether `state` occludes, which is Minecraft's `canOcclude()`.
    ///
    /// **Nothing consumes it**, and the reason is worth keeping: it was carried
    /// on the guess that sky light would want it — a state may cost nothing to
    /// enter and still not let daylight fall straight through it — and the
    /// guess was wrong. `cargo xtask harness light` reaches a hundred per cent
    /// agreement with Minecraft's own light without it. What sky light actually
    /// wanted from this direction was the `MOTION_BLOCKING` predicate, which is
    /// a different question off the same object.
    #[must_use]
    pub fn occludes(&self, state: u32) -> bool {
        self.occludes.get(state as usize).copied().unwrap_or(true)
    }

    /// The flag column called `name`, if this table carries one.
    ///
    /// `None` is not an error. A table written before a column existed is
    /// still a table, and a caller that asks for a column it does not get falls
    /// back to whatever it did before — which is how a server keeps running on
    /// a file an operator extracted a version ago.
    #[must_use]
    pub fn flag(&self, name: &str) -> Option<Flag> {
        self.flags
            .iter()
            .position(|column| column.name == name)
            .map(Flag)
    }

    /// Whether `state` is set in `flag`.
    ///
    /// # Panics
    ///
    /// Never for a [`Flag`] this table produced; a `Flag` from a *different*
    /// table is a programming error and would answer about the wrong column,
    /// which is why it panics rather than returning `false`.
    #[must_use]
    pub fn is_set(&self, flag: Flag, state: u32) -> bool {
        self.flags[flag.0]
            .set
            .get(state as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The names of the flag columns this table carries, in header order.
    pub fn flags(&self) -> impl Iterator<Item = &str> {
        self.flags.iter().map(|column| column.name.as_str())
    }

    /// How many states this table describes. Equal to
    /// [`STATE_COUNT`](crate::STATE_COUNT) or the table would not have parsed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.opacity.len()
    }

    /// Whether the table describes nothing, which a parsed table never does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.opacity.is_empty()
    }

    /// How many states emit any light at all.
    #[must_use]
    pub fn emitting(&self) -> usize {
        self.emission.iter().filter(|e| **e > 0).count()
    }

    /// Read a table the oracle wrote.
    ///
    /// The first `#` line is the header and names the columns; blank lines and
    /// any further `#` lines are skipped. `state_id`, `opacity` and `emission`
    /// must be there; `occlude` is optional and defaults to occluding, which is
    /// what the engine assumed before the column existed; every other column is
    /// a flag holding `0` or `1`.
    ///
    /// # Errors
    ///
    /// [`ConstantsError`], which names the line and what was wrong with it.
    pub fn parse(text: &str) -> Result<Self, ConstantsError> {
        let expected = STATE_COUNT as usize;
        let header = Header::read(text)?;

        let mut opacity = vec![None; expected];
        let mut emission = vec![0u8; expected];
        let mut occludes = vec![true; expected];
        let mut flags: Vec<FlagColumn> = header
            .flags
            .iter()
            .map(|(name, _)| FlagColumn {
                name: name.clone(),
                set: vec![false; expected].into_boxed_slice(),
            })
            .collect();

        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != header.width {
                return Err(ConstantsError::Malformed {
                    line: at,
                    detail: format!(
                        "{} field(s) where the header names {}",
                        fields.len(),
                        header.width
                    ),
                });
            }
            let cell = |column: usize| fields[column];

            let state = number(at, "state_id", cell(header.state_id))?;
            let op = level(at, "opacity", number(at, "opacity", cell(header.opacity))?)?;
            let em = level(
                at,
                "emission",
                number(at, "emission", cell(header.emission))?,
            )?;
            let occlude = match header.occlude {
                None => true,
                Some(column) => boolean(at, "occlude", cell(column))?,
            };

            let slot = opacity
                .get_mut(state as usize)
                .ok_or(ConstantsError::UnknownState {
                    line: at,
                    state,
                    states: STATE_COUNT,
                })?;
            if slot.is_some() {
                return Err(ConstantsError::DuplicateState { line: at, state });
            }
            *slot = Some(op);
            emission[state as usize] = em;
            occludes[state as usize] = occlude;
            for (flag, (name, column)) in flags.iter_mut().zip(&header.flags) {
                flag.set[state as usize] = boolean_named(at, name, cell(*column))?;
            }
        }

        let present = opacity.iter().filter(|o| o.is_some()).count();
        if present != expected {
            return Err(ConstantsError::Incomplete { present, expected });
        }

        Ok(Self {
            opacity: opacity
                .into_iter()
                .map(|o| o.expect("every slot was just counted as present"))
                .collect(),
            emission: emission.into_boxed_slice(),
            occludes: occludes.into_boxed_slice(),
            flags,
        })
    }
}

/// Which column is which, read out of the table's own header.
struct Header {
    width: usize,
    state_id: usize,
    opacity: usize,
    emission: usize,
    occlude: Option<usize>,
    /// Every other column, by name and position.
    flags: Vec<(String, usize)>,
}

impl Header {
    /// Read the first `#` line as the column names.
    fn read(text: &str) -> Result<Self, ConstantsError> {
        let (at, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.starts_with('#'))
            .ok_or(ConstantsError::NoHeader)?;
        let names: Vec<String> = line
            .trim_start_matches('#')
            .split('\t')
            .map(|name| name.trim().to_owned())
            .collect();
        let column = |wanted: &str| names.iter().position(|name| name == wanted);
        let required = |wanted: &'static str| {
            column(wanted).ok_or(ConstantsError::MissingColumn {
                line: at + 1,
                column: wanted,
            })
        };
        let state_id = required("state_id")?;
        let opacity = required("opacity")?;
        let emission = required("emission")?;
        let occlude = column("occlude");
        let flags = names
            .iter()
            .enumerate()
            .filter(|(at, _)| {
                *at != state_id && *at != opacity && *at != emission && Some(*at) != occlude
            })
            .map(|(at, name)| (name.clone(), at))
            .collect();
        Ok(Self {
            width: names.len(),
            state_id,
            opacity,
            emission,
            occlude,
            flags,
        })
    }
}

/// Parse one unsigned field, naming it if it is not a number.
fn number(line: usize, field: &'static str, text: &str) -> Result<u32, ConstantsError> {
    text.parse().map_err(|_| ConstantsError::Malformed {
        line,
        detail: format!("{field} is {text:?}, which is not a whole number"),
    })
}

/// Refuse a light level outside `0..=15`.
///
/// The failure this catches is not a typo in a file nobody edits. It is the
/// oracle resolving to the wrong Java member — two members of one class share
/// an obfuscated letter, and reading the wrong one produces a table full of
/// plausible integers that are something else entirely. Every light level
/// Minecraft has fits in a nibble, so anything larger did not come from the
/// field this asked for.
fn level(line: usize, field: &'static str, value: u32) -> Result<u8, ConstantsError> {
    u8::try_from(value)
        .ok()
        .filter(|v| *v <= 15)
        .ok_or(ConstantsError::OutOfRange { line, field, value })
}

/// A flag is `0` or `1` and nothing else.
fn boolean(line: usize, field: &str, text: &str) -> Result<bool, ConstantsError> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(ConstantsError::Malformed {
            line,
            detail: format!("{field} is {other:?}, and it is 0 or 1"),
        }),
    }
}

/// [`boolean`], for a column whose name came out of the file.
fn boolean_named(line: usize, field: &str, text: &str) -> Result<bool, ConstantsError> {
    boolean(line, field, text)
}

/// Why a constants table could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstantsError {
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
    /// A light level outside `0..=15`. See [`level`] for why this is a check
    /// worth having rather than paranoia about a file format.
    OutOfRange {
        /// One-based line number in the file.
        line: usize,
        /// Which of the two numbers it was.
        field: &'static str,
        /// What was written there.
        value: u32,
    },
    /// One state described twice, which is two answers to one question.
    DuplicateState {
        /// One-based line number in the file.
        line: usize,
        /// The state described a second time.
        state: u32,
    },
    /// A state id this build has no block for — the table was extracted from
    /// a different version of Minecraft than the generated tables.
    UnknownState {
        /// One-based line number in the file.
        line: usize,
        /// The id the row was for.
        state: u32,
        /// How many states this build has.
        states: u32,
    },
    /// The table stopped short. Same cause as [`ConstantsError::UnknownState`]
    /// seen from the other end: a version with fewer states than this one.
    Incomplete {
        /// How many distinct states were described.
        present: usize,
        /// How many this build has.
        expected: usize,
    },
}

impl fmt::Display for ConstantsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHeader => write!(
                f,
                "no `#` header line, so nothing says which column is which — \
                 this is not a table the oracle wrote"
            ),
            Self::MissingColumn { line, column } => write!(
                f,
                "line {line}: the header does not name a `{column}` column"
            ),
            Self::Malformed { line, detail } => write!(f, "line {line}: {detail}"),
            Self::OutOfRange { line, field, value } => write!(
                f,
                "line {line}: {field} is {value}, and a light level is 0..=15 — \
                 the oracle read the wrong member for this version"
            ),
            Self::DuplicateState { line, state } => {
                write!(f, "line {line}: block state {state} is described twice")
            }
            Self::UnknownState {
                line,
                state,
                states,
            } => write!(
                f,
                "line {line}: block state {state}, and this build has {states} \
                 of them — the table is from a different Minecraft version"
            ),
            Self::Incomplete { present, expected } => write!(
                f,
                "{present} of {expected} block states are described — the table \
                 is from a different Minecraft version, or was truncated"
            ),
        }
    }
}

impl std::error::Error for ConstantsError {}

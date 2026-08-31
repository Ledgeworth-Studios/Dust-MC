//! What entering a block state costs, and what it gives off.
//!
//! Two numbers per state, and neither is in any `--reports` output or any data
//! pack: Minecraft keeps both as Java code. Decision record 0008 is the whole
//! of that problem, and `cargo xtask extract --only light` is the oracle that
//! asks the game itself — it boots Minecraft's static initialisation against
//! the operator's own jar and reads `getLightBlock` and `lightEmission` off
//! every state in the block-state registry.
//!
//! This module is the reader for what that oracle writes, and the table the
//! light engine consults. **Nothing here is generated and no value in it is
//! committed**, which is the point D6, D7 and D8 all make: Minecraft's numbers
//! arrive from the operator's copy of the game, not from this repository. What
//! the repository holds is the question and the parser for the answer.
//!
//! # Why this crate
//!
//! `dust-world` walks light across a graph and says in its own documentation
//! that *meaning* — which block state attenuates what — belongs here. A table
//! keyed by block-state id is meaningless without the thing that says how many
//! block states there are and what order they are in, and that is
//! [`STATE_COUNT`](crate::STATE_COUNT), which lives in this crate.
//!
//! # What the reader refuses
//!
//! A table is either complete or refused. Every state in `0..STATE_COUNT`
//! appears exactly once, or [`LightTableError`] names what was wrong — because
//! the failure this is really guarding against is not a corrupt file. It is a
//! table extracted from **a different version of Minecraft than the generated
//! tables were**, where every row parses, every number is in range, and every
//! state id means a different block than it does here. A row count that has to
//! match is the one check that catches it.
//!
//! ```text
//! # state_id | opacity | emission | occlude      (tab-separated in the file)
//! 0           0         0          0             air
//! 1           15        0          1             stone
//! ```

use std::fmt;

use crate::STATE_COUNT;

/// The three light constants for every block state, as the oracle read them
/// out of Minecraft.
///
/// Dense and indexed by state id: the oracle reads Minecraft's own
/// `IdMapper`, so the ids are the ids `dust-registry`'s generated tables are
/// numbered by and there is no name-matching step for a mistake to hide in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightTable {
    /// How much light is lost entering each state, `0..=15`.
    opacity: Box<[u8]>,
    /// How much light each state gives off, `0..=15`.
    emission: Box<[u8]>,
    /// Whether each state occludes — Minecraft's `canOcclude()`.
    occludes: Box<[bool]>,
}

impl LightTable {
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
    /// nothing, by the same argument as [`LightTable::opacity`].
    #[must_use]
    pub fn emission(&self, state: u32) -> u8 {
        self.emission.get(state as usize).copied().unwrap_or(0)
    }

    /// Whether `state` occludes, which is Minecraft's `canOcclude()`.
    ///
    /// Read by the oracle and carried here because it is the third constant of
    /// the same kind and comes off the same object in the same pass. Sky light
    /// wants it: a state may cost nothing to enter and still not let daylight
    /// fall straight through it.
    #[must_use]
    pub fn occludes(&self, state: u32) -> bool {
        self.occludes.get(state as usize).copied().unwrap_or(true)
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

    /// Read a table the light oracle wrote.
    ///
    /// Blank lines and lines opening with `#` are skipped; every other line is
    /// a state id and its numbers, tab-separated. A fourth column carries
    /// `canOcclude()` as `0` or `1`; a table written before that column existed
    /// is read without it and every state is taken to occlude, which is what
    /// the engine already assumed.
    ///
    /// # Errors
    ///
    /// [`LightTableError`], which names the line and what was wrong with it.
    pub fn parse(text: &str) -> Result<Self, LightTableError> {
        let expected = STATE_COUNT as usize;
        let mut opacity = vec![None; expected];
        let mut emission = vec![0u8; expected];
        let mut occludes = vec![true; expected];

        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = index + 1;
            let mut fields = line.split('\t');
            let (Some(state), Some(op), Some(em)) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(LightTableError::Malformed {
                    line: at,
                    detail: "expected at least three tab-separated fields".to_owned(),
                });
            };
            let state = number(at, "state_id", state)?;
            let op = level(at, "opacity", number(at, "opacity", op)?)?;
            let em = level(at, "emission", number(at, "emission", em)?)?;
            let occlude = match fields.next() {
                None => true,
                Some("0") => false,
                Some("1") => true,
                Some(other) => {
                    return Err(LightTableError::Malformed {
                        line: at,
                        detail: format!("occlude is {other:?}, and it is 0 or 1"),
                    })
                }
            };

            let slot = opacity
                .get_mut(state as usize)
                .ok_or(LightTableError::UnknownState {
                    line: at,
                    state,
                    states: STATE_COUNT,
                })?;
            if slot.is_some() {
                return Err(LightTableError::DuplicateState { line: at, state });
            }
            *slot = Some(op);
            emission[state as usize] = em;
            occludes[state as usize] = occlude;
        }

        let present = opacity.iter().filter(|o| o.is_some()).count();
        if present != expected {
            return Err(LightTableError::Incomplete { present, expected });
        }

        Ok(Self {
            opacity: opacity
                .into_iter()
                .map(|o| o.expect("every slot was just counted as present"))
                .collect(),
            emission: emission.into_boxed_slice(),
            occludes: occludes.into_boxed_slice(),
        })
    }
}

/// Parse one unsigned field, naming it if it is not a number.
fn number(line: usize, field: &'static str, text: &str) -> Result<u32, LightTableError> {
    text.parse().map_err(|_| LightTableError::Malformed {
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
fn level(line: usize, field: &'static str, value: u32) -> Result<u8, LightTableError> {
    u8::try_from(value)
        .ok()
        .filter(|v| *v <= 15)
        .ok_or(LightTableError::OutOfRange { line, field, value })
}

/// Why a light table could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightTableError {
    /// A line was not a state id and its numbers.
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
    /// The table stopped short. Same cause as [`LightTableError::UnknownState`]
    /// seen from the other end: a version with fewer states than this one.
    Incomplete {
        /// How many distinct states were described.
        present: usize,
        /// How many this build has.
        expected: usize,
    },
}

impl fmt::Display for LightTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

impl std::error::Error for LightTableError {}

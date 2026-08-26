//! The four palette strategies a paletted container can be in.
//!
//! A palette is an indirection: the packed storage holds small indices, and the
//! palette turns an index into a registry id. Which palette a container uses is
//! decided entirely by how many distinct values it currently holds, and it
//! changes underneath the container as blocks are placed. The four are, in the
//! order a container passes through them:
//!
//! | Strategy | Holds | Storage width | Lookup |
//! |---|---|---|---|
//! | [`Single`] | one value | 0 bits | trivial |
//! | [`Linear`] | up to `1 << bits` | `bits` | a scan |
//! | [`Hashed`] | up to `1 << bits` | `bits` | a map |
//! | [`Global`] | the whole registry | `ceil_log2(registry)` | none: index *is* the id |
//!
//! The thresholds between them are per-container-kind and live on
//! [`Strategy`](crate::container::Strategy), not here, because they are a
//! property of what is being stored rather than of the palettes themselves.
//!
//! # Why linear and hashed both exist
//!
//! They hold the same thing and differ only in how `value -> index` is
//! answered. A scan of up to sixteen `u32`s beats a hash map on every measure
//! including cache behaviour; a scan of up to 256 does not. Vanilla draws the
//! line by bit width, and Dust draws it in the same place — not because the
//! crossover is exactly there, but because the palette a container is in is
//! visible in the chunk format, and a container that switched at a different
//! size would write files a vanilla client and server read differently.
//!
//! **What this does not catch:** nothing here checks that a value is a real
//! registry id. [`Global`] knows the registry's *size* and rejects an index
//! past it, but a wrong id inside the range is stored and returned faithfully.

use std::collections::HashMap;

/// The smallest `b` with `2^b >= n`. Vanilla's `Mth.ceillog2`.
///
/// This is the function that turns "the palette now holds n values" into "the
/// storage needs b bits", so its behaviour at the boundaries is the whole
/// promotion schedule: `ceil_log2(16) == 4` and `ceil_log2(17) == 5`, which is
/// why a seventeenth block state is what tips a section out of a linear
/// palette and not a sixteenth.
#[must_use]
pub const fn ceil_log2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        u32::BITS - (n - 1).leading_zeros()
    }
}

/// Which of the four a palette is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteKind {
    Single,
    Linear,
    Hashed,
    Global,
}

impl std::fmt::Display for PaletteKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Single => "single-valued",
            Self::Linear => "linear",
            Self::Hashed => "hashed",
            Self::Global => "global",
        })
    }
}

/// One value for the whole container, and no packed storage at all.
///
/// The overwhelmingly common section: air, or stone, sixteen cubed. Storing it
/// costs one `u32` and no long array, which is why the format has this case at
/// all rather than treating it as a linear palette of length one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Single {
    value: u32,
}

/// A short array of values, searched linearly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linear {
    entries: Vec<u32>,
    bits: u32,
}

/// The same array with a map from value back to index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashed {
    entries: Vec<u32>,
    lookup: HashMap<u32, u32>,
    bits: u32,
}

/// No palette: the packed index is the registry id.
///
/// `size` is how many ids the registry has, which fixes the storage width. It
/// is supplied by the caller rather than read from `dust-registry` so that this
/// crate does not depend on the block table — the same seam the heightmap
/// predicates use, and for the same reason: the container is arithmetic over
/// integers and knows nothing about blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Global {
    size: u32,
    bits: u32,
}

/// A palette in whichever strategy it is currently in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Palette {
    Single(Single),
    Linear(Linear),
    Hashed(Hashed),
    Global(Global),
}

impl Palette {
    /// A single-valued palette holding `value`.
    #[must_use]
    pub const fn single(value: u32) -> Self {
        Self::Single(Single { value })
    }

    /// An empty linear palette whose storage is `bits` wide.
    #[must_use]
    pub fn linear(bits: u32) -> Self {
        Self::Linear(Linear {
            entries: Vec::with_capacity(capacity_at(bits).min(64)),
            bits,
        })
    }

    /// An empty hashed palette whose storage is `bits` wide.
    #[must_use]
    pub fn hashed(bits: u32) -> Self {
        Self::Hashed(Hashed {
            entries: Vec::new(),
            lookup: HashMap::new(),
            bits,
        })
    }

    /// The direct palette over a registry of `size` ids.
    #[must_use]
    pub fn global(size: u32) -> Self {
        Self::Global(Global {
            size,
            bits: ceil_log2(size),
        })
    }

    #[must_use]
    pub const fn kind(&self) -> PaletteKind {
        match self {
            Self::Single(_) => PaletteKind::Single,
            Self::Linear(_) => PaletteKind::Linear,
            Self::Hashed(_) => PaletteKind::Hashed,
            Self::Global(_) => PaletteKind::Global,
        }
    }

    /// The width the packed storage must use with this palette.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        match self {
            Self::Single(_) => 0,
            Self::Linear(p) => p.bits,
            Self::Hashed(p) => p.bits,
            Self::Global(p) => p.bits,
        }
    }

    /// How many values the palette currently maps.
    ///
    /// For [`Global`] this is the registry size: every id is already in it.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Linear(p) => p.entries.len(),
            Self::Hashed(p) => p.entries.len(),
            Self::Global(p) => p.size as usize,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many values this palette could hold before it has to be replaced.
    #[must_use]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Linear(p) => capacity_at(p.bits),
            Self::Hashed(p) => capacity_at(p.bits),
            Self::Global(p) => p.size as usize,
        }
    }

    /// The registry id an index maps to, or `None` if the index is not in the
    /// palette.
    #[must_use]
    pub fn value(&self, index: u32) -> Option<u32> {
        match self {
            Self::Single(p) => (index == 0).then_some(p.value),
            Self::Linear(p) => p.entries.get(index as usize).copied(),
            Self::Hashed(p) => p.entries.get(index as usize).copied(),
            Self::Global(p) => (index < p.size).then_some(index),
        }
    }

    /// The index a value already has, without adding it.
    #[must_use]
    pub fn index_of(&self, value: u32) -> Option<u32> {
        match self {
            Self::Single(p) => (p.value == value).then_some(0),
            // A scan. `position` over at most `1 << bits` entries, and the tier
            // exists precisely because that number is small.
            Self::Linear(p) => p.entries.iter().position(|v| *v == value).map(|i| i as u32),
            Self::Hashed(p) => p.lookup.get(&value).copied(),
            Self::Global(p) => (value < p.size).then_some(value),
        }
    }

    /// Add a value and return its index, or `None` if there is no room.
    ///
    /// `None` is not a failure. It is the signal the container promotes on, and
    /// the caller is expected to build the next palette up and try again. A
    /// [`Global`] palette returns `None` only for an id the registry does not
    /// have, which is a different thing and is why the container turns that one
    /// into a named error rather than a promotion.
    pub fn try_insert(&mut self, value: u32) -> Option<u32> {
        match self {
            Self::Single(p) => (p.value == value).then_some(0),
            Self::Linear(p) => {
                if let Some(index) = p.entries.iter().position(|v| *v == value) {
                    return Some(index as u32);
                }
                if p.entries.len() >= capacity_at(p.bits) {
                    return None;
                }
                p.entries.push(value);
                Some((p.entries.len() - 1) as u32)
            }
            Self::Hashed(p) => {
                if let Some(index) = p.lookup.get(&value) {
                    return Some(*index);
                }
                if p.entries.len() >= capacity_at(p.bits) {
                    return None;
                }
                let index = p.entries.len() as u32;
                p.entries.push(value);
                p.lookup.insert(value, index);
                Some(index)
            }
            Self::Global(p) => (value < p.size).then_some(value),
        }
    }

    /// The entries in index order, or `None` for [`Global`].
    ///
    /// `None` rather than `0..size` because the distinction is the point: a
    /// global palette has no entry list, which is exactly what makes it worth
    /// switching to once the list would be longer than the data it indexes.
    #[must_use]
    pub fn entries(&self) -> Option<&[u32]> {
        match self {
            Self::Single(p) => Some(std::slice::from_ref(&p.value)),
            Self::Linear(p) => Some(&p.entries),
            Self::Hashed(p) => Some(&p.entries),
            Self::Global(_) => None,
        }
    }
}

/// How many distinct values a palette of this storage width can index.
const fn capacity_at(bits: u32) -> usize {
    if bits >= usize::BITS {
        usize::MAX
    } else {
        1usize << bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceil_log2_is_the_width_a_palette_of_that_many_entries_needs() {
        // The boundaries are the promotion schedule, so they are spelled out
        // rather than checked against a formula that could be the same mistake
        // written twice.
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
        assert_eq!(ceil_log2(16), 4);
        assert_eq!(ceil_log2(17), 5);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
        assert_eq!(ceil_log2(26_684), 15);

        // And the property, over a range that crosses every power of two that
        // a palette reaches.
        for n in 1..=4096u32 {
            let bits = ceil_log2(n);
            assert!(1u64 << bits >= u64::from(n), "{n} does not fit in {bits}");
            if bits > 0 {
                assert!(
                    1u64 << (bits - 1) < u64::from(n),
                    "{n} fits in {} bits",
                    bits - 1
                );
            }
        }
    }

    #[test]
    fn a_single_palette_holds_one_value_and_refuses_a_second() {
        let mut palette = Palette::single(7);
        assert_eq!(palette.kind(), PaletteKind::Single);
        assert_eq!(palette.bits(), 0);
        assert_eq!(palette.len(), 1);
        assert_eq!(palette.value(0), Some(7));
        assert_eq!(palette.value(1), None);
        assert_eq!(palette.index_of(7), Some(0));
        assert_eq!(palette.try_insert(7), Some(0));
        assert_eq!(palette.try_insert(8), None, "the container must promote");
    }

    #[test]
    fn linear_and_hashed_fill_to_capacity_then_signal() {
        for mut palette in [Palette::linear(4), Palette::hashed(4)] {
            let kind = palette.kind();
            assert_eq!(palette.capacity(), 16, "{kind}");
            for n in 0..16u32 {
                assert_eq!(palette.try_insert(n * 3), Some(n), "{kind} entry {n}");
            }
            assert_eq!(palette.len(), 16);
            // A value already present is found, full or not.
            assert_eq!(palette.try_insert(0), Some(0), "{kind}");
            assert_eq!(palette.try_insert(1), None, "{kind} is full");
            for n in 0..16u32 {
                assert_eq!(palette.value(n), Some(n * 3), "{kind}");
                assert_eq!(palette.index_of(n * 3), Some(n), "{kind}");
            }
            assert_eq!(palette.entries().map(<[u32]>::len), Some(16), "{kind}");
        }
    }

    #[test]
    fn a_global_palette_is_the_registry_and_has_no_entry_list() {
        let mut palette = Palette::global(26_684);
        assert_eq!(palette.kind(), PaletteKind::Global);
        assert_eq!(palette.bits(), 15);
        assert_eq!(palette.value(9), Some(9), "the index is the id");
        assert_eq!(palette.index_of(9), Some(9));
        assert_eq!(palette.try_insert(26_683), Some(26_683));
        assert_eq!(palette.value(26_684), None, "past the registry");
        assert_eq!(palette.try_insert(26_684), None);
        assert_eq!(
            palette.entries(),
            None,
            "a global palette has no list, which is the point of it"
        );
    }
}

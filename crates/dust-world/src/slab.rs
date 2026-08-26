//! A generational slot array: stable identities over storage that moves.
//!
//! A chunk's block entities outlive the structures that point at them. An
//! engine holding "the chest at slot 3" cannot be told mid-step that slot 3
//! now means something else, and it cannot hold a `&mut` either — records
//! move when the array grows and vanish when they are removed. What it can
//! hold is a *key*: the slot's index plus the generation stamped on that
//! slot's occupant. Every removal retires the slot's generation, so a key
//! into a slot whose occupant left names nothing, loudly, instead of
//! silently reading whoever moved in afterwards.
//!
//! # Why the free list is ordered, and why iteration is slot order
//!
//! Reuse always takes the lowest vacant slot. Any policy is correct; this
//! one makes the slot sequence a function of the *live set* rather than of
//! the interleaving of inserts and removes that produced it, which keeps two
//! runs over the same edit schedule interchangeable in tests and replays.
//! Iteration visits slots in ascending index and therefore skips vacancies
//! without reordering anything: the same schedule of edits yields the same
//! visit order every time, whatever a hash map would have done with the
//! same contents.
//!
//! **What this does not catch:** a key forged for the wrong slab. Keys carry
//! no slab identity, so passing one to another arena of the right type is a
//! caller bug here, not a reportable condition — the same trust a `Vec`
//! index gets, with generations catching the one mistake indexing alone
//! cannot see.

/// Where a value lives, and which occupancy it belongs to.
///
/// Keys are plain numbers so engines can copy them freely; the slab they
/// name is fixed by context. [`SlabKey::from_raw`] exists for keys that
/// crossed a boundary — the network, a log line — and arrive untrusted;
/// validating one costs the same lookup as using it and reports the same
/// errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlabKey {
    slot: u32,
    generation: u32,
}

impl SlabKey {
    /// Assemble a key from parts that did not come from the slab itself.
    ///
    /// Nothing validates them here — a key is meaningful only against a
    /// slab, and using a forged one produces the same typed errors any other
    /// dead key would.
    #[must_use]
    pub const fn from_raw(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }

    /// The slot this key names.
    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// The occupancy this key expects to find there.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Why a key does not name a live value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlabError {
    /// The slot exists but its current occupant is not the one the key
    /// named: the value the key held was removed (retiring the generation),
    /// or the slot was freed and filled again since. Either way the key is
    /// dead, and the current generation says how far the slot has moved on.
    StaleGeneration {
        slot: u32,
        generation: u32,
        current_generation: u32,
    },
    /// No such slot in this slab. A key past the end never referred to
    /// anything here.
    Unallocated { slot: u32 },
}

impl std::fmt::Display for SlabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleGeneration {
                slot,
                generation,
                current_generation,
            } => write!(
                f,
                "the key names slot {slot} at generation {generation}, but that slot has \
                 moved on to generation {current_generation}; whatever the key pointed at \
                 is gone"
            ),
            Self::Unallocated { slot } => write!(
                f,
                "the key names slot {slot}, and this slab has never had a slot {slot}"
            ),
        }
    }
}

impl std::error::Error for SlabError {}

#[derive(Debug, Clone)]
struct Slot<T> {
    /// Counts retirements: zero until this slot's first occupant leaves, one
    /// more at every removal after. A key issued at insert time names the
    /// number current then; every later retirement moves the slot past it.
    generation: u32,
    value: Option<T>,
}

/// Storage where removal is cheap, identities survive reshuffling, and dead
/// identities are detectable.
///
/// See the module documentation for why the keys are generational and the
/// reuse policy is what it is.
#[derive(Debug, Clone)]
pub struct Slab<T> {
    slots: Vec<Slot<T>>,
    /// Vacant slot indices, kept sorted; reuse pops the front.
    free: Vec<u32>,
    occupied: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slab<T> {
    /// An empty slab.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            occupied: 0,
        }
    }

    /// How many values are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.occupied
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.occupied == 0
    }

    /// Store a value and hand back its key.
    pub fn insert(&mut self, value: T) -> SlabKey {
        self.occupied += 1;
        match self.free.first().copied() {
            Some(slot) => {
                // Lowest vacant slot first: the slot sequence tracks the live
                // set, not the shape of the history that produced it. The new
                // occupant keeps the generation the removal stamped -- a
                // number no outstanding key holds, because keys are issued
                // only at insert and every older one names a smaller one.
                let entry = &mut self.slots[slot as usize];
                debug_assert!(entry.value.is_none(), "a listed free slot held a value");
                entry.value = Some(value);
                self.free.remove(0);
                SlabKey {
                    slot,
                    generation: entry.generation,
                }
            }
            None => {
                let slot = self.slots.len() as u32;
                self.slots.push(Slot {
                    generation: 0,
                    value: Some(value),
                });
                SlabKey {
                    slot,
                    generation: 0,
                }
            }
        }
    }

    /// The value a live key names.
    pub fn get(&self, key: SlabKey) -> Result<&T, SlabError> {
        let entry = self.slot_for(key)?;
        entry.value.as_ref().ok_or(SlabError::StaleGeneration {
            slot: key.slot,
            generation: key.generation,
            current_generation: entry.generation,
        })
    }

    /// The value a live key names, for writing.
    pub fn get_mut(&mut self, key: SlabKey) -> Result<&mut T, SlabError> {
        let Some(entry) = self.slots.get_mut(key.slot as usize) else {
            return Err(SlabError::Unallocated { slot: key.slot });
        };
        if entry.generation != key.generation {
            return Err(SlabError::StaleGeneration {
                slot: key.slot,
                generation: key.generation,
                current_generation: entry.generation,
            });
        }
        entry.value.as_mut().ok_or(SlabError::StaleGeneration {
            slot: key.slot,
            generation: key.generation,
            current_generation: entry.generation,
        })
    }

    /// Whether the key names a value that is still there.
    #[must_use]
    pub fn contains(&self, key: SlabKey) -> bool {
        self.get(key).is_ok()
    }

    /// Take the value away, retiring its key.
    ///
    /// The slot survives to be reused; its generation moves on first, so the
    /// key just used is dead the moment this returns and any other holder of
    /// it finds out through [`SlabError::StaleGeneration`] rather than by
    /// getting somebody else's value.
    pub fn remove(&mut self, key: SlabKey) -> Result<T, SlabError> {
        let Some(entry) = self.slots.get_mut(key.slot as usize) else {
            return Err(SlabError::Unallocated { slot: key.slot });
        };
        if entry.generation != key.generation {
            return Err(SlabError::StaleGeneration {
                slot: key.slot,
                generation: key.generation,
                current_generation: entry.generation,
            });
        }
        let taken = entry.value.take().ok_or(SlabError::StaleGeneration {
            slot: key.slot,
            generation: key.generation,
            current_generation: entry.generation,
        })?;
        entry.generation = entry.generation.wrapping_add(1);
        let index = key.slot;
        // Vacancy is never listed twice: this slot was occupied a line ago,
        // so it cannot already be on the free list. A search that *finds* it
        // would mean exactly that bookkeeping hole; a miss returns the slot's
        // sorted place, which is where it goes.
        let position = match self.free.binary_search(&index) {
            Ok(listed) => {
                panic!("slot {listed} was freed while still listed vacant")
            }
            Err(position) => position,
        };
        self.free.insert(position, index);
        self.occupied -= 1;
        Ok(taken)
    }

    /// Every live value with its key, in ascending slot order.
    ///
    /// The order is a property of the schedule of edits, not of hashing or
    /// timing: identical schedules produce identical sequences.
    pub fn iter(&self) -> impl Iterator<Item = (SlabKey, &T)> {
        self.slots.iter().enumerate().filter_map(|(slot, entry)| {
            let value = entry.value.as_ref()?;
            Some((
                SlabKey {
                    slot: slot as u32,
                    generation: entry.generation,
                },
                value,
            ))
        })
    }

    /// Every live value, in ascending slot order.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|entry| entry.value.as_ref())
    }

    /// Whether two slabs hold exactly the same values, wherever each happens
    /// to store them.
    ///
    /// Slots and generations are bookkeeping; equality of contents is not.
    /// The comparison is quadratic in the live count, which is the right
    /// trade for collections the size a chunk's block entities reach.
    #[must_use]
    pub fn equivalent(&self, other: &Self) -> bool
    where
        T: PartialEq,
    {
        self.occupied == other.occupied
            && self
                .values()
                .all(|mine| other.values().any(|theirs| mine == theirs))
    }

    fn slot_for(&self, key: SlabKey) -> Result<&Slot<T>, SlabError> {
        let Some(entry) = self.slots.get(key.slot as usize) else {
            return Err(SlabError::Unallocated { slot: key.slot });
        };
        if entry.generation != key.generation {
            return Err(SlabError::StaleGeneration {
                slot: key.slot,
                generation: key.generation,
                current_generation: entry.generation,
            });
        }
        Ok(entry)
    }
}

impl<T: PartialEq> PartialEq for Slab<T> {
    fn eq(&self, other: &Self) -> bool {
        self.equivalent(other)
    }
}

impl<T: PartialEq> Eq for Slab<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_survive_growth_and_name_the_same_value_through_it() {
        let mut slab = Slab::new();
        let first = slab.insert("first");
        let second = slab.insert("second");
        assert_eq!(slab.get(first), Ok(&"first"));
        assert_eq!(slab.get(second), Ok(&"second"));

        // Enough inserts to force reallocation several times over.
        for _ in 0..200 {
            slab.insert("filler");
        }
        assert_eq!(slab.get(first), Ok(&"first"), "the first key still lands");
        assert_eq!(slab.get(second), Ok(&"second"));
        assert_eq!(slab.len(), 202);
    }

    #[test]
    fn a_removed_key_is_dead_the_moment_its_value_leaves() {
        let mut slab = Slab::new();
        let key = slab.insert(41);
        assert_eq!(slab.remove(key), Ok(41));
        assert_eq!(slab.len(), 0);

        let err = slab.get(key).expect_err("the key is retired");
        assert_eq!(
            err,
            SlabError::StaleGeneration {
                slot: 0,
                generation: 0,
                current_generation: 1
            }
        );
        assert!(err.to_string().contains("moved on"), "{err}");
        assert_eq!(slab.remove(key), Err(err), "removing through it fails too");
    }

    #[test]
    fn a_reused_slot_never_answers_an_earlier_occupant_s_key() {
        // The whole reason for generations. Without the bump-on-remove, a key
        // kept from before a removal would read whoever moved in afterwards,
        // which is how a furnace becomes a chest without anyone noticing.
        let mut slab = Slab::new();
        let original = slab.insert('a');
        slab.remove(original).expect("present");
        let replacement = slab.insert('b');

        assert_eq!(replacement.slot(), original.slot(), "the slot was reused");
        assert_ne!(
            replacement.generation(),
            original.generation(),
            "and its generation moved"
        );
        // The retired key names slot 0 at generation 0; the slot now lives at
        // generation 1, and says so rather than answering with 'b'.
        assert_eq!(
            slab.get(original).err(),
            Some(SlabError::StaleGeneration {
                slot: 0,
                generation: 0,
                current_generation: 1,
            })
        );
        assert_eq!(slab.get(replacement), Ok(&'b'));
    }

    #[test]
    fn reuse_always_takes_the_lowest_vacant_slot_whatever_the_order_was() {
        // Fill six, empty three non-adjacent ones in an arbitrary order, and
        // the refills must take them lowest-first -- including ahead of
        // growth, which would hand out fresh slots past the end. The slot
        // sequence therefore depends only on which slots are open -- the
        // property that keeps two replays of one schedule interchangeable.
        let mut slab = Slab::new();
        let keys: Vec<SlabKey> = (0..6u32).map(|_| slab.insert(())).collect();
        for gone in [4usize, 0, 2] {
            slab.remove(keys[gone]).expect("live");
        }
        let refilled: Vec<u32> = (0..2).map(|_| slab.insert(()).slot()).collect();
        assert_eq!(refilled, vec![0, 2]);
        assert_eq!(
            slab.insert(()).slot(),
            4,
            "the last hole in line, still ahead of growth"
        );
        assert_eq!(
            slab.insert(()).slot(),
            6,
            "only once every hole is spent does the slab grow"
        );
    }

    #[test]
    fn iteration_is_slot_order_and_skips_nothing_live_twice() {
        let mut slab = Slab::new();
        let letters = ['x', 'y', 'z'];
        let keys: Vec<SlabKey> = letters.iter().map(|c| slab.insert(*c)).collect();
        slab.remove(keys[1]).expect("live");

        let seen: Vec<char> = slab.iter().map(|(_, v)| *v).collect();
        assert_eq!(seen, vec!['x', 'z'], "one pass");
        let again: Vec<char> = slab.iter().map(|(_, v)| *v).collect();
        assert_eq!(again, seen, "and the same pass again");
        assert_eq!(slab.len(), 2);
    }

    #[test]
    fn equality_follows_contents_wherever_they_are_stored() {
        let mut straight = Slab::new();
        let mut scenic = Slab::new();
        for word in ["alpha", "beta", "gamma"] {
            straight.insert(word);
        }
        // The second slab reaches the same three values by a different road:
        // a fourth value in and out again leaves a hole that the last insert
        // refills, so the two slabs' internal layouts disagree.
        for word in ["alpha", "beta"] {
            scenic.insert(word);
        }
        let scratch = scenic.insert("scratch");
        scenic.remove(scratch).expect("live");
        scenic.insert("gamma");

        assert_eq!(straight, scenic, "same values, different slots");

        scenic.insert("extra");
        assert_ne!(straight, scenic, "an extra value is a difference");
    }

    #[test]
    fn a_key_from_beyond_the_slab_is_named_rather_than_guessed_at() {
        let slab: Slab<u8> = Slab::new();
        let err = slab
            .get(SlabKey::from_raw(7, 0))
            .expect_err("no slot 7 exists");
        assert_eq!(err, SlabError::Unallocated { slot: 7 });
        assert!(err.to_string().contains("never had"), "{err}");
    }
}

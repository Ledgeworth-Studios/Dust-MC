//! The slab against a map-of-keys model, under random edit schedules.
//!
//! One insert-remove-insert cycle is easy to eyeball; the failures that
//! matter are schedules. A key kept across three unrelated removals must
//! still report dead instead of reading the third replacement, the free list
//! must hand back the lowest hole whatever order holes were made in, and two
//! runs over one schedule must agree slot for slot -- the property replay
//! logs depend on. Those are properties of sequences, so this file drives a
//! few hundred deterministic ones through [`Slab`] and checks every step
//! against an independent model: a [`BTreeMap`] holding exactly the live
//! keys, and a [`BTreeSet`] of the vacancies the reuse policy owes us.
//!
//! **On randomness:** every schedule comes from a fixed-seed xorshift, so a
//! failure replays exactly; the panic names the seed, the step and both
//! sides' views.
//!
//! **What this does not catch:** a slab whose model *is* a map with the same
//! blind spots. The model here answers "which keys are live" and "which slot
//! is owed"; it cannot judge whether those are the right rules, which is what
//! the unit tests beside the implementation argue.

use std::collections::{BTreeMap, BTreeSet};

use dust_world::{Slab, SlabError, SlabKey};

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// What one step of a schedule does, decided by the seed alone so the driver
/// and its failure message see the same stream.
#[derive(Debug, Clone, Copy)]
enum Op {
    /// Put a fresh record in.
    Insert,
    /// Remove the record at this position of the still-live list.
    Remove(usize),
    /// Read through a key the model says is dead: an old occupant's key, or a
    /// forged one naming a slot past the end.
    ProbeDead,
}

fn schedule(seed: u64, steps: usize) -> Vec<Op> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut ops = Vec::with_capacity(steps);
    let mut live = 0usize;
    for _ in 0..steps {
        match xorshift(&mut state) % 10 {
            0..=4 | 8..=9 => {
                ops.push(Op::Insert);
                live += 1;
            }
            5..=6 => {
                if live > 0 {
                    ops.push(Op::Remove((xorshift(&mut state) as usize) % live));
                    live -= 1;
                } else {
                    ops.push(Op::Insert);
                    live += 1;
                }
            }
            _ => ops.push(Op::ProbeDead),
        }
    }
    ops
}

/// Everything the model knows after watching the same schedule.
#[derive(Default)]
struct Model {
    /// Every live key, mapped to the value that went with it.
    live: BTreeMap<SlabKey, u64>,
    /// Keys retired by removal, kept so later steps can confirm they stay
    /// dead through further churn.
    dead: Vec<SlabKey>,
    /// Slots currently vacant, per the bookkeeping the reuse policy promises.
    vacant: BTreeSet<u32>,
    /// How many times each slot has had an occupant leave, which is exactly
    /// the generation the slot now carries.
    retirements: BTreeMap<u32, u32>,
    /// The next slot growth would allocate, when there are no vacancies.
    fresh: u32,
    /// How many records have been inserted, so values can be unique.
    issued: u64,
}

impl Model {
    /// The slot the next insert must take: the lowest vacancy, or growth.
    fn next_slot(&self) -> u32 {
        self.vacant.iter().next().copied().unwrap_or(self.fresh)
    }

    fn insert(&mut self, slab: &mut Slab<u64>, step: usize, seed: u64) {
        let expected_slot = self.next_slot();
        self.issued += 1;
        let value = self.issued;
        let key = slab.insert(value);
        assert_eq!(
            key.slot(),
            expected_slot,
            "seed {seed} step {step}: insert took slot {} where the lowest vacancy or \
             growth owed {}",
            key.slot(),
            expected_slot,
        );
        let displaced = self.live.insert(key, value);
        assert!(
            displaced.is_none(),
            "seed {seed} step {step}: the slab reissued the live key {key:?}"
        );
        if !self.vacant.remove(&expected_slot) {
            self.fresh += 1;
        }
    }

    fn remove(&mut self, slab: &mut Slab<u64>, position: usize, step: usize, seed: u64) {
        let key = *self
            .live
            .keys()
            .nth(position)
            .expect("live at that position");
        let value = self.live.remove(&key).expect("removed from the model too");
        assert_eq!(
            slab.remove(key),
            Ok(value),
            "seed {seed} step {step}: removing {key:?}"
        );
        assert_eq!(
            slab.get(key).err(),
            Some(SlabError::StaleGeneration {
                slot: key.slot(),
                generation: key.generation(),
                current_generation: key.generation() + 1,
            }),
            "seed {seed} step {step}: the key outlived its own removal"
        );
        self.dead.push(key);
        self.vacant.insert(key.slot());
        *self.retirements.entry(key.slot()).or_insert(0) += 1;
    }

    fn check(&self, slab: &Slab<u64>, step: usize, seed: u64) {
        assert_eq!(
            slab.len(),
            self.live.len(),
            "seed {seed} step {step}: disagree about how much is live"
        );
        let seen: BTreeMap<SlabKey, u64> = slab.iter().map(|(k, v)| (k, *v)).collect();
        assert_eq!(
            seen, self.live,
            "seed {seed} step {step}: the live set diverged"
        );
        // Iteration visits ascending slots, skipping vacancies without
        // reordering: the determinism the replay story rests on.
        let slots: Vec<u32> = slab.iter().map(|(k, _)| k.slot()).collect();
        assert!(
            slots.windows(2).all(|w| w[0] < w[1]),
            "seed {seed} step {step}: iteration left slot order ({slots:?})"
        );
        // Every key ever retired by removal is still dead, and says why by
        // name: a slot growth never reached was never anyone's; any other
        // dead key names the generation the slot has moved to, which the
        // model knows independently as its retirement count.
        for key in &self.dead {
            let outcome = slab.get(*key);
            let expected = if key.slot() >= self.fresh {
                SlabError::Unallocated { slot: key.slot() }
            } else {
                SlabError::StaleGeneration {
                    slot: key.slot(),
                    generation: key.generation(),
                    current_generation: self.retirements[&key.slot()],
                }
            };
            assert_eq!(
                outcome.err(),
                Some(expected),
                "seed {seed} step {step}: dead key {key:?} reported wrongly"
            );
        }
    }
}

#[test]
fn random_schedules_never_let_a_dead_key_answer_or_a_hole_go_unclaimed() {
    for seed in 0..100u64 {
        let ops = schedule(seed, 400);
        let mut slab = Slab::new();
        let mut model = Model::default();
        for (step, op) in ops.iter().enumerate() {
            match *op {
                Op::Insert => model.insert(&mut slab, step, seed),
                Op::Remove(position) => model.remove(&mut slab, position, step, seed),
                Op::ProbeDead => {
                    // A forged key: right slot shape, generation nobody issued.
                    let forged = SlabKey::from_raw(model.fresh + 7, 9_000);
                    assert!(
                        slab.get(forged).is_err(),
                        "seed {seed} step {step}: a forged key answered"
                    );
                }
            }
            model.check(&slab, step, seed);
        }
    }
}

#[test]
fn two_runs_over_one_schedule_agree_slot_for_slot_and_generation_for_generation() {
    // Replay determinism: the slot sequence is a function of the schedule, so
    // a log recorded against one run reads true against another.
    for seed in [0u64, 41, 7_777] {
        let ops = schedule(seed, 300);
        let mut first = Slab::new();
        let mut second = Slab::new();
        let mut live_first: Vec<SlabKey> = Vec::new();
        let mut live_second: Vec<SlabKey> = Vec::new();
        for op in ops.iter() {
            match *op {
                Op::Insert => {
                    let a = first.insert(seed);
                    let b = second.insert(seed);
                    assert_eq!(a, b, "seed {seed}: the same schedule issued different keys");
                    live_first.push(a);
                    live_second.push(b);
                }
                Op::Remove(position) => {
                    let key = live_first[position];
                    assert_eq!(first.remove(key), second.remove(live_second[position]));
                    live_first.retain(|k| *k != key);
                    live_second.retain(|k| *k != key);
                }
                Op::ProbeDead => {
                    let forged = SlabKey::from_raw(u32::from(u8::MAX), u16::MAX as u32);
                    assert!(first.get(forged).is_err(), "seed {seed}: forged answered");
                    assert!(second.get(forged).is_err(), "seed {seed}: forged answered");
                }
            }
        }
        let history_a: Vec<(SlabKey, u64)> = first.iter().map(|(k, v)| (k, *v)).collect();
        let history_b: Vec<(SlabKey, u64)> = second.iter().map(|(k, v)| (k, *v)).collect();
        assert_eq!(history_a, history_b, "seed {seed}: final states differ");
    }
}

//! What runs inside a tick, and the order it runs in.
//!
//! A tick is not one operation; it is a queue of them. Networking wants to go
//! first so input is fresh, world simulation wants the middle, and flushes
//! want the end. Rather than hard-code that order into a loop that will
//! outlive all of its current stages, the engine asks each stage for a
//! [`priority`] and sorts ascending: **lower number, earlier in the tick**.
//! Ties keep registration order, because between two things that do not care
//! when they run, "the order they were added" is at least explainable.
//!
//! The trait is deliberately tiny — a name, a priority, one method — because
//! every field it grew would be a field the network and worldgen crates would
//! have to invent values for forever.

use std::fmt;

use crate::logging::Logger;

/// Everything one tick hands to a participant.
///
/// This is intentionally read-only bookkeeping rather than a god object: a
/// participant that wants to change server state does it through whatever
/// shared handle its constructor was given, not by reaching through here.
#[derive(Debug)]
pub struct TickContext<'a> {
    /// Index of this tick across the process lifetime; the first tick ever
    /// run is 0. Deterministic under virtual time, which is what makes
    /// assertions like "the status probe fired on tick 40" possible.
    pub tick_index: u64,
    /// How much simulated time this tick represents, in nanoseconds. Fixed at
    /// 50 ms today; participants should compute from this, never from their
    /// own constant, so a future tick-rate change finds no victims.
    pub tick_duration_ns: u64,
    /// The server logger, target-prefixed by the caller's own name.
    pub logger: &'a Logger,
}

/// One unit of work inside the tick loop.
///
/// Implementors are registered on the [`ParticipantSet`](ParticipantSet)
/// before the loop starts (or handed to `ServerOptions::extra_tasks`); the
/// engine then calls `tick` once per executed tick, in priority order.
///
/// The `Send` bound is what allows the whole registry to move into the thread
/// that owns the loop.
pub trait TickParticipant: Send {
    /// Stable identifier used in logs and per-participant timing tables.
    fn name(&self) -> &str;

    /// Position within a tick; lower runs earlier. Ties keep registration
    /// order. Rough bands, if you want company: listeners below 0, simulation
    /// near 0, bookkeeping and flushing above 100.
    fn priority(&self) -> i32;

    /// Do this tick's work. The clock keeps running while you do, which is
    /// exactly how long ticks end up visible in the timing histogram.
    fn tick(&mut self, ctx: &TickContext);
}

/// An ordered collection of participants.
///
/// Order is maintained on insertion, so iteration during the hot loop is a
/// plain slice walk with no sorting cost where sorting would hurt.
pub struct ParticipantSet {
    entries: Vec<Entry>,
    next_order: u64,
}

struct Entry {
    priority: i32,
    order: u64,
    participant: Box<dyn TickParticipant>,
}

impl Default for ParticipantSet {
    fn default() -> Self {
        Self::new()
    }
}

impl ParticipantSet {
    /// An empty set.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_order: 0,
        }
    }

    /// Register a participant. Later calls with an equal priority run after
    /// earlier ones; nothing stops two participants sharing a name except
    /// everyone's ability to read the timing table afterwards, so callers
    /// should keep names unique.
    pub fn insert(&mut self, participant: Box<dyn TickParticipant>) {
        let entry = Entry {
            priority: participant.priority(),
            order: self.next_order,
            participant,
        };
        self.next_order += 1;
        self.entries.push(entry);
        // Stable sort: equal (priority) keys preserve insertion order via the
        // explicit `order` tiebreaker, making the ordering total regardless.
        self.entries
            .sort_by(|a, b| a.priority.cmp(&b.priority).then(a.order.cmp(&b.order)));
    }

    /// How many participants are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nobody signed up.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Names in execution order, for logs and reports.
    pub fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e.participant.name().to_owned())
            .collect()
    }

    /// Walk every participant in execution order.
    pub(crate) fn for_each(&mut self, mut f: impl FnMut(&mut dyn TickParticipant)) {
        for entry in &mut self.entries {
            f(entry.participant.as_mut());
        }
    }
}

impl fmt::Debug for ParticipantSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        name: &'static str,
        priority: i32,
        log: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl TickParticipant for Recorder {
        fn name(&self) -> &str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        fn tick(&mut self, _: &TickContext) {
            self.log.lock().unwrap().push(self.name);
        }
    }

    fn set_with(log: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>) -> ParticipantSet {
        let mut set = ParticipantSet::new();
        let mk = |name, priority| {
            Box::new(Recorder {
                name,
                priority,
                log: std::sync::Arc::clone(&log),
            }) as Box<dyn TickParticipant>
        };
        // Registered deliberately out of order: the set must sort them.
        set.insert(mk("flusher", 100));
        set.insert(mk("world", 0));
        set.insert(mk("listener", -10));
        set.insert(mk("second-world", 0));
        set
    }

    #[test]
    fn participants_run_in_priority_order_with_registration_ties() {
        let log = std::sync::Arc::default();
        let mut set = set_with(std::sync::Arc::clone(&log));
        let logger = crate::logging::Logger::to_stdout(
            crate::logging::Level::Error,
            std::sync::Arc::new(crate::clock::ManualClock::new()),
        );
        let ctx = TickContext {
            tick_index: 0,
            tick_duration_ns: 50_000_000,
            logger: &logger,
        };
        set.for_each(|p| p.tick(&ctx));
        assert_eq!(
            *log.lock().unwrap(),
            vec!["listener", "world", "second-world", "flusher"],
            "ascending priority, ties by registration"
        );
    }

    #[test]
    fn names_report_execution_order_not_registration_order() {
        let set = set_with(std::sync::Arc::default());
        assert_eq!(
            set.names(),
            vec!["listener", "world", "second-world", "flusher"]
        );
    }

    #[test]
    fn an_empty_set_is_ready_but_does_nothing() {
        let mut set = ParticipantSet::new();
        assert!(set.is_empty());
        set.for_each(|_| panic!("nothing is registered; nothing may run"));
    }
}

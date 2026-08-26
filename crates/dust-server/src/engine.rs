//! The fixed-timestep tick engine.
//!
//! # The accumulator, and why it surrenders
//!
//! A Minecraft server owes the world one tick every 50 ms ([`TICK_NS`]). Real
//! time does not arrive in 50 ms slices — it arrives in bursts and gaps — so
//! the loop keeps an *accumulator* of owed time: each pass adds however long
//! since the last pass, then spends it in whole ticks.
//!
//! The failure mode of that design is famous: if the machine stalls longer
//! than the catch-up allowance, the debt grows faster than it is paid, every
//! pass runs the maximum number of ticks, the server spends all its CPU on
//! the past and none on the present. That is the **spiral of death**. The
//! guard here is [`MAX_CATCHUP_TICKS`]: a single burst may never run more
//! than that many ticks, and when the cap is hit the engine *surrenders* —
//! it drops the remaining debt and resynchronises to now, trading a
//! discontinuity (mobs were un-simulated for a moment) for a livelock.
//!
//! Twenty ticks is one second of game time. A stall shorter than that is
//! caught up smoothly; a stall longer than that is skipped past. Both halves
//! of that trade are deliberate.
//!
//! # Virtual-time discipline
//!
//! Every reading comes from the injected [`Clock`]. `advance` is also callable
//! with an explicit reading (`advance_at`) so pure tests can drive the
//! arithmetic without any threading at all.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::clock::Clock;
use crate::histogram::{TimingHistogram, TimingStats};
use crate::logging::Logger;
use crate::participant::{ParticipantSet, TickContext};

/// One tick of simulated time: 50 ms, the vanilla 20-TPS cadence.
pub const TICK_NS: u64 = 50_000_000;

/// The most simulated time one burst may try to repay, in ticks.
///
/// See the module docs for why this limit exists. If you raise it, you are
/// deciding the server should try harder to hide long stalls; if you lower
/// it, you are deciding it should give up sooner and stay responsive. Either
/// is a defensible answer; having no answer is not.
pub const MAX_CATCHUP_TICKS: u32 = 20;

/// What one call to [`TickEngine::advance`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdvanceReport {
    /// Ticks executed by this call.
    pub ticks_executed: u64,
    /// Whether this call hit the catch-up cap and dropped the remaining debt.
    pub surrendered: bool,
}

/// Runs participants at a fixed cadence over an injected clock.
pub struct TickEngine {
    clock: Arc<dyn Clock>,
    /// Owed-but-unpaid time, carried between advances.
    debt_ns: u64,
    /// Reading of the previous advance, for computing elapsed time.
    last_now: Option<u64>,
    /// Absolute deadline of the next scheduled tick.
    next_tick_at: Option<u64>,
    ticks_run: u64,
    paused: bool,
    surrendered_batches: u64,
    overall: TimingHistogram,
    per_participant: BTreeMap<String, TimingHistogram>,
}

impl std::fmt::Debug for TickEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickEngine")
            .field("ticks_run", &self.ticks_run)
            .field("debt_ns", &self.debt_ns)
            .field("paused", &self.paused)
            .field("surrendered_batches", &self.surrendered_batches)
            .finish_non_exhaustive()
    }
}

impl TickEngine {
    /// An idle engine reading time from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            debt_ns: 0,
            last_now: None,
            next_tick_at: None,
            ticks_run: 0,
            paused: false,
            surrendered_batches: 0,
            overall: TimingHistogram::new(),
            per_participant: BTreeMap::new(),
        }
    }

    /// Advance to the clock's current reading and run whatever is due.
    pub fn advance(
        &mut self,
        participants: &mut ParticipantSet,
        logger: &Logger,
    ) -> AdvanceReport {
        self.advance_at(self.clock.now_ns(), participants, logger)
    }

    /// Advance to an explicit reading. This is where the whole cadence lives;
    /// [`advance`](Self::advance) is a thin wrapper over it.
    ///
    /// Semantics, in the order they are applied:
    ///
    /// 1. The first call arms the schedule without running anything — a boot
    ///    must not instantly owe ticks for all of process history.
    /// 2. While paused, nothing runs and any accumulated debt is forgiven, so
    ///    resuming never unleashes a burst to pay for time nobody observed.
    /// 3. Otherwise, due ticks run one at a time until either the schedule
    ///    catches up with `now` or [`MAX_CATCHUP_TICKS`] have run in this
    ///    single burst, at which point the rest of the debt is dropped and
    ///    the schedule resynchronises to now.
    pub fn advance_at(
        &mut self,
        now_ns: u64,
        participants: &mut ParticipantSet,
        logger: &Logger,
    ) -> AdvanceReport {
        let Some(last) = self.last_now.replace(now_ns) else {
            // First observation: arm from here, owe nothing for the past.
            self.next_tick_at = Some(now_ns.saturating_add(TICK_NS));
            return AdvanceReport::default();
        };
        let _ = last; // elapsed is derived from the schedule, not the delta

        if self.paused {
            // Pause forgives: the alternative (banking the gap) would punish
            // whoever resumes with a maximum-length burst they did not cause.
            self.next_tick_at = Some(now_ns.saturating_add(TICK_NS));
            self.debt_ns = 0;
            return AdvanceReport::default();
        }

        let mut report = AdvanceReport::default();
        while let Some(deadline) = self.next_tick_at {
            if now_ns < deadline {
                break;
            }
            if report.ticks_executed >= u64::from(MAX_CATCHUP_TICKS) {
                report.surrendered = true;
                self.surrendered_batches += 1;
                logger.warn(
                    "dust::engine",
                    format!(
                        "tick loop fell {} tick(s) behind; skipping ahead to stay live",
                        MAX_CATCHUP_TICKS
                    ),
                );
                self.next_tick_at = Some(now_ns.saturating_add(TICK_NS));
                break;
            }
            self.run_one_tick(participants, logger);
            self.next_tick_at = Some(deadline.saturating_add(TICK_NS));
            report.ticks_executed += 1;
        }

        report
    }

    /// Execute exactly one tick against every participant in priority order,
    /// charging the wall time each participant took to its own histogram.
    fn run_one_tick(&mut self, participants: &mut ParticipantSet, logger: &Logger) {
        let ctx = TickContext {
            tick_index: self.ticks_run,
            tick_duration_ns: TICK_NS,
            logger,
        };
        let started = self.clock.now_ns();
        let mut measured = BTreeMap::<String, u64>::new();
        participants.for_each(|p| {
            let before = self.clock.now_ns();
            p.tick(&ctx);
            let after = self.clock.now_ns();
            measured.insert(p.name().to_owned(), after.saturating_sub(before));
        });
        let finished = self.clock.now_ns();

        self.overall.record(finished.saturating_sub(started));
        for (name, ns) in measured {
            self.per_participant.entry(name).or_default().record(ns);
        }
        self.ticks_run += 1;
    }

    /// When the next tick is scheduled, or `None` before the first advance
    /// arms the schedule. This is what the loop parks on between passes.
    pub fn next_deadline(&self) -> Option<u64> {
        self.next_tick_at
    }

    /// Ticks executed since construction. Deterministic under virtual time.
    pub fn ticks_run(&self) -> u64 {
        self.ticks_run
    }

    /// Stop executing ticks. See [`advance_at`](Self::advance_at) for what
    /// pause does to accumulated debt, and why.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Resume the normal cadence from the next advance.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// How many bursts have hit the catch-up cap.
    pub fn surrendered_batches(&self) -> u64 {
        self.surrendered_batches
    }

    /// Whole-loop timing over the recent window.
    pub fn overall_timing(&self) -> TimingStats {
        self.overall.snapshot()
    }

    /// Per-participant timing, keyed by participant name.
    pub fn participant_timing(&self, name: &str) -> Option<TimingStats> {
        self.per_participant.get(name).map(TimingHistogram::snapshot)
    }

    /// Every participant that has been accounted, in name order.
    pub fn accounted_participants(&self) -> Vec<String> {
        self.per_participant.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    /// A participant that charges `charge_ns` of virtual time per tick by
    /// advancing the very clock the engine measures with, so the accounting
    /// has something honest — and exactly predictable — to measure.
    struct Work {
        name: &'static str,
        priority: i32,
        charge_ns: u64,
        clock: Arc<ManualClock>,
        log: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    }

    impl TickParticipant for Work {
        fn name(&self) -> &str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        fn tick(&mut self, ctx: &TickContext) {
            if self.charge_ns > 0 {
                self.clock.advance_ns(self.charge_ns);
            }
            self.log.lock().unwrap().push(ctx.tick_index);
        }
    }

    struct Harness {
        clock: Arc<ManualClock>,
        engine: TickEngine,
        set: ParticipantSet,
        logger: Logger,
    }

    impl Harness {
        fn new() -> Self {
            Self::with(|_, _| {})
        }

        /// Build a harness whose participants already share its clock.
        fn with(setup: impl FnOnce(&Arc<ManualClock>, &mut ParticipantSet)) -> Self {
            let clock = Arc::new(ManualClock::new());
            let sink = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
            let logger =
                Logger::new(sink, Level::Error, Arc::clone(&clock) as Arc<dyn Clock>);
            let mut set = ParticipantSet::new();
            setup(&clock, &mut set);
            let engine = TickEngine::new(Arc::clone(&clock) as Arc<dyn Clock>);
            Self { clock, engine, set, logger }
        }

        /// Arm the schedule, then move virtual time forward by whole ticks.
        fn run_ticks(&mut self, count: usize) -> AdvanceReport {
            self.engine.advance_at(0, &mut self.set, &self.logger);
            self.clock.advance_ns(u64::try_from(count).unwrap() * TICK_NS);
            self.engine.advance(&mut self.set, &self.logger)
        }
    }

    #[test]
    fn the_first_advance_arms_the_schedule_without_ticking() {
        let mut h = Harness::new();
        let report = h.engine.advance(&mut h.set, &h.logger);
        assert_eq!(report, AdvanceReport::default());
        assert_eq!(h.engine.ticks_run(), 0);
        assert_eq!(
            h.engine.next_deadline(),
            Some(TICK_NS),
            "the first tick is due one period after boot"
        );
    }

    #[test]
    fn one_advance_runs_exactly_the_ticks_that_came_due() {
        let mut h = Harness::new();
        assert_eq!(h.run_ticks(5).ticks_executed, 5);
        assert_eq!(h.engine.ticks_run(), 5);
        // Time passing without an advance banks up; one advance then pays it.
        h.clock.advance_ns(3 * TICK_NS);
        let report = h.engine.advance(&mut h.set, &h.logger);
        assert_eq!(report.ticks_executed, 3);
        assert_eq!(h.engine.ticks_run(), 8);
    }

    #[test]
    fn partial_periods_wait_for_their_turn_instead_of_running_early() {
        let mut h = Harness::new();
        h.run_ticks(1);
        h.clock.advance_ns(TICK_NS / 2); // half a period: not due yet
        let report = h.engine.advance(&mut h.set, &h.logger);
        assert_eq!(report.ticks_executed, 0);
        assert_eq!(h.engine.ticks_run(), 1);
    }

    #[test]
    fn a_burst_past_the_cap_surrenders_and_resynchronises() {
        let mut h = Harness::new();
        h.engine.advance_at(0, &mut h.set, &h.logger);
        // Thirty seconds of stall against a twenty-tick allowance.
        h.clock.advance_ns(600 * TICK_NS);
        let report = h.engine.advance(&mut h.set, &h.logger);
        assert!(report.surrendered, "a 600-tick debt cannot be repaid");
        assert_eq!(report.ticks_executed, u64::from(MAX_CATCHUP_TICKS));
        assert_eq!(h.engine.ticks_run(), u64::from(MAX_CATCHUP_TICKS));
        // After surrendering, the schedule is anchored to *now*, so the next
        // tick is one period away rather than another immediate burst.
        assert_eq!(
            h.engine.next_deadline(),
            Some(600 * TICK_NS + TICK_NS),
            "resync anchors to the current reading"
        );
        assert_eq!(h.engine.surrendered_batches(), 1);
        // And time moving normally again produces normal ticking, proving the
        // death spiral did not survive the resync.
        h.clock.advance_ns(TICK_NS);
        let again = h.engine.advance(&mut h.set, &h.logger);
        assert!(!again.surrendered);
        assert_eq!(again.ticks_executed, 1);
    }

    #[test]
    fn a_burst_within_the_cap_never_surrenders() {
        let mut h = Harness::new();
        h.engine.advance_at(0, &mut h.set, &h.logger);
        h.clock.advance_ns(u64::from(MAX_CATCHUP_TICKS) * TICK_NS);
        let report = h.engine.advance(&mut h.set, &h.logger);
        assert!(!report.surrendered);
        assert_eq!(report.ticks_executed, u64::from(MAX_CATCHUP_TICKS));
    }

    #[test]
    fn pause_runs_nothing_and_resume_does_not_repay_the_gap() {
        let mut h = Harness::new();
        h.run_ticks(2);
        h.engine.pause();
        assert!(h.engine.is_paused());
        // Two seconds pass while paused.
        h.clock.advance_ns(40 * TICK_NS);
        let while_paused = h.engine.advance(&mut h.set, &h.logger);
        assert_eq!(while_paused, AdvanceReport::default());
        h.engine.resume();
        // Resuming forgives the debt: the very next advance is still quiet,
        // and only a full new period produces a tick.
        let just_resumed = h.engine.advance(&mut h.set, &h.logger);
        assert_eq!(just_resumed.ticks_executed, 0);
        h.clock.advance_ns(TICK_NS / 4);
        assert_eq!(h.engine.advance(&mut h.set, &h.logger).ticks_executed, 0);
        h.clock.advance_ns(TICK_NS - TICK_NS / 4);
        assert_eq!(h.engine.advance(&mut h.set, &h.logger).ticks_executed, 1);
    }

    #[test]
    fn the_tick_counter_is_deterministic_under_virtual_time() {
        let mut h = Harness::new();
        h.run_ticks(7);
        h.run_ticks(0);
        h.run_ticks(3);
        assert_eq!(h.engine.ticks_run(), 10);
    }

    #[test]
    fn each_participant_is_accounted_separately_from_the_whole() {
        let work_log = std::sync::Arc::default();
        let mut h = Harness::with(|clock, set| {
            set.insert(Box::new(Work {
                name: "quick",
                priority: 0,
                charge_ns: 0,
                clock: Arc::clone(clock),
                log: std::sync::Arc::clone(&work_log),
            }));
            set.insert(Box::new(Work {
                name: "slow",
                priority: 10,
                charge_ns: 1_500,
                clock: Arc::clone(clock),
                log: std::sync::Arc::clone(&work_log),
            }));
        });
        h.run_ticks(4);

        let quick = h.engine.participant_timing("quick").expect("accounted");
        let slow = h.engine.participant_timing("slow").expect("accounted");
        assert_eq!(quick.window_samples, 4);
        assert_eq!(slow.window_samples, 4);
        assert_eq!(quick.avg, Some(0), "a free participant is measured as free");
        assert_eq!(slow.avg, Some(1_500), "charged time lands on the right row");
        let overall = h.engine.overall_timing();
        assert_eq!(overall.avg, Some(1_500), "the whole tick pays for its parts");
        assert_eq!(
            h.engine.accounted_participants(),
            vec!["quick", "slow"],
            "every registered participant appears in the table"
        );
        assert_eq!(*work_log.lock().unwrap(), vec![0, 0, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn an_unaccounted_name_reports_none_rather_than_zeros() {
        let mut h = Harness::new();
        h.run_ticks(1);
        assert!(h.engine.participant_timing("ghost").is_none());
    }
}

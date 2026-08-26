//! How a running server is told to stop, and how it keeps its promises about
//! stopping.
//!
//! Four small pieces live here, and together they carry two guarantees the
//! rest of the crate leans on:
//!
//! 1. **Stop is observed between ticks, never mid-tick.** [`StopHandle`] is
//!    just an atomic plus a broadcast; it never interrupts anyone. The tick
//!    loop checks it between passes, which means a ctrl-C waits at most one
//!    tick batch before taking effect — coarse by design. Interrupting a
//!    half-finished world update is not coarse, it is corrupting.
//! 2. **Shutdown has a deadline.** A graceful path that never finishes is a
//!    hang wearing a lanyard. The [`WatchdogPolicy`] arms when stop is
//!    requested; if shutdown has not completed within the grace period, the
//!    configured action runs (in production: hard-exit the process). The
//!    action is injected so tests can observe the firing without dying.
//!
//! Parking between ticks goes through the [`Parker`] trait for the same
//! virtual-time reason everything else does: production parks on a condvar,
//! tests park by advancing a manual clock, and neither knows about the other.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::clock::Clock;

/// Shared stop state: one atomic bit plus a condvar so sleepers wake early.
///
/// Opaque by design — outside callers hold it only to hand it back: a
/// [`StopHandle`] is the polite front door for requesting a stop, and parker
/// constructors take this state because two threads must agree on the same
/// condvar. Nothing here is worth calling directly.
#[derive(Debug, Default)]
pub struct StopState {
    stopped: AtomicBool,
    sleeper: Mutex<()>,
    wake: Condvar,
}

impl StopState {
    /// Request a stop. Returns true for the caller that flipped the bit, so
    /// "stop was already requested" stays distinguishable from "I stopped it".
    pub(crate) fn request(&self) -> bool {
        self.stopped
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Sleep until either a stop arrives or `deadline_ns` passes on `clock`.
    ///
    /// This is the production park: it cannot busy-wait (a server idling
    /// between ticks would burn a core) and it must not sleep through a stop
    /// request (shutdown would wait out the whole inter-tick gap).
    pub(crate) fn park_until_stopped_or(&self, deadline_ns: u64, clock: &dyn Clock) {
        let mut guard = self.sleeper.lock().unwrap();
        loop {
            if self.is_stopped() {
                return;
            }
            let now = clock.now_ns();
            if now >= deadline_ns {
                return;
            }
            let wait = Duration::from_nanos(deadline_ns - now);
            let (next_guard, _) = self.wake.wait_timeout(guard, wait).unwrap();
            // Spurious wake-ups re-enter the loop and re-check both ends.
            guard = next_guard;
        }
    }

    /// Wake every parker alongside the stop bit.
    pub(crate) fn broadcast_stop(&self) {
        self.wake.notify_all();
    }
}

/// A handle for requesting a clean shutdown.
///
/// Cloned into signal handlers, console commands and tests. This is what a
/// simulated ctrl-C *is*: calling [`request_stop`](Self::request_stop) is
/// indistinguishable, to the server, from the real keypress.
#[derive(Clone, Debug)]
pub struct StopHandle {
    state: Arc<StopState>,
}

impl StopHandle {
    pub(crate) fn new(state: Arc<StopState>) -> Self {
        Self { state }
    }

    /// Ask the server to stop at the next safe boundary. Idempotent; the
    /// return value says whether this call was the first request.
    pub fn request_stop(&self) -> bool {
        let first = self.state.request();
        if first {
            self.state.broadcast_stop();
        }
        first
    }

    /// Whether anyone has asked for a stop yet.
    pub fn is_stop_requested(&self) -> bool {
        self.state.is_stopped()
    }
}

/// Sleeps between tick-loop passes until a deadline (or forever, if the
/// implementation chooses).
///
/// Implementations must be safe to own from exactly one thread — the loop —
/// but may share their internals however they like.
pub trait Parker: Send {
    /// Park until `deadline_ns` on the associated clock, or until whatever
    /// this parker considers a good reason to wake early.
    fn park_until(&self, deadline_ns: u64);
}

/// Production parker: block on the stop condvar, waking for stop or deadline.
///
/// It reads deadlines against the same injected [`Clock`] as everything else,
/// which is also why it degrades gracefully under a manual clock in tests:
/// a frozen manual clock makes the computed wait long, but any stop request
/// still wakes it immediately.
pub struct CondvarParker {
    state: Arc<StopState>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for CondvarParker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CondvarParker").finish_non_exhaustive()
    }
}

impl CondvarParker {
    pub(crate) fn new(state: Arc<StopState>, clock: Arc<dyn Clock>) -> Self {
        Self { state, clock }
    }
}

impl Parker for CondvarParker {
    fn park_until(&self, deadline_ns: u64) {
        self.state
            .park_until_stopped_or(deadline_ns, self.clock.as_ref());
    }
}

/// Test parker: every park advances virtual time by a fixed step instead of
/// sleeping.
///
/// With the step set to one tick period, a parked loop wakes to find exactly
/// one tick due — which turns "run N ticks" into "let the loop park N times",
/// deterministic and instant. The watchdog uses the same trick with a coarser
/// step to march virtual time toward its deadline.
#[derive(Debug)]
pub struct StepParker {
    clock: std::sync::Arc<crate::clock::ManualClock>,
    step_ns: u64,
}

impl StepParker {
    /// Advance the shared manual clock by `step_ns` each time the loop parks.
    pub fn new(clock: std::sync::Arc<crate::clock::ManualClock>, step_ns: u64) -> Self {
        assert!(
            step_ns > 0,
            "a zero-step parker would spin without progressing"
        );
        Self { clock, step_ns }
    }
}

impl Parker for StepParker {
    fn park_until(&self, _deadline_ns: u64) {
        // The deadline is ignored on purpose: under virtual time the test,
        // not the wall, decides when "later" happens.
        self.clock.advance_ns(self.step_ns);
    }
}

/// What fired when a watchdog ran out of patience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogFiring {
    /// Grace period that elapsed, in nanoseconds.
    pub grace_ns: u64,
    /// Ticks completed before the firing, for the post-mortem.
    pub ticks_run: u64,
}

/// What a watchdog does when grace expires.
type WatchdogAction = Arc<dyn Fn(WatchdogFiring) + Send + Sync>;

/// Shutdown-deadline configuration: after stop is requested, grace runs out
/// once, and then `action` happens whether or not the graceful path agrees.
///
/// Production builds this with [`WatchdogPolicy::exit_process`]; tests build
/// it with a closure that records the firing, because an integration test
/// that kills the test process proves nothing.
#[derive(Clone)]
pub struct WatchdogPolicy {
    pub(crate) grace_ns: u64,
    pub(crate) action: WatchdogAction,
}

impl std::fmt::Debug for WatchdogPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchdogPolicy")
            .field("grace_ns", &self.grace_ns)
            .finish_non_exhaustive()
    }
}

impl WatchdogPolicy {
    /// Log loudly and leave. Exit code [`crate::cli::EXIT_WATCHDOG`] follows
    /// GNU timeout's convention for "this ended by force", so operators can
    /// tell a watchdog kill from a crash at a glance in shell history.
    pub fn exit_process(grace_ns: u64) -> Self {
        Self {
            grace_ns,
            action: Arc::new(|firing| {
                eprintln!(
                    "[dust] graceful shutdown exceeded {:?} ns ({} ticks done); \
                     exiting by watchdog",
                    firing.grace_ns, firing.ticks_run
                );
                std::process::exit(crate::cli::EXIT_WATCHDOG);
            }),
        }
    }

    /// Run an arbitrary action instead of exiting. For tests and embedded
    /// hosts that want to decide for themselves.
    pub fn custom(grace_ns: u64, action: impl Fn(WatchdogFiring) + Send + Sync + 'static) -> Self {
        Self {
            grace_ns,
            action: Arc::new(action),
        }
    }
}

/// Bookkeeping for every thread this process phase spawned.
///
/// The no-leaked-threads guarantee is only as good as its evidence, so each
/// spawn is recorded and each join is checked. `join_all` reports panics by
/// name rather than re-raising them: one wedged helper should be visible in
/// the shutdown report, not invisible inside someone else's `join()`.
#[derive(Debug, Default)]
pub(crate) struct ThreadTracker {
    threads: Mutex<Vec<Tracked>>,
}

#[derive(Debug)]
struct Tracked {
    name: String,
    handle: Option<JoinHandle<()>>,
}

impl ThreadTracker {
    /// Spawn a thread and remember it. Panicking spawns are recorded too —
    /// `std::thread::Builder::spawn` fails before a handle exists, and a
    /// spawn that never started needs no joining.
    pub(crate) fn spawn(self: &Arc<Self>, name: &str, f: impl FnOnce() + Send + 'static) {
        match thread::Builder::new().name(name.to_owned()).spawn(f) {
            Ok(handle) => self.threads.lock().unwrap().push(Tracked {
                name: name.to_owned(),
                handle: Some(handle),
            }),
            Err(e) => {
                // Losing the watchdog thread is worth knowing about at any
                // log level; losing nothing else exists yet.
                eprintln!("[dust] could not spawn thread {name}: {e}");
            }
        }
    }

    /// Join everything ever spawned. Returns the names joined and the names
    /// that died mid-flight.
    pub(crate) fn join_all(self: &Arc<Self>) -> (Vec<String>, Vec<String>) {
        let mut threads = self.threads.lock().unwrap();
        let mut joined = Vec::new();
        let mut panicked = Vec::new();
        for tracked in threads.iter_mut() {
            if let Some(handle) = tracked.handle.take() {
                match handle.join() {
                    Ok(()) => joined.push(tracked.name.clone()),
                    Err(_) => panicked.push(tracked.name.clone()),
                }
            }
        }
        (joined, panicked)
    }

    /// Threads spawned but not yet joined. Zero after `join_all`, which is
    /// precisely the assertion tests make.
    #[cfg(test)]
    pub(crate) fn outstanding(self: &Arc<Self>) -> usize {
        self.threads
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.handle.is_some())
            .count()
    }
}

/// The pieces a watchdog thread needs to do its one job.
pub(crate) struct WatchdogHarness {
    pub(crate) stop: Arc<StopState>,
    pub(crate) complete: Arc<AtomicBool>,
    pub(crate) fired: Arc<AtomicBool>,
    pub(crate) ticks_run: Arc<AtomicU64>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) parker: Box<dyn Parker>,
    pub(crate) policy: WatchdogPolicy,
}

/// Poll-and-fire loop for the watchdog thread.
///
/// It polls rather than sleeping until the exact instant, because the exact
/// instant is defined by the injected clock, which may be virtual. The slice
/// length trades polling frequency against reaction time and is irrelevant
/// under a stepper parker.
pub(crate) const WATCHDOG_SLICE_NS: u64 = 100_000_000;

pub(crate) fn watch_dog(harness: WatchdogHarness) {
    let WatchdogHarness {
        stop,
        complete,
        fired,
        ticks_run,
        clock,
        parker,
        policy,
    } = harness;

    // `None` until stop is requested; the deadline is measured from arming.
    let mut armed_at: Option<u64> = None;
    loop {
        if armed_at.is_none() && stop.is_stopped() {
            armed_at = Some(clock.now_ns());
        }
        if let Some(armed) = armed_at {
            if complete.load(Ordering::SeqCst) {
                // Graceful shutdown won the race. Nothing to enforce.
                return;
            }
            let expired = clock.now_ns().saturating_sub(armed) >= policy.grace_ns;
            if expired && !fired.load(Ordering::SeqCst) {
                fired.store(true, Ordering::SeqCst);
                (policy.action)(WatchdogFiring {
                    grace_ns: policy.grace_ns,
                    ticks_run: ticks_run.load(Ordering::SeqCst),
                });
                // The default action exits the process and never returns. A
                // custom action that does return leaves enforcement spent:
                // fire once, then keep watching only for completion.
            }
        }
        parker.park_until(clock.now_ns().saturating_add(WATCHDOG_SLICE_NS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{ManualClock, MonotonicClock};

    fn manual_clock() -> Arc<ManualClock> {
        Arc::new(ManualClock::new())
    }

    #[test]
    fn only_the_first_stop_request_counts_as_first() {
        let state = Arc::new(StopState::default());
        let handle = StopHandle::new(Arc::clone(&state));
        assert!(!handle.is_stop_requested());
        assert!(handle.request_stop(), "the first request flips the bit");
        assert!(!handle.request_stop(), "the second is a follower");
        assert!(handle.is_stop_requested());
    }

    #[test]
    fn a_condvar_park_returns_when_its_deadline_passes() {
        let state = Arc::new(StopState::default());
        let clock = Arc::new(MonotonicClock::new());
        // One millisecond: long enough that the timed wait is really taken,
        // short enough that nobody notices. This is the one test in the crate
        // that waits on real time, because real-time waking *is* the property
        // under test; everything else stays on virtual clocks.
        let deadline = clock.now_ns() + 1_000_000;
        state.park_until_stopped_or(deadline, clock.as_ref());
        assert!(clock.now_ns() >= deadline);
    }

    #[test]
    fn a_stop_request_wakes_a_parked_thread_promptly() {
        let state = Arc::new(StopState::default());
        let clock = Arc::new(MonotonicClock::new());
        let handle = StopHandle::new(Arc::clone(&state));
        let parked_state = Arc::clone(&state);
        let parked_clock = Arc::clone(&clock);
        // Parked against a one-minute deadline; only the stop request should
        // reasonably end this park.
        let worker = thread::spawn(move || {
            parked_state.park_until_stopped_or(
                parked_clock.now_ns() + 60_000_000_000,
                parked_clock.as_ref(),
            );
        });
        thread::yield_now();
        handle.request_stop();
        let started = std::time::Instant::now();
        worker.join().expect("the parker must return");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a stop request must wake sleepers instead of letting them wait out the deadline"
        );
    }

    #[test]
    fn a_step_parker_advances_virtual_time_instead_of_sleeping() {
        let clock = manual_clock();
        let parker = StepParker::new(Arc::clone(&clock), 50_000_000);
        parker.park_until(u64::MAX);
        parker.park_until(u64::MAX);
        assert_eq!(clock.now_ns(), 100_000_000);
    }

    #[test]
    fn the_watchdog_fires_once_after_grace_expires_unfinished() {
        let clock = manual_clock();
        let firings = Arc::new(std::sync::Mutex::new(Vec::<WatchdogFiring>::new()));
        let harness = WatchdogHarness {
            stop: Arc::new(StopState::default()),
            complete: Arc::new(AtomicBool::new(false)),
            fired: Arc::default(),
            ticks_run: Arc::new(AtomicU64::new(9)),
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            parker: Box::new(StepParker::new(Arc::clone(&clock), 250_000_000)),
            policy: WatchdogPolicy::custom(1_000_000_000, {
                let firings = Arc::clone(&firings);
                move |f| firings.lock().unwrap().push(f)
            }),
        };
        let stop_handle = StopHandle::new(Arc::clone(&harness.stop));
        let complete = Arc::clone(&harness.complete);
        let watcher_fired = Arc::clone(&harness.fired);

        let worker = thread::spawn(move || watch_dog(harness));

        // Arm, then wait for the watcher to march virtual time past grace on
        // its own — each of its parks steps 250 ms, five cross the line.
        assert!(stop_handle.request_stop());
        for _ in 0..1_000_000 {
            if watcher_fired.load(Ordering::SeqCst) {
                break;
            }
            thread::yield_now();
        }
        assert!(
            watcher_fired.load(Ordering::SeqCst),
            "grace expiry must fire even though nobody completed shutdown"
        );
        let seen = firings.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "fire exactly once");
        assert_eq!(seen[0].grace_ns, 1_000_000_000);
        assert_eq!(seen[0].ticks_run, 9);

        // Completion still retires the watcher cleanly after a firing.
        complete.store(true, Ordering::SeqCst);
        worker
            .join()
            .expect("watcher thread exits after completion");
    }

    #[test]
    fn a_completed_shutdown_never_fires_the_watchdog() {
        let clock = manual_clock();
        let fired_anyway = Arc::new(AtomicBool::new(false));
        let harness = WatchdogHarness {
            stop: Arc::new(StopState::default()),
            complete: Arc::new(AtomicBool::new(false)),
            fired: Arc::clone(&fired_anyway),
            ticks_run: Arc::default(),
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            parker: Box::new(StepParker::new(Arc::clone(&clock), 2_000_000_000)),
            policy: WatchdogPolicy::custom(1_000_000_000, |_| {
                panic!("completion must pre-empt the watchdog");
            }),
        };
        let stop_handle = StopHandle::new(Arc::clone(&harness.stop));
        let complete = Arc::clone(&harness.complete);

        let worker = thread::spawn(move || watch_dog(harness));
        assert!(stop_handle.request_stop());
        // Shutdown completes well inside grace.
        complete.store(true, Ordering::SeqCst);
        worker.join().expect("watcher exits promptly on completion");
        assert!(!fired_anyway.load(Ordering::SeqCst));
    }

    #[test]
    fn the_tracker_accounts_every_spawn_and_reports_panics_by_name() {
        let tracker = Arc::new(ThreadTracker::default());
        tracker.spawn("good", || ());
        tracker.spawn("doomed", || panic!("expected"));
        assert_eq!(tracker.outstanding(), 2);
        let (joined, panicked) = tracker.join_all();
        assert_eq!(joined, vec!["good"]);
        assert_eq!(panicked, vec!["doomed"]);
        assert_eq!(tracker.outstanding(), 0, "nothing leaks, not even failures");
    }
}

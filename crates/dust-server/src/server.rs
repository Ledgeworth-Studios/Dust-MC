//! The lifecycle: ordered start, observed stop, symmetric shutdown.
//!
//! # The shape of a boot
//!
//! ```text
//! run() ─► 1. config.load      read + validate dust.toml (the Phase 0.3 path)
//!          2. world.ensure     create world directories
//!          3. network.bind*    validate/resolve [server].bind — placeholder,
//!                              dust-net will own the real socket later
//!          4. tick.loop        fixed-timestep engine over participants
//!                 ▲                    │
//!                 └── ctrl-C / watchdog-requested stop, checked BETWEEN passes
//!
//! then, in exact reverse:
//!          4. tick.stop        final stats captured
//!          3. network.release  placeholder released
//!          2. world.release    directories left on disk, noted honestly
//!          1. config.release   configuration released
//! ```
//!
//! Three properties are enforced rather than hoped for.
//!
//! **Symmetry.** Every phase whose start lands in the [`Transcript`] gets a
//! stop beside it, newest first, whether shutdown is graceful, interrupted,
//! or caused by a failure. A phase that fails does not get a start entry at
//! all, and only completed phases are torn down — you cannot release what
//! never began.
//!
//! **Between-pass stopping.** The stop flag is an atomic plus a condvar; it
//! never interrupts anyone. The loop checks it between passes, so a tick
//! batch always finishes whole. Worst-case shutdown latency is one batch;
//! that trade is deliberate, because corrupting a half-written world update
//! to save 50 ms is not a trade anyone would sign twice.
//!
//! **No leaked threads.** Every thread spawned here is tracked by name and
//! joined before `run` returns; the report says how many were joined and
//! whether any died. The guarantee has teeth because it is measured.
//!
//! All time — deadlines, grace periods, uptime — reads from the injected
//! [`Clock`](crate::clock::Clock). Production injects a monotonic clock;
//! tests inject a manual one, and the code cannot tell the difference.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{PathBuf, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dust_config::{ConfigError, DustConfig};

use crate::clock::{Clock, ManualClock, MonotonicClock};
use crate::engine::TickEngine;
use crate::histogram::TimingStats;
use crate::logging::{Level, Logger};
use crate::participant::ParticipantSet;
use crate::stop::{
    watch_dog, CondvarParker, Parker, StopHandle, StopState, ThreadTracker,
    WatchdogHarness, WatchdogPolicy,
};
use crate::tasks;

/// Default configuration file, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "dust.toml";
/// Default world directory, relative to the working directory.
///
/// Placeholder until the schema gains a level-name setting; the constant
/// exists so there is exactly one place to change.
pub const DEFAULT_WORLD_DIR: &str = "world";
/// Default watchdog grace after a stop request: ten seconds against whatever
/// clock the server runs on (real seconds under the production clock).
pub const DEFAULT_SHUTDOWN_GRACE_NS: u64 = 10_000_000_000;

/// One named stage of the boot sequence, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    ConfigLoad,
    WorldDirs,
    NetworkBind,
    TickLoop,
}

impl Phase {
    /// Short name used in transcripts and logs.
    pub fn label(self) -> &'static str {
        match self {
            Self::ConfigLoad => "config",
            Self::WorldDirs => "world-dirs",
            Self::NetworkBind => "network",
            Self::TickLoop => "tick-loop",
        }
    }

    /// What tearing this phase down means, for transcripts. The wording is
    /// deliberately honest about what a placeholder does and does not do.
    fn teardown_detail(self, ticks_run: u64) -> String {
        match self {
            Self::ConfigLoad => "configuration released".to_owned(),
            Self::WorldDirs => "directories left on disk".to_owned(),
            Self::NetworkBind => "placeholder released".to_owned(),
            Self::TickLoop => format!("stopped after {ticks_run} tick(s)"),
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Which way a transcript line faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The phase began (and succeeded — failures never get a start line).
    Start,
    /// The phase was torn down.
    Stop,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
        })
    }
}

/// One line of the lifecycle transcript.
///
/// Transcripts exist because ordering claims are cheap to make and expensive
/// to trust. "Shutdown is symmetric" is a sentence; a transcript reading
/// `start config … stop config` is evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub phase: Phase,
    pub direction: Direction,
    /// What actually happened, for humans reading the report.
    pub detail: String,
}

impl fmt::Display for TranscriptEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.direction, self.phase, self.detail)
    }
}

/// Everything that can end a boot badly.
#[derive(Debug)]
pub enum ServerError {
    /// The configuration failed to load or validate.
    Config(ConfigError),
    /// The world directories could not be created.
    WorldDirs { path: PathBuf, source: std::io::Error },
    /// `[server] bind` did not describe a usable address.
    NetworkBind { bind: String, message: String },
    /// A tracked thread died during shutdown.
    ThreadPanic(Vec<String>),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(e) => write!(f, "{e}"),
            Self::WorldDirs { path, source } => {
                write!(f, "could not prepare world directory {path:?}: {source}")
            }
            Self::NetworkBind { bind, message } => {
                write!(f, "[server] bind = {bind:?}: {message}")
            }
            Self::ThreadPanic(names) => {
                write!(f, "thread(s) panicked during shutdown: {}", names.join(", "))
            }
        }
    }
}

impl std::error::Error for ServerError {}

/// Live counters an outside observer can poll while the server runs.
///
/// This is what lets a test say "wait until three ticks have happened"
/// without sleeping, guessing or reaching into internals.
#[derive(Clone)]
pub struct LiveMetrics {
    ticks_observed: Arc<AtomicU64>,
    stop: Arc<StopState>,
}

impl LiveMetrics {
    /// Ticks completed so far, published by the loop between passes.
    pub fn ticks_observed(&self) -> u64 {
        self.ticks_observed.load(Ordering::SeqCst)
    }

    /// Whether anyone has requested a stop.
    pub fn is_stop_requested(&self) -> bool {
        self.stop.is_stopped()
    }
}

impl fmt::Debug for LiveMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveMetrics")
            .field("ticks_observed", &self.ticks_observed())
            .field("is_stop_requested", &self.is_stop_requested())
            .finish()
    }
}

/// How the tick loop's parkers are built, per run.
///
/// Factories rather than instances because each run needs its own parker,
/// two different threads need two of them, and some parkers carry state.
pub type ParkerFactory =
    Arc<dyn Fn(Arc<StopState>, Arc<dyn Clock>) -> Box<dyn Parker> + Send + Sync>;

/// Everything configurable about a server run.
///
/// Defaults describe a normal production boot: monotonic clock, condvar
/// parking, a process-exiting watchdog with [`DEFAULT_SHUTDOWN_GRACE_NS`] of
/// grace, info-level logs on stdout. Tests override pieces individually and
/// leave the rest honest.
pub struct ServerOptions {
    pub config_path: PathBuf,
    pub world_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    /// Builds the parker the tick loop owns.
    pub loop_parker: ParkerFactory,
    /// Builds the parker the watchdog thread owns.
    pub watchdog_parker: ParkerFactory,
    /// `None` disables the watchdog entirely (tests that need fully
    /// deterministic single-stepping do this).
    pub watchdog: Option<WatchdogPolicy>,
    pub logger: Logger,
    /// Participants registered on top of the ones built from configuration.
    pub extra_tasks: Vec<Box<dyn TickParticipant>>,
    /// When set, config-built participants that simulate work charge *this*
    /// clock. It must be the same manual clock as `clock`, otherwise the
    /// measurements are lies. Production leaves it `None`: real work charges
    /// real time without help.
    pub virtual_work_clock: Option<Arc<ManualClock>>,
}

impl fmt::Debug for ServerOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerOptions")
            .field("config_path", &self.config_path)
            .field("world_dir", &self.world_dir)
            .field("watchdog", &self.watchdog.is_some())
            .field("extra_tasks", &self.extra_tasks.len())
            .finish_non_exhaustive()
    }
}

impl Default for ServerOptions {
    fn default() -> Self {
        let clock: Arc<dyn Clock> = Arc::new(MonotonicClock::new());
        let logger = Logger::to_stdout(Level::Info, Arc::clone(&clock));
        let loop_parker: ParkerFactory =
            Arc::new(|state, clock| Box::new(CondvarParker::new(state, clock)));
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_PATH),
            world_dir: PathBuf::from(DEFAULT_WORLD_DIR),
            loop_parker: Arc::clone(&loop_parker),
            watchdog_parker: loop_parker,
            watchdog: Some(WatchdogPolicy::exit_process(DEFAULT_SHUTDOWN_GRACE_NS)),
            logger,
            extra_tasks: Vec::new(),
            virtual_work_clock: None,
            clock,
        }
    }
}

/// Shared mutable state between the server and its helper threads.
struct Shared {
    stop: Arc<StopState>,
    complete: Arc<AtomicBool>,
    ticks_observed: Arc<AtomicU64>,
    watchdog_fired: Arc<AtomicBool>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field(
                "ticks_observed",
                &self.ticks_observed.load(Ordering::SeqCst),
            )
            .finish_non_exhaustive()
    }
}

/// A configured-but-not-yet-run server.
///
/// Construct once, clone the [`StopHandle`] out to whoever should be allowed
/// to stop it, then hand the whole thing to a thread (or run it inline) with
/// [`run`](Server::run).
pub struct Server {
    options: ServerOptions,
    /// Loaded by phase 1, consumed by phase 4. The slot exists so each phase
    /// reads exactly what earlier phases produced, in order.
    config: Option<DustConfig>,
    shared: Shared,
    stop_handle: StopHandle,
    tracker: Arc<ThreadTracker>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Prepare a server around `options`. Construction wires handles together
    /// and touches nothing else — no files are opened until `run`.
    pub fn new(options: ServerOptions) -> Self {
        let shared = Shared {
            stop: Arc::new(StopState::default()),
            complete: Arc::new(AtomicBool::new(false)),
            ticks_observed: Arc::new(AtomicU64::new(0)),
            watchdog_fired: Arc::new(AtomicBool::new(false)),
        };
        let stop_handle = StopHandle::new(Arc::clone(&shared.stop));
        Self {
            options,
            config: None,
            shared,
            stop_handle,
            tracker: Arc::new(ThreadTracker::default()),
        }
    }

    /// The handle a signal handler (or a test playing one) uses to request
    /// shutdown.
    pub fn stop_handle(&self) -> StopHandle {
        self.stop_handle.clone()
    }

    /// Counters an outside thread can poll while `run` is in flight.
    pub fn metrics(&self) -> LiveMetrics {
        LiveMetrics {
            ticks_observed: Arc::clone(&self.shared.ticks_observed),
            stop: Arc::clone(&self.shared.stop),
        }
    }

    /// Execute the full lifecycle, blocking until shutdown completes.
    ///
    /// `Ok(report)` means the lifecycle ran through to the end — inspect the
    /// report for whether it was clean. `Err` means startup failed and every
    /// phase that had completed was unwound in reverse before returning.
    pub fn run(mut self) -> Result<ShutdownReport, ServerError> {
        let mut transcript: Vec<TranscriptEntry> = Vec::new();
        let mut completed: Vec<Phase> = Vec::new();
        let started_at = self.options.clock.now_ns();

        // ---- Phase 1: configuration ------------------------------------
        match self.start_config_load(&mut transcript) {
            Ok(()) => {}
            Err(e) => return Err(e), // nothing has started yet; nothing to undo
        }
        completed.push(Phase::ConfigLoad);
        if self.shared.stop.is_stopped() {
            return Ok(self.abort_startup(completed, &mut transcript, started_at));
        }

        // ---- Phase 2: world directories --------------------------------
        if let Err(e) = self.start_world_dirs(&mut transcript) {
            self.teardown(completed, &mut transcript, 0);
            return Err(e);
        }
        completed.push(Phase::WorldDirs);
        if self.shared.stop.is_stopped() {
            return Ok(self.abort_startup(completed, &mut transcript, started_at));
        }

        // ---- Phase 3: network placeholder ------------------------------
        if let Err(e) = self.start_network_placeholder(&mut transcript) {
            self.teardown(completed, &mut transcript, 0);
            return Err(e);
        }
        completed.push(Phase::NetworkBind);
        if self.shared.stop.is_stopped() {
            return Ok(self.abort_startup(completed, &mut transcript, started_at));
        }

        // ---- Phase 4: the tick loop ------------------------------------
        let config = self.config.take().expect("config staged by phase 1");
        let mut participants = tasks::registry_from_config(
            &config,
            &tasks::WorkCharger::from_option(self.options.virtual_work_clock.clone()),
        );
        for extra in std::mem::take(&mut self.options.extra_tasks) {
            participants.insert(extra);
        }
        let participant_names = participants.names();
        transcript.push(TranscriptEntry {
            phase: Phase::TickLoop,
            direction: Direction::Start,
            detail: format!("{} participant(s)", participant_names.len()),
        });

        // From here on the watchdog watches: armed by the stop request, it
        // enforces the deadline across the loop and the whole teardown.
        let policy = self.options.watchdog.take();
        if let Some(policy) = &policy {
            let harness = WatchdogHarness {
                stop: Arc::clone(&self.shared.stop),
                complete: Arc::clone(&self.shared.complete),
                fired: Arc::clone(&self.shared.watchdog_fired),
                ticks_run: Arc::clone(&self.shared.ticks_observed),
                clock: Arc::clone(&self.options.clock),
                parker: (self.options.watchdog_parker)(
                    Arc::clone(&self.shared.stop),
                    Arc::clone(&self.options.clock),
                ),
                policy: policy.clone(),
            };
            let tracker = Arc::clone(&self.tracker);
            tracker.spawn("dust-watchdog", move || watch_dog(harness));
        }

        let summary = self.run_tick_loop(&mut participants);
        completed.push(Phase::TickLoop);

        // ---- Shutdown: exact reverse of everything completed ------------
        self.teardown(completed, &mut transcript, summary.ticks_run);

        // Release the watchdog first so a not-yet-fired one retires quietly;
        // one that already fired has already done its worst.
        self.shared.complete.store(true, Ordering::SeqCst);
        let (joined, panicked) = self.tracker.join_all();

        Ok(ShutdownReport {
            interrupted: false,
            ticks_run: summary.ticks_run,
            uptime_ns: self.options.clock.now_ns().saturating_sub(started_at),
            surrendered_batches: summary.surrendered_batches,
            overall_timing: summary.overall_timing,
            participant_timing: summary.participant_timing,
            participants: participant_names,
            transcript,
            watchdog_fired: self.shared.watchdog_fired.load(Ordering::SeqCst),
            threads_joined: joined.len(),
            thread_panics: panicked,
        })
    }

    // ---- phases ----------------------------------------------------------

    fn start_config_load(
        &mut self,
        transcript: &mut Vec<TranscriptEntry>,
    ) -> Result<(), ServerError> {
        match DustConfig::load(&self.options.config_path) {
            Ok(config) => {
                let warnings = config
                    .check()
                    .into_iter()
                    .filter(|f| f.severity == dust_config::Severity::Warning)
                    .count();
                let origin = self.options.config_path.display();
                transcript.push(TranscriptEntry {
                    phase: Phase::ConfigLoad,
                    direction: Direction::Start,
                    detail: format!(
                        "loaded {origin} ({warnings} warning(s)); defaults applied \
                         for anything unset"
                    ),
                });
                self.config = Some(config);
                Ok(())
            }
            Err(e) => {
                self.options.logger.error("dust::server", format!("{e}"));
                Err(ServerError::Config(e))
            }
        }
    }

    fn start_world_dirs(
        &self,
        transcript: &mut Vec<TranscriptEntry>,
    ) -> Result<(), ServerError> {
        let dir = self.options.world_dir.clone();
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                transcript.push(TranscriptEntry {
                    phase: Phase::WorldDirs,
                    direction: Direction::Start,
                    detail: format!("{} ready", dir.display()),
                });
                Ok(())
            }
            Err(source) => {
                self.options.logger.error(
                    "dust::server",
                    format!("world directory {} failed: {source}", dir.display()),
                );
                Err(ServerError::WorldDirs { path: dir, source })
            }
        }
    }

    /// Validate and resolve `[server] bind`. No socket is opened: binding is
    /// dust-net's job, and doing it twice teaches the wrong lesson. What the
    /// placeholder proves today is that a bad bind stops the boot here,
    /// cleanly, with an error naming the setting.
    fn start_network_placeholder(
        &self,
        transcript: &mut Vec<TranscriptEntry>,
    ) -> Result<(), ServerError> {
        let bind = self
            .config
            .as_ref()
            .expect("config staged before network phase")
            .server
            .bind
            .clone();
        match resolve_bind(&bind) {
            Ok(addr) => {
                transcript.push(TranscriptEntry {
                    phase: Phase::NetworkBind,
                    direction: Direction::Start,
                    detail: format!("{bind} resolves to {addr}; binding deferred"),
                });
                Ok(())
            }
            Err(message) => {
                let err = ServerError::NetworkBind { bind: bind.clone(), message };
                self.options.logger.error("dust::server", format!("{err}"));
                Err(err)
            }
        }
    }

    /// The hot loop. Checks stop **between** passes; a batch in flight always
    /// finishes whole.
    fn run_tick_loop(&self, participants: &mut ParticipantSet) -> EngineSummary {
        let parker = (self.options.loop_parker)(
            Arc::clone(&self.shared.stop),
            Arc::clone(&self.options.clock),
        );
        let mut engine = TickEngine::new(Arc::clone(&self.options.clock));
        while !self.shared.stop.is_stopped() {
            engine.advance(participants, &self.options.logger);
            self.shared.ticks_observed.store(engine.ticks_run(), Ordering::SeqCst);
            if self.shared.stop.is_stopped() {
                break;
            }
            if let Some(deadline) = engine.next_deadline() {
                parker.park_until(deadline);
            }
        }
        EngineSummary {
            ticks_run: engine.ticks_run(),
            surrendered_batches: engine.surrendered_batches(),
            overall_timing: engine.overall_timing(),
            participant_timing: engine
                .accounted_participants()
                .into_iter()
                .filter_map(|name| {
                    engine.participant_timing(&name).map(|stats| (name, stats))
                })
                .collect(),
        }
    }

    // ---- teardown --------------------------------------------------------

    /// Push stop entries for everything in `completed`, newest first.
    ///
    /// This is the symmetric-shutdown guarantee in six lines, which is why it
    /// takes the completed list rather than re-deriving it: one list, one
    /// order, no archaeology.
    fn teardown(
        &self,
        completed: Vec<Phase>,
        transcript: &mut Vec<TranscriptEntry>,
        ticks_run: u64,
    ) {
        for phase in completed.into_iter().rev() {
            transcript.push(TranscriptEntry {
                phase,
                direction: Direction::Stop,
                detail: phase.teardown_detail(ticks_run),
            });
        }
    }

    /// Graceful abort: stop arrived during startup. Everything completed is
    /// torn down in reverse and the report says so.
    fn abort_startup(
        self,
        completed: Vec<Phase>,
        transcript: &mut Vec<TranscriptEntry>,
        started_at: u64,
    ) -> ShutdownReport {
        // No helper threads exist on this path: the watchdog spawns only
        // once the tick loop begins.
        self.teardown(completed, transcript, 0);
        ShutdownReport {
            interrupted: true,
            ticks_run: 0,
            uptime_ns: self.options.clock.now_ns().saturating_sub(started_at),
            surrendered_batches: 0,
            overall_timing: TimingStats::default(),
            participant_timing: BTreeMap::new(),
            participants: Vec::new(),
            transcript,
            watchdog_fired: false,
            threads_joined: 0,
            thread_panics: Vec::new(),
        }
    }
}

/// Resolve a `host:port` bind string to the address the real listener will
/// take.
///
/// IP literals take the fast path; hostnames go through the platform
/// resolver. An empty resolution is an error, not a shrug: a bind that
/// resolves to nothing must stop the boot now rather than at first listen.
pub fn resolve_bind(bind: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let (host, port) = bind
        .rsplit_once(':')
        .ok_or_else(|| format!("expected host:port, got {bind:?} with no port"))?;
    let port: u16 =
        port.parse().map_err(|_| format!("port {port:?} is not a u16"))?;
    (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve host {host:?}: {e}"))?
        .next()
        .ok_or_else(|| format!("host {host:?} resolved to no addresses"))
}

/// Summary of the finished loop, captured exactly where the numbers stop
/// being live.
struct EngineSummary {
    ticks_run: u64,
    surrendered_batches: u64,
    overall_timing: TimingStats,
    participant_timing: BTreeMap<String, TimingStats>,
}

/// What a completed run hands back.
///
/// Everything an integration test — or an operator's post-mortem — wants:
/// what ran, in what order, how many ticks, what the timing looked like,
/// whether the watchdog had to intervene and whether any thread died.
#[derive(Debug)]
pub struct ShutdownReport {
    /// Stop arrived during startup phases, before any tick ran.
    pub interrupted: bool,
    pub ticks_run: u64,
    pub uptime_ns: u64,
    /// Bursts that hit the catch-up cap and resynchronised.
    pub surrendered_batches: u64,
    pub overall_timing: TimingStats,
    pub participant_timing: BTreeMap<String, TimingStats>,
    /// Participant names in execution order.
    pub participants: Vec<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub watchdog_fired: bool,
    pub threads_joined: usize,
    pub thread_panics: Vec<String>,
}

impl ShutdownReport {
    /// Whether the run ended the way a clean run should: uninterrupted, all
    /// threads retired, nobody panicking.
    pub fn is_clean(&self) -> bool {
        !self.interrupted && self.thread_panics.is_empty()
    }

    /// Transcript lines as `(phase, direction)` pairs, ready to compare.
    pub fn transcript_pairs(&self) -> Vec<(Phase, Direction)> {
        self.transcript
            .iter()
            .map(|e| (e.phase, e.direction))
            .collect()
    }
}

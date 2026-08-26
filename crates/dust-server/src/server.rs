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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use dust_config::model::LogLevel;
use dust_config::{ConfigError, DustConfig};

use crate::clock::{Clock, ManualClock, MonotonicClock};
use crate::engine::TickEngine;
use crate::histogram::TimingStats;
use crate::logging::{Level, Logger};
use crate::participant::{ParticipantSet, TickParticipant};
use crate::stop::{
    watch_dog, CondvarParker, Parker, StopHandle, StopState, ThreadTracker, WatchdogHarness,
    WatchdogPolicy,
};
use crate::tasks;

/// Default configuration file, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "dust.toml";
/// Default world directory, relative to the working directory.
///
/// Placeholder until the schema gains a level-name setting; the constant
/// exists so there is exactly one place to change.
pub const DEFAULT_WORLD_DIR: &str = "world";

/// The runtime settings phases 4 and 5 consume, extracted from the loaded
/// configuration in one place.
///
/// Phase 1 loads the file; everything downstream reads these numbers instead
/// of reaching back into the config tree. One extraction point means the
/// mapping from setting to behaviour is written once and testable alone.
#[derive(Debug, Clone, Copy)]
struct RuntimeSettings {
    /// `[server].max_catchup_ticks`, verbatim.
    catchup_cap: u32,
    /// `[server].shutdown_timeout_secs`, converted to nanoseconds on whatever
    /// clock the run uses.
    shutdown_grace_ns: u64,
}

impl RuntimeSettings {
    fn from_config(config: &DustConfig) -> Self {
        Self {
            catchup_cap: config.server.max_catchup_ticks,
            shutdown_grace_ns: u64::from(config.server.shutdown_timeout_secs) * 1_000_000_000,
        }
    }
}

/// Map a configured log level onto the logger's severity scale.
fn log_level_of(level: LogLevel) -> Level {
    match level {
        LogLevel::Error => Level::Error,
        LogLevel::Warn => Level::Warn,
        LogLevel::Info => Level::Info,
        LogLevel::Debug => Level::Debug,
        LogLevel::Trace => Level::Trace,
    }
}

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
            // Overridden by the teardown, which knows the address and the
            // counters. This is the wording for a caller that unwinds a phase
            // list without a listener in hand.
            Self::NetworkBind => "listener released".to_owned(),
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
///
/// The phase failures carry the transcript as it stood when they fired, so a
/// caller can show (or a test can assert) exactly which phases ran and were
/// unwound. Configuration failure carries none because configuration is the
/// first phase: nothing has started, so nothing can have been unwound.
#[derive(Debug)]
pub enum ServerError {
    /// The configuration failed to load or validate.
    Config(ConfigError),
    /// The world directories could not be created.
    WorldDirs {
        path: PathBuf,
        source: std::io::Error,
        transcript: Vec<TranscriptEntry>,
    },
    /// `[server] bind` did not describe a usable address.
    NetworkBind {
        bind: String,
        message: String,
        transcript: Vec<TranscriptEntry>,
    },
    /// A tracked thread died during shutdown.
    ThreadPanic(Vec<String>),
}

impl ServerError {
    /// The boot transcript at the moment of failure: every phase that had
    /// started, plus the stop entries written while unwinding it.
    pub fn transcript(&self) -> &[TranscriptEntry] {
        match self {
            Self::Config(_) | Self::ThreadPanic(_) => &[],
            Self::WorldDirs { transcript, .. } | Self::NetworkBind { transcript, .. } => transcript,
        }
    }

    /// Attach the boot transcript to a phase failure. Called by [`Server::run`]
    /// *after* unwinding, so the error carries the stops as well as the starts.
    fn attach(&mut self, transcript: Vec<TranscriptEntry>) {
        match self {
            Self::Config(_) | Self::ThreadPanic(_) => {}
            Self::WorldDirs {
                transcript: slot, ..
            }
            | Self::NetworkBind {
                transcript: slot, ..
            } => *slot = transcript,
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(e) => write!(f, "{e}"),
            Self::WorldDirs { path, source, .. } => {
                write!(f, "could not prepare world directory {path:?}: {source}")
            }
            Self::NetworkBind { bind, message, .. } => {
                write!(f, "[server] bind = {bind:?}: {message}")
            }
            Self::ThreadPanic(names) => {
                write!(
                    f,
                    "thread(s) panicked during shutdown: {}",
                    names.join(", ")
                )
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
    bound: Arc<OnceLock<SocketAddr>>,
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

    /// The address the listener actually took, once phase 3 has completed.
    ///
    /// Not the same as `[server].bind`, and the difference is the reason this
    /// exists: `0.0.0.0:0` and `127.0.0.1:0` both mean "any free port", and the
    /// number the operating system chose is knowable only after the bind. An
    /// operator wants it in the log and a test wants it to connect to; both
    /// would otherwise have to parse it back out of a log line.
    ///
    /// A `OnceLock` because a listener binds once per run and never rebinds, so
    /// "not yet" and "never" are the same answer and neither is a lock anybody
    /// contends for.
    pub fn bound_addr(&self) -> Option<SocketAddr> {
        self.bound.get().copied()
    }
}

impl fmt::Debug for LiveMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveMetrics")
            .field("ticks_observed", &self.ticks_observed())
            .field("is_stop_requested", &self.is_stop_requested())
            .field("bound_addr", &self.bound_addr())
            .finish()
    }
}

/// How the tick loop's parkers are built, per run.
///
/// Factories rather than instances because each run needs its own parker,
/// two different threads need two of them, and some parkers carry state.
pub type ParkerFactory =
    Arc<dyn Fn(Arc<StopState>, Arc<dyn Clock>) -> Box<dyn Parker> + Send + Sync>;

/// What the watchdog thread should be, per run.
///
/// The default is [`WatchdogSetting::FromConfig`]: the grace period is
/// `[server].shutdown_timeout_secs` from the loaded file, which is how that
/// setting reaches a thread that starts after configuration has been read.
/// Tests and embedded hosts either name an explicit policy or switch the
/// watchdog off entirely.
#[derive(Clone, Debug, Default)]
pub enum WatchdogSetting {
    /// Build the policy from the loaded configuration's timeout.
    #[default]
    FromConfig,
    /// No watchdog. Nothing enforces the shutdown deadline; only do this when
    /// something else owns it.
    Disabled,
    /// An explicit policy, overriding whatever the file says.
    Custom(WatchdogPolicy),
}

/// Everything configurable about a server run.
///
/// Defaults describe a normal production boot: monotonic clock, condvar
/// parking, a config-driven process-exiting watchdog, info-level logs on
/// stdout until the file says otherwise. Tests override pieces individually
/// and leave the rest honest.
pub struct ServerOptions {
    pub config_path: PathBuf,
    pub world_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    /// Builds the parker the tick loop owns.
    pub loop_parker: ParkerFactory,
    /// Builds the parker the watchdog thread owns.
    pub watchdog_parker: ParkerFactory,
    /// What watchdog to run across the tick loop and teardown.
    pub watchdog: WatchdogSetting,
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
            .field(
                "watchdog",
                match &self.watchdog {
                    WatchdogSetting::Disabled => &"disabled",
                    WatchdogSetting::FromConfig => &"from-config",
                    WatchdogSetting::Custom(_) => &"custom",
                },
            )
            .field("extra_tasks", &self.extra_tasks.len())
            .finish_non_exhaustive()
    }
}

impl Default for ServerOptions {
    fn default() -> Self {
        let clock: Arc<dyn Clock> = Arc::new(MonotonicClock::new());
        // Anchored once, here: the monotonic clock reads from process start,
        // and log lines should show the calendar, not the uptime.
        let logger = Logger::to_stdout(Level::Info, Arc::clone(&clock)).anchored_to_unix_now();
        let loop_parker: ParkerFactory =
            Arc::new(|state, clock| Box::new(CondvarParker::new(state, clock)));
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_PATH),
            world_dir: PathBuf::from(DEFAULT_WORLD_DIR),
            loop_parker: Arc::clone(&loop_parker),
            watchdog_parker: loop_parker,
            watchdog: WatchdogSetting::default(),
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
    /// Published by phase 3 the moment the socket is taken. See
    /// [`LiveMetrics::bound_addr`].
    bound: Arc<OnceLock<SocketAddr>>,
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
    /// Set by phase 3, released by the teardown at phase 3's position. Held on
    /// the server rather than in a local so that the failure paths and the
    /// happy path release it through the same code.
    listener: Option<crate::net::ListenerHandle>,
    /// The world and the player positions, kept so the teardown can write them
    /// down. `None` until phase 3 builds them.
    saveable: Option<Saveable>,
}

/// What the teardown has to write out.
struct Saveable {
    world: crate::net::SharedWorld,
    positions: crate::net::save::SharedPositions,
    world_dir: PathBuf,
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
            bound: Arc::new(OnceLock::new()),
        };
        let stop_handle = StopHandle::new(Arc::clone(&shared.stop));
        Self {
            options,
            config: None,
            shared,
            stop_handle,
            tracker: Arc::new(ThreadTracker::default()),
            listener: None,
            saveable: None,
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
            bound: Arc::clone(&self.shared.bound),
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

        // Configuration is in; everything downstream takes its numbers from
        // it. The log filter applies from here on — phases 2 and later speak
        // at the loudness the file asked for.
        let staged = self.config.as_ref().expect("config staged by phase 1");
        let runtime = RuntimeSettings::from_config(staged);
        self.options.logger = self
            .options
            .logger
            .with_filter(log_level_of(staged.server.log_level));
        if self.shared.stop.is_stopped() {
            let transcript = std::mem::take(&mut transcript);
            return Ok(self.abort_startup(completed, transcript, started_at));
        }

        // ---- Phase 2: world directories --------------------------------
        if let Err(mut e) = self.start_world_dirs(&mut transcript) {
            let mut listener = self.listener.take();
            let mut saveable = self.saveable.take();
            self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
            e.attach(std::mem::take(&mut transcript));
            return Err(e);
        }
        completed.push(Phase::WorldDirs);
        if self.shared.stop.is_stopped() {
            {
                let transcript = std::mem::take(&mut transcript);
                return Ok(self.abort_startup(completed, transcript, started_at));
            }
        }

        // ---- Phase 3: bind and serve -----------------------------------
        if let Err(mut e) = self.start_network(&mut transcript) {
            let mut listener = self.listener.take();
            let mut saveable = self.saveable.take();
            self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
            e.attach(std::mem::take(&mut transcript));
            return Err(e);
        }
        completed.push(Phase::NetworkBind);
        if self.shared.stop.is_stopped() {
            {
                let transcript = std::mem::take(&mut transcript);
                return Ok(self.abort_startup(completed, transcript, started_at));
            }
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
        // enforces the deadline across the loop and the whole teardown. The
        // grace period comes from configuration unless the run was given an
        // explicit policy or told to go without.
        let policy = match &self.options.watchdog {
            WatchdogSetting::Disabled => None,
            WatchdogSetting::Custom(policy) => Some(policy.clone()),
            WatchdogSetting::FromConfig => {
                Some(WatchdogPolicy::exit_process(runtime.shutdown_grace_ns))
            }
        };
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

        let summary = self.run_tick_loop(&mut participants, runtime.catchup_cap);
        completed.push(Phase::TickLoop);

        // ---- Shutdown: exact reverse of everything completed ------------
        let mut listener = self.listener.take();
        let mut saveable = self.saveable.take();
        self.teardown(
            completed,
            &mut transcript,
            summary.ticks_run,
            &mut listener,
            &mut saveable,
        );

        // Release the watchdog first so a not-yet-fired one retires quietly;
        // one that already fired has already done its worst. The extra wake
        // matters under virtual time: nothing else moves the clock after the
        // loop exits, so without this the watcher would sleep out its whole
        // grace period on a clock that will never reach it.
        self.shared.complete.store(true, Ordering::SeqCst);
        self.shared.stop.broadcast_stop();
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
            shutdown_grace_ns: policy.map(|_| runtime.shutdown_grace_ns),
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
                let server = &config.server;
                transcript.push(TranscriptEntry {
                    phase: Phase::ConfigLoad,
                    direction: Direction::Start,
                    detail: format!(
                        "loaded {origin} ({warnings} warning(s)); defaults applied \
                         for anything unset; catch-up capped at {} tick(s), \
                         shutdown grace {}s, logs at {}, bind {}",
                        server.max_catchup_ticks,
                        server.shutdown_timeout_secs,
                        server.log_level,
                        server.bind,
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

    fn start_world_dirs(&self, transcript: &mut Vec<TranscriptEntry>) -> Result<(), ServerError> {
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
                Err(ServerError::WorldDirs {
                    path: dir,
                    source,
                    transcript: Vec::new(),
                })
            }
        }
    }

    /// Resolve `[server] bind`, take the port, and start serving on it.
    ///
    /// The socket is bound **here**, synchronously, inside the ordered boot —
    /// not inside the task that accepts on it. That is the whole point of this
    /// phase existing where it does. A port already in use, an address the
    /// machine does not have, a privileged port without the privilege: each is
    /// an error that stops the boot with the setting named, the earlier phases
    /// unwound in reverse, and a non-zero exit. Bound from inside a spawned
    /// task instead, every one of them would produce a server that started
    /// cleanly, ticked forever and answered nothing.
    ///
    /// The favicon is read here too, for the same reason and one more: a client
    /// shows *nothing* for a picture it cannot use, which is indistinguishable
    /// from a server that set none. An operator who points the setting at the
    /// wrong file has to be told, and boot is the only moment anybody is
    /// listening.
    fn start_network(&mut self, transcript: &mut Vec<TranscriptEntry>) -> Result<(), ServerError> {
        let config = self
            .config
            .as_ref()
            .expect("config staged before network phase");
        let bind = config.server.bind.clone();
        let motd = config.server.motd.clone();
        let max_players = config.server.max_players;
        let favicon_path = config.server.favicon.clone();
        let online_mode = config.server.online_mode;

        let fail = |message: String| -> ServerError {
            ServerError::NetworkBind {
                bind: bind.clone(),
                message,
                transcript: Vec::new(),
            }
        };

        let addr = resolve_bind(&bind).map_err(&fail)?;

        let favicon = if favicon_path.is_empty() {
            None
        } else {
            let icon = crate::net::Favicon::load(std::path::Path::new(&favicon_path))
                .map_err(|e| fail(e.to_string()))?;
            Some(icon)
        };

        // The one version this server speaks. It is resolved by name from the
        // generated table rather than written as a constant, so that the day
        // there are two, this line is where the choice becomes visible instead
        // of being spread through the code that assumed one.
        let version = dust_protocol::version::V1_21_1;

        // Both of these are done *before* the socket exists, because both can
        // fail and a failure after the bind leaves a port taken by a server
        // that is about to stop. Key generation in particular is slow and
        // unbounded — RSA prime search has no worst case — which is exactly
        // why it happens once here and never on a login.
        let authority = if online_mode {
            let transport = dust_net::session::TlsTransport::mojang().map_err(|e| {
                fail(format!(
                    "online mode needs to reach Mojang's session server and could not: {e}. \
                     Set [server] online_mode = false to run without verification, knowing \
                     that anyone may then join under any name"
                ))
            })?;
            let key = dust_net::login::ServerKey::generate()
                .map_err(|e| fail(format!("online mode needs a server key pair: {e}")))?;
            crate::net::Authority::Online {
                session: std::sync::Arc::new(dust_net::session::HttpSessionServer::new(transport)),
                key: std::sync::Arc::new(key),
            }
        } else {
            self.options.logger.warn(
                "dust::server",
                "[server] online_mode = false: nobody is verified and anyone may join \
                 under any name. Safe only behind a proxy that checks for you.",
            );
            crate::net::Authority::Offline
        };

        // The world, and the two registry positions the join packet quotes.
        // Both are looked up in the same synced tables the configuration state
        // sends, because an id here is a *position in what the client was
        // told* — a constant would be a second answer to a question the sync
        // already answers, and the two would disagree the day a registry gains
        // an entry.
        let palette = crate::net::world::Palette::resolve().map_err(|e| fail(e.to_string()))?;
        let biomes = dust_registry::synced::by_name("minecraft:worldgen/biome")
            .ok_or_else(|| fail("the synced registries have no biome registry".to_owned()))?;
        let plains = biomes.id_of("minecraft:plains").ok_or_else(|| {
            fail("the biome registry has no minecraft:plains to build a flat world from".to_owned())
        })? as u32;
        let dimension_types = dust_registry::synced::by_name("minecraft:dimension_type")
            .ok_or_else(|| fail("the synced registries have no dimension types".to_owned()))?;
        let overworld = dimension_types
            .id_of("minecraft:overworld")
            .ok_or_else(|| fail("the dimension types have no overworld".to_owned()))?
            as u32;
        let world = std::sync::Arc::new(crate::net::edits::EditedWorld::new(
            crate::net::world::FlatWorld::new(palette, plains, biomes.entries.len() as u32),
        ));

        // What players changed last time, and where they were standing. A
        // world that has never been played has no file and that is not an
        // error; a file this build cannot read *is* one, because starting with
        // an empty world beside a save that exists would quietly discard it on
        // the next write.
        let world_dir = self.options.world_dir.clone();
        let positions: crate::net::save::SharedPositions = std::sync::Arc::default();
        match crate::net::save::load(&world_dir) {
            Ok(Some(saved)) => {
                let (blocks, unknown) = crate::net::save::resolve(&saved.blocks);
                let applied = world.restore(blocks);
                *positions.lock().expect("not poisoned") = crate::net::save::positions(&saved);
                self.options.logger.info(
                    "dust::server",
                    format!(
                        "restored {applied} block change(s) and {} player position(s)",
                        saved.players.len()
                    ),
                );
                if !unknown.is_empty() {
                    // Named, not counted. An operator who renamed a block or
                    // changed Minecraft version needs to know *which* block
                    // stopped existing, and a number tells them only that
                    // something did.
                    self.options.logger.warn(
                        "dust::server",
                        format!(
                            "the save names {} block(s) this build has no entry for, and they \
                             were dropped: {}",
                            unknown.len(),
                            unknown.join(", ")
                        ),
                    );
                }
            }
            Ok(None) => {}
            Err(e) => return Err(fail(format!("{e}"))),
        }

        // Shared between the accept loop and every session on it: the accept
        // loop counts connections, and the sessions count the players inside
        // them, because only a session knows when somebody has actually
        // arrived.
        let counters = std::sync::Arc::new(crate::net::Counters::default());

        let listener = crate::net::Listener::bind(addr).map_err(|e| fail(e.to_string()))?;
        let bound = listener.addr();

        let ctx = std::sync::Arc::new(crate::net::SessionContext {
            version,
            status: crate::net::StatusPolicy::new(version, motd, max_players, favicon),
            conn: dust_net::io::ConnConfig::default(),
            auth: authority,
            world: std::sync::Arc::clone(&world),
            view_distance: VIEW_DISTANCE,
            overworld_dimension_type: overworld,
            blocks: crate::net::PlaceableBlocks {
                air: palette.air,
                placeable: palette.grass,
            },
            logger: self.options.logger.clone(),
            positions: std::sync::Arc::clone(&positions),
            counters: std::sync::Arc::clone(&counters),
        });

        let handle = listener
            .serve(ctx, counters, self.options.logger.clone())
            .map_err(|e| fail(format!("could not start serving: {e}")))?;

        transcript.push(TranscriptEntry {
            phase: Phase::NetworkBind,
            direction: Direction::Start,
            detail: format!(
                "listening on {bound} for protocol {} ({}), {} mode",
                version.number(),
                version.name(),
                if online_mode { "online" } else { "offline" }
            ),
        });
        // Published only after the handle exists, so an observer that sees an
        // address knows there is something accepting on it.
        let _ = self.shared.bound.set(bound);
        self.listener = Some(handle);
        self.saveable = Some(Saveable {
            world: std::sync::Arc::clone(&world),
            positions: std::sync::Arc::clone(&positions),
            world_dir,
        });
        Ok(())
    }

    /// The hot loop. Checks stop **between** passes; a batch in flight always
    /// finishes whole.
    ///
    /// `catchup_cap` is the per-burst repayment allowance, from
    /// `[server].max_catchup_ticks`; it is an argument rather than a field so
    /// the loop cannot run without the configuration phase having supplied it.
    fn run_tick_loop(&self, participants: &mut ParticipantSet, catchup_cap: u32) -> EngineSummary {
        let parker = (self.options.loop_parker)(
            Arc::clone(&self.shared.stop),
            Arc::clone(&self.options.clock),
        );
        let mut engine =
            TickEngine::new(Arc::clone(&self.options.clock)).with_catchup_cap(catchup_cap);
        while !self.shared.stop.is_stopped() {
            engine.advance(participants, &self.options.logger);
            self.shared
                .ticks_observed
                .store(engine.ticks_run(), Ordering::SeqCst);
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
                .filter_map(|name| engine.participant_timing(&name).map(|stats| (name, stats)))
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
        listener: &mut Option<crate::net::ListenerHandle>,
        saveable: &mut Option<Saveable>,
    ) {
        for phase in completed.into_iter().rev() {
            // The listener is released *at the position the transcript claims
            // it is*, rather than wherever the handle happens to go out of
            // scope. Symmetry that is only true of the log is not symmetry.
            let detail = if phase == Phase::NetworkBind {
                match listener.take() {
                    Some(handle) => {
                        let addr = handle.addr();
                        let stats = handle.stats();
                        handle.shutdown();
                        format!(
                            "released {addr} after {} connection(s): {} ping(s), \
                             {} login(s), {} login(s) refused, {} failed, \
                             {} still online",
                            stats.accepted,
                            stats.status_served,
                            stats.logins,
                            stats.logins_failed,
                            stats.failed,
                            stats.online
                        )
                    }
                    // The bind failed, so the phase never completed and this
                    // arm is unreachable from `run`. Written honestly rather
                    // than as an unwrap, because a future caller could unwind a
                    // list this function did not build.
                    None => "nothing was listening".to_owned(),
                }
            } else if phase == Phase::WorldDirs {
                // The world is written *here*, after the listener is released
                // and so after every session has stopped changing it. Writing
                // it while connections were still live would save a world that
                // was still moving, and the last edit would be the one lost.
                self.save_world(saveable.take())
            } else {
                phase.teardown_detail(ticks_run)
            };
            transcript.push(TranscriptEntry {
                phase,
                direction: Direction::Stop,
                detail,
            });
        }
    }

    /// Write the world down, and say what happened either way.
    ///
    /// A failure is reported into the transcript rather than returned, because
    /// this runs during teardown and there is nothing left to abort. It is
    /// still loud: an operator whose disk filled needs to know the world did
    /// not survive, and a silent failure here is the one that is discovered by
    /// the blocks being gone.
    fn save_world(&self, saveable: Option<Saveable>) -> String {
        let Some(saveable) = saveable else {
            // The bind failed, so the phase never completed and nothing was
            // ever built to save.
            return "directories left on disk".to_owned();
        };

        let blocks: Vec<crate::net::save::SavedBlock> = saveable
            .world
            .snapshot()
            .into_iter()
            .filter_map(|(position, state)| {
                crate::net::save::name_of(state).map(|block| crate::net::save::SavedBlock {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                    block: block.to_owned(),
                })
            })
            .collect();
        let players: Vec<crate::net::save::SavedPlayer> = {
            let held = saveable.positions.lock().expect("not poisoned");
            let mut players: Vec<_> = held
                .iter()
                .map(|(id, (x, y, z))| crate::net::save::SavedPlayer {
                    id: id.clone(),
                    x: *x,
                    y: *y,
                    z: *z,
                })
                .collect();
            // Ordered, so two saves of one world are the same file.
            players.sort_by(|a, b| a.id.cmp(&b.id));
            players
        };

        let counts = format!(
            "{} block change(s) and {} player position(s)",
            blocks.len(),
            players.len()
        );
        let save = crate::net::save::Save {
            version: crate::net::save::SAVE_VERSION,
            blocks,
            players,
        };
        match crate::net::save::store(&saveable.world_dir, &save) {
            Ok(()) => format!("saved {counts}"),
            Err(e) => {
                self.options
                    .logger
                    .error("dust::server", format!("the world could not be saved: {e}"));
                format!("FAILED to save {counts}: {e}")
            }
        }
    }

    /// Graceful abort: stop arrived during startup. Everything completed is
    /// torn down in reverse and the report says so.
    fn abort_startup(
        mut self,
        completed: Vec<Phase>,
        mut transcript: Vec<TranscriptEntry>,
        started_at: u64,
    ) -> ShutdownReport {
        // No helper threads exist on this path: the watchdog spawns only
        // once the tick loop begins.
        let mut listener = self.listener.take();
        let mut saveable = self.saveable.take();
        self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
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
            shutdown_grace_ns: None,
            threads_joined: 0,
            thread_panics: Vec::new(),
        }
    }
}

/// How many columns out from a joining player are streamed.
///
/// Two, which is twenty-five columns: enough to stand on and look at, small
/// enough that a join is one burst rather than a stream. It is not
/// `[server].view_distance` because there is no such setting yet — adding one
/// before there is a streaming loop to honour it would be a knob that lies.
const VIEW_DISTANCE: u32 = 2;

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
    let port: u16 = port
        .parse()
        .map_err(|_| format!("port {port:?} is not a u16"))?;
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
    /// The grace period the watchdog ran with, in nanoseconds — `None` when
    /// the run went without one. Recorded because "the timeout came from the
    /// file" is a claim a report can carry and a reader can check.
    pub shutdown_grace_ns: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TICK_NS;
    use crate::stop::StepParker;
    use std::io::Write;
    use std::sync::Mutex;

    /// A unique temp file per call: tests run in parallel in one process, and
    /// two of them sharing a config path would share a fate.
    fn write_config(text: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dust-server-test-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::write(&path, text).expect("write the temp config");
        path
    }

    /// Bytes collected from the logger, shareable with the test thread.
    #[derive(Clone, Default)]
    struct SinkBytes(Arc<Mutex<Vec<u8>>>);

    impl Write for SinkBytes {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A parker factory whose parks advance virtual time by `step_ns`, so a
    /// full lifecycle costs no real time.
    fn stepping(clock: Arc<ManualClock>, step_ns: u64) -> ParkerFactory {
        Arc::new(move |_state, _clock| {
            Box::new(StepParker::new(clock.clone(), step_ns)) as Box<dyn Parker>
        })
    }

    /// A server wired for virtual time around one config file, plus the
    /// handles a test needs to watch and stop it.
    struct Run {
        metrics: LiveMetrics,
        stop: StopHandle,
        sink: Arc<Mutex<Vec<u8>>>,
    }

    /// A config for a test, with a listener that cannot collide.
    ///
    /// Every boot now takes a real port, and two things follow that the tests
    /// have to say out loud. The bind is **loopback**, so a unit test never
    /// opens a port to the network; and it is **port 0**, so the operating
    /// system picks a free one and two tests running in parallel — which is
    /// the default — cannot fight over it. Without this, the suite would pass
    /// alone and fail together, which is the shape of flakiness that costs the
    /// most to diagnose.
    ///
    /// A config text that names `bind` itself is left alone: those tests are
    /// about the bind.
    fn with_test_bind(config_text: &str) -> String {
        if config_text.contains("bind") {
            return config_text.to_owned();
        }
        if let Some(rest) = config_text.strip_prefix("[server]\n") {
            format!("[server]\nbind = \"127.0.0.1:0\"\n{rest}")
        } else {
            format!("[server]\nbind = \"127.0.0.1:0\"\n{config_text}")
        }
    }

    /// Build (but do not start) a virtual-time run of `dust server`.
    fn boot(config_text: &str, configure: impl FnOnce(&mut ServerOptions)) -> (Run, Server) {
        let clock = Arc::new(ManualClock::new());
        let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
        let logger = Logger::new(
            Arc::new(Mutex::new(SinkBytes(Arc::clone(&sink)))),
            Level::Info,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let config_path = write_config(&with_test_bind(config_text));
        let mut options = ServerOptions {
            config_path: config_path.clone(),
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            // The tick loop parks by advancing virtual time; the watchdog
            // deliberately keeps the default condvar park. A stepper would
            // let the watcher manufacture its own grace expiry faster than
            // the other threads could report completion — the watchdog may
            // observe time, never fabricate it.
            loop_parker: stepping(Arc::clone(&clock), TICK_NS),
            logger,
            ..ServerOptions::default()
        };
        configure(&mut options);
        let server = Server::new(options);
        let run = Run {
            metrics: server.metrics(),
            stop: server.stop_handle(),
            sink,
        };
        (run, server)
    }

    /// Wait for progress without sleeping: bounded cooperative spinning,
    /// panicking loudly if the server never gets there.
    fn wait_until_ticks(metrics: &LiveMetrics, minimum: u64) {
        for _ in 0..10_000_000 {
            if metrics.ticks_observed() >= minimum {
                return;
            }
            std::thread::yield_now();
        }
        panic!(
            "the server never reached {minimum} tick(s); stuck at {}",
            metrics.ticks_observed()
        );
    }

    #[test]
    fn an_unresolvable_bind_unwinds_the_completed_phases_in_reverse() {
        let (_run, server) = boot("[server]\nbind = \"no port here\"\n", |_| {});
        let err = server.run().expect_err("a bad bind stops the boot");
        assert!(matches!(err, ServerError::NetworkBind { .. }), "{err}");
        assert!(err.to_string().contains("no port here"), "{err}");
        // Config and world started and were both released; network never
        // started, so it never appears — not even as a failure line.
        assert_eq!(
            err.transcript()
                .iter()
                .map(|e| (e.phase, e.direction))
                .collect::<Vec<_>>(),
            vec![
                (Phase::ConfigLoad, Direction::Start),
                (Phase::WorldDirs, Direction::Start),
                (Phase::WorldDirs, Direction::Stop),
                (Phase::ConfigLoad, Direction::Stop),
            ]
        );
    }

    #[test]
    fn an_invalid_config_stops_the_boot_before_anything_started() {
        let (_run, server) = boot("[server]\nmotdd = \"typo\"\n", |_| {});
        let err = server.run().expect_err("an unknown key is refused");
        assert!(matches!(err, ServerError::Config(_)), "{err}");
        assert!(err.transcript().is_empty(), "{:?}", err.transcript());
    }

    #[test]
    fn the_watchdog_thread_is_spawned_once_and_joined_once() {
        // This is the lifecycle-level half of the no-leaked-threads
        // guarantee; the tracker's own books are checked in `stop`'s tests.
        // What the run adds: the one thread spawned during phase 4 retires
        // through the same books instead of being detached, and a graceful
        // shutdown always beats it to completion.
        let (run, server) = boot("[server]\nshutdown_timeout_secs = 600\n", |options| {
            options.watchdog =
                WatchdogSetting::Custom(WatchdogPolicy::custom(600_000_000_000, |_| {}));
        });
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run.metrics, 2);
        assert!(run.stop.request_stop());
        let report = worker.join().expect("run finishes").expect("clean");
        assert_eq!(report.thread_panics, Vec::<String>::new());
        assert_eq!(report.threads_joined, 1, "the watchdog thread retires");
        assert!(!report.watchdog_fired);
    }

    #[test]
    fn the_configured_shutdown_timeout_reaches_the_watchdog() {
        let (run, server) = boot("[server]\nshutdown_timeout_secs = 600\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run.metrics, 3);
        assert!(run.stop.request_stop());
        let report = worker.join().expect("run finishes").expect("clean");
        assert_eq!(report.shutdown_grace_ns, Some(600_000_000_000));
        assert!(!report.watchdog_fired, "graceful shutdown wins the race");
        assert_eq!(report.thread_panics, Vec::<String>::new());
    }

    #[test]
    fn a_stop_before_boot_aborts_with_a_symmetric_transcript_and_no_ticks() {
        let (run, server) = boot("", |_| {});
        run.stop.request_stop();
        let report = server.run().expect("an early stop is not an error");
        assert!(report.interrupted);
        assert_eq!(report.ticks_run, 0);
        assert_eq!(
            report.transcript_pairs(),
            vec![
                (Phase::ConfigLoad, Direction::Start),
                (Phase::ConfigLoad, Direction::Stop),
            ]
        );
    }

    #[test]
    fn the_configured_log_level_silences_the_heartbeat() {
        // At warn, the info-level heartbeat never reaches the sink...
        let (run, server) = boot("[server]\nlog_level = \"warn\"\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run.metrics, 2);
        run.stop.request_stop();
        let report = worker.join().expect("finishes").expect("clean");
        assert!(report.is_clean());
        let text = String::from_utf8(run.sink.lock().unwrap().clone()).expect("utf8");
        assert!(!text.contains("heartbeat"), "{text}");

        // ...and at the default it does, proving silence was the setting and
        // not a broken sink.
        let (run, server) = boot("", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run.metrics, 2);
        run.stop.request_stop();
        worker.join().expect("finishes").expect("clean");
        let text = String::from_utf8(run.sink.lock().unwrap().clone()).expect("utf8");
        assert!(text.contains("heartbeat"), "{text}");
    }

    #[test]
    fn jvm_disabled_keeps_its_placeholder_out_of_the_participant_list() {
        let (run, server) = boot("[jvm]\nenabled = false\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run.metrics, 1);
        run.stop.request_stop();
        let report = worker.join().expect("finishes").expect("clean");
        assert!(
            !report.participants.contains(&"jvm-placeholder".to_owned()),
            "{:?}",
            report.participants
        );
        assert!(report.participants.contains(&"status-probe".to_owned()));
    }

    #[test]
    fn an_environment_override_reaches_the_runtime_knobs() {
        // The env layer sits between file and types; these are the settings
        // this crate consumes, arriving exactly as a container would set
        // them. Pure function, no process environment touched.
        let config = DustConfig::from_toml_and_env(
            "",
            "test",
            [
                ("DUST__SERVER__MAX_CATCHUP_TICKS".to_owned(), "7".to_owned()),
                (
                    "DUST__SERVER__SHUTDOWN_TIMEOUT_SECS".to_owned(),
                    "9".to_owned(),
                ),
                ("DUST__SERVER__LOG_LEVEL".to_owned(), "debug".to_owned()),
            ],
        )
        .expect("valid");
        let runtime = RuntimeSettings::from_config(&config);
        assert_eq!(runtime.catchup_cap, 7);
        assert_eq!(runtime.shutdown_grace_ns, 9_000_000_000);
        assert_eq!(log_level_of(config.server.log_level), Level::Debug);
    }
}

//! The Dust server process: one binary's worth of lifecycle, tick loop and
//! stop discipline.
//!
//! # The shape of a run
//!
//! A Dust process is a strictly ordered boot, a loop that never runs a partial
//! tick, and a teardown that walks the boot backwards. In prose:
//!
//! ```text
//! dust server
//!   │
//!   ├─ 1. config.load      read + validate dust.toml, layer DUST__* over it,
//!   │                      extract the runtime settings (catch-up cap,
//!   │                      shutdown timeout, log level) and re-filter logging
//!   ├─ 2. world.ensure     create the world directories
//!   ├─ 3. network.bind     resolve [server].bind — placeholder; dust-net owns
//!   │                      the real socket later
//!   └─ 4. tick.loop        fixed-timestep engine over participants, checking
//!            ▲             the stop flag BETWEEN passes, never mid-tick
//!            └── stop requested (ctrl-C handler, console, test) or watchdog fired
//!
//! then shutdown, in exact reverse of whatever completed:
//!
//!   4. tick.stop           final timing captured into ShutdownReport
//!   3. network.release     placeholder released
//!   2. world.release       directories left on disk, noted honestly
//!   1. config.release      configuration released
//! ```
//!
//! Three properties are enforced rather than hoped for, each with its own
//! module and its own tests:
//!
//! **Symmetry** ([`server`]). Every phase that starts gets a stop beside it in
//! the run transcript, newest first, whether shutdown is graceful or a failed
//! phase unwinds the boot. Only completed phases are torn down.
//!
//! **Bounded stopping** ([`stop`]). A stop request is an atomic plus a
//! condvar broadcast; it waits at most until the current pass finishes.
//! Worst-case shutdown latency is therefore one tick batch — coarse by
//! design, because interrupting a half-finished world update corrupts. A
//! watchdog thread arms itself when stop is requested and ends the process by
//! force if graceful shutdown outlives `[server].shutdown_timeout_secs`.
//!
//! **Deterministic cadence** ([`engine`], [`clock`]). Ticks are 50 ms of
//! simulated time, driven by an accumulator with a per-burst catch-up cap so
//! a stall cannot become a spiral. All time comes from the injected
//! [`Clock`](clock::Clock): production supplies a monotonic clock, tests a
//! manual one, and neither the engine nor the lifecycle can tell which is
//! running.
//!
//! # What Phase 3 builds on
//!
//! Later crates integrate here rather than inside the loop:
//!
//! * Networking and world simulation implement
//!   [`TickParticipant`](participant::TickParticipant) — a name for logs and
//!   timing tables, a priority (lower runs earlier within a tick), and one
//!   `tick` call per executed tick — and register on the
//!   [`ParticipantSet`](participant::ParticipantSet) through
//!   `ServerOptions::extra_tasks`.
//! * Anything needing to end the process holds a
//!   [`StopHandle`](stop::StopHandle); requesting a stop from a test is
//!   indistinguishable from ctrl-C.
//! * Per-tick cost per participant lands in sliding-window histograms
//!   ([`histogram`]), surfaced in the [`ShutdownReport`](server::ShutdownReport).
//!
//! # Virtual time is not a test trick, it is the contract
//!
//! Every deadline, park and measurement in this crate reads the clock it was
//! given. That is why a full boot-ticks-ctrl-C-shutdown cycle runs in
//! microseconds under a [`ManualClock`](clock::ManualClock) with exact tick
//! counts, and why no test here sleeps.

pub mod cli;
pub mod clock;
pub mod engine;
pub mod histogram;
pub mod logging;
pub mod net;
pub mod participant;
pub mod server;
pub mod stop;
pub mod tasks;

pub use clock::{Clock, ManualClock, MonotonicClock};
pub use engine::{AdvanceReport, TickEngine, TICK_NS};
pub use histogram::{TimingHistogram, TimingStats};
pub use logging::{Level, Logger};
pub use participant::{ParticipantSet, TickContext, TickParticipant};
pub use server::{
    Direction, LiveMetrics, Phase, Server, ServerError, ServerOptions, ShutdownReport,
    TranscriptEntry, WatchdogSetting,
};
pub use stop::{CondvarParker, Parker, StepParker, StopHandle, WatchdogPolicy};

//! Demonstration participants, and the proof that configuration reaches them.
//!
//! These three small tasks exist for two reasons. First, a tick loop with no
//! participants proves nothing — something has to run for the ordering,
//! accounting and shutdown guarantees to be observable. Second, and more
//! important, each one is wired to a *real* field of `dust.toml`, so every
//! test that boots a server also exercises the claim underneath Phase 0.3:
//! that a setting written by an operator becomes behaviour executed by the
//! process, through typed config and nothing else.
//!
//! They are honest placeholders, not stubs pretending to be features: the JVM
//! bridge does not load plugins and the ore workload simulates no blocks.
//! What they do — read config, gate registration, scale work — is exactly
//! what their real replacements will do.

use dust_config::model::DustConfig;
use dust_config::ore::VANILLA_ORE_GROUPS;

use crate::clock::ManualClock;
use crate::participant::{ParticipantSet, TickContext, TickParticipant};

/// Where in the tick the status probe runs.
pub const PRIORITY_STATUS_PROBE: i32 = -100;
/// Where the JVM placeholder would run, relative to everything else.
pub const PRIORITY_JVM_PLACEHOLDER: i32 = 200;
/// Where the ore workload runs.
pub const PRIORITY_ORE_WORKLOAD: i32 = 300;

/// Log target prefix for the status probe.
pub const TARGET_STATUS: &str = "dust::status";
/// Log target prefix for the JVM placeholder.
pub const TARGET_JVM: &str = "dust::jvm";
/// Log target prefix for the ore workload.
pub const TARGET_ORES: &str = "dust::ores";

/// Virtual nanoseconds of work one work-unit costs, when charging is on.
///
/// The number itself is arbitrary; what matters is that it is constant, so a
/// doubling of configured frequency doubles the measured tick cost exactly.
pub const WORK_UNIT_NS: u64 = 250_000;

/// How the ore workload pays for its simulated work.
///
/// `RealWorld` means the measurement sees whatever the host actually spends,
/// which is approximately nothing today. `Virtual` means the task advances
/// the injected manual clock by a fixed amount per unit, making its cost
/// exact and therefore assertable — this is the mode tests use.
#[derive(Clone, Debug)]
pub enum WorkCharger {
    RealWorld,
    Virtual(std::sync::Arc<ManualClock>),
}

impl WorkCharger {
    fn charge(&self, ns: u64) {
        if let Self::Virtual(clock) = self {
            clock.advance_ns(ns);
        }
    }
}

/// Build the skeleton's participant set from a loaded configuration.
///
/// This function is the wiring diagram made code: read it to see which
/// setting gates which participant. It is deliberately infallible — a
/// configuration that reached here has already passed validation.
pub fn registry_from_config(
    config: &DustConfig,
    charger: &WorkCharger,
) -> ParticipantSet {
    let mut set = ParticipantSet::new();

    // `[server]`: identity and capacity reach the runtime verbatim.
    set.insert(Box::new(StatusProbe::new(config)));

    // `[jvm] enabled`: the master switch for plugin support. Off means the
    // participant never registers at all — absence, not a sleeping presence.
    if config.jvm.enabled {
        set.insert(Box::new(JvmPlaceholder::new()));
    }

    // `[worldgen.ores]`: enabled gates registration; the resolved frequency
    // of an ore group scales how much work one tick performs.
    if config.worldgen.ores.enabled {
        set.insert(Box::new(OreWorkload::new(config, charger)));
    }

    set
}

/// Heartbeat showing that `[server]` reached the running process.
///
/// Every twentieth tick it logs capacity and identity — the numbers an
/// operator compares against `server.max_players` and `server.motd` in the
/// file they wrote. Twenty ticks is one vanilla second, chosen so a minute
/// of logs is three lines rather than twelve hundred.
pub struct StatusProbe {
    max_players: u32,
    motd: String,
}

impl StatusProbe {
    pub fn new(config: &DustConfig) -> Self {
        Self {
            max_players: config.server.max_players,
            motd: config.server.motd.clone(),
        }
    }

    /// The player capacity this probe saw in the configuration.
    pub fn max_players(&self) -> u32 {
        self.max_players
    }

    /// The message of the day this probe saw.
    pub fn motd(&self) -> &str {
        &self.motd
    }
}

impl TickParticipant for StatusProbe {
    fn name(&self) -> &str {
        "status-probe"
    }

    fn priority(&self) -> i32 {
        PRIORITY_STATUS_PROBE
    }

    fn tick(&mut self, ctx: &TickContext) {
        if ctx.tick_index % 20 == 0 {
            ctx.logger.info(
                TARGET_STATUS,
                format!("heartbeat: 0/{self} players"),
            );
        }
    }
}

impl std::fmt::Display for StatusProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.max_players, self.motd)
    }
}

/// Stand-in for the embedded JVM bridge, gated by `[jvm] enabled`.
///
/// Its whole job is to announce once that plugin loading *would* happen here,
/// then go quiet. The announcement matters more than silence: in a log
/// transcript, "jvm-placeholder announced" versus "absent from the
/// participant list" is the observable difference the enable flag makes.
pub struct JvmPlaceholder {
    announced: bool,
}

impl Default for JvmPlaceholder {
    fn default() -> Self {
        Self::new()
    }
}

impl JvmPlaceholder {
    pub fn new() -> Self {
        Self { announced: false }
    }

    /// Whether the one-time announcement has happened.
    pub fn has_announced(&self) -> bool {
        self.announced
    }
}

impl TickParticipant for JvmPlaceholder {
    fn name(&self) -> &str {
        "jvm-placeholder"
    }

    fn priority(&self) -> i32 {
        PRIORITY_JVM_PLACEHOLDER
    }

    fn tick(&mut self, ctx: &TickContext) {
        if !self.announced {
            self.announced = true;
            ctx.logger.info(TARGET_JVM, "plugin bridge would mount here");
        }
    }
}

/// Turns `[worldgen.ores]` into measurable per-tick work.
///
/// One work-unit per whole point of the resolved frequency multiplier of the
/// first vanilla ore group — diamond, by convention of this demonstration —
/// so `frequency = 3.0` costs three units and `frequency = 0.5` rounds to
/// one. With the master switch off, the task does not exist; with an ore
/// disabled by override, its resolution falls back through the same
/// precedence `dust-gen` will honour later, which is precisely the point:
/// there is one resolver, and everyone borrows it.
pub struct OreWorkload {
    frequency: f64,
    work_units: u32,
    charger: WorkCharger,
    units_done: u64,
}

impl OreWorkload {
    pub fn new(config: &DustConfig, charger: &WorkCharger) -> Self {
        let group = VANILLA_ORE_GROUPS
            .first()
            .map(dust_config::ore::OreGroup::new)
            .expect("the vanilla ore group table is non-empty");
        let settings = config.worldgen.ores.resolve_group(&group);
        let work_units = settings.frequency.round().clamp(1.0, 64.0) as u32;
        Self {
            frequency: settings.frequency,
            work_units,
            charger: charger.clone(),
            units_done: 0,
        }
    }

    /// Units of work one tick performs, derived from configuration.
    pub fn work_units(&self) -> u32 {
        self.work_units
    }

    /// The raw multiplier the units were derived from.
    pub fn frequency(&self) -> f64 {
        self.frequency
    }

    /// Total units performed since registration.
    pub fn units_done(&self) -> u64 {
        self.units_done
    }
}

impl TickParticipant for OreWorkload {
    fn name(&self) -> &str {
        "ore-workload"
    }

    fn priority(&self) -> i32 {
        PRIORITY_ORE_WORKLOAD
    }

    fn tick(&mut self, ctx: &TickContext) {
        for _ in 0..self.work_units {
            self.charger.charge(WORK_UNIT_NS);
            self.units_done += 1;
        }
        let _ = ctx; // quiet by default; the timing table tells the story
    }
}

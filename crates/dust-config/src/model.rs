//! The configuration tree. These types are the single definition of
//! `dust.toml`; the reference documentation and the schema are generated from
//! them.

use serde::{Deserialize, Serialize};

use crate::ore::OresConfig;
use crate::{ConfigSection, Finding};

/// The whole of `dust.toml`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct DustConfig {
    /// Listener, identity and the basics a server needs to answer a ping.
    #[config(section)]
    pub server: ServerConfig,

    /// The embedded JVM that runs plugin bytecode.
    #[config(section)]
    pub jvm: JvmConfig,

    /// World generation: the engine, and Dust's knobs over it.
    #[config(section)]
    pub worldgen: WorldgenConfig,
}

/// Listener, identity and the basics a server needs to answer a ping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on, as `host:port`.
    #[config(restart)]
    pub bind: String,

    /// Message shown in the client's server list.
    pub motd: String,

    /// Maximum concurrent players. This is the number shown in the server list,
    /// not a licence — the gateway may admit more across several backends.
    pub max_players: u32,

    /// Verify each joining player against Mojang's session servers. Turning this
    /// off means anyone may join under any name, and is only safe behind a proxy
    /// that does the check itself.
    #[config(restart)]
    pub online_mode: bool,

    /// The most ticks the server will try to repay after a stall, per pass of
    /// the tick loop. A stall longer than this is skipped past rather than
    /// caught up, which keeps a hiccup from becoming a death spiral.
    #[config(restart)]
    pub max_catchup_ticks: u32,

    /// How long, in seconds, a shutdown may take after a stop request before
    /// the watchdog ends the process by force. Grace worth having is grace
    /// worth bounding.
    #[config(restart)]
    pub shutdown_timeout_secs: u32,

    /// The lowest severity the server logs: one of `error`, `warn`, `info`,
    /// `debug`, `trace`. Everything less severe than the chosen level is
    /// suppressed.
    #[config(restart)]
    pub log_level: LogLevel,

    /// Path to the icon shown beside this server in the client's list, or empty
    /// for none. Must be a 64x64 PNG; the client silently shows nothing for a
    /// picture it cannot use, so the server refuses one at boot instead.
    #[config(restart)]
    pub favicon: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:25565".to_owned(),
            motd: "A Dust server".to_owned(),
            max_players: 20,
            online_mode: true,
            max_catchup_ticks: 20,
            shutdown_timeout_secs: 10,
            log_level: LogLevel::default(),
            favicon: String::new(),
        }
    }
}

impl ServerConfig {
    /// Everything wrong with this section. See [`DustConfig::check`].
    ///
    /// Both bounds reject only what makes the setting mean the opposite of its
    /// purpose: a zero-tick catch-up allowance skips every stall including the
    /// ordinary ones between two passes, and a zero-second shutdown grace fires
    /// the watchdog before the graceful path has run its first line.
    pub fn check(&self, path: &str, findings: &mut Vec<Finding>) {
        if self.max_catchup_ticks == 0 {
            findings.push(Finding::error(
                format!("{path}.max_catchup_ticks"),
                "must be at least 1; at 0 the loop repays no time at all and \
                 every pass surrenders",
            ));
        }
        if self.shutdown_timeout_secs == 0 {
            findings.push(Finding::error(
                format!("{path}.shutdown_timeout_secs"),
                "must be at least 1; at 0 seconds nothing graceful can finish",
            ));
        }
    }
}

/// How loudly the server logs.
///
/// An enum rather than a free string so a typo fails at parse time naming the
/// field, exactly like every other typed setting; a string would move the same
/// mistake into validation where it reads as a second-class value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        };
        f.write_str(name)
    }
}

/// The embedded JVM that runs plugin bytecode.
///
/// Turning this off must leave every other feature working. That is not a
/// courtesy, it is the standing test that keeps game logic out of Java — see
/// decision record 0005.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct JvmConfig {
    /// Start the embedded JVM and load plugins. With this off, Dust runs with no
    /// JVM in the process at all and everything except plugins still works.
    #[config(restart)]
    pub enabled: bool,

    /// Heap ceiling for the embedded JVM, in mebibytes.
    #[config(restart)]
    pub max_heap_mib: u32,
}

impl Default for JvmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_heap_mib: 1024,
        }
    }
}

/// World generation: the engine, and Dust's knobs over it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct WorldgenConfig {
    /// How common each ore is, and where it generates.
    #[config(section)]
    pub ores: OresConfig,
}

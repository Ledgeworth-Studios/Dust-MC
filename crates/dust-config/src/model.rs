//! The configuration tree. These types are the single definition of
//! `dust.toml`; the reference documentation and the schema are generated from
//! them.

use serde::{Deserialize, Serialize};

use crate::ore::OresConfig;
use crate::ConfigSection;

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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:25565".to_owned(),
            motd: "A Dust server".to_owned(),
            max_players: 20,
            online_mode: true,
        }
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

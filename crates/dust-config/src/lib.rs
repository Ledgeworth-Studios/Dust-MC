//! Dust's configuration system: one `dust.toml`, typed once.
//!
//! Three things are true of every setting in Dust, and this crate is where each
//! one is enforced rather than asked for in review:
//!
//! 1. **It is typed.** The types in [`model`] are the only definition. The JSON
//!    schema and `docs/configuration.md` are generated from them, so they cannot
//!    drift from what the server actually reads.
//! 2. **It is documented.** `#[derive(ConfigSection)]` refuses to compile a
//!    field with no doc comment. This is the Phase 0.3 exit criterion.
//! 3. **It says when it takes effect.** Every field carries a [`Reload`] marker
//!    on the type, not in a comment.
//!
//! Layering, lowest precedence first: built-in defaults, then `dust.toml`, then
//! `DUST__*` environment variables, then runtime changes made by command.

// The derive refers to this crate by its public name, which is how it works in
// every crate but this one. This makes that name resolve here too.
extern crate self as dust_config;

pub mod docs;
pub mod env;
pub mod model;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

pub use dust_config_derive::ConfigSection;
pub use model::DustConfig;

/// When a change to a setting takes effect.
///
/// This is on the type because "requires a restart" written in a comment is a
/// claim nobody can test. Here the documentation generator reads it, the
/// hot-reload path reads it, and the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// Takes effect immediately on reload.
    Hot,
    /// Takes effect immediately, but only for chunks generated from now on.
    /// Chunks already written to disk keep whatever they were generated with.
    HotNewChunksOnly,
    /// Read once at startup. Changing it needs a restart.
    Restart,
}

impl Reload {
    /// The words that appear in the generated reference.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::HotNewChunksOnly => "hot, new chunks only",
            Self::Restart => "restart",
        }
    }
}

/// One setting, as it appears in the generated reference.
#[derive(Debug, Clone)]
pub struct FieldDoc {
    pub name: &'static str,
    pub doc: &'static str,
    pub ty: &'static str,
    pub reload: Reload,
    pub default: String,
}

/// One `[section]` of `dust.toml`, as it appears in the generated reference.
#[derive(Debug, Clone)]
pub struct SectionDoc {
    /// This section's key relative to its parent. Empty for the root.
    pub key: &'static str,
    pub doc: &'static str,
    /// `Some(label)` when the section is a map of operator-chosen keys, in
    /// which case this doc describes one entry rather than the table itself.
    pub keyed_by: Option<&'static str>,
    pub fields: Vec<FieldDoc>,
    pub subsections: Vec<SectionDoc>,
}

/// Implemented by `#[derive(ConfigSection)]`. Do not implement by hand.
pub trait ConfigSection: Default {
    /// What an operator-chosen key means, for sections used as map values.
    const MAP_KEY_LABEL: &'static str = "key";

    fn describe() -> SectionDoc;
}

/// Renders a default value the way it should be typed into `dust.toml`.
pub trait ConfigValue {
    fn render_default(&self) -> String;
}

macro_rules! impl_display_value {
    ($($t:ty),*) => { $(
        impl ConfigValue for $t {
            fn render_default(&self) -> String { self.to_string() }
        }
    )* };
}
impl_display_value!(bool, i8, i16, i32, i64, u8, u16, u32, u64, usize);

macro_rules! impl_float_value {
    ($($t:ty),*) => { $(
        impl ConfigValue for $t {
            fn render_default(&self) -> String {
                // `1` and `1.0` are the same number and not the same TOML: the
                // first is an integer, and pasting it back into the file as the
                // default for a float is a type error the reference invited.
                if self.fract() == 0.0 && self.is_finite() {
                    format!("{self:.1}")
                } else {
                    self.to_string()
                }
            }
        }
    )* };
}
impl_float_value!(f32, f64);

impl ConfigValue for String {
    fn render_default(&self) -> String {
        format!("{self:?}")
    }
}

impl<T: ConfigValue> ConfigValue for Option<T> {
    fn render_default(&self) -> String {
        match self {
            Some(v) => v.render_default(),
            // An absent optional is not `null` in TOML; it is a line you do not
            // write. Saying so is more useful in the reference than "None".
            None => "unset".to_owned(),
        }
    }
}

impl<T: ConfigValue> ConfigValue for Vec<T> {
    fn render_default(&self) -> String {
        let mut out = String::from("[");
        for (i, v) in self.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&v.render_default());
        }
        out.push(']');
        out
    }
}

impl<K: std::fmt::Display, V> ConfigValue for BTreeMap<K, V> {
    fn render_default(&self) -> String {
        if self.is_empty() {
            return "no entries".to_owned();
        }
        let mut out = String::new();
        for (i, k) in self.keys().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{k}");
        }
        out
    }
}

/// Everything that can go wrong between a file on disk and a `DustConfig`.
#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: String,
        source: std::io::Error,
    },
    Parse {
        path: String,
        source: toml::de::Error,
    },
    Env(env::EnvError),
    /// The file parsed, but the values in it do not describe a runnable server.
    Invalid(Vec<Finding>),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "could not read {path}: {source}"),
            Self::Parse { path, source } => write!(f, "could not parse {path}: {source}"),
            Self::Env(e) => write!(f, "{e}"),
            Self::Invalid(findings) => {
                writeln!(f, "{} problem(s) in the configuration:", findings.len())?;
                for finding in findings {
                    writeln!(f, "  {finding}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// How much a [`Finding`] matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The server starts. Something in the file probably does not do what the
    /// person who wrote it expected.
    Warning,
    /// The server does not start on this file.
    Error,
}

/// A single problem, named by the setting that caused it.
///
/// Findings carry the dotted path because "invalid value" with no path is the
/// error message that sends an operator hunting through a file by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Finding {
    /// A problem that stops the server starting.
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    /// A problem the operator should see, on a server that still starts.
    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{level} at {}: {}", self.path, self.message)
    }
}

impl DustConfig {
    /// Built-in defaults, with nothing layered over them.
    ///
    /// A Dust that is handed no configuration at all must still be a running
    /// server, so the defaults are a valid configuration by construction and a
    /// test asserts it.
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Load `dust.toml`, layer `DUST__*` over it, and validate the result.
    ///
    /// A missing file is not an error — it means "run on the defaults".
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ConfigError::Read {
                    path: path.display().to_string(),
                    source: e,
                })
            }
        };
        Self::from_toml_and_env(&text, &path.display().to_string(), env::from_process())
    }

    /// The whole load path with the file contents and environment supplied, so
    /// it is testable without touching the filesystem or the process
    /// environment.
    pub fn from_toml_and_env(
        text: &str,
        origin: &str,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ConfigError> {
        let mut table: toml::Value = if text.trim().is_empty() {
            toml::Value::Table(toml::Table::new())
        } else {
            toml::from_str(text).map_err(|e| ConfigError::Parse {
                path: origin.to_owned(),
                source: e,
            })?
        };
        env::overlay(&mut table, vars).map_err(ConfigError::Env)?;
        let config: Self = table.try_into().map_err(|e| ConfigError::Parse {
            path: origin.to_owned(),
            source: e,
        })?;
        let findings = config.check();
        if findings.iter().any(|f| f.severity == Severity::Error) {
            // Warnings travel with the errors so one run of `dust config check`
            // shows an operator everything at once.
            Err(ConfigError::Invalid(findings))
        } else {
            Ok(config)
        }
    }

    /// Everything wrong with this configuration, all of it, in one pass.
    ///
    /// Reporting every problem at once is deliberate: an operator who fixes one
    /// value, restarts, and is told about the next one learns to distrust the
    /// server rather than the file.
    pub fn check(&self) -> Vec<Finding> {
        // Sections add their own checks here as they arrive. Nothing in the
        // groundwork configuration can be individually wrong yet — the types
        // already reject everything that is — so this is empty rather than
        // absent, because a section that has nowhere to report to is a section
        // that reports nothing.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_valid_configuration() {
        assert_eq!(DustConfig::defaults().check(), Vec::new());
    }

    #[test]
    fn an_empty_file_means_the_defaults() {
        let loaded = DustConfig::from_toml_and_env("", "test", []).expect("empty file loads");
        assert_eq!(loaded, DustConfig::defaults());
    }

    #[test]
    fn a_misspelled_key_is_an_error_and_not_a_silent_default() {
        let err = DustConfig::from_toml_and_env("[server]\nmotdd = \"typo\"\n", "test", [])
            .expect_err("a typo must not be ignored");
        let text = err.to_string();
        assert!(text.contains("motdd"), "error should name the key: {text}");
    }

    #[test]
    fn parse_serialise_reparse_is_stable() {
        // Property 4 from Testing.md, in its concrete form.
        let original = DustConfig::from_toml_and_env("[server]\nmax_players = 64\n", "test", [])
            .expect("valid");
        let text = toml::to_string(&original).expect("serialises");
        let round_tripped =
            DustConfig::from_toml_and_env(&text, "round-trip", []).expect("reparses");
        assert_eq!(original, round_tripped);
    }
}

//! Environment overrides: `DUST__SECTION__FIELD=value`.
//!
//! Container platforms configure by environment, so every setting has to be
//! reachable that way. Overrides are applied to the parsed TOML *before*
//! deserialisation, so an environment variable is subject to exactly the same
//! type checking, unknown-key rejection and validation as a line in the file.
//! An override that reaches the running server having skipped a check is a
//! second configuration system, and two configuration systems disagree.

/// The prefix that marks an environment variable as configuration.
pub const PREFIX: &str = "DUST__";

/// The separator between path segments. Two underscores, because a single one
/// is a legal character inside a setting name — `default_frequency`.
pub const SEPARATOR: &str = "__";

/// A problem with an environment override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvError {
    pub variable: String,
    pub message: String,
}

impl std::fmt::Display for EnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.variable, self.message)
    }
}

impl std::error::Error for EnvError {}

/// The `DUST__*` variables of the current process.
pub fn from_process() -> impl Iterator<Item = (String, String)> {
    std::env::vars().filter(|(k, _)| k.starts_with(PREFIX))
}

/// Apply overrides onto a parsed TOML document, creating tables as needed.
pub fn overlay(
    root: &mut toml::Value,
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<(), EnvError> {
    for (key, raw) in vars {
        let Some(rest) = key.strip_prefix(PREFIX) else {
            continue;
        };
        let path: Vec<String> = rest
            .split(SEPARATOR)
            .map(|s| s.to_ascii_lowercase())
            .collect();
        if path.iter().any(String::is_empty) {
            return Err(EnvError {
                variable: key.clone(),
                message: format!(
                    "is not a setting path. Expected {PREFIX}SECTION{SEPARATOR}FIELD, \
                     for example {PREFIX}WORLDGEN{SEPARATOR}ORES{SEPARATOR}DEFAULT_FREQUENCY"
                ),
            });
        }
        set(root, &path, parse_scalar(&raw), &key)?;
    }
    Ok(())
}

fn set(
    root: &mut toml::Value,
    path: &[String],
    value: toml::Value,
    variable: &str,
) -> Result<(), EnvError> {
    let mut cursor = root;
    for (depth, segment) in path.iter().enumerate() {
        let table = cursor.as_table_mut().ok_or_else(|| EnvError {
            variable: variable.to_owned(),
            message: format!(
                "cannot be applied: `{}` is a value in the file, not a section",
                path[..depth].join(".")
            ),
        })?;
        if depth == path.len() - 1 {
            table.insert(segment.clone(), value);
            return Ok(());
        }
        cursor = table
            .entry(segment.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
    Ok(())
}

/// Interpret the text the way TOML would have on the right of an `=`, falling
/// back to a plain string. This is what makes `DUST__JVM__ENABLED=false` a
/// boolean rather than the string `"false"`, which would then fail to
/// deserialise with a type error that names nothing an operator did wrong.
fn parse_scalar(raw: &str) -> toml::Value {
    match raw.parse::<toml::Value>() {
        Ok(value) => value,
        Err(_) => toml::Value::String(raw.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use crate::DustConfig;

    #[test]
    fn an_override_reaches_a_nested_setting() {
        let config = DustConfig::from_toml_and_env(
            "",
            "test",
            [("DUST__SERVER__MAX_PLAYERS".to_owned(), "2".to_owned())],
        )
        .expect("valid");
        assert_eq!(config.server.max_players, 2);
    }

    #[test]
    fn an_override_beats_the_file() {
        let config = DustConfig::from_toml_and_env(
            "[server]\nmax_players = 5\n",
            "test",
            [("DUST__SERVER__MAX_PLAYERS".to_owned(), "30".to_owned())],
        )
        .expect("valid");
        assert_eq!(config.server.max_players, 30);
    }

    #[test]
    fn an_override_is_type_checked_like_anything_else() {
        // The point of overlaying before deserialisation: this is the same
        // error the same value would produce written in the file.
        let err = DustConfig::from_toml_and_env(
            "",
            "test",
            [("DUST__SERVER__MAX_PLAYERS".to_owned(), "-4".to_owned())],
        )
        .expect_err("a negative player count is not a u32");
        assert!(err.to_string().contains("max_players"), "{err}");
    }

    #[test]
    fn a_boolean_override_is_a_boolean() {
        let config = DustConfig::from_toml_and_env(
            "",
            "test",
            [("DUST__JVM__ENABLED".to_owned(), "false".to_owned())],
        )
        .expect("valid");
        assert!(!config.jvm.enabled);
    }

    #[test]
    fn a_string_override_needs_no_quoting() {
        let config = DustConfig::from_toml_and_env(
            "",
            "test",
            [("DUST__SERVER__MOTD".to_owned(), "Ledgeworth".to_owned())],
        )
        .expect("valid");
        assert_eq!(config.server.motd, "Ledgeworth");
    }

    #[test]
    fn a_malformed_variable_name_is_reported_and_not_ignored() {
        let err =
            DustConfig::from_toml_and_env("", "test", [("DUST__".to_owned(), "1".to_owned())])
                .expect_err("malformed");
        assert!(err.to_string().contains("DUST__"), "{err}");
    }
}

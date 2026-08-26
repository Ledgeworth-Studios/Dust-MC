//! The command-line surface: `dust server [--config PATH] [--dry-run]`.
//!
//! # Why there is no argument-parser dependency
//!
//! The whole grammar is one subcommand with three flags, every flag has a
//! fixed arity, and nothing composes. A parser crate would weigh more than
//! the grammar it parsed and would add a second place where behaviour lives.
//! The workspace adds dependencies when the platform forces it (signal
//! handling is the example in this very crate) and not before; this module is
//! written to stay reviewable in one sitting.
//!
//! # Exit codes
//!
//! Numbers are chosen once, here, and stated:
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `0` | The command did what was asked — a clean run, a validated dry run, `--version`, `--help`. |
//! | `1` | Runtime failure: a phase failed, threads died during shutdown. |
//! | `2` | Usage error: unknown flag, missing value, missing subcommand. Nothing was attempted. |
//! | `3` | The configuration failed to load or validate; the message names every finding. |
//! | `124` | The watchdog ended a hung shutdown by force, following GNU `timeout`'s convention so a shell history tells the two kinds of forced exit apart. |
//!
//! Scripts can therefore distinguish "your flag is wrong", "your file is
//! wrong", "the server failed" and "the server had to be killed" without
//! parsing output.

use std::fmt;
use std::path::PathBuf;

use dust_config::DustConfig;

/// Everything went as asked.
pub const EXIT_OK: i32 = 0;
/// The server ran into something only a runtime failure can explain.
pub const EXIT_FAILURE: i32 = 1;
/// The command line itself was wrong; nothing was attempted.
pub const EXIT_USAGE: i32 = 2;
/// The configuration file failed to load or validate.
pub const EXIT_CONFIG_INVALID: i32 = 3;
/// The watchdog force-ended a shutdown that outlived its grace period,
/// matching GNU `timeout`'s exit convention. See [`crate::stop`].
pub const EXIT_WATCHDOG: i32 = 124;

/// What the parsed command line asks the process to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the server.
    Server(ServerInvocation),
    /// Print the usage text.
    Help,
    /// Print the version.
    Version,
}

/// One invocation of `dust server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInvocation {
    /// Where to read `dust.toml` from; defaults to [`crate::server::DEFAULT_CONFIG_PATH`].
    pub config_path: PathBuf,
    /// Load and validate the configuration, print what would run, exit 0.
    pub dry_run: bool,
}

/// A command line that cannot be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UsageError {}

/// Parse the process arguments (without the program name).
///
/// Explicit rather than global: the parser reads what it is handed, so tests
/// drive it directly and the binary is the only caller that touches
/// `std::env`.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, UsageError> {
    let args: Vec<String> = args.into_iter().collect();
    let Some(first) = args.first() else {
        return Err(UsageError::new(
            "expected a subcommand; try `dust server --help`",
        ));
    };

    // The two informational flags are answered wherever they appear, before
    // or after the subcommand: an operator who typed `--version` wants an
    // answer, not a lecture about position.
    if args.iter().any(|a| a == "--help" || a == "-h") && !args.iter().any(|a| a == "server") {
        return Ok(Command::Help);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") && !args.iter().any(|a| a == "server") {
        return Ok(Command::Version);
    }

    if first != "server" {
        return Err(UsageError::new(format!(
            "unknown subcommand {first:?}; expected `server`"
        )));
    }

    let mut invocation = ServerInvocation {
        config_path: PathBuf::from(crate::server::DEFAULT_CONFIG_PATH),
        dry_run: false,
    };
    let mut seen_config = false;
    let mut rest = args[1..].iter().peekable();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--config" => {
                if seen_config {
                    return Err(UsageError::new("--config given more than once"));
                }
                seen_config = true;
                let value = rest
                    .next()
                    .ok_or_else(|| UsageError::new("--config needs a PATH argument"))?;
                invocation.config_path = PathBuf::from(value);
            }
            "--dry-run" => invocation.dry_run = true,
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            other => {
                return Err(UsageError::new(format!(
                    "unknown flag {other:?}; try `dust server --help`"
                )))
            }
        }
    }
    Ok(Command::Server(invocation))
}

/// The text `--help` prints, and the usage part of every usage error.
pub fn usage_text() -> &'static str {
    "\
Usage: dust server [--config PATH] [--dry-run] [--version]

Runs a Dust Minecraft server.

  --config PATH   Read dust.toml from PATH (default: dust.toml)
  --dry-run       Load and validate the configuration, print what would
                  run, and exit 0 without touching anything else
  --version       Print the version and exit
  --help          Print this help and exit

Exit codes:
  0  success          2  usage error        124  shutdown forced by watchdog
  1  runtime failure  3  invalid configuration"
}

/// The text `--version` prints.
pub fn version_text() -> String {
    format!("dust {}", env!("CARGO_PKG_VERSION"))
}

/// Render what a loaded configuration would run, for `--dry-run`.
///
/// The summary shows effective values — everything the environment overlaid
/// on the file is already merged by the time this sees the config — so what
/// an operator reads here is what the server would do, not what they wrote.
pub fn render_summary(config: &DustConfig) -> String {
    let mut findings = config.check();
    let mut out = String::new();
    if findings
        .iter()
        .any(|f| f.severity == dust_config::Severity::Error)
    {
        out.push_str("dust server dry run — configuration is NOT runnable\n\n");
    } else {
        out.push_str("dust server dry run — configuration OK\n\n");
    }
    let server = &config.server;
    out.push_str("[server]\n");
    out.push_str(&format!("  bind                  {}\n", server.bind));
    out.push_str(&format!("  motd                  {}\n", server.motd));
    out.push_str(&format!("  max_players           {}\n", server.max_players));
    out.push_str(&format!("  online_mode           {}\n", server.online_mode));
    out.push_str(&format!(
        "  max_catchup_ticks     {} per stall\n",
        server.max_catchup_ticks
    ));
    out.push_str(&format!(
        "  shutdown_timeout_secs {}s before forced exit\n",
        server.shutdown_timeout_secs
    ));
    out.push_str(&format!("  log_level             {}\n", server.log_level));

    out.push_str("\n[jvm]\n");
    out.push_str(&format!("  enabled               {}\n", config.jvm.enabled));
    out.push_str(&format!(
        "  max_heap_mib          {} MiB\n",
        config.jvm.max_heap_mib
    ));

    out.push_str("\n[worldgen.ores]\n");
    out.push_str(&format!(
        "  enabled               {}\n",
        config.worldgen.ores.enabled
    ));
    out.push_str(&format!(
        "  default_frequency     {}\n",
        config.worldgen.ores.default_frequency
    ));
    out.push_str(&format!(
        "  overrides             {} group(s)\n",
        config.worldgen.ores.overrides.len()
    ));

    findings.retain(|f| f.severity == dust_config::Severity::Warning);
    if !findings.is_empty() {
        out.push_str("\nWarnings:\n");
        for finding in findings {
            out.push_str(&format!("  {finding}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_plain_subcommand_takes_the_defaults() {
        let command = parse(args(&["server"])).expect("parses");
        assert_eq!(
            command,
            Command::Server(ServerInvocation {
                config_path: PathBuf::from(crate::server::DEFAULT_CONFIG_PATH),
                dry_run: false,
            })
        );
    }

    #[test]
    fn every_flag_is_recognised_in_position() {
        let command =
            parse(args(&["server", "--config", "/etc/dust.toml", "--dry-run"])).expect("parses");
        assert_eq!(
            command,
            Command::Server(ServerInvocation {
                config_path: PathBuf::from("/etc/dust.toml"),
                dry_run: true,
            })
        );
    }

    #[test]
    fn a_missing_value_is_a_usage_error_and_not_a_panic() {
        let err =
            parse(args(&["server", "--config"])).expect_err("a valueless --config must be refused");
        assert!(err.to_string().contains("PATH"), "{err}");
    }

    #[test]
    fn an_unknown_flag_names_itself() {
        let err = parse(args(&["server", "--verbose"])).expect_err("unknown flags are refused");
        assert!(err.to_string().contains("--verbose"), "{err}");
    }

    #[test]
    fn a_repeated_config_is_refused_rather_than_quietly_won() {
        let err = parse(args(&[
            "server", "--config", "a.toml", "--config", "b.toml",
        ]))
        .expect_err("two files is ambiguous");
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn no_arguments_at_all_ask_for_help_without_running_anything() {
        let err = parse(args(&[])).expect_err("bare `dust` does nothing");
        assert!(err.to_string().contains("subcommand"), "{err}");
    }

    #[test]
    fn an_unknown_subcommand_is_reported_as_the_problem() {
        let err = parse(args(&["start"])).expect_err("only `server` exists today");
        assert!(err.to_string().contains("start"), "{err}");
    }

    #[test]
    fn help_and_version_answer_wherever_they_appear() {
        assert_eq!(parse(args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(args(&["-h"])), Ok(Command::Help));
        assert_eq!(parse(args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(args(&["server", "--version"])), Ok(Command::Version));
        assert_eq!(parse(args(&["server", "--help"])), Ok(Command::Help));
    }

    #[test]
    fn the_version_line_carries_the_package_version() {
        let text = version_text();
        assert!(text.starts_with("dust "), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
    }

    #[test]
    fn the_usage_text_documents_every_flag_and_exit_code() {
        let text = usage_text();
        for fragment in ["--config PATH", "--dry-run", "--version", "124", "3 "] {
            assert!(text.contains(fragment), "usage omits {fragment:?}: {text}");
        }
    }

    #[test]
    fn the_summary_shows_effective_values_not_file_text() {
        // Two settings arrive through the environment layer, not a file, and
        // the summary must show them as if they were typed — because to the
        // runtime, they were.
        let config = DustConfig::from_toml_and_env(
            "[server]\nmax_players = 40\n",
            "test",
            [
                ("DUST__SERVER__MAX_CATCHUP_TICKS".to_owned(), "5".to_owned()),
                ("DUST__SERVER__LOG_LEVEL".to_owned(), "warn".to_owned()),
            ],
        )
        .expect("valid");
        let summary = render_summary(&config);
        assert!(summary.contains("configuration OK"), "{summary}");
        assert!(summary.contains("40"), "{summary}");
        assert!(summary.contains("max_catchup_ticks     5"), "{summary}");
        assert!(summary.contains("log_level             warn"), "{summary}");
    }

    #[test]
    fn a_summary_with_warnings_lists_them_but_stays_ok() {
        // The master switch off under live overrides is the classic warning
        // from the configuration system; a dry run exists precisely to show
        // it before a boot ever happens.
        let config = DustConfig::from_toml_and_env(
            "[worldgen.ores]\nenabled = false\n\n\
             [worldgen.ores.overrides.diamond]\nfrequency = 4.0\n",
            "test",
            [],
        )
        .expect("loads with warnings");
        let summary = render_summary(&config);
        assert!(summary.contains("configuration OK"), "{summary}");
        assert!(summary.contains("Warnings:"), "{summary}");
    }
}

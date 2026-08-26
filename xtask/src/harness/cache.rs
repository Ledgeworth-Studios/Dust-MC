//! Where the harness keeps everything it downloads or generates.
//!
//! The extractor's cache (`.dust-extract/`) sits inside the repository behind a
//! `.gitignore` line. The harness deliberately does not follow it: this cache
//! holds not just Mojang's jar but whole generated worlds and the digest files
//! derived from them, and the licensing rule is that nothing of that kind is
//! ever at risk of being committed. So the root lands in the platform's user
//! cache area — outside every worktree, shared by all of them, deletable
//! without touching the checkout.
//!
//! `DUST_HARNESS_CACHE` moves the whole tree for CI runners and for anyone who
//! wants these gigabytes on another volume.

use std::path::{Path, PathBuf};

/// The environment variable that overrides the default location.
pub const ENV_OVERRIDE: &str = "DUST_HARNESS_CACHE";

/// Every directory the harness owns, resolved from a cache root.
///
/// One struct rather than free functions so a call site can never mix a jar
/// path from one root with a server directory from another.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The cache root itself, printed by `provision` so operators can find it.
    pub root: PathBuf,
    /// Verified server jars, laid out exactly where
    /// `extract::download::server_jar` expects its own cache.
    pub jars: PathBuf,
    /// Vanilla run directories, one per (version, seed).
    pub servers: PathBuf,
    /// Digest sets written by `capture`, read back by `compare`.
    pub captures: PathBuf,
}

impl Layout {
    /// Resolve the layout under an explicit root.
    pub fn under(root: PathBuf) -> Self {
        Self {
            jars: root.join("jars"),
            servers: root.join("servers"),
            captures: root.join("captures"),
            root,
        }
    }

    /// Resolve the layout from the environment.
    ///
    /// Order: `DUST_HARNESS_CACHE`, then `$XDG_CACHE_HOME/dust-harness`, then
    /// the platform default. See [`default_root`].
    pub fn resolve() -> Result<Self, String> {
        let root = match non_empty(std::env::var(ENV_OVERRIDE).ok()) {
            Some(value) => PathBuf::from(value),
            None => default_root(
                non_empty(std::env::var("XDG_CACHE_HOME").ok()),
                non_empty(std::env::var("HOME").ok()).as_deref(),
            ),
        };
        for dir in [root.clone(), root.join("jars")] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        Ok(Self::under(root))
    }

    /// The vanilla run directory for one version and seed.
    ///
    /// One directory per seed rather than one per version: vanilla reads
    /// `level-seed` only when a world is created, so a reused directory would
    /// quietly keep generating the *first* seed it saw while the operator
    /// believes they asked for another. Distinct directories make the stale-
    /// world mistake impossible instead of documented.
    pub fn server_dir(&self, version: &str, seed: i64) -> PathBuf {
        self.servers.join(format!("{version}/seed-{seed}"))
    }

    /// The capture output directory for a labelled run.
    pub fn capture_dir(&self, label: &str) -> PathBuf {
        self.captures.join(label)
    }
}

/// An environment value counts as set only when it names something.
///
/// A variable exported empty (`export DUST_HARNESS_CACHE=`) is a shell
/// accident, not a decision to write into the current directory.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// The cache root when nothing is set explicitly.
fn default_root(xdg: Option<String>, home: Option<&str>) -> PathBuf {
    if let Some(xdg) = xdg {
        return Path::new(&xdg).join("dust-harness");
    }
    match home {
        Some(home) => {
            // XDG_CACHE_HOME unset: `$HOME/.cache` on the Unixes, the library
            // directory Apple documents for caches on macOS. Both are outside
            // anything a git command touches.
            if cfg!(target_os = "macos") {
                Path::new(home).join("Library/Caches/dust-harness")
            } else {
                Path::new(home).join(".cache/dust-harness")
            }
        }
        // No HOME either. Refusing to guess would strand the harness; landing
        // in the temporary directory keeps runs working and keeps the tree out
        // of the repository, which is the property that matters here.
        None => std::env::temp_dir().join("dust-harness"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_override_wins_over_every_default() {
        assert_eq!(
            default_root(Some("/mnt/big-cache".to_owned()), Some("/home/op")),
            PathBuf::from("/mnt/big-cache/dust-harness")
        );
    }

    #[test]
    fn xdg_unset_falls_through_to_the_platform_location() {
        let home = "/home/op";
        let expected = if cfg!(target_os = "macos") {
            "/home/op/Library/Caches/dust-harness"
        } else {
            "/home/op/.cache/dust-harness"
        };
        assert_eq!(default_root(None, Some(home)), PathBuf::from(expected));
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        assert_eq!(non_empty(Some(String::new())), None);
        assert_eq!(
            non_empty(Some("   ".to_owned())),
            None,
            "whitespace names nothing either"
        );
        assert_eq!(non_empty(Some("/x".to_owned())).as_deref(), Some("/x"));
        // With the variable filtered away, the platform location applies.
        let expected = if cfg!(target_os = "macos") {
            "/home/op/Library/Caches/dust-harness"
        } else {
            "/home/op/.cache/dust-harness"
        };
        assert_eq!(
            default_root(None, Some("/home/op")),
            PathBuf::from(expected)
        );
    }

    #[test]
    fn seeds_get_separate_server_directories() {
        let layout = Layout::under(PathBuf::from("/cache"));
        assert_eq!(
            layout.server_dir("1.21.1", 0),
            PathBuf::from("/cache/servers/1.21.1/seed-0")
        );
        assert_ne!(
            layout.server_dir("1.21.1", 0),
            layout.server_dir("1.21.1", 1),
            "two seeds must never share a world directory"
        );
    }

    #[test]
    fn negative_seeds_name_directories_that_round_trip() {
        // Vanilla accepts signed 64-bit seeds and `-5` is a legitimate one;
        // the path component has to survive being written and looked up again.
        let layout = Layout::under(PathBuf::from("/cache"));
        assert_eq!(
            layout.server_dir("1.21.1", -5),
            PathBuf::from("/cache/servers/1.21.1/seed--5")
        );
    }
}

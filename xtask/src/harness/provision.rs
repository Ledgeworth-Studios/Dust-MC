//! `harness provision` — a vanilla server, ready to boot.
//!
//! Three artefacts, each with one owner:
//!
//! - **The jar** is owned by the extractor's resolver: manifest lookup,
//!   download on demand, SHA-1 verified against the manifest *on every run*,
//!   cached copy re-checked rather than trusted. Reusing it means the harness
//!   and the data extraction can never disagree about which bytes are
//!   "vanilla 1.21.1".
//! - **The run directory** holds what the server itself will write. One per
//!   (version, seed), so no run ever inherits another seed's world — see
//!   [`cache::Layout::server_dir`] for why that cannot be relaxed.
//! - **The two operator decisions** are `eula.txt` and `server.properties`.
//!   The properties file is generated wholesale so the determinism contract
//!   lives in code ([`properties`]); the EULA is never accepted implicitly.
//!
//! # Why not the official launcher bundle
//!
//! Mojang also publishes a `server.jar` *bundle* that unpacks libraries before
//! first boot. The manifest's plain server artifact already is that bundler —
//! it self-unpacks into its working directory on first run, which is exactly
//! how the extractor runs its data generators. Pointing java at this jar with
//! the working directory set to the run directory gets the full layout for
//! free, with no second download format to verify or explain.

use std::path::{Path, PathBuf};

use super::{cache, properties};

#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub seed: i64,
    /// A jar the operator has already obtained, instead of downloading.
    pub jar: Option<PathBuf>,
    /// Accept Minecraft's EULA on the operator's behalf by writing eula.txt.
    ///
    /// Deliberately explicit and deliberately not implied by anything else:
    /// agreeing to a licence is an act, and a flag that can be forgotten is a
    /// flag that makes the act visible in shell history where it belongs.
    pub yes: bool,
}

/// Ensure everything `capture` will need exists, printing what was done.
pub fn run(options: &Options) -> Result<(), String> {
    let layout = cache::Layout::resolve()?;
    println!(
        "provisioning Minecraft {} seed {} under {}",
        options.version,
        options.seed,
        layout.root.display()
    );

    if let Some(path) = &options.jar {
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        // No digest to check against: the jar did not come through the
        // manifest, and inventing a check would be worse than saying so.
        // Same stance as `extract --server-jar`.
        println!(
            "using the operator-supplied jar at {} (no SHA-1 check: it did not come \
             through the manifest)",
            path.display()
        );
    } else {
        crate::extract::download::server_jar(&options.version, &layout.jars)?;
    }

    let dir = layout.server_dir(&options.version, options.seed);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    write_properties(
        &dir,
        &properties::Settings {
            seed: options.seed,
            rcon_port: properties::RCON_PORT,
        },
    )?;
    accept_eula_if_allowed(&dir, options.yes)?;

    println!("\nready: {}", dir.display());
    if !properties::eula_accepted(&dir)? {
        println!(
            "the EULA is NOT accepted yet. Read it (the server links it when it refuses to \
             start), then run the same command again with --yes to accept on your behalf."
        );
    } else {
        println!(
            "next: cargo xtask harness capture --version {} --seed {} --radius <chunks>",
            options.version, options.seed
        );
    }
    Ok(())
}

/// Write server.properties unconditionally.
///
/// Idempotent by construction: the content is a pure function of the settings,
/// so provisioning twice writes identical bytes. An existing world directory
/// is left entirely alone — but a seed change against an existing world is
/// called out loudly, because vanilla would silently ignore the new seed and
/// the next capture would fingerprint the old world under the new seed's name.
fn write_properties(dir: &Path, settings: &properties::Settings) -> Result<(), String> {
    let path = dir.join("server.properties");
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let text = properties::render(settings);
    std::fs::write(&path, &text).map_err(|e| format!("could not write {}: {e}", path.display()))?;

    match previous
        .lines()
        .find(|l| l.starts_with("level-seed="))
        .map(str::to_owned)
    {
        Some(previous_seed) if previous_seed != format!("level-seed={}", settings.seed) => {
            println!(
                "wrote {} over an earlier seed ({previous_seed}); note vanilla ignores the \
                 seed of an existing world, so delete {}/world if you meant to regenerate",
                path.display(),
                dir.display()
            );
        }
        _ => println!("wrote {}", path.display()),
    }
    Ok(())
}

/// Write eula.txt only behind `--yes`; otherwise leave the decision visible.
fn accept_eula_if_allowed(dir: &Path, yes: bool) -> Result<(), String> {
    let path = dir.join("eula.txt");
    if properties::eula_accepted(dir)? {
        println!("eula.txt already accepts the EULA");
        return Ok(());
    }
    if !yes {
        return Ok(());
    }
    std::fs::write(&path, properties::eula_text())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!(
        "wrote {} — the operator accepted Minecraft's EULA via --yes",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioning_twice_writes_identical_bytes() {
        // Determinism of the tool itself: nothing in a provisioned directory
        // may depend on how many times the command ran.
        let dir = crate::harness::testing::scratch_dir("provision-idempotent");
        let settings = properties::Settings::default();
        write_properties(&dir, &settings).expect("first write");
        let first = std::fs::read_to_string(dir.join("server.properties")).expect("read");
        write_properties(&dir, &settings).expect("second write");
        let second = std::fs::read_to_string(dir.join("server.properties")).expect("read");
        assert_eq!(first, second);
    }

    #[test]
    fn a_seed_change_against_an_existing_world_is_reported() {
        // The output is the operator's only warning that the world on disk
        // predates the seed they just asked for; assert it fires.
        let dir = crate::harness::testing::scratch_dir("provision-reseed");
        write_properties(&dir, &properties::Settings::default()).expect("seed zero");
        write_properties(
            &dir,
            &properties::Settings {
                seed: 99,
                ..properties::Settings::default()
            },
        )
        .expect("reseed");
        let text = std::fs::read_to_string(dir.join("server.properties")).expect("read");
        assert_eq!(
            super::super::properties::value_of(&text, "level-seed"),
            Some("99")
        );
    }
}

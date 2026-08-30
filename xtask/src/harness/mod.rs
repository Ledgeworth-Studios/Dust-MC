//! `cargo xtask harness <verb>` — the groundwork of differential testing.
//!
//! Testing.md names differential testing against vanilla the highest-value test
//! this project will have: run the real server and Dust over identical inputs,
//! compare what each produced, and let Mojang's implementation argue with ours.
//! Phase 6 stands on it. This module is the part that does not need Dust to
//! exist yet — the machinery to provision a vanilla server, drive it, read the
//! world it wrote, and fingerprint that world chunk by chunk.
//!
//! # The verbs, as one pipeline
//!
//! ```text
//! harness provision --version 1.21.1 --seed 0 --yes   jar + run dir + properties
//! harness capture --version 1.21.1 --seed 0 --radius 2  boot → pregen → digests
//! harness compare A B                                 diff two digest sets
//! harness rewrite --version 1.21.1 --seed 0 --radius 2  Dust writes it, vanilla reads it
//! ```
//!
//! `rewrite` is the one verb that puts Dust's own code in the loop, and it is
//! Phase 2's exit criterion made runnable — see [`rewrite`] for what a green
//! run does and does not prove.
//!
//! `harness rcon` stands alone as a small client for talking to a running
//! server (`/stop`, status queries), which both `capture` and operators use.
//!
//! # Licensing, stated where the code lives
//!
//! Nothing Mojang ships is ever committed. The jar arrives by download at
//! run time (or from an operator-supplied path), is verified against the
//! manifest's SHA-1 on every use, and lives in the cache directory outside the
//! repository — see [`cache`]. Worlds generated during runs and the digests
//! read out of them stay there too. What may be committed is this code, its
//! tests, and nothing derived from the game's data.
//!
//! # Determinism, honestly
//!
//! Vanilla world generation for a fixed seed and fixed version is stable:
//! terrain, biomes, ore and feature placement, structure placement. That is
//! what the digest captures. Everything clock-shaped is excluded *by
//! construction*, not filtered after the fact: no player joins, so weather and
//! mob cycles never start ticking; container loot is not rolled until opened;
//! entities live outside region files entirely and are not read; block
//! entities are ignored. The capture reads the saved world once, after a full
//! forced pregeneration and an explicit flush, so disk state is settled.
//!
//! What remains genuinely uncontrolled — JIT, GC timing, thread scheduling —
//! does not reach the bytes being hashed. The JVM flags chosen in
//! [`capture`] favour short, boring runs over micro-optimisation.
//!
//! # What the NBT reader here is
//!
//! A throwaway. `dust-nbt` is not implemented on this base, so [`nbt`] carries
//! a harness-local reader covering exactly the tags a chunk compound contains,
//! and [`region`] covers exactly the anvil layout vanilla writes. Both are
//! tested against synthetic bytes constructed in these tests, not against
//! Mojang files; when `dust-nbt` lands, both are deleted in favour of it.

pub mod cache;
pub mod capture;
pub mod compare;
pub mod digest;
mod nbt;
mod properties;
mod provision;
mod rcon;
mod region;
pub mod registries;
mod rewrite;
mod wire;

use std::process::ExitCode;

/// Usage text for the group, printed on bad arguments and `-h`.
///
/// Every verb documents its own contract here rather than in a man page that
/// can drift from the parser sitting beside it.
pub const USAGE: &str = "\
cargo xtask harness <verb>

Differential-testing groundwork: provision a vanilla server, capture a
fingerprint of a world it generates, compare fingerprints. Needs Java 21+ on
PATH; downloads the server jar through Mojang's manifest into a cache kept
outside the repository (override with DUST_HARNESS_CACHE).

  provision --version <v> [--seed <n>] [--jar <path>] [--yes]
      Ensure a cached server jar (SHA-1 verified against the manifest) and a
      run directory tuned for headless determinism exist for this version and
      seed. With --yes, accept Minecraft's EULA on your behalf by writing
      eula.txt; without it, the file is left unwritten and vanilla will refuse
      to boot until you have read the EULA and chosen.

  rcon [--host <h>] [--port <p>] [--password <pw>] <command> [<command>...]
      Send commands to a running server's RCON port and print the responses.
      Defaults match what provision writes: 127.0.0.1:25575, password
      dust-harness.

  capture --version <v> --seed <n> --radius <r> [--jar <path>] [--timeout <s>]
      Boot the provisioned server headless, force-generate the square of
      chunks within <r> chunks of spawn, flush and stop it, then hash every
      chunk directly out of the region files: a block-state multiset digest, a
      biome digest and per-heightmap digests per chunk. Writes chunks.bin plus
      a human-readable chunks.tsv into the cache. Refuses to run before
      `provision` has accepted the EULA.

  rewrite --version <v> --seed <n> --radius <r> [--jar <path>] [--timeout <s>]
      Copy the provisioned world, rewrite every chunk of it through Dust's
      own Anvil reader and writer, boot vanilla on the copy, and diff what
      vanilla read back against this (version, seed, radius)'s capture. The
      capture has to exist first. Exit codes match compare's: 0 the worlds
      match, 1 they differ, 2 the run could not happen.

  compare <a> <b> [--tsv <path>]
      Diff two capture outputs (directories holding chunks.bin). Prints
      per-chunk rows — missing, extra, divergent, each with the digest pairs
      — and a totals line. Exit codes: 0 identical, 1 they differ (a finding,
      not a failure), 2 the comparison could not run. Refuses sets from
      different seeds or different data versions outright.

  registries --version <v> [--data <dir>] [--timeout <s>]
      Boot Minecraft, boot Dust, and ask both what they tell a client that
      acknowledges no data packs: the synced registries with their contents,
      and the whole tag set. Compares registries as trees and tags as sets —
      the two servers write compounds and tags in different orders and a
      client builds a map and a set either way. Exit 0 if they agree, 1 if
      they do not, 2 if the run failed.
";

/// Which verb was selected, with its parsed options.
#[derive(Debug)]
enum Verb {
    Provision(provision::Options),
    Rcon(rcon::ClientOptions),
    Capture(capture::Options),
    Compare(compare::Options),
    Rewrite(rewrite::Options),
    Registries(registries::Options),
}

/// Parse and run one harness verb.
///
/// Asking for help is not an error: it prints this group's usage on stdout
/// and exits successfully, like every other command here.
pub fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    if args
        .first()
        .is_none_or(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let verb = parse(args)?;
    match verb {
        Verb::Provision(options) => {
            provision::run(&options)?;
            Ok(ExitCode::SUCCESS)
        }
        Verb::Rcon(options) => rcon::run_client(&options).map(|_| ExitCode::SUCCESS),
        Verb::Capture(options) => {
            capture::run(&options)?;
            Ok(ExitCode::SUCCESS)
        }
        // Neither compare nor rewrite fails upward: their exit codes carry
        // both the verdict (0/1) and their own operational failures (2). A
        // difference between two worlds is a finding to be read, not an error
        // to be reported in one line with the table thrown away.
        Verb::Compare(options) => Ok(compare::run(&options)),
        Verb::Rewrite(options) => Ok(rewrite::run(&options)),
        Verb::Registries(options) => Ok(registries::run(&options)),
    }
}

fn parse(args: &[String]) -> Result<Verb, String> {
    let verb = args
        .first()
        .ok_or_else(|| format!("harness needs a verb\n\n{USAGE}"))?;
    let rest = &args[1..];
    match verb.as_str() {
        "provision" => parse_provision(rest).map(Verb::Provision),
        "rcon" => rcon::parse_client_options(rest).map(Verb::Rcon),
        "capture" => capture::parse(rest).map(Verb::Capture),
        "compare" => compare::parse(rest).map(Verb::Compare),
        "rewrite" => rewrite::parse(rest).map(Verb::Rewrite),
        "registries" => registries::parse(rest).map(Verb::Registries),
        other => Err(format!(
            "unknown harness verb `{other}`\n\nThe verbs are: provision, rcon, capture, \
             compare, rewrite, registries."
        )),
    }
}

/// The flag-walking pattern shared by the parsers: take a value after a flag,
/// refuse duplicates, refuse unknowns with the usage text.
fn take_value(
    seen: &mut Vec<(&'static str, String)>,
    name: &'static str,
    rest: &[String],
    at: usize,
) -> Result<usize, String> {
    let value = rest
        .get(at)
        .ok_or_else(|| format!("{name} needs a value"))?;
    if seen.iter().any(|(k, _)| *k == name) {
        return Err(format!("{name} given twice"));
    }
    seen.push((name, value.clone()));
    Ok(at + 1)
}

// Each verb's parser lives beside its runner; the helpers below are only the
// shared plumbing they call back into.

fn parse_provision(args: &[String]) -> Result<provision::Options, String> {
    let mut version = None;
    let mut seed = properties::Settings::default().seed;
    let mut jar = None;
    let mut yes = false;
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            // Value-taking arms advance `at` past their value via
            // `take_value`; a flag arm advances by one itself.
            "--version" => {
                at = take_value(&mut seen, "--version", args, at + 1)?;
                version = Some(seen.last().expect("just stored").1.clone());
            }
            "--seed" => {
                at = take_value(&mut seen, "--seed", args, at + 1)?;
                seed = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--seed needs a signed 64-bit integer")?;
            }
            "--jar" => {
                at = take_value(&mut seen, "--jar", args, at + 1)?;
                jar = Some(std::path::PathBuf::from(
                    seen.last().expect("just stored").1.clone(),
                ));
            }
            "--yes" => {
                yes = true;
                at += 1;
            }
            other => return Err(format!("unknown provision option `{other}`\n\n{USAGE}")),
        }
    }
    let version = version.ok_or_else(|| {
        format!("provision needs --version, e.g. `harness provision --version 1.21.1`\n\n{USAGE}")
    })?;
    Ok(provision::Options {
        version,
        seed,
        jar,
        yes,
    })
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn every_verb_parses_from_its_documented_spelling() {
        assert!(matches!(
            parse(&args(&[
                "provision",
                "--version",
                "1.21.1",
                "--seed",
                "7",
                "--yes"
            ])),
            Ok(Verb::Provision(_))
        ));
        assert!(matches!(
            parse(&args(&[
                "capture",
                "--version",
                "1.21.1",
                "--seed",
                "0",
                "--radius",
                "2"
            ])),
            Ok(Verb::Capture(_))
        ));
        assert!(matches!(
            parse(&args(&["compare", "/a", "/b"])),
            Ok(Verb::Compare(_))
        ));
        assert!(matches!(parse(&args(&["rcon", "list"])), Ok(Verb::Rcon(_))));
    }

    #[test]
    fn a_missing_verb_and_an_unknown_verb_both_name_the_problem() {
        assert!(parse(&args(&[]))
            .expect_err("empty")
            .contains("needs a verb"));
        assert!(parse(&args(&["provison"]))
            .expect_err("typo")
            .contains("unknown harness verb `provison`"));
    }

    #[test]
    fn provision_refuses_to_run_without_a_version() {
        assert!(parse(&args(&["provision", "--seed", "3"]))
            .expect_err("missing")
            .contains("--version"));
    }

    #[test]
    fn a_flag_at_the_end_of_the_world_is_an_error_not_a_panic() {
        assert!(parse(&args(&["provision", "--version"])).is_err());
        assert!(parse(&args(&["capture", "--radius"])).is_err());
    }

    #[test]
    fn a_repeated_flag_is_refused_rather_than_last_one_wins() {
        let err = parse(&args(&[
            "provision",
            "--version",
            "1.21.1",
            "--seed",
            "1",
            "--seed",
            "2",
        ]))
        .expect_err("duplicate");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn negative_seeds_parse() {
        let Verb::Provision(options) =
            parse(&args(&["provision", "--version", "1.21.1", "--seed", "-5"])).expect("parses")
        else {
            panic!("expected provision");
        };
        assert_eq!(options.seed, -5);
    }
}

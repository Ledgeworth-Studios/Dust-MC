//! `cargo xtask extract --version 1.21.1`.
//!
//! Minecraft's own data generators emit the block state table, the registries,
//! the recipes and the packet report. Dust needs that information and may not
//! redistribute the files it arrives in, so the pipeline is: download the
//! server jar on this machine, run its generators, read the reports, and commit
//! the *Rust* that results.
//!
//! This runs by hand, a few times per Minecraft release. It is deliberately not
//! part of `just verify` — it needs a network, a JVM and a fifty-megabyte
//! download, and CI has no business doing any of that. What CI checks is the
//! generated code, which is committed, compiles, and has the round-trip test
//! over every state in `dust-registry`.
//!
//! Two of the jar's generators are needed and they write different trees.
//! `--reports` produces the block state table; `--server` produces the worldgen
//! data, which is where the vanilla ore baseline in `dust-gen` comes from. Each
//! is cached on a path only that generator writes, so having one does not look
//! like having both.

mod blocks;
mod codegen;
mod download;
mod sha1;
mod worldgen;

use std::path::{Path, PathBuf};

/// Where the server jar and the generated reports are cached. Gitignored, and
/// outside `target/` so that `cargo clean` does not throw away a fifty-megabyte
/// download.
const CACHE_DIR: &str = ".dust-extract";

pub struct Options {
    pub version: String,
    /// A server jar the operator has already obtained, instead of downloading.
    pub server_jar: Option<PathBuf>,
}

pub fn run(options: &Options, workspace_root: &Path) -> Result<(), String> {
    let cache = workspace_root.join(CACHE_DIR);
    let version = &options.version;

    let jar = match &options.server_jar {
        Some(path) => {
            if !path.exists() {
                return Err(format!("{} does not exist", path.display()));
            }
            println!("using the server jar at {}", path.display());
            path.clone()
        }
        None => download::server_jar(version, &cache)?,
    };

    let reports = generate(
        &jar,
        &cache.join(format!("reports-{version}")),
        "--reports",
        "reports/blocks.json",
    )?;
    let server_data = generate(
        &jar,
        &cache.join(format!("data-{version}")),
        "--server",
        "data/minecraft/worldgen/placed_feature",
    )?;

    let block_json = std::fs::read(reports.join("reports/blocks.json"))
        .map_err(|e| format!("could not read the generated block report: {e}"))?;
    let parsed = blocks::parse(&block_json)?;
    println!(
        "read {} blocks and {} states from the block report",
        parsed.blocks.len(),
        parsed.state_count
    );

    let generated = workspace_root.join("crates/dust-registry/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("blocks.rs");
    std::fs::write(&path, codegen::blocks(&parsed, version, &parsed.reported))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    ores(&server_data.join("data"), workspace_root, version)?;

    println!(
        "\nRun `cargo test -p dust-registry -p dust-gen` — the round-trip over all {} \
         states and the ore baseline's source-row check are what say this worked.",
        parsed.state_count
    );
    Ok(())
}

/// Read the vanilla ore baseline out of the `--server` tree and write it into
/// `dust-gen`.
fn ores(data_root: &Path, workspace_root: &Path, version: &str) -> Result<(), String> {
    let ores = worldgen::parse(data_root)?;
    println!(
        "\nread {} ore placements in {} group(s) across {} dimension(s)",
        ores.placements.len(),
        ores.groups.len(),
        ores.dimensions.len()
    );

    // Which groups the configuration's hand-written vanilla list knows about,
    // said out loud rather than left for someone to work out from the table.
    // The half it does not know about are the stone variants and terrain blobs,
    // and they are knobs too — see xtask/src/extract/worldgen.rs.
    let known: Vec<&str> = ores
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| dust_config::ore::VANILLA_ORE_GROUPS.contains(n))
        .collect();
    let rest: Vec<&str> = ores
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| !dust_config::ore::VANILLA_ORE_GROUPS.contains(n))
        .collect();
    println!("  ores the configuration documents: {}", known.join(", "));
    println!("  other groups the ore feature places: {}", rest.join(", "));
    if ores.ungrouped.is_empty() {
        println!("  every placement was grouped");
    } else {
        println!(
            "  NOT GROUPED, and therefore not in the table — no setting will reach \
             these: {}",
            ores.ungrouped.join(", ")
        );
    }

    let generated = workspace_root.join("crates/dust-gen/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("ores.rs");
    std::fs::write(&path, codegen::ores(&ores, version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Run one of the server jar's own data generators.
///
/// `marker` is a path inside the output that only exists once that generator
/// has run, so the two trees cache independently: `--reports` and `--server`
/// write different things, and having one already does not mean having the
/// other.
fn generate(jar: &Path, output: &Path, generator: &str, marker: &str) -> Result<PathBuf, String> {
    if output.join(marker).exists() {
        println!(
            "using the cached {generator} output in {}",
            output.display()
        );
        return Ok(output.to_path_buf());
    }

    println!("running Minecraft's {generator} data generator (this takes a minute)");
    let status = std::process::Command::new("java")
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(jar)
        .arg(generator)
        .arg("--output")
        .arg(output)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "java was not found on PATH. Minecraft 1.21.1's data generators need a JDK of \
                 21 or newer."
                    .to_owned()
            }
            _ => format!("could not run java: {e}"),
        })?;

    if !status.success() {
        return Err(format!(
            "Minecraft's data generators exited with {status}. If the message above mentions \
             a class version, the JDK on PATH is older than 21."
        ));
    }
    Ok(output.to_path_buf())
}

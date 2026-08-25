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

mod blocks;
mod codegen;
mod download;
mod sha1;

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

    let reports = generate_reports(&jar, &cache.join(format!("reports-{version}")))?;

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

    println!(
        "\nRun `cargo test -p dust-registry` — the round-trip over all {} states is what \
         says this worked.",
        parsed.state_count
    );
    Ok(())
}

/// Run the server jar's own data generators.
fn generate_reports(jar: &Path, output: &Path) -> Result<PathBuf, String> {
    if output.join("reports/blocks.json").exists() {
        println!("using the cached reports in {}", output.display());
        return Ok(output.to_path_buf());
    }

    println!("running Minecraft's data generators (this takes a minute)");
    let status = std::process::Command::new("java")
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(jar)
        .arg("--reports")
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

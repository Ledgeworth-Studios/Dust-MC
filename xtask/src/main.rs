//! Build tooling. `cargo xtask <command>`.
//!
//! Everything here is something `just verify` runs, so that the local gate and
//! the remote gate execute the same code rather than two descriptions of it.

mod extract;
mod harness;
mod licenses;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
cargo xtask <command>

  docs [--check]   Generate docs/configuration.md from the configuration types.
                   With --check, verify the committed file matches and fail if
                   it does not, rather than rewriting it.
  licenses         Audit every dependency's licence for GPL-3.0 compatibility.

  extract --version <v> [--server-jar <path>] [--only <domain,domain>]
                   Download the Minecraft server jar for <v>, run its own data
                   generators, and regenerate the tables in dust-registry and
                   dust-protocol from the reports. Needs a network and a JDK 21
                   or newer. Not part of `just verify`, and not something CI
                   runs. With --only, extract just the named domains (blocks,
                   items, packets, worldgen, mappings) instead of all of them.
                   `mappings` and `light` are the odd ones: they read the
                   jar and the obfuscated-name table published beside it
                   rather than anything the data generators produced, and
                   write to the extract cache rather than to crates/.
                   `light` runs a small Java program on Minecraft's own
                   classpath and asks it what every block state's opacity and
                   emission are — constants that appear in no report and no
                   data pack. Nothing either produces is committed. See
                   decision record 0008.

  harness <verb>   Differential-testing groundwork against vanilla: provision
                   a cached server, capture a fingerprint of a world it
                   generates, compare two fingerprints. Has its own usage —
                   run `cargo xtask harness` to see it. Not part of `just
                   verify`; needs Java on PATH and (once) a network.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The harness owns its exit codes (compare reports a *finding* with 1 and
    // keeps 2 for failures), so it returns one rather than mapping onto the
    // success/failure pair every other command uses.
    if args.first().map(String::as_str) == Some("harness") {
        return match harness::dispatch(&args[1..]) {
            Ok(code) => code,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::FAILURE
            }
        };
    }

    let result = match args.first().map(String::as_str) {
        Some("docs") => docs(args.iter().any(|a| a == "--check")),
        Some("licenses") => audit_licenses(),
        Some("extract") => extract_data(&args[1..]),
        Some("--help" | "-h") | None => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn docs(check: bool) -> Result<(), String> {
    let path = workspace_root().join("docs/configuration.md");
    let generated = dust_config::docs::render();

    if !check {
        std::fs::create_dir_all(path.parent().expect("has a parent"))
            .map_err(|e| format!("could not create docs/: {e}"))?;
        std::fs::write(&path, &generated)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed == generated {
        println!("{} is up to date", path.display());
        Ok(())
    } else {
        Err(format!(
            "{} is out of date with the configuration types.\n\
             Run `cargo xtask docs` and commit the result — the reference and \
             the types move in the same commit or not at all.",
            path.display()
        ))
    }
}

fn extract_data(args: &[String]) -> Result<(), String> {
    let mut version = None;
    let mut server_jar = None;
    let mut only = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--version" => {
                version = Some(
                    rest.next()
                        .ok_or("--version needs a Minecraft version, e.g. 1.21.1")?
                        .clone(),
                )
            }
            "--server-jar" => {
                server_jar = Some(PathBuf::from(
                    rest.next().ok_or("--server-jar needs a path")?,
                ))
            }
            "--only" => {
                let list = rest.next().ok_or("--only needs a comma-separated list")?;
                only = extract::parse_only(list)?;
            }
            other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
        }
    }
    let version = version.ok_or_else(|| {
        format!("extract needs --version, e.g. `cargo xtask extract --version 1.21.1`\n\n{USAGE}")
    })?;
    extract::run(
        &extract::Options {
            version,
            server_jar,
            only,
        },
        &workspace_root(),
    )
}

fn audit_licenses() -> Result<(), String> {
    let output = std::process::Command::new(std::env::var("CARGO").as_deref().unwrap_or("cargo"))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not run `cargo metadata`: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let metadata: licenses::Metadata = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("could not read `cargo metadata` output: {e}"))?;

    let rejections = licenses::audit(&metadata);
    if rejections.is_empty() {
        println!(
            "{} dependencies, all licences compatible with GPL-3.0",
            metadata.packages.len()
        );
        return Ok(());
    }

    let mut message = format!("{} dependency licence problem(s):\n", rejections.len());
    for rejection in &rejections {
        message.push_str(&format!("  {rejection}\n"));
    }
    message.push_str(
        "\nSee docs/decisions/0002-license.md. A dependency that cannot be used has to be \
         replaced, not waived.",
    );
    Err(message)
}

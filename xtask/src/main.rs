//! Build tooling. `cargo xtask <command>`.
//!
//! Everything here is something `just verify` runs, so that the local gate and
//! the remote gate execute the same code rather than two descriptions of it.

mod licenses;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
cargo xtask <command>

  docs [--check]   Generate docs/configuration.md from the configuration types.
                   With --check, verify the committed file matches and fail if
                   it does not, rather than rewriting it.
  licenses         Audit every dependency's licence for GPL-3.0 compatibility.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("docs") => docs(args.iter().any(|a| a == "--check")),
        Some("licenses") => audit_licenses(),
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

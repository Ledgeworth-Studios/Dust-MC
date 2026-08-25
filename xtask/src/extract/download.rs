//! Resolving a Minecraft version to a server jar, and fetching it.
//!
//! Nothing this module downloads is ever committed. The jar lands in a local
//! cache directory that `.gitignore` covers, and it is the operator's copy of
//! a file Mojang distributes to them — see the project's Code Provenance
//! document. What the repository keeps is this code and the Rust generated from
//! what it reads.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::sha1;

const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Deserialize)]
struct VersionManifest {
    versions: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct VersionDetail {
    downloads: Downloads,
}

#[derive(Debug, Deserialize)]
struct Downloads {
    server: Artifact,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    url: String,
    sha1: String,
    size: u64,
}

/// Fetch a URL's body.
///
/// Shelling out to `curl` rather than taking an HTTP client as a dependency.
/// This runs on a developer's machine, by hand, a few times per Minecraft
/// release; it is not on any path the server takes. A TLS stack and its
/// transitive tree is a large thing to add to a workspace that audits its own
/// dependency licences on every build, in exchange for a request made twice a
/// year.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let output = std::process::Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "600",
            url,
        ])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "curl was not found on PATH, and the extractor uses it to fetch Mojang's \
                 published files. Install curl, or download the server jar yourself and pass \
                 --server-jar."
                    .to_owned()
            }
            _ => format!("could not run curl: {e}"),
        })?;

    if !output.status.success() {
        return Err(format!(
            "could not fetch {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let body = fetch(url)?;
    serde_json::from_slice(&body).map_err(|e| format!("could not read the JSON at {url}: {e}"))
}

/// Resolve a version id to its server jar, download it if the cache does not
/// already hold it, and verify the digest either way.
///
/// The cached copy is verified on every run, not only when it is fetched. A
/// cache that is checked when written and trusted forever afterwards is a cache
/// that silently serves a truncated file for the rest of its life.
pub fn server_jar(version: &str, cache: &Path) -> Result<PathBuf, String> {
    let manifest: VersionManifest = fetch_json(VERSION_MANIFEST)?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| format!("Mojang's manifest lists no version `{version}`"))?;

    let detail: VersionDetail = fetch_json(&entry.url)?;
    let artifact = &detail.downloads.server;

    std::fs::create_dir_all(cache)
        .map_err(|e| format!("could not create {}: {e}", cache.display()))?;
    let path = cache.join(format!("server-{version}.jar"));

    if path.exists() {
        match verify(&path, artifact) {
            Ok(()) => {
                println!("using the cached server jar at {}", path.display());
                return Ok(path);
            }
            Err(why) => {
                println!("the cached server jar is unusable ({why}); fetching it again");
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    println!(
        "downloading the {version} server jar ({} MiB)",
        artifact.size / (1024 * 1024)
    );
    let body = fetch(&artifact.url)?;
    std::fs::write(&path, &body).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    verify(&path, artifact)?;
    Ok(path)
}

fn verify(path: &Path, artifact: &Artifact) -> Result<(), String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if bytes.len() as u64 != artifact.size {
        return Err(format!(
            "it is {} bytes and should be {}",
            bytes.len(),
            artifact.size
        ));
    }
    let digest = sha1::hex(&bytes);
    if digest != artifact.sha1 {
        return Err(format!(
            "its SHA-1 is {digest} and should be {}",
            artifact.sha1
        ));
    }
    Ok(())
}

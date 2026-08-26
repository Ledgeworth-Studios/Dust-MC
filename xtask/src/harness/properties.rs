//! `server.properties`, written rather than hand-edited.
//!
//! The harness boots vanilla headless and reads the world it saves, so the
//! properties file is part of the experiment: every knob that could make two
//! runs of the same seed differ, or make the run depend on a human being
//! present, is pinned here in one place. The server fills in everything this
//! leaves out with its own defaults.
//!
//! Two choices deserve their reasoning written down:
//!
//! - **`pause-when-empty-seconds` is set to a huge positive number.** Since
//!   1.20.3 vanilla pauses the tick loop when no player has been online for
//!   this many seconds, and chunk generation for force-loaded chunks runs on
//!   that loop. The default of 60 would pause the pregeneration this harness
//!   depends on. A huge *positive* value says "never" without relying on the
//!   sign convention of 0 or negatives, which changed across releases.
//! - **`sync-chunk-writes=false`.** Writes are flushed by `save-all flush`
//!   before anything is read, which is both faster and more explicit than
//!   paying an fsync per chunk as it is written.

use std::path::Path;

/// The RCON password provisioned into every harness server.
///
/// Fixed and documented because the alternative — asking the operator to invent
/// one per run — guarantees it lands in a shell history somewhere. The server
/// binds its game port to the loopback interface; RCON itself has no bind
/// option and listens on all interfaces on machines where that matters, so
/// this value exists to be obvious about what it protects: nothing. Run the
/// harness on a machine whose port 25575 you do not mind being reachable.
pub const RCON_PASSWORD: &str = "dust-harness";

/// The RCON port provisioned unless overridden.
pub const RCON_PORT: u16 = 25575;

/// Everything `provision` needs to render the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub seed: i64,
    pub rcon_port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seed: 0,
            rcon_port: RCON_PORT,
        }
    }
}

/// Render the whole file exactly as it should sit on disk.
///
/// Keys are emitted sorted, which is also the order vanilla's own writer
/// produces, so a server-initiated rewrite of this file moves as few lines as
/// possible. The comment header will be dropped by any such rewrite; the
/// authoritative statement of these choices is this module, not the file.
pub fn render(settings: &Settings) -> String {
    let mut out = String::from(
        "# Written by `cargo xtask harness provision` for Dust's differential harness.\n",
    );
    let mut keys: Vec<(&str, String)> = vec![
        ("allow-nether", "true".to_owned()),
        // No player ever joins; peaceful keeps mob spawning out of the
        // picture entirely rather than merely unobserved.
        ("difficulty", "peaceful".to_owned()),
        ("enable-rcon", "true".to_owned()),
        (
            // With online-mode off, clients cannot sign chat; leaving this at
            // its default true only produces warnings nobody can act on.
            "enforce-secure-profile",
            "false".to_owned(),
        ),
        ("generate-structures", "true".to_owned()),
        ("initial-enabled-packs", "vanilla".to_owned()),
        ("level-name", "world".to_owned()),
        ("level-seed", settings.seed.to_string()),
        // Escaped the way Java's properties reader and vanilla's writer both
        // spell it; see `value_of`, which undoes exactly this escaping.
        ("level-type", r"minecraft\:normal".to_owned()),
        ("motd", "Dust differential harness".to_owned()),
        ("online-mode", "false".to_owned()),
        ("pause-when-empty-seconds", "1000000000".to_owned()),
        ("rcon.password", RCON_PASSWORD.to_owned()),
        ("rcon.port", settings.rcon_port.to_string()),
        ("server-ip", "127.0.0.1".to_owned()),
        ("simulation-distance", "5".to_owned()),
        ("spawn-protection", "0".to_owned()),
        ("sync-chunk-writes", "false".to_owned()),
        ("view-distance", "8".to_owned()),
    ];
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in &keys {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

/// Read one key back out of a properties-format file.
///
/// Deliberately minimal: this verifies what `provision` wrote (or what an
/// operator edited it into) before `capture` trusts it. Handles comments,
/// `=` and `:` separators, surrounding whitespace, and the `\x` escape form
/// Java's reader defines — which is how vanilla spells `minecraft\:normal`.
pub fn value_of<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        // A separator-less line is malformed but not ours to police; skip it
        // rather than abandoning the whole search.
        let Some(split_at) = line.find(['=', ':']) else {
            continue;
        };
        let candidate = line[..split_at].trim_end();
        if candidate != key {
            continue;
        }
        let value = line[split_at + 1..].trim_start();
        return Some(value);
    }
    None
}

/// Undo the single-character escapes Java's properties reader applies.
///
/// Only used on values read back by [`value_of`]; block names contain none of
/// the characters that would need it, but `level-type` arrives as the escaped
/// spelling and comparisons should not care.
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => break,
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The eula.txt contents when the operator has accepted on their own behalf.
///
/// Vanilla reads only the `eula` key; the second line records who agreed and
/// through which flag, so an acceptance is never anonymous.
pub fn eula_text() -> String {
    "# Accepted by the operator via `cargo xtask harness provision --yes`.\neula=true\n".to_owned()
}

/// Whether an existing eula.txt already accepts the EULA.
///
/// Absent file counts as not accepted: vanilla refuses to boot without one,
/// and inventing agreement here is precisely what `--yes` exists to require.
pub fn eula_accepted(server_dir: &Path) -> Result<bool, String> {
    let path = server_dir.join("eula.txt");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    Ok(value_of(&text, "eula").map(unescape).as_deref() == Some("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rendered_file_pins_every_determinism_knob() {
        let text = render(&Settings::default());
        for (key, expected) in [
            ("level-seed", "0"),
            ("online-mode", "false"),
            ("spawn-protection", "0"),
            ("sync-chunk-writes", "false"),
            ("enable-rcon", "true"),
            ("rcon.port", "25575"),
            ("rcon.password", RCON_PASSWORD),
            ("level-type", r"minecraft\:normal"),
            ("pause-when-empty-seconds", "1000000000"),
            ("server-ip", "127.0.0.1"),
            ("difficulty", "peaceful"),
        ] {
            assert_eq!(
                value_of(&text, key),
                Some(expected),
                "{key} is part of the determinism contract"
            );
        }
    }

    #[test]
    fn the_seed_is_the_only_line_that_moves_between_seeds() {
        let zero = render(&Settings::default());
        let other = render(&Settings {
            seed: 123456789,
            ..Settings::default()
        });
        let differing: Vec<(&str, &str)> = zero
            .lines()
            .zip(other.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(differing.len(), 1, "{differing:?}");
        assert!(
            differing[0].0.starts_with("level-seed=") && differing[0].1.starts_with("level-seed="),
            "{differing:?}"
        );
    }

    #[test]
    fn rendering_is_idempotent() {
        let once = render(&Settings::default());
        assert_eq!(once, render(&Settings::default()));
        // And reading it back through the same reader the verification path
        // uses must produce the values that went in.
        assert_eq!(
            value_of(&once, "level-seed").map(unescape).as_deref(),
            Some("0")
        );
    }

    #[test]
    fn the_reader_understands_the_formats_vanilla_writes() {
        let text = "\
# a comment\n\
! and another\n\
level-seed = -42\n\
level-type=minecraft\\:normal\n\
rcon.port:25599\n";
        assert_eq!(value_of(text, "level-seed"), Some("-42"));
        assert_eq!(
            value_of(text, "level-type").map(unescape).as_deref(),
            Some("minecraft:normal")
        );
        assert_eq!(value_of(text, "rcon.port"), Some("25599"));
        assert_eq!(value_of(text, "absent"), None);
    }

    #[test]
    fn a_key_is_not_confused_with_a_longer_key_sharing_its_prefix() {
        let text = "rcon.port=1\nrcon.password=secret\n";
        assert_eq!(value_of(text, "rcon.port"), Some("1"));
        assert_eq!(value_of(text, "rcon.password"), Some("secret"));
    }

    #[test]
    fn an_absent_or_false_eula_is_not_acceptance() {
        let dir = crate::harness::testing::scratch_dir("eula-absent");
        std::fs::create_dir_all(&dir).expect("scratch");
        assert!(!eula_accepted(&dir).expect("read"));

        std::fs::write(dir.join("eula.txt"), "# Generated\neula=false\n").expect("write");
        assert!(!eula_accepted(&dir).expect("read"));

        std::fs::write(dir.join("eula.txt"), eula_text()).expect("write");
        assert!(eula_accepted(&dir).expect("read"));
    }

    #[test]
    fn the_eula_text_records_its_own_provenance() {
        let text = eula_text();
        assert_eq!(value_of(&text, "eula"), Some("true"));
        assert!(text.contains("--yes"), "the accepting flag must be named");
    }
}

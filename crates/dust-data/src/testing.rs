//! Test-only helpers shared by the unit tests across this crate.
//!
//! This exists because `#[cfg(test)]` code in one module cannot reach the
//! integration tests' `tests/support/`, and because three copies of an
//! in-memory pack would be two too many. It compiles only under `cargo test`,
//! so nothing here ships.

use std::collections::BTreeMap;

use crate::pack::{PackError, PackSource};

/// A pack held entirely in memory, so loader rules are tested against data
/// rather than against a filesystem that has opinions about timing.
///
/// Every file body is text: packs are JSON, and the byte-level container
/// formats are exercised by the zip writer in the integration tests.
#[derive(Debug)]
pub(crate) struct MemPack {
    id: String,
    files: BTreeMap<String, Vec<u8>>,
}

impl MemPack {
    pub(crate) fn new(id: &str, files: &[(&str, &str)]) -> Self {
        Self {
            id: id.to_owned(),
            files: files
                .iter()
                .map(|(path, body)| ((*path).to_owned(), body.as_bytes().to_vec()))
                .collect(),
        }
    }

    /// A well-formed format-48 pack with these files added underneath.
    pub(crate) fn with_meta(id: &str, files: &[(&str, &str)]) -> Self {
        let mut all = vec![(
            "pack.mcmeta",
            r#"{"pack":{"pack_format":48,"description":"test"}}"#,
        )];
        all.extend_from_slice(files);
        Self::new(id, &all)
    }
}

impl PackSource for MemPack {
    fn id(&self) -> &str {
        &self.id
    }

    fn origin(&self) -> String {
        format!("<memory:{}>", self.id)
    }

    fn list(&self) -> Result<Vec<String>, PackError> {
        Ok(self.files.keys().cloned().collect())
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError> {
        Ok(self.files.get(path).cloned())
    }
}

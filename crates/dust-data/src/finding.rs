//! What the loader says went wrong, and how it says it.
//!
//! This is deliberately the same shape as `dust_config::Finding`, for the same
//! reason that crate gives: an operator who fixes one problem, restarts, and is
//! told about the next one learns to distrust the server rather than the file.
//! A pack with forty problems produces forty findings in one run.
//!
//! Two things are added that a configuration file does not need. A finding
//! carries the **file it came from**, because a datapack is thousands of files
//! and a dotted path names none of them; and it carries the **pack**, because
//! the whole point of the overlay model is that two packs can both define one
//! resource, and "this recipe is wrong" is unanswerable without knowing which
//! pack won.
//!
//! # What the severities mean here
//!
//! [`Severity::Error`] means *this resource is not loaded*. It does not mean
//! the server refuses to start — see [`crate::LoadedData`] — because refusing
//! to start over one broken advancement in one pack is a worse outcome than
//! starting without it and saying so loudly. What is never acceptable is the
//! third option: dropping it quietly.

use crate::ResourceLocation;

/// How much a [`Finding`] matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The resource loaded. Something about it probably does not do what the
    /// person who wrote it expected.
    Warning,
    /// The resource did not load.
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A single problem, named by the file and the resource that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// The pack this came from, as [`crate::PackSource::id`] gives it.
    pub pack: String,
    /// The path inside the pack, e.g. `data/minecraft/tags/block/logs.json`.
    /// Empty when the problem is about the pack as a whole.
    pub file: String,
    /// The resource, when the problem is about one in particular.
    pub subject: Option<ResourceLocation>,
    /// What is wrong and what to do about it. Ends without a full stop only
    /// when it ends with a suggestion, per the examples in `dust-config`.
    pub message: String,
}

impl Finding {
    /// A problem that stopped something loading.
    pub fn error(
        pack: impl Into<String>,
        file: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            pack: pack.into(),
            file: file.into(),
            subject: None,
            message: message.into(),
        }
    }

    /// A problem on something that loaded anyway.
    pub fn warning(
        pack: impl Into<String>,
        file: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            pack: pack.into(),
            file: file.into(),
            subject: None,
            message: message.into(),
        }
    }

    /// Name the resource this finding is about.
    #[must_use]
    pub fn about(mut self, subject: ResourceLocation) -> Self {
        self.subject = Some(subject);
        self
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} in ", self.severity.label())?;
        match (self.pack.is_empty(), self.file.is_empty()) {
            (true, true) => f.write_str("<unknown>")?,
            (true, false) => write!(f, "{}", self.file)?,
            (false, true) => write!(f, "pack `{}`", self.pack)?,
            (false, false) => write!(f, "pack `{}`, {}", self.pack, self.file)?,
        }
        if let Some(subject) = &self.subject {
            write!(f, " ({subject})")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// How many of these stopped something loading.
pub fn error_count(findings: &[Finding]) -> usize {
    findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count()
}

/// The largest edit distance at which a suggestion is offered.
///
/// Three, the same as `dust_config::ore`'s. The number is arbitrary in both
/// places and the point of matching it is that "did you mean" behaves the same
/// way wherever an operator meets it; two different thresholds would show up as
/// one of them being mysteriously less helpful.
pub const SUGGESTION_DISTANCE: usize = 3;

/// The closest candidate to `target`, when one is close enough to suggest.
///
/// Ties go to the candidate that sorts first, so the message is stable across
/// runs; a suggestion that changes between two runs of the same command is a
/// suggestion nobody trusts.
pub fn nearest<'a, I>(target: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(target, candidate), candidate))
        .filter(|(distance, _)| *distance <= SUGGESTION_DISTANCE)
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)))
        .map(|(_, candidate)| candidate)
}

/// ` Did you mean `x`?`, or nothing at all.
///
/// Returned ready to append to a message, so that the caller does not have to
/// decide how to phrase the absence of a suggestion.
pub fn suggestion<'a, I>(target: &str, candidates: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    match nearest(target, candidates) {
        Some(candidate) => format!(" Did you mean `{candidate}`?"),
        None => String::new(),
    }
}

/// Levenshtein distance, two rows at a time.
///
/// The same implementation as `dust_config::ore::edit_distance`. It is copied
/// rather than shared because this crate must not depend on `dust-config` — a
/// datapack loader that cannot run without the server's configuration types is
/// a datapack loader that cannot be tested on its own — and fifteen lines of
/// textbook dynamic programming is a cheaper thing to duplicate than a
/// dependency edge is to add.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finding_names_the_pack_and_the_file() {
        let finding = Finding::error("my_pack", "data/minecraft/recipe/x.json", "went wrong")
            .about(ResourceLocation::parse("minecraft:x").unwrap());
        let text = finding.to_string();
        assert!(text.contains("my_pack"), "{text}");
        assert!(text.contains("data/minecraft/recipe/x.json"), "{text}");
        assert!(text.contains("minecraft:x"), "{text}");
    }

    #[test]
    fn a_near_miss_is_suggested_and_a_far_one_is_not() {
        let known = ["minecraft:stone", "minecraft:sand"];
        assert_eq!(nearest("minecraft:stnoe", known), Some("minecraft:stone"));
        assert_eq!(nearest("totally:different", known), None);
    }

    #[test]
    fn a_tie_is_broken_the_same_way_every_run() {
        // Both are one edit away. Without the tie-break the answer depends on
        // iteration order, and a suggestion that changes between runs of the
        // same command is one nobody trusts.
        let known = ["minecraft:sand", "minecraft:band"];
        assert_eq!(nearest("minecraft:cand", known), Some("minecraft:band"));
        assert_eq!(
            nearest("minecraft:cand", ["minecraft:sand", "minecraft:band"]),
            Some("minecraft:band")
        );
    }

    #[test]
    fn suggestion_is_empty_rather_than_awkward_when_nothing_is_close() {
        assert_eq!(suggestion("zzzz", ["aaaa"]), "");
        assert_eq!(suggestion("aaab", ["aaaa"]), " Did you mean `aaaa`?");
    }

    #[test]
    fn edit_distance_matches_the_textbook_cases() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
    }
}

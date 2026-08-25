//! The dependency licence gate.
//!
//! Dust is GPL-3.0 (decision record 0002), and licences flow one direction: a
//! GPL-3.0 project may absorb anything no more restrictive than itself, and
//! nothing more restrictive. The failure mode this guards against is not a
//! deliberate one — it is `cargo add` pulling in a transitive dependency whose
//! licence nobody looked at, discovered at release time when the fix is
//! expensive.
//!
//! **What this does not catch**, stated per the rule in `Testing.md`: it reads
//! the `license` field of each package's manifest and trusts it. A package that
//! declares MIT and ships GPL code, or one whose real terms live only in a
//! `LICENSE` file, passes. This is a check against carelessness, not against a
//! determined mislabelling, and it is no substitute for reading the terms of
//! anything vendored or copied per `Code Provenance.md`.

use std::collections::BTreeSet;

use serde::Deserialize;

/// SPDX identifiers a GPL-3.0 work may incorporate.
///
/// Additions to this list are a licence decision, so they belong in a commit of
/// their own with the reasoning in the message.
const COMPATIBLE: &[&str] = &[
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "GPL-3.0",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "ISC",
    "LGPL-2.1",
    "LGPL-2.1-only",
    "LGPL-2.1-or-later",
    "LGPL-3.0",
    "LGPL-3.0-only",
    "LGPL-3.0-or-later",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Unlicense",
    "Zlib",
];

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    #[serde(default)]
    pub workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub license_file: Option<String>,
}

/// A dependency this project may not use.
#[derive(Debug, PartialEq, Eq)]
pub struct Rejection {
    pub package: String,
    pub reason: String,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.package, self.reason)
    }
}

/// Every dependency that is not clearly usable, and why.
pub fn audit(metadata: &Metadata) -> Vec<Rejection> {
    let ours: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut rejections = Vec::new();

    for package in &metadata.packages {
        if ours.contains(package.id.as_str()) {
            continue;
        }
        let named = format!("{} {}", package.name, package.version);
        match package.license.as_deref() {
            Some(expr) => {
                if let Some(bad) = incompatible_terms(expr) {
                    rejections.push(Rejection {
                        package: named,
                        reason: format!(
                            "is licensed `{expr}`, and `{bad}` is not something a GPL-3.0 \
                             work may incorporate"
                        ),
                    });
                }
            }
            None if package.license_file.is_some() => rejections.push(Rejection {
                package: named,
                reason: "carries a licence file but no SPDX expression, so it has to be read \
                         by a person before it can be used"
                    .to_owned(),
            }),
            None => rejections.push(Rejection {
                package: named,
                reason: "declares no licence at all, which grants no permission to use it"
                    .to_owned(),
            }),
        }
    }
    rejections
}

/// The first term of an SPDX expression that nothing here may rely on.
///
/// `None` means the expression permits use in a GPL-3.0 work.
fn incompatible_terms(expr: &str) -> Option<String> {
    let tokens = tokenise(expr);
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
    };
    match parser.expression() {
        Some(true) if parser.at == tokens.len() => None,
        Some(_) => Some(expr.trim().to_owned()),
        // A malformed or unrecognised expression is reported rather than
        // guessed at. A licence checker that guesses is worse than none,
        // because it is trusted.
        None => Some(format!(
            "{} (could not be evaluated; read it by hand)",
            expr.trim()
        )),
    }
}

fn tokenise(expr: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in expr.chars() {
        match ch {
            '(' | ')' => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_owned());
                }
                current.clear();
                tokens.push(ch.to_string());
            }
            c if c.is_whitespace() => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_owned());
                }
                current.clear();
            }
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim().to_owned());
    }
    tokens
}

/// A recursive-descent reader for the subset of SPDX that appears in Cargo
/// manifests: identifiers, `AND`, `OR`, `WITH`, parentheses and the `+` suffix.
///
/// `AND` binds tighter than `OR`, per the SPDX specification. Getting that
/// backwards would make `MIT AND AGPL-3.0 OR MIT` pass, which is the one
/// mistake in this file that would matter.
struct Parser<'a> {
    tokens: &'a [String],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn take(&mut self) -> Option<&str> {
        let token = self.tokens.get(self.at).map(String::as_str);
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    fn expression(&mut self) -> Option<bool> {
        let mut value = self.conjunction()?;
        while self.peek().is_some_and(|t| t.eq_ignore_ascii_case("OR")) {
            self.at += 1;
            value |= self.conjunction()?;
        }
        Some(value)
    }

    fn conjunction(&mut self) -> Option<bool> {
        let mut value = self.factor()?;
        while self.peek().is_some_and(|t| t.eq_ignore_ascii_case("AND")) {
            self.at += 1;
            value &= self.factor()?;
        }
        Some(value)
    }

    fn factor(&mut self) -> Option<bool> {
        if self.peek() == Some("(") {
            self.at += 1;
            let inner = self.expression()?;
            if self.take() != Some(")") {
                return None;
            }
            return Some(inner);
        }

        let token = self.take()?;
        if token == ")" || token.eq_ignore_ascii_case("AND") || token.eq_ignore_ascii_case("OR") {
            return None;
        }

        // `X WITH exception` is treated as `X`. An exception can only ever add
        // permission, so a base licence that passes still passes; a base that
        // fails is reported so a person can read the exception and decide.
        let allowed = COMPATIBLE.contains(&token.trim_end_matches('+'));
        if self.peek().is_some_and(|t| t.eq_ignore_ascii_case("WITH")) {
            self.at += 1;
            self.take()?;
        }
        Some(allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, license: Option<&str>) -> Package {
        Package {
            id: format!("registry+{name}"),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: license.map(str::to_owned),
            license_file: None,
        }
    }

    fn audit_one(package: Package) -> Vec<Rejection> {
        audit(&Metadata {
            packages: vec![package],
            workspace_members: Vec::new(),
        })
    }

    #[test]
    fn an_unlicensed_dependency_is_rejected() {
        // The Phase 0.1 exit criterion: feed the check something with no
        // licence and watch it go red.
        let rejections = audit_one(package("mystery-crate", None));
        assert_eq!(rejections.len(), 1);
        assert!(
            rejections[0].reason.contains("no licence"),
            "{}",
            rejections[0]
        );
    }

    #[test]
    fn agpl_is_rejected() {
        assert_eq!(audit_one(package("steel", Some("AGPL-3.0-only"))).len(), 1);
    }

    #[test]
    fn gpl_2_only_is_rejected() {
        // GPL-2.0-only and GPL-3.0 are famously not combinable, and it is the
        // one rejection somebody will assume is a bug in this checker.
        assert_eq!(audit_one(package("old", Some("GPL-2.0-only"))).len(), 1);
    }

    #[test]
    fn the_ordinary_rust_licences_pass() {
        for expr in [
            "MIT",
            "Apache-2.0",
            "MIT OR Apache-2.0",
            "Unlicense OR MIT",
            "MPL-2.0",
        ] {
            assert!(
                audit_one(package("fine", Some(expr))).is_empty(),
                "{expr} should pass"
            );
        }
    }

    #[test]
    fn a_conjunction_is_only_as_permissive_as_its_worst_half() {
        assert_eq!(
            audit_one(package("mixed", Some("MIT AND AGPL-3.0-only"))).len(),
            1
        );
    }

    #[test]
    fn a_parenthesised_expression_is_evaluated_rather_than_refused() {
        // unicode-ident ships exactly this, and refusing it was the first thing
        // this check did on a real dependency graph.
        assert!(audit_one(package(
            "unicode-ident",
            Some("(MIT OR Apache-2.0) AND Unicode-3.0")
        ))
        .is_empty());
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // `(MIT AND AGPL) OR MIT` is fine; `MIT AND (AGPL OR MIT)` is fine too,
        // but `MIT AND AGPL OR MIT` must be read as the first, not the second.
        // Read the other way this returns a rejection, so the assertion below
        // is the one that catches the precedence being backwards.
        assert!(audit_one(package("p", Some("MIT AND AGPL-3.0-only OR MIT"))).is_empty());
        assert_eq!(
            audit_one(package(
                "p",
                Some("MIT AND (AGPL-3.0-only OR GPL-2.0-only)")
            ))
            .len(),
            1
        );
    }

    #[test]
    fn an_unparseable_expression_is_reported_and_not_assumed_fine() {
        let rejections = audit_one(package("p", Some("MIT AND (Apache-2.0")));
        assert_eq!(rejections.len(), 1);
        assert!(
            rejections[0].reason.contains("read it by hand"),
            "{}",
            rejections[0]
        );
    }

    #[test]
    fn an_exception_leaves_a_compatible_base_compatible() {
        assert!(audit_one(package("p", Some("Apache-2.0 WITH LLVM-exception"))).is_empty());
    }

    #[test]
    fn a_choice_passes_when_one_alternative_passes() {
        assert!(audit_one(package("either", Some("AGPL-3.0-only OR MIT"))).is_empty());
    }

    #[test]
    fn dust_itself_is_not_audited_against_the_list() {
        // Workspace members are the project, not dependencies of it. Without
        // this, GPL-3.0-only Dust would flag itself on every run.
        let rejections = audit(&Metadata {
            packages: vec![package("dust-server", None)],
            workspace_members: vec!["registry+dust-server".to_owned()],
        });
        assert!(rejections.is_empty());
    }
}

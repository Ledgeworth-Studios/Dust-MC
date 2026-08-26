//! `namespace:path` — the name every resource in a datapack is known by.
//!
//! # Why a bare path is accepted here and not in `dust-config`
//!
//! `dust_config::ore::OreGroup` and `dust_registry::Block::from_name` both
//! refuse a bare name, for the same stated reason: accepting `oak_log` and
//! `minecraft:oak_log` as the same thing would leave every caller downstream
//! unsure which of the two it is holding. That reasoning is right, and it does
//! not apply here — because of *where* the two sit.
//!
//! Those are **lookup** APIs. Their input is already a canonical id and there
//! is no document that says what a missing namespace would have meant, so
//! defaulting one in is inventing a rule.
//!
//! This is a **parse boundary**. The datapack file format has a rule already,
//! written by Mojang and relied on by every pack ever shipped: inside a data
//! file, a name with no namespace means the `minecraft` namespace. Refusing a
//! bare name here would not make Dust stricter, it would make Dust *wrong about
//! the format* — it would reject packs that vanilla loads, and the operator
//! would be told their pack was invalid when it is Dust that cannot read it.
//!
//! The way both concerns are satisfied at once is that the defaulting happens
//! **at the boundary and nowhere else**. There is no way to hold a
//! `ResourceLocation` whose namespace is unsettled: the field is not optional
//! and [`Display`](std::fmt::Display) always writes both halves. So the
//! ambiguity exists for the length of one function call in the parser and never
//! reaches a caller. [`ResourceLocation::parse`] is that boundary;
//! [`ResourceLocation::parse_qualified`] is for the places — a configuration
//! file, an operator command — where no file-format contract says what a
//! missing namespace means, and guessing would be the config crate's mistake
//! repeated.
//!
//! # What the guards here do not catch
//!
//! A well-formed name is not a name that refers to anything. `minecraft:stobe`
//! parses; whether it is a block is a question for a
//! [`Vocabulary`](crate::Vocabulary), which this crate takes as a parameter
//! because the registries it would need are being written in another crate.

use std::sync::Arc;

/// The namespace a bare path in a data file belongs to.
pub const MINECRAFT: &str = "minecraft";

/// The namespace Dust's own built-in data uses.
pub const DUST: &str = "dust";

/// A `namespace:path` identifier.
///
/// # Representation
///
/// One reference-counted string holding the joined `namespace:path`, plus the
/// index of the colon. Clone is an atomic increment rather than an allocation,
/// which matters because this type is the key of every map in the loaded data
/// and is cloned every time a resource is looked at.
///
/// The joined form is stored rather than two separate strings so that
/// [`as_str`](Self::as_str) — by far the most common thing done with one, since
/// it is what gets logged, hashed and written to the wire — is free.
#[derive(Clone)]
pub struct ResourceLocation {
    text: Arc<str>,
    /// Byte index of the `:` in `text`. Always valid: every constructor writes
    /// the colon itself rather than trusting an input to contain one.
    colon: u32,
}

impl ResourceLocation {
    /// Build from an already-split namespace and path.
    pub fn new(namespace: &str, path: &str) -> Result<Self, LocationError> {
        check_namespace(namespace, namespace)?;
        check_path(path, path)?;
        Ok(Self::joined(namespace, path))
    }

    /// The `minecraft:` name with this path.
    pub fn minecraft(path: &str) -> Result<Self, LocationError> {
        Self::new(MINECRAFT, path)
    }

    /// Parse a name as it appears **inside a data file**, where a bare path
    /// means the `minecraft` namespace.
    ///
    /// See the module documentation for why that default lives here and not in
    /// the lookup APIs.
    pub fn parse(text: &str) -> Result<Self, LocationError> {
        match text.split_once(':') {
            Some((namespace, path)) => {
                check_namespace(namespace, text)?;
                check_path(path, text)?;
                Ok(Self::joined(namespace, path))
            }
            None => {
                check_path(text, text)?;
                Ok(Self::joined(MINECRAFT, text))
            }
        }
    }

    /// Parse a name that must carry its namespace.
    ///
    /// For inputs that are not data files — configuration, commands, anything a
    /// person typed at Dust rather than at Minecraft's format. There is no
    /// document there saying what a missing namespace means, so this says so
    /// instead of guessing.
    pub fn parse_qualified(text: &str) -> Result<Self, LocationError> {
        if !text.contains(':') {
            return Err(LocationError {
                input: text.to_owned(),
                kind: LocationErrorKind::MissingNamespace,
            });
        }
        Self::parse(text)
    }

    /// The whole `namespace:path`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn namespace(&self) -> &str {
        &self.text[..self.colon as usize]
    }

    pub fn path(&self) -> &str {
        &self.text[self.colon as usize + 1..]
    }

    /// The same name with a different path — used to turn a tag's own name into
    /// the file it is expected to live in, and back.
    pub fn with_path(&self, path: &str) -> Result<Self, LocationError> {
        Self::new(self.namespace(), path)
    }

    fn joined(namespace: &str, path: &str) -> Self {
        let mut text = String::with_capacity(namespace.len() + 1 + path.len());
        text.push_str(namespace);
        text.push(':');
        text.push_str(path);
        Self {
            colon: namespace.len() as u32,
            text: Arc::from(text),
        }
    }
}

impl std::fmt::Display for ResourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::fmt::Debug for ResourceLocation {
    /// Prints as the name itself. The derived form — a struct with a `colon`
    /// index in it — turns every `{findings:?}` in a failing test into noise.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", &*self.text)
    }
}

impl PartialEq for ResourceLocation {
    fn eq(&self, other: &Self) -> bool {
        // Two clones of one name share an allocation, which is the common case
        // in a map lookup against a key that came out of the same map.
        Arc::ptr_eq(&self.text, &other.text) || self.text == other.text
    }
}

impl Eq for ResourceLocation {}

impl std::hash::Hash for ResourceLocation {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // The colon index is a function of the text, so hashing the text alone
        // is consistent with `PartialEq`, which also ignores it.
        self.text.hash(state);
    }
}

impl Ord for ResourceLocation {
    /// Orders by namespace, then path.
    ///
    /// Written out rather than comparing the joined text, because those are not
    /// the same order. A byte-wise comparison of `namespace:path` only agrees
    /// with the tuple order if `:` sorts below every character a namespace may
    /// contain, and it does not: `-`, `.` and every digit are all below `:` in
    /// ASCII. `mine:x` against `mine9:x` is the case that diverges.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.namespace()
            .cmp(other.namespace())
            .then_with(|| self.path().cmp(other.path()))
    }
}

impl PartialOrd for ResourceLocation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::str::FromStr for ResourceLocation {
    type Err = LocationError;

    /// Uses the data-file rule. See [`ResourceLocation::parse`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Why a string is not a resource location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationError {
    /// The whole string as it was written, so the message can quote what the
    /// pack author actually typed rather than a fragment of it.
    pub input: String,
    pub kind: LocationErrorKind,
}

/// The specific fault in a malformed resource location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationErrorKind {
    EmptyNamespace,
    EmptyPath,
    /// A character no namespace may contain, and where it was.
    BadNamespaceChar {
        found: char,
        at: usize,
    },
    /// A character no path may contain, and where it was.
    BadPathChar {
        found: char,
        at: usize,
    },
    /// Only from [`ResourceLocation::parse_qualified`].
    MissingNamespace,
}

impl std::fmt::Display for LocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` is not a resource location: ", self.input)?;
        match &self.kind {
            LocationErrorKind::EmptyNamespace => f.write_str(
                "the namespace before the `:` is empty. Write `minecraft:name`, \
                 or leave the `:` off entirely to mean the same thing.",
            ),
            LocationErrorKind::EmptyPath => f.write_str("there is no name after the `:`."),
            LocationErrorKind::BadNamespaceChar { found, at } => write!(
                f,
                "the namespace contains `{found}` at character {at}. A namespace \
                 is lowercase letters, digits, `_`, `.` and `-`.",
            ),
            LocationErrorKind::BadPathChar { found, at } => write!(
                f,
                "the name contains `{found}` at character {at}. A name is \
                 lowercase letters, digits, `_`, `.`, `-` and `/`.",
            ),
            LocationErrorKind::MissingNamespace => f.write_str(
                "it has no `namespace:` prefix. A bare name is only allowed \
                 inside a data file, where the format says it means `minecraft:`; \
                 here there is no such rule to fall back on, so write it out.",
            ),
        }
    }
}

impl std::error::Error for LocationError {}

fn check_namespace(namespace: &str, input: &str) -> Result<(), LocationError> {
    if namespace.is_empty() {
        return Err(LocationError {
            input: input.to_owned(),
            kind: LocationErrorKind::EmptyNamespace,
        });
    }
    for (at, ch) in namespace.char_indices() {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '_' | '.' | '-') {
            return Err(LocationError {
                input: input.to_owned(),
                kind: LocationErrorKind::BadNamespaceChar { found: ch, at },
            });
        }
    }
    Ok(())
}

fn check_path(path: &str, input: &str) -> Result<(), LocationError> {
    if path.is_empty() {
        return Err(LocationError {
            input: input.to_owned(),
            kind: LocationErrorKind::EmptyPath,
        });
    }
    for (at, ch) in path.char_indices() {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '_' | '.' | '-' | '/') {
            return Err(LocationError {
                input: input.to_owned(),
                kind: LocationErrorKind::BadPathChar { found: ch, at },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn a_bare_path_is_minecraft_in_a_data_file() {
        let parsed = ResourceLocation::parse("stone").expect("valid");
        assert_eq!(parsed.namespace(), "minecraft");
        assert_eq!(parsed.path(), "stone");
        assert_eq!(parsed.as_str(), "minecraft:stone");
    }

    #[test]
    fn a_bare_path_is_refused_where_no_format_rule_says_what_it_means() {
        let err = ResourceLocation::parse_qualified("stone").expect_err("bare");
        assert_eq!(err.kind, LocationErrorKind::MissingNamespace);
        // The message has to say why the same string was fine elsewhere, or the
        // inconsistency reads as a bug.
        assert!(err.to_string().contains("data file"), "{err}");
    }

    #[test]
    fn a_bare_name_and_its_written_out_form_are_one_value() {
        // This is the whole justification for defaulting at the boundary: after
        // parsing, nothing downstream can tell which spelling it came from.
        assert_eq!(
            ResourceLocation::parse("stone").unwrap(),
            ResourceLocation::parse("minecraft:stone").unwrap()
        );
    }

    #[test]
    fn a_path_may_contain_slashes_and_a_namespace_may_not() {
        assert!(ResourceLocation::parse("minecraft:blocks/stone").is_ok());
        let err = ResourceLocation::parse("mine/craft:stone").expect_err("slash");
        assert!(
            matches!(
                err.kind,
                LocationErrorKind::BadNamespaceChar { found: '/', .. }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn uppercase_is_refused_and_the_message_says_where() {
        let err = ResourceLocation::parse("minecraft:Stone").expect_err("uppercase");
        assert_eq!(
            err.kind,
            LocationErrorKind::BadPathChar { found: 'S', at: 0 }
        );
        assert!(err.to_string().contains("lowercase"), "{err}");
    }

    #[test]
    fn empty_halves_are_refused() {
        assert_eq!(
            ResourceLocation::parse(":stone").unwrap_err().kind,
            LocationErrorKind::EmptyNamespace
        );
        assert_eq!(
            ResourceLocation::parse("minecraft:").unwrap_err().kind,
            LocationErrorKind::EmptyPath
        );
        assert_eq!(
            ResourceLocation::parse("").unwrap_err().kind,
            LocationErrorKind::EmptyPath
        );
    }

    #[test]
    fn a_second_colon_is_a_bad_path_character_rather_than_a_second_split() {
        // `a:b:c` is one namespace and one path in Minecraft's parser too. What
        // matters is that it fails rather than silently keeping `b:c`.
        let err = ResourceLocation::parse("a:b:c").expect_err("two colons");
        assert!(
            matches!(err.kind, LocationErrorKind::BadPathChar { found: ':', .. }),
            "{err:?}"
        );
    }

    #[test]
    fn ordering_is_by_namespace_then_path_not_by_the_joined_text() {
        // The divergent case named in `Ord`'s doc comment. Byte-wise on the
        // joined text, `mine:x` sorts *after* `mine9:x`, because `:` is above
        // `9` in ASCII. By namespace, `mine` sorts first.
        let short = ResourceLocation::parse("mine:x").unwrap();
        let long = ResourceLocation::parse("mine9:x").unwrap();
        assert!(short < long);
        assert!(
            short.as_str() > long.as_str(),
            "the trap this guards against"
        );
    }

    #[test]
    fn a_set_deduplicates_the_two_spellings() {
        let mut set = BTreeSet::new();
        set.insert(ResourceLocation::parse("stone").unwrap());
        set.insert(ResourceLocation::parse("minecraft:stone").unwrap());
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_clone_shares_the_allocation() {
        let original = ResourceLocation::parse("minecraft:stone").unwrap();
        let copy = original.clone();
        assert!(Arc::ptr_eq(&original.text, &copy.text));
    }

    #[test]
    fn display_always_writes_both_halves() {
        assert_eq!(
            ResourceLocation::parse("stone").unwrap().to_string(),
            "minecraft:stone"
        );
    }
}

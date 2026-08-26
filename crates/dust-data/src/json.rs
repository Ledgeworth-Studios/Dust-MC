//! Reading JSON, and saying precisely where it went wrong.
//!
//! # Why resources stay as `serde_json::Value`
//!
//! This crate reads recipes, loot tables and advancements and does **not**
//! model them as Rust structs. That is a deliberate refusal, and it is the
//! reason this crate exists at all.
//!
//! Blocks, registry ids and packet ids became generated Rust in Phase 0.5
//! because they are identifiers the wire format depends on: the codec cannot go
//! and read a file to find out what id 1,234 means. Recipes and loot tables are
//! not that. They are the *datapack schema* — the shapes an operator's own
//! `datapacks/` directory is full of — and a datapack loader is by definition a
//! reader of them. Generating structs for them too would give Dust two readers
//! for one schema, and **two readers of one schema disagree**: one gets updated
//! for a new recipe type and the other does not, and the discrepancy surfaces
//! as a recipe that loads and then does nothing.
//!
//! The same argument was already made in this project about configuration,
//! where environment overrides are overlaid onto the parsed TOML *before*
//! deserialisation precisely so that an override cannot reach the server having
//! skipped a check.
//!
//! So the layer boundary is: this crate answers "what files are there, which
//! pack won, and is the JSON well-formed", and the crate that *uses* a recipe
//! is the one that decides what a recipe is. [`unknown_keys`] is offered as
//! public API so that layer reports its unknown keys the same way this one
//! reports its own, rather than inventing a second style.
//!
//! # What this does not catch
//!
//! Well-formed JSON is not correct data. `{"type": "minecraft:crafting_shaped"}`
//! with no pattern parses perfectly here and is a broken recipe. Nothing at this
//! layer can tell, and pretending otherwise by half-validating would be the two
//! readers again.

use serde_json::{Map, Value};

use crate::finding::{suggestion, Finding};

/// Parse a resource file, or produce the finding that says why not.
///
/// The finding names the file, the line and the column, because "invalid JSON"
/// against a 1,400-file pack is not an actionable sentence.
pub fn parse(bytes: &[u8], pack: &str, file: &str) -> Result<Value, Finding> {
    // A UTF-8 problem and a syntax problem are different mistakes and get
    // different messages; `from_slice` would fold them together into one
    // position that is a byte offset into something that is not text.
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Finding::error(
            pack,
            file,
            format!(
                "is not UTF-8: byte {} is not valid. Datapack JSON must be UTF-8.",
                error.valid_up_to()
            ),
        )
    })?;

    // A byte-order mark is invisible in an editor and makes serde_json fail on
    // "line 1 column 1", which sends people looking at a `{` that is fine.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    serde_json::from_str(text).map_err(|error| {
        Finding::error(
            pack,
            file,
            format!(
                "is not valid JSON at line {}, column {}: {}",
                error.line(),
                error.column(),
                error.classify_message(),
            ),
        )
    })
}

/// Wording for the different ways `serde_json` can fail, without repeating the
/// position it has already put in its own `Display`.
trait ClassifyMessage {
    fn classify_message(&self) -> String;
}

impl ClassifyMessage for serde_json::Error {
    fn classify_message(&self) -> String {
        let text = self.to_string();
        // `serde_json` ends every message with ` at line N column M`, which the
        // caller has already said in its own words.
        match text.rfind(" at line ") {
            Some(cut) => text[..cut].to_owned(),
            None => text,
        }
    }
}

/// Report object keys that are not in `known`.
///
/// A key nobody reads is the datapack version of a misspelled setting: the pack
/// author wrote something, the server started, and nothing they wrote had any
/// effect. That is the outcome this project rules out, so it is a finding —
/// a warning rather than an error, because the rest of the file is still
/// readable and refusing the whole resource over one stray key would lose more
/// than it saves.
///
/// The suggestion uses the same closest-match rule as everywhere else in Dust;
/// see [`crate::finding::nearest`].
pub fn unknown_keys(
    object: &Map<String, Value>,
    known: &[&str],
    pack: &str,
    file: &str,
    within: &str,
) -> Vec<Finding> {
    object
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .map(|key| {
            Finding::warning(
                pack,
                file,
                format!(
                    "has the key `{key}`{in_what}, which Dust does not read, so \
                     it will have no effect.{hint}",
                    in_what = if within.is_empty() {
                        String::new()
                    } else {
                        format!(" in `{within}`")
                    },
                    hint = suggestion(key, known.iter().copied()),
                ),
            )
        })
        .collect()
}

/// The object at `key`, or `None` — with a finding when the key is present and
/// is something other than an object.
///
/// The distinction matters: an absent optional section is not a mistake, and a
/// section written as a list is.
pub fn optional_object<'a>(
    parent: &'a Map<String, Value>,
    key: &str,
    pack: &str,
    file: &str,
    findings: &mut Vec<Finding>,
) -> Option<&'a Map<String, Value>> {
    match parent.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Object(object)) => Some(object),
        Some(other) => {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "has `{key}` as {}, but it must be an object.",
                    kind_of(other)
                ),
            ));
            None
        }
    }
}

/// The English name of a JSON value's type, for error messages.
pub fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_syntax_error_names_the_line_and_the_column() {
        let finding = parse(b"{\n  \"a\": 1,\n  \"b\",\n}", "p", "f.json").unwrap_err();
        let text = finding.to_string();
        assert!(text.contains("line 3"), "{text}");
        assert!(text.contains("column"), "{text}");
        assert!(text.contains("f.json"), "{text}");
    }

    #[test]
    fn the_position_is_not_printed_twice() {
        let finding = parse(b"{,}", "p", "f.json").unwrap_err();
        assert_eq!(
            finding.message.matches("line 1").count(),
            1,
            "{}",
            finding.message
        );
    }

    #[test]
    fn a_byte_order_mark_does_not_become_a_syntax_error() {
        // Windows editors add one, it is invisible, and the error it causes
        // points at a character that is not the problem.
        let mut bytes = "\u{feff}".as_bytes().to_vec();
        bytes.extend_from_slice(b"{\"a\": 1}");
        assert!(parse(&bytes, "p", "f.json").is_ok());
    }

    #[test]
    fn invalid_utf8_is_a_different_message_from_invalid_json() {
        let finding = parse(&[b'{', 0xff, b'}'], "p", "f.json").unwrap_err();
        assert!(finding.message.contains("UTF-8"), "{}", finding.message);
    }

    #[test]
    fn an_unknown_key_is_a_warning_that_suggests_the_right_one() {
        let object: Map<String, Value> =
            serde_json::from_str(r#"{"valeus": [], "replace": true}"#).unwrap();
        let findings = unknown_keys(&object, &["values", "replace"], "p", "f.json", "");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("`valeus`"), "{}", findings[0]);
        assert!(
            findings[0].message.contains("Did you mean `values`?"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn a_wrong_typed_section_is_named_by_its_type() {
        let object: Map<String, Value> = serde_json::from_str(r#"{"pack": []}"#).unwrap();
        let mut findings = Vec::new();
        assert!(optional_object(&object, "pack", "p", "f.json", &mut findings).is_none());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("a list"), "{}", findings[0]);
    }

    #[test]
    fn an_absent_section_is_not_a_finding() {
        let object: Map<String, Value> = serde_json::from_str("{}").unwrap();
        let mut findings = Vec::new();
        assert!(optional_object(&object, "pack", "p", "f.json", &mut findings).is_none());
        assert!(findings.is_empty());
    }
}

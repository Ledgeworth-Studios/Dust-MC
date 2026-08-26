//! One check that both the item report and the command report need.
//!
//! Both files carry Java `float`s printed as `double`s, and both would be
//! silently wrong if a number were read at one width and written at another.
//! The check is the same either way, so it lives here rather than twice.

use serde_json::Value as Json;

/// A number compared by value: whole numbers exactly, everything else by the
/// bits of the `f64` it parses to.
///
/// Two spellings of one number — Gson's `5.9999968E7` and the same value as
/// `59999968.0` — are the same variant of the same number. That is deliberate:
/// the serialiser picks a dialect and this extraction does not have to match
/// it. A different *value*, which is what reading `1.2` through an `f32`
/// produces, lands on different bits and does not match.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Number {
    Int(i64),
    /// An f64 held as its bits, because NaN is not equal to itself and f64 is
    /// not `Ord`, and the multiset comparison wants both.
    Bits(u64),
}

impl Number {
    fn from_token(token: &str) -> Option<Self> {
        if let Ok(i) = token.parse::<i64>() {
            return Some(Self::Int(i));
        }
        // JSON allows `.5` where Rust's parser wants a leading digit. A minus,
        // if there is one, sits in front of whatever gets inserted.
        let normalised = match token.split_once('.') {
            Some(("", mantissa)) => format!("0.{mantissa}"),
            Some(("-", mantissa)) => format!("-0.{mantissa}"),
            _ => token.to_owned(),
        };
        normalised
            .parse::<f64>()
            .ok()
            .map(|f| Self::Bits(f.to_bits()))
    }

    fn from_json(value: &Json) -> Option<Self> {
        match value {
            Json::Number(n) => match n.as_i64() {
                Some(i) => Some(Self::Int(i)),
                None => n.as_f64().map(|f| Self::Bits(f.to_bits())),
            },
            _ => None,
        }
    }
}

/// Every number in the report is present, once, in both the bytes and the parse.
///
/// This tokenises the raw bytes — every number token in the file, outside
/// strings — and compares that multiset against the numbers the JSON parse
/// produced, **by value** rather than by spelling.
///
/// What this buys is the assumption everything downstream leans on: that
/// walking the parsed tree visits every number the file contains, exactly as
/// often. A scanner that started a token inside `false` (whose trailing `e` is
/// a number's continuation character), or skipped one inside a string, or a
/// file whose bytes and whose parse disagree about what is there, produces two
/// multisets that do not match, and this names the file and stops.
///
/// What it deliberately does not catch: a value attached to the wrong item or
/// argument. That is what the golden samples are for.
pub fn check_every_number_reprints(json: &[u8], file: &str) -> Result<usize, String> {
    let mut in_file: Vec<Number> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut number: Option<usize> = None;
    for (index, byte) in json.iter().copied().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        // A number *starts* with a digit or a minus. Continuation is wider than
        // that, and must not be mistaken for a start: `false` ends in an `e`,
        // and a scanner that treats every `e` as a number produces a token
        // nothing in the parse can match.
        let continues = byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-');
        match number {
            Some(begin) if !continues => {
                let token = String::from_utf8_lossy(&json[begin..index]);
                let value = Number::from_token(&token).ok_or_else(|| {
                    format!(
                        "{file} holds `{token}`, which is not a number any reading of it \
                             accepts"
                    )
                })?;
                in_file.push(value);
                number = None;
            }
            _ => {}
        }
        if number.is_none() && (byte.is_ascii_digit() || byte == b'-') {
            number = Some(index);
        } else if byte == b'"' {
            in_string = true;
        }
    }
    if let Some(begin) = number {
        let token = String::from_utf8_lossy(&json[begin..]).into_owned();
        let value = Number::from_token(&token)
            .ok_or_else(|| format!("{file} ends in `{token}`, which is not a number"))?;
        in_file.push(value);
    }

    let parsed: Json =
        serde_json::from_slice(json).map_err(|e| format!("could not read {file}: {e}"))?;
    let mut from_parse = Vec::new();
    collect_numbers(&parsed, &mut from_parse);

    in_file.sort();
    from_parse.sort();
    if in_file != from_parse {
        return Err(format!(
            "{} numbers in {file}, {} from the parse, and they do not agree by value. The \
             bytes of the file and the tree parsed out of them describe different numbers, \
             so whatever this extraction reads next is reading something nobody wrote.",
            in_file.len(),
            from_parse.len()
        ));
    }
    Ok(in_file.len())
}

fn collect_numbers(value: &Json, out: &mut Vec<Number>) {
    if let Some(number) = Number::from_json(value) {
        out.push(number);
        return;
    }
    match value {
        Json::Array(items) => items.iter().for_each(|v| collect_numbers(v, out)),
        Json::Object(fields) => fields.values().for_each(|v| collect_numbers(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mismatch_between_bytes_and_parse_is_refused() {
        // Duplicate keys: the scanner sees both literals, the parse keeps only
        // the second. Whatever produced that disagreement, the answer is to
        // stop rather than pick a winner.
        let err = check_every_number_reprints(br#"{"a": 1, "a": 2}"#, "test.json")
            .expect_err("must not be accepted");
        assert!(err.contains("do not agree"), "{err}");
    }

    #[test]
    fn the_widths_that_matter_pass_the_check() {
        // The positive control, and it is not a formality: these literals are
        // the spellings the reports actually mix — an f32 widened to f64, the
        // shortest round-trip spelling, and Gson's exponent form for a large
        // double — and a check that refused any of them stops the extraction
        // on real data.
        let count = check_every_number_reprints(
            br#"{"speed": -2.4000000953674316, "saturation": 1.2, "n": 7.2000003, "i": 1561, "bound": -5.9999968E7}"#,
            "test.json",
        )
        .expect("these are the reports' own spellings");
        assert_eq!(count, 5);
    }

    #[test]
    fn two_spellings_of_one_value_are_one_number() {
        // Gson writes `5.9999968E7`; the same f64 re-spells as `59999968.0`.
        // The check compares values, so both sides land on the same bits and
        // the dialect difference is not a defect. Reading the same literal
        // through an f32 first would have moved the bits, and that is the
        // defect this check exists for.
        let count = check_every_number_reprints(
            br#"{"a": 5.9999968E7, "b": 59999968.0, "c": 5.9999968e7}"#,
            "test.json",
        )
        .expect("one value, three spellings");
        assert_eq!(count, 3);
    }

    #[test]
    fn the_scanner_does_not_find_a_number_inside_false() {
        // `false` ends in an `e`, which is a number's continuation character.
        // A scanner that treats every `e` as the start of one produces a token
        // the parse cannot match, and the check fails on valid data.
        let count = check_every_number_reprints(br#"{"a": false, "b": true, "c": 3}"#, "test.json")
            .expect("booleans are not numbers");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_number_inside_a_string_is_not_a_number() {
        let count = check_every_number_reprints(
            br#"{"a": "minecraft:music_disc_13", "b": 5}"#,
            "test.json",
        )
        .expect("parses");
        assert_eq!(count, 1);
    }
}

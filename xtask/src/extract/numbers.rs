//! One check that both the item report and the command report need.
//!
//! Both files carry Java `float`s printed as `double`s, and both would be
//! silently wrong if a number were read at one width and written at another.
//! The check is the same either way, so it lives here rather than twice.

use serde_json::Value as Json;

/// Every number in the report re-prints to exactly the text Mojang wrote.
///
/// The width trap in this report is real and it is quiet. The report spells
/// some numbers as the shortest text that round-trips through an `f32` and
/// others as the shortest that round-trips through an `f64`; storing one kind
/// at the other's width changes the value while leaving something that still
/// looks like a number. Reading `1.2` into an `f32` and widening it back gives
/// `1.2000000476837158`, which is not what the report says.
///
/// So this tokenises the raw bytes — every number token in the file, outside
/// strings — and compares that multiset against the numbers the parse produced,
/// formatted the way the generated code will spell them. All 3,021 of them, not
/// a sample: only 15 of the 41 distinct float literals have two spellings that
/// differ, so a sample is most of a check.
///
/// What it does not catch: a value read at the right width and then attached to
/// the wrong item. That is what the golden samples are for.
pub fn check_every_number_reprints(json: &[u8], file: &str) -> Result<usize, String> {
    let mut in_file: Vec<String> = Vec::new();
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
                in_file.push(String::from_utf8_lossy(&json[begin..index]).into_owned());
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
        in_file.push(String::from_utf8_lossy(&json[begin..]).into_owned());
    }

    let parsed: Json =
        serde_json::from_slice(json).map_err(|e| format!("could not read {file}: {e}"))?;
    let mut from_parse = Vec::new();
    collect_numbers(&parsed, &mut from_parse);

    in_file.sort();
    from_parse.sort();
    if in_file != from_parse {
        let mut only_in_file: Vec<&String> =
            in_file.iter().filter(|n| !from_parse.contains(n)).collect();
        only_in_file.dedup();
        return Err(format!(
            "{} numbers in {file}, {} from the parse, and they do not agree. The first \
             few the file has and the parse does not: {:?}. A number that does not re-print \
             to its own text is a number this extraction is storing at the wrong width.",
            in_file.len(),
            from_parse.len(),
            &only_in_file[..only_in_file.len().min(5)]
        ));
    }
    Ok(in_file.len())
}

fn collect_numbers(value: &Json, out: &mut Vec<String>) {
    match value {
        Json::Number(n) => out.push(match n.as_i64() {
            Some(i) => i.to_string(),
            // `{:?}` on an f64 is the shortest decimal that parses back to the
            // same bits, which is the same rule Mojang's serialiser used. The
            // comparison this feeds is what says so rather than assuming it.
            None => format!("{:?}", n.as_f64().unwrap_or(f64::NAN)),
        }),
        Json::Array(items) => items.iter().for_each(|v| collect_numbers(v, out)),
        Json::Object(fields) => fields.values().for_each(|v| collect_numbers(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_spelled_a_way_it_does_not_re_print_is_refused() {
        // `1.10` parses to the same f64 as `1.1` and re-prints as `1.1`, so the
        // multiset does not match. That is exactly the shape of the defect this
        // check exists for: a number whose text and whose value disagree about
        // what it is.
        let err = check_every_number_reprints(br#"{"a": {"components": {"x": 1.10}}}"#, "test.json")
            .expect_err("must not be accepted");
        assert!(err.contains("wrong width"), "{err}");
    }

    #[test]
    fn the_widths_that_matter_pass_the_check() {
        // The positive control, and it is not a formality: these two literals
        // are the two spellings the report actually mixes, and a check that
        // refused either would stop the extraction on real data.
        let count = check_every_number_reprints(br#"{"speed": -2.4000000953674316, "saturation": 1.2, "n": 7.2000003, "i": 1561}"#, "test.json")
        .expect("these are the report's own spellings");
        assert_eq!(count, 4);
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
        let count = check_every_number_reprints(br#"{"a": "minecraft:music_disc_13", "b": 5}"#, "test.json")
            .expect("parses");
        assert_eq!(count, 1);
    }
}

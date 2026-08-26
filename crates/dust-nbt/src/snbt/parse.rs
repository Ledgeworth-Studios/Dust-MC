//! The SNBT parser.
//!
//! Hand-written, over bytes, with an explicit cursor. The numeric rules in the
//! module documentation are regexes in Minecraft; here they are the small
//! functions in [`classify`], which recognise exactly the same languages. A
//! regex engine is not worth a dependency for seven patterns this simple, and
//! writing them out means each rule sits next to a comment saying which pattern
//! it is, which is the part a reader needs.
//!
//! Errors carry a byte offset and what was expected there. "Invalid input" is
//! not a bug report anyone can act on; `expected ':' after the key at byte 14`
//! is.

use std::fmt;

use crate::tag::{Compound, List, Tag, TagType};

/// Where an SNBT document stopped making sense, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset into the input.
    pub offset: usize,
    /// What the parser was looking for.
    pub expected: Expected,
    /// The character actually there, or `None` at the end of the input.
    pub found: Option<char>,
}

/// What the parser wanted at the offset it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    /// A specific character.
    Char(char),
    /// The start of a value.
    Value,
    /// A compound key.
    Key,
    /// A closing quote for a string that was opened.
    ClosingQuote(char),
    /// A `\` escape that means something. Only the quote character and `\`
    /// itself are escapable; Brigadier rejects every other escape by name.
    ValidEscape,
    /// One of `B`, `I` or `L` after `[`, or a list element.
    ArrayKindOrElement,
    /// An element of the type the array or list is holding.
    ///
    /// For an array the type is the array's own width, and vanilla requires the
    /// element to carry the matching suffix. For a list it is the type the
    /// first element set.
    ArrayElement(TagType),
    /// Nothing: the document was complete and there was more text.
    EndOfInput,
    /// The document nested deeper than the limit.
    LessNesting(usize),
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Char(c) => write!(f, "{c:?}"),
            Self::Value => f.write_str("a value"),
            Self::Key => f.write_str("a key"),
            Self::ClosingQuote(c) => write!(f, "the closing {c:?}"),
            Self::ValidEscape => f.write_str("an escape of the quote character or of a backslash"),
            Self::ArrayKindOrElement => f.write_str("'B;', 'I;', 'L;' or a list element after '['"),
            Self::ArrayElement(tag) => write!(f, "an element of the {tag} being built"),
            Self::EndOfInput => f.write_str("the end of the input"),
            Self::LessNesting(limit) => write!(f, "nesting no deeper than {limit}"),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.found {
            Some(c) => write!(
                f,
                "at byte {}: expected {}, found {c:?}",
                self.offset, self.expected
            ),
            None => write!(
                f,
                "at byte {}: expected {}, and the input ended",
                self.offset, self.expected
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// How deep an SNBT document may nest.
///
/// The parser recurses, so this is a stack guard and not a taste question. It
/// matches the binary reader's default for the same reason that one matches
/// vanilla: a document that survives one path through the server and not the
/// other is a bug waiting for someone to find it.
const MAX_DEPTH: usize = 512;

/// Parse a complete SNBT document, which may be any tag.
///
/// Trailing text is an error. `TagParser.parseTag` behaves the same way and
/// reports it as `argument.nbt.trailing`.
///
/// ```
/// use dust_nbt::{snbt, Tag};
///
/// # fn main() -> Result<(), dust_nbt::snbt::ParseError> {
/// let item = snbt::parse("{Count:3b,id:'minecraft:diamond'}")?;
/// let compound = item.as_compound().expect("a compound");
/// assert_eq!(compound.get("Count"), Some(&Tag::Byte(3)));
/// # Ok(())
/// # }
/// ```
pub fn parse(input: &str) -> Result<Tag, ParseError> {
    let mut parser = Parser::new(input);
    let tag = parser.value()?;
    parser.skip_whitespace();
    if parser.position < parser.bytes.len() {
        return Err(parser.error(Expected::EndOfInput));
    }
    Ok(tag)
}

/// Parse a document that must be a compound, which is what every command
/// argument spelled `nbt_compound_tag` accepts.
pub fn parse_compound(input: &str) -> Result<Compound, ParseError> {
    match parse(input)? {
        Tag::Compound(compound) => Ok(compound),
        _ => Err(ParseError {
            offset: 0,
            expected: Expected::Char('{'),
            found: other_first_char(input),
        }),
    }
}

fn other_first_char(input: &str) -> Option<char> {
    input.trim_start().chars().next()
}

struct Parser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    position: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            position: 0,
            depth: 0,
        }
    }

    fn error(&self, expected: Expected) -> ParseError {
        ParseError {
            offset: self.position,
            expected,
            found: self.input[self.position..].chars().next(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    /// Whitespace between tokens.
    ///
    /// Brigadier's `StringReader.skipWhitespace` uses `Character.isWhitespace`,
    /// which is a Unicode property and not this set. The difference only shows
    /// up for exotic separators — an ideographic space between a key and its
    /// colon — which nothing produces and which would otherwise cost a Unicode
    /// table here. Restricting it is a deliberate narrowing and this is the
    /// note that says so.
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), ParseError> {
        self.skip_whitespace();
        if self.peek() == Some(wanted) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.error(Expected::Char(wanted as char)))
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(Expected::LessNesting(MAX_DEPTH)));
        }
        Ok(())
    }

    fn value(&mut self) -> Result<Tag, ParseError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => Ok(Tag::Compound(self.compound()?)),
            Some(b'[') => self.array_or_list(),
            Some(_) => {
                let (text, quoted) = self.string()?;
                Ok(if quoted {
                    Tag::String(text)
                } else {
                    classify(&text)
                })
            }
            None => Err(self.error(Expected::Value)),
        }
    }

    fn compound(&mut self) -> Result<Compound, ParseError> {
        self.enter()?;
        self.expect(b'{')?;
        let mut compound = Compound::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            self.depth -= 1;
            return Ok(compound);
        }
        loop {
            self.skip_whitespace();
            let key_offset = self.position;
            let (key, _) = self.string()?;
            if key.is_empty() {
                // `TagParser.readKey` refuses an empty key with
                // `argument.nbt.expected.key`. An unquoted empty key is what a
                // stray comma produces, so refusing it here turns `{a:1,,b:2}`
                // into an error at the second comma rather than a compound with
                // a nameless field.
                return Err(ParseError {
                    offset: key_offset,
                    expected: Expected::Key,
                    found: self.input[key_offset..].chars().next(),
                });
            }
            self.expect(b':')?;
            let value = self.value()?;
            // `insert`, not `append`: SNBT is written by people, and
            // `{a:1,a:2}` from a person means the second one. This is the same
            // last-wins rule the binary reader follows, arrived at from the
            // other direction — there it is `HashMap.put`, here it is that a
            // human typed the second value on purpose.
            compound.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    self.depth -= 1;
                    return Ok(compound);
                }
                _ => return Err(self.error(Expected::Char('}'))),
            }
        }
    }

    /// `[` opens four different things and only the next two bytes say which.
    fn array_or_list(&mut self) -> Result<Tag, ParseError> {
        // `[B;`, `[I;` and `[L;` — the three prefixes are exactly these three
        // strings in `TagParser`, which are also three of the string constants
        // in the class file.
        if let Some(kind) = match self.bytes.get(self.position..self.position + 3) {
            Some(b"[B;") => Some(TagType::ByteArray),
            Some(b"[I;") => Some(TagType::IntArray),
            Some(b"[L;") => Some(TagType::LongArray),
            _ => None,
        } {
            self.position += 3;
            return self.array(kind);
        }
        self.list()
    }

    fn array(&mut self, kind: TagType) -> Result<Tag, ParseError> {
        let mut bytes = Vec::new();
        let mut ints = Vec::new();
        let mut longs = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.position += 1;
                break;
            }
            let element_offset = self.position;
            let element = self.value()?;
            // The element type must match *exactly*, which is stricter than it
            // looks. `TagParser.readArray` compares the parsed tag's type
            // against the array's element type and throws
            // `argument.nbt.array.mixed` if they differ — so `[B;1,2]` is an
            // error, because `1` is a `TAG_Int`, and only `[B;1b,2b]` is a byte
            // array. Widening a bare int into a byte here would accept
            // documents vanilla rejects, and this parser exists to agree with
            // vanilla rather than to be helpful.
            match (kind, &element) {
                (TagType::ByteArray, Tag::Byte(v)) => bytes.push(*v),
                (TagType::IntArray, Tag::Int(v)) => ints.push(*v),
                (TagType::LongArray, Tag::Long(v)) => longs.push(*v),
                _ => {
                    return Err(ParseError {
                        offset: element_offset,
                        expected: Expected::ArrayElement(kind),
                        found: self.input[element_offset..].chars().next(),
                    })
                }
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    break;
                }
                _ => return Err(self.error(Expected::Char(']'))),
            }
        }
        Ok(match kind {
            TagType::ByteArray => Tag::ByteArray(bytes),
            TagType::IntArray => Tag::IntArray(ints),
            TagType::LongArray => Tag::LongArray(longs),
            _ => unreachable!("array() is only called with the three array types"),
        })
    }

    fn list(&mut self) -> Result<Tag, ParseError> {
        self.enter()?;
        self.expect(b'[')?;
        let mut list = List::new(TagType::End);
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            self.depth -= 1;
            // An empty list written `[]` gets element type TAG_End, which is
            // what vanilla writes for one and what `List::new` gives it.
            return Ok(Tag::List(list));
        }
        loop {
            self.skip_whitespace();
            let element_offset = self.position;
            let element = self.value()?;
            list.push(element).map_err(|_| ParseError {
                offset: element_offset,
                expected: Expected::ArrayElement(list.element_type()),
                found: self.input[element_offset..].chars().next(),
            })?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    self.depth -= 1;
                    return Ok(Tag::List(list));
                }
                _ => return Err(self.error(Expected::Char(']'))),
            }
        }
    }

    /// A key or a scalar: quoted with either quote character, or unquoted.
    ///
    /// Returns whether it was quoted, because that is the difference between
    /// the string `"1b"` and the byte `1b` and nothing else distinguishes them.
    fn string(&mut self) -> Result<(String, bool), ParseError> {
        self.skip_whitespace();
        match self.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                self.position += 1;
                self.quoted(quote).map(|text| (text, true))
            }
            Some(_) => {
                let start = self.position;
                while self
                    .peek()
                    .is_some_and(|b| is_allowed_in_unquoted_string(b as char))
                {
                    self.position += 1;
                }
                if self.position == start {
                    return Err(self.error(Expected::Value));
                }
                Ok((self.input[start..self.position].to_owned(), false))
            }
            None => Err(self.error(Expected::Value)),
        }
    }

    /// The body of a quoted string, `quote` already consumed.
    ///
    /// Brigadier's `readStringUntil` allows exactly two escapes: the quote
    /// character that opened the string, and `\`. Anything else after a
    /// backslash is `readerInvalidEscape`. In particular `\n` is **not** a
    /// newline — it is an error — and a real newline inside quotes is accepted
    /// verbatim.
    fn quoted(&mut self, quote: u8) -> Result<String, ParseError> {
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error(Expected::ClosingQuote(quote as char)));
            };
            self.position += 1;
            match byte {
                b'\\' => {
                    let escape_offset = self.position;
                    match self.peek() {
                        Some(b) if b == quote || b == b'\\' => {
                            out.push(b as char);
                            self.position += 1;
                        }
                        _ => {
                            return Err(ParseError {
                                offset: escape_offset,
                                expected: Expected::ValidEscape,
                                found: self.input[escape_offset..].chars().next(),
                            })
                        }
                    }
                }
                b if b == quote => return Ok(out),
                _ => {
                    // Step back over the whole character: a multi-byte one has
                    // continuation bytes that are not `\` or the quote, so
                    // copying the slice is both correct and one memcpy instead
                    // of a char decode.
                    let start = self.position - 1;
                    let mut end = self.position;
                    while end < self.bytes.len() && !self.input.is_char_boundary(end) {
                        end += 1;
                    }
                    out.push_str(&self.input[start..end]);
                    self.position = end;
                }
            }
        }
    }
}

/// Brigadier's `StringReader.isAllowedInUnquotedString`, byte for byte.
///
/// Read out of `com/mojang/brigadier/StringReader.class`: the method compares
/// against 48-57, 65-90, 97-122, and then `_`, `-`, `.`, `+` individually.
/// Notably absent: `:`, which is why `minecraft:stone` has to be quoted as a
/// *value* even though it appears unquoted in item ids everywhere else — those
/// are parsed by a different argument type.
pub(crate) fn is_allowed_in_unquoted_string(c: char) -> bool {
    c.is_ascii_digit()
        || c.is_ascii_uppercase()
        || c.is_ascii_lowercase()
        || c == '_'
        || c == '-'
        || c == '.'
        || c == '+'
}

/// Turn an unquoted word into the tag `TagParser` would make of it.
///
/// The order is the order of the `if` chain in `TagParser.type(String)`, which
/// matters: `1` matches both the int rule and — were it tried first — nothing
/// else, but `1.0` matches the bare-double rule and must not be tried against
/// the int rule at all.
///
/// Every arm that parses a number falls through to a string when the parse
/// overflows, which is `TagParser` catching `NumberFormatException` around the
/// whole chain. That is why `300b` is a string.
fn classify(word: &str) -> Tag {
    if let Some(body) = strip_suffix_ci(word, 'f') {
        if is_float_body(body, true) {
            if let Ok(value) = body.parse::<f32>() {
                return Tag::Float(value);
            }
        }
    }
    if let Some(body) = strip_suffix_ci(word, 'b') {
        if is_integer_body(body) {
            if let Ok(value) = body.parse::<i8>() {
                return Tag::Byte(value);
            }
        }
    }
    if let Some(body) = strip_suffix_ci(word, 'l') {
        if is_integer_body(body) {
            if let Ok(value) = body.parse::<i64>() {
                return Tag::Long(value);
            }
        }
    }
    if let Some(body) = strip_suffix_ci(word, 's') {
        if is_integer_body(body) {
            if let Ok(value) = body.parse::<i16>() {
                return Tag::Short(value);
            }
        }
    }
    if is_integer_body(word) {
        if let Ok(value) = word.parse::<i32>() {
            return Tag::Int(value);
        }
    }
    if let Some(body) = strip_suffix_ci(word, 'd') {
        if is_float_body(body, true) {
            if let Ok(value) = body.parse::<f64>() {
                return Tag::Double(value);
            }
        }
    }
    if is_float_body(word, false) {
        if let Ok(value) = word.parse::<f64>() {
            return Tag::Double(value);
        }
    }
    if word.eq_ignore_ascii_case("true") {
        return Tag::Byte(1);
    }
    if word.eq_ignore_ascii_case("false") {
        return Tag::Byte(0);
    }
    Tag::String(word.to_owned())
}

fn strip_suffix_ci(word: &str, suffix: char) -> Option<&str> {
    let last = word.chars().next_back()?;
    (last.eq_ignore_ascii_case(&suffix)).then(|| &word[..word.len() - last.len_utf8()])
}

/// `[-+]?(?:0|[1-9][0-9]*)`.
///
/// The leading-zero rule is the surprising half: `01` does not match, so it is
/// not an int, not a byte with a suffix, and ends up a string.
fn is_integer_body(body: &str) -> bool {
    let digits = body.strip_prefix(['-', '+']).unwrap_or(body);
    match digits.as_bytes() {
        [] => false,
        [b'0'] => true,
        [b'0', ..] => false,
        rest => rest.iter().all(u8::is_ascii_digit),
    }
}

/// `[-+]?(?:[0-9]+[.]?|[0-9]*[.][0-9]+)(?:e[-+]?[0-9]+)?` when
/// `point_optional`, and the same with `[0-9]+[.]` — the point required — when
/// not.
///
/// The two differ by one `?` in Mojang's patterns and that `?` is the whole
/// reason `1` is an int rather than a double.
fn is_float_body(body: &str, point_optional: bool) -> bool {
    let body = body.strip_prefix(['-', '+']).unwrap_or(body);
    // The exponent, if any, splits first: `e` is not part of the mantissa.
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(index) => (&body[..index], Some(&body[index + 1..])),
        None => (body, None),
    };
    if let Some(exponent) = exponent {
        let digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    match mantissa.split_once('.') {
        // `[0-9]+[.]` — digits, a point, nothing after.
        Some((before, "")) => !before.is_empty() && before.bytes().all(|b| b.is_ascii_digit()),
        // `[0-9]*[.][0-9]+` — a point with digits after it.
        Some((before, after)) => {
            before.bytes().all(|b| b.is_ascii_digit())
                && !after.is_empty()
                && after.bytes().all(|b| b.is_ascii_digit())
        }
        // `[0-9]+` with no point at all, which only the suffixed rules allow.
        None => {
            point_optional && !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

//! A minimal NBT reader, harness-local and throwaway.
//!
//! Chunk compounds contain eleven tag kinds nested a few levels deep, and
//! reading them is the only thing standing between `capture` and the bytes it
//! hashes. `dust-nbt` — the real implementation, zero-copy where possible — is
//! not built on this base, so this module walks the format directly. It is
//! written to be deleted: no allocation tricks, no borrowed views, just a
//! tree of owned values small enough (a chunk is tens of kilobytes) that
//! simplicity wins.
//!
//! What is *not* supported, on purpose: writing, SNBT text, network NBT with
//! varint names, skipping unknown tags by length arithmetic without parsing
//! them (every tag kind below is parsed for real, because a skipped tag whose
//! layout changed silently corrupts everything after it).
//!
//! Strings are read as UTF-8 lossily. Java's "modified UTF-8" differs from
//! standard UTF-8 only in its encoding of NUL and astral code points; block,
//! biome and heightmap names are ASCII in every version this targets.

/// Every tag type the format defines, as it appears on disk.
mod tag {
    pub const END: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const INT: u8 = 3;
    pub const LONG: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const DOUBLE: u8 = 6;
    pub const BYTE_ARRAY: u8 = 7;
    pub const STRING: u8 = 8;
    pub const LIST: u8 = 9;
    pub const COMPOUND: u8 = 10;
    pub const INT_ARRAY: u8 = 11;
    pub const LONG_ARRAY: u8 = 12;
}

/// One node of an NBT tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Node>),
    Compound(Vec<(String, Node)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Node {
    /// The value of a compound entry, if this is a compound holding it.
    pub fn get(&self, name: &str) -> Option<&Node> {
        match self {
            Node::Compound(entries) => entries.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This compound's entries as a slice, or empty for any other tag.
    pub fn entries(&self) -> &[(String, Node)] {
        match self {
            Node::Compound(entries) => entries,
            _ => &[],
        }
    }

    /// The elements of a list, or empty for anything else.
    pub fn list(&self) -> &[Node] {
        match self {
            Node::List(items) => items,
            _ => &[],
        }
    }

    /// The string content of a `TAG_String`, or `None` for any other tag.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer content of an `TAG_Int`, or `None` for any other tag.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Node::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The array content of a `TAG_Long_Array`, or `None` for any other tag.
    pub fn as_longs(&self) -> Option<&[i64]> {
        match self {
            Node::LongArray(v) => Some(v),
            _ => None,
        }
    }
}

/// Read a root compound: one named tag of type `TAG_Compound`.
pub fn read_root(bytes: &[u8]) -> Result<Node, String> {
    let mut r = Reader { bytes, at: 0 };
    let kind = r.u8("the root tag type")?;
    if kind != tag::COMPOUND {
        return Err(format!("a root must be a compound, found tag type {kind}"));
    }
    let name = r.string("the root tag name")?;
    let _ = name; // conventionally empty; nothing here keys off it
    let node = r.compound("the root")?;
    r.end()?;
    Ok(node)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize, what: &str) -> Result<&[u8], String> {
        let end = self.at.checked_add(count).ok_or_else(|| {
            format!(
                "{what}: offset overflow reading {count} bytes at {}",
                self.at
            )
        })?;
        if end > self.bytes.len() {
            return Err(format!(
                "{what}: needed {count} byte(s) at offset {} but the buffer ends at {}",
                self.at,
                self.bytes.len()
            ));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self, what: &str) -> Result<u8, String> {
        Ok(self.take(1, what)?[0])
    }

    fn i16(&mut self, what: &str) -> Result<i16, String> {
        let b = self.take(2, what)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn i32(&mut self, what: &str) -> Result<i32, String> {
        let b = self.take(4, what)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64(&mut self, what: &str) -> Result<i64, String> {
        let b = self.take(8, what)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f32(&mut self, what: &str) -> Result<f32, String> {
        Ok(f32::from_bits(self.i32(what)? as u32))
    }

    fn f64(&mut self, what: &str) -> Result<f64, String> {
        Ok(f64::from_bits(self.i64(what)? as u64))
    }

    fn string(&mut self, what: &str) -> Result<String, String> {
        let len = self.i16(what)? as usize;
        let raw = self.take(len, what)?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    fn payload(&mut self, kind: u8, what: &str) -> Result<Node, String> {
        Ok(match kind {
            tag::BYTE => Node::Byte(self.i8(what)?),
            tag::SHORT => Node::Short(self.i16(what)?),
            tag::INT => Node::Int(self.i32(what)?),
            tag::LONG => Node::Long(self.i64(what)?),
            tag::FLOAT => Node::Float(self.f32(what)?),
            tag::DOUBLE => Node::Double(self.f64(what)?),
            tag::BYTE_ARRAY => {
                let len = self.i32(what)?;
                negative(len, what)?;
                Node::ByteArray(self.take(len as usize, what)?.to_vec())
            }
            tag::STRING => Node::String(self.string(what)?),
            tag::LIST => {
                let element_kind = self.u8(&format!("{what}: list element type"))?;
                let len = self.i32(&format!("{what}: list length"))?;
                negative(len, what)?;
                let mut items = Vec::with_capacity((len as usize).min(1024));
                for index in 0..len {
                    items.push(self.payload(element_kind, &format!("{what}[{index}]"))?);
                }
                Node::List(items)
            }
            tag::COMPOUND => self.compound(what)?,
            tag::INT_ARRAY => {
                let len = self.i32(what)?;
                negative(len, what)?;
                let mut items = Vec::with_capacity(len as usize);
                for index in 0..len {
                    items.push(self.i32(&format!("{what}[{index}]"))?);
                }
                Node::IntArray(items)
            }
            tag::LONG_ARRAY => {
                let len = self.i32(what)?;
                negative(len, what)?;
                let mut items = Vec::with_capacity(len as usize);
                for index in 0..len {
                    items.push(self.i64(&format!("{what}[{index}]"))?);
                }
                Node::LongArray(items)
            }
            tag::END => {
                return Err(format!(
                    "{what}: an END tag appeared where a value was expected"
                ))
            }
            other => return Err(format!("{what}: unknown tag type {other}")),
        })
    }

    fn i8(&mut self, what: &str) -> Result<i8, String> {
        Ok(self.take(1, what)?[0] as i8)
    }

    fn compound(&mut self, what: &str) -> Result<Node, String> {
        let mut entries = Vec::new();
        loop {
            let kind = self.u8(&format!("{what}: entry tag type"))?;
            if kind == tag::END {
                return Ok(Node::Compound(entries));
            }
            let name = self.string(&format!("{what}: entry name"))?;
            let value = self.payload(kind, &format!("{what}.{name}"))?;
            entries.push((name, value));
        }
    }

    fn end(&mut self) -> Result<(), String> {
        // Trailing bytes after the root compound mean the caller handed us a
        // stream containing more than one object, which every caller here
        // treats as a framing bug rather than data to ignore.
        if self.at != self.bytes.len() {
            return Err(format!(
                "{} trailing byte(s) after the root compound",
                self.bytes.len() - self.at
            ));
        }
        Ok(())
    }
}

fn negative(length: i32, what: &str) -> Result<(), String> {
    if length < 0 {
        return Err(format!("{what}: negative array length {length}"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod writer;

#[cfg(test)]
mod tests {
    use super::writer::{n, root};
    use super::*;

    #[test]
    fn a_compound_of_every_tag_type_round_trips() {
        let bytes = root(vec![
            ("byte", n::b(-5)),
            ("short", n::s(300)),
            ("int", n::i(-70_000)),
            ("long", n::l(i64::MIN)),
            ("float", n::f(1.5)),
            ("double", n::d(-0.25)),
            ("bytes", n::ba(&[1, 2, 3])),
            ("text", n::str("minecraft:stone")),
            ("list", n::list(vec![n::i(7), n::i(8)])),
            ("nested", n::comp(vec![("inner", n::str("value"))])),
            ("ints", n::ia(&[1, -2, 3])),
            ("longs", n::la(&[i64::MAX, 0])),
        ]);
        let parsed = read_root(&bytes).expect("parses");

        assert_eq!(parsed.get("byte"), Some(&Node::Byte(-5)));
        assert_eq!(parsed.get("short"), Some(&Node::Short(300)));
        assert_eq!(parsed.get("int"), Some(&Node::Int(-70_000)));
        assert_eq!(parsed.get("long"), Some(&Node::Long(i64::MIN)));
        assert_eq!(parsed.get("float"), Some(&Node::Float(1.5)));
        assert_eq!(parsed.get("double"), Some(&Node::Double(-0.25)));
        assert_eq!(parsed.get("bytes"), Some(&Node::ByteArray(vec![1, 2, 3])));
        assert_eq!(
            parsed.get("text").and_then(Node::as_str),
            Some("minecraft:stone")
        );
        assert_eq!(parsed.get("list").map(Node::list).unwrap().len(), 2);
        assert_eq!(
            parsed
                .get("nested")
                .and_then(|c| c.get("inner"))
                .and_then(Node::as_str),
            Some("value")
        );
        assert_eq!(parsed.get("ints"), Some(&Node::IntArray(vec![1, -2, 3])));
        assert_eq!(
            parsed.get("longs"),
            Some(&Node::LongArray(vec![i64::MAX, 0]))
        );
    }

    #[test]
    fn an_empty_list_reads_as_empty() {
        let bytes = root(vec![("none", n::list(vec![]))]);
        let parsed = read_root(&bytes).expect("parses");
        assert_eq!(parsed.get("none").map(Node::list).unwrap().len(), 0);
    }

    #[test]
    fn truncation_is_reported_with_where_it_happened() {
        let full = root(vec![("deep", n::comp(vec![("values", n::la(&[1, 2, 3]))]))]);
        for cut in [4, 6, 12, full.len() - 1] {
            let err = read_root(&full[..cut]).expect_err("truncated");
            assert!(
                err.contains("needed") || err.contains("offset"),
                "cut to {cut}: {err}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let mut bytes = root(vec![("x", n::i(1))]);
        bytes.extend_from_slice(&[0, 0, 0]);
        assert!(
            read_root(&bytes).expect_err("refused").contains("trailing"),
            "extra bytes must not pass silently"
        );
    }

    #[test]
    fn a_non_compound_root_is_named_as_the_problem() {
        let mut bytes = Vec::new();
        bytes.push(tag::INT);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&5i32.to_be_bytes());
        assert!(
            read_root(&bytes)
                .expect_err("refused")
                .contains("must be a compound"),
            "int root refused"
        );
    }

    #[test]
    fn a_negative_length_is_an_error_not_an_allocation() {
        let mut bytes = root(vec![]);
        bytes.pop(); // drop the END so the forged entry can be appended
        bytes.push(tag::BYTE_ARRAY);
        bytes.extend_from_slice(&3u16.to_be_bytes()); // name length
        bytes.extend_from_slice(b"bad"); // name
        bytes.extend_from_slice(&(-1i32).to_be_bytes()); // array length
        let err = read_root(&bytes).expect_err("refused");
        assert!(err.contains("negative"), "{err}");
    }

    #[test]
    fn an_end_tag_in_value_position_is_rejected() {
        let mut bytes = root(vec![]);
        bytes.insert(bytes.len() - 1, tag::END);
        assert!(read_root(&bytes).is_err());
    }

    #[test]
    fn deep_nesting_does_not_confuse_the_offsets() {
        // Five compounds deep, each carrying data, then back out intact.
        let bytes = root(vec![
            (
                "l1",
                n::comp(vec![(
                    "l2",
                    n::comp(vec![(
                        "l3",
                        n::comp(vec![("l4", n::comp(vec![("leaf", n::l(42))]))]),
                    )]),
                )]),
            ),
            ("after", n::str("still readable")),
        ]);
        let parsed = read_root(&bytes).expect("parses");
        let mut node = &parsed;
        for step in ["l1", "l2", "l3", "l4"] {
            node = node.get(step).expect("level exists");
        }
        assert_eq!(node.get("leaf"), Some(&Node::Long(42)));
        assert_eq!(
            parsed.get("after").and_then(Node::as_str),
            Some("still readable")
        );
    }
}

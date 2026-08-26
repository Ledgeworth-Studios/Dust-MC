//! Reading binary NBT as a borrowed view over the input.
//!
//! The owned reader turns bytes into [`Tag`](crate::Tag) trees that outlive
//! their input. This module is the other end of that trade: the document
//! keeps pointing into the buffer it was parsed from, and stays alive only
//! for as long as that buffer is.
//!
//! # What is borrowed, and what cannot be
//!
//! Every numeric payload is a view. A `TAG_Byte_Array` is a `&[u8]` slice of
//! the input; a `TAG_Int_Array` or `TAG_Long_Array` is a typed wrapper over
//! the same bytes, decoded on iteration rather than materialised — the wire
//! stores big-endian words, not `i32` arrays, and copying four bytes per
//! element to *store* them would give back the allocation this module exists
//! to avoid. A list of scalars — `Pos`, `Motion`, `Rotation`, the most common
//! lists in every chunk — is likewise a slice and a width, decoded element by
//! element on demand: zero allocations where the owned reader builds one
//! `Vec` per list plus one boxed [`Tag`](crate::Tag) per element.
//!
//! Strings are different, and the reason is the encoding. Modified UTF-8 is
//! not UTF-8 (see [`crate::mutf8`]): a NUL is two bytes, a character above
//! the BMP is six. A borrowed string that pointed into the input could not be
//! a `&str` at all — only a subset of payloads happen to be valid UTF-8, and
//! handing callers "a string that might secretly be invalid" is how the bug
//! class starts. So strings are decoded eagerly, on parse, into one flat
//! region the document owns; names and values reference it by offset. One
//! growing buffer for the whole document, whatever its string count — not
//! one allocation per string — and every decode error surfaces at parse time,
//! exactly where the owned reader reports it.
//!
//! Offsets rather than references also settle the self-reference question:
//! the region may reallocate as it grows, but offsets do not care, so no
//! unsafe and no pinning are needed to hold a tree that points at its own
//! string table.
//!
//! # The lifetime contract
//!
//! [`Document<'input>`] borrows the *decompressed* payload. The region-file
//! shape of a read is: decompress a slot into a buffer, parse it here, use
//! the view, drop both together. The borrow means the buffer cannot be
//! reused or freed while any part of the view is live — the compiler holds
//! the two ends together, which is the whole point of borrowing instead of
//! arena-freeing by hand.
//!
//! # Guards
//!
//! Identical to the owned reader's, because an attacker does not care which
//! module parsed them: the depth limit before recursion, the length check
//! before any reservation, the heap budget charged in this crate's own sizes.
//! Errors are the same type with the same offsets, so a caller can log one
//! line about a document without saying which reader produced it.

use crate::error::{Error, Result};
use crate::mutf8;
use crate::read::{Limits, Mode};
use crate::tag::TagType;

/// How many elements a node list reserves capacity for before it has read any.
///
/// Same policy, and the same number, as the owned reader: large enough that
/// real documents never regrow, small enough that a lying length prefix buys
/// a bounded reservation.
const RESERVE_LIMIT: usize = 4096;

/// An offset-and-length handle to text in a document's string region.
///
/// It means nothing outside the [`Document`] that produced it; resolve it
/// with [`Document::text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Str {
    offset: usize,
    len: u32,
}

/// A parsed document whose numbers borrow from the input.
///
/// Text lives in the document's own string region ([`Document::text`]
/// resolves it); everything else is a view over `'input`.
#[derive(Debug)]
pub struct Document<'input> {
    /// Decoded names and string values, concatenated in parse order.
    strings: String,
    root_name: Str,
    root: Value<'input>,
}

impl<'input> Document<'input> {
    /// Resolve a handle to the text it names.
    pub fn text(&self, text: Str) -> &str {
        let start = text.offset;
        let end = start + text.len as usize;
        // Offsets are only produced by this document's own parser, against
        // exactly this region; the bounds check below is unreachable except
        // through a handle from another document, which the API makes hard
        // to have and harder to want.
        &self.strings[start..end]
    }

    /// The name the root tag was stored under, empty in network form and in
    /// nearly every file vanilla writes.
    pub fn root_name(&self) -> &str {
        self.text(self.root_name)
    }

    /// The root value.
    pub fn root(&self) -> &Value<'input> {
        &self.root
    }

    /// A field of the root compound, if the root is one.
    ///
    /// Like the owned [`Compound::get`](crate::Compound::get), the last
    /// binding of `name` wins when the document carries duplicates.
    pub fn get<'doc>(&'doc self, name: &str) -> Option<&'doc Value<'input>> {
        match &self.root {
            Value::Compound(compound) => self.compound_get(compound, name),
            _ => None,
        }
    }

    /// Look up `name` in `compound`. On the document, not the compound:
    /// field names are offsets into this document's region, so comparing
    /// them needs the region, and keeping the method here makes it impossible
    /// to compare a compound's fields against some other document's text.
    pub fn compound_get<'doc>(
        &'doc self,
        compound: &'doc CompoundView<'input>,
        name: &str,
    ) -> Option<&'doc Value<'input>> {
        compound
            .fields
            .iter()
            .rev()
            .find(|(key, _)| self.text(*key) == name)
            .map(|(_, value)| value)
    }

    /// Follow a path of segments from the root, the walk of
    /// [`Tag::get_path`](crate::Tag::get_path): a segment names a compound
    /// field, or parses as an index into a list.
    ///
    /// Returns an owned [`Value`] rather than a reference because a step may
    /// land in a scalar-backed list, whose elements exist only when decoded.
    /// Compound fields and node-list elements are clones of the view's own
    /// data — cheap for scalars and handles, deeper for nested containers.
    pub fn get_path(&self, path: &[&str]) -> Option<Value<'input>> {
        let mut current = self.root.clone();
        for segment in path {
            current = match current {
                Value::Compound(compound) => self.compound_get(&compound, segment).cloned()?,
                Value::List(list) => list.get(segment.parse::<usize>().ok()?)?,
                _ => None?,
            };
        }
        Some(current)
    }
}

/// One NBT value, borrowed where the format permits.
///
/// # Equality
///
/// [`PartialEq`] answers "same document", not "same number": floats compare
/// by bit pattern exactly as [`Tag`](crate::Tag)'s do, so a NaN equals itself
/// and `-0.0` differs from `0.0`. A `Str` handle equals another when they
/// name the same slice of the same region — meaningful within one document,
/// meaningless across documents whose regions differ, which is what keeps
/// this cheap and honest at once.
#[derive(Debug, Clone)]
pub enum Value<'input> {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// The raw payload, still in the input. Chunk lighting reads this as
    /// unsigned nibbles; `Bytes::iter` yields the unsigned reading.
    ByteArray(Bytes<'input>),
    /// A handle into the document's string region.
    String(Str),
    List(ListView<'input>),
    Compound(CompoundView<'input>),
    /// Big-endian `i32`s, decoded on iteration.
    IntArray(Ints<'input>),
    /// Big-endian `i64`s, decoded on iteration.
    LongArray(Longs<'input>),
}

impl<'input> Value<'input> {
    /// Which of the thirteen tags this mirrors, matching
    /// [`Tag::tag_type`](crate::Tag::tag_type).
    pub fn tag_type(&self) -> TagType {
        match self {
            Value::Byte(_) => TagType::Byte,
            Value::Short(_) => TagType::Short,
            Value::Int(_) => TagType::Int,
            Value::Long(_) => TagType::Long,
            Value::Float(_) => TagType::Float,
            Value::Double(_) => TagType::Double,
            Value::ByteArray(_) => TagType::ByteArray,
            Value::String(_) => TagType::String,
            Value::List(_) => TagType::List,
            Value::Compound(_) => TagType::Compound,
            Value::IntArray(_) => TagType::IntArray,
            Value::LongArray(_) => TagType::LongArray,
        }
    }

    /// The compound this value is, if it is one.
    pub fn as_compound(&self) -> Option<&CompoundView<'input>> {
        match self {
            Value::Compound(compound) => Some(compound),
            _ => None,
        }
    }

    /// The list this value is, if it is one.
    pub fn as_list(&self) -> Option<&ListView<'input>> {
        match self {
            Value::List(list) => Some(list),
            _ => None,
        }
    }
}

impl PartialEq for Value<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Byte(a), Value::Byte(b)) => a == b,
            (Value::Short(a), Value::Short(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Long(a), Value::Long(b)) => a == b,
            // Bit patterns, matching `Tag`: the question is "same bytes".
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
            (Value::ByteArray(a), Value::ByteArray(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Compound(a), Value::Compound(b)) => a == b,
            (Value::IntArray(a), Value::IntArray(b)) => a == b,
            (Value::LongArray(a), Value::LongArray(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Value<'_> {}

/// The payload of a `TAG_Byte_Array`: the input's bytes, verbatim.
///
/// The format stores signed bytes here; lighting code and friends want the
/// unsigned reading, and `u8` is what a Rust slice natively yields, so
/// iteration is unsigned and [`Bytes::as_i8`] gives the signed one per byte.
#[derive(Debug, Clone, Copy)]
pub struct Bytes<'input>(&'input [u8]);

impl<'input> Bytes<'input> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The payload as it lies on the wire, for checksums and slices.
    pub fn as_slice(&self) -> &'input [u8] {
        self.0
    }
    /// The signed reading of element `index` — the format's own, since
    /// `TAG_Byte_Array` is an array of `TAG_Byte` and `TAG_Byte` is Java's
    /// signed byte.
    pub fn as_i8(&self, index: usize) -> Option<i8> {
        self.0.get(index).map(|&b| b as i8)
    }

    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().copied()
    }
}

impl PartialEq for Bytes<'_> {
    /// Byte equality, which for equal lengths is the whole question.
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Bytes<'_> {}

/// The payload of a `TAG_Int_Array`: big-endian `i32`s viewed in place.
///
/// No alignment is promised by the format — the array starts wherever the
/// previous tag ended — so the words are decoded per access rather than cast
/// in bulk, which a safe `&[i32]` could not promise anyway.
#[derive(Debug, Clone, Copy)]
pub struct Ints<'input>(&'input [u8]);

impl<'input> Ints<'input> {
    /// The raw big-endian words, for callers that want the bytes themselves.
    pub fn as_slice(&self) -> &'input [u8] {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len() / 4
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<i32> {
        let word = self.0.get(index * 4..index * 4 + 4)?;
        Some(i32::from_be_bytes([word[0], word[1], word[2], word[3]]))
    }

    pub fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        self.0
            .chunks_exact(4)
            .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
    }
}

impl PartialEq for Ints<'_> {
    /// Element equality: two views agree when every decoded word does, so a
    /// UUID is the same UUID however it arrived.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl Eq for Ints<'_> {}

/// The payload of a `TAG_Long_Array`: big-endian `i64`s viewed in place. See
/// [`Ints`] for why these are wrappers and not slices.
#[derive(Debug, Clone, Copy)]
pub struct Longs<'input>(&'input [u8]);

impl<'input> Longs<'input> {
    /// The raw big-endian words — see [`Ints::as_slice`].
    pub fn as_slice(&self) -> &'input [u8] {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len() / 8
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<i64> {
        let word = self.0.get(index * 8..index * 8 + 8)?;
        Some(i64::from_be_bytes([
            word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7],
        ]))
    }

    pub fn iter(&self) -> impl Iterator<Item = i64> + '_ {
        self.0
            .chunks_exact(8)
            .map(|c| i64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
    }
}

impl PartialEq for Longs<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl Eq for Longs<'_> {}

/// A `TAG_List` seen as a length, a declared element type, and a body that is
/// usually just more input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListView<'input> {
    element_type: TagType,
    body: Body<'input>,
}

/// What the elements physically are.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body<'input> {
    /// Declared empty. The declared type is kept on the list itself; vanilla
    /// writes `TAG_End`, other tools write whatever they liked.
    Empty,
    /// One primitive width, back to back. This is the case that pays for the
    /// whole module: a position list is eight bytes of input per coordinate,
    /// nothing allocated.
    Scalars { kind: Scalar, payload: &'input [u8] },
    /// Strings, compounds, lists or arrays — anything needing per-element
    /// structure beyond a fixed stride. Materialised as nodes.
    Values(Vec<Value<'input>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

impl Scalar {
    fn width(self) -> usize {
        match self {
            Scalar::Byte => 1,
            Scalar::Short => 2,
            Scalar::Int | Scalar::Float => 4,
            Scalar::Long | Scalar::Double => 8,
        }
    }

    fn decode(self, payload: &[u8]) -> Value<'static> {
        // Every arm decodes a copy small enough to own outright; no view is
        // involved, so the value carries no lifetime of its own.
        match self {
            Scalar::Byte => Value::Byte(payload[0] as i8),
            Scalar::Short => Value::Short(i16::from_be_bytes([payload[0], payload[1]])),
            Scalar::Int => Value::Int(i32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ])),
            Scalar::Long => Value::Long(i64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ])),
            Scalar::Float => Value::Float(f32::from_bits(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]))),
            Scalar::Double => Value::Double(f64::from_bits(u64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]))),
        }
    }
}

impl<'input> ListView<'input> {
    /// The declared element type, `TAG_End` included for the empty lists
    /// vanilla writes. Preserved exactly, like the owned reader's, so a
    /// rewrite of a foreign-typed empty list matches its input.
    pub fn element_type(&self) -> TagType {
        self.element_type
    }

    pub fn len(&self) -> usize {
        match &self.body {
            Body::Empty => 0,
            Body::Scalars { kind, payload } => payload.len() / kind.width(),
            Body::Values(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element `index`, decoded or cloned.
    ///
    /// Scalars cost a few bytes of arithmetic; node-backed elements clone the
    /// subtree, which is why [`ListView::values`] exists for callers that
    /// only need to look.
    pub fn get(&self, index: usize) -> Option<Value<'input>> {
        match &self.body {
            Body::Empty => None,
            Body::Scalars { kind, payload } => {
                let width = kind.width();
                payload
                    .get(index * width..index * width + width)
                    .map(|slice| kind.decode(slice))
            }
            Body::Values(values) => values.get(index).cloned(),
        }
    }

    /// The materialised nodes, when the body has them. `None` for scalar and
    /// empty bodies, which have no nodes to share.
    pub fn values(&self) -> Option<&[Value<'input>]> {
        match &self.body {
            Body::Values(values) => Some(values),
            _ => None,
        }
    }
}

/// A `TAG_Compound`: named fields in file order, names as [`Str`] handles.
///
/// Lookup by name lives on [`Document::compound_get`], because resolving a
/// name means reading this document's string region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompoundView<'input> {
    fields: Vec<(Str, Value<'input>)>,
}

impl<'input> CompoundView<'input> {
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Fields in file order: a name handle and the value it binds.
    pub fn iter(&self) -> std::slice::Iter<'_, (Str, Value<'input>)> {
        self.fields.iter()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Read a file-form document as a view: root id, root name, root payload.
///
/// Trailing bytes are not an error — region slots pad after the document;
/// [`from_bytes_exact`] refuses them.
///
/// ```
/// use dust_nbt::{borrow, write, Tag};
///
/// # fn main() -> Result<(), dust_nbt::Error> {
/// let original = write::to_vec("level", &Tag::Long(3955))?;
///
/// let document = borrow::from_bytes(&original)?;
/// assert_eq!(document.root_name(), "level");
/// assert_eq!(document.root(), borrow::Value::Long(3955));
/// # Ok(())
/// # }
/// ```
pub fn from_bytes(input: &[u8]) -> Result<Document<'_>> {
    from_bytes_with(input, Limits::default())
}

/// [`from_bytes`] with limits of your own.
pub fn from_bytes_with(input: &[u8], limits: Limits) -> Result<Document<'_>> {
    let mut parser = Parser::new(input, limits);
    let (root_name, root) = parser.read_root(Mode::File)?;
    Ok(Document {
        strings: parser.strings,
        root_name,
        root,
    })
}

/// [`from_bytes`], refusing anything after the document.
pub fn from_bytes_exact(input: &[u8]) -> Result<Document<'_>> {
    let mut parser = Parser::new(input, Limits::default());
    let (root_name, root) = parser.read_root(Mode::File)?;
    if parser.position < parser.input.len() {
        return Err(Error::TrailingBytes {
            offset: parser.position,
            remaining: parser.input.len() - parser.position,
        });
    }
    Ok(Document {
        strings: parser.strings,
        root_name,
        root,
    })
}

/// Read a network-form document as a view: root id, root payload, no name.
///
/// `Ok(None)` for the single `00` byte, exactly like the owned reader: that
/// is how the protocol spells "no NBT here", and treating it as an error
/// would reject the most common value on the wire.
pub fn from_bytes_network(input: &[u8]) -> Result<Option<Document<'_>>> {
    from_bytes_network_with(input, Limits::NETWORK)
}

/// [`from_bytes_network`] with limits of your own.
pub fn from_bytes_network_with(input: &[u8], limits: Limits) -> Result<Option<Document<'_>>> {
    let mut parser = Parser::new(input, limits);
    let first = match parser.input.first() {
        Some(&byte) => byte,
        None => {
            return Err(Error::UnexpectedEnd {
                offset: 0,
                needed: 1,
                available: 0,
                while_reading: "a tag id",
            })
        }
    };
    if first == TagType::End.id() {
        return Ok(None);
    }
    let (_, root) = parser.read_root(Mode::Network)?;
    Ok(Some(Document {
        strings: parser.strings,
        root_name: Str { offset: 0, len: 0 },
        root,
    }))
}

struct Parser<'input> {
    input: &'input [u8],
    position: usize,
    limits: Limits,
    depth: usize,
    heap_used: usize,
    /// Decoded text, appended in parse order; handles point into this.
    strings: String,
}

impl<'input> Parser<'input> {
    fn new(input: &'input [u8], limits: Limits) -> Self {
        Self {
            input,
            position: 0,
            limits,
            depth: 0,
            heap_used: 0,
            strings: String::new(),
        }
    }

    fn take(&mut self, count: usize, while_reading: &'static str) -> Result<&'input [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(Error::UnexpectedEnd {
                offset: self.position,
                needed: count,
                available: self.remaining(),
                while_reading,
            })?;
        let slice = self
            .input
            .get(self.position..end)
            .ok_or(Error::UnexpectedEnd {
                offset: self.position,
                needed: count,
                available: self.remaining(),
                while_reading,
            })?;
        self.position = end;
        Ok(slice)
    }

    fn remaining(&self) -> usize {
        self.input.len() - self.position
    }

    fn charge(&mut self, offset: usize, bytes: usize) -> Result<()> {
        self.heap_used = self.heap_used.saturating_add(bytes);
        if self.heap_used > self.limits.max_heap_bytes {
            return Err(Error::HeapBudgetExceeded {
                offset,
                used: self.heap_used,
                limit: self.limits.max_heap_bytes,
            });
        }
        Ok(())
    }

    fn enter(&mut self, offset: usize) -> Result<()> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            return Err(Error::TooDeep {
                offset,
                limit: self.limits.max_depth,
            });
        }
        Ok(())
    }

    fn read_i16(&mut self, while_reading: &'static str) -> Result<i16> {
        let bytes = self.take(2, while_reading)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self, while_reading: &'static str) -> Result<i32> {
        let bytes = self.take(4, while_reading)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_string(&mut self) -> Result<Str> {
        let header = self.position;
        let length = usize::from(self.read_i16("a string's length prefix")? as u16);
        let payload_offset = self.position;
        if length > self.remaining() {
            return Err(Error::UnexpectedEnd {
                offset: header,
                needed: length,
                available: self.remaining(),
                while_reading: "a string payload",
            });
        }
        self.charge(header, length)?;
        let bytes = self.take(length, "a string payload")?;
        let text = mutf8::decode(bytes).map_err(|source| Error::Utf8 {
            offset: payload_offset,
            source,
        })?;
        let str_handle = Str {
            offset: self.strings.len(),
            len: text.len() as u32,
        };
        self.strings.push_str(&text);
        Ok(str_handle)
    }

    /// Validate a claimed length against what the input could possibly hold,
    /// before anything reserves capacity for it. Same arithmetic, and the
    /// same refusal, as the owned reader.
    fn check_length(
        &self,
        offset: usize,
        raw: i32,
        tag: TagType,
        element_bytes: usize,
    ) -> Result<usize> {
        if raw < 0 {
            return Err(Error::NegativeLength {
                offset,
                length: raw,
                tag,
            });
        }
        let claimed = raw as usize;
        let minimum_bytes = claimed.saturating_mul(element_bytes);
        if minimum_bytes > self.remaining() {
            return Err(Error::LengthExceedsInput {
                offset,
                claimed,
                minimum_bytes,
                available: self.remaining(),
                tag,
            });
        }
        Ok(claimed)
    }

    fn read_root(&mut self, mode: Mode) -> Result<(Str, Value<'input>)> {
        let offset = self.position;
        let id = self.take(1, "a tag id")?[0];
        let tag_type = TagType::from_id(id).ok_or(Error::UnknownTagId { offset, id })?;
        if tag_type == TagType::End {
            return Err(Error::UnexpectedEndTag {
                offset,
                context: "the root tag of a document",
            });
        }
        // File form names the root; network form has nowhere to. The empty
        // handle is safe against an empty region and a populated one alike.
        let root_name = match mode {
            Mode::File => self.read_string()?,
            Mode::Network => Str { offset: 0, len: 0 },
        };
        let root = self.read_payload(tag_type)?;
        Ok((root_name, root))
    }

    fn read_payload(&mut self, tag_type: TagType) -> Result<Value<'input>> {
        self.charge(self.position, std::mem::size_of::<Value>())?;
        Ok(match tag_type {
            TagType::End => {
                return Err(Error::UnexpectedEndTag {
                    offset: self.position,
                    context: "a value",
                })
            }
            TagType::Byte => Value::Byte(self.take(1, "a TAG_Byte payload")?[0] as i8),
            TagType::Short => Value::Short(self.read_i16("a TAG_Short payload")?),
            TagType::Int => Value::Int(self.read_i32("a TAG_Int payload")?),
            TagType::Long => {
                let bytes = self.take(8, "a TAG_Long payload")?;
                Value::Long(i64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]))
            }
            TagType::Float => {
                let bytes = self.take(4, "a TAG_Float payload")?;
                Value::Float(f32::from_bits(u32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ])))
            }
            TagType::Double => {
                let bytes = self.take(8, "a TAG_Double payload")?;
                Value::Double(f64::from_bits(u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])))
            }
            TagType::ByteArray => {
                let offset = self.position;
                let raw = self.read_i32("a TAG_Byte_Array length")?;
                let length = self.check_length(offset, raw, TagType::ByteArray, 1)?;
                self.charge(offset, length)?;
                Value::ByteArray(Bytes(self.take(length, "a TAG_Byte_Array payload")?))
            }
            TagType::String => Value::String(self.read_string()?),
            TagType::List => Value::List(self.read_list()?),
            TagType::Compound => Value::Compound(self.read_compound()?),
            TagType::IntArray => {
                let offset = self.position;
                let raw = self.read_i32("a TAG_Int_Array length")?;
                let length = self.check_length(offset, raw, TagType::IntArray, 4)?;
                self.charge(offset, length * 4)?;
                Value::IntArray(Ints(self.take(length * 4, "a TAG_Int_Array payload")?))
            }
            TagType::LongArray => {
                let offset = self.position;
                let raw = self.read_i32("a TAG_Long_Array length")?;
                let length = self.check_length(offset, raw, TagType::LongArray, 8)?;
                self.charge(offset, length * 8)?;
                Value::LongArray(Longs(self.take(length * 8, "a TAG_Long_Array payload")?))
            }
        })
    }

    fn read_list(&mut self) -> Result<ListView<'input>> {
        let type_offset = self.position;
        let id = self.take(1, "a tag id")?[0];
        let element_type = TagType::from_id(id).ok_or(Error::UnknownTagId {
            offset: type_offset,
            id,
        })?;
        let length_offset = self.position;
        let raw = self.read_i32("a TAG_List length")?;

        if element_type == TagType::End {
            let length = self.check_length(length_offset, raw, TagType::List, 0)?;
            if length != 0 {
                return Err(Error::UnexpectedEndTag {
                    offset: type_offset,
                    context: "the element type of a list that claims elements",
                });
            }
            return Ok(ListView {
                element_type,
                body: Body::Empty,
            });
        }

        self.enter(length_offset)?;

        let body = match Scalar::of(element_type) {
            Some(kind) => {
                // Exact stride: the length check above already proved the
                // input holds every element the header claims.
                let length = self.check_length(length_offset, raw, TagType::List, kind.width())?;
                self.charge(length_offset, length * kind.width())?;
                let payload = self.take(length * kind.width(), "a scalar list's payload")?;
                Body::Scalars { kind, payload }
            }
            None => {
                let length = self.check_length(
                    length_offset,
                    raw,
                    TagType::List,
                    element_type.min_encoded_len(),
                )?;
                let mut values = Vec::with_capacity(length.min(RESERVE_LIMIT));
                for _ in 0..length {
                    values.push(self.read_payload(element_type)?);
                }
                Body::Values(values)
            }
        };

        self.depth -= 1;
        Ok(ListView { element_type, body })
    }

    fn read_compound(&mut self) -> Result<CompoundView<'input>> {
        let offset = self.position;
        self.enter(offset)?;
        let mut fields = Vec::new();
        loop {
            let id = self.take(1, "a tag id")?[0];
            let tag_type = TagType::from_id(id).ok_or(Error::UnknownTagId {
                offset: self.position - 1,
                id,
            })?;
            if tag_type == TagType::End {
                break;
            }
            let name = self.read_string()?;
            self.charge(self.position, std::mem::size_of::<(Str, Value)>())?;
            let value = self.read_payload(tag_type)?;
            fields.push((name, value));
        }
        self.depth -= 1;
        Ok(CompoundView { fields })
    }
}

impl Scalar {
    /// The scalar a list's element type must be for its body to be a plain
    /// stride of bytes; `None` for every container, string and array type.
    fn of(tag_type: TagType) -> Option<Self> {
        Some(match tag_type {
            TagType::Byte => Self::Byte,
            TagType::Short => Self::Short,
            TagType::Int => Self::Int,
            TagType::Long => Self::Long,
            TagType::Float => Self::Float,
            TagType::Double => Self::Double,
            _ => return None,
        })
    }
}

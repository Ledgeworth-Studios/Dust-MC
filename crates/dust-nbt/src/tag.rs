//! The tag model: what an NBT document is once it has been read.

use std::fmt;

/// The thirteen tag ids, as they appear on the wire and on disk.
///
/// `End` is one of the thirteen and is not one of the twelve variants of
/// [`Tag`]. It has no payload: on disk it is the single zero byte that
/// terminates a compound, and the element type of an empty list. A `Tag::End`
/// variant would be constructible, and every caller matching on a `Tag` would
/// have to write an arm for a value that can never be the value of a compound
/// field or a list element. Keeping it here and not there costs one enum and
/// removes that arm from every match in every downstream crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum TagType {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl TagType {
    /// The tag with this id, or `None` if nothing has that id.
    ///
    /// Ids above 12 are the single most common thing an attacker sends, so this
    /// is a lookup and not a cast.
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            0 => Self::End,
            1 => Self::Byte,
            2 => Self::Short,
            3 => Self::Int,
            4 => Self::Long,
            5 => Self::Float,
            6 => Self::Double,
            7 => Self::ByteArray,
            8 => Self::String,
            9 => Self::List,
            10 => Self::Compound,
            11 => Self::IntArray,
            12 => Self::LongArray,
            _ => return None,
        })
    }

    pub fn id(self) -> u8 {
        self as u8
    }

    /// The name this tag has in the format's own documentation, for error
    /// messages. `TAG_Byte_Array`, not `ByteArray`: an operator reading a log
    /// line is more likely to find the former in a wiki page.
    pub fn name(self) -> &'static str {
        match self {
            Self::End => "TAG_End",
            Self::Byte => "TAG_Byte",
            Self::Short => "TAG_Short",
            Self::Int => "TAG_Int",
            Self::Long => "TAG_Long",
            Self::Float => "TAG_Float",
            Self::Double => "TAG_Double",
            Self::ByteArray => "TAG_Byte_Array",
            Self::String => "TAG_String",
            Self::List => "TAG_List",
            Self::Compound => "TAG_Compound",
            Self::IntArray => "TAG_Int_Array",
            Self::LongArray => "TAG_Long_Array",
        }
    }

    /// The smallest number of input bytes one value of this type can occupy.
    ///
    /// This is what lets the reader refuse a length prefix before allocating
    /// for it: a list claiming four billion compounds needs at least four
    /// billion bytes of input, and the input is nine bytes long. Zero for
    /// `End`, which occupies no bytes at all and is therefore never counted
    /// this way — a list of `End` is rejected outright before this is asked.
    pub fn min_encoded_len(self) -> usize {
        match self {
            Self::End => 0,
            Self::Byte => 1,
            Self::Short => 2,
            Self::Int | Self::Float => 4,
            Self::Long | Self::Double => 8,
            // A byte array, string, list, int array or long array is at
            // minimum its own length prefix with a length of zero.
            Self::String => 2,
            Self::ByteArray | Self::List | Self::IntArray | Self::LongArray => 4,
            // The zero byte that ends it.
            Self::Compound => 1,
        }
    }
}

impl fmt::Display for TagType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One NBT value.
///
/// # Equality
///
/// [`PartialEq`] compares floats **by bit pattern**, not by IEEE 754 rules.
/// That is deliberate and it is not what `f32 == f32` does:
///
/// * `Tag::Float(f32::NAN) == Tag::Float(f32::NAN)` is `true` here, and
///   `f32::NAN == f32::NAN` is `false`.
/// * `Tag::Double(0.0) == Tag::Double(-0.0)` is `false` here, and
///   `0.0 == -0.0` is `true`.
///
/// The justification is that NBT is a serialisation format and the only
/// question this type is ever asked is whether two documents are the same
/// document. `0.0` and `-0.0` are different bytes and vanilla will tell them
/// apart; a NaN read back from a file is the same tag it was written from. IEEE
/// equality makes a tag unequal to itself, which would make every round-trip
/// assertion in the test suite unusable and would have to be worked around at
/// each call site — which is where a wrong workaround hides.
///
/// **What this does not catch**: two NaNs with different payload bits compare
/// unequal. That is correct for a byte-identity question and surprising for a
/// numeric one. Do not use `Tag` equality to ask a numeric question.
#[derive(Debug, Clone)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// `TAG_Byte_Array` holds *signed* bytes: it is an array of `TAG_Byte`, and
    /// `TAG_Byte` is Java's `byte`. Chunk lighting is the usual consumer and
    /// wants unsigned nibbles; `b as u8` is a no-op the optimiser removes, and
    /// a whole array converts to a `Vec<u8>` as a memcpy.
    ByteArray(Vec<i8>),
    /// Owned, not borrowed. See the module note in `mutf8`: the format's own
    /// string encoding is not UTF-8, so a string containing a NUL or any
    /// character above the BMP cannot be a `&str` pointing into the input at
    /// all. A `Cow<'a, str>` would borrow only the subset that happens to be
    /// plain UTF-8, in exchange for a lifetime parameter on `Tag`, `Compound`,
    /// `List` and everything downstream that holds one — and the buffer it
    /// would borrow from is decompression scratch that is dropped the moment
    /// the chunk is parsed. The cost is one allocation per string; the reader
    /// pays it exactly once, with no intermediate `Vec<u8>`.
    String(String),
    List(List),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn tag_type(&self) -> TagType {
        match self {
            Self::Byte(_) => TagType::Byte,
            Self::Short(_) => TagType::Short,
            Self::Int(_) => TagType::Int,
            Self::Long(_) => TagType::Long,
            Self::Float(_) => TagType::Float,
            Self::Double(_) => TagType::Double,
            Self::ByteArray(_) => TagType::ByteArray,
            Self::String(_) => TagType::String,
            Self::List(_) => TagType::List,
            Self::Compound(_) => TagType::Compound,
            Self::IntArray(_) => TagType::IntArray,
            Self::LongArray(_) => TagType::LongArray,
        }
    }

    /// The compound this tag is, if it is one.
    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Self::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&List> {
        match self {
            Self::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The value of any of the six numeric tags, widened.
    ///
    /// Deliberately not `as_int`: Minecraft's own data is full of fields that
    /// are a byte in one version and an int in the next, and a caller that
    /// matches on one of them breaks on a world saved by a different version.
    /// Returns `None` for a float or double, which are a different question.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Self::Byte(v) => i64::from(*v),
            Self::Short(v) => i64::from(*v),
            Self::Int(v) => i64::from(*v),
            Self::Long(v) => *v,
            _ => return None,
        })
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            Self::Float(v) => f64::from(*v),
            Self::Double(v) => *v,
            _ => return None,
        })
    }

    /// Follow a path of segments from this tag.
    ///
    /// A segment names a compound field, or — if the current tag is a list —
    /// parses as a decimal index into it, so
    /// `root.get_path(&["sections", "0", "block_states"])` reads the way the
    /// same navigation reads in `/data`. Returns `None` the first time a
    /// segment does not resolve: a missing field, an out-of-range index, or
    /// any segment applied to a scalar.
    ///
    /// What this deliberately is not is vanilla's full selector syntax: no
    /// ranges, no `{}`-matches-every-child, no recursion. Callers needing
    /// those have Minecraft knowledge, and per the crate note that lives with
    /// the code that has it.
    ///
    /// ```
    /// use dust_nbt::{Compound, Tag};
    ///
    /// let mut sections = dust_nbt::List::new(dust_nbt::TagType::Compound);
    /// let mut section = Compound::new();
    /// section.insert("Y", Tag::Byte(-4));
    /// sections.push(Tag::Compound(section)).expect("homogeneous");
    /// let mut root = Compound::new();
    /// root.insert("sections", Tag::List(sections));
    ///
    /// assert_eq!(
    ///     Tag::Compound(root).get_path(&["sections", "0", "Y"]),
    ///     Some(&Tag::Byte(-4))
    /// );
    /// ```
    pub fn get_path(&self, path: &[&str]) -> Option<&Tag> {
        let mut current = self;
        for segment in path {
            current = step(current, segment)?;
        }
        Some(current)
    }
}

fn step<'a>(current: &'a Tag, segment: &str) -> Option<&'a Tag> {
    match current {
        Tag::Compound(compound) => compound.get(segment),
        Tag::List(list) => list.get(segment.parse::<usize>().ok()?),
        _ => None,
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Byte(a), Self::Byte(b)) => a == b,
            (Self::Short(a), Self::Short(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Long(a), Self::Long(b)) => a == b,
            // Bit patterns, not IEEE 754 — see the type's doc comment.
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Double(a), Self::Double(b)) => a.to_bits() == b.to_bits(),
            (Self::ByteArray(a), Self::ByteArray(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Compound(a), Self::Compound(b)) => a == b,
            (Self::IntArray(a), Self::IntArray(b)) => a == b,
            (Self::LongArray(a), Self::LongArray(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Tag {}

/// A `TAG_List`: a length, an element type, and elements that all have it.
///
/// # The empty list
///
/// An empty list still carries an element type byte, and what goes in it is a
/// real interoperability question rather than a formality. Vanilla's
/// `ListTag.write` sets the type to `TAG_End` whenever the list is empty,
/// discarding whatever type the list was declared with, and only then writes
/// it. So every empty list in every file Minecraft has ever written has element
/// type 0.
///
/// This type stores the declared type instead of recomputing it, and writes
/// what it stores. On a file vanilla wrote the two are the same thing. On a
/// file some other tool wrote — one that emitted, say, type 10 for an empty
/// list of compounds — preserving it is what makes a read-then-write
/// byte-identical to its input. Constructing an empty list here with
/// [`List::new`] gives it `TAG_End`, so anything Dust originates matches
/// vanilla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    element_type: TagType,
    elements: Vec<Tag>,
}

impl List {
    /// An empty list of `element_type`.
    ///
    /// Pass [`TagType::End`] for a list that is meant to stay empty; that is
    /// what vanilla writes and what it expects to read.
    pub fn new(element_type: TagType) -> Self {
        Self {
            element_type,
            elements: Vec::new(),
        }
    }

    /// A list from elements that must already agree with each other.
    ///
    /// `Err` names the first element that disagrees, by index. A list whose
    /// elements disagree with its type is malformed NBT, not a list that needs
    /// widening: vanilla's reader refuses it and so does this one.
    pub fn from_elements(element_type: TagType, elements: Vec<Tag>) -> Result<Self, ListError> {
        for (index, element) in elements.iter().enumerate() {
            if element.tag_type() != element_type {
                return Err(ListError {
                    index,
                    expected: element_type,
                    found: element.tag_type(),
                });
            }
        }
        Ok(Self {
            element_type,
            elements,
        })
    }

    /// The declared element type. `TagType::End` for a list built empty.
    pub fn element_type(&self) -> TagType {
        self.element_type
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Tag> {
        self.elements.get(index)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Tag> {
        self.elements.iter()
    }

    /// Append, adopting the element type if the list was built empty.
    ///
    /// Returns `Err` rather than panicking or silently widening, because the
    /// caller that hits this is usually building a list from a heterogeneous
    /// source and needs to decide what to do about it, not to be killed.
    pub fn push(&mut self, tag: Tag) -> Result<(), ListError> {
        if self.elements.is_empty() && self.element_type == TagType::End {
            self.element_type = tag.tag_type();
        } else if tag.tag_type() != self.element_type {
            return Err(ListError {
                index: self.elements.len(),
                expected: self.element_type,
                found: tag.tag_type(),
            });
        }
        self.elements.push(tag);
        Ok(())
    }

    /// The elements, for a caller that wants to take them.
    pub fn into_elements(self) -> Vec<Tag> {
        self.elements
    }
}

impl<'a> IntoIterator for &'a List {
    type Item = &'a Tag;
    type IntoIter = std::slice::Iter<'a, Tag>;

    fn into_iter(self) -> Self::IntoIter {
        self.elements.iter()
    }
}

/// An element that does not belong in the list it was offered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListError {
    pub index: usize,
    pub expected: TagType,
    pub found: TagType,
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "list element {} is {} but the list holds {}",
            self.index, self.found, self.expected
        )
    }
}

impl std::error::Error for ListError {}

/// A `TAG_Compound`: named fields, **in the order they were read or added**.
///
/// # Why order is preserved rather than sorted
///
/// Vanilla's `CompoundTag` is a `java.util.HashMap`, and its `write` iterates
/// `keySet()` — so the order fields appear in a file is HashMap iteration
/// order, which is neither insertion order nor alphabetical, and which is a
/// function of the key hashes and the map's capacity at the time. It is not
/// reproducible from the field names alone by any rule.
///
/// That settles the question. Sorting the keys, or hashing them into a map of
/// our own, would produce a *different* byte sequence for the same document,
/// and read-then-write of a real `level.dat` or structure file would no longer
/// be byte-identical. Keeping the file's own order is the only representation
/// under which "we wrote back exactly what we read" is a check that can be run
/// — and that check is the one thing in this crate that cannot be satisfied by
/// being self-consistent. `tests/vanilla.rs` asserts, against Mojang's own
/// files, both that the rewrite is byte-identical and that the key order in
/// them is not sorted, so that this decision fails loudly if anyone changes it.
///
/// # Lookup cost
///
/// Fields are a `Vec<(String, Tag)>` and lookup is a scan. Real compounds are
/// small — a chunk root has about ten fields, a block entity fewer — and for
/// those a scan beats a hash. **What this does not catch**: a hostile document
/// may contain one compound with tens of thousands of one-character keys, and
/// a caller that looks up n keys in it does O(n·m) work. The reader itself
/// never looks anything up while parsing, so parsing such a document is linear;
/// the exposure is a *consumer* that walks an attacker-shaped compound by name.
///
/// # Duplicate keys
///
/// The reader appends fields without checking whether the name is already
/// present, because checking is what would make parsing quadratic in the
/// paragraph above. A document may therefore hold the same key twice.
/// [`Compound::get`] searches from the end and so returns the last binding,
/// which is what Java's `HashMap.put` leaves behind — matching vanilla here
/// matters, because a reader that took the *first* binding while the server it
/// talks to takes the last is a parser differential, and a document with two
/// `id` fields is exactly how that gets exploited. A rewrite preserves both
/// bindings, in order, so byte-identity survives.
#[derive(Debug, Clone, Default)]
pub struct Compound {
    fields: Vec<(String, Tag)>,
}

impl Compound {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            fields: Vec::with_capacity(capacity),
        }
    }

    /// The value bound to `name`, or `None`.
    ///
    /// Searches from the end, so a duplicated key resolves to the last
    /// binding. See the type's doc comment for why that is the vanilla answer.
    pub fn get(&self, name: &str) -> Option<&Tag> {
        self.fields
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Tag> {
        self.fields
            .iter_mut()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Bind `name`, replacing an existing binding in place so that field order
    /// does not change, and returning what was there.
    ///
    /// This is the API for building a compound, and it scans. The reader does
    /// not use it; see [`Compound::append`].
    pub fn insert(&mut self, name: impl Into<String>, value: Tag) -> Option<Tag> {
        let name = name.into();
        if let Some(slot) = self.fields.iter_mut().find(|(key, _)| *key == name) {
            return Some(std::mem::replace(&mut slot.1, value));
        }
        self.fields.push((name, value));
        None
    }

    /// Append a field without looking for an existing one.
    ///
    /// This is what the reader calls. It is O(1) where [`Compound::insert`] is
    /// O(n), and the difference is the difference between linear and quadratic
    /// parsing of a compound an attacker chose the shape of.
    pub fn append(&mut self, name: String, value: Tag) {
        self.fields.push((name, value));
    }

    /// Remove the last binding of `name`, preserving the order of the rest.
    pub fn remove(&mut self, name: &str) -> Option<Tag> {
        let position = self.fields.iter().rposition(|(key, _)| key == name)?;
        Some(self.fields.remove(position).1)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Fields in file order.
    pub fn iter(&self) -> std::slice::Iter<'_, (String, Tag)> {
        self.fields.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|(key, _)| key.as_str())
    }

    /// Follow a path of segments from this compound.
    ///
    /// The same walk as [`Tag::get_path`], starting one level in so a caller
    /// already holding a `Compound` need not wrap it first.
    ///
    /// ```
    /// use dust_nbt::{Compound, Tag};
    ///
    /// let mut inner = Compound::new();
    /// inner.insert("id", Tag::String("minecraft:stone".to_owned()));
    /// let mut root = Compound::new();
    /// root.insert("item", Tag::Compound(inner));
    ///
    /// assert_eq!(
    ///     root.get_path(&["item", "id"]).and_then(Tag::as_str),
    ///     Some("minecraft:stone")
    /// );
    /// ```
    pub fn get_path(&self, path: &[&str]) -> Option<&Tag> {
        let (first, rest) = path.split_first()?;
        let mut current = self.get(first)?;
        for segment in rest {
            current = step(current, segment)?;
        }
        Some(current)
    }
}

impl PartialEq for Compound {
    /// Order-sensitive, like everything else about this type.
    ///
    /// Two compounds with the same fields in a different order are different
    /// documents, because they are different bytes. A caller that wants the
    /// order-insensitive question should ask it explicitly.
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}

impl Eq for Compound {}

impl<'a> IntoIterator for &'a Compound {
    type Item = &'a (String, Tag);
    type IntoIter = std::slice::Iter<'a, (String, Tag)>;

    fn into_iter(self) -> Self::IntoIter {
        self.fields.iter()
    }
}

impl FromIterator<(String, Tag)> for Compound {
    fn from_iter<I: IntoIterator<Item = (String, Tag)>>(iter: I) -> Self {
        Self {
            fields: iter.into_iter().collect(),
        }
    }
}

impl<'a> IntoIterator for &'a mut Compound {
    type Item = &'a mut (String, Tag);
    type IntoIter = std::slice::IterMut<'a, (String, Tag)>;

    fn into_iter(self) -> Self::IntoIter {
        self.fields.iter_mut()
    }
}

impl IntoIterator for Compound {
    type Item = (String, Tag);
    type IntoIter = std::vec::IntoIter<(String, Tag)>;

    fn into_iter(self) -> Self::IntoIter {
        self.fields.into_iter()
    }
}

impl IntoIterator for List {
    type Item = Tag;
    type IntoIter = std::vec::IntoIter<Tag>;

    fn into_iter(self) -> Self::IntoIter {
        self.elements.into_iter()
    }
}

// The `From` conversions below exist for the shape of document *building*: a
// caller assembling a component payload or a test fixture thinks in values,
// and every one of these is a tag whose variant is exactly the value's type.
// Conversions that would have to choose a representation — a number widened
// to a wider tag, a slice copied — are left out; where the choice matters,
// the caller should make it visibly.

impl From<bool> for Tag {
    /// NBT has no boolean; Minecraft spells one as a byte. This picks the
    /// spelling SNBT's `true` and `false` parse to.
    fn from(value: bool) -> Self {
        Self::Byte(i8::from(value))
    }
}

impl From<i8> for Tag {
    fn from(value: i8) -> Self {
        Self::Byte(value)
    }
}

impl From<i16> for Tag {
    fn from(value: i16) -> Self {
        Self::Short(value)
    }
}

impl From<i32> for Tag {
    fn from(value: i32) -> Self {
        Self::Int(value)
    }
}

impl From<i64> for Tag {
    fn from(value: i64) -> Self {
        Self::Long(value)
    }
}

impl From<f32> for Tag {
    fn from(value: f32) -> Self {
        Self::Float(value)
    }
}

impl From<f64> for Tag {
    fn from(value: f64) -> Self {
        Self::Double(value)
    }
}

impl From<&str> for Tag {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Tag {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<Vec<i8>> for Tag {
    fn from(value: Vec<i8>) -> Self {
        Self::ByteArray(value)
    }
}

impl From<Vec<i32>> for Tag {
    fn from(value: Vec<i32>) -> Self {
        Self::IntArray(value)
    }
}

impl From<Vec<i64>> for Tag {
    fn from(value: Vec<i64>) -> Self {
        Self::LongArray(value)
    }
}

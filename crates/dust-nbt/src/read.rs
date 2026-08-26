//! Reading binary NBT.
//!
//! The reader works over a `&[u8]` with a cursor rather than an
//! [`std::io::Read`]. Two reasons, and only one of them is speed.
//!
//! The speed reason is that a chunk is read field by field and a `Read` costs a
//! virtual call and a bounds check per field, none of which is needed when the
//! whole document is already in memory — and it always is, because a chunk
//! arrives as a decompressed buffer and a packet arrives as a frame.
//!
//! The other reason is that it is what makes the allocation guard exact. A
//! `TAG_List` header carries a count, and the count is attacker-chosen. With a
//! stream, the only way to find out whether the count is a lie is to try to
//! satisfy it — which means allocating first. With a slice, the number of bytes
//! that could possibly remain is known, so a list claiming four billion
//! compounds is refused by a comparison against nine bytes of input rather than
//! by an allocator returning null. That check lives in
//! [`Reader::check_length`] and every length in the format goes through it.

use crate::error::{Error, Result};
use crate::mutf8;
use crate::tag::{Compound, List, Tag, TagType};

/// The most elements a list reserves capacity for before it has read any.
///
/// Large enough that no real document pays for a regrow — the biggest lists in
/// vanilla data are a chunk's block-entity list and a structure's block list,
/// both in the low thousands — and small enough that the reservation is 128 KiB
/// rather than a number the sender chose.
const RESERVE_LIMIT: usize = 4096;

/// How much a reader will put up with.
///
/// The three defences do three different jobs and none substitutes for
/// another. The **depth** limit stops a document from consuming the parser's
/// *stack*. The **length checks**, which are not configurable because there is
/// no sensible way to loosen them, stop a header from causing an allocation
/// the input could never fill. The **heap budget** stops the remaining case,
/// which the other two miss entirely: a document where every byte of input is
/// a legitimate tag, but each tag costs far more in memory than it did on the
/// wire. Two megabytes of `TAG_End` bytes inside a list of compounds is two
/// million `Tag` values — a forty-fold amplification, from input the length
/// check has no reason to object to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How many levels of list or compound may nest.
    ///
    /// Vanilla's `NbtAccounter` uses 512 and throws "Tried to read NBT tag with
    /// too high complexity, depth > 512" past it. Matching that number is
    /// deliberate: a lower limit would reject documents a vanilla client
    /// legitimately sends, and a higher one would accept documents the server
    /// on the other side of a proxy will not.
    pub max_depth: usize,
    /// How many bytes of decoded tag the document may add up to.
    ///
    /// This mirrors vanilla's `NbtAccounter` quota, which
    /// `FriendlyByteBuf.readNbt` sets to 2 MiB for packet NBT and which
    /// `NbtAccounter.unlimitedHeap()` leaves effectively unbounded for files.
    ///
    /// **What this does not catch, and where it disagrees with vanilla**: the
    /// budget is charged in *this* crate's sizes, not the JVM's. Vanilla
    /// charges 48 bytes for a compound and 37 for a list plus 4 per element;
    /// here a tag costs what a `Tag` costs. The totals are the same order of
    /// magnitude and are not equal, so a document engineered to sit within a
    /// few per cent of 2 MiB may be accepted by one implementation and refused
    /// by the other. That matters for a document nobody sends and not for any
    /// real one; making the two agree exactly would mean encoding the JVM's
    /// object layout here, which would be a lie the first time either changed.
    pub max_heap_bytes: usize,
}

impl Limits {
    /// What vanilla enforces on NBT arriving in a packet: 512 deep, 2 MiB of
    /// decoded tag.
    pub const NETWORK: Self = Self {
        max_depth: 512,
        max_heap_bytes: 2 * 1024 * 1024,
    };

    /// What vanilla enforces on NBT read from a file: 512 deep, and a heap
    /// quota it does not really impose.
    ///
    /// A file is not adversarial in the way a packet is — an operator who can
    /// write to the world directory has already won — and a legitimate chunk
    /// with a full block-entity list is large. The budget is left at
    /// [`usize::MAX`] here for the same reason vanilla leaves it at
    /// `Long.MAX_VALUE`: the thing that bounds a file is the decompression
    /// limit in [`crate::compression`], applied before these bytes exist.
    pub const FILE: Self = Self {
        max_depth: 512,
        max_heap_bytes: usize::MAX,
    };
}

impl Default for Limits {
    /// [`Limits::FILE`]. The network entry points do not use this; they use
    /// [`Limits::NETWORK`], so the stricter default lands where it belongs
    /// rather than where a caller forgot to ask for it.
    fn default() -> Self {
        Self::FILE
    }
}

/// Which dialect of the binary format a document is in.
///
/// # Why this is a parameter and not a guess
///
/// Both dialects start with the root tag's id byte. In the file form the id is
/// followed by a `u16` length and that many bytes of the root's name — which is
/// the empty string in every file Minecraft writes, so the first three bytes
/// are `0a 00 00`. In the network form, used since 1.20.2 and therefore by
/// 1.21.1, the name is absent entirely and the root's first field follows the
/// id directly.
///
/// A reader could try to tell them apart by looking at the two bytes after the
/// id and treating `00 00` as an empty name. It would be right almost always,
/// and wrong on the case that matters. Consider the network-form compound whose
/// first field is a `TAG_End` — that is, the empty compound `{}`, whose whole
/// encoding is `0a 00`. Read as file form, the `00` is the first half of a name
/// length and the document is truncated. Consider instead a network-form
/// compound whose first field has a one-character name: `0a 01 00 01 78 …`. The
/// two bytes after the id are `01 00`, so the file-form reading takes a
/// 256-byte name and gets something else entirely.
///
/// The guess is not merely unreliable, it is *attacker-selectable*: the first
/// field of a packet compound is very often a name the sender chose, so the
/// sender chooses which way the guess goes. The mode is therefore told to the
/// reader by the caller who knows whether the bytes came from a file or from a
/// packet, and there is no `guess()` in this crate to reach for instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `level.dat`, region-file chunks, player data: id, name, payload.
    File,
    /// Packet NBT since 1.20.2: id, payload. No root name.
    Network,
}

/// A cursor over a document.
#[derive(Debug)]
pub struct Reader<'a> {
    input: &'a [u8],
    position: usize,
    limits: Limits,
    depth: usize,
    heap_used: usize,
}

/// A root tag together with the name it was stored under.
///
/// The name is the empty string in every file vanilla writes; it is returned
/// rather than discarded because a tool that is not Minecraft may have written
/// something there, and dropping it would make a rewrite differ from its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub tag: Tag,
}

/// Read a file-form document: root id, root name, root payload.
///
/// Trailing bytes are not an error — a region file stores each chunk in a slot
/// padded to a multiple of 4 KiB, and the padding follows the document. Use
/// [`from_bytes_exact`] where nothing should follow.
///
/// ```
/// use dust_nbt::{read, write, Tag};
///
/// # fn main() -> Result<(), dust_nbt::Error> {
/// let original = write::to_vec("level", &Tag::Long(3955))?;
///
/// let document = read::from_bytes(&original)?;
/// assert_eq!(document.name, "level");
/// assert_eq!(document.tag, Tag::Long(3955));
/// # Ok(())
/// # }
/// ```
pub fn from_bytes(input: &[u8]) -> Result<Named> {
    from_bytes_with(input, Limits::default())
}

/// [`from_bytes`] with limits of your own.
pub fn from_bytes_with(input: &[u8], limits: Limits) -> Result<Named> {
    let mut reader = Reader::new(input, limits);
    reader.read_root(Mode::File)
}

/// [`from_bytes`], refusing anything after the document.
pub fn from_bytes_exact(input: &[u8]) -> Result<Named> {
    let mut reader = Reader::new(input, Limits::default());
    let named = reader.read_root(Mode::File)?;
    reader.finish()?;
    Ok(named)
}

/// Read a network-form document: root id, root payload, no name.
///
/// Since 1.20.2 this is what NBT in a packet looks like. Returns `Ok(None)` for
/// the single byte `00`, which is how the protocol spells "no NBT here" — a
/// slot with no components, an entity with no custom data. Treating that as a
/// parse error would reject the most common value on the wire.
///
/// ```
/// use dust_nbt::{read, write, Tag};
///
/// # fn main() -> Result<(), dust_nbt::Error> {
/// let absent = read::from_bytes_network(&[0x00])?;
/// assert_eq!(absent, None);
///
/// let bytes = write::to_vec_network(Some(&Tag::Byte(1)))?;
/// assert_eq!(read::from_bytes_network(&bytes)?, Some(Tag::Byte(1)));
/// # Ok(())
/// # }
/// ```
pub fn from_bytes_network(input: &[u8]) -> Result<Option<Tag>> {
    from_bytes_network_with(input, Limits::NETWORK)
}

/// [`from_bytes_network`] with limits of your own.
pub fn from_bytes_network_with(input: &[u8], limits: Limits) -> Result<Option<Tag>> {
    let mut reader = Reader::new(input, limits);
    // The single byte `00` is how the protocol spells an absent NBT field, so
    // it is checked before the root is read rather than being let through to
    // fail as a TAG_End root.
    if reader.peek_tag_id()? == TagType::End {
        return Ok(None);
    }
    reader.read_root(Mode::Network).map(|named| Some(named.tag))
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a [u8], limits: Limits) -> Self {
        Self {
            input,
            position: 0,
            limits,
            depth: 0,
            heap_used: 0,
        }
    }

    /// How many bytes of decoded tag have been charged against the budget.
    pub fn heap_used(&self) -> usize {
        self.heap_used
    }

    /// How many bytes have been consumed. A region-file reader uses this to
    /// find where the chunk ended inside its slot.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Fail if anything is left.
    pub fn finish(&self) -> Result<()> {
        if self.position < self.input.len() {
            return Err(Error::TrailingBytes {
                offset: self.position,
                remaining: self.input.len() - self.position,
            });
        }
        Ok(())
    }

    /// Read one document in `mode`.
    pub fn read_root(&mut self, mode: Mode) -> Result<Named> {
        let offset = self.position;
        let tag_type = self.read_tag_id()?;
        if tag_type == TagType::End {
            return Err(Error::UnexpectedEndTag {
                offset,
                context: "the root tag of a document",
            });
        }
        let name = match mode {
            Mode::File => self.read_string()?,
            Mode::Network => String::new(),
        };
        let tag = self.read_payload(tag_type)?;
        Ok(Named { name, tag })
    }

    fn peek_tag_id(&self) -> Result<TagType> {
        let &id = self.input.get(self.position).ok_or(Error::UnexpectedEnd {
            offset: self.position,
            needed: 1,
            available: 0,
            while_reading: "a tag id",
        })?;
        TagType::from_id(id).ok_or(Error::UnknownTagId {
            offset: self.position,
            id,
        })
    }

    fn read_tag_id(&mut self) -> Result<TagType> {
        let tag_type = self.peek_tag_id()?;
        self.position += 1;
        Ok(tag_type)
    }

    fn take(&mut self, count: usize, while_reading: &'static str) -> Result<&'a [u8]> {
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

    /// Validate a length prefix before anything is allocated for it.
    ///
    /// `claimed` is what the header says, `element_bytes` the fewest bytes one
    /// entry can occupy. The product is the fewest bytes the whole thing could
    /// occupy, and if that exceeds what is left of the input the header is a
    /// lie — no allocation, no reserve, no `with_capacity`.
    ///
    /// **What this does not catch**: a header that claims a plausible number of
    /// elements the input genuinely does contain. That is not an attack, it is
    /// a large document, and bounding *that* is the job of whoever chose to
    /// hand this function a large slice — the decompressor's output limit for a
    /// file, the packet frame length for a packet.
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

    /// Charge `bytes` against the heap budget.
    ///
    /// Saturating, so that a document cannot overflow the counter back to a
    /// small number and buy itself more room — which is the bug this kind of
    /// counter usually has.
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

    fn read_i8(&mut self, while_reading: &'static str) -> Result<i8> {
        Ok(self.take(1, while_reading)?[0] as i8)
    }

    fn read_i16(&mut self, while_reading: &'static str) -> Result<i16> {
        let bytes = self.take(2, while_reading)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self, while_reading: &'static str) -> Result<i32> {
        let bytes = self.take(4, while_reading)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self, while_reading: &'static str) -> Result<i64> {
        let bytes = self.take(8, while_reading)?;
        let mut buffer = [0u8; 8];
        buffer.copy_from_slice(bytes);
        Ok(i64::from_be_bytes(buffer))
    }

    /// A `u16`-prefixed modified-UTF-8 string.
    ///
    /// The length is unsigned here — it is Java's `writeUTF` prefix, not one of
    /// the format's signed lengths — so there is no negative case, only a
    /// length longer than the input.
    fn read_string(&mut self) -> Result<String> {
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
        mutf8::decode(bytes)
            .map(|text| text.into_owned())
            .map_err(|source| Error::Utf8 {
                offset: payload_offset,
                source,
            })
    }

    fn read_payload(&mut self, tag_type: TagType) -> Result<Tag> {
        // Every tag costs at least one `Tag` in the tree, whatever its
        // payload; the payload is charged on top by the readers below.
        self.charge(self.position, std::mem::size_of::<Tag>())?;
        Ok(match tag_type {
            // Only reachable through a caller that already ruled it out; each
            // does so with a message naming where the End was found, which is
            // more useful than anything that could be said here.
            TagType::End => {
                return Err(Error::UnexpectedEndTag {
                    offset: self.position,
                    context: "a value",
                })
            }
            TagType::Byte => Tag::Byte(self.read_i8("a TAG_Byte payload")?),
            TagType::Short => Tag::Short(self.read_i16("a TAG_Short payload")?),
            TagType::Int => Tag::Int(self.read_i32("a TAG_Int payload")?),
            TagType::Long => Tag::Long(self.read_i64("a TAG_Long payload")?),
            TagType::Float => {
                Tag::Float(f32::from_bits(self.read_i32("a TAG_Float payload")? as u32))
            }
            TagType::Double => {
                Tag::Double(f64::from_bits(self.read_i64("a TAG_Double payload")? as u64))
            }
            TagType::ByteArray => Tag::ByteArray(self.read_byte_array()?),
            TagType::String => Tag::String(self.read_string()?),
            TagType::List => Tag::List(self.read_list()?),
            TagType::Compound => Tag::Compound(self.read_compound()?),
            TagType::IntArray => Tag::IntArray(self.read_int_array()?),
            TagType::LongArray => Tag::LongArray(self.read_long_array()?),
        })
    }

    fn read_byte_array(&mut self) -> Result<Vec<i8>> {
        let offset = self.position;
        let raw = self.read_i32("a TAG_Byte_Array length")?;
        let length = self.check_length(offset, raw, TagType::ByteArray, 1)?;
        self.charge(offset, length)?;
        let bytes = self.take(length, "a TAG_Byte_Array payload")?;
        // `u8 as i8` is a reinterpretation with no run-time cost, and the
        // optimiser turns the whole loop into a memcpy. Written this way
        // rather than with a transmute because the crate denies unsafe.
        Ok(bytes.iter().map(|&b| b as i8).collect())
    }

    fn read_int_array(&mut self) -> Result<Vec<i32>> {
        let offset = self.position;
        let raw = self.read_i32("a TAG_Int_Array length")?;
        let length = self.check_length(offset, raw, TagType::IntArray, 4)?;
        self.charge(offset, length * 4)?;
        let bytes = self.take(length * 4, "a TAG_Int_Array payload")?;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    fn read_long_array(&mut self) -> Result<Vec<i64>> {
        let offset = self.position;
        let raw = self.read_i32("a TAG_Long_Array length")?;
        let length = self.check_length(offset, raw, TagType::LongArray, 8)?;
        self.charge(offset, length * 8)?;
        let bytes = self.take(length * 8, "a TAG_Long_Array payload")?;
        Ok(bytes
            .chunks_exact(8)
            .map(|c| i64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
            .collect())
    }

    fn read_list(&mut self) -> Result<List> {
        let type_offset = self.position;
        let element_type = self.read_tag_id()?;
        let length_offset = self.position;
        let raw = self.read_i32("a TAG_List length")?;

        if element_type == TagType::End {
            // Vanilla writes TAG_End as the element type of an empty list, so
            // this is normal — but only with a length of zero. `ListTag`'s
            // reader throws "Missing type on ListTag" for a non-empty one, and
            // it has to: there is no type to read the elements as.
            let length = self.check_length(length_offset, raw, TagType::List, 0)?;
            if length != 0 {
                return Err(Error::UnexpectedEndTag {
                    offset: type_offset,
                    context: "the element type of a list that claims elements",
                });
            }
            return Ok(List::new(TagType::End));
        }

        let length = self.check_length(
            length_offset,
            raw,
            TagType::List,
            element_type.min_encoded_len(),
        )?;
        self.enter(length_offset)?;
        // Reserve for what is plausible, not for what was claimed. The length
        // check above proves the input is long enough for `length` elements at
        // their *minimum* size, and for a list of compounds that minimum is one
        // byte — so a two-megabyte packet may honestly claim two million
        // elements, and reserving for all of them up front is eighty megabytes
        // before a single one has been read. Growing the vector costs an
        // amortised copy and bounds the reservation to what the input actually
        // produced.
        let mut elements = Vec::with_capacity(length.min(RESERVE_LIMIT));
        for _ in 0..length {
            elements.push(self.read_payload(element_type)?);
        }
        self.depth -= 1;
        // Every element was read *as* `element_type`, so this cannot fail; it
        // is the constructor that upholds the invariant and going through it
        // means there is exactly one place where a `List` can be made.
        List::from_elements(element_type, elements).map_err(|e| Error::HeterogeneousList {
            index: e.index,
            expected: e.expected,
            found: e.found,
        })
    }

    fn read_compound(&mut self) -> Result<Compound> {
        let offset = self.position;
        self.enter(offset)?;
        let mut compound = Compound::new();
        loop {
            let tag_type = self.read_tag_id()?;
            if tag_type == TagType::End {
                break;
            }
            let name = self.read_string()?;
            // The `(String, Tag)` slot the field will occupy, over and above
            // the name's bytes and the value's own cost.
            self.charge(self.position, std::mem::size_of::<(String, Tag)>())?;
            let value = self.read_payload(tag_type)?;
            // `append`, not `insert`: see `Compound`'s doc comment. Checking
            // for a duplicate here is what would make a compound of n
            // attacker-chosen keys cost O(n^2) to parse.
            compound.append(name, value);
        }
        self.depth -= 1;
        Ok(compound)
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
}

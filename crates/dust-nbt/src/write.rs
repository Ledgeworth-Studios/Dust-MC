//! Writing binary NBT.
//!
//! The writer appends to a `Vec<u8>` rather than driving an
//! [`std::io::Write`], for the same reason the reader takes a slice: a chunk is
//! compressed after it is serialised and a packet is framed after it is
//! serialised, so the bytes have to exist in a buffer either way, and going
//! through a `Write` only adds a copy.
//!
//! Everything that can fail is checked *before* anything is appended, so a
//! failed write leaves the buffer as it found it up to the tag that failed —
//! not a half-written document with a plausible-looking prefix.

use crate::error::{Error, Result};
use crate::mutf8;
use crate::read::Mode;
use crate::tag::{Compound, List, Tag, TagType};

/// Serialise a file-form document: root id, root name, root payload.
///
/// The name is the empty string in every file Minecraft writes. It is a
/// parameter rather than a constant because reading gives one back, and a
/// round-trip that silently dropped it would not be one.
pub fn to_vec(name: &str, tag: &Tag) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1024);
    write_into(&mut out, name, tag)?;
    Ok(out)
}

/// [`to_vec`], appending to a buffer the caller owns.
pub fn write_into(out: &mut Vec<u8>, name: &str, tag: &Tag) -> Result<()> {
    out.push(tag.tag_type().id());
    write_string(out, name)?;
    write_payload(out, tag)
}

/// Serialise a network-form document: root id, root payload, no name.
///
/// `None` writes the single byte `00`, which is how the protocol since 1.20.2
/// spells "no NBT here".
pub fn to_vec_network(tag: Option<&Tag>) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1024);
    write_into_network(&mut out, tag)?;
    Ok(out)
}

/// [`to_vec_network`], appending to a buffer the caller owns.
pub fn write_into_network(out: &mut Vec<u8>, tag: Option<&Tag>) -> Result<()> {
    match tag {
        None => {
            out.push(TagType::End.id());
            Ok(())
        }
        Some(tag) => {
            out.push(tag.tag_type().id());
            write_payload(out, tag)
        }
    }
}

/// Serialise in whichever dialect `mode` names.
///
/// `name` is ignored in [`Mode::Network`], which has nowhere to put it. It is
/// ignored rather than refused because the caller that has both is usually
/// forwarding a document from a file to a packet, and the name it is carrying
/// is the empty string.
pub fn write_into_mode(out: &mut Vec<u8>, mode: Mode, name: &str, tag: &Tag) -> Result<()> {
    match mode {
        Mode::File => write_into(out, name, tag),
        Mode::Network => write_into_network(out, Some(tag)),
    }
}

fn write_string(out: &mut Vec<u8>, text: &str) -> Result<()> {
    // Measured before anything is appended. `writeUTF`'s length prefix is a
    // `u16`, so a string whose *encoded* length exceeds 65,535 cannot be
    // written at all — and note that the limit is on the encoded length, not on
    // `text.len()`: a string of 40,000 emoji is 160,000 bytes in UTF-8 and
    // 240,000 here, and one of 40,000 NULs is 40,000 bytes in UTF-8 and 80,000
    // here. Silently truncating would produce a document that parses and means
    // something else; panicking would let a player's item name kill the server.
    let encoded_len = mutf8::encoded_len(text);
    if encoded_len > mutf8::MAX_ENCODED_LEN {
        return Err(Error::StringTooLong(mutf8::StringTooLong {
            encoded_len,
            prefix: text.chars().take(32).collect(),
        }));
    }
    out.extend_from_slice(&(encoded_len as u16).to_be_bytes());
    mutf8::encode_into(text, out);
    Ok(())
}

fn write_array_len(out: &mut Vec<u8>, len: usize, tag: TagType) -> Result<()> {
    if len > i32::MAX as usize {
        return Err(Error::ArrayTooLong { len, tag });
    }
    out.extend_from_slice(&(len as i32).to_be_bytes());
    Ok(())
}

fn write_payload(out: &mut Vec<u8>, tag: &Tag) -> Result<()> {
    match tag {
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        // `to_bits` rather than any float formatting: the bit pattern is the
        // payload, which is what makes a NaN read from a file the same NaN when
        // written back, quiet or signalling, payload bits and all.
        Tag::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::ByteArray(values) => {
            write_array_len(out, values.len(), TagType::ByteArray)?;
            out.reserve(values.len());
            out.extend(values.iter().map(|&v| v as u8));
        }
        Tag::String(text) => write_string(out, text)?,
        Tag::List(list) => write_list(out, list)?,
        Tag::Compound(compound) => write_compound(out, compound)?,
        Tag::IntArray(values) => {
            write_array_len(out, values.len(), TagType::IntArray)?;
            out.reserve(values.len() * 4);
            for value in values {
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
        Tag::LongArray(values) => {
            write_array_len(out, values.len(), TagType::LongArray)?;
            out.reserve(values.len() * 8);
            for value in values {
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    Ok(())
}

fn write_list(out: &mut Vec<u8>, list: &List) -> Result<()> {
    // A `List` built through this crate's constructors cannot disagree with
    // itself, but one can be cloned, mutated field-by-field by a future change,
    // or built by a downstream crate that finds a way around the constructor.
    // Checking here is cheap and the alternative is writing a document that
    // vanilla refuses with a message about a different tag entirely.
    for (index, element) in list.iter().enumerate() {
        if element.tag_type() != list.element_type() {
            return Err(Error::HeterogeneousList {
                index,
                expected: list.element_type(),
                found: element.tag_type(),
            });
        }
    }
    // The declared type, not `elements[0]`'s type. For an empty list those
    // differ: there is no element zero, and what goes in the byte is the whole
    // question. See `List`'s doc comment — vanilla writes TAG_End for an empty
    // list, `List::new` gives an empty list TAG_End, and a declared type that
    // came from somewhere else is preserved so that a rewrite matches its
    // input.
    out.push(list.element_type().id());
    write_array_len(out, list.len(), TagType::List)?;
    for element in list.iter() {
        write_payload(out, element)?;
    }
    Ok(())
}

fn write_compound(out: &mut Vec<u8>, compound: &Compound) -> Result<()> {
    for (name, value) in compound.iter() {
        out.push(value.tag_type().id());
        write_string(out, name)?;
        write_payload(out, value)?;
    }
    out.push(TagType::End.id());
    Ok(())
}

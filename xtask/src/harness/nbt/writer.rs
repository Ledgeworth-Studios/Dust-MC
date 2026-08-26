//! NBT construction for the tests.
//!
//! The reader above is tested against bytes built here, never against Mojang
//! files — the repository may hold neither. Tests build a [`Node`] tree with
//! the small constructors at the bottom of this file, then encode it; the
//! encoder mirrors the reader one rule at a time, which is what makes a
//! round-trip test meaningful rather than circular.

use super::tag;
use super::Node;

/// Encode one root compound, exactly as [`super::read_root`] expects it.
///
/// `named` emits the type byte itself, so the root is just a normal named
/// tag with the conventional empty name.
pub(crate) fn root(entries: Vec<(&str, Node)>) -> Vec<u8> {
    let mut out = Vec::new();
    named(
        &mut out,
        "",
        &Node::Compound(
            entries
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        ),
    );
    out
}

/// Encode one named tag: type byte, name, payload.
pub(crate) fn named(out: &mut Vec<u8>, name: &str, node: &Node) {
    out.push(kind_of(node));
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    payload(out, node);
}

fn kind_of(node: &Node) -> u8 {
    match node {
        Node::Byte(_) => tag::BYTE,
        Node::Short(_) => tag::SHORT,
        Node::Int(_) => tag::INT,
        Node::Long(_) => tag::LONG,
        Node::Float(_) => tag::FLOAT,
        Node::Double(_) => tag::DOUBLE,
        Node::ByteArray(_) => tag::BYTE_ARRAY,
        Node::String(_) => tag::STRING,
        Node::List(_) => tag::LIST,
        Node::Compound(_) => tag::COMPOUND,
        Node::IntArray(_) => tag::INT_ARRAY,
        Node::LongArray(_) => tag::LONG_ARRAY,
    }
}

fn payload(out: &mut Vec<u8>, node: &Node) {
    match node {
        Node::Byte(v) => out.push(*v as u8),
        Node::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Node::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Node::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Node::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Node::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Node::ByteArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            out.extend_from_slice(v);
        }
        Node::String(v) => {
            out.extend_from_slice(&(v.len() as u16).to_be_bytes());
            out.extend_from_slice(v.as_bytes());
        }
        Node::List(items) => {
            let element_kind = items.first().map(kind_of).unwrap_or(tag::END);
            out.push(element_kind);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for item in items {
                payload(out, item);
            }
        }
        Node::Compound(entries) => {
            for (name, value) in entries {
                named(out, name, value);
            }
            out.push(tag::END);
        }
        Node::IntArray(values) => {
            out.extend_from_slice(&(values.len() as i32).to_be_bytes());
            for v in values {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Node::LongArray(values) => {
            out.extend_from_slice(&(values.len() as i32).to_be_bytes());
            for v in values {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
}

/// Constructors, kept short because test code reads like data.
///
/// `n::i(3)` is an int node; the single letters follow the tag names.
pub(crate) mod n {
    use super::Node;

    pub(crate) fn b(v: i8) -> Node {
        Node::Byte(v)
    }

    pub(crate) fn s(v: i16) -> Node {
        Node::Short(v)
    }

    pub(crate) fn i(v: i32) -> Node {
        Node::Int(v)
    }

    pub(crate) fn l(v: i64) -> Node {
        Node::Long(v)
    }

    pub(crate) fn f(v: f32) -> Node {
        Node::Float(v)
    }

    pub(crate) fn d(v: f64) -> Node {
        Node::Double(v)
    }

    pub(crate) fn str(v: &str) -> Node {
        Node::String(v.to_owned())
    }

    pub(crate) fn ba(v: &[u8]) -> Node {
        Node::ByteArray(v.to_vec())
    }

    pub(crate) fn ia(v: &[i32]) -> Node {
        Node::IntArray(v.to_vec())
    }

    pub(crate) fn la(v: &[i64]) -> Node {
        Node::LongArray(v.to_vec())
    }

    pub(crate) fn list(items: Vec<Node>) -> Node {
        Node::List(items)
    }

    pub(crate) fn comp(entries: Vec<(&str, Node)>) -> Node {
        Node::Compound(
            entries
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }
}

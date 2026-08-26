//! The field types only the chat packets use, and the seam where signing
//! will plug in.
//!
//! # What is decided here, and what is deliberately not
//!
//! The 1.19 chat format binds every player message to a signing session: a
//! key pair the client registers, a per-message signature chain over recent
//! messages, and acknowledgements that tell the server which of those the
//! client has seen. All of it is *layout*, and layout is this crate's job —
//! a decoder that cannot step over a signature loses the packet. All of the
//! *meaning* is elsewhere: Dust is offline-first, verifies no signatures and
//! produces none, and that policy is why these types hold bytes rather than
//! cryptography.
//!
//! The seam is therefore exact. [`SignatureBytes`] is where a signature lives;
//! [`AcknowledgedMessage`] is how a reference to an earlier message travels;
//! [`MessageAcknowledgement`] is the client's "here is what I have seen". The
//! day online mode arrives, verification reads those same fields and nothing
//! in `packet_group` changes. Until then, tests pin the layouts, because an
//! uninterpreted field is exactly the kind a refactor quietly reorders.

use crate::types::{BitSet, Decode, Encode, FixedBitSet, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, wire_struct, ProtocolVersion};

/// How many bytes a chat signature always is.
///
/// Always means always: present or absent as a whole, never truncated, and
/// **not** length-prefixed when present. The count is a constant of the
/// format, which is why it is a type here and not a number at each call site.
pub const SIGNATURE_BYTES: usize = 256;

/// One RSA signature over a chat message or its context.
///
/// A fixed byte array rather than prefixed bytes for the reason
/// [`SIGNATURE_BYTES`] states. Held opaquely: interpreting one is online-mode
/// work that does not exist yet, and pretending otherwise would mean shipping
/// half a verifier.
pub type SignatureBytes = [u8; SIGNATURE_BYTES];

/// A reference to a previously seen signed message, in its packed wire form.
///
/// The packing is subtle enough to own a type. A message is referenced by its
/// index plus one; index-plus-one of **zero** is the escape hatch meaning "no
/// index — here is the full signature instead", and in exactly that case the
/// 256 signature bytes follow with no boolean prefix. Every other value
/// carries nothing after it. Reading this as a plain optional adds a phantom
/// boolean and desynchronises the rest of the packet; reading it without the
/// zero case drops real signatures on the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgedMessage {
    /// The referenced message's index plus one, or zero for the inline form.
    pub id: VarInt,
    /// Present if and only if [`Self::id`] is zero.
    pub signature: Option<SignatureBytes>,
}

impl Decode for AcknowledgedMessage {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let id = input.read_var_int()?;
        let signature = if id == 0 {
            Some(input.read_array()?)
        } else {
            None
        };
        Ok(Self {
            id: VarInt(id),
            signature,
        })
    }
}

impl Encode for AcknowledgedMessage {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.id.0);
        if self.id.0 == 0 {
            // Encoding zero without the signature it promises would be a
            // frame the peer decodes into the next field's bytes. Refusing
            // beats writing a lie.
            let signature = self.signature.as_ref().ok_or(EncodeError::Unsupported {
                field: "acknowledged message",
                why: "id zero promises an inline signature, and none was given",
            })?;
            out.write_slice(signature.as_slice());
        } else if let Some(_unused) = &self.signature {
            // A non-zero id must not carry bytes: the reader would take them
            // for the next entry's id. The type cannot represent that state
            // through decode, so this arm is unreachable from round trips —
            // and deliberately still refuses, for hand-built values.
            return Err(EncodeError::Unsupported {
                field: "acknowledged message",
                why: "a referenced message carries no inline signature",
            });
        }
        Ok(())
    }
}

wire_struct! {
    /// What the client says it has verified, sent with every chat message.
    ///
    /// The offset is a position in the server's message log; the bit set marks
    /// the twenty messages after it as acknowledged or not. Twenty is a
    /// constant of the format — the size of the client's history window — so
    /// the fixed-width bit set is the honest shape and not an optimisation.
    pub struct MessageAcknowledgement {
        offset: VarInt,
        acknowledged: FixedBitSet<20>,
    }
}

/// How much of a received message a server with profanity filtering hid.
///
/// A tagged union rather than an optional: the mask exists only in the partial
/// case, and modelling it as `Option` outside an enum would let an encoder
/// send a mask alongside "nothing was filtered" — nonsense the vanilla client
/// has no defence against because no vanilla server can produce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatFilter {
    /// Nothing was hidden.
    PassThrough,
    /// Everything was hidden.
    FullyFiltered,
    /// The marked characters were hidden.
    PartiallyFiltered(crate::types::BitSet),
}

impl Decode for ChatFilter {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        const PASS_THROUGH: i32 = 0;
        const FULLY_FILTERED: i32 = 1;
        const PARTIALLY_FILTERED: i32 = 2;
        match input.read_var_int()? {
            PASS_THROUGH => Ok(Self::PassThrough),
            FULLY_FILTERED => Ok(Self::FullyFiltered),
            PARTIALLY_FILTERED => BitSet::decode(input, version).map(Self::PartiallyFiltered),
            other => Err(DecodeError::UnknownVariant {
                name: "ChatFilter",
                value: other,
            }),
        }
    }
}

impl Encode for ChatFilter {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::PassThrough => out.write_var_int(0),
            Self::FullyFiltered => out.write_var_int(1),
            Self::PartiallyFiltered(bits) => {
                out.write_var_int(2);
                return bits.encode(out, version);
            }
        }
        Ok(())
    }
}

var_int_enum! {
    /// What a chat-completions update does to the client's hint set.
    pub enum ChatCompletionsAction {
        Add = 0,
        Remove = 1,
        Set = 2,
    }
}

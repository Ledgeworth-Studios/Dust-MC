//! What players say to each other, and what the server says to them.
//!
//! # Everything goes out as a *system* message, and that is a decision
//!
//! The protocol has two ways to deliver a chat line. `player_chat` carries the
//! sender's identity, a timestamp, a salt and a cryptographic signature the
//! client verifies against that player's session key — it is how a vanilla
//! client can show a message as provably from somebody. `system_chat` carries
//! a text component and nothing else.
//!
//! Dust sends system messages, including for player speech, and formats the
//! sender's name into the text itself. That is what a server which does not
//! sign chat can honestly do. Forwarding a client's own signature would be
//! worse than not signing: the signature covers the message *and the
//! acknowledgement chain it was sent in*, so a server that relays it while
//! reordering, filtering or delaying anything produces a signature the client
//! rejects — and a chat system that intermittently rejects real messages is
//! harder to live with than one that never claims to be signed at all.
//!
//! The visible consequence is that messages carry no "not secure" warning and
//! also no verified badge: to the client they are the server talking. That is
//! accurate. The status document says `enforcesSecureChat` is absent, meaning
//! false, so no client is expecting otherwise.
//!
//! # Why nothing is rendered here as a string
//!
//! A message is built as a [`Component`] tree and encoded as NBT. Formatting a
//! name into a string and sending that would put whatever the player typed
//! into a position where the client parses formatting codes out of it — so a
//! player called `a` saying `§kb` would be styling somebody else's chat. A
//! component keeps the two apart by construction: the name is one node and the
//! text is another, and neither can become the other.

use dust_protocol::text::{Color, Component, NamedColor};

/// The longest message this server will relay.
///
/// The protocol bounds the field at 256 and `dust-protocol` enforces that on
/// the way in. This is the *outgoing* bound, and it exists separately because
/// what goes out is longer than what came in — a name and punctuation are
/// added — and the client's own limit is on the rendered component.
pub const MAX_MESSAGE: usize = 256;

/// `<name> message`, the shape every Minecraft server has used since 2010.
///
/// Two nodes, not one string: see the module note on why the name and the
/// message never touch.
pub fn player_said(name: &str, message: &str) -> Component {
    Component::text(format!("<{name}> ")).with_extra(vec![Component::text(message.to_owned())])
}

/// `name joined the game`, in vanilla's yellow.
pub fn joined(name: &str) -> Component {
    Component::text(format!("{name} joined the game")).colored(Color::Named(NamedColor::Yellow))
}

/// `name left the game`.
pub fn left(name: &str) -> Component {
    Component::text(format!("{name} left the game")).colored(Color::Named(NamedColor::Yellow))
}

/// Whether a message is worth relaying at all.
///
/// Empty and whitespace-only messages are dropped: a client cannot normally
/// send one, which is exactly why a server should not assume it never will.
/// Everything else is relayed as typed — there is no filtering here, and
/// pretending otherwise by adding a token blocklist would be worse than the
/// honest absence.
pub fn is_sendable(message: &str) -> bool {
    !message.trim().is_empty() && message.len() <= MAX_MESSAGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_protocol::text::Body;

    fn text_of(component: &Component) -> String {
        match &component.body {
            Body::Text(text) => text.clone(),
            other => panic!("expected plain text, got {other:?}"),
        }
    }

    #[test]
    fn a_message_keeps_the_name_and_the_words_in_separate_nodes() {
        // The property this exists for. A player called `a` saying `§kb` must
        // not be able to style anybody else's chat, and the only way to
        // guarantee that is for the two never to be one string.
        let said = player_said("a", "§kb");
        assert_eq!(text_of(&said), "<a> ");
        assert_eq!(said.extra.len(), 1);
        assert_eq!(text_of(&said.extra[0]), "§kb");
    }

    #[test]
    fn an_empty_message_is_not_relayed() {
        assert!(!is_sendable(""));
        assert!(!is_sendable("   "));
        assert!(!is_sendable("\t\n"));
        assert!(is_sendable("hello"));
    }

    #[test]
    fn an_overlong_message_is_refused_rather_than_truncated() {
        // Truncating would relay half a sentence as though the player had said
        // it, which is putting words in somebody's mouth — and the client
        // cannot send one this long anyway, so refusing costs nobody anything.
        assert!(is_sendable(&"a".repeat(MAX_MESSAGE)));
        assert!(!is_sendable(&"a".repeat(MAX_MESSAGE + 1)));
    }

    #[test]
    fn the_join_and_leave_lines_read_the_way_every_server_has_since_2010() {
        assert_eq!(text_of(&joined("Steve")), "Steve joined the game");
        assert_eq!(text_of(&left("Steve")), "Steve left the game");
    }
}

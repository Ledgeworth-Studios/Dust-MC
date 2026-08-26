//! The server-list entry: what a client shows before anybody has logged in.
//!
//! # Why the JSON is built here and not in `dust-protocol`
//!
//! `dust-protocol` carries a status response as a string and says so in its own
//! documentation: the shape inside — version, players, description, favicon —
//! is a *server policy* decision rather than a protocol one. Two Dust
//! deployments answer the same packet with different documents, and a proxy in
//! front of them answers with a third. So the packet layer ends at "a
//! length-prefixed string travels here", and this module owns the string.
//!
//! # What a real 1.21.1 server sends, and where Dust differs on purpose
//!
//! Captured by pinging a vanilla 1.21.1 server, because a document that is
//! valid JSON can still be the wrong document and no unit test would say so:
//!
//! ```text
//! {"version":{"name":"1.21.1","protocol":767},
//!  "description":"A Minecraft Server",
//!  "players":{"max":20,"online":0}}
//! ```
//!
//! Two things Dust used to send and no longer does, because that capture said
//! otherwise: an empty `players.sample`, and an explicit
//! `"enforcesSecureChat":false`. Each key's own note below records what the
//! wrong reasoning was.
//!
//! One deliberate difference remains. Vanilla writes a plain MOTD as a bare
//! string, since a string *is* a text component; Dust writes `{"text":"…"}`.
//! Both are valid and the client renders them identically, and the object form
//! is chosen because it is unambiguous: an operator whose MOTD happens to begin
//! with a brace would have it parsed as a component rather than shown, and a
//! MOTD is arbitrary operator text.
//!
//! # Why the version name is not a constant
//!
//! When a client's protocol number does not match the server's, the client
//! shows the server's `version.name` where it would otherwise show a ping, in
//! red. That text is the only place a mismatched player is told what happened,
//! so it is derived from the protocol version this server actually speaks
//! rather than written down beside it — a hard-coded "1.21.1" beside a server
//! that had been bumped would tell every mismatched client the wrong thing,
//! and no test that only speaks the matching version would notice.

use std::fmt::Write as _;

use dust_protocol::ProtocolVersion;

use super::favicon::Favicon;

/// Everything the answer to a ping is made of, resolved once at boot.
///
/// Held as owned data rather than borrowed from the configuration because it
/// outlives the boot phase that built it and is read from the network runtime,
/// which has no relationship to the configuration's lifetime.
#[derive(Debug, Clone)]
pub struct StatusPolicy {
    version: ProtocolVersion,
    motd: String,
    max_players: u32,
    favicon: Option<Favicon>,
}

impl StatusPolicy {
    pub fn new(
        version: ProtocolVersion,
        motd: impl Into<String>,
        max_players: u32,
        favicon: Option<Favicon>,
    ) -> Self {
        Self {
            version,
            motd: motd.into(),
            max_players,
            favicon,
        }
    }

    pub fn max_players(&self) -> u32 {
        self.max_players
    }

    /// Render the document for a moment when `online` players are connected.
    ///
    /// `online` is an argument rather than a field because it is the one part
    /// of this that changes between two pings a second apart. Everything else
    /// was decided at boot.
    ///
    /// # The document, key by key
    ///
    /// * `version.name` — shown only on a protocol mismatch; see the module
    ///   documentation.
    /// * `version.protocol` — what the client compares against its own. A
    ///   client whose number differs gets the "outdated server/client" line.
    /// * `players.online` / `players.max` — the two numbers in the list. The
    ///   maximum is presentation, not admission control: the gateway may admit
    ///   more across several backends, which is why the configuration says so
    ///   at its own field.
    /// * `players.sample` — the hover list, **omitted when empty**, which is
    ///   what a real 1.21.1 server does. Checked rather than assumed: a vanilla
    ///   server with nobody on it sends `{"max":20,"online":0}` and no
    ///   `sample` key at all. Dust sent an empty array here first, on the
    ///   theory that some list clients dislike a missing key; that theory had
    ///   no evidence behind it and vanilla's behaviour is evidence against, so
    ///   it went.
    /// * `description` — a text component. Still *JSON* here on 1.21.1, unlike
    ///   every other component since 1.20.3, which is the trap this whole
    ///   packet is known for.
    /// * `favicon` — omitted entirely when there is none. A `null` is not the
    ///   same thing to every client that parses this.
    /// * `enforcesSecureChat` — **omitted**, which means false. The client's
    ///   codec reads this field with a default of `false`, so absent and
    ///   `false` are the same statement, and vanilla omits it: a real 1.21.1
    ///   server sends no such key whether `enforce-secure-profile` is on or
    ///   off. Dust sent an explicit `false` first with a comment claiming the
    ///   key had to be present or the client would warn about unsigned
    ///   messages — which had the sense backwards, since it is the *true* value
    ///   that makes a client expect signatures. The day Dust signs chat, this
    ///   is where `true` goes.
    pub fn render(&self, online: u32) -> String {
        let mut json = String::with_capacity(256);
        json.push_str(r#"{"version":{"name":""#);
        escape_json_into(self.version.name(), &mut json);
        // `protocol_number` is what the client compares; `name` is what it
        // prints when the comparison fails.
        let _ = write!(json, r#"","protocol":{}}},"#, self.version.number());
        let _ = write!(
            json,
            r#""players":{{"max":{},"online":{online}}},"#,
            self.max_players
        );
        json.push_str(r#""description":{"text":""#);
        escape_json_into(&self.motd, &mut json);
        json.push_str(r#""}"#);
        if let Some(favicon) = &self.favicon {
            json.push_str(r#","favicon":""#);
            // A data URI is base64 plus a fixed prefix: no character in it
            // needs escaping. It goes through the escaper anyway, because the
            // day something else supplies this string the alternative is a
            // silent injection.
            escape_json_into(favicon.data_uri(), &mut json);
            json.push('"');
        }
        json.push('}');
        json
    }
}

/// Append `text` as the inside of a JSON string.
///
/// Hand-written for the same reason as the base64 encoder next door, and with
/// the same obligation: it is pinned to the cases that matter rather than to
/// its own output. The rule is RFC 8259's — the two mandatory escapes are the
/// quote and the backslash, and every code point below U+0020 must be escaped
/// somehow. A MOTD is operator-supplied text that reaches an unauthenticated
/// stranger's parser, so "somehow" is `\u00XX` for everything in that range
/// that has no shorter spelling.
///
/// What is deliberately *not* escaped: `<`, `>`, `&` and U+2028/U+2029. Those
/// are HTML and JavaScript concerns, and this string is never HTML; escaping
/// them would change a MOTD's bytes for no reader's benefit.
fn escape_json_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_protocol::version;
    use std::path::Path;

    fn policy(motd: &str) -> StatusPolicy {
        StatusPolicy::new(version::V1_21_1, motd, 20, None)
    }

    /// Pull a `"key":` value out of the rendered document by scanning, so the
    /// tests read the bytes that go on the wire rather than a parse of them.
    /// A test that parses first cannot see a document that is valid JSON and
    /// still wrong for this protocol.
    fn field<'a>(json: &'a str, key: &str) -> &'a str {
        let start = json
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("no {key} in {json}"))
            + key.len()
            + 3;
        let rest = &json[start..];
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                    if depth == 0 {
                        return &rest[..=i];
                    }
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' if depth == 0 => return &rest[..i],
                '}' | ']' => depth -= 1,
                ',' if depth == 0 => return &rest[..i],
                _ => {}
            }
        }
        rest
    }

    #[test]
    fn the_protocol_number_is_the_one_this_server_speaks() {
        let json = policy("hi").render(0);
        assert_eq!(field(&json, "protocol"), "767", "{json}");
        assert_eq!(field(&json, "name"), "\"1.21.1\"", "{json}");
    }

    #[test]
    fn the_online_count_is_the_one_passed_and_the_maximum_the_one_configured() {
        let json = StatusPolicy::new(version::V1_21_1, "hi", 137, None).render(9);
        assert_eq!(field(&json, "online"), "9", "{json}");
        assert_eq!(field(&json, "max"), "137", "{json}");
    }

    #[test]
    fn an_empty_sample_is_omitted_the_way_vanilla_omits_it() {
        // Captured from a real 1.21.1 server with nobody connected: the whole
        // players object is `{"max":20,"online":0}`. Dust sent `"sample":[]`
        // here until that capture said otherwise, on a theory about
        // third-party list clients that had no evidence behind it.
        let json = policy("hi").render(0);
        assert!(!json.contains("sample"), "{json}");
        assert!(
            json.contains(r#""players":{"max":20,"online":0}"#),
            "{json}"
        );
    }

    #[test]
    fn secure_chat_enforcement_is_omitted_because_dust_does_not_enforce_it() {
        // Absent and `false` are the same statement to the client's codec, and
        // vanilla sends no key at all — with enforce-secure-profile on *or*
        // off, which was checked against a running server rather than assumed.
        assert!(!policy("hi").render(0).contains("enforcesSecureChat"));
    }

    #[test]
    fn no_favicon_means_no_key_rather_than_a_null() {
        assert!(!policy("hi").render(0).contains("favicon"));
    }

    #[test]
    fn a_favicon_appears_as_a_data_uri() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&64u32.to_be_bytes());
        let icon = Favicon::from_png(Path::new("i.png"), &png).expect("valid");
        let json = StatusPolicy::new(version::V1_21_1, "hi", 20, Some(icon)).render(0);
        assert!(
            field(&json, "favicon").starts_with("\"data:image/png;base64,"),
            "{json}"
        );
    }

    #[test]
    fn a_motd_containing_a_quote_cannot_end_the_string_early() {
        // The whole reason the escaper exists. An operator writes a MOTD with
        // an apostrophe-style quote in it and every client in the world gets a
        // document that stops parsing halfway.
        let json = policy(r#"the "best" server"#).render(0);
        assert!(json.contains(r#"\"best\""#), "{json}");
        assert_eq!(field(&json, "text"), r#""the \"best\" server""#, "{json}");
    }

    #[test]
    fn a_motd_containing_a_backslash_survives_intact() {
        let json = policy(r"C:\dust").render(0);
        assert!(json.contains(r"C:\\dust"), "{json}");
    }

    #[test]
    fn control_characters_are_escaped_rather_than_emitted_raw() {
        // A raw byte below 0x20 inside a JSON string is invalid JSON, and the
        // client's parser is entitled to give up on the whole document.
        let json = policy("a\u{1}b\u{1f}c").render(0);
        assert!(json.contains(r"\u0001"), "{json}");
        assert!(json.contains(r"\u001f"), "{json}");
        assert!(
            !json.chars().any(|c| (c as u32) < 0x20),
            "no raw control byte may reach the wire: {json:?}"
        );
    }

    #[test]
    fn the_legacy_section_sign_passes_through_untouched() {
        // Colour codes in a MOTD are the normal case, and the client applies
        // them itself. Escaping the section sign would render the codes as
        // literal text.
        let json = policy("§aGreen §lBold").render(0);
        assert!(json.contains("§aGreen §lBold"), "{json}");
    }

    #[test]
    fn a_motd_of_pure_emoji_is_carried_as_written() {
        // The status document is UTF-8, unlike NBT's modified UTF-8 next door
        // in the text component encoder. Nothing here should be doing surrogate
        // arithmetic, and this test fails if somebody adds some.
        let json = policy("⛏️🔥").render(0);
        assert!(json.contains("⛏️🔥"), "{json}");
    }
}

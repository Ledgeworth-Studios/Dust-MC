//! The login conversation: a checked state machine that walks one connection
//! from Login Start to the authenticated transition.
//!
//! # Why this exists as its own module
//!
//! [`crate::login`] owns the login *materials* — key pair, verify token,
//! decryption, digest. [`crate::session`] owns the *question to Mojang*.
//! Neither owns the conversation, and the conversation is where the real
//! rules live: which packet may arrive now, what gets sent in which order,
//! when the mode switches land, and what the client is told when any of it
//! fails. Those rules are the difference between a pile of correct primitives
//! and a login a vanilla client can complete.
//!
//! # The seam, stated honestly
//!
//! `dust-net` does not know what packets mean — see [`crate::state`]. This
//! module knows what four login-phase packets *look like*, because building
//! and parsing them is negotiation mechanics rather than gameplay knowledge:
//! the encryption request carries this server's public key, the compression
//! announcement configures this crate's own codec, the success packet names
//! who authenticated, and the acknowledgement is the transition trigger. The
//! ids below are 1.21.1's; if a protocol bump moves them, they move here and
//! nowhere else in the crate.
//!
//! # The two paths
//!
//! **Offline mode** is three packets: Login Start, then Set Compression (if a
//! threshold is configured), then Login Success carrying an MD5-derived
//! version-3 UUID over `"OfflinePlayer:" + name`, exactly as a vanilla
//! offline server computes it. Nobody is verified; that is what offline
//! means, and the docs of [`AuthMode::Offline`] say so plainly.
//!
//! **Online mode** inserts the key exchange and Mojang between Start and
//! Success: Encryption Request, Encryption Response (decrypt, verify the
//! token challenge), the session-server query with the login digest from
//! [`server_id_hash`], then encryption on, compression announced, and a
//! Success whose identity comes from the profile Mojang answered with —
//! including Mojang's spelling of the name, which outranks whatever case the
//! client typed.
//!
//! # Name policy, written down rather than implied
//!
//! The wire name is trimmed of leading and trailing ASCII whitespace, must be
//! 3–16 characters of `[A-Za-z0-9_]` after trimming, and keeps its case for
//! display while comparisons use ASCII-lowercase. Vanilla does not trim; Dust
//! does, because a name arriving with a stray space is a launcher quirk worth
//! absorbing once, here, rather than a mismatch worth debugging later. In
//! online mode Mojang's authoritative spelling replaces the trimmed input at
//! Success; offline mode has no authority above the trim.
//!
//! # Failure is a Disconnect, then an error
//!
//! Nearly every rejection path sends a login-state Disconnect naming the
//! reason before returning, so the client sees why instead of a bare hangup.
//! Two arms stay silent on purpose: transport failures, where there is nobody
//! left to tell, and verify-token mismatches, where the two ends disagree
//! about which bytes are ciphertext and any message would arrive as noise.
//! In online mode the rejection Disconnect itself travels encrypted, because
//! the client switched its ciphers the moment it answered the challenge —
//! the handler enables its own before asking Mojang for exactly this reason.
//! The returned error always carries the structured cause; log lines should
//! come from it, not from reverse-engineering a JSON string.

use md5::{Digest as _, Md5};

use crate::frame::{Compress, Frame};
use crate::io::{Conn, ConnError};
use crate::login::{server_id_hash, KeyError, ServerKey, VerifyToken};
use crate::session::{Profile, SessionError, SessionServer};
use crate::state::State;
use crate::varint::{read_var_int, write_var_int};

/// Serverbound Login Start, in the login state. See the seam note in the
/// module docs for why these ids live here.
pub const LOGIN_START_ID: i32 = 0x00;
/// The profile id a client appends to its name in Login Start, in bytes.
///
/// A UUID, unprefixed and mandatory since 1.20.5. See [`LoginHandler`]'s
/// `expect_start` for what it used to be and why that mattered.
pub const PROFILE_ID_BYTES: usize = 16;
/// Serverbound Encryption Response.
pub const ENCRYPTION_RESPONSE_ID: i32 = 0x01;
/// Serverbound Login Acknowledged, added in 1.20.2.
pub const LOGIN_ACKNOWLEDGED_ID: i32 = 0x03;
/// Clientbound Disconnect, login state.
pub const LOGIN_DISCONNECT_ID: i32 = 0x00;
/// Clientbound Encryption Request.
pub const ENCRYPTION_REQUEST_ID: i32 = 0x01;
/// Clientbound Login Success.
pub const LOGIN_SUCCESS_ID: i32 = 0x02;
/// Clientbound Set Compression.
pub const SET_COMPRESSION_ID: i32 = 0x03;

/// Which identity regime a login runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Verify against Mojang. Requires a session server and a server key;
    /// every player is who Mojang says they are, or is not let in.
    Online,
    /// Verify nothing. The name is canonicalised and an MD5 id derived from
    /// it, and anyone may claim any name. This is the default because it is
    /// the configuration that cannot leak a token or lean on third-party
    /// uptime without being chosen on purpose; a public server should choose
    /// [`AuthMode::Online`] deliberately.
    Offline,
}

/// Everything a login attempt needs beyond the connection itself.
#[derive(Debug, Clone)]
pub struct LoginConfig {
    /// Which identity regime to run. See [`AuthMode`] for why the default is
    /// offline.
    pub mode: AuthMode,
    /// The compression threshold announced in Set Compression, or `None` to
    /// never compress the login. The default of 256 matches vanilla: packets
    /// smaller than it travel raw, because zlib on a short keepalive buys no
    /// bytes and spends latency.
    pub compression_threshold: Option<i32>,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::Offline,
            compression_threshold: Some(256),
        }
    }
}

impl LoginConfig {
    /// Configuration for [`AuthMode::Online`].
    pub fn online() -> Self {
        Self {
            mode: AuthMode::Online,
            ..Self::default()
        }
    }

    /// Configuration for [`AuthMode::Offline`].
    pub fn offline() -> Self {
        Self::default()
    }
}

/// Who completed a login, and under which regime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticated {
    /// The profile id the game should know this player by: Mojang's answer in
    /// online mode, the derived MD5 id in offline mode.
    pub profile_id: [u8; 16],
    /// The username after canonicalisation — Mojang's spelling online, the
    /// trimmed input offline.
    pub username: String,
    /// The full profile in online mode, textures included; `None` offline,
    /// where no such thing exists to hand over.
    pub profile: Option<Profile>,
}

/// Why a login ended without an [`Authenticated`].
#[derive(Debug)]
pub enum LoginError {
    /// The transport failed or a clock ran out underneath the conversation.
    /// No Disconnect was sent; there was nowhere left to send one.
    Transport(ConnError),
    /// A frame arrived that this phase could not use — wrong id, or a body
    /// that does not parse. The conversation cannot resume: there is no
    /// telling whether the misread byte was the head of some other packet.
    UnexpectedFrame { reason: String },
    /// The claimed name breaks the character rule in the module docs.
    BadUsername(BadUsername),
    /// The key exchange refused the response: undecryptable blob, wrong-length
    /// secret, or a verify token answering a different challenge.
    KeyExchange(KeyError),
    /// The session-server exchange failed at the HTTP layer or below.
    Session(SessionError),
    /// The session server answered, and the answer was "no such join". The
    /// everyday shape of someone playing someone else.
    Unverified { username: String },
    /// Online mode was configured without supplying a server key.
    MissingServerKey,
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "the connection failed during login: {error}"),
            Self::UnexpectedFrame { reason } => {
                write!(f, "the login carried an unexpected frame: {reason}")
            }
            Self::BadUsername(error) => write!(f, "the claimed username is unusable: {error}"),
            Self::KeyExchange(error) => write!(f, "the key exchange failed: {error}"),
            Self::Session(error) => write!(f, "the session server exchange failed: {error}"),
            Self::Unverified { username } => write!(
                f,
                "{username} never joined this server according to the session server"
            ),
            Self::MissingServerKey => write!(
                f,
                "online mode needs a server key; none was supplied to the handler"
            ),
        }
    }
}

impl std::error::Error for LoginError {}

/// A username the rule refuses, with the raw text kept for the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadUsername {
    pub attempted: String,
    pub rule: UsernameRule,
}

impl std::fmt::Display for BadUsername {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rule = match self.rule {
            UsernameRule::Length => "3-to-16-characters",
            UsernameRule::Characters => "letters-digits-underscore",
        };
        write!(f, "{:?} breaks the {} rule", self.attempted, rule)
    }
}

impl std::error::Error for BadUsername {}

/// Which half of the name rule a claim broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernameRule {
    /// Fewer than 3 or more than 16 characters after trimming.
    Length,
    /// Something outside `[A-Za-z0-9_]`.
    Characters,
}

/// A canonicalised username.
///
/// Display case is preserved; matching goes through
/// [`as_key`](Self::as_key), which is the point of a type instead of a bare
/// `String`: two names are the same player only when their keys agree, and a
/// bare string invites an `==` that thinks `Steve` and `steve` differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Username {
    display: String,
    key: String,
}

impl Username {
    /// The lowercase comparison form.
    pub fn as_key(&self) -> &str {
        &self.key
    }

    /// The display form: as typed, minus surrounding whitespace.
    pub fn as_str(&self) -> &str {
        &self.display
    }
}

/// Apply the name rule. See the policy section in the module docs.
pub fn canonical_username(raw: &str) -> Result<Username, BadUsername> {
    let display = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if !(3..=16).contains(&display.chars().count()) {
        return Err(BadUsername {
            attempted: raw.to_owned(),
            rule: UsernameRule::Length,
        });
    }
    if !display
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(BadUsername {
            attempted: raw.to_owned(),
            rule: UsernameRule::Characters,
        });
    }
    Ok(Username {
        display: display.to_owned(),
        key: display.to_ascii_lowercase(),
    })
}

/// Derive the offline-mode profile id: UUID version 3 over
/// `"OfflinePlayer:" + name`, MD5-based, exactly as vanilla's
/// `nameUUIDFromBytes` computes it. The version and variant nibbles are set
/// per RFC 4122 because Java's method sets them, not because anything here
/// reads them back.
///
/// # The name is the **display** form, and this took a real server to notice
///
/// Vanilla hashes the name as the client typed it. Case matters:
/// `OfflinePlayer:Tester` and `OfflinePlayer:tester` are different strings and
/// therefore different players, with different inventories and different
/// entries in every permission system keyed on a uuid.
///
/// This function used to take a `&str` and its one caller passed
/// [`Username::as_key`] — the *lowercase* comparison form — because that is the
/// form the type steers callers towards, deliberately, for matching. So every
/// offline player on Dust got a different id from the one they have on every
/// other offline server, and nothing in either crate could tell:
///
/// ```text
/// vanilla 1.21.1, name "Tester"  ->  f3d28cb0-7225-3cb1-baeb-2dadd2be89ae
/// dust before this fix           ->  dd823a0c-b94a-369f-acd6-ddd287e3180e   (= "tester")
/// ```
///
/// It takes a [`Username`] now rather than a string, so the caller cannot pick
/// a form at all. A guard against a mistake somebody already made, placed where
/// the mistake was available.
pub fn offline_profile_id(username: &Username) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(b"OfflinePlayer:");
    hasher.update(username.as_str().as_bytes());
    let mut digest: [u8; 16] = hasher.finalize().into();
    digest[6] = (digest[6] & 0x0F) | 0x30; // version 3
    digest[8] = (digest[8] & 0x3F) | 0x80; // IETF variant
    digest
}

/// Where the conversation stands.
///
/// Public because it is observable through
/// [`LoginHandler::phase`](LoginHandler::phase): mid-flow inspection is how
/// the ordering guarantees are tested without peeking inside frames. It does
/// not accept transitions from outside — driving is what
/// [`authenticate`](LoginHandler::authenticate) is for, and a phase another
/// caller could mutate would be a state machine in name only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for Login Start.
    ExpectStart,
    /// Encryption Request sent; waiting for Encryption Response.
    ExpectResponse,
    /// The response decrypted cleanly; waiting on Mojang's answer.
    Validating,
    /// Identity settled; waiting for Login Acknowledged.
    ExpectAck,
}

/// Drives one connection through one login attempt.
///
/// Constructed with the connection, the configuration, a session server and
/// — for online mode — the server's key pair. Consumed by
/// [`authenticate`](LoginHandler::authenticate): a login happens once per
/// handler, and a type that could be re-run would quietly invite replaying
/// half a conversation into the other half.
pub struct LoginHandler<'a, W, S>
where
    W: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    S: SessionServer,
{
    conn: &'a mut Conn<W>,
    config: LoginConfig,
    session: &'a S,
    server_key: Option<&'a ServerKey>,
    phase: Phase,
}

// Manual because the session server and the connection are not `Debug`, and
// neither belongs in a log line.
impl<W, S> std::fmt::Debug for LoginHandler<'_, W, S>
where
    W: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    S: SessionServer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginHandler")
            .field("phase", &self.phase)
            .field("mode", &self.config.mode)
            .field("server_key", &self.server_key.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a, W, S> LoginHandler<'a, W, S>
where
    W: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    S: SessionServer,
{
    /// Assemble a handler over an existing driver.
    ///
    /// The connection should have consumed nothing but the handshake; the
    /// login conversation starts from the next frame. `server_key` is
    /// required by online mode and ignored by offline mode — supplied as an
    /// option rather than split into two constructors so callers can build
    /// one code path and switch modes from configuration.
    pub fn new(
        conn: &'a mut Conn<W>,
        config: LoginConfig,
        session: &'a S,
        server_key: Option<&'a ServerKey>,
    ) -> Self {
        Self {
            conn,
            config,
            session,
            server_key,
            phase: Phase::ExpectStart,
        }
    }

    /// Where the conversation stood when last observed. See [`Phase`].
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Run the whole conversation: Start → (encryption, Mojang) → Success →
    /// Acknowledged, ending either authenticated or with the reason on the
    /// wire and in the error.
    pub async fn authenticate(mut self) -> Result<Authenticated, LoginError> {
        // No `?` anywhere before the rejection handler runs: every failure
        // this early owes the client a Disconnect, and a bare `?` would
        // return past it.
        let start = match self.expect_start().await {
            Ok(start) => start,
            Err(error) => return self.reject_and_fail(error).await,
        };
        let outcome = match self.config.mode {
            AuthMode::Offline => {
                let profile_id = offline_profile_id(&start.username);
                self.finish_offline(&start, profile_id).await
            }
            AuthMode::Online => self.finish_online(&start).await,
        };
        match outcome {
            Ok(authenticated) => {
                self.await_acknowledged().await?;
                self.conn
                    .transition(State::Configuration)
                    .map_err(|error| LoginError::UnexpectedFrame {
                        reason: error.to_string(),
                    })?;
                Ok(authenticated)
            }
            Err(error) => return self.reject_and_fail(error).await,
        }
    }

    // -- Steps --------------------------------------------------------------

    async fn expect_start(&mut self) -> Result<StartOfLogin, LoginError> {
        debug_assert_eq!(self.phase, Phase::ExpectStart);
        let frame = self.next_frame("Login Start").await?;
        if frame.id != LOGIN_START_ID {
            return Err(unexpected(LOGIN_START_ID, &frame));
        }
        let (name_raw, used) =
            read_wire_string(&frame.body).ok_or_else(|| bad_body(LOGIN_START_ID, "name"))?;

        // The name is followed by a profile id: sixteen raw bytes, mandatory,
        // with no presence flag in front of them.
        //
        // This code used to accept a boolean-then-sixteen-bytes shape, and to
        // accept a bare name, and to refuse the sixteen raw bytes — which is
        // every case exactly inverted. That shape was real, in 1.20.2 through
        // 1.20.4; 1.20.5 made the id mandatory and unprefixed, and 1.21.1 is
        // on the far side of that change. `dust-protocol`'s definition of this
        // packet had it right the whole time and says so in a comment beside
        // the field; nothing tied the two together, so they disagreed in
        // silence.
        //
        // Confirmed against a running 1.21.1 server, all three shapes:
        //
        // ```text
        // name + 16 raw bytes      -> accepted, Set Compression follows
        // name + bool + 16 bytes   -> refused: "1 bytes extra"
        // name alone               -> refused: "Failed to decode packet"
        // ```
        //
        // The claimed id is never trusted — offline mode derives its own from
        // the name and online mode takes Mojang's — so it is checked for
        // length and dropped. Checked anyway, because a body that is the wrong
        // length is a client this server cannot talk to, and saying so now
        // beats desynchronising every packet after it.
        let trailing = frame.body.len() - used;
        if trailing != PROFILE_ID_BYTES {
            return Err(LoginError::UnexpectedFrame {
                reason: format!(
                    "Login Start carries {trailing} byte(s) after the name; since 1.20.5 it \
                     carries a {PROFILE_ID_BYTES}-byte profile id there, with no presence flag"
                ),
            });
        }

        let username = canonical_username(name_raw).map_err(LoginError::BadUsername)?;
        self.phase = match self.config.mode {
            AuthMode::Online => Phase::ExpectResponse,
            AuthMode::Offline => Phase::ExpectAck,
        };
        Ok(StartOfLogin { username })
    }

    async fn finish_offline(
        &mut self,
        start: &StartOfLogin,
        profile_id: [u8; 16],
    ) -> Result<Authenticated, LoginError> {
        debug_assert_eq!(self.phase, Phase::ExpectAck);
        self.announce_compression().await?;
        // Offline profiles carry the properties array like everyone else —
        // just empty. Omitting the count entirely would be a different,
        // wrong packet.
        self.send_login_success(profile_id, start.username.as_str(), EMPTY_PROPERTIES)
            .await?;
        Ok(Authenticated {
            profile_id,
            username: start.username.as_str().to_owned(),
            profile: None,
        })
    }

    async fn finish_online(&mut self, start: &StartOfLogin) -> Result<Authenticated, LoginError> {
        debug_assert_eq!(self.phase, Phase::ExpectResponse);
        let Some(server_key) = self.server_key else {
            return Err(LoginError::MissingServerKey);
        };

        // -- Encryption Request, plaintext ---------------------------------
        let token = VerifyToken::generate().map_err(LoginError::KeyExchange)?;
        let mut request_body = Vec::with_capacity(4 + server_key.public_key_der().len() + 8);
        push_wire_string(&mut request_body, ""); // server id: empty, as vanilla sends
        push_byte_array(&mut request_body, server_key.public_key_der());
        push_byte_array(&mut request_body, token.as_bytes());
        self.conn
            .send(Frame::new(ENCRYPTION_REQUEST_ID, request_body))
            .await
            .map_err(LoginError::Transport)?;

        // -- Encryption Response, still plaintext ---------------------------
        let frame = self.next_frame("Encryption Response").await?;
        if frame.id != ENCRYPTION_RESPONSE_ID {
            return Err(unexpected(ENCRYPTION_RESPONSE_ID, &frame));
        }
        let (secret_blob, used) = read_byte_array(&frame.body)
            .ok_or_else(|| bad_body(ENCRYPTION_RESPONSE_ID, "secret"))?;
        let (token_blob, _) = read_byte_array(&frame.body[used..])
            .ok_or_else(|| bad_body(ENCRYPTION_RESPONSE_ID, "verify token"))?;

        let secret = server_key
            .decrypt_shared_secret(secret_blob)
            .map_err(LoginError::KeyExchange)?;
        server_key
            .verify_token(token_blob, &token)
            .map_err(LoginError::KeyExchange)?;

        // -- Switch both modes, before anything further is said -------------
        //
        // The switch lands after the request already written and before
        // everything below: Set Compression will travel as ciphertext
        // announcing uncompressedness, and Success follows compressed.
        // That is vanilla's exact byte stream, and it falls out of queue
        // order rather than anyone remembering a flush.
        //
        // It happens *before* the session query for the same reason vanilla
        // enables here: the client turned its own ciphers on the moment it
        // sent the response, and every byte this side writes from now on —
        // including a rejection Disconnect — must be ciphertext or the
        // client reads noise. A token mismatch above is different: vanilla
        // simply drops such a connection rather than speak across a mode it
        // cannot match, which is why that arm skips Disconnect entirely.
        self.conn
            .enable_encryption(&secret)
            .await
            .map_err(LoginError::Transport)?;
        self.phase = Phase::Validating;

        // -- Mojang ----------------------------------------------------------
        let digest = server_id_hash("", &secret, server_key.public_key_der());
        let profile = self
            .session
            .has_joined(start.username.as_str(), &digest)
            .await
            .map_err(LoginError::Session)?
            .ok_or_else(|| LoginError::Unverified {
                username: start.username.as_str().to_owned(),
            })?;

        self.phase = Phase::ExpectAck;

        self.announce_compression().await?;

        let authoritative_name = profile.name.clone();
        let properties = encode_properties(&profile);
        let profile_id = parse_profile_id(&profile)?;
        self.send_login_success(profile_id, &authoritative_name, &properties)
            .await?;

        Ok(Authenticated {
            profile_id,
            username: authoritative_name,
            profile: Some(profile),
        })
    }

    async fn await_acknowledged(&mut self) -> Result<(), LoginError> {
        debug_assert_eq!(self.phase, Phase::ExpectAck);
        let frame = self.next_frame("Login Acknowledged").await?;
        if frame.id != LOGIN_ACKNOWLEDGED_ID {
            return Err(unexpected(LOGIN_ACKNOWLEDGED_ID, &frame));
        }
        if !frame.body.is_empty() {
            return Err(bad_body(
                LOGIN_ACKNOWLEDGED_ID,
                "an acknowledgement carries no body",
            ));
        }
        Ok(())
    }

    async fn announce_compression(&mut self) -> Result<(), LoginError> {
        let Some(threshold) = self.config.compression_threshold else {
            return Ok(());
        };
        if threshold < 0 {
            return Ok(());
        }
        let mut body = Vec::with_capacity(5);
        write_var_int(threshold, &mut body);
        // Announce before applying: the frame was encoded — and frozen
        // uncompressed — when accepted, and the codec turns on for whatever
        // comes next. See `Conn::set_compression` for why this holds by
        // construction.
        self.conn
            .send(Frame::new(SET_COMPRESSION_ID, body))
            .await
            .map_err(LoginError::Transport)?;
        self.conn.set_compression(Compress::At {
            threshold: threshold as usize,
        });
        Ok(())
    }

    async fn send_login_success(
        &mut self,
        profile_id: [u8; 16],
        name: &str,
        encoded_properties: &[u8],
    ) -> Result<(), LoginError> {
        let mut body = Vec::with_capacity(16 + 5 + name.len() + encoded_properties.len());
        body.extend_from_slice(&profile_id); // UUID: two big-endian u64s
        push_wire_string(&mut body, name);
        // Already-encoded properties array, count included — see
        // `encode_properties`, whose output this is.
        body.extend_from_slice(encoded_properties);
        self.conn
            .send(Frame::new(LOGIN_SUCCESS_ID, body))
            .await
            .map_err(LoginError::Transport)
    }

    /// Reject, then fail: the only exit from `authenticate` that carries an
    /// error, so no caller-visible failure can skip telling the peer.
    async fn reject_and_fail(mut self, error: LoginError) -> Result<Authenticated, LoginError> {
        self.reject(&error).await;
        Err(error)
    }

    /// Tell the client why, unless the transport is already gone.
    async fn reject(&mut self, error: &LoginError) {
        let reason = match error {
            // Nobody is listening, or the clocks already ended the
            // conversation; sending would only surface a second error.
            LoginError::Transport(_) => return,
            // Nobody is left who can read it: the client switched ciphers on
            // sending its response, and this side never did. Vanilla drops
            // here too.
            LoginError::KeyExchange(_) => return,
            LoginError::UnexpectedFrame { .. } => {
                "{\"translate\":\"multiplayer.disconnect.unexpected_query_response\"}"
            }
            LoginError::BadUsername(_) => "{\"text\":\"Invalid username\"}",
            LoginError::Session(_) => "{\"translate\":\"multiplayer.disconnect.authservers_down\"}",
            LoginError::Unverified { .. } => "{\"text\":\"Failed to verify username\"}",
            LoginError::MissingServerKey => "{\"text\":\"Server authentication error\"}",
        };
        let mut body = Vec::with_capacity(reason.len() + 5);
        push_wire_string(&mut body, reason);
        let _ = self.conn.send(Frame::new(LOGIN_DISCONNECT_ID, body)).await;
    }

    async fn next_frame(&mut self, expecting: &'static str) -> Result<Frame, LoginError> {
        match self.conn.next_frame().await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(LoginError::UnexpectedFrame {
                reason: format!("the peer hung up instead of sending {expecting}"),
            }),
            Err(error) => Err(LoginError::Transport(error)),
        }
    }
}

struct StartOfLogin {
    username: Username,
}

fn unexpected(expected: i32, got: &Frame) -> LoginError {
    LoginError::UnexpectedFrame {
        reason: format!(
            "this step needed packet id {expected:#04x} and got {:#04x} with {} body byte(s)",
            got.id,
            got.body.len()
        ),
    }
}

fn bad_body(id: i32, what: &str) -> LoginError {
    LoginError::UnexpectedFrame {
        reason: format!("packet {id:#04x}'s {what} field does not parse"),
    }
}

// -- Wire shapes local to the login phase -----------------------------------
//
// Strings are VarInt-prefixed UTF-8; byte arrays are VarInt-prefixed raw
// bytes. These differ from the u16-length strings elsewhere in the protocol,
// which is precisely the kind of fact that belongs next to its users.

fn push_wire_string(out: &mut Vec<u8>, text: &str) {
    write_var_int(text.len() as i32, out);
    out.extend_from_slice(text.as_bytes());
}

fn read_wire_string(input: &[u8]) -> Option<(&str, usize)> {
    let (length, used) = read_var_int(input).ok()?;
    let length = usize::try_from(length).ok()?;
    let end = used.checked_add(length)?;
    if end > input.len() {
        return None;
    }
    let text = std::str::from_utf8(&input[used..end]).ok()?;
    Some((text, end))
}

fn push_byte_array(out: &mut Vec<u8>, bytes: &[u8]) {
    write_var_int(bytes.len() as i32, out);
    out.extend_from_slice(bytes);
}

fn read_byte_array(input: &[u8]) -> Option<(&[u8], usize)> {
    let (length, used) = read_var_int(input).ok()?;
    let length = usize::try_from(length).ok()?;
    let end = used.checked_add(length)?;
    if end > input.len() {
        return None;
    }
    Some((&input[used..end], end))
}

/// Properties array of Login Success: count, then triplets with an optional
/// signature each. Serialized from Mojang's answer unchanged — signing is
/// their claim to check, not ours to re-litigate.
/// The encoded shape of an empty properties array: the count, and nothing
/// after it.
const EMPTY_PROPERTIES: &[u8] = &[0];

fn encode_properties(profile: &Profile) -> Vec<u8> {
    let mut out = Vec::new();
    write_var_int(profile.properties.len() as i32, &mut out);
    for property in &profile.properties {
        push_wire_string(&mut out, &property.name);
        push_wire_string(&mut out, &property.value);
        match &property.signature {
            Some(signature) => {
                out.push(1);
                push_wire_string(&mut out, signature);
            }
            None => out.push(0),
        }
    }
    out
}

fn parse_profile_id(profile: &Profile) -> Result<[u8; 16], LoginError> {
    hex_16_bytes(profile.id.as_str()).ok_or_else(|| LoginError::UnexpectedFrame {
        reason: format!(
            "the session server answered with id {:?}, which is not 32 hex digits",
            profile.id.as_str()
        ),
    })
}

fn hex_16_bytes(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (index, pair) in bytes.chunks(2).enumerate() {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out[index] = ((high << 4) | low) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_pass_through_trimmed_and_cased() {
        let name = canonical_username("  Steve ").expect("legal");
        assert_eq!(name.as_str(), "Steve", "display keeps case");
        assert_eq!(name.as_key(), "steve", "matching is case-insensitive");
        assert_eq!(canonical_username("abc"), canonical_username("abc"));
        assert_ne!(
            canonical_username("Steve").expect("legal").as_key(),
            "not steve"
        );
    }

    #[test]
    fn names_breaking_each_rule_are_refused_by_that_rule() {
        assert_eq!(
            canonical_username("ab"),
            Err(BadUsername {
                attempted: "ab".to_owned(),
                rule: UsernameRule::Length
            })
        );
        // Sixteen is legal, seventeen is not — the boundary, not just the idea.
        assert!(canonical_username("abcdefghijklmnop").is_ok());
        assert_eq!(
            canonical_username("abcdefghijklnmopq").unwrap_err().rule,
            UsernameRule::Length
        );
        // Whitespace does not rescue an otherwise-empty claim.
        assert_eq!(
            canonical_username("   ").unwrap_err().rule,
            UsernameRule::Length
        );
        for hostile in ["no!", "sp ace", "\u{00e9}clair", "semi;colon"] {
            assert_eq!(
                canonical_username(hostile).unwrap_err().rule,
                UsernameRule::Characters,
                "{hostile}"
            );
        }
    }

    #[test]
    fn the_offline_id_matches_an_independently_computed_vector() {
        // Computed with Python's hashlib plus the RFC 4122 nibble rules —
        // not with this function — so a wrong MD5, a missing version nibble
        // or a swapped variant all fail here. Vanilla parity means matching
        // Java's `nameUUIDFromBytes`, whose rules these are.
        let name = |raw: &str| canonical_username(raw).expect("legal");
        assert_eq!(
            offline_profile_id(&name("notch")),
            [
                0x42, 0x65, 0x30, 0x81, 0xa9, 0x0e, 0x34, 0x75, 0xb3, 0xd6, 0x35, 0x50, 0xcd, 0xb4,
                0x3f, 0x8e
            ]
        );
        assert_eq!(
            offline_profile_id(&name("steve")),
            [
                0x53, 0x90, 0x99, 0x32, 0xf7, 0x94, 0x33, 0xc0, 0x93, 0x29, 0x94, 0x80, 0x45, 0xa4,
                0xc1, 0xce
            ]
        );
    }

    #[test]
    fn display_case_forks_the_offline_identity_because_it_does_in_vanilla() {
        // The test that used to sit here asserted the opposite — that the
        // comparison form is hashed, so `Steve` and `steve` are one player.
        // That is a reasonable thing to want and it is not what Minecraft
        // does, which makes it the wrong thing to implement: an offline player
        // whose id differs from every other server's has a different
        // inventory, a different position and a different row in every
        // permission plugin.
        //
        // The vector below was read off the wire of a running 1.21.1 server in
        // offline mode, logging in as "Tester" — not computed here, and not
        // computed by the same code being tested.
        let tester = canonical_username("Tester").expect("legal");
        assert_eq!(
            offline_profile_id(&tester),
            [
                0xf3, 0xd2, 0x8c, 0xb0, 0x72, 0x25, 0x3c, 0xb1, 0xba, 0xeb, 0x2d, 0xad, 0xd2, 0xbe,
                0x89, 0xae
            ],
            "the id vanilla issues for the name \"Tester\""
        );

        let lower = canonical_username("tester").expect("legal");
        assert_ne!(
            offline_profile_id(&tester),
            offline_profile_id(&lower),
            "case is part of the name vanilla hashes, so it is part of the identity"
        );

        // Leading and trailing whitespace is still not: canonicalisation trims
        // before anything reaches the digest, so a name pasted with a space is
        // the same player rather than a new one.
        let padded = canonical_username("  Tester ").expect("legal");
        assert_eq!(offline_profile_id(&tester), offline_profile_id(&padded));
    }

    #[test]
    fn login_phase_strings_round_trip_through_the_varint_form() {
        let mut buffer = Vec::new();
        push_wire_string(&mut buffer, "steve");
        assert_eq!(buffer.len(), 6, "VarInt length plus payload");
        assert_eq!(read_wire_string(&buffer), Some(("steve", 6)));
        // A length reaching past the buffer is refused rather than sliced.
        buffer[0] = 0x7F; // claims 127 bytes
        assert_eq!(read_wire_string(&buffer), None);
    }

    #[test]
    fn byte_arrays_refuse_lengths_past_the_buffer() {
        let mut buffer = Vec::new();
        push_byte_array(&mut buffer, &[1, 2, 3]);
        assert_eq!(read_byte_array(&buffer), Some((&[1, 2, 3][..], 4)));
        buffer.truncate(2); // header says 3, body has none
        assert_eq!(read_byte_array(&buffer), None);
    }

    #[test]
    fn hex_ids_parse_and_rubbish_does_not() {
        let text = "853c80ef3c3749fdaa49938b674adae6";
        let parsed = hex_16_bytes(text).expect("hex");
        assert_eq!(parsed[..2], [0x85, 0x3c]);
        assert_eq!(hex_16_bytes("853C80EF"), None, "wrong length");
        assert_eq!(hex_16_bytes("zz3c80ef3c3749fdaa49938b674adae6"), None);
    }
}

//! The Mojang session server: the HTTP half of online-mode authentication.
//!
//! # Where this sits in the login story
//!
//! [`crate::login`] ends its account of a vanilla login at step 6: *the server
//! asks Mojang's session server whether the player is who they say they are.*
//! This module is step 6. A client that has completed the key exchange calls
//! `POST /session/minecraft/join` with its access token and the login digest;
//! the server then asks `GET /session/minecraft/hasJoined?username=…&serverId=…`
//! and accepts the login only when Mojang answers with a profile. The two
//! calls are two halves of one contract — the client tells Mojang which
//! server it is joining, and the server checks that Mojang was told about it.
//!
//! A stock Dust server only ever sends `hasJoined`; `join` is implemented here
//! because its parameter shape is the other half of that contract and because
//! launcher and tooling consumers of this crate speak the client side. Both
//! shapes are pinned by tests against recorded wire bytes, so neither drifts.
//!
//! # The seams, and why there are two
//!
//! [`SessionServer`] is the seam `dust-protocol` and the server consume:
//! join/hasJoined in protocol terms, no HTTP anywhere near it. Anything that
//! implements it can stand behind a login; tests inject a scripted fake and
//! never touch a network. [`HttpSessionServer`] is the production
//! implementation, generic over [`RawTransport`] — the second seam, which
//! moves finished request bytes to `sessionserver.mojang.com:443` and brings
//! the raw answer back. HTTP/1.1 is written by hand below rather than pulled
//! in as a framework, because the conversation is exactly two fixed requests,
//! and a dependency able to express arbitrary REST could also express an
//! arbitrary surprise.
//!
//! Real TLS lives behind the `tls` feature (off by default): the transport is
//! where the network enters, so it is the part tests must not require. See
//! [`TlsTransport`] for what the feature buys and what it still does not do.
//!
//! # Timeout policy
//!
//! Every call to Mojang is bounded twice. Locally, a transport applies its own
//! deadlines — [`TlsTransport`] caps connect plus exchange at ten seconds of
//! wall clock. Structurally, the whole call happens inside the login phase,
//! where [`crate::io::Timeouts::pre_auth_budget`] already bounds how long any
//! unauthenticated connection may live at all; a session server that hangs
//! cannot hold a connection past that budget no matter what the transport
//! does. There are deliberately no retries. `hasJoined` failing once fails the
//! login; the client re-logs-in, which is the retry, and automatic retries
//! from the server would multiply load on a third-party service during exactly
//! the outages when we least want to.

use std::future::Future;
use std::time::Duration;

/// The host both endpoints live on.
pub const SESSION_HOST: &str = "sessionserver.mojang.com";

/// The port both endpoints live on. HTTPS only; there is no plaintext
/// variant, for the reason under [`TlsTransport`].
pub const SESSION_PORT: u16 = 443;

/// The largest response body accepted, in bytes.
///
/// A real `hasJoined` answer is well under four kilobytes even with signed
/// textures attached. Sixty-four kilobytes leaves room for every future the
/// API plausibly has while keeping a hostile peer's answer bounded before
/// parsing — the same "bound it before you read it" rule the frame decoder
/// applies, applied to HTTP instead of Minecraft.
pub const MAX_RESPONSE_BODY: usize = 64 * 1024;

/// How long a production transport may spend on one call, wall clock, connect
/// to final byte. See the module docs for why this is not the only bound.
pub const CALL_BUDGET: Duration = Duration::from_secs(10);

/// A Mojang profile id: the UUID as 32 lowercase hex digits, no dashes.
///
/// That rendering is what the session server speaks on the wire — both as the
/// `selectedProfile` value sent to `join` and as the `id` field of a
/// `hasJoined` answer — and it is kept verbatim rather than parsed into a
/// general-purpose UUID type, because nothing here computes with it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileId(String);

impl ProfileId {
    /// Accept only what the wire can carry: exactly 32 lowercase hex digits.
    ///
    /// Dashed forms, uppercase and non-hex text are refused rather than
    /// normalised. Normalising would silently accept a caller bug — passing a
    /// display form where a wire form belongs — and send it to Mojang to be
    /// rejected later, far from the mistake.
    pub fn parse(text: &str) -> Result<Self, ProfileError> {
        if text.len() != 32 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(ProfileError);
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a profile id was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileError;

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a profile id is 32 lowercase hex digits, without dashes")
    }
}

impl std::error::Error for ProfileError {}

/// One signed or unsigned property from a profile, passed through untouched.
///
/// The values — most often the base64 textures blob and its signature — mean
/// nothing to the transport layer. They belong to whoever renders a skin or
/// verifies a signature, both of which are play-phase concerns with their own
/// trust questions; parsing them here would put this module in the business
/// of answers it cannot check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    /// Present when Mojang signed the property. Absence is normal for
    /// unsigned properties and not an error.
    pub signature: Option<String>,
}

/// Who the session server says the player is.
///
/// The name here is authoritative: whatever case the client typed into Login
/// Start, Mojang's spelling of the account is the one a server should log and
/// display. See [`crate::login_flow`] for how that interacts with offline
/// mode, where nobody authoritative exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub properties: Vec<ProfileProperty>,
}

/// What asking the session server can go wrong.
#[derive(Debug)]
pub enum SessionError {
    /// The exchange never produced an answer: connection failed, TLS failed,
    /// or [`CALL_BUDGET`] elapsed. Whether the fault is ours, theirs or the
    /// network's is unknowable from here, and the wording keeps it that way.
    Transport { reason: String },
    /// The server answered and said no — any 4xx. On `join` this is usually
    /// an invalid or expired access token; on `hasJoined`, a username/server
    /// pair Mojang does not recognise.
    Rejected { status: u16 },
    /// The server answered and is broken — any 5xx. Retrying is somebody
    /// else's decision; see the timeout policy in the module docs for why it
    /// is not done automatically here.
    Unavailable { status: u16 },
    /// An answer arrived and is not what the contract promised: unparseable
    /// status line, framing this client does not speak, an oversized body, or
    /// JSON that does not describe a profile.
    Malformed { reason: String },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport { reason } => {
                write!(f, "the session server exchange failed: {reason}")
            }
            Self::Rejected { status } => {
                write!(f, "the session server refused the request ({status})")
            }
            Self::Unavailable { status } => write!(
                f,
                "the session server answered with {status}; the service, not this \
                 request, is what failed"
            ),
            Self::Malformed { reason } => {
                write!(f, "the session server's answer made no sense: {reason}")
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// The parameters of `POST /session/minecraft/join`.
///
/// Borrowed rather than owned because all three fields live just long enough
/// to be serialised: they are produced at the end of a key exchange and
/// consumed by one HTTP call, and copying them would only blur who owns them
/// between those two points.
#[derive(Debug, Clone, Copy)]
pub struct JoinRequest<'a> {
    /// The player's session token, obtained from their launcher. This is a
    /// credential; see the TLS note under [`TlsTransport`] for why it moves
    /// over HTTPS only.
    pub access_token: &'a str,
    /// The id of the profile being played.
    pub profile_id: &'a ProfileId,
    /// The login digest — [`crate::login::server_id_hash`]'s output — binding this
    /// session to one server's RSA key pair.
    pub server_id_hash: &'a str,
}

/// The question the rest of the codebase asks, in protocol terms.
///
/// Object safety is not provided on purpose: implementations are chosen at
/// compile time (a scripted fake in tests, [`HttpSessionServer`] or nothing
/// in production), and a `dyn SessionServer` would buy nothing but an
/// indirection nobody needs. Native `async fn` in traits keeps the dependency
/// count at zero.
pub trait SessionServer: Send + Sync {
    /// The client-side half: tell Mojang this player is joining this server.
    ///
    /// `Ok(())` means the join was recorded. It proves the access token was
    /// live and the digest was formed; it proves nothing about the server
    /// side, which is what [`has_joined`](Self::has_joined) is for.
    fn join(
        &self,
        request: JoinRequest<'_>,
    ) -> impl Future<Output = Result<(), SessionError>> + Send;

    /// The server-side half: ask whether `username` joined a server whose
    /// login digest is `server_id_hash`.
    ///
    /// `Ok(None)` is Mojang answering "no such join" — the normal shape of an
    /// impostor, and distinct from [`SessionError::Rejected`], which means
    /// the request itself was malformed. A login proceeds only on
    /// `Ok(Some(_))`.
    fn has_joined(
        &self,
        username: &str,
        server_id_hash: &str,
    ) -> impl Future<Output = Result<Option<Profile>, SessionError>> + Send;
}

/// Moves finished request bytes to the session server and brings the raw
/// answer back.
///
/// The narrowest useful seam: one method, bytes in, bytes out, no HTTP
/// vocabulary. That is what makes recorded-fixture tests exact — a fixture
/// *is* these bytes — and what lets the TLS implementation stay a detail of
/// one type instead of a concern threaded through the client.
pub trait RawTransport: Send + Sync {
    fn exchange(
        &self,
        request: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, SessionError>> + Send;
}

/// The production client: hand-rolled HTTP/1.1 over anything that carries
/// bytes to Mojang and back.
///
/// One instance per server is typical; the transport decides what sharing it
/// costs. Nothing here is stateful between calls beyond what the transport
/// holds.
#[derive(Debug)]
pub struct HttpSessionServer<T: RawTransport> {
    host: &'static str,
    port: u16,
    transport: T,
}

impl<T: RawTransport> HttpSessionServer<T> {
    /// A client pointed at the real Mojang endpoint.
    pub fn new(transport: T) -> Self {
        Self {
            host: SESSION_HOST,
            port: SESSION_PORT,
            transport,
        }
    }

    /// A client pointed somewhere else — for tests, proxies and future
    /// endpoint moves. The path prefix is unchanged; only the authority
    /// moves.
    pub fn with_endpoint(host: &'static str, port: u16, transport: T) -> Self {
        Self {
            host,
            port,
            transport,
        }
    }

    async fn exchange(&self, request: &[u8]) -> Result<Response, SessionError> {
        let raw = self.transport.exchange(request).await?;
        Response::parse(&raw)
    }
}

impl<T: RawTransport> SessionServer for HttpSessionServer<T> {
    async fn join(&self, request: JoinRequest<'_>) -> Result<(), SessionError> {
        // Key order matches what first-party launchers send. JSON objects are
        // unordered on paper; pinning the order anyway makes the wire bytes
        // deterministic, which is what the recorded fixtures pin.
        let mut body = String::from("{\"accessToken\":");
        push_json_string(&mut body, request.access_token);
        body.push_str(",\"selectedProfile\":");
        push_json_string(&mut body, request.profile_id.as_str());
        body.push_str(",\"serverId\":");
        push_json_string(&mut body, request.server_id_hash);
        body.push('}');

        let mut wire = Vec::with_capacity(192 + body.len());
        push_request_line(
            &mut wire,
            "POST",
            "/session/minecraft/join",
            self.host,
            self.port,
        );
        push_header(&mut wire, "Content-Type", "application/json");
        push_header(&mut wire, "Content-Length", &body.len().to_string());
        finish_headers(&mut wire);
        wire.extend_from_slice(body.as_bytes());

        let response = self.exchange(&wire).await?;
        match response.status {
            // 204 No Content is the documented success. Other 2xx codes would
            // mean the contract moved; accepting them silently would hide
            // that from the day it happens.
            204 => Ok(()),
            400..=499 => Err(SessionError::Rejected {
                status: response.status,
            }),
            500..=599 => Err(SessionError::Unavailable {
                status: response.status,
            }),
            other => Err(SessionError::Malformed {
                reason: format!(
                    "join answered with status {other}, which the contract has no meaning for"
                ),
            }),
        }
    }

    async fn has_joined(
        &self,
        username: &str,
        server_id_hash: &str,
    ) -> Result<Option<Profile>, SessionError> {
        let mut target = String::from("/session/minecraft/hasJoined?username=");
        percent_encode(&mut target, username.as_bytes());
        target.push_str("&serverId=");
        percent_encode(&mut target, server_id_hash.as_bytes());

        let mut wire = Vec::with_capacity(256 + target.len());
        push_request_line(&mut wire, "GET", &target, self.host, self.port);
        finish_headers(&mut wire);

        let response = self.exchange(&wire).await?;
        match response.status {
            // 204 No Content: nobody by that name joined that server. This is
            // an answer, not a failure — the everyday form of an impostor —
            // so it is `Ok(None)` rather than an error.
            204 => Ok(None),
            200 => parse_profile(&response.body).map(Some),
            400..=499 => Err(SessionError::Rejected {
                status: response.status,
            }),
            500..=599 => Err(SessionError::Unavailable {
                status: response.status,
            }),
            other => Err(SessionError::Malformed {
                reason: format!("hasJoined answered with status {other}"),
            }),
        }
    }
}

/// A parsed HTTP/1.1 response, as far as this client cares.
struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    /// Parse the recorded wire bytes, refusing everything the contract does
    /// not promise.
    ///
    /// Refusals are deliberate per clause: HTTP/0.9 body-only replies cannot
    /// carry a status, so trusting them would let a captive portal pass;
    /// chunked framing exists to serve streaming, which a two-kilobyte JSON
    /// answer is not; and a missing length with no close semantics is a
    /// hang waiting to be called a timeout.
    fn parse(raw: &[u8]) -> Result<Self, SessionError> {
        const SEPARATOR: &[u8] = b"\r\n\r\n";
        let header_end = raw
            .windows(SEPARATOR.len())
            .position(|window| window == SEPARATOR)
            .ok_or_else(|| SessionError::Malformed {
                reason: "no header terminator in the answer".to_owned(),
            })?;

        let headers =
            std::str::from_utf8(&raw[..header_end]).map_err(|_| SessionError::Malformed {
                reason: "the answer's headers are not ASCII".to_owned(),
            })?;

        let mut lines = headers.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        if !(status_line.starts_with("HTTP/1.0 ") || status_line.starts_with("HTTP/1.1 ")) {
            return Err(SessionError::Malformed {
                reason: format!("unrecognised status line {status_line:?}"),
            });
        }
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .filter(|code| (100..600).contains(code))
            .ok_or_else(|| SessionError::Malformed {
                reason: format!("status line {status_line:?} carries no three-digit status"),
            })?;

        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => {
                    let parsed = value
                        .parse::<usize>()
                        .map_err(|_| SessionError::Malformed {
                            reason: format!("content-length {value:?} is not a number"),
                        })?;
                    // Two lengths that disagree mean a smuggling-shaped
                    // answer, even if this client reads only to EOF. Refuse
                    // rather than pick.
                    if let Some(existing) = content_length {
                        if existing != parsed {
                            return Err(SessionError::Malformed {
                                reason: "conflicting content-length headers".to_owned(),
                            });
                        }
                    }
                    content_length = Some(parsed);
                }
                "transfer-encoding" if value.eq_ignore_ascii_case("chunked") => {
                    chunked = true;
                }
                _ => {}
            }
        }
        if chunked {
            return Err(SessionError::Malformed {
                reason: "chunked transfer encoding, which this fixed-shape client does not speak"
                    .to_owned(),
            });
        }

        let body = &raw[header_end + SEPARATOR.len()..];
        let body = match content_length {
            Some(length) => {
                if length > MAX_RESPONSE_BODY {
                    return Err(SessionError::Malformed {
                        reason: format!(
                            "the answer declares {length} body bytes; the ceiling is {MAX_RESPONSE_BODY}"
                        ),
                    });
                }
                if length > body.len() {
                    return Err(SessionError::Malformed {
                        reason: format!(
                            "the answer declares {length} body bytes and stops after {}",
                            body.len()
                        ),
                    });
                }
                &body[..length]
            }
            // No length declared: the close defines the end, per HTTP/1.1.
            // The cap applies here too — the transport handed us what it
            // read, and reading more than the ceiling is already a refusal.
            None if body.len() <= MAX_RESPONSE_BODY => body,
            None => {
                return Err(SessionError::Malformed {
                    reason: format!(
                        "an answer without content-length ran past {MAX_RESPONSE_BODY} bytes"
                    ),
                })
            }
        };

        Ok(Self {
            status,
            body: body.to_vec(),
        })
    }
}

/// Parse a `hasJoined` 200 body into a profile.
///
/// Navigates `serde_json::Value` directly rather than deriving structs: the
/// document is small, the shape is checked field by field with named errors,
/// and unknown fields are skipped exactly as forward compatibility wants.
fn parse_profile(body: &[u8]) -> Result<Profile, SessionError> {
    let malformed = |reason: &str| SessionError::Malformed {
        reason: format!("{reason} in the hasJoined answer"),
    };

    let document: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| malformed(&error.to_string()))?;

    let id = document
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("no string \"id\""))?;
    let id = ProfileId::parse(id).map_err(|_| {
        malformed(&format!(
            "\"id\" is {id:?}, which is not 32 lowercase hex digits"
        ))
    })?;

    let name = document
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("no string \"name\""))?
        .to_owned();

    let properties = match document.get("properties") {
        // Absent or null: an unsigned profile. Normal.
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(entries)) => entries
            .iter()
            .map(|entry| {
                let name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| malformed("a property without a string \"name\""))?;
                let value = entry
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| malformed("a property without a string \"value\""))?;
                Ok(ProfileProperty {
                    name: name.to_owned(),
                    value: value.to_owned(),
                    signature: entry
                        .get("signature")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(malformed("\"properties\" is neither absent nor an array")),
    };

    Ok(Profile {
        id,
        name,
        properties,
    })
}

// -- Wire formatting ---------------------------------------------------------

fn push_request_line(wire: &mut Vec<u8>, method: &str, target: &str, host: &str, port: u16) {
    wire.extend_from_slice(method.as_bytes());
    wire.push(b' ');
    wire.extend_from_slice(target.as_bytes());
    wire.extend_from_slice(b" HTTP/1.1\r\n");
    // The default HTTPS port is omitted from the Host header, matching what
    // browsers and launchers send; a non-default port is spelled out.
    if port == 443 {
        push_header(wire, "Host", host);
    } else {
        let authority = format!("{host}:{port}");
        push_header(wire, "Host", &authority);
    }
    // One request per connection: the transport closes after each exchange,
    // which is what makes "read to EOF" a valid body terminator and keeps
    // keep-alive state out of a type that does not need it.
    push_header(wire, "Connection", "close");
}

fn push_header(wire: &mut Vec<u8>, name: &str, value: &str) {
    wire.extend_from_slice(name.as_bytes());
    wire.extend_from_slice(b": ");
    wire.extend_from_slice(value.as_bytes());
    wire.extend_from_slice(b"\r\n");
}

fn finish_headers(wire: &mut Vec<u8>) {
    wire.extend_from_slice(b"\r\n");
}

/// Append `text` as a JSON string literal, escaping what must be escaped.
///
/// Written by hand because the input is three fields this code chose the
/// shape of, and a JSON serialiser dependency for three strings is weight.
/// Escaping covers quotes, backslashes and the control range — the inputs
/// (`accessToken`, hex ids, SHA-1 digests) never contain them, but the
/// function refuses to assume that of its callers.
fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Percent-encode bytes for a query component: unreserved characters pass,
/// everything else becomes `%XX`.
///
/// Applied to the whole byte string rather than trying to know whether the
/// digest contains reserved characters. The digest is hex-or-minus-signed
/// hex today; encoding is correct for anything it might be tomorrow.
fn percent_encode(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
}

// -- The real transport ------------------------------------------------------

/// Speaks to Mojang over real TLS. Requires the `tls` feature; nothing in
/// the default build compiles it, which is what lets tests stay off the
/// network entirely.
///
/// # Why HTTPS only, and why the OS root store
///
/// The `join` request carries the player's access token, and `hasJoined`
/// carries the login digest that binds the session to this server's identity.
/// Either intercepted is either abused, so there is no plaintext variant of
/// this type to accidentally reach for. Roots come from the operating system
/// via `rustls-native-certs`: a bundled root list would make this crate the
/// maintainer of a CA set, which is a trust decision operators already own.
#[cfg(feature = "tls")]
pub struct TlsTransport {
    connector: tokio_rustls::TlsConnector,
    /// Fixed at construction so a proxying deployment can point elsewhere;
    /// verification always uses this name regardless of where packets go.
    server_name: rustls::pki_types::ServerName<'static>,
}

#[cfg(feature = "tls")]
impl std::fmt::Debug for TlsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual because the connector's config carries the whole trust
        // store, which is public material but far too much to print.
        f.debug_struct("TlsTransport")
            .field("host", &SESSION_HOST)
            .field("port", &SESSION_PORT)
            .finish()
    }
}

#[cfg(feature = "tls")]
impl TlsTransport {
    /// A transport for `sessionserver.mojang.com:443`, verified against the
    /// operating system's root certificates.
    ///
    /// Fails at construction if the OS yields no usable certificate store —
    /// better than failing on the first login, inside someone's startup
    /// window.
    pub fn mojang() -> Result<Self, SessionError> {
        // `load_native_certs` reports per-certificate problems without
        // failing wholesale: partial trust is still some trust, and a
        // machine whose store lists one malformed file can reach Mojang
        // perfectly well. Only total failure, or an empty result with
        // nothing usable, stops construction.
        let loaded = rustls_native_certs::load_native_certs();
        if !loaded.errors.is_empty() {
            return Err(SessionError::Transport {
                reason: format!(
                    "could not load the system root certificates: {:?}",
                    loaded.errors
                ),
            });
        }
        if loaded.certs.is_empty() {
            return Err(SessionError::Transport {
                reason: "the system yielded no root certificates".to_owned(),
            });
        }
        // Same partial-trust rule as the load itself: a certificate the
        // parser cannot take is skipped and counted, and only an empty
        // store stops construction.
        let mut roots = rustls::RootCertStore::empty();
        let mut unusable = loaded.errors.len();
        for certificate in &loaded.certs {
            if roots.add(certificate.clone()).is_err() {
                unusable += 1;
            }
        }
        if !loaded.errors.is_empty() || roots.is_empty() {
            return Err(SessionError::Transport {
                reason: format!(
                    "the system's root certificates were not usable ({unusable} rejected, {} kept)",
                    roots.len()
                ),
            });
        }
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        // ALPN is left unset: Mojang serves plain HTTP/1.1 here, and h2
        // negotiation would change the framing this client writes.
        config.alpn_protocols = Vec::new();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
        let server_name = rustls::pki_types::ServerName::try_from(SESSION_HOST.to_owned())
            .map_err(|error| SessionError::Transport {
                reason: format!("{SESSION_HOST} is not a valid TLS server name: {error}"),
            })?
            .to_owned();
        Ok(Self {
            connector,
            server_name,
        })
    }
}

#[cfg(feature = "tls")]
impl RawTransport for TlsTransport {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, SessionError> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let attempt = async {
            let address = format!("{SESSION_HOST}:{SESSION_PORT}");
            let tcp = tokio::net::TcpStream::connect(address)
                .await
                .map_err(|error| SessionError::Transport {
                    reason: format!("connect failed: {error}"),
                })?;
            let mut tls = self
                .connector
                .connect(self.server_name.clone(), tcp)
                .await
                .map_err(|error| SessionError::Transport {
                    reason: format!("the TLS handshake failed: {error}"),
                })?;
            tls.write_all(request)
                .await
                .map_err(|error| SessionError::Transport {
                    reason: format!("the request did not leave: {error}"),
                })?;
            tls.flush().await.map_err(|error| SessionError::Transport {
                reason: format!("the request did not leave: {error}"),
            })?;

            let mut answer = Vec::new();
            // The request says `Connection: close`, so EOF terminates the
            // body. The ceiling bounds the read; the parser re-checks the
            // declared length against it afterwards.
            tls.take(MAX_RESPONSE_BODY as u64 + 1)
                .read_to_end(&mut answer)
                .await
                .map_err(|error| SessionError::Transport {
                    reason: format!("the answer was cut short: {error}"),
                })?;
            if answer.len() > MAX_RESPONSE_BODY {
                return Err(SessionError::Transport {
                    reason: format!(
                        "the answer exceeded {MAX_RESPONSE_BODY} bytes and was stopped"
                    ),
                });
            }
            Ok(answer)
        };

        tokio::time::timeout(CALL_BUDGET, attempt)
            .await
            .unwrap_or_else(|_| {
                Err(SessionError::Transport {
                    reason: format!("the exchange spent more than {CALL_BUDGET:?}"),
                })
            })
    }
}

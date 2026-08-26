//! The connection state machine, and the seam it defines with
//! `dust-protocol`.
//!
//! # Why this layer owns the state
//!
//! A packet id is meaningless on its own. Serverbound `0x00` is Handshake in
//! [`State::Handshaking`], Status Request in [`State::Status`], Login Start in
//! [`State::Login`], Client Information in [`State::Configuration`] and
//! Confirm Teleportation in [`State::Play`]. Five packets, one number. The
//! thing that resolves the ambiguity is the connection's state, and the
//! connection's state is a property of the *transport* — it changes because of
//! a handshake field, a login acknowledgement, a compression packet — so it
//! lives here rather than in the layer that knows what packets mean.
//!
//! # The seam
//!
//! `dust-net` hands up a [`crate::frame::Frame`] and a [`PacketContext`]. The
//! frame is an id and opaque bytes; the context says which of `dust-protocol`'s
//! tables that id is to be read in. `dust-net` never looks an id up, and
//! `dust-protocol` never reads a length prefix. That division is what lets the
//! two be built at the same time by people who do not have each other's code:
//! nothing in this crate needs to know that 1.21.1's protocol number is 767 or
//! that Login Success is `0x02`.
//!
//! Transitions are *driven from above*, because the events that cause them are
//! packet contents and this crate cannot read packet contents. The handshake's
//! next-state field, the client's Login Acknowledged, the server's Finish
//! Configuration — all of them are parsed by `dust-protocol` and turned into a
//! [`Connection::transition`] call. What this layer guarantees is that the call
//! is *checked*: an illegal transition is an error naming both states, not a
//! connection that quietly starts reading the wrong table.
//!
//! # Configuration is not a phase you pass through once
//!
//! [`State::Configuration`] arrived in 1.20.2 and sits between login and play,
//! and a connection can **go back to it from [`State::Play`]** — that is how a
//! server changes a client's resource pack or datapack set without a
//! reconnect. A state machine written as a forward-only pipeline is wrong on
//! 1.21.1, and wrong in a way that only shows up on a server that actually
//! uses the feature. The [`Play`] → [`Configuration`] edge is here, and
//! `reconfiguration_is_a_legal_round_trip` is the test that says so.
//!
//! [`Play`]: State::Play
//! [`Configuration`]: State::Configuration
//!
//! # What this does not catch
//!
//! It checks that a transition is *legal*, never that it is *warranted*. A
//! layer above that calls `transition(Configuration)` because it misparsed a
//! packet gets a perfectly legal state change into the wrong table. Nothing
//! here can tell the difference; only the caller knows why it asked.

/// Where a connection is in the protocol.
///
/// `Disconnected` is a state rather than an absence of one, so that "what
/// happened to that connection" has an answer and so the transition table has
/// somewhere to send every failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// Before anything: the client has connected and sent nothing, or has sent
    /// only the handshake. Every connection starts here.
    Handshaking,
    /// The server-list ping. **No authentication happens on this path at all**,
    /// which is why the byte budget in [`crate::io`] applies to it.
    Status,
    /// Login, including the encryption handshake and the compression switch.
    Login,
    /// Registry sync, resource packs, feature flags. Since 1.20.2, and
    /// re-enterable from [`State::Play`].
    Configuration,
    /// In the world.
    Play,
    /// Finished, for any reason. Terminal.
    Disconnected,
}

impl State {
    /// The name used in errors and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Handshaking => "Handshaking",
            Self::Status => "Status",
            Self::Login => "Login",
            Self::Configuration => "Configuration",
            Self::Play => "Play",
            Self::Disconnected => "Disconnected",
        }
    }

    /// Whether this state is reached before the client has been authenticated.
    ///
    /// The byte budget and the handshake deadline in [`crate::io`] key off
    /// this. `Login` counts: authentication *completes* at the end of login,
    /// so every byte of it is pre-authentication, including the ones that
    /// carry the encrypted shared secret.
    pub fn is_pre_authentication(self) -> bool {
        matches!(self, Self::Handshaking | Self::Status | Self::Login)
    }

    /// Whether `to` is a legal next state.
    ///
    /// The whole table, in one place, so that adding an edge is a visible
    /// change to a list rather than a new branch somewhere in a match.
    pub fn may_become(self, to: State) -> bool {
        // Every state may end. A connection can be dropped at any point, and a
        // machine that cannot express that grows an escape hatch instead.
        if to == State::Disconnected {
            return self != State::Disconnected;
        }
        match (self, to) {
            // The handshake's next-state field decides which of two paths this
            // connection is on. There is no third option and no way back.
            (State::Handshaking, State::Status | State::Login) => true,
            // Login Acknowledged, 1.20.2 and later. Before that, login went
            // straight to Play; Dust targets 1.21.1 and does not model the
            // older shape.
            (State::Login, State::Configuration) => true,
            // Finish Configuration.
            (State::Configuration, State::Play) => true,
            // Start Configuration: the server pulls a playing client back to
            // reconfigure it. This is the edge a forward-only machine omits.
            (State::Play, State::Configuration) => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Which way a packet is travelling.
///
/// Part of [`PacketContext`] because the id tables differ by direction as well
/// as by state: clientbound `0x00` in `Login` is Disconnect and serverbound
/// `0x00` in `Login` is Login Start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Client to server.
    Serverbound,
    /// Server to client.
    Clientbound,
}

/// Everything the layer above needs to resolve a packet id.
///
/// This is the seam. `dust-net` produces it; `dust-protocol` consumes it and
/// nothing travels the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketContext {
    pub state: State,
    pub direction: Direction,
}

/// A transition that is not in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: State,
    pub to: State,
}

impl std::fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a connection in {} cannot move to {}",
            self.from, self.to
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// What a client said it connected for, in the handshake's next-state field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Next state 1: a server-list ping.
    Status,
    /// Next state 2: an ordinary login.
    Login,
    /// Next state 3, added in 1.20.5: a login that arrived via a Transfer
    /// packet from another server.
    ///
    /// It is a *distinct intent* that leads to the *same state*. Collapsing it
    /// into `Login` here would lose the one thing it tells you — that this
    /// player was sent, not that they typed an address — which is exactly what
    /// a transfer-aware server needs to know.
    Transfer,
}

impl Intent {
    /// Read the handshake's next-state field.
    ///
    /// Anything else is refused. The field is a VarInt, so a client can send
    /// `-1` or `2147483647`; both are simply not intents.
    pub fn from_wire(value: i32) -> Result<Self, UnknownIntent> {
        match value {
            1 => Ok(Self::Status),
            2 => Ok(Self::Login),
            3 => Ok(Self::Transfer),
            _ => Err(UnknownIntent { value }),
        }
    }

    /// The number this intent is written as.
    pub fn to_wire(self) -> i32 {
        match self {
            Self::Status => 1,
            Self::Login => 2,
            Self::Transfer => 3,
        }
    }

    /// The state the connection moves to.
    pub fn state(self) -> State {
        match self {
            Self::Status => State::Status,
            Self::Login | Self::Transfer => State::Login,
        }
    }
}

/// A handshake next-state field that names no intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownIntent {
    pub value: i32,
}

impl std::fmt::Display for UnknownIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the handshake asked for next state {}; only 1 (status), 2 (login) and 3 \
             (transfer) exist",
            self.value
        )
    }
}

impl std::error::Error for UnknownIntent {}

/// The state of one connection, with every change checked.
///
/// Held by the transport rather than by the protocol layer, and mutated only
/// through [`transition`](Self::transition) and
/// [`disconnect`](Self::disconnect). There is deliberately no setter: a
/// connection whose state can be assigned has no state machine, only a field.
#[derive(Debug, Clone)]
pub struct Connection {
    state: State,
    /// What the handshake asked for, once it has been read. `None` in
    /// `Handshaking`.
    intent: Option<Intent>,
    /// How many times this connection has entered `Configuration`.
    ///
    /// Not a limit — it is a fact worth having when a client and a server
    /// disagree about how many reconfigurations happened, which is the shape
    /// of the bug the re-entrant edge introduces.
    configurations: u32,
}

impl Connection {
    /// A new connection, in [`State::Handshaking`].
    pub fn new() -> Self {
        Self {
            state: State::Handshaking,
            intent: None,
            configurations: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn intent(&self) -> Option<Intent> {
        self.intent
    }

    /// How many times this connection has entered `Configuration`; `1` for an
    /// ordinary login that has finished configuring.
    pub fn configuration_count(&self) -> u32 {
        self.configurations
    }

    /// The context for a packet travelling in `direction` right now.
    pub fn context(&self, direction: Direction) -> PacketContext {
        PacketContext {
            state: self.state,
            direction,
        }
    }

    /// Move to `to`, or fail naming both states.
    pub fn transition(&mut self, to: State) -> Result<(), IllegalTransition> {
        if !self.state.may_become(to) {
            return Err(IllegalTransition {
                from: self.state,
                to,
            });
        }
        if to == State::Configuration {
            self.configurations += 1;
        }
        self.state = to;
        Ok(())
    }

    /// Apply the handshake's next-state field.
    ///
    /// Combines reading the intent with the transition it implies, because
    /// doing them separately is how a connection ends up in `Login` with no
    /// record of whether it was transferred.
    pub fn handshake(&mut self, next_state: i32) -> Result<Intent, HandshakeError> {
        let intent = Intent::from_wire(next_state).map_err(HandshakeError::Unknown)?;
        self.transition(intent.state())
            .map_err(HandshakeError::Illegal)?;
        self.intent = Some(intent);
        Ok(intent)
    }

    /// End the connection. Idempotent, because the read half and the write
    /// half both discover a dead connection and neither should have to check
    /// whether the other got there first.
    pub fn disconnect(&mut self) {
        self.state = State::Disconnected;
    }

    pub fn is_disconnected(&self) -> bool {
        self.state == State::Disconnected
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a handshake was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// The next-state field named no intent.
    Unknown(UnknownIntent),
    /// A second handshake on a connection that had already left `Handshaking`.
    Illegal(IllegalTransition),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(e) => write!(f, "{e}"),
            Self::Illegal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// The type-level half of the state machine.
///
/// [`Connection`] checks transitions at run time because the wire decides
/// them. Where a *path* is known at compile time — the full login
/// conversation written out in order in `tests/login_session.rs` — the
/// state can be a type parameter instead, and the illegal transitions are then
/// methods that do not exist.
///
/// This is not a replacement for [`Connection`]; it is the other half of
/// "enforced by types where you can, and checked where you cannot". A server
/// reading frames off a socket cannot use it, because the type would have to
/// change based on a byte that has not arrived yet.
///
/// ```
/// use dust_net::state::{Session, State};
///
/// let session = Session::new();
/// assert_eq!(Session::<dust_net::state::Handshaking>::STATE, State::Handshaking);
/// let playing = session.login().configure().play();
/// assert_eq!(playing.state(), State::Play);
/// // And back, which 1.21.1 allows.
/// let reconfiguring = playing.reconfigure();
/// assert_eq!(reconfiguring.state(), State::Configuration);
/// ```
///
/// The illegal edges are absent rather than guarded:
///
/// ```compile_fail,E0599
/// use dust_net::state::Session;
/// // Handshaking has no `play`: login and configuration cannot be skipped.
/// let _ = Session::new().play();
/// ```
///
/// ```compile_fail,E0599
/// use dust_net::state::Session;
/// // Status is terminal. There is no way out of a server-list ping.
/// let _ = Session::new().status().login();
/// ```
///
/// **What the two blocks above do not catch.** A `compile_fail` doctest passes
/// when the code fails to compile for *any* reason, including a typo in a name
/// that was never going to resolve. Both are pinned to `E0599` — "no method
/// named X" — so a misspelling of `Session` or a bad import fails the test
/// rather than passing it. That narrows the hole; it does not close it, since
/// any other missing method also raises E0599.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session<S: Phase> {
    phase: std::marker::PhantomData<S>,
}

/// A state, as a type. Implemented only by the marker types in this module.
pub trait Phase {
    /// The run-time state this type stands for, so the two halves of the
    /// machine can be compared rather than assumed to agree.
    const STATE: State;
}

macro_rules! phases {
    ($($name:ident => $state:ident),* $(,)?) => { $(
        #[doc = concat!("The type-level [`State::", stringify!($state), "`].")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name;

        impl Phase for $name {
            const STATE: State = State::$state;
        }
    )* };
}

phases! {
    Handshaking => Handshaking,
    Status => Status,
    Login => Login,
    Configuration => Configuration,
    Play => Play,
}

impl<S: Phase> Session<S> {
    /// The run-time state matching this type.
    pub const STATE: State = S::STATE;

    /// The run-time state matching this type.
    pub fn state(self) -> State {
        S::STATE
    }

    fn next<T: Phase>(self) -> Session<T> {
        Session {
            phase: std::marker::PhantomData,
        }
    }
}

impl Session<Handshaking> {
    pub fn new() -> Self {
        Self {
            phase: std::marker::PhantomData,
        }
    }

    /// Next state 1.
    pub fn status(self) -> Session<Status> {
        self.next()
    }

    /// Next state 2 or 3.
    pub fn login(self) -> Session<Login> {
        self.next()
    }
}

impl Default for Session<Handshaking> {
    fn default() -> Self {
        Self::new()
    }
}

impl Session<Login> {
    /// Login Acknowledged.
    pub fn configure(self) -> Session<Configuration> {
        self.next()
    }
}

impl Session<Configuration> {
    /// Finish Configuration.
    pub fn play(self) -> Session<Play> {
        self.next()
    }
}

impl Session<Play> {
    /// Start Configuration: back to configuring, without a reconnect.
    pub fn reconfigure(self) -> Session<Configuration> {
        self.next()
    }
}

// `Session<Status>` has no transitions at all, which is the point: a status
// ping ends when the pong is written.

#[cfg(test)]
mod tests {
    use super::*;

    /// Every edge the protocol has, written out independently of
    /// [`State::may_become`].
    ///
    /// This is a second statement of the table rather than a derivation from
    /// it. A test that asks `may_become` whether `may_become` is right is a
    /// round trip, and agrees with itself under any table including a wrong
    /// one. These pairs come from the protocol's own state diagram.
    const LEGAL: &[(State, State)] = &[
        (State::Handshaking, State::Status),
        (State::Handshaking, State::Login),
        (State::Login, State::Configuration),
        (State::Configuration, State::Play),
        (State::Play, State::Configuration),
    ];

    const ALL: &[State] = &[
        State::Handshaking,
        State::Status,
        State::Login,
        State::Configuration,
        State::Play,
        State::Disconnected,
    ];

    #[test]
    fn exactly_the_protocol_edges_are_legal() {
        for &from in ALL {
            for &to in ALL {
                let expected = LEGAL.contains(&(from, to))
                    || (to == State::Disconnected && from != State::Disconnected);
                assert_eq!(
                    from.may_become(to),
                    expected,
                    "{from} -> {to} should be {}",
                    if expected { "legal" } else { "illegal" }
                );
            }
        }
    }

    #[test]
    fn an_illegal_transition_names_both_states() {
        let mut connection = Connection::new();
        let error = connection.transition(State::Play).unwrap_err();
        assert_eq!(
            error,
            IllegalTransition {
                from: State::Handshaking,
                to: State::Play
            }
        );
        let message = error.to_string();
        assert!(
            message.contains("Handshaking") && message.contains("Play"),
            "{message}"
        );
        // And the state did not move.
        assert_eq!(connection.state(), State::Handshaking);
    }

    #[test]
    fn status_is_terminal() {
        // A status ping that could reach Login would be an unauthenticated
        // path into the authenticated one.
        let mut connection = Connection::new();
        connection.handshake(1).expect("status intent");
        assert_eq!(connection.state(), State::Status);
        for &to in ALL {
            if to == State::Disconnected {
                continue;
            }
            assert!(connection.transition(to).is_err(), "Status -> {to}");
        }
    }

    #[test]
    fn reconfiguration_is_a_legal_round_trip() {
        // The 1.20.2 edge a forward-only machine omits. Asserting the count as
        // well as the state is what makes this different from "Play can reach
        // Configuration once".
        let mut connection = Connection::new();
        connection.handshake(2).expect("login intent");
        connection
            .transition(State::Configuration)
            .expect("login ack");
        connection
            .transition(State::Play)
            .expect("finish configuration");
        assert_eq!(connection.configuration_count(), 1);

        for round in 2..=5 {
            connection
                .transition(State::Configuration)
                .expect("start configuration");
            assert_eq!(connection.configuration_count(), round);
            connection.transition(State::Play).expect("finish again");
            assert_eq!(connection.state(), State::Play);
        }
    }

    #[test]
    fn transfer_is_its_own_intent_and_the_same_state() {
        let mut connection = Connection::new();
        assert_eq!(connection.handshake(3), Ok(Intent::Transfer));
        assert_eq!(connection.state(), State::Login);
        assert_eq!(connection.intent(), Some(Intent::Transfer));
        // The thing that would be lost by folding it into Login.
        assert_ne!(connection.intent(), Some(Intent::Login));
    }

    #[test]
    fn a_next_state_field_that_names_no_intent_is_refused() {
        for value in [i32::MIN, -1, 0, 4, 767, i32::MAX] {
            let mut connection = Connection::new();
            assert_eq!(
                connection.handshake(value),
                Err(HandshakeError::Unknown(UnknownIntent { value })),
                "next state {value}"
            );
            assert_eq!(connection.state(), State::Handshaking);
        }
    }

    #[test]
    fn a_second_handshake_is_refused() {
        let mut connection = Connection::new();
        connection.handshake(2).expect("first");
        assert_eq!(
            connection.handshake(2),
            Err(HandshakeError::Illegal(IllegalTransition {
                from: State::Login,
                to: State::Login
            }))
        );
    }

    #[test]
    fn intent_wire_values_round_trip_against_the_documented_numbers() {
        // The numbers are from the protocol, not from `to_wire`. Reading them
        // back through `from_wire` alone would agree with any numbering.
        assert_eq!(Intent::from_wire(1), Ok(Intent::Status));
        assert_eq!(Intent::from_wire(2), Ok(Intent::Login));
        assert_eq!(Intent::from_wire(3), Ok(Intent::Transfer));
        assert_eq!(Intent::Status.to_wire(), 1);
        assert_eq!(Intent::Login.to_wire(), 2);
        assert_eq!(Intent::Transfer.to_wire(), 3);
    }

    #[test]
    fn every_state_can_disconnect_and_disconnected_is_final() {
        for &from in ALL {
            let mut connection = Connection::new();
            connection.state = from;
            connection.disconnect();
            assert!(connection.is_disconnected());
            for &to in ALL {
                assert!(connection.transition(to).is_err(), "Disconnected -> {to}");
            }
        }
    }

    #[test]
    fn pre_authentication_covers_login_as_well_as_status() {
        // Login is pre-authentication: authentication *completes* at the end
        // of it. A budget that exempted Login would exempt the state an
        // attacker would simply stay in.
        assert!(State::Handshaking.is_pre_authentication());
        assert!(State::Status.is_pre_authentication());
        assert!(State::Login.is_pre_authentication());
        assert!(!State::Configuration.is_pre_authentication());
        assert!(!State::Play.is_pre_authentication());
    }

    #[test]
    fn the_typed_and_checked_halves_agree() {
        // Two machines that disagree about the table are worse than one. This
        // walks the typed path and asserts each type's `STATE` against a
        // `Connection` walked in step.
        let mut connection = Connection::new();
        let session = Session::new();
        assert_eq!(session.state(), connection.state());

        let session = session.login();
        connection.handshake(2).expect("login");
        assert_eq!(session.state(), connection.state());

        let session = session.configure();
        connection.transition(State::Configuration).expect("ack");
        assert_eq!(session.state(), connection.state());

        let session = session.play();
        connection.transition(State::Play).expect("finish");
        assert_eq!(session.state(), connection.state());

        let session = session.reconfigure();
        connection
            .transition(State::Configuration)
            .expect("restart");
        assert_eq!(session.state(), connection.state());
    }

    #[test]
    fn the_context_carries_both_halves_of_the_seam() {
        let mut connection = Connection::new();
        connection.handshake(1).expect("status");
        assert_eq!(
            connection.context(Direction::Serverbound),
            PacketContext {
                state: State::Status,
                direction: Direction::Serverbound
            }
        );
        assert_ne!(
            connection.context(Direction::Serverbound),
            connection.context(Direction::Clientbound),
            "direction must be part of the context; the id tables differ by it"
        );
    }
}

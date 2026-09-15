//! ICE server configuration: STUN for the common case, TURN relay as the
//! fallback when direct/reflexive connectivity fails (symmetric NATs,
//! locked-down corporate networks, ...).
//!
//! This is pure data — no sockets, no ICE agent, no ties to any specific
//! WebRTC binding — deliberately shaped to map 1:1 onto `webrtc::
//! peer_connection::transport::RTCIceServer` (webrtc-rs 0.20, confirmed
//! buildable in this sandbox) once a future phase wires up a real
//! `PeerConnectionBuilder`: `IceServer { urls, username, credential }` here
//! is `RTCIceServer { urls, username, credential, .. }` there, field for
//! field. See `crate` root docs for why that wiring isn't done yet.
//!
//! "The relay never sees plaintext" (CLAUDE.md / phase 4 spec) doesn't
//! depend on anything in this module: a TURN server only relays opaque
//! encrypted DTLS-SRTP packets between the two peers' already-established
//! DTLS session — it never terminates DTLS, so it's structurally incapable
//! of reading media regardless of which TURN server is configured here.

/// One ICE server entry — a STUN server (no credentials) or a TURN relay
/// (credentials required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl IceServer {
    pub fn stun(url: impl Into<String>) -> Self {
        Self { urls: vec![url.into()], username: None, credential: None }
    }

    pub fn turn(url: impl Into<String>, username: impl Into<String>, credential: impl Into<String>) -> Self {
        Self {
            urls: vec![url.into()],
            username: Some(username.into()),
            credential: Some(credential.into()),
        }
    }
}

/// The full ICE server list for a session: STUN always included so the
/// common case (direct or server-reflexive connectivity) doesn't need a
/// relay at all; TURN servers included only when the host has actually
/// configured one, since a relay costs real bandwidth to run and should
/// never be silently assumed to exist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IceConfig {
    pub servers: Vec<IceServer>,
}

impl IceConfig {
    /// A default STUN-only configuration. Works for the large majority of
    /// NAT types; without a configured TURN server, connections behind a
    /// symmetric NAT on both ends will simply fail to establish rather than
    /// silently falling back to something insecure — there is no fallback
    /// that isn't TURN.
    pub fn with_default_stun() -> Self {
        Self { servers: vec![IceServer::stun("stun:stun.l.google.com:19302")] }
    }

    /// Adds a TURN relay as a fallback candidate source, alongside
    /// whatever STUN/other servers are already configured. ICE tries
    /// host/server-reflexive candidates first and only actually routes
    /// media through TURN if nothing more direct works — adding a TURN
    /// server here does not mean traffic *will* be relayed, only that it
    /// *can* be if nothing better is available.
    pub fn with_turn_fallback(mut self, turn: IceServer) -> Self {
        self.servers.push(turn);
        self
    }

    pub fn has_turn_fallback(&self) -> bool {
        self.servers.iter().any(|s| s.username.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_stun_only_no_turn() {
        let config = IceConfig::with_default_stun();
        assert_eq!(config.servers.len(), 1);
        assert!(!config.has_turn_fallback());
        assert!(config.servers[0].urls[0].starts_with("stun:"));
    }

    #[test]
    fn adding_turn_preserves_the_existing_stun_servers() {
        let config = IceConfig::with_default_stun()
            .with_turn_fallback(IceServer::turn("turn:relay.example.com:3478", "user", "pass"));
        assert_eq!(config.servers.len(), 2);
        assert!(config.has_turn_fallback());
        assert!(config.servers[0].urls[0].starts_with("stun:"));
        assert!(config.servers[1].urls[0].starts_with("turn:"));
    }

    #[test]
    fn turn_server_carries_credentials_stun_does_not() {
        let stun = IceServer::stun("stun:example.com:3478");
        assert!(stun.username.is_none());
        assert!(stun.credential.is_none());

        let turn = IceServer::turn("turn:example.com:3478", "u", "p");
        assert_eq!(turn.username.as_deref(), Some("u"));
        assert_eq!(turn.credential.as_deref(), Some("p"));
    }

    #[test]
    fn no_turn_configured_means_no_fallback_not_a_silent_default() {
        // A host who never configures TURN gets exactly what they asked
        // for — no relay — rather than the library quietly inventing one.
        let config = IceConfig::with_default_stun();
        assert!(!config.has_turn_fallback());
    }
}

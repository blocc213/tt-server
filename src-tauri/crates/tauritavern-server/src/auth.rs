//! Single-user password gate.
//!
//! Scope is deliberately small: one shared password, one opaque session cookie,
//! sessions held in memory. This is not a user-account system; it exists so the
//! port is not open to anyone who can reach it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE: &str = "tt_session";
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub struct Auth {
    password_hash: Option<[u8; 32]>,
    sessions: Mutex<HashMap<String, Instant>>,
}

impl Auth {
    pub fn new(password: Option<String>) -> Self {
        Self {
            password_hash: password.map(|value| hash(value.as_bytes())),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// True when no password was configured, so every request is already allowed.
    pub fn is_open(&self) -> bool {
        self.password_hash.is_none()
    }

    /// Verifies a password in constant time and issues a session token.
    pub fn login(&self, password: &str) -> Option<String> {
        let expected = self.password_hash?;
        let candidate = hash(password.as_bytes());
        if !bool::from(candidate.ct_eq(&expected)) {
            return None;
        }

        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = hex::encode(bytes);

        let mut sessions = self.sessions.lock().expect("session lock");
        sessions.retain(|_, issued| issued.elapsed() < SESSION_TTL);
        sessions.insert(token.clone(), Instant::now());
        Some(token)
    }

    pub fn is_valid_session(&self, token: &str) -> bool {
        if self.is_open() {
            return true;
        }

        let mut sessions = self.sessions.lock().expect("session lock");
        match sessions.get(token) {
            Some(issued) if issued.elapsed() < SESSION_TTL => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn logout(&self, token: &str) {
        self.sessions.lock().expect("session lock").remove(token);
    }
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Extracts the session token from a `Cookie` header value.
pub fn session_token_from_cookies(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == SESSION_COOKIE).then(|| value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_password_yields_no_session() {
        let auth = Auth::new(Some("hunter2".into()));
        assert!(auth.login("nope").is_none());
        assert!(!auth.is_valid_session("anything"));
    }

    #[test]
    fn correct_password_yields_a_usable_session() {
        let auth = Auth::new(Some("hunter2".into()));
        let token = auth.login("hunter2").expect("login");
        assert!(auth.is_valid_session(&token));
        auth.logout(&token);
        assert!(!auth.is_valid_session(&token));
    }

    #[test]
    fn open_mode_accepts_any_request() {
        let auth = Auth::new(None);
        assert!(auth.is_open());
        assert!(auth.is_valid_session(""));
    }

    #[test]
    fn cookie_parsing_picks_the_session_entry() {
        assert_eq!(
            session_token_from_cookies("theme=dark; tt_session=abc123; other=1"),
            Some("abc123")
        );
        assert_eq!(session_token_from_cookies("theme=dark"), None);
    }
}

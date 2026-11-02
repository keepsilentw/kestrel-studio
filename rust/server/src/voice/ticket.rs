use std::collections::HashMap;
use std::sync::Mutex;

use crate::config;

/// A call is authorised by a short-lived, single-use ticket rather than by the
/// session cookie.
///
/// A browser cannot set a header on the `WebSocket` handshake, so the credential
/// has to ride in the URL. Anything in a URL lands in proxy logs and stays there,
/// which is why the value is one-time and expires in seconds: what an attacker
/// reads from a log is worth nothing by the time they can use it.
pub struct TicketStore {
    /// token -> (user id, expiry in ms)
    open: Mutex<HashMap<String, (i64, i64)>>,
}

/// One minute, as in the Nest server: long enough for a handshake plus the
/// upstream connect, short enough that a token read out of a proxy log is dead
/// before anyone can use it. It works once regardless.
pub const TICKET_TTL_MS: i64 = 60_000;

impl Default for TicketStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TicketStore {
    pub fn new() -> Self {
        Self { open: Mutex::new(HashMap::new()) }
    }

    pub fn issue(&self, user_id: i64, now_ms: i64) -> String {
        let token = crate::config::hex::encode(&config::random_bytes(16));
        let mut open = self.open.lock().expect("ticket lock");
        // The sweep rides on issue rather than a timer: the store only grows while
        // calls are being started, which is exactly when it is worth the lock.
        open.retain(|_, (_, expiry)| *expiry > now_ms);
        open.insert(token.clone(), (user_id, now_ms + TICKET_TTL_MS));
        token
    }

    /// Consumes the ticket: a second redeem of the same token is a miss even
    /// while it is unexpired.
    pub fn redeem(&self, token: &str, now_ms: i64) -> Option<i64> {
        let mut open = self.open.lock().expect("ticket lock");
        let (user_id, expiry) = open.remove(token)?;
        (expiry > now_ms).then_some(user_id)
    }

    pub fn len(&self) -> usize {
        self.open.lock().expect("ticket lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn a_ticket_authenticates_exactly_once() {
        let store = TicketStore::new();
        let token = store.issue(7, NOW);
        assert_eq!(store.len(), 1);
        assert_eq!(store.redeem(&token, NOW + 1), Some(7));
        assert_eq!(store.redeem(&token, NOW + 2), None, "replay must not work");
        assert!(store.is_empty(), "the entry goes with the redeem");
    }

    #[test]
    fn an_expired_ticket_is_worth_nothing() {
        let store = TicketStore::new();
        let token = store.issue(7, NOW);
        assert_eq!(store.redeem(&token, NOW + TICKET_TTL_MS), None);
        // Redeeming takes the row whether or not it was still live: a dead ticket
        // has no reason to sit in memory.
        assert_eq!(store.len(), 0);
        store.issue(8, NOW + TICKET_TTL_MS + 1);
        assert_eq!(store.len(), 1, "issuing swept the dead one");
    }

    #[test]
    fn unknown_and_empty_tokens_miss_without_panicking() {
        let store = TicketStore::new();
        assert_eq!(store.redeem("", NOW), None);
        assert_eq!(store.redeem("deadbeefdeadbeefdeadbeefdeadbeef", NOW), None);
    }

    #[test]
    fn tokens_are_unpredictable_and_carried_as_hex() {
        let store = TicketStore::new();
        let first = store.issue(1, NOW);
        let second = store.issue(1, NOW);
        assert_eq!(first.len(), 32, "16 bytes of entropy, hex");
        assert_ne!(first, second);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

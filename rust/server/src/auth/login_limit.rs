use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Five wrong passwords for one account from one address, then a cool-down.
/// The per-identity bound is deliberately tight: a human who mistypes five times
/// in fifteen minutes can wait; a script cannot spend the window guessing.
pub const MAX_IDENTITY_FAILURES: u32 = 5;

/// A looser per-address bound stops username rotation from turning the identity
/// bucket into a no-op. Twenty failures in fifteen minutes is already far above
/// anything a person does on this page.
pub const MAX_IP_FAILURES: u32 = 20;

pub const WINDOW: Duration = Duration::from_secs(15 * 60);

/// A fixed window is enough here: login attempts are rare, and the failure mode
/// of a fixed window (one extra burst at the boundary) costs far less than the
/// bookkeeping a sliding log would add to the request path.
#[derive(Debug, Clone, Copy)]
struct Window {
    failures: u32,
    started: Instant,
}

/// In-process login throttling. The service runs one process per stack, and the
/// deployment has a single public entry point, so a process-local map is the
/// right scope: a restart intentionally clears the counters.
#[derive(Default)]
pub struct LoginLimiter {
    entries: Mutex<HashMap<String, Window>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, Window>> {
        // A poisoned lock only means another request panicked while holding it;
        // refusing every later login would turn that into an outage.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Seconds until this identity/address may try again, or `None` while it is
    /// still allowed. Expired entries are pruned here so the map cannot grow
    /// without bound from one-off scans.
    pub fn blocked_for(&self, ip: &str, username: &str) -> Option<u64> {
        let now = Instant::now();
        let mut entries = self.locked();
        entries.retain(|_, window| now.saturating_duration_since(window.started) < WINDOW);

        let mut wait: Option<Duration> = None;
        for (key, maximum) in [
            (identity_key(ip, username), MAX_IDENTITY_FAILURES),
            (ip_key(ip), MAX_IP_FAILURES),
        ] {
            let Some(window) = entries.get(&key) else {
                continue;
            };
            if window.failures < maximum {
                continue;
            }
            let remaining = WINDOW.saturating_sub(now.saturating_duration_since(window.started));
            wait = Some(wait.map_or(remaining, |current| current.max(remaining)));
        }
        wait.map(|remaining| remaining.as_secs().max(1))
    }

    pub fn record_failure(&self, ip: &str, username: &str) {
        let now = Instant::now();
        let mut entries = self.locked();
        entries.retain(|_, window| now.saturating_duration_since(window.started) < WINDOW);
        for key in [identity_key(ip, username), ip_key(ip)] {
            match entries.get_mut(&key) {
                Some(window) => window.failures = window.failures.saturating_add(1),
                None => {
                    entries.insert(
                        key,
                        Window {
                            failures: 1,
                            started: now,
                        },
                    );
                }
            }
        }
    }

    /// A correct password clears the account bucket. The address bucket stays:
    /// one valid login should not reset a spray that is still in progress.
    pub fn record_success(&self, ip: &str, username: &str) {
        self.locked().remove(&identity_key(ip, username));
    }
}

fn normalize_ip(ip: &str) -> String {
    ip.trim().chars().take(64).collect()
}

fn normalize_username(username: &str) -> String {
    username.trim().to_lowercase().chars().take(64).collect()
}

fn identity_key(ip: &str, username: &str) -> String {
    format!("id:{}|{}", normalize_ip(ip), normalize_username(username))
}

fn ip_key(ip: &str) -> String {
    format!("ip:{}", normalize_ip(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_failures_block_the_same_identity() {
        let limiter = LoginLimiter::new();
        for _ in 0..MAX_IDENTITY_FAILURES {
            assert!(limiter.blocked_for("203.0.113.7", "admin").is_none());
            limiter.record_failure("203.0.113.7", "admin");
        }
        assert!(limiter.blocked_for("203.0.113.7", "admin").is_some());
        // A different account from the same address still has room.
        assert!(limiter.blocked_for("203.0.113.7", "someone-else").is_none());
    }

    #[test]
    fn a_success_clears_the_identity_bucket() {
        let limiter = LoginLimiter::new();
        for _ in 0..MAX_IDENTITY_FAILURES - 1 {
            limiter.record_failure("203.0.113.7", "admin");
        }
        limiter.record_success("203.0.113.7", "admin");
        limiter.record_failure("203.0.113.7", "admin");
        assert!(limiter.blocked_for("203.0.113.7", "admin").is_none());
    }

    #[test]
    fn rotating_usernames_still_hit_the_address_bucket() {
        let limiter = LoginLimiter::new();
        for index in 0..MAX_IP_FAILURES {
            limiter.record_failure("203.0.113.7", &format!("user-{index}"));
        }
        assert!(limiter.blocked_for("203.0.113.7", "one-more").is_some());
        assert!(limiter.blocked_for("203.0.113.8", "one-more").is_none());
    }
}

use sha2::Sha256;

use crate::config::random_bytes;

/// Two formats share one column, dispatched per row by its prefix.
///
/// bcrypt (`$2b$`, cost 10) is the default: it is what `bcryptjs` — the package
/// the Nest server used, and the one that wrote the hashes already in
/// `data/kestrel-studio.db` — produces, so accounts survive the cutover in both
/// directions. `crates.io`'s `bcrypt` is not in the local registry and this box
/// has no outbound network, so the algorithm lives in `super::bcrypt`, ported
/// from that same `bcryptjs` and checked against it and against Apache
/// crypt_blowfish.
///
/// PBKDF2 (`$pbkdf2-sha256$`) is the format that stood in while bcrypt was not
/// yet implemented. It stays verifiable so no row written during that gap is
/// orphaned; nothing new is written in it.
const ITERATIONS: u32 = 120_000;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
const BCRYPT_PREFIX: &str = "$2";
const PREFIX: &str = "$pbkdf2-sha256$";

/// The stored form for a new password.
pub fn hash(password: &str) -> String {
    super::bcrypt::generate_hash(password)
}

/// Writes a PBKDF2 hash. Kept for the tests and for any operator who prefers a
/// format from std-only primitives; the login path accepts either.
pub fn hash_pbkdf2(password: &str) -> String {
    let salt = random_bytes(SALT_LEN);
    let derived = derive(password, &salt);
    format!(
        "{PREFIX}{ITERATIONS}${}${}",
        base64encode(&salt),
        base64encode(&derived)
    )
}

pub fn verify(password: &str, stored: &str) -> bool {
    if stored.starts_with(BCRYPT_PREFIX) {
        return super::bcrypt::verify(password, stored);
    }
    if !stored.starts_with(PREFIX) {
        return false;
    }
    let mut parts = stored.trim_start_matches(PREFIX).split('$');
    let (Some(iterations), Some(salt), Some(expected)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Ok(iterations) = iterations.parse::<u32>() else {
        return false;
    };
    let Some(salt) = base64decode(salt) else {
        return false;
    };
    let Some(expected) = base64decode(expected) else {
        return false;
    };

    let mut out = vec![0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut out);
    constant_time_eq(&out, &expected)
}

/// Whether a stored hash uses a format this build cannot check at all. The login
/// route can then answer "reset required" instead of "wrong password" — with both
/// formats supported, that is now only the case for a hand-edited row.
pub fn is_unverifiable(stored: &str) -> bool {
    !stored.starts_with(BCRYPT_PREFIX) && !stored.starts_with(PREFIX)
}

fn derive(password: &str, salt: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, ITERATIONS, &mut out);
    out
}

fn base64encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64decode(value: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_passwords_are_written_as_bcrypt_and_verify() {
        let stored = hash("123456");
        assert!(stored.starts_with("$2b$10$"), "{stored}");
        assert_eq!(stored.chars().count(), 60);
        assert!(verify("123456", &stored));
        assert!(!verify("123457", &stored));
        assert!(!verify("", &stored));
        assert!(verify("红隼", &hash("红隼")), "non-ASCII round trips");
    }

    #[test]
    fn the_pbkdf2_format_still_verifies_so_no_early_row_is_orphaned() {
        let stored = hash_pbkdf2("123456");
        assert!(stored.starts_with(PREFIX), "{stored}");
        assert!(verify("123456", &stored));
        assert!(!verify("1234567", &stored));
    }

    #[test]
    fn the_same_password_yields_different_hashes() {
        assert_ne!(hash("a"), hash("a"), "salted");
        assert_ne!(hash_pbkdf2("a"), hash_pbkdf2("a"), "salted");
    }

    /// The check that actually matters for the cutover: the hashes sitting in the
    /// developer's `data/kestrel-studio.db` — written by `bcryptjs` under the Nest
    /// server — still authenticate here.
    ///
    /// Ignored by default because it reads a real database and its credentials are
    /// not in the repository. Run it with:
    ///   KESTREL_LOCAL_DB=data/kestrel-studio.db cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn the_accounts_in_the_local_database_keep_signing_in() {
        use sqlx::Connection;
        use sqlx::Row;

        let path = std::env::var("KESTREL_LOCAL_DB").expect("KESTREL_LOCAL_DB");
        let mut connection = sqlx::sqlite::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .read_only(true)
                .create_if_missing(false),
        )
        .await
        .expect("open read-only");

        let rows = sqlx::query("SELECT username, password_hash FROM users")
            .fetch_all(&mut connection)
            .await
            .expect("read users");
        assert!(!rows.is_empty(), "the local database has accounts");
        for row in rows {
            let username: String = row.try_get("username").expect("username");
            let stored: String = row.try_get("password_hash").expect("password_hash");
            // The seeded local account is the only one whose plaintext is known;
            // any other row is checked for shape, not for a password.
            if username == super::super::seed::DEFAULT_ADMIN_USERNAME {
                assert!(
                    verify(super::super::seed::DEFAULT_ADMIN_PASSWORD, &stored),
                    "{username} must still be able to sign in"
                );
            }
            assert!(
                !is_unverifiable(&stored),
                "{username} holds a format this build cannot check"
            );
        }
    }

    /// The other direction of the same guarantee: a row written **here** must
    /// still authenticate against the Nest server, so both binaries can share one
    /// database during the cutover instead of locking each other out.
    ///
    /// Shells out to the repository's own `bcryptjs` — the library the Nest server
    /// uses — hence ignored as well. Run it with:
    ///   cd rust && cargo test -- --ignored
    #[test]
    #[ignore]
    fn a_hash_written_here_is_accepted_by_bcryptjs() {
        let password = "cross-check-红隼";
        let stored = hash(password);
        let probe = "console.log(require('bcryptjs').compareSync(process.argv[1], process.argv[2]))";
        let output = std::process::Command::new("node")
            .args(["-e", probe, password, &stored])
            .output()
            .expect("node is on PATH for this check");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "bcryptjs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout.trim(),
            "true",
            "bcryptjs rejected the hash written by this implementation ({stored})"
        );
        // And the reverse pair, to show the check is not vacuous.
        let rejected = std::process::Command::new("node")
            .args(["-e", probe, "wrong", &stored])
            .output()
            .expect("node");
        assert_eq!(String::from_utf8_lossy(&rejected.stdout).trim(), "false");
    }

    #[test]
    fn garbage_stored_values_never_verify_and_are_reported_as_unknown() {
        for stored in [
            "",
            "plain",
            "$pbkdf2-sha256$1$c2FsdA==$",
            "$pbkdf2-sha256$notanumber$c2FsdA==$c2FsdA==",
            "$2b$05$abcdefghijklmnopqrstuv",
            "$2b$99$abcdefghijklmnopqrstuvabcdefghijklmnopqrstuvwxyz0123456789ab",
        ] {
            assert!(!verify("123456", stored), "{stored}");
        }
        assert!(!is_unverifiable(&hash("x")), "bcrypt is a known format");
        assert!(!is_unverifiable(&hash_pbkdf2("x")), "pbkdf2 is a known format");
        assert!(is_unverifiable("plain"), "anything else is not");
    }
}

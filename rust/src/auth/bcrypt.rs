//! bcrypt, hand-ported from the `bcryptjs` implementation this project already
//! ships as a dependency.
//!
//! The reason it is here at all: the accounts in `data/kestrel-studio.db` carry
//! `$2b$` hashes written by `bcryptjs`, and crates.io's `bcrypt` is not in the
//! local registry (this box has no outbound network), so the only way to keep
//! those accounts signing in during the NestJS-to-Rust cutover is to implement
//! the algorithm. Being a port rather than a re-reading of the paper, it matches
//! `bcryptjs`'s choices exactly — including that `$2a`, `$2b` and `$2y` are all
//! verified the same way, which is what `bcryptjs.compareSync` does. Every hash
//! written here is therefore readable by the Nest server and the other way
//! round, which is what makes a staged cutover possible.
//!
//! Not constant-time in the key schedule (the same as the reference), but the
//! final comparison is.

use super::blowfish_tables::{C_ORIG, P_ORIG, S_ORIG};

const BCRYPT_SALT_LEN: usize = 16;
const NUM_ROUNDS: usize = 16;
const MIN_COST: u32 = 4;
const MAX_COST: u32 = 31;

/// bcrypt's own non-standard base64 dictionary: `.` and `/` first, no padding.
const ITOA64: &[u8; 64] = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Matches `bcryptjs`'s default, so the column keeps one format across both
/// servers.
pub const DEFAULT_COST: u32 = 10;

struct Blowfish {
    p: [u32; 18],
    s: [u32; 1024],
}

impl Blowfish {
    fn new() -> Self {
        Self {
            p: P_ORIG,
            s: S_ORIG,
        }
    }

    /// The four-table substitution at the heart of Blowfish.
    fn feistel(&self, word: u32) -> u32 {
        let mut n = self.s[(word >> 24) as usize];
        n = n.wrapping_add(self.s[0x100 | ((word >> 16) & 0xff) as usize]);
        n ^= self.s[0x200 | ((word >> 8) & 0xff) as usize];
        n.wrapping_add(self.s[0x300 | (word & 0xff) as usize])
    }

    /// One Blowfish encipherment of a 64-bit block, as `P[0]`-prefixed Feistel
    /// rounds. The reference unrolls sixteen of these; a loop is the same
    /// arithmetic.
    fn encipher(&mut self, left: u32, right: u32) -> (u32, u32) {
        let mut l = left ^ self.p[0];
        let mut r = right;
        for round in 0..NUM_ROUNDS / 2 {
            r = r ^ self.feistel(l) ^ self.p[round * 2 + 1];
            l = l ^ self.feistel(r) ^ self.p[round * 2 + 2];
        }
        (r ^ self.p[NUM_ROUNDS + 1], l)
    }

    /// Four bytes of `data`, wrapping around its end, read as a big-endian word.
    /// This cycling is why a password longer than 72 bytes keeps being consumed.
    fn stream_to_word(data: &[u8], offset: &mut usize) -> u32 {
        let mut word = 0u32;
        for _ in 0..4 {
            word = (word << 8) | u32::from(data[*offset] & 0xff);
            *offset = (*offset + 1) % data.len();
        }
        word
    }

    /// The ordinary Blowfish key expansion.
    fn expand_key(&mut self, key: &[u8]) {
        let mut offset = 0usize;
        for index in 0..18 {
            let word = Self::stream_to_word(key, &mut offset);
            self.p[index] ^= word;
        }
        let mut lr = (0u32, 0u32);
        for index in (0..18).step_by(2) {
            lr = self.encipher(lr.0, lr.1);
            self.p[index] = lr.0;
            self.p[index + 1] = lr.1;
        }
        for index in (0..1024).step_by(2) {
            lr = self.encipher(lr.0, lr.1);
            self.s[index] = lr.0;
            self.s[index + 1] = lr.1;
        }
    }

    /// The "expensive key schedule" that makes bcrypt slow: the salt is folded in
    /// as data, so the same password yields a different cipher per salt.
    fn eks_key(&mut self, data: &[u8], key: &[u8]) {
        let mut offset = 0usize;
        for index in 0..18 {
            let word = Self::stream_to_word(key, &mut offset);
            self.p[index] ^= word;
        }
        offset = 0;
        let mut lr = (0u32, 0u32);
        for index in (0..18).step_by(2) {
            lr.0 ^= Self::stream_to_word(data, &mut offset);
            lr.1 ^= Self::stream_to_word(data, &mut offset);
            lr = self.encipher(lr.0, lr.1);
            self.p[index] = lr.0;
            self.p[index + 1] = lr.1;
        }
        for index in (0..1024).step_by(2) {
            lr.0 ^= Self::stream_to_word(data, &mut offset);
            lr.1 ^= Self::stream_to_word(data, &mut offset);
            lr = self.encipher(lr.0, lr.1);
            self.s[index] = lr.0;
            self.s[index + 1] = lr.1;
        }
    }
}

/// The expensive part: EksBlowfish, `2^cost` key-schedule rounds, then sixty-four
/// encipherments of "OrpheanBeholderScryDoubt".
fn crypt(password: &[u8], salt: &[u8], cost: u32) -> Vec<u8> {
    let mut cipher = Blowfish::new();
    cipher.eks_key(salt, password);
    let rounds = 1u64 << cost;
    for _ in 0..rounds {
        cipher.expand_key(password);
        cipher.expand_key(salt);
    }

    let mut cdata = C_ORIG;
    for _ in 0..64 {
        for pair in 0..(cdata.len() / 2) {
            let (left, right) = cipher.encipher(cdata[pair * 2], cdata[pair * 2 + 1]);
            cdata[pair * 2] = left;
            cdata[pair * 2 + 1] = right;
        }
    }

    let mut out = Vec::with_capacity(cdata.len() * 4);
    for word in cdata {
        out.push(((word >> 24) & 0xff) as u8);
        out.push(((word >> 16) & 0xff) as u8);
        out.push(((word >> 8) & 0xff) as u8);
        out.push((word & 0xff) as u8);
    }
    out
}

/// The parsed form of `$2[ayb]$CC$<22 salt chars>`.
struct Salt {
    /// The version letter after `$2`, or None for the bare `$2$` form.
    minor: Option<char>,
    cost: u32,
    bytes: Vec<u8>,
}

impl Salt {
    /// Whether the password gets a trailing NUL byte. `bcryptjs` appends it for
    /// every revision from `$2a` onwards and not for the legacy `$2$`.
    fn appends_nul(&self) -> bool {
        self.minor.is_some_and(|minor| minor >= 'a')
    }
}

fn parse_salt(value: &str) -> Result<Salt, String> {
    let bytes = value.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'$' || bytes[1] != b'2' {
        return Err("Invalid salt version".to_string());
    }
    let (minor, offset) = if bytes[2] == b'$' {
        (None, 3usize)
    } else {
        let minor = bytes[2] as char;
        if !matches!(minor, 'a' | 'b' | 'y') || bytes.get(3) != Some(&b'$') {
            return Err("Invalid salt revision".to_string());
        }
        (Some(minor), 4usize)
    };

    let digits = value.get(offset..offset + 2).ok_or("Missing salt rounds")?;
    if digits.len() != 2 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Missing salt rounds".to_string());
    }
    let cost: u32 = digits
        .parse()
        .map_err(|_| "Missing salt rounds".to_string())?;
    if cost < MIN_COST || cost > MAX_COST {
        return Err(format!("Illegal number of rounds (4-31): {cost}"));
    }

    let encoded = value
        .get(offset + 3..offset + 3 + 22)
        .ok_or("Missing salt bytes")?;
    let salt_bytes = base64_decode(encoded, BCRYPT_SALT_LEN);
    if salt_bytes.len() != BCRYPT_SALT_LEN {
        return Err("Illegal salt length".to_string());
    }

    Ok(Salt {
        minor,
        cost,
        bytes: salt_bytes,
    })
}

/// `$2b$10$` plus twenty-two salt characters.
pub fn generate_salt(cost: u32) -> String {
    let mut random = crate::config::random_bytes(BCRYPT_SALT_LEN);
    if cost < MIN_COST || cost > MAX_COST {
        random = crate::config::random_bytes(BCRYPT_SALT_LEN);
    }
    format!(
        "$2b${:02}${}",
        cost.clamp(MIN_COST, MAX_COST),
        base64_encode(&random, BCRYPT_SALT_LEN)
    )
}

/// Hashes with a fresh salt at `DEFAULT_COST`.
pub fn generate_hash(password: &str) -> String {
    hash_with(password, &generate_salt(DEFAULT_COST)).expect("a salt this function made itself")
}

pub fn hash_with(password: &str, salt: &str) -> Result<String, String> {
    let parsed = parse_salt(salt)?;
    let mut key = password.as_bytes().to_vec();
    if parsed.appends_nul() {
        key.push(0);
    }
    let digest = crypt(&key, &parsed.bytes, parsed.cost);

    Ok(format!(
        "$2{}${:02}${}{}",
        parsed.minor.map_or(String::new(), |minor| minor.to_string()),
        parsed.cost,
        base64_encode(&parsed.bytes, BCRYPT_SALT_LEN),
        // 23 of the 24 digest bytes: the last half-byte is not part of the
        // encoded hash, which is why a bcrypt hash is 60 characters.
        base64_encode(&digest, digest.len() - 1)
    ))
}

/// Checks a password against a stored hash. Anything malformed answers false
/// rather than panicking: the column is attacker-adjacent only through an
/// account rename, but a bad row must not take the process down.
pub fn verify(password: &str, stored: &str) -> bool {
    let Some(salt_part) = stored.get(..29) else {
        return false;
    };
    match hash_with(password, salt_part) {
        Ok(recomputed) => constant_time_eq(recomputed.as_bytes(), stored.as_bytes()),
        Err(_) => false,
    }
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

/// Mirrors `bcryptjs.base64_encode`: no padding, and the final byte contributes
/// one character instead of two.
fn base64_encode(bytes: &[u8], len: usize) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    while offset < len {
        let mut value = bytes[offset] & 0xff;
        offset += 1;
        out.push(ITOA64[((value >> 2) & 0x3f) as usize] as char);
        value = (value & 0x03) << 4;
        if offset >= len {
            out.push(ITOA64[(value & 0x3f) as usize] as char);
            break;
        }
        let next = bytes[offset] & 0xff;
        offset += 1;
        value |= (next >> 4) & 0x0f;
        out.push(ITOA64[(value & 0x3f) as usize] as char);
        value = (next & 0x0f) << 2;
        if offset >= len {
            out.push(ITOA64[(value & 0x3f) as usize] as char);
            break;
        }
        let last = bytes[offset] & 0xff;
        offset += 1;
        value |= (last >> 6) & 0x03;
        out.push(ITOA64[(value & 0x3f) as usize] as char);
        out.push(ITOA64[(last & 0x3f) as usize] as char);
    }
    out
}

/// Mirrors `bcryptjs.base64_decode`, which stops at the first character outside
/// the dictionary and never reads more than `len` bytes.
fn base64_decode(value: &str, len: usize) -> Vec<u8> {
    let chars: Vec<char> = value.chars().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut offset = 0usize;

    let index_of = |character: Option<char>| -> i32 {
        character
            .and_then(|found| ITOA64.iter().position(|&needle| needle as char == found))
            .map(|position| position as i32)
            .unwrap_or(-1)
    };

    while offset + 1 < chars.len() && out.len() < len {
        let c1 = index_of(chars.get(offset).copied());
        offset += 1;
        let c2 = index_of(chars.get(offset).copied());
        offset += 1;
        if c1 == -1 || c2 == -1 {
            break;
        }
        out.push(((c1 << 2) | ((c2 & 0x30) >> 4)) as u8);
        if out.len() >= len || offset >= chars.len() {
            break;
        }
        let c3 = index_of(chars.get(offset).copied());
        offset += 1;
        if c3 == -1 {
            break;
        }
        out.push((((c2 & 0x0f) << 4) | ((c3 & 0x3c) >> 2)) as u8);
        if out.len() >= len || offset >= chars.len() {
            break;
        }
        let c4 = index_of(chars.get(offset).copied());
        offset += 1;
        out.push((((c3 & 0x03) << 6) | c4) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent references, not self-consistency:
    ///   * the `$2b$` rows come from `bcryptjs` (the package this project already
    ///     depends on, and the one that wrote the hashes in `data/`):
    ///     `node -e "console.log(require('bcryptjs').hashSync(pw, cost))"`
    ///   * the `$2y$` rows come from `/usr/sbin/htpasswd -nbB <user> <pw>`
    ///     (Apache crypt_blowfish, a third-party implementation).
    /// A port that agrees with two unrelated implementations is the point of this
    /// module; agreement with itself would prove nothing.
    const VECTORS: &[(&str, &str)] = &[
        (
            "hunter2",
            "$2b$05$l4nOTvAT81unTIZ8sapbhOokDUWqdit33yrM/FqEDPeVtTiY8dZK6",
        ),
        (
            "",
            "$2b$06$kJaiLyZ7Vqdl0ts1VCDHDesw9Mi6My2MVYWAsr3vFY1lWPyAb1LDS",
        ),
        (
            "红隼-口令",
            "$2b$05$bSO5ybxpBK0G8XBW2IgFp.uOqKFTAu/OrdKrmqE4Jf1vYTWvZCxie",
        ),
        // 80 bytes: only the first 72 are ever read, so this is also the hash of
        // 72 x's. See `key_material_past_seventy_two_bytes_is_never_read`.
        (
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "$2b$04$abcdefghijklmnopqrstuubzadhGtS2zEF.gu0yd0opP6cVzb.e0i",
        ),
        // Exactly 72 bytes.
        (
            "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
            "$2b$04$abcdefghijklmnopqrstuuwurWIdVVT4m5pTArtqnFNM69nySdHj.",
        ),
        // The legacy version, where no NUL byte is appended to the password.
        (
            "hunter2",
            "$2$05$abcdefghijklmnopqrstuuhcrv8VqLg5vQLk/2Eh2DdLCN3dPGiFm",
        ),
        (
            "hunter2",
            "$2y$05$aiSPow2FlMOCRZ4eGAWa3OUOfkOcpobRFCBYSst7yEle14FT/WvTK",
        ),
        (
            "",
            "$2y$05$qErdtKu2Qu0TqgXGXLsM9.59Dg8UMkv6uVuk1PmzU4emuS1K5q7Um",
        ),
    ];

    #[test]
    fn every_reference_vector_verifies() {
        for (password, stored) in VECTORS {
            assert!(verify(password, stored), "must verify {stored}");
            // Recomputing with the stored salt reproduces the whole string.
            let salt = stored.get(..29).expect("salt prefix");
            assert_eq!(&hash_with(password, salt).expect("hash"), stored);
        }
    }

    #[test]
    fn a_wrong_password_never_verifies() {
        for (password, stored) in VECTORS {
            // Mutate the first character. Appending one is NOT a reliable wrong
            // password here: bytes past 72 are never read (see the test below), so
            // "x".repeat(80) + "!" is the same key as "x".repeat(80).
            let mut chars: Vec<char> = password.chars().collect();
            let wrong = if chars.is_empty() {
                "!".to_string()
            } else {
                chars[0] = 'Q';
                chars.iter().collect()
            };
            assert!(!verify(&wrong, stored), "{wrong:?} must not pass");
        }
        assert!(!verify("hunter2", "$2b$05$short"));
        assert!(!verify("hunter2", "not-a-hash-at-all"));
        assert!(!verify("hunter2", "$2z$05$abcdefghijklmnopqrstuvabcdefghijklmnopqrstuvabcdefghi"));
        assert!(!verify("hunter2", "$2b$03$abcdefghijklmnopqrstuvabcdefghijklmnopqrstuvabcdefghi"));
        assert!(!verify("hunter2", "$2b$40$abcdefghijklmnopqrstuvabcdefghijklmnopqrstuvabcdefghi"));
    }

    #[test]
    fn a_generated_hash_round_trips_at_the_default_cost() {
        let stored = generate_hash("s3cret");
        assert!(stored.starts_with("$2b$10$"), "{stored}");
        assert_eq!(stored.chars().count(), 60);
        assert!(verify("s3cret", &stored));
        assert!(!verify("S3cret", &stored));
    }

    #[test]
    fn the_same_salt_gives_the_same_hash_and_a_different_salt_does_not() {
        let salt = "$2b$04$abcdefghijklmnopqrstuv";
        assert_eq!(hash_with("abc", salt).expect("a"), hash_with("abc", salt).expect("b"));
        assert_ne!(
            hash_with("abc", salt).expect("a"),
            hash_with("abc", "$2b$04$vwutsrqponmlkjihgfedcb").expect("b")
        );
    }

    #[test]
    fn key_material_past_seventy_two_bytes_is_never_read() {
        // Each key pass consumes exactly the P-array: eighteen words, seventy-two
        // bytes, with the offset restarting per pass. So the digest of a longer
        // password is the digest of its first 72 bytes - and `bcryptjs` behaves the
        // same way, which the reference vector below proves:
        //   node -e "console.log(require('bcryptjs').compareSync('x'.repeat(72), H))"
        //   ... => true against the hash of 80 x's, while 71 x's is false.
        let long = "$2b$04$abcdefghijklmnopqrstuubzadhGtS2zEF.gu0yd0opP6cVzb.e0i";
        assert!(verify(&"x".repeat(72), long));
        assert!(verify(&"x".repeat(80), long));
        assert!(!verify(&"x".repeat(71), long), "the cut is exactly at 72");
        assert_eq!(
            hash_with(&"A".repeat(73), "$2b$04$abcdefghijklmnopqrstuv").expect("a"),
            hash_with(&"A".repeat(72), "$2b$04$abcdefghijklmnopqrstuv").expect("b"),
            "the 73rd byte cannot change anything"
        );
    }

    #[test]
    fn generated_salts_are_shaped_right_and_distinct() {
        let first = generate_salt(5);
        let second = generate_salt(5);
        assert!(first.starts_with("$2b$05$"), "{first}");
        assert_eq!(first.chars().count(), 29);
        assert_ne!(first, second, "16 random bytes");
        assert!(parse_salt(&first).is_ok());
        assert!(parse_salt("$2b$03$abcdefghijklmnopqrstuv").is_err(), "cost below 4");
        assert!(parse_salt("$2b$32$abcdefghijklmnopqrstuv").is_err(), "cost above 31");
        assert!(parse_salt("$2x$05$abcdefghijklmnopqrstuv").is_err(), "unknown revision");
        assert!(parse_salt("x2b$05$abcdefghijklmnopqrstuv").is_err(), "no dollar prefix");
        assert!(parse_salt("$2b$05$short").is_err(), "too few salt bytes");
    }

    #[test]
    fn the_dictionary_and_its_inverse_agree_on_a_full_salt() {
        let bytes: Vec<u8> = (0..16u8).collect();
        let encoded = base64_encode(&bytes, bytes.len());
        assert_eq!(encoded.chars().count(), 22, "16 bytes make 22 characters");
        assert_eq!(base64_decode(&encoded, 16), bytes);
        assert_eq!(base64_decode("./ABCDEFGHIJKLMNOPQRSTUVWXYZ", 16).len(), 16);
        // Stops at the first character outside the dictionary.
        assert_eq!(base64_decode("ab!cd", 16).len(), 1);
        assert!(base64_decode("", 16).is_empty());
    }

    #[test]
    fn only_the_modern_revisions_append_the_nul_byte() {
        assert!(parse_salt("$2b$05$abcdefghijklmnopqrstuv").expect("2b").appends_nul());
        assert!(parse_salt("$2y$05$abcdefghijklmnopqrstuv").expect("2y").appends_nul());
        assert!(parse_salt("$2a$05$abcdefghijklmnopqrstuv").expect("2a").appends_nul());
        assert!(
            !parse_salt("$2$05$abcdefghijklmnopqrstuv").expect("legacy").appends_nul(),
            "the bare $2$ form is what the reference treats as minor 0"
        );
    }
}

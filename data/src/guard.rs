//! Who may ask share.rs's server, and how each request proves it.
//!
//! **Devices, not a shared code.** Each Magnitor Remote keeps a P-256 key pair in its platform's
//! keychain; the private half never leaves the phone. The Mac knows a phone by its public key
//! (an `id`, the first 8 bytes of its SHA-256, in hex) once the person has approved it in
//! Settings. The pairing code only introduces a phone: it works once, for `CODE_LIFE`, and for at
//! most `CODE_TRIES` wrong guesses.
//!
//! **Pairing** (`POST /v1/pair?name=…&x=…&y=…&proof=…`): `x` and `y` are the public key's JWK
//! coordinates (base64url), `proof` is `pair_proof`, the hex SHA-256 of the code and the key, so
//! the code itself never crosses the network. A matching proof uses the code up and leaves the
//! phone *pending* until the person allows it on the Mac, comparing the six digits both screens
//! show (`check_digits`, from the key): someone who learned the code and raced the phone would show
//! other digits.
//!
//! **Every other request** carries
//! `Authorization: Magnitor id=<id>,ch=<challenge>,seq=<n>,sig=<base64url r‖s>`, an ES256
//! signature over `signed_text`: the session's challenge, the sequence number, the method and the
//! path with its query. The challenge is named too, so a session the server no longer has is told
//! apart from a forgery. `GET /v1/hello?id=<id>` hands a paired phone its session's
//! challenge and the highest sequence number used so far; a number is accepted once, within a
//! window of `WINDOW` below the highest (`Session::accept`), so a request seen on the network can't
//! be sent again and requests sent together may arrive in any order. Sessions last while the
//! server runs; a phone whose challenge is gone is told the session is `stale` and says hello again.
//!
//! **Lockout.** An address that fails `MAX_FAILS` times in `FAIL_WINDOW` (a wrong proof, a bad
//! signature, an unknown phone) is refused for `LOCKOUT`, without the server waiting on it.
//! Only peers on the local network are answered at all (`is_local`).
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// How long a pairing code works, and how many wrong proofs void it.
pub const CODE_LIFE: Duration = Duration::from_secs(10 * 60);
pub const CODE_TRIES: u32 = 5;
/// How long a phone waits for the person to allow it before it must pair again.
pub const PENDING_LIFE: Duration = Duration::from_secs(5 * 60);
/// Sequence numbers this far below the highest are still accepted once.
pub const WINDOW: u64 = 64;
/// A session unused this long is dropped; the phone says hello again.
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);
pub const MAX_FAILS: u32 = 5;
pub const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// The most a device's name may be, in characters.
const NAME_MAX: usize = 40;

/// A phone the person has allowed: its id, name and public key (SEC1, uncompressed: `04‖x‖y`).
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub x: String,
    pub y: String,
    key: Vec<u8>,
}

impl Device {
    /// From its coordinates as the phone sends them; `None` unless both are 32 bytes.
    pub fn new(name: &str, x: &str, y: &str) -> Option<Device> {
        let (bx, by) = (unbase64url(x)?, unbase64url(y)?);
        if bx.len() != 32 || by.len() != 32 {
            return None;
        }
        let mut key = Vec::with_capacity(65);
        key.push(4);
        key.extend_from_slice(&bx);
        key.extend_from_slice(&by);
        Some(Device { id: device_id(&key), name: clean_name(name), x: x.to_owned(), y: y.to_owned(), key })
    }

    /// As app.contract keeps it: `"<id> <x> <y> <name>"`. A line whose id isn't its key's is
    /// refused, so an edited list can't name one phone with another's key.
    pub fn parse(line: &str) -> Option<Device> {
        let mut parts = line.splitn(4, ' ');
        let (id, x, y, name) = (parts.next()?, parts.next()?, parts.next()?, parts.next().unwrap_or(""));
        Device::new(name, x, y).filter(|d| d.id == id)
    }

    pub fn line(&self) -> String {
        format!("{} {} {} {}", self.id, self.x, self.y, self.name)
    }

    pub fn check(&self) -> String {
        check_digits(&self.key)
    }

    /// Whether `sig` (base64url of the raw 64-byte r‖s) signs `text` with this device's key.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn verify(&self, text: &str, sig: &str) -> bool {
        use ring::signature;
        let Some(sig) = unbase64url(sig).filter(|s| s.len() == 64) else { return false };
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, &self.key).verify(text.as_bytes(), &sig).is_ok()
    }

    /// The web build serves no phone.
    #[cfg(target_arch = "wasm32")]
    pub fn verify(&self, _text: &str, _sig: &str) -> bool {
        false
    }
}

/// A name as the Mac shows it: printable, trimmed, at most `NAME_MAX` characters.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(NAME_MAX).collect();
    match name.trim() {
        "" => "iPhone".into(),
        n => n.to_owned(),
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn device_id(key: &[u8]) -> String {
    hex(&sha256(key)[..8])
}

/// Six digits from the key, `"482 913"`, the same on the phone (remote/app.ts's `checkDigits`).
pub fn check_digits(key: &[u8]) -> String {
    let h = sha256(key);
    let n = u32::from_be_bytes([h[8], h[9], h[10], h[11]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// What a phone proves it knows the code with (remote/app.ts's `pairProof`).
pub fn pair_proof(code: &str, x: &str, y: &str) -> String {
    hex(&sha256(format!("magnitor-pair\n{code}\n{x}\n{y}").as_bytes()))
}

/// What a request's signature covers (remote/app.ts's `signedText`).
pub fn signed_text(challenge: &str, seq: u64, method: &str, path: &str, query: &str) -> String {
    format!("magnitor-v1\n{challenge}\n{seq}\n{method}\n{path}?{query}")
}

/// Whether two strings are equal, comparing every byte whatever differs first.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

pub fn unbase64url(s: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'-' | b'+' => Some(62),
        b'_' | b'/' => Some(63),
        _ => None,
    };
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(s.len() * 3 / 4), 0u32, 0);
    for c in s.trim_end_matches('=').bytes() {
        acc = (acc << 6) | u32::from(value(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// A random challenge, 16 bytes in hex.
#[cfg(not(target_arch = "wasm32"))]
pub fn challenge() -> Option<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(hex(&bytes))
}

#[cfg(target_arch = "wasm32")]
pub fn challenge() -> Option<String> {
    None
}

/// The `Authorization: Magnitor …` header's fields.
#[derive(Debug, PartialEq)]
pub struct Signed {
    pub id: String,
    pub ch: String,
    pub seq: u64,
    pub sig: String,
}

pub fn parse_signed(header: &str) -> Option<Signed> {
    let fields = header.strip_prefix("Magnitor ")?;
    let get = |name: &str| fields.split(',').filter_map(|f| f.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_owned());
    Some(Signed { id: get("id")?, ch: get("ch")?, seq: get("seq")?.parse().ok()?, sig: get("sig")? })
}

/// A paired phone's session: its challenge, the highest sequence number accepted, which of the
/// `WINDOW` below it were, and when it was last used.
#[derive(Debug)]
pub struct Session {
    pub challenge: String,
    pub top: u64,
    seen: u64,
    pub used: Instant,
}

impl Session {
    pub fn new(challenge: String, now: Instant) -> Session {
        Session { challenge, top: 0, seen: 0, used: now }
    }

    /// Whether `seq` may still be used: above the window's floor and not seen.
    pub fn fresh(&self, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        if seq > self.top {
            return true;
        }
        let back = self.top - seq;
        back < WINDOW && self.seen & (1 << back) == 0
    }

    /// Marks `seq` used (it must be `fresh`).
    pub fn accept(&mut self, seq: u64, now: Instant) {
        if seq > self.top {
            let shift = seq - self.top;
            self.seen = if shift >= WINDOW { 0 } else { self.seen << shift };
            self.top = seq;
            self.seen |= 1;
        } else {
            self.seen |= 1 << (self.top - seq);
        }
        self.used = now;
    }
}

/// Whether a peer is on the local network: a private or link-local IPv4 address, IPv6's
/// link-local or unique local ones (or IPv4's, mapped), or this Mac itself.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
        }
    }
}

/// Failures by address, and the addresses refused until a time.
#[derive(Default)]
pub struct Lockout {
    fails: HashMap<IpAddr, (u32, Instant)>,
    until: HashMap<IpAddr, Instant>,
}

impl Lockout {
    pub fn locked(&mut self, ip: IpAddr, now: Instant) -> bool {
        match self.until.get(&ip) {
            Some(&t) if now < t => true,
            Some(_) => {
                self.until.remove(&ip);
                false
            }
            None => false,
        }
    }

    /// One more failure from `ip`; whether it is now locked out.
    pub fn fail(&mut self, ip: IpAddr, now: Instant) -> bool {
        // An attacker with many addresses can't grow the map without end.
        if self.fails.len() > 1024 {
            self.fails.retain(|_, (_, at)| now.duration_since(*at) < FAIL_WINDOW);
        }
        let entry = self.fails.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= FAIL_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
        if entry.0 >= MAX_FAILS {
            self.fails.remove(&ip);
            self.until.insert(ip, now + LOCKOUT);
            return true;
        }
        false
    }

    pub fn clear(&mut self, ip: IpAddr) {
        self.fails.remove(&ip);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    pub fn base64url(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..=chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        out
    }

    /// A phone: its key pair and its coordinates as its JWK gives them.
    pub struct Phone {
        pair: EcdsaKeyPair,
        pub x: String,
        pub y: String,
    }

    impl Default for Phone {
        fn default() -> Phone {
            Phone::new()
        }
    }

    impl Phone {
        pub fn new() -> Phone {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
            let public = pair.public_key().as_ref();
            let (x, y) = (base64url(&public[1..33]), base64url(&public[33..65]));
            Phone { pair, x, y }
        }

        pub fn device(&self, name: &str) -> Device {
            Device::new(name, &self.x, &self.y).unwrap()
        }

        pub fn sign(&self, text: &str) -> String {
            base64url(self.pair.sign(&SystemRandom::new(), text.as_bytes()).unwrap().as_ref())
        }
    }

    #[test]
    fn a_device_is_known_by_its_key() {
        let phone = Phone::new();
        let d = phone.device("Ada’s iPhone");
        assert_eq!(d.id.len(), 16);
        assert_eq!(Device::parse(&d.line()), Some(d.clone()));
        // Another phone's id with this key is refused.
        let other = Phone::new().device("x");
        assert_eq!(Device::parse(&format!("{} {} {} Ada", other.id, phone.x, phone.y)), None);
        assert_eq!(Device::new("a", "AAAA", &phone.y), None);
        assert!(d.check().len() == 7 && d.check().as_bytes()[3] == b' ');
    }

    #[test]
    fn a_signature_verifies_only_its_text_and_key() {
        let phone = Phone::new();
        let d = phone.device("p");
        let text = signed_text("c0ffee", 3, "POST", "/v1/lock", "");
        let sig = phone.sign(&text);
        assert!(d.verify(&text, &sig));
        assert!(!d.verify(&signed_text("c0ffee", 4, "POST", "/v1/lock", ""), &sig));
        assert!(!Phone::new().device("q").verify(&text, &sig));
        assert!(!d.verify(&text, "AAAA"));
    }

    #[test]
    fn a_name_is_cleaned() {
        assert_eq!(clean_name("  Ada\u{7}’s\niPhone "), "Ada’siPhone");
        assert_eq!(clean_name(""), "iPhone");
        assert_eq!(clean_name(&"x".repeat(100)).len(), NAME_MAX);
    }

    #[test]
    fn reads_the_signed_header() {
        assert_eq!(parse_signed("Magnitor id=ab12,ch=c0ffee,seq=7,sig=Zm9v"), Some(Signed { id: "ab12".into(), ch: "c0ffee".into(), seq: 7, sig: "Zm9v".into() }));
        assert_eq!(parse_signed("Bearer ABCD-EFGH"), None);
        assert_eq!(parse_signed("Magnitor id=ab12,ch=c,seq=x,sig=Zm9v"), None);
    }

    #[test]
    fn a_sequence_number_is_used_once_within_the_window() {
        let now = Instant::now();
        let mut s = Session::new("c".into(), now);
        assert!(!s.fresh(0));
        for seq in [1, 3, 2] {
            assert!(s.fresh(seq), "{seq}");
            s.accept(seq, now);
            assert!(!s.fresh(seq), "{seq} again");
        }
        s.accept(100, now);
        assert!(s.fresh(99) && s.fresh(100 - WINDOW + 1));
        assert!(!s.fresh(100 - WINDOW) && !s.fresh(3));
    }

    #[test]
    fn only_local_peers() {
        for ok in ["192.168.1.20", "10.0.0.2", "172.16.4.1", "169.254.3.3", "127.0.0.1", "fe80::1", "fd12::4", "::1", "::ffff:192.168.1.2"] {
            assert!(is_local(ok.parse().unwrap()), "{ok}");
        }
        for no in ["8.8.8.8", "100.64.0.1", "2001:db8::1", "::ffff:8.8.8.8", "172.32.0.1"] {
            assert!(!is_local(no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn failures_lock_an_address_out() {
        let (now, ip): (Instant, IpAddr) = (Instant::now(), "192.168.1.9".parse().unwrap());
        let mut l = Lockout::default();
        for _ in 1..MAX_FAILS {
            assert!(!l.fail(ip, now));
        }
        assert!(!l.locked(ip, now));
        assert!(l.fail(ip, now));
        assert!(l.locked(ip, now + LOCKOUT - Duration::from_secs(1)));
        assert!(!l.locked(ip, now + LOCKOUT));
        // Failures far apart never add up.
        let mut l = Lockout::default();
        for i in 0..MAX_FAILS * 2 {
            assert!(!l.fail(ip, now + FAIL_WINDOW * i));
        }
    }

    #[test]
    fn base64url_round_trips() {
        for n in 0..40 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(unbase64url(&base64url(&bytes)).unwrap(), bytes);
        }
        assert_eq!(unbase64url("a b"), None);
    }
}

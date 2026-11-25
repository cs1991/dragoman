//! Time based one time passwords for batch accounts.
//!
//! An account line can carry a 32 character base32 authenticator secret as its
//! third field. Google shows a six digit code challenge for those accounts, and
//! generating the code locally avoids waiting for a mailbox message.
//!
//! SHA-1 and HMAC are implemented here so the runtime keeps its current
//! dependency set.

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const STEP_SECONDS: u64 = 30;
const DIGITS: u32 = 6;

/// Canonical uppercase base32 for a secret. Whitespace, dashes and padding are
/// dropped first, which covers secrets copied with spaces or lowercase letters.
pub fn normalize_secret(raw: &str) -> Option<String> {
    let compact: String = raw
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    let compact = compact.trim_end_matches('=').to_string();
    if compact.len() < 16 || !compact.bytes().all(|byte| BASE32.contains(&byte)) {
        return None;
    }
    Some(compact)
}

/// True when the field can be used as an authenticator secret.
pub fn is_secret(raw: &str) -> bool {
    normalize_secret(raw).is_some()
}

/// Six digit code for the wall clock.
pub fn current_code(secret: &str) -> Option<String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    code_at(secret, seconds)
}

/// Six digit code for a fixed time. Kept separate from `current_code` so the
/// RFC 6238 vectors can be tested without touching the clock.
pub fn code_at(secret: &str, unix_time: u64) -> Option<String> {
    let normalized = normalize_secret(secret)?;
    let key = base32_decode(&normalized)?;
    Some(hotp(&key, unix_time / STEP_SECONDS))
}

fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
        buffer &= if bits == 0 { 0 } else { (1u32 << bits) - 1 };
    }
    Some(output)
}

fn hotp(key: &[u8], counter: u64) -> String {
    let digest = hmac_sha1(key, &counter.to_be_bytes());
    let offset = usize::from(digest[19] & 0x0f);
    let binary = ((u32::from(digest[offset]) & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    let code = binary % 10u32.pow(DIGITS);
    format!("{code:0width$}", width = DIGITS as usize)
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&sha1(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend(block.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner_hash = sha1(&inner);

    let mut outer = Vec::with_capacity(84);
    outer.extend(block.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64).wrapping_mul(8);

    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let (chunks, _) = message.as_chunks::<64>();
    for chunk in chunks {
        let mut words = [0u32; 80];
        for (index, slot) in words.iter_mut().take(16).enumerate() {
            *slot = u32::from_be_bytes([
                chunk[index * 4],
                chunk[index * 4 + 1],
                chunk[index * 4 + 2],
                chunk[index * 4 + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut digest = [0u8; 20];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B uses the ASCII secret "12345678901234567890".
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn decodes_base32_and_matches_rfc_vectors() {
        assert_eq!(
            base32_decode(RFC_SECRET).unwrap(),
            b"12345678901234567890".to_vec()
        );
        assert_eq!(code_at(RFC_SECRET, 59).unwrap(), "287082");
        assert_eq!(code_at(RFC_SECRET, 1_111_111_109).unwrap(), "081804");
        assert_eq!(code_at(RFC_SECRET, 1_111_111_111).unwrap(), "050471");
        assert_eq!(code_at(RFC_SECRET, 1_234_567_890).unwrap(), "005924");
        assert_eq!(code_at(RFC_SECRET, 2_000_000_000).unwrap(), "279037");
    }

    #[test]
    fn normalizes_lowercase_and_spaced_secrets() {
        let normalized = normalize_secret("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq").unwrap();
        assert_eq!(normalized, RFC_SECRET);
        assert_eq!(code_at(&normalized, 59).unwrap(), "287082");
    }

    #[test]
    fn rejects_fields_that_are_not_base32() {
        assert!(normalize_secret("zerq#ASY2").is_none());
        assert!(normalize_secret("short").is_none());
        assert!(normalize_secret("hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh0").is_none());
    }
}

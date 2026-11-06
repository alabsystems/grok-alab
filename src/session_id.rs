// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh session id for `grok --session-id` (1.0.70: "Use a specific session UUID for a new
//! conversation").

/// A random (version 4, RFC 9562) UUID in lowercase 8-4-4-4-12 form, from the kernel's
/// entropy, or `None` when the kernel gives none.
#[must_use]
pub fn new_v4() -> Option<String> {
    let mut b = [0_u8; 16];
    // SAFETY: `getentropy` writes exactly `b.len()` (16, under its 256-byte limit) bytes into
    // the live local buffer it is given.
    let rc = unsafe { libc::getentropy(b.as_mut_ptr().cast(), b.len()) };
    if rc != 0 {
        return None;
    }
    Some(format_v4(b))
}

/// `bytes` as a version 4, variant 1 UUID.
#[must_use]
pub fn format_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_id_is_a_version_4_uuid_and_two_differ() {
        let a = new_v4().expect("entropy");
        let b = new_v4().expect("entropy");
        assert!(crate::lanes::is_session_uuid(&a), "{a}");
        assert_ne!(a, b);
        assert_eq!(a.as_bytes()[14], b'4', "{a}");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "{a}");
    }

    #[test]
    fn the_version_and_variant_bits_are_set_whatever_the_bytes() {
        assert_eq!(format_v4([0; 16]), "00000000-0000-4000-8000-000000000000");
        assert_eq!(
            format_v4([0xff; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
    }
}

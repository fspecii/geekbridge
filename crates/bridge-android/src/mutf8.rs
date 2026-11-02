// SPDX-License-Identifier: GPL-3.0-or-later
//! Check on the JVM's modified UTF-8 (JNI `GetStringUTFChars`) before it is decoded to UTF-8.
//!
//! Java strings are UTF-16 and may hold unpaired surrogates, which MUTF8 encodes as lone
//! three-byte sequences and no UTF-8 decoder can represent. The JNI layer's decoder would replace
//! them silently, so the layer refuses such strings first and the text that was validated is
//! always exactly the text Kotlin supplied.

/// True if `bytes` (modified UTF-8) contain a surrogate that is not half of a high+low pair.
///
/// Surrogates U+D800..=U+DFFF are encoded `ED A0..=BF 80..=BF`: high `ED A0..=AF xx`, low
/// `ED B0..=BF xx`. `0xED` is never a continuation byte, so scanning byte-wise cannot misread
/// the middle of another character. Other malformed bytes are not this function's concern.
pub fn has_unpaired_surrogate(bytes: &[u8]) -> bool {
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == 0xED {
            match bytes.get(i + 1) {
                // Lone low surrogate (a valid pair is consumed below as a unit).
                Some(0xB0..=0xBF) => return true,
                Some(0xA0..=0xAF) => {
                    let low_follows = matches!(
                        (
                            bytes.get(i + 2),
                            bytes.get(i + 3),
                            bytes.get(i + 4),
                            bytes.get(i + 5)
                        ),
                        (
                            Some(0x80..=0xBF),
                            Some(0xED),
                            Some(0xB0..=0xBF),
                            Some(0x80..=0xBF)
                        )
                    );
                    if !low_follows {
                        return true;
                    }
                    i += 6;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIR: [u8; 6] = [0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]; // U+1F600
    const HIGH: [u8; 3] = [0xED, 0xA0, 0xBD];
    const LOW: [u8; 3] = [0xED, 0xB8, 0x80];

    #[test]
    fn well_formed_strings_pass() {
        assert!(!has_unpaired_surrogate(b""));
        assert!(!has_unpaired_surrogate(b"plain ascii"));
        assert!(!has_unpaired_surrogate("héllo €".as_bytes()));
        assert!(!has_unpaired_surrogate(&PAIR));
        assert!(!has_unpaired_surrogate(&[b'a', 0xC0, 0x80, b'b'])); // encoded NUL
        // U+D7FF (ED 9F BF) and U+E000 (EE 80 80) flank the surrogate range.
        assert!(!has_unpaired_surrogate(&[
            0xED, 0x9F, 0xBF, 0xEE, 0x80, 0x80
        ]));
        let mut s = b"a".to_vec();
        s.extend_from_slice(&PAIR);
        s.extend_from_slice(&PAIR);
        s.push(b'z');
        assert!(!has_unpaired_surrogate(&s));
    }

    #[test]
    fn unpaired_surrogates_are_detected() {
        assert!(has_unpaired_surrogate(&HIGH));
        assert!(has_unpaired_surrogate(&LOW));
        let mut high_then_ascii = HIGH.to_vec();
        high_then_ascii.push(b'x');
        assert!(has_unpaired_surrogate(&high_then_ascii));
        let mut reversed = LOW.to_vec();
        reversed.extend_from_slice(&HIGH);
        assert!(has_unpaired_surrogate(&reversed));
        let mut two_highs = HIGH.to_vec();
        two_highs.extend_from_slice(&HIGH);
        assert!(has_unpaired_surrogate(&two_highs));
        let mut pair_then_low = PAIR.to_vec();
        pair_then_low.extend_from_slice(&LOW);
        assert!(has_unpaired_surrogate(&pair_then_low));
        let mut truncated = HIGH.to_vec();
        truncated.extend_from_slice(&[0xED, 0xB8]);
        assert!(has_unpaired_surrogate(&truncated));
    }
}

//! # Text — Utilidades de texto seguras con UTF-8
//!
//! Helpers que evitan panics por slicing en medio de un carácter
//! multi-byte. Compatible con el MSRV 1.85 del workspace (no usa
//! `str::floor_char_boundary`, estabilizado en Rust 1.91).

/// Trunca `s` a `max_bytes` bytes sin cortar un carácter UTF-8 a la mitad.
///
/// Devuelve el mayor prefijo válido de `s` con longitud ≤ `max_bytes`.
/// Nunca paniquea, incluso si el contenido tiene emojis, acentos, etc.
#[must_use]
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    let max = max_bytes.min(s.len());
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate_utf8("hello world", 5), "hello");
        assert_eq!(truncate_utf8("hello", 200), "hello");
        assert_eq!(truncate_utf8("", 10), "");
    }

    #[test]
    fn test_truncate_multibyte_no_panic() {
        let s = "ñandú 🦜 cacatúa éàîôü";
        // Ningún truncado a cualquier byte debe paniquear.
        for i in 0..=s.len() {
            let t = truncate_utf8(s, i);
            assert!(s.starts_with(t));
            assert!(t.len() <= i);
        }
        // El resultado nunca corta un char a la mitad.
        assert_eq!(truncate_utf8("ñ", 1), "");
        assert_eq!(truncate_utf8("ñ", 2), "ñ");
    }
}

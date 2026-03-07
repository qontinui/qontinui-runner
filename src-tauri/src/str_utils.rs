/// Safely truncate a string to at most `max_bytes` bytes, splitting on a UTF-8
/// character boundary.  Returns the full string unchanged when it already fits.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // floor_char_boundary is nightly-only; do it manually.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_within_limit() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn ascii_at_limit() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn ascii_over_limit() {
        assert_eq!(truncate_str("hello world", 5), "hello");
    }

    #[test]
    fn multibyte_boundary() {
        // U+00E9 is 2 bytes in UTF-8
        let s = "caf\u{00e9}!";
        // 'c'=1, 'a'=2, 'f'=3, '\u{00e9}'=3+2=5, '!'=6
        assert_eq!(truncate_str(s, 4), "caf");
        assert_eq!(truncate_str(s, 5), "caf\u{00e9}");
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_str("", 10), "");
    }

    #[test]
    fn zero_limit() {
        assert_eq!(truncate_str("hello", 0), "");
    }
}

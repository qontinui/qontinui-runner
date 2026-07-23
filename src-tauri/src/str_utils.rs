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

/// Safely truncate a string to at most `max_bytes` bytes on a UTF-8 character
/// boundary, appending a `...` marker only when the string was actually
/// shortened.  Returns an owned `String` since the marker has to be appended.
pub fn truncate_str_ellipsis(s: &str, max_bytes: usize) -> String {
    let truncated = truncate_str(s, max_bytes);
    if truncated.len() == s.len() {
        truncated.to_string()
    } else {
        format!("{truncated}...")
    }
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

    #[test]
    fn ellipsis_appended_when_shortened() {
        assert_eq!(truncate_str_ellipsis("hello world", 5), "hello...");
    }

    #[test]
    fn ellipsis_omitted_when_exactly_fitting() {
        assert_eq!(truncate_str_ellipsis("hello", 5), "hello");
        assert_eq!(truncate_str_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn ellipsis_empty_string() {
        assert_eq!(truncate_str_ellipsis("", 10), "");
        assert_eq!(truncate_str_ellipsis("", 0), "");
    }

    #[test]
    fn ellipsis_multibyte_straddling_cut() {
        // The em-dash U+2014 occupies bytes 5..8; cutting at 6 or 7 would land
        // inside it, so the cut snaps back down to byte 5.
        let s = "hello\u{2014}world";
        assert_eq!(truncate_str_ellipsis(s, 6), "hello...");
        assert_eq!(truncate_str_ellipsis(s, 7), "hello...");
        assert_eq!(truncate_str_ellipsis(s, 8), "hello\u{2014}...");
    }
}

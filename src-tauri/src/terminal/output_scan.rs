//! Shared terminal-text scanning primitives.
//!
//! Text normalization used by the grid-scan watchers ([`super::usage_limit`] and
//! [`super::auto_response`]). Both read a terminal's rendered VT grid and match
//! patterns against it, so they share the same lowercase/whitespace-collapse
//! normalization. It lives here so neither watcher owns the other.
//!
//! (This module previously also held a byte-level ANSI stripper + rolling
//! window for raw-PTY-stream hooks. Both watchers now scan the VT-parsed grid
//! instead — which is already ANSI-resolved on-screen text — so the stripper is
//! gone and only the normalizer remains.)

/// Lowercase + collapse whitespace runs to single spaces, so TUI padding /
/// line wraps inside a message don't break the substring (or regex) match.
pub(crate) fn normalize(window: &str) -> String {
    let mut out = String::with_capacity(window.len());
    let mut last_space = false;
    for c in window.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize("USAGE   LIMIT\n REACHED"), "usage limit reached");
        assert_eq!(normalize("  Hello\t\tWorld  "), " hello world ");
    }
}

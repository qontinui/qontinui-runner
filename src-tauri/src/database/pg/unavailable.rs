//! The fallbacks a `database/pg` read may still take — EXCEPT for an outage.
//!
//! Plan `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`,
//! Phase 6: a down database never reads as empty data. Each method of
//! [`UnlessUnavailable`] mirrors the `Result` fallback it replaces
//! (`unwrap_or_default`, `unwrap_or`, `unwrap_or_else`) and keeps it for every
//! other failure, but an error carrying the `DATABASE_UNAVAILABLE:` token
//! (`qontinui_runner_lib::pg_circuit::is_db_unavailable`) is handed back
//! unchanged. The caller then propagates it (`?`, a 503) or skips its tick.
//!
//! Known gap: the very first failing query of an outage (the one whose
//! connect failure trips the breaker), and a connection lost mid-query, are
//! formatted by `pg_err` without the token, so that one call still takes the
//! old fallback; every call after the trip carries the token.
//!
//! The bare fallbacks over a PG read are ratcheted by
//! [`super::empty_census`]; these spellings are the sanctioned replacement.

use qontinui_runner_lib::pg_circuit::is_db_unavailable;

/// Outage-aware read fallbacks for the `Result<T, String>` the `database/pg`
/// method layer returns.
pub trait UnlessUnavailable<T> {
    /// `unwrap_or_default()`, except an outage stays an `Err`.
    fn default_unless_unavailable(self) -> Result<T, String>
    where
        T: Default;
    /// `unwrap_or(fallback)`, except an outage stays an `Err`.
    fn or_unless_unavailable(self, fallback: T) -> Result<T, String>;
    /// `unwrap_or_else(f)`, except an outage stays an `Err` (and `f` never runs).
    fn or_else_unless_unavailable<F: FnOnce(String) -> T>(self, f: F) -> Result<T, String>;
}

impl<T> UnlessUnavailable<T> for Result<T, String> {
    fn default_unless_unavailable(self) -> Result<T, String>
    where
        T: Default,
    {
        self.or_else_unless_unavailable(|_| T::default())
    }

    fn or_unless_unavailable(self, fallback: T) -> Result<T, String> {
        self.or_else_unless_unavailable(|_| fallback)
    }

    fn or_else_unless_unavailable<F: FnOnce(String) -> T>(self, f: F) -> Result<T, String> {
        match self {
            Ok(v) => Ok(v),
            Err(e) if is_db_unavailable(&e) => Err(e),
            Err(e) => Ok(f(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::pg_circuit::DB_UNAVAILABLE_TOKEN;

    fn down() -> Result<Vec<u8>, String> {
        Err(format!("PG pool error: {DB_UNAVAILABLE_TOKEN} unreachable"))
    }

    fn other() -> Result<Vec<u8>, String> {
        Err("PG query: relation missing".into())
    }

    #[test]
    fn pg_error_to_empty_census_helpers_keep_the_fallback_except_for_an_outage() {
        assert_eq!(
            Ok(vec![1]),
            Ok::<_, String>(vec![1]).default_unless_unavailable()
        );
        assert_eq!(Ok(vec![]), other().default_unless_unavailable());
        assert!(down().default_unless_unavailable().is_err());

        assert_eq!(Ok(vec![7]), other().or_unless_unavailable(vec![7]));
        assert!(down().or_unless_unavailable(vec![7]).is_err());

        let seen = other().or_else_unless_unavailable(|e| e.into_bytes());
        assert_eq!(Ok(b"PG query: relation missing".to_vec()), seen);
        let mut called = false;
        let r = down().or_else_unless_unavailable(|_| {
            called = true;
            vec![]
        });
        assert!(r.is_err_and(|e| is_db_unavailable(&e)));
        assert!(!called, "the fallback must not run for an outage");
    }
}

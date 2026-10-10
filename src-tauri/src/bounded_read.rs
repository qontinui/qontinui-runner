//! The runner's half of the bounded-read contract: every caller-supplied
//! `limit` resolves through a NAMED, CLAMPED [`ReadLimit`], and every
//! keyset walk mints and decodes its cursor through the shared
//! `qontinui_types::page` codec.
//!
//! Plan `2026-09-05-every-bounded-read-is-a-page-that-reads-as-a-corpus`,
//! Phase 6 (the runner's long tail). Before this module the runner spelled
//! "how many rows?" as `limit.unwrap_or(N)` at 75 call sites, with ten
//! distinct magic defaults and, at most of them, no ceiling at all, so a
//! caller asking for `limit=10000000` got exactly that and a caller asking
//! for nothing got a page it could not tell from the corpus.
//!
//! Two pieces:
//!
//! - [`ReadLimit`] — the default and the ceiling of one read, declared once as
//!   a `const`, so the number the statement applies and the number the
//!   envelope reports can never disagree. `resolve` clamps to `1..=max`.
//! - The keyset helpers ([`row_position`], [`decode_cursor`], [`keyset_page`])
//!   — the runner-side glue between a `(timestamp, text id)` row and the shared
//!   codec, which every runner keyset walk shares instead of re-deriving.
//!
//! The census test (`tests` below) is the gate: a NEW `limit.unwrap_or(` fails
//! the build, and every [`ReadLimit`] call site must appear in [`TRIAGE`] with
//! its door class, so an agent-facing read cannot land without a decision
//! about its disclosure.

use chrono::{DateTime, Utc};
use qontinui_types::page::{CursorError, KeysetPosition, Page, ScopeFingerprint, SortKey};

/// The Phase 6 triage of every `limit` site — the census test's table.
#[cfg(test)]
mod triage;

// ============================================================================
// ReadLimit
// ============================================================================

/// The default and the ceiling of one bounded read.
///
/// Declare one per read as a `const` beside the code that applies it, and
/// resolve the caller's `limit` through it. The ceiling is the cap the read
/// actually applies, which is the number a disclosing door reports as `limit`
/// (`BoundedReadMeta::limit`); a `limit` above it is clamped, never refused,
/// and a `limit` of zero or below is raised to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimit {
    default: i64,
    max: i64,
}

impl ReadLimit {
    /// A read that serves `default` rows when the caller names no `limit` and
    /// never more than `max`. Evaluated at compile time for a `const`, so a
    /// default above its own ceiling (or below one) does not build.
    pub const fn new(default: i64, max: i64) -> Self {
        assert!(default >= 1, "ReadLimit default must be at least 1");
        assert!(
            default <= max,
            "ReadLimit default must not exceed its ceiling"
        );
        Self { default, max }
    }

    /// The cap to apply: the caller's `limit` (or the default), clamped to
    /// `1..=max`, in the caller's own integer type.
    pub fn resolve<N: LimitInt>(self, requested: Option<N>) -> N {
        let wanted = requested.map_or(self.default, LimitInt::to_i64);
        N::from_i64(wanted.clamp(1, self.max))
    }
}

/// The integer types a `limit` arrives in. Conversions saturate, so an
/// out-of-range request clamps rather than wrapping.
pub trait LimitInt: Copy {
    fn to_i64(self) -> i64;
    fn from_i64(value: i64) -> Self;
}

macro_rules! limit_int {
    ($($t:ty),*) => {$(
        impl LimitInt for $t {
            fn to_i64(self) -> i64 {
                i64::try_from(self).unwrap_or(i64::MAX)
            }
            fn from_i64(value: i64) -> Self {
                <$t>::try_from(value).unwrap_or(<$t>::MAX)
            }
        }
    )*};
}

limit_int!(u16, u32, u64, usize, i32, i64);

// ============================================================================
// Keyset helpers over the shared codec
// ============================================================================

/// The keyset position of a served row whose id is TEXT. A runner statement
/// compares `id` as text, so the position is exact only when the stored id is
/// the canonical lowercase-hyphenated form `Uuid::to_string` renders — every
/// row the runner inserts is (`Uuid::new_v4().to_string()`). Any other id
/// cannot be resumed after, and saying so beats minting a cursor that skips
/// rows.
pub fn row_position(id: &str, at: DateTime<Utc>) -> Result<KeysetPosition, String> {
    match uuid::Uuid::parse_str(id) {
        Ok(uuid) if uuid.to_string() == id => Ok(KeysetPosition { at, id: uuid }),
        _ => Err(format!(
            "row id {id:?} is not a canonical lowercase uuid, so the walk cannot resume after it"
        )),
    }
}

/// Decode a caller-supplied `cursor` against the read's scope. `None` is the
/// first page; a token this read did not mint is a [`CursorError`] whose
/// `code()` is `cursor_malformed` and whose `refusal(surface)` names the
/// parameter — never a clamp to the nearest position, which would resume the
/// walk with rows silently missing.
pub fn decode_cursor<K: SortKey>(
    scope: &ScopeFingerprint<K>,
    cursor: Option<&str>,
) -> Result<Option<KeysetPosition>, CursorError> {
    cursor.map(|token| scope.decode(token)).transpose()
}

/// The `(timestamp, id)` a SQL keyset filter binds: `None`s on the first page
/// (the statement spells `($n::timestamptz IS NULL OR (key, id) < ($n, $m))`),
/// the decoded position after it.
pub fn keyset_binds(after: Option<KeysetPosition>) -> (Option<DateTime<Utc>>, Option<String>) {
    match after {
        Some(pos) => (Some(pos.at), Some(pos.id.to_string())),
        None => (None, None),
    }
}

/// Build a [`Page`] from a keyset fetch of up to `limit + 1` rows (the extra
/// row is the has-more probe, read by the SAME statement as the page).
///
/// `exact_total` is the match count a separate statement measured over the
/// whole filtered set — pass it on the FIRST page only (a later page starts
/// mid-walk, so a whole-set count is not its bound). When it agrees with the
/// probe the page is `exact`; when it does not (a writer appended rows between
/// the two statements) the probe wins and the page is `at_least`. Without a
/// count the page is `at_least` while rows remain and `complete` on the last.
///
/// The next cursor is minted from the LAST KEPT row, never the probe, and a
/// truncated page whose last kept row has no resumable position is an error
/// rather than a cursor that would skip rows.
pub fn keyset_page<T, K: SortKey>(
    mut rows: Vec<T>,
    limit: i64,
    exact_total: Option<i64>,
    scope: &ScopeFingerprint<K>,
    position_of: impl Fn(&T) -> Result<KeysetPosition, String>,
) -> Result<Page<T>, String> {
    let limit = limit.max(1);
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = rows.len() > keep;
    if has_more {
        if let Some(last_kept) = rows.get(keep - 1) {
            position_of(last_kept)?;
        }
    }
    let cursor_of = |last: &T| {
        position_of(last)
            .map(|pos| scope.encode(pos))
            .unwrap_or_default()
    };
    Ok(
        match exact_total.filter(|total| (*total > limit) == has_more) {
            Some(total) => {
                rows.truncate(keep);
                Page::from_window_count(rows, limit, total, cursor_of)
            }
            None => Page::from_probe(rows, limit, cursor_of),
        },
    )
}

#[cfg(test)]
mod tests;

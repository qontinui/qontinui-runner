//! The one fold over a set of reflection fixes' verdicts.
//!
//! Every reader that reports how well fixes work counts by verdict in SQL
//! ([`COUNT_COLUMNS`]) and folds the five counts through
//! [`FixVerdictCounts`], so that no reader invents its own ratio. A fix
//! nobody judged (`effectiveness IS NULL`, or a value outside the four
//! verdicts) is `unjudged`; `inconclusive` is a verdict that judged nothing.
//! Neither is in the denominator of a rate, and a scope with nothing judged
//! has no rate (`None`), never 0 and never 1.

/// `COUNT(*) FILTER` columns, in the order [`FixVerdictCounts::from_row`]
/// reads them: effective, ineffective, caused_regression, inconclusive,
/// unjudged. Put it in a `SELECT` over a table or alias that exposes an
/// unqualified `effectiveness` column.
pub const COUNT_COLUMNS: &str = "\
COUNT(*) FILTER (WHERE effectiveness = 'effective'), \
COUNT(*) FILTER (WHERE effectiveness = 'ineffective'), \
COUNT(*) FILTER (WHERE effectiveness = 'caused_regression'), \
COUNT(*) FILTER (WHERE effectiveness = 'inconclusive'), \
COUNT(*) FILTER (WHERE effectiveness IS NULL OR effectiveness NOT IN \
('effective', 'ineffective', 'caused_regression', 'inconclusive'))";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FixVerdictCounts {
    pub effective: i64,
    pub ineffective: i64,
    pub caused_regression: i64,
    pub inconclusive: i64,
    pub unjudged: i64,
}

impl FixVerdictCounts {
    /// Read the five [`COUNT_COLUMNS`] starting at column `first`. A column
    /// that cannot be decoded is an error, never a 0.
    pub fn from_row(row: &tokio_postgres::Row, first: usize) -> Result<Self, String> {
        let col = |offset: usize| -> Result<i64, String> {
            row.try_get::<_, i64>(first + offset)
                .map_err(|e| format!("fix verdict count column {}: {}", first + offset, e))
        };
        Ok(Self {
            effective: col(0)?,
            ineffective: col(1)?,
            caused_regression: col(2)?,
            inconclusive: col(3)?,
            unjudged: col(4)?,
        })
    }

    /// Every fix counted, judged or not.
    pub fn total(&self) -> i64 {
        self.judged() + self.inconclusive + self.unjudged
    }

    /// Fixes with a verdict that says whether they worked. Inconclusive and
    /// unjudged are excluded.
    pub fn judged(&self) -> i64 {
        self.effective + self.ineffective + self.caused_regression
    }

    /// The share of judged fixes that were effective, or `None` when
    /// nothing is judged.
    pub fn effective_rate(&self) -> Option<f64> {
        let judged = self.judged();
        if judged > 0 {
            Some(self.effective as f64 / judged as f64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(e: i64, i: i64, r: i64, inc: i64, u: i64) -> FixVerdictCounts {
        FixVerdictCounts {
            effective: e,
            ineffective: i,
            caused_regression: r,
            inconclusive: inc,
            unjudged: u,
        }
    }

    #[test]
    fn effective_rate_over_literal_counts() {
        assert_eq!(counts(0, 0, 0, 0, 0).effective_rate(), None);
        assert_eq!(counts(0, 0, 0, 0, 5).effective_rate(), None, "all unjudged");
        assert_eq!(
            counts(0, 0, 0, 3, 0).effective_rate(),
            None,
            "inconclusive only"
        );
        assert_eq!(counts(0, 0, 0, 3, 2).effective_rate(), None);
        assert_eq!(counts(1, 0, 0, 0, 0).effective_rate(), Some(1.0));
        assert_eq!(counts(1, 0, 0, 0, 9).effective_rate(), Some(1.0));
        assert_eq!(counts(1, 1, 2, 0, 0).effective_rate(), Some(0.25));
        assert_eq!(counts(0, 1, 0, 0, 0).effective_rate(), Some(0.0));
    }

    #[test]
    fn judged_and_total_exclude_and_include_the_right_buckets() {
        let c = counts(1, 2, 3, 4, 5);
        assert_eq!(c.judged(), 6);
        assert_eq!(c.total(), 15);
    }

    #[test]
    fn the_sql_columns_name_each_verdict_once() {
        for verdict in ["effective", "ineffective", "caused_regression", "inconclusive"] {
            assert!(COUNT_COLUMNS.contains(&format!("effectiveness = '{verdict}'")));
        }
        assert_eq!(COUNT_COLUMNS.matches("COUNT(*) FILTER").count(), 5);
    }
}

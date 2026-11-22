//! Test-only schema oracle: what the vendored pg_dump declares, so an inline
//! SQL string can be held to the columns that exist.

/// The vendored pg_dump of the alembic-managed schema. `schema-pg-sql-fresh`
/// CI keeps it honest, which is what makes it usable as an oracle here.
pub(crate) const GENERATED_SCHEMA: &str = include_str!("../../../schema.pg.sql.generated");

/// Column names declared by a `CREATE TABLE <table> (...)` block.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub(crate) fn declared_columns(table: &str) -> Vec<String> {
    let header = format!("CREATE TABLE {} (\n", table);
    let start = GENERATED_SCHEMA
        .find(&header)
        .unwrap_or_else(|| panic!("{} not found in schema.pg.sql.generated", table))
        + header.len();
    let len = GENERATED_SCHEMA[start..]
        .find("\n);")
        .unwrap_or_else(|| panic!("unterminated CREATE TABLE for {}", table));
    GENERATED_SCHEMA[start..start + len]
        .lines()
        .filter_map(|line| line.trim().split_whitespace().next())
        .map(|tok| tok.trim_end_matches(',').to_string())
        .filter(|tok| {
            !tok.is_empty()
                && !matches!(
                    tok.as_str(),
                    "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN"
                )
        })
        .collect()
}

/// Every identifier written as `<alias>.<ident>` in `sql`.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub(crate) fn aliased_identifiers(sql: &str, alias: &str) -> Vec<String> {
    let needle = format!("{}.", alias);
    let bytes = sql.as_bytes();
    let mut found = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = sql[cursor..].find(&needle) {
        let at = cursor + offset;
        // Reject a match inside a longer word (e.g. `abc.` in `xabc.`).
        let standalone =
            at == 0 || !matches!(bytes[at - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
        let ident: String = sql[at + needle.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if standalone && !ident.is_empty() {
            found.push(ident);
        }
        cursor = at + needle.len();
    }
    found
}

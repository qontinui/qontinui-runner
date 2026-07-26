//! Page-slug conversions shared by every spec-id producer and consumer.
//!
//! Three call sites need the pathname ↔ spec-id mapping and must agree on it:
//!
//! - [`crate::state_discovery::capture`] labels each co-occurrence observation
//!   with the page it was captured on.
//! - [`crate::spec_api::proposals`] compares on-disk spec ids against observed
//!   pathnames to find un-spec'd pages.
//! - [`crate::workflow_generation::spec_authoring`] resolves the page whose
//!   states it is authoring.
//!
//! They previously kept two independent copies, and the copies disagreed on
//! the index page: `pathname_to_spec_id("/")` produced `root` while
//! `spec_id_to_pathname` only recognised `index`, so `root` round-tripped to
//! `/root` — a page that does not exist. That was latent only because nothing
//! labelled observations yet; the index case is the single most common
//! pathname, so it would have broken as soon as labelling shipped. Keeping
//! both directions in one module makes the round-trip property testable, and
//! `spec_authoring` is feature-gated while capture is not, so a shared
//! ungated home is also a build requirement.

/// Slugify a pathname into a spec id.
///
/// Non-alphanumeric runs collapse to a single `-`; leading/trailing hyphens
/// are trimmed; the result is lowercased. An empty result (i.e. the index
/// page) becomes `root`.
pub(crate) fn pathname_to_spec_id(pathname: &str) -> String {
    let mut out = String::with_capacity(pathname.len());
    let mut last_was_hyphen = true; // suppress leading hyphens
    for ch in pathname.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            None
        };
        match mapped {
            Some(c) => {
                out.push(c);
                last_was_hyphen = false;
            }
            None => {
                if !last_was_hyphen {
                    out.push('-');
                    last_was_hyphen = true;
                }
            }
        }
    }
    // Trim trailing hyphen (leading is suppressed by `last_was_hyphen` init).
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        INDEX_SPEC_ID.to_string()
    } else {
        out
    }
}

/// The spec id [`pathname_to_spec_id`] emits for the index pathname (`/`).
pub(crate) const INDEX_SPEC_ID: &str = "root";

/// Spec ids that denote the index page. `root` is what
/// [`pathname_to_spec_id`] emits; `index` is the older on-disk convention and
/// is still accepted so an existing `specs/pages/index/` keeps resolving.
const INDEX_ALIASES: [&str; 2] = [INDEX_SPEC_ID, "index"];

/// Recover the pathname a spec id was slugged from.
///
/// Inverse of [`pathname_to_spec_id`] for ids that round-trip cleanly.
/// Returns `None` for shapes that could not have been produced by it —
/// uppercase, doubled hyphens (which would collapse on re-slug), or
/// leading/trailing hyphens.
///
/// Note this cannot recover which separators were originally slashes versus
/// other punctuation; `-` is always expanded back to `/`.
pub(crate) fn spec_id_to_pathname(spec_id: &str) -> Option<String> {
    if spec_id.is_empty() {
        return None;
    }
    if INDEX_ALIASES.contains(&spec_id) {
        return Some("/".to_string());
    }
    if spec_id.chars().any(|c| c.is_uppercase())
        || spec_id.contains("--")
        || spec_id.starts_with('-')
        || spec_id.ends_with('-')
    {
        return None;
    }
    Some(format!("/{}", spec_id.replace('-', "/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathname_to_spec_id_basic() {
        assert_eq!(pathname_to_spec_id("/account/billing"), "account-billing");
        assert_eq!(pathname_to_spec_id("/"), "root");
        assert_eq!(pathname_to_spec_id(""), "root");
        assert_eq!(pathname_to_spec_id("/Foo/Bar/"), "foo-bar");
        assert_eq!(pathname_to_spec_id("/foo--bar//baz"), "foo-bar-baz");
        assert_eq!(pathname_to_spec_id("/settings/general"), "settings-general");
        // Idempotent on canonical form.
        let once = pathname_to_spec_id("/Account/Billing/");
        let twice = pathname_to_spec_id(&format!("/{}", once));
        assert_eq!(once, twice);
    }

    #[test]
    fn spec_id_to_pathname_simple() {
        assert_eq!(
            spec_id_to_pathname("account-billing").as_deref(),
            Some("/account/billing")
        );
        assert_eq!(spec_id_to_pathname("home").as_deref(), Some("/home"));
        assert_eq!(spec_id_to_pathname(""), None);
        // Reject non-roundtrippable shapes.
        assert_eq!(spec_id_to_pathname("Account-Billing"), None);
        assert_eq!(spec_id_to_pathname("-leading"), None);
        assert_eq!(spec_id_to_pathname("trailing-"), None);
        assert_eq!(spec_id_to_pathname("double--dash"), None);
    }

    #[test]
    fn both_index_spellings_resolve_to_root_pathname() {
        // The bug this module exists to close: `root` is what the slugger
        // emits, `index` is the older on-disk name, and both must map back to
        // `/` rather than `/root` and `/index`.
        assert_eq!(spec_id_to_pathname("root").as_deref(), Some("/"));
        assert_eq!(spec_id_to_pathname("index").as_deref(), Some("/"));
    }

    #[test]
    fn index_round_trips() {
        let id = pathname_to_spec_id("/");
        assert_eq!(spec_id_to_pathname(&id).as_deref(), Some("/"));
    }

    #[test]
    fn round_trip_holds_for_canonical_ids() {
        for path in ["/account/billing", "/settings/general", "/a/b/c", "/"] {
            let id = pathname_to_spec_id(path);
            let back = spec_id_to_pathname(&id).expect("canonical id round-trips");
            assert_eq!(
                pathname_to_spec_id(&back),
                id,
                "re-slugging the recovered pathname must be stable for {path}"
            );
        }
    }
}

//! Step-level hardener rules.
//!
//! Each sub-module implements exactly one [`HardenRule`]. Rules are
//! registered in [`default_rules`] in the order they must be applied —
//! which mirrors the historic ordering inside
//! `sanitize_commands_in_steps` so behavior is bit-identical.
//!
//! The only rule that holds the `working_directory` capability lives in
//! `working_directory_preserved.rs` (which currently does nothing but
//! exists to demonstrate the pattern). Every other rule is snapshot-and-
//! reverted by the engine if it touches `working_directory`.

use super::rule::HardenRule;

pub mod ensure_command_mode;
pub mod fix_bash_negation;
pub mod fix_curl_sf_piped;
pub mod fix_health_check_assertion;
pub mod flatten_retry_object;
pub mod quote_curl_ampersand_urls;
pub mod replace_jq_with_python;
pub mod replace_python_fstring;
pub mod working_directory_preserved;

// Shared command-mutation helpers used by rules that rewrite the
// `command` / `shell_command` string.
pub(crate) mod common;

/// Build the canonical rule set in the order `sanitize_commands_in_steps`
/// applied them historically. Changing this order changes semantics.
pub fn default_rules() -> Vec<Box<dyn HardenRule>> {
    vec![
        Box::new(ensure_command_mode::EnsureCommandMode),
        Box::new(flatten_retry_object::FlattenRetryObject),
        Box::new(replace_jq_with_python::ReplaceJqWithPython),
        Box::new(replace_python_fstring::ReplacePythonFstring),
        Box::new(fix_bash_negation::FixBashNegation),
        Box::new(fix_curl_sf_piped::FixCurlSfInPiped),
        Box::new(fix_health_check_assertion::FixHealthCheckAssertion),
        Box::new(quote_curl_ampersand_urls::QuoteCurlAmpersandUrls),
        // Only rule that may mutate working_directory. Runs last so its
        // capability is granted only after all other rules have seen the
        // step (and been reverted if they tried to touch the field).
        Box::new(working_directory_preserved::WorkingDirectoryPreserved),
    ]
}

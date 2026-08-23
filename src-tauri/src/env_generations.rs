//! Env **generations** — the same variable at three different ages, and the
//! divergence between them
//! (plan `2026-08-20-effective-config-provenance-and-env-generation`, Phase 3).
//!
//! # The question this module answers
//!
//! "I set the env var. Why did nothing happen?"
//!
//! Because there is no such thing as *the* environment of a runner session.
//! There are at least three, each frozen at a different moment:
//!
//! 1. **The runner's own process env.** Frozen when the runner process started.
//!    Every ad-hoc `std::env::var(…)` call in the runtime reads THIS — so it
//!    only changes on a runner restart.
//! 2. **The launch snapshot.** `launch_env::RunnerLaunchEnv::read()` is called
//!    exactly once, in `main()`, and every consumer pulls the typed value from
//!    there. It is therefore at most as fresh as (1), and can be *staler* if
//!    anything mutates the process env after boot.
//! 3. **What a PTY child gets.** `portable_pty::CommandBuilder` seeds its env
//!    map from `std::env::vars_os()` **and then, on Windows, re-reads the HKLM
//!    and HKCU `Environment` registry keys OVER those entries**
//!    (`portable-pty`'s `get_base_env`; the same fact
//!    [`crate::terminal`]`::scrub_credential_env_pty` documents for the scrub).
//!    So a terminal opened *now* can see a value the runner process has never
//!    held — and a terminal opened an hour ago holds the value from an hour ago,
//!    forever.
//!
//! Claude's Bash-tool grandchildren inherit their PTY's frozen copy, which puts
//! an operator's flag flip **three restarts deep**: the runner must restart, the
//! terminal must be re-opened, and only then does a tool call see it.
//!
//! `terminal/session.rs` already states this in prose — *"a terminal started
//! before the operator flips the flag keeps the value it was spawned with. That
//! is NOT a regression"*. Prose in a source file is not an answer an operator
//! can reach. **This module turns it into a line of output**: which variables
//! differ between generations, in which direction, and what that implies.
//!
//! # Redaction: withheld at the MODEL layer, never at the renderer
//!
//! The session env is known to carry plaintext passwords (three of them are
//! enumerated in [`crate::terminal`]'s `CREDENTIAL_VALUE_ENV_VARS`; the fleet
//! carries more). A config dump that leaks one is worse than no dump, so the
//! rule here is structural rather than textual:
//!
//! - every value is classified **at ingestion** by [`EnvVarReading::classify`];
//! - a credential-classed value produces [`EnvValue::Withheld`], **which has no
//!   field capable of holding the value**. It is dropped before any renderer,
//!   any `serde`, any log, ever sees it.
//!
//! ## Why not just run `session::redact::redact_secrets` over the output?
//!
//! Two independent reasons, either of which is disqualifying:
//!
//! 1. **Shape.** That module's `SECRET_RE` fires on `key[=:]value` *adjacency*.
//!    This report renders aligned columns and pipe tables —
//!    `| POSTGRES_PASSWORD | hunter2 | G1 |` has neither separator between the
//!    key and the value, and the regex matches **nothing**. A redaction pass
//!    that only ever saw `key: value` fixtures passes vacuously against exactly
//!    the shape this module emits. So the regex's FAILURE on that shape is
//!    asserted as an explicit negative control, in
//!    `config_report_cmd::tests::config_report_env_table_defeats_redact_secrets_but_not_the_classifier`
//!    (bin-side, because `session` is a BIN-only module this crate cannot
//!    call); the lib-side twin
//!    `env_generations_table_render_never_carries_a_credential_value` asserts
//!    the other half — that the value was never in a cell to begin with.
//! 2. **Status.** `session/redact.rs` says of itself, in its own header:
//!    *"Defense in depth, NOT a security boundary … a determined leak
//!    (multi-line secrets, base64 blobs, custom formats) still gets through …
//!    redaction is a courtesy backstop."* Gating a deliberate,
//!    credential-adjacent dump on a self-declared courtesy backstop is unsound.
//!    Withholding structurally cannot leak; pattern-matching rendered text can,
//!    and its own author says so.
//!
//! `redact_secrets` remains a legitimate SECOND pass over rendered text. It is
//! the braces, never the belt.
//!
//! ## Over-withholding is the correct failure direction
//!
//! [`classify_env_var`] withholds on the NAME (a credential-class token), on
//! the VALUE's shape (a JWT, a GitHub/OpenAI/AWS/Slack key prefix, a PEM block,
//! a long mixed-case high-entropy blob, or a URL with a password in its
//! userinfo), or on the two JOINTLY (a `*_URL`/`*_URI`/`*_DSN` name whose value
//! carries userinfo at all). A variable that is merely *named*
//! confusingly — `QONTINUI_KEYBOARD_DELAY` — is withheld, and that is fine: the
//! report states how many variables were withheld, so a reader can see that the
//! rows exist. The opposite error prints a password.
//!
//! # Comparing values we deliberately never kept
//!
//! Divergence has to answer "did this credential CHANGE between generations?"
//! without ever holding two credential values to compare. It does that with a
//! **per-run keyed fingerprint** ([`EnvFingerprinter`]): a `RandomState`-keyed
//! SipHash, freshly keyed on every report run, rendered as 8 hex characters.
//! Equal fingerprints inside one report mean equal values; the key never leaves
//! the process, so the digest is meaningless to anyone holding only the report —
//! it cannot be dictionary-attacked across runs the way a bare `sha256` of a
//! human-chosen password could.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;

use chrono::{DateTime, SecondsFormat, Utc};

// ===========================================================================
// Classification — the security-critical half.
// ===========================================================================

/// Name tokens that mark a variable as credential-bearing.
///
/// Matched as a **substring** of the upper-cased name, not as a word: a
/// credential variable is as likely to be `MY_APP_TOKEN_V2` as `TOKEN`, and the
/// cost of a false positive here is one withheld diagnostic line while the cost
/// of a false negative is a printed password.
///
/// `PAT` is deliberately **not** in this list — it is a substring of `PATH`,
/// which is neither a credential nor a variable anyone can afford to have
/// hidden. It is matched separately as the documented `*_PAT` suffix by
/// [`name_is_credential`].
pub const CREDENTIAL_NAME_TOKENS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "KEY",
    "JWT",
    "BEARER",
    "CREDENTIAL",
    "AUTH",
    "SESSION_ID",
    "SESSIONID",
    "NONCE",
];

/// Value prefixes that identify a credential regardless of the variable's name.
///
/// These are the shapes that show up in this fleet's environments: JWTs, GitHub
/// tokens and fine-grained PATs, OpenAI/Anthropic-style keys, Slack tokens, AWS
/// access-key ids, and inline PEM material.
pub const CREDENTIAL_VALUE_PREFIXES: &[&str] = &[
    "eyJ",         // JWT header `{"…` base64url-encoded
    "gho_",        // GitHub OAuth
    "ghp_",        // GitHub personal access token
    "ghs_",        // GitHub server-to-server
    "ghu_",        // GitHub user-to-server
    "ghr_",        // GitHub refresh
    "github_pat_", // GitHub fine-grained PAT
    "sk-",         // OpenAI / Anthropic style
    "sk_live_",
    "sk_test_",
    "xoxb-",      // Slack bot
    "xoxp-",      // Slack user
    "AKIA",       // AWS access key id (long-term)
    "ASIA",       // AWS access key id (temporary)
    "AIza",       // Google API key
    "-----BEGIN", // PEM private key / certificate block
];

/// Name tokens that mark a variable as credential-bearing **only when its value
/// carries URL userinfo** — the connection-string family.
///
/// Matched as a suffix (or as the whole name), not as a substring: `URL` is a
/// substring of far too much to withhold unconditionally, and a bare
/// `QONTINUI_API_URL=http://127.0.0.1:8000` is one of the most useful lines in
/// the report. The gate is the VALUE's shape — see
/// [`url_userinfo`](fn@url_userinfo). `postgresql://qontinui@localhost/db`
/// names an account and is withheld under this arm; `https://api.qontinui.io`
/// has no userinfo at all and stays printable.
pub const URL_NAME_TOKENS: &[&str] = &["URL", "URI", "DSN"];

/// What a URL's userinfo component carries, when it has one.
///
/// `scheme://user:password@host` versus `scheme://user@host` are different
/// findings: the first is a printed password, the second is a printed account
/// name next to a host. Both are withheld, under different arms, so a reader
/// can tell which happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlUserinfo {
    /// `scheme://user@host` — a userinfo component with no `:password` part.
    UserOnly,
    /// `scheme://user:password@host` — a password embedded in the URL.
    WithPassword,
}

/// The userinfo component of `value`, when `value` **contains** a URL that has
/// one.
///
/// Hand-rolled rather than regex-matched so the lib crate keeps its dependency
/// surface, and parsed rather than substring-searched so a value that merely
/// CONTAINS an `@` (a `PATH` entry, an email in a commit message) is not
/// mistaken for one. The shape recognised is the RFC 3986 authority:
/// `scheme "://" [ userinfo "@" ] host …` — but the authority is delimited
/// **from the `@` outwards**, not from the `://` inwards. See
/// "Why the authority is delimited from the `@`, not from the `://`" below;
/// delimiting it at the first `/`, `?` or `#` after the separator cut INSIDE
/// passwords that contain one, and every such cut was a printed password.
///
/// This is the arm the entropy heuristic structurally cannot reach:
/// [`value_is_credential`]'s token charset excludes `:` and `@`, so
/// `postgresql://qontinui:hunter2@localhost:5432/qontinui` short-circuits to
/// `false` there — while `QONTINUI_DATABASE_URL` matches
/// [`HIGHLIGHT_PREFIXES`] and would be printed verbatim into the side-by-side
/// table. `ci_node::services` already documents that `DATABASE_URL` "embeds
/// the password".
///
/// # Why EVERY `://` is scanned, and why the scheme is walked BACKWARDS
///
/// The first cut of this function took `value.find("://")` — the FIRST
/// separator only — and required the whole prefix before it to be the scheme
/// (`&value[..sep]` alphabetic-then-alphanumeric). Both halves of that produced
/// silent false negatives on values this fleet genuinely carries, and a false
/// negative here is a printed password, because the other two arms cannot cover
/// for it: [`value_is_credential`]'s entropy charset excludes `:` and `@`, and
/// [`url_name_with_userinfo`] is GATED on this function returning `Some`.
///
/// | Value | What the old shape did |
/// |---|---|
/// | `https://pypi.org/simple https://ci:tok3n@pkgs.internal/simple` | took the FIRST `://`, whose authority is credential-free, and stopped |
/// | `jdbc:postgresql://u:hunter2@h:5432/db` | scheme slice `jdbc:postgresql` — the embedded `:` fails the charset |
/// | `--proxy=http://user:pass@proxy:8080` | scheme slice `--proxy=http` — the leading `-` fails `is_ascii_alphabetic` |
///
/// The first is a real `PIP_EXTRA_INDEX_URL`, the second a real Spring
/// `SPRING_DATASOURCE_URL`, the third a real `CURL_OPTS`. So: every `://` in the
/// value is a candidate, and the scheme is recovered by walking BACKWARDS from
/// each separator over the RFC 3986 scheme charset until a character that
/// cannot be in a scheme (`=`, `:`, a space, a quote) stops the walk. That
/// admits a URL embedded anywhere in a larger string, which is what a
/// credential-bearing env value most often is.
///
/// # Why the authority is delimited from the `@`, not from the `://`
///
/// The second cut of this function ended the authority at the first `/`, `?` or
/// `#` **after the separator**, then rejected any authority containing
/// whitespace. Both halves of that cut through the middle of real passwords, and
/// every one of them printed the password verbatim — the same class of false
/// negative the backwards scheme walk was written to close, one layer further
/// in. Confirmed by execution against the previous body:
///
/// | Value | The old delimiter | What it returned |
/// |---|---|---|
/// | `postgres://admin:p#ssw0rd@db.internal:5432/app` | the `#` INSIDE the password → authority `admin:p` | `None` |
/// | `postgres://admin:pa/ss@db.internal:5432/app` | the `/` INSIDE the password → authority `admin:pa` | `None` |
/// | `https://ci:tok3n@pkgs.internal https://pypi.org/simple` | no `/?#` before the space → whitespace guard | `None` |
/// | `--proxy http://user:pass@proxy:8080 --silent` | same | `None` |
/// | `postgresql://qontinui:hunter 2@localhost:5432/db` | the space INSIDE the password → whitespace guard | `None` |
///
/// A password is opaque: RFC 3986 wants `/`, `?`, `#` and space percent-encoded
/// inside userinfo, and connection strings in the wild routinely do not. So the
/// `@` — not the first structural character — is the anchor. For each `://`, the
/// candidates are the `@`s of the remainder taken from the LAST backwards (RFC
/// 3986 puts the userinfo before the LAST `@` of an authority); an earlier one is
/// reached only when the later candidate is not authority-shaped at all. The
/// authority then runs to the first `/`, `?`, `#` or whitespace **after** the
/// chosen `@`.
///
/// # What replaces the two guards that were doing the rejecting
///
/// Delimiting from the `@` means the userinfo candidate can now swallow
/// arbitrary text to its left, so two SHAPE tests carry the load the delimiter
/// used to — and they are applied to the two halves that are genuinely
/// constrained, never to the password:
///
/// - **the username** — the userinfo up to its first `:` — must be drawn from
///   `[A-Za-z0-9-._~%+@!$&'()*;]` plus any non-ASCII, non-whitespace,
///   non-control character. Account names are; JSON (`"`, `,`, `{`),
///   comma-joined lists, prose and path/query text are not.
/// - **the host** — from the `@` to the first `/`, `?`, `#` or whitespace — must
///   be non-empty and drawn from `[A-Za-z0-9-._:%\[\],]` plus the same non-ASCII
///   admission (port and IPv6 brackets included, so `http://u:p@[::1]:8080/db`
///   is still found, an IDN host like `münchen.example.com` too, and the comma
///   of a multi-host authority — see below).
///
/// That is what keeps the widened parse off ordinary structured values, all
/// three verified by execution:
///
/// | Value | Where it dies |
/// |---|---|
/// | `{"url":"https://api.example.com","contact":"ops@example.com"}` | username `api.example.com","contact"` carries `"` and `,` |
/// | `https://a.example.com,mailto:ops@example.com` | username `a.example.com,mailto` carries `,` |
/// | `service.name=api,repo=https://github.com,owner=a@b.com` | username `github.com,owner=a` carries `,` and `=` |
///
/// The username charset is also what keeps the pre-existing true negatives true:
/// `https://h/p@q` has username `h/p`, `https://h?x=a@b` has `h?x=a`,
/// `scheme://host/path?q=a:b@c` has `host/path?q=a` — all carry a structural
/// character no account name may.
///
/// # Why the username charset is WIDER than an account name looks, and empty
///
/// The third cut of this function read the username as
/// `[A-Za-z0-9-._~%+]`, non-empty and ASCII-only. Both halves of that were
/// false negatives — i.e. printed passwords — against connection strings this
/// fleet and its dependencies genuinely produce. Confirmed by execution against
/// the previous body, all returning `None`:
///
/// | Value | Why the old shape rejected it |
/// |---|---|
/// | `redis://:s3cretpw@127.0.0.1:6379/0` | the username is EMPTY — the canonical pre-ACL Redis form |
/// | `amqp://:guestpw@rabbit.internal:5672/%2f` | same, and the RabbitMQ default |
/// | `https://:ghp_A1b2C3d4E5f6G7h8@github.com/o/r.git` | same, the git-over-HTTPS token form |
/// | `postgres://myadmin@mydemoserver:mypassword@srv.postgres.database.azure.com:5432/db` | `@` INSIDE the username — mandated by Azure Database for PostgreSQL/MySQL Single Server |
/// | `mongodb+srv://ops@corp.com:hunter2@cluster0.mongodb.net/test` | same, via email-as-account (Atlas, Snowflake) |
/// | `postgres://o'brien:hunter2@db.internal:5432/app` | RFC 3986 permits the sub-delims `!$&'()*+,;=` unencoded in userinfo |
/// | `https://u:pw@münchen.example.com/x` | a non-ASCII (IDN) host |
///
/// This module CONSTRUCTS `redis://` URLs and exports `REDIS_URL` itself
/// (`bin/qontinui_profile.rs`, `ci_node::services`, `ci_node::executor`, and
/// `ci_node::manifest` names `REDIS_URL` alongside `DATABASE_URL` as a
/// credential-bearing family), so the empty-username row is not a curiosity —
/// it is the shape the report is most likely to meet.
///
/// So: `@` and the RFC 3986 sub-delims `!$&'()*;` are admitted, non-ASCII is
/// admitted in both halves, and an empty username is admitted **when a password
/// is declared beside it**. `https://@h/x` — no account, no password — is still
/// not a finding.
///
/// **`,` and `=` are the two sub-delims deliberately held out of the
/// USERNAME**, and they are exactly the punctuation of the three structured
/// non-URL values in the table above: admit `,` and
/// `https://a.example.com,mailto:ops@example.com` parses as user
/// `a.example.com,mailto` / password `ops`; admit both and
/// `service.name=api,repo=https://github.com,owner=a@b.com` parses as a bare
/// user. Holding `=` out is also what keeps `;` safe — a semicolon-joined
/// key/value list still dies on its `=`. The cost is a NAMED residual: a
/// username containing a literal `,` or `=` still leaks, which the contract
/// table pins as a visible hole rather than leaving it to be rediscovered.
///
/// That exclusion is **per position, and only the username's** — see
/// "Why the two halves do not share one charset" below. `,` in a host is the
/// multi-host separator and is admitted there.
///
/// # Why the two halves do not share one charset, and why the host admits `,`
///
/// The fourth cut of this function widened the USERNAME to the RFC 3986
/// sub-delims and left the HOST charset untouched — so the two halves
/// disagreed about a production they both parse, and every **multi-host**
/// connection string fell out. The host span ends at the first `/?#`/whitespace
/// after the `@`, so for `…@pg1.internal:5432,pg2.internal:5432/db` the host is
/// `pg1.internal:5432,pg2.internal:5432`, the `,` failed the charset, the
/// candidate was discarded, and no later arm rescues it: the entropy charset
/// admits neither `:` nor `@`, and [`url_name_with_userinfo`] is gated on this
/// function. Confirmed by execution against the previous body — each of these
/// returned `None` while its single-host control returned `WithPassword`, so
/// the comma was the only difference:
///
/// | Value | What the missing `,` did |
/// |---|---|
/// | `postgresql://qontinui:hunter2@pg1.internal:5432,pg2.internal:5432/qontinui?target_session_attrs=primary` | libpq multi-host failover — `None` |
/// | `mongodb://appuser:hunter2@h1.internal:27017,h2.internal:27017,h3.internal:27017/app?replicaSet=rs0` | a Mongo replica-set seed list — `None` |
/// | `redis://:s3cretpw@s1.internal:26379,s2.internal:26379/0` | Redis Sentinel, empty username — `None` |
/// | `kafka://svc:s3cret@b1.internal:9092,b2.internal:9092` | a Kafka broker list — `None` |
///
/// The two halves nonetheless **cannot** be one charset, and that is the rule
/// this function follows: each half admits exactly what real values carry **in
/// that position**, and every exclusion is named against the shape it defends.
/// `,` is the case that proves it — in a username it is the comma-joined-list
/// discriminator that holds `https://a.example.com,mailto:ops@example.com` at
/// `None`; in a host it is the multi-host separator four families above depend
/// on. Same character, opposite roles.
///
/// Full RFC alignment of the host half — admitting every sub-delim
/// `!$&'()*+,;=` the username now admits — was measured and rejected. It buys
/// one hypothetical shape (an option-carrying authority such as
/// `sqlserver://sa:P@ssw0rd@host:1433;encrypt=true`, a form this fleet does not
/// produce and whose STANDARD spelling
/// `jdbc:sqlserver://host:1433;user=sa;password=P@ssw0rd` is already caught)
/// and costs printed diagnostics on shapes that are real: with `=` admitted,
/// `service.name=api,repo=https://git@github.com,owner=x` and
/// `k=v;repo=https://git@github.com;x=1` are withheld, and with `'` admitted so
/// is `curl 'https://svc@host'`. The intermediate "RFC minus `=`" is strictly
/// dominated — it pays the `'` cost and closes nothing. So the sub-delims with
/// no carrier in a host stay OUT, and the `;`-and-`=` option authority is a
/// NAMED residual, pinned in the contract table beside the username's `,`/`=`
/// one rather than left to be rediscovered as a fifth leak.
///
/// # The one direction this deliberately errs in
///
/// A password is unconstrained, so `https://u:80/path?x=a@b` — host `u`, port
/// `80`, an `@` in the QUERY — reads as `u` + password `80/path?x=a` and is
/// withheld. That is over-withholding: it costs a printed diagnostic URL. The
/// alternative is to constrain the password, and every constraint that would
/// reject it also rejects `pa/ss`, `p#ssw0rd` and `hunter 2` above — i.e. costs a
/// printed PASSWORD. The trade is taken in that direction on purpose, and only
/// for the password half; the username and host halves stay strict, which is why
/// the ordinary-structured-value rows are still `None`.
///
/// Note the resulting asymmetry, which is deliberate and pinned in both
/// directions: whitespace in a PASSWORD is caught (`u:hunter 2@h`), whitespace in
/// a USERNAME is not (`user :pw@h`). Real account names have no spaces, and
/// admitting them would make `"…via https://gateway then ask bob@corp.com"`
/// withhold a whole prose line on nothing but an email address.
///
/// [`UrlUserinfo::WithPassword`] wins over [`UrlUserinfo::UserOnly`] wherever
/// both occur, since the reason a reader is shown must be the worst one present.
pub fn url_userinfo(value: &str) -> Option<UrlUserinfo> {
    let bytes = value.as_bytes();
    let scheme_char = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-');
    // Non-ASCII is admitted in BOTH shape tests — an IDN host
    // (`münchen.example.com`) and a non-ASCII account name are real, and a
    // charset that is ASCII-only rejects them into a PRINTED password. Unicode
    // whitespace and control characters stay out: whitespace is the class that
    // separates prose from a URL, and it is the only thing holding the prose
    // row below at `None`.
    let non_ascii_char = |c: char| !c.is_ascii() && !c.is_whitespace() && !c.is_control();
    // The two shape tests that replaced the structural delimiter. Neither is
    // applied to the password half — see this function's docs.
    let username_char = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | '~'
                    | '%'
                    | '+'
                    | '@'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | ';'
            )
            || non_ascii_char(c)
    };
    // The host span is the AUTHORITY REMAINDER, not a hostname: it runs from
    // the `@` to the first `/?#`/whitespace and therefore carries the port
    // (`:`), IPv6 brackets, a zone id's `%`, an IDN's non-ASCII — and the
    // comma that separates the members of a MULTI-HOST authority
    // (`h1:5432,h2:5432`). Omitting `,` printed four real connection-string
    // families verbatim; see this function's docs.
    let host_char = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '.' | '_' | ':' | '%' | '[' | ']' | ',')
            || non_ascii_char(c)
    };
    let host_end = |c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace();

    let mut weakest: Option<UrlUserinfo> = None;
    for (sep, _) in value.match_indices("://") {
        // Walk backwards over the scheme charset. Only ASCII bytes are ever
        // stepped over, so `start` stays on a UTF-8 boundary.
        let mut start = sep;
        while start > 0 && scheme_char(bytes[start - 1]) {
            start -= 1;
        }
        // A scheme must exist and must begin with a letter (`8080://` is not a
        // URL, and neither is a bare `://`).
        if start == sep || !bytes[start].is_ascii_alphabetic() {
            continue;
        }

        let rest = &value[sep + 3..];
        // Candidate `@`s, LAST first. `@` is one ASCII byte, so `at` and
        // `at + 1` are both UTF-8 boundaries.
        for (at, _) in rest.rmatch_indices('@') {
            let userinfo = &rest[..at];
            let (user, password) = match userinfo.split_once(':') {
                Some((u, p)) => (u, Some(p)),
                None => (userinfo, None),
            };
            // Emptiness rejects only when the WHOLE userinfo is empty of
            // meaning. An empty username beside a declared password is
            // `redis://:password@host` — the canonical pre-ACL Redis form, and
            // the shape three rewrites of this function printed verbatim. An
            // empty username with no password (`https://@h/x`) names no account
            // and carries no secret, so it stays a non-finding.
            let names_an_account = !user.is_empty();
            let declares_a_password = password.is_some_and(|p| !p.is_empty());
            if !names_an_account && !declares_a_password {
                continue;
            }
            if !user.chars().all(username_char) {
                continue;
            }
            let after = &rest[at + 1..];
            let host = match after.find(host_end) {
                Some(end) => &after[..end],
                None => after,
            };
            if host.is_empty() || !host.chars().all(host_char) {
                continue;
            }
            match password {
                // The worst finding in the value; nothing later can outrank it.
                Some(p) if !p.is_empty() => return Some(UrlUserinfo::WithPassword),
                // `user:@host` — a declared-but-empty password is not a secret;
                // the account name still is, under the same arm as a bare
                // `user@host`. This `@` parsed, so stop widening for this `://`.
                _ => {
                    weakest = Some(UrlUserinfo::UserOnly);
                    break;
                }
            }
        }
    }
    weakest
}

/// Why a value is withheld. Carried into the report so a reader can tell "this
/// is a password" from "this merely looked like one".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WithholdReason {
    /// The NAME contains a credential-class token (or the `*_PAT` suffix).
    Name { token: String },
    /// The VALUE has a recognised credential prefix.
    ValuePrefix { prefix: String },
    /// The VALUE is a long, mixed-case, high-entropy token-shaped string.
    ValueEntropy,
    /// The VALUE is a URL with a password embedded in its userinfo —
    /// `scheme://user:password@host`. Independent of the name: `HTTPS_PROXY`
    /// and `REDIS_URL` carry one just as `DATABASE_URL` does.
    ValueUrlPassword,
    /// The NAME is a connection-string name ([`URL_NAME_TOKENS`]) **and** the
    /// value carries a userinfo component naming an account. A URL with no
    /// userinfo is not withheld under this arm.
    NameUrlWithUserinfo { token: String },
    /// The NAME carries U+FFFD, so [`name_is_credential`]'s `contains` test
    /// could not be trusted about it. The LAST arm — every other arm is tried
    /// first, so a mangled-name variable that any real arm catches keeps that
    /// arm's more informative reason. See [`name_is_unreadable`].
    NameUnreadable,
}

impl WithholdReason {
    /// One-line human form, used in the rendered report.
    pub fn describe(&self) -> String {
        match self {
            WithholdReason::Name { token } => format!("name contains {token}"),
            WithholdReason::ValuePrefix { prefix } => format!("value prefix {prefix:?}"),
            WithholdReason::ValueEntropy => "value is a long high-entropy token".to_string(),
            WithholdReason::ValueUrlPassword => {
                "value is a URL with a password in its userinfo".to_string()
            }
            WithholdReason::NameUrlWithUserinfo { token } => {
                format!("name ends with {token} and the value carries URL userinfo")
            }
            WithholdReason::NameUnreadable => {
                "name carries U+FFFD, so the credential-name test cannot be trusted".to_string()
            }
        }
    }
}

/// True when `name` is credential-bearing by NAME alone.
fn name_is_credential(name: &str) -> Option<WithholdReason> {
    let upper = name.to_ascii_uppercase();
    // `*_PAT` / bare `PAT` — matched as a suffix so `PATH` is untouched.
    if upper == "PAT" || upper.ends_with("_PAT") {
        return Some(WithholdReason::Name {
            token: "_PAT".to_string(),
        });
    }
    CREDENTIAL_NAME_TOKENS
        .iter()
        .find(|t| upper.contains(**t))
        .map(|t| WithholdReason::Name {
            token: (*t).to_string(),
        })
}

/// True when the NAME itself came through the lossy conversion damaged.
///
/// [`name_is_credential`] is `upper.contains(token)`, so ONE byte mangled
/// inside the token — `POSTGRES_PASSW\u{FFFD}RD` — matches nothing, and a short
/// value beside it (`hunter2`: seven characters, no entropy, no URL shape, no
/// prefix) has no value arm to fall into either. That was a named, pinned
/// residual until this function; the cheap repair genuinely does not work,
/// because the mangled byte REPLACED the `O` rather than sitting beside it, so
/// stripping U+FFFD yields `POSTGRES_PASSWRD` and still matches nothing.
///
/// What closes it is not a better name match but the same judgement
/// [`token_charset`] already makes about values: **a variable whose NAME is
/// partly unreadable is a variable this report cannot classify**, and nothing
/// is defended by printing its value anyway. No legitimate environment variable
/// name carries U+FFFD — it exists in a name only because [`lossy_env_pairs`]
/// put it there — so the arm cannot fire on a healthy machine, and on a damaged
/// one it withholds a value whose own name the operator cannot match to
/// anything they exported. The row is still PRESENT, still counted, and still
/// fingerprinted; only the value is held back.
///
/// Deliberately the LAST arm of [`classify_env_var`], so a mangled-name
/// variable that any other arm catches reports THAT arm instead — the mangled
/// entropy token in
/// `env_generations_g1_capture_survives_a_non_unicode_environment` is still
/// [`WithholdReason::ValueEntropy`], which is the more diagnostic answer.
fn name_is_unreadable(name: &str) -> Option<WithholdReason> {
    name.contains(TOKEN_MANGLED_CHAR)
        .then_some(WithholdReason::NameUnreadable)
}

/// True when the NAME is a connection-string name AND the VALUE actually
/// carries userinfo.
///
/// Both halves are required, in that order, and the value half is the one doing
/// the work: `QONTINUI_API_URL=http://127.0.0.1:8000` must stay printable (it
/// is one of the report's most-read lines and is not a secret), while
/// `QONTINUI_DATABASE_URL=postgresql://qontinui@localhost/db` names an account
/// on a host and must not be.
fn url_name_with_userinfo(name: &str, value: &str) -> Option<WithholdReason> {
    let upper = name.to_ascii_uppercase();
    let token = URL_NAME_TOKENS
        .iter()
        .find(|t| upper == **t || upper.ends_with(&format!("_{t}")))?;
    url_userinfo(value).map(|_| WithholdReason::NameUrlWithUserinfo {
        token: format!("_{token}"),
    })
}

/// The character `to_string_lossy` substitutes for an unpaired UTF-16
/// surrogate (Windows) or a non-UTF-8 byte (Linux) — see [`lossy_env_pairs`].
///
/// Named once, as a constant, because it is the **sole** difference between
/// [`token_charset`] and [`token_boundary_char`], and those two differ in
/// OPPOSITE directions. Spelling `'\u{FFFD}'` twice would let one edit move it
/// in one place only.
const TOKEN_MANGLED_CHAR: char = '\u{FFFD}';

/// The characters every credential-token charset in this module agrees on —
/// base64, base64url, hex, and the `-`/`.`-separated forms.
///
/// This is the SHARED BASE, and it is the whole of the agreement between the
/// two charsets below. Neither of them is written as a second literal:
/// [`token_charset`] is `base ∪ {U+FFFD}` and [`token_boundary_char`] is
/// `base \ {U+FFFD}`, both expressed in terms of this function and
/// [`TOKEN_MANGLED_CHAR`], so the one character they disagree about is named
/// once and the relationship is visible at both sites.
///
/// Two independent literals is exactly how [`url_userinfo`]'s username and host
/// charsets came to disagree about one character and print four connection
/// strings — which is why the two charsets below are a documented DIFFERENCE
/// from a shared base rather than a fork.
fn token_base_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.')
}

/// The **INTERIOR** charset — what may appear ANYWHERE INSIDE a high-entropy
/// credential token. `base ∪ {U+FFFD}`.
///
/// Read by [`value_is_credential`]'s entropy arm, which requires
/// `chars().all(token_charset)` over the whole value.
///
/// # Why U+FFFD is IN the set
///
/// It is the character [`lossy_env_pairs`] substitutes for an unpaired UTF-16
/// surrogate (Windows) or a non-UTF-8 byte (Linux) — on exactly the machine
/// whose environment is odd enough that someone opened this diagnostic. With it
/// excluded, `chars().all` was FALSE for a mangled token, so a 32+ character
/// mixed-case secret carrying one bad byte fell OUT of the entropy arm and
/// rendered with every other character verbatim. That is a redaction hole
/// opened BY the lossy conversion, and it was live:
/// `value_is_credential("AbCdEf0123456789AbCdEf0123456789Ab")` was
/// `Some(ValueEntropy)` while the same token with one U+FFFD in it was `None`.
///
/// Admitting it costs nothing readable: no legitimate value carries U+FFFD
/// deliberately, and a value that does is already partly unreadable, so
/// withholding it loses a diagnostic line that was damaged anyway. That is the
/// direction this module errs in everywhere else.
fn token_charset(c: char) -> bool {
    token_base_char(c) || c == TOKEN_MANGLED_CHAR
}

/// The **BOUNDARY** charset — what may appear at a token's EDGE, and therefore
/// what [`trim_token`] keeps. `base \ {U+FFFD}`.
///
/// # Why this is NOT [`token_charset`]
///
/// The two consumers ask different questions, and sharing one answer broke the
/// prefix arm. `trim_token` strips the punctuation prose wraps a value in; a
/// mangled byte is legitimately INTERIOR to a token (hence [`token_charset`])
/// but at a token's boundary it is exactly the noise to strip. A CP-1252 curly
/// quote around a Windows env value comes through [`lossy_env_pairs`] as a
/// LEADING U+FFFD, and while U+FFFD was in the trim keep-set that leading
/// character survived, so `value_is_credential`'s `starts_with` PREFIX arm
/// could never match:
/// `classify_free_text(…, "read failed: bad value \u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD} in profile")`
/// returned `None` and printed the AWS access-key id verbatim.
///
/// The entropy arm does not backstop that. It needs `value.len() >= 32` AND an
/// ASCII lower-case character, and four of [`CREDENTIAL_VALUE_PREFIXES`]'
/// shapes satisfy neither: `AKIA…`/`ASIA…` are 20 characters of upper-case and
/// digits, `-----BEGIN…` heads a 31-character line that also carries spaces,
/// and a short `xoxp-` token sits well under the floor. Measured per prefix in
/// `env_generations_credential_prefix_survives_a_mangled_boundary`.
///
/// Trimming U+FFFD at the edge can, on its own, shorten a mangled token past
/// the entropy arm's 32-byte floor (U+FFFD is three bytes, so a 34-byte token
/// trims to 31). That is why [`EnvVarReading::classify_free_text`] offers each
/// prose token to [`value_is_credential`] under BOTH trims — see
/// [`trim_token_interior`]. Neither arm pays for the other.
fn token_boundary_char(c: char) -> bool {
    token_base_char(c) && c != TOKEN_MANGLED_CHAR
}

/// True when `value` looks credential-bearing regardless of its name.
///
/// Three arms, and the third is the one a name-based classifier can never
/// reach:
///
/// 1. a recognised credential PREFIX ([`CREDENTIAL_VALUE_PREFIXES`]);
/// 2. a long mixed-case high-entropy token;
/// 3. a URL with a password in its userinfo ([`url_userinfo`]).
///
/// Arm 3 is not a refinement of arm 2 — it is the arm arm 2 **excludes by
/// construction**. The entropy charset admits neither `:` nor `@`, so every
/// connection string short-circuits it to `false`; before arm 3 existed,
/// `QONTINUI_DATABASE_URL=postgresql://qontinui:hunter2@localhost:5432/qontinui`
/// matched no prefix, no name token and no entropy test, and was printed
/// verbatim into the side-by-side table (which the `QONTINUI_` highlight prefix
/// puts it in) while `total_withheld()` counted it as safe.
///
/// The entropy arm remains deliberately narrow enough not to swallow the values
/// an operator most needs to read: a `PATH` carries separators and spaces, a git
/// SHA is 40 characters of *lower-case* hex (no upper-case, so it fails the
/// mixed-case requirement), and a plain URL carries `:` and `/`.
pub fn value_is_credential(value: &str) -> Option<WithholdReason> {
    // Arm 1 reads the value AND its boundary-trimmed form. `starts_with` is
    // defeated by ONE leading character that is not part of the credential —
    // the U+FFFD a CP-1252-quoted Windows value arrives with through
    // [`lossy_env_pairs`], or the quote an operator exported literally — and
    // for `AKIA`/`ASIA`/`-----BEGIN`/short `xoxp-` there is no entropy arm
    // underneath to catch the value instead (see [`token_boundary_char`]).
    // Trimming here can only remove characters no credential shape begins
    // with, so it cannot make arm 1 miss a value it used to find.
    let trimmed = trim_token(value);
    if let Some(p) = CREDENTIAL_VALUE_PREFIXES
        .iter()
        .find(|p| value.starts_with(**p) || trimmed.starts_with(**p))
    {
        return Some(WithholdReason::ValuePrefix {
            prefix: (*p).to_string(),
        });
    }
    if url_userinfo(value) == Some(UrlUserinfo::WithPassword) {
        return Some(WithholdReason::ValueUrlPassword);
    }
    if value.len() >= 32
        && value.chars().all(token_charset)
        && value.chars().any(|c| c.is_ascii_lowercase())
        && value.chars().any(|c| c.is_ascii_uppercase())
        && value.chars().any(|c| c.is_ascii_digit())
    {
        return Some(WithholdReason::ValueEntropy);
    }
    None
}

/// Classify one environment variable. `None` means "safe to print".
///
/// Name first, then value shape — an unrecognised name whose value looks like a
/// credential is still withheld, which is what makes the classifier able to
/// catch a variable nobody thought to enumerate. Then the joint arm: a
/// connection-string NAME whose VALUE carries userinfo, where neither half
/// alone is enough to withhold.
///
/// [`name_is_unreadable`] is LAST on purpose — it is a backstop for a name the
/// other arms could not be trusted to have read, so every arm that can give a
/// more specific reason gets to answer first.
pub fn classify_env_var(name: &str, value: &str) -> Option<WithholdReason> {
    name_is_credential(name)
        .or_else(|| value_is_credential(value))
        .or_else(|| url_name_with_userinfo(name, value))
        .or_else(|| name_is_unreadable(name))
}

/// Per-report keyed fingerprinter — see the module docs on comparing values we
/// deliberately never kept.
///
/// One instance per report run. The key is `RandomState`'s, drawn fresh per
/// instance and never printed, so two fingerprints are comparable *within* one
/// report and carry no information outside it.
pub struct EnvFingerprinter {
    state: RandomState,
}

impl Default for EnvFingerprinter {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvFingerprinter {
    /// A fresh key. Two `EnvFingerprinter`s never agree, by construction — do
    /// not create a second one inside one report.
    pub fn new() -> Self {
        Self {
            state: RandomState::new(),
        }
    }

    /// 8 hex characters of the keyed hash of `value`.
    pub fn fingerprint(&self, value: &str) -> String {
        format!("{:08x}", self.state.hash_one(value) as u32)
    }
}

/// A value that has PASSED classification.
///
/// The inner `String` is private, so the ONLY way to obtain one outside this
/// module is [`EnvVarReading::classify`], which returns [`EnvValue::Withheld`]
/// for anything credential-classed. That is the structural half of this
/// module's guarantee: a caller cannot hand-build a `Shown` value that skipped
/// the classifier.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SafeValue(String);

impl SafeValue {
    /// The value, which by construction has passed [`classify_env_var`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a report may say about one variable's value.
///
/// Note the shape of [`Withheld`](Self::Withheld): a reason and a fingerprint,
/// and **no field able to carry the value**. A withheld reading cannot leak,
/// whether it reaches a text renderer, `serde_json`, a log line or a panic
/// message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnvValue {
    Shown {
        value: SafeValue,
    },
    Withheld {
        reason: WithholdReason,
        /// Per-run keyed fingerprint — equal fingerprints inside ONE report mean
        /// equal values. Meaningless across reports.
        fingerprint: String,
    },
}

impl EnvValue {
    /// The cell text for a table/column render. A withheld value renders as its
    /// fingerprint, never as any part of the value.
    pub fn cell(&self) -> String {
        match self {
            EnvValue::Shown { value } => value.as_str().to_string(),
            EnvValue::Withheld { fingerprint, .. } => format!("<withheld #{fingerprint}>"),
        }
    }

    /// The detail text: the cell plus, for a withheld value, why.
    pub fn detail(&self) -> String {
        match self {
            EnvValue::Shown { value } => value.as_str().to_string(),
            EnvValue::Withheld {
                reason,
                fingerprint,
            } => format!("<withheld #{fingerprint}: {}>", reason.describe()),
        }
    }

    /// True when this reading is withheld.
    pub fn is_withheld(&self) -> bool {
        matches!(self, EnvValue::Withheld { .. })
    }
}

/// One variable, as one generation holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvVarReading {
    pub name: String,
    pub value: EnvValue,
}

impl EnvVarReading {
    /// THE ingestion point. Every raw environment value in this module enters
    /// through here and is classified before it can be stored.
    pub fn classify(fp: &EnvFingerprinter, name: &str, value: &str) -> Self {
        let classified = match classify_env_var(name, value) {
            None => EnvValue::Shown {
                value: SafeValue(value.to_string()),
            },
            Some(reason) => EnvValue::Withheld {
                reason,
                fingerprint: fp.fingerprint(value),
            },
        };
        EnvVarReading {
            name: name.to_string(),
            value: classified,
        }
    }

    /// The ingestion point for a value that is **prose**, not an environment
    /// value: an OS message, a `serde` message, anything assembled with
    /// `format!` around a variable the assembler did not choose.
    ///
    /// # Why [`Self::classify`] is not enough for this shape
    ///
    /// Every arm of [`classify_env_var`] examines the value AS A WHOLE. The
    /// entropy arm requires `chars().all(token_charset)`, so it dies on the
    /// first space — which means for a string like
    /// `parse failed: invalid type: string "AbCd…", expected u16 at line 4`
    /// the classifier returns `None` and the call is **inert**: it protects
    /// nothing while reading as though it does. That is the same vacuity class
    /// this module's header criticises `redact_secrets` for, so a doc comment
    /// claiming the protection is worse than no call at all.
    ///
    /// So free text is classified whole AND per whitespace-separated token,
    /// with surrounding punctuation trimmed (a JSON message wraps the offending
    /// value in `"…",`) — under BOTH trims, because a boundary U+FFFD must be
    /// stripped for the PREFIX arm and kept for the ENTROPY arm's 32-byte floor
    /// (see [`trim_token_interior`]). A hit on ANY token withholds the WHOLE
    /// string — the value is prose, there is no safe way to emit "the rest of
    /// it", and the fingerprint is taken over the whole text so two reports can
    /// still tell whether the same message occurred.
    ///
    /// Deliberately NOT folded into [`classify_env_var`]: tokenising every
    /// environment value would put `PATH` — separators, spaces, and dozens of
    /// mixed-case path segments — one unlucky segment away from being withheld,
    /// and `PATH` is the variable this report can least afford to hide.
    pub fn classify_free_text(fp: &EnvFingerprinter, name: &str, text: &str) -> Self {
        let reason = classify_env_var(name, text).or_else(|| {
            text.split_whitespace()
                // BOTH trims, per token — they fail in opposite directions and
                // each is the other's backstop. See [`trim_token_interior`].
                .flat_map(|t| [trim_token(t), trim_token_interior(t)])
                .filter(|t| !t.is_empty())
                .find_map(value_is_credential)
        });
        let classified = match reason {
            None => EnvValue::Shown {
                value: SafeValue(text.to_string()),
            },
            Some(reason) => EnvValue::Withheld {
                reason,
                fingerprint: fp.fingerprint(text),
            },
        };
        EnvVarReading {
            name: name.to_string(),
            value: classified,
        }
    }
}

/// Strip the punctuation a prose message wraps a value in (`"…",`, `(…)`,
/// `'…'`) so the token handed to [`value_is_credential`] is the value itself.
/// Only the ENDS are trimmed, so a URL's internal `:` and `@` survive.
///
/// The keep-set is [`token_boundary_char`] — the shared base MINUS U+FFFD —
/// because a mangled byte at a token's edge is noise, and while it was kept a
/// leading one defeated the `starts_with` PREFIX arm outright. See
/// [`token_boundary_char`] for the executed failure and for why the entropy arm
/// does not backstop it.
fn trim_token(token: &str) -> &str {
    token.trim_matches(|c: char| !token_boundary_char(c))
}

/// The same trim under the INTERIOR charset ([`token_charset`]) — punctuation
/// off the ends, but a U+FFFD at the edge KEPT.
///
/// This exists because the two trims fail in opposite directions and each is
/// the other's backstop, so [`EnvVarReading::classify_free_text`] offers a
/// prose token under both:
///
/// - [`trim_token`] strips a leading U+FFFD, which is what lets the PREFIX arm
///   see `AKIA…`;
/// - stripping it costs three bytes, so a 34-byte mangled entropy token trims
///   to 31 and drops under the entropy arm's 32-byte floor. This trim keeps
///   those bytes, so the ENTROPY arm still sees the token.
///
/// Neither is a superset of the other, which is why there are two and not a
/// "better" one.
fn trim_token_interior(token: &str) -> &str {
    token.trim_matches(|c: char| !token_charset(c))
}

// ===========================================================================
// Generations.
// ===========================================================================

/// One environment, as it stood at one moment, from one vantage point.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvGeneration {
    /// Short column label — `G1`, `G2`, `G3`. Stable across runs.
    pub id: String,
    /// Machine name, e.g. `runner_process`.
    pub name: String,
    /// What this generation IS, in one line.
    pub describes: String,
    /// Why it is as old as it is — the sentence that makes the report
    /// diagnostic rather than merely descriptive.
    pub freshness: String,
    pub captured_at: DateTime<Utc>,
    /// Sorted by name, unique.
    pub vars: Vec<EnvVarReading>,
    /// True when this generation is a full environment map (comparable to
    /// another full map). A PARSED, typed subset — the launch snapshot — is
    /// not, and is never value-diffed against a raw env: it holds
    /// `server_mode: false`, not the string `"0"` the operator exported.
    pub is_full_env: bool,
}

/// The identity of a generation — everything about it that does not depend on
/// the capture itself. Constant per generation, so it travels as one value
/// instead of five positional strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvGenerationSpec {
    /// Short column label — `G1`, `G2`, `G3`.
    pub id: &'static str,
    /// Machine name, e.g. `runner_process`.
    pub name: &'static str,
    /// What this generation IS, in one line.
    pub describes: &'static str,
    /// Why it is as old as it is — the sentence that makes the report
    /// diagnostic rather than merely descriptive.
    pub freshness: &'static str,
    /// True for a FULL environment map (comparable to another full map by
    /// value). A parsed, typed subset is not — see [`EnvGeneration::is_full_env`].
    pub is_full_env: bool,
}

/// `(name, value)` pairs from an OS-string environment iterator, lossily.
///
/// **This is the only supported way to feed a real process environment into
/// [`EnvGeneration::capture`].** `std::env::vars()` is not: it PANICS if any
/// name or any value is not valid Unicode — an unpaired UTF-16 surrogate on
/// Windows, any non-UTF-8 byte on Linux — and `config_report_run` is a
/// `#[tauri::command]`, so a panic there takes down the IPC handler. It would
/// do so on precisely the machine whose environment is weird enough that
/// someone opened the diagnostic to look at it: the report would be
/// unavailable exactly where it is the only tool for the job.
///
/// `to_string_lossy` substitutes U+FFFD and keeps going, which is the same
/// choice `config_report_cmd::std_command_env` already makes for
/// `Command::get_envs`, and the same iterator `portable_pty`'s
/// `CommandBuilder` seeds from (this module's header documents that for G3).
/// A replacement character in a rendered name is a visible, reportable fact;
/// a dead IPC handler is not.
///
/// # What lossiness does and does not cost the classifier
///
/// The pairs are handed to [`EnvVarReading::classify`] exactly as any other
/// pair is, so the question is what U+FFFD does to each charset it reaches.
/// This comment previously claimed lossiness "cannot open a redaction hole" and
/// that a mangled value is "classified MORE conservatively, never less". Both
/// were false as written — the same class of doc comment claiming a protection
/// the code does not provide that this module's header criticises
/// `redact_secrets` for — so what follows is per-arm and per-position:
///
/// - **the VALUE, entropy arm** — U+FFFD is IN [`token_charset`], so a mangled
///   32+ character mixed-case token is still withheld. It was NOT, until the
///   review that produced this paragraph: `chars().all` was false for the
///   mangled token, `classify_env_var("MY_THING", …)` returned `None`, and the
///   secret rendered with every other character verbatim. See
///   [`token_charset`] for why admitting it costs nothing readable.
/// - **the VALUE, URL arm** — U+FFFD is inside [`url_userinfo`]'s username and
///   host charsets, which admit any non-ASCII, non-whitespace, non-control
///   character so an IDN host is not read as a printable URL. A mangled
///   connection string is still found.
/// - **the VALUE, prefix arm** — two different manglings, and only one of them
///   is a residual.
///
///   A mangling immediately BEFORE the prefix (`\u{FFFD}AKIA…`, the shape a
///   CP-1252-quoted Windows value arrives in) is **closed**: the prefix arm
///   reads the value's [`trim_token`]ed form as well as the value, and
///   `trim_token`'s keep-set is [`token_boundary_char`], which excludes
///   U+FFFD. It was open for one review round — U+FFFD was admitted to the
///   trim keep-set along with the entropy charset — and in that round
///   `classify_free_text` returned `None` for
///   `read failed: bad value \u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD} in profile`.
///
///   A mangling INSIDE the prefix itself (`gh\u{FFFD}_`, `s\u{FFFD}-`) still
///   defeats `starts_with` and is a residual. **The entropy arm is not a
///   general backstop for it**, contrary to what this bullet used to claim: it
///   needs `len() >= 32` AND an ASCII lower-case character, and four of
///   [`CREDENTIAL_VALUE_PREFIXES`]' shapes satisfy neither — `AKIA…`/`ASIA…`
///   (20 characters, upper-case and digits only), `-----BEGIN…` (a 31-character
///   line that also carries spaces) and the short `xoxp-` form (well under 32).
///   The measurement is in
///   `env_generations_credential_prefix_survives_a_mangled_boundary`, which
///   re-runs every prefix with its prefix broken rather than asserting this in
///   prose. It does backstop the long
///   mixed-case shapes: `eyJ`, `gho_`, `ghp_`, `github_pat_`, `sk-`,
///   `sk_live_`, `xoxb-`, `AIza`.
/// - **the NAME** — `name_is_credential` is `upper.contains(token)`, so a byte
///   mangled inside the token itself (`POSTGRES_PASSW\u{FFFD}RD`) matches
///   nothing, and a short value beside it (`hunter2` — seven characters, no
///   entropy, no URL shape, no prefix) has no value arm to fall into. This was
///   a named residual; it is now **closed** by [`name_is_unreadable`], the last
///   arm of [`classify_env_var`], which withholds any value whose NAME carries
///   U+FFFD at all. Not by a better name match — the mangled byte REPLACED the
///   `O`, so stripping it yields `POSTGRES_PASSWRD` and still matches nothing —
///   but by the judgement [`token_charset`] already makes about values: a name
///   this report cannot read is a variable it cannot classify. Pinned as a
///   literal in `env_generations_g1_capture_survives_a_non_unicode_environment`.
pub fn lossy_env_pairs<I>(vars: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    vars.into_iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

impl EnvGeneration {
    /// Capture a generation from raw `(name, value)` pairs, classifying every
    /// value on the way in.
    ///
    /// The identity half arrives as an [`EnvGenerationSpec`] rather than as
    /// five loose arguments: they are a fixed description of WHICH generation
    /// this is, they never vary per capture, and threading them separately made
    /// call sites where the wrong string could silently land in the wrong slot.
    pub fn capture<I, K, V>(
        fp: &EnvFingerprinter,
        spec: EnvGenerationSpec,
        captured_at: DateTime<Utc>,
        pairs: I,
    ) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        // BTreeMap both sorts and de-duplicates (last write wins, matching the
        // way a process env behaves for a repeated name).
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in pairs {
            map.insert(k.as_ref().to_string(), v.as_ref().to_string());
        }
        let vars = map
            .iter()
            .map(|(k, v)| EnvVarReading::classify(fp, k, v))
            .collect();
        EnvGeneration {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            describes: spec.describes.to_string(),
            freshness: spec.freshness.to_string(),
            captured_at,
            vars,
            is_full_env: spec.is_full_env,
        }
    }

    /// How many of this generation's variables are withheld.
    pub fn withheld_count(&self) -> usize {
        self.vars.iter().filter(|v| v.value.is_withheld()).count()
    }

    /// This generation's reading for `name`, if it holds one.
    pub fn get(&self, name: &str) -> Option<&EnvValue> {
        self.vars.iter().find(|v| v.name == name).map(|v| &v.value)
    }

    /// `G1 runner_process`
    fn column_label(&self) -> String {
        format!("{} {}", self.id, self.name)
    }
}

// ===========================================================================
// Divergence — the whole diagnostic.
// ===========================================================================

/// One variable's difference between two generations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "delta", rename_all = "snake_case")]
pub enum EnvDelta {
    /// Present in the older generation, gone from the newer one.
    Removed { name: String, left: EnvValue },
    /// Absent from the older generation, present in the newer one — the
    /// operator added it after the older generation was frozen.
    Added { name: String, right: EnvValue },
    /// Present in both, with different values.
    Changed {
        name: String,
        left: EnvValue,
        right: EnvValue,
    },
}

impl EnvDelta {
    /// The variable this delta is about.
    pub fn name(&self) -> &str {
        match self {
            EnvDelta::Removed { name, .. }
            | EnvDelta::Added { name, .. }
            | EnvDelta::Changed { name, .. } => name,
        }
    }

    fn involves_withheld(&self) -> bool {
        match self {
            EnvDelta::Removed { left, .. } => left.is_withheld(),
            EnvDelta::Added { right, .. } => right.is_withheld(),
            EnvDelta::Changed { left, right, .. } => left.is_withheld() || right.is_withheld(),
        }
    }
}

/// The full difference between two FULL environment generations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvDivergence {
    pub left: String,
    pub right: String,
    /// What a delta MEANS for this particular pair — the operator-facing
    /// interpretation, supplied by the capturing site because only it knows
    /// which two generations these are.
    pub interpretation: String,
    pub deltas: Vec<EnvDelta>,
}

/// Diff two full environment generations, oldest first.
///
/// Comparison of a withheld value is by FINGERPRINT (see [`EnvFingerprinter`]),
/// so "this credential changed" is reportable without either value ever having
/// been kept.
pub fn diff_generations(
    left: &EnvGeneration,
    right: &EnvGeneration,
    interpretation: impl Into<String>,
) -> EnvDivergence {
    let names: BTreeSet<&str> = left
        .vars
        .iter()
        .chain(right.vars.iter())
        .map(|v| v.name.as_str())
        .collect();

    let mut deltas = Vec::new();
    for name in names {
        match (left.get(name), right.get(name)) {
            (Some(l), None) => deltas.push(EnvDelta::Removed {
                name: name.to_string(),
                left: l.clone(),
            }),
            (None, Some(r)) => deltas.push(EnvDelta::Added {
                name: name.to_string(),
                right: r.clone(),
            }),
            (Some(l), Some(r)) if l != r => deltas.push(EnvDelta::Changed {
                name: name.to_string(),
                left: l.clone(),
                right: r.clone(),
            }),
            _ => {}
        }
    }

    EnvDivergence {
        left: left.column_label(),
        right: right.column_label(),
        interpretation: interpretation.into(),
        deltas,
    }
}

// ===========================================================================
// The launch-snapshot staleness check.
// ===========================================================================

/// One field of the launch snapshot that a re-read no longer agrees with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LaunchFieldDrift {
    /// The `RunnerLaunchEnv` field name.
    pub field: String,
    /// The value `main()`'s single `read()` captured.
    pub at_launch: EnvValue,
    /// The value the same parser produces from the process env right now.
    pub now: EnvValue,
}

/// `RunnerLaunchEnv` as captured in `main()` versus the same parser re-run now.
///
/// This comparison is exact — same type, same parsing — which is precisely why
/// the launch snapshot is compared against ITSELF rather than value-diffed
/// against a raw environment map: the snapshot holds `server_mode: false`, not
/// the string the operator exported, so a textual diff against the raw env
/// would manufacture differences that do not exist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LaunchSnapshotDrift {
    pub fields_compared: usize,
    pub differing: Vec<LaunchFieldDrift>,
    /// When `main()` took the snapshot.
    ///
    /// Not optional: a drift record only exists because a launch read exists,
    /// and that read is what stamps this. "No snapshot" is the ABSENCE of a
    /// `LaunchSnapshotDrift`, which the renderer states in its own words —
    /// modelling it twice would add an arm nothing can reach.
    pub captured_at_launch: DateTime<Utc>,
}

// ===========================================================================
// Spawn seams.
// ===========================================================================

/// What ONE of the eight spawn seams does to a child's environment.
///
/// Captured by CALLING the seam's own extracted env-construction function on a
/// throwaway `Command` and reading the overrides back — never by restating what
/// the source does. The functions are extracted and unit-tested precisely so
/// this is possible without touching the spawn path (see the eight-seam table
/// in [`crate::terminal`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeamEnvReport {
    /// The symbol, e.g. `session::TerminalSession::finalize_child_env`.
    pub seam: String,
    /// Which `Command` type it builds.
    pub command_type: String,
    /// The `scrub_credential_env_*` wrapper it ends with.
    pub scrub_wrapper: String,
    /// Names the seam SETS, with classified values.
    pub sets: Vec<EnvVarReading>,
    /// Names the seam CLEARS (removes from the child's env).
    pub clears: Vec<String>,
}

// ===========================================================================
// The whole section + its byte-stable renderer.
// ===========================================================================

/// Variables always shown in the side-by-side table, by prefix. The full
/// environment is 150-odd variables on this fleet; the divergence section below
/// covers ALL of them, so the table is scoped to the configuration surface a
/// reader is actually reasoning about.
pub const HIGHLIGHT_PREFIXES: &[&str] = &[
    "QONTINUI_",
    "CLAUDE",
    "COORD_",
    "ANTHROPIC_",
    "WEBVIEW2_",
    "RESTATE_",
];

/// Widest a table cell renders before it is elided. A safe value is never
/// hidden by this — it is truncated, and the truncation is announced.
const CELL_WIDTH: usize = 56;

/// The env-generation section of the config report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvGenerations {
    pub generations: Vec<EnvGeneration>,
    pub divergences: Vec<EnvDivergence>,
    pub launch_drift: Option<LaunchSnapshotDrift>,
    pub seams: Vec<SeamEnvReport>,
}

fn stamp(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Replace every control character with a printable escape.
///
/// Runs BEFORE any width computation in [`elide`], which is the whole point: a
/// literal `\n` inside a pipe-table cell breaks the row across two lines, and
/// [`pad`] then counts the newline as one column of width — so that row and
/// every column after it misalign, against a renderer whose contract is "two
/// machines' reports must differ only where their configuration differs".
///
/// This is not hypothetical-only. `QONTINUI_RUNNER_CONTEXT` is multi-line,
/// reaches the table through the `QONTINUI_` highlight prefix, and was latent
/// purely because its first line happened to be longer than [`CELL_WIDTH`] —
/// a shorter version string or short-sha moves the newline inside the elided
/// head and the table breaks.
fn escape_control(s: &str) -> String {
    if !s.chars().any(char::is_control) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything else that cannot be printed — including the C1 range
            // and the ANSI escape a colourised value can carry — becomes an
            // unambiguous, fixed-shape `\u{…}` so the cell width is exactly
            // what `chars().count()` says it is.
            c if c.is_control() => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn elide(s: &str) -> String {
    let s = escape_control(s);
    if s.chars().count() <= CELL_WIDTH {
        return s;
    }
    let head: String = s.chars().take(CELL_WIDTH - 1).collect();
    format!("{head}…")
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

impl EnvGenerations {
    /// Total variables withheld across every generation reading in this
    /// section. Stated in the output so a reader knows the rows exist.
    pub fn total_withheld(&self) -> usize {
        self.generations
            .iter()
            .map(|g| g.withheld_count())
            .sum::<usize>()
            + self
                .seams
                .iter()
                .map(|s| s.sets.iter().filter(|v| v.value.is_withheld()).count())
                .sum::<usize>()
    }

    /// Render the section. Deterministic: same inputs ⇒ same bytes.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push('\n');
        out.push_str("env generations — the same variable at three ages\n");
        out.push_str("=========================================================\n");
        out.push_str(
            "There is no single \"the environment\". Each generation below was frozen at a\n\
             different moment, and a value only reaches a Claude tool call through ALL of them.\n\n",
        );

        for g in &self.generations {
            out.push_str(&format!("{} {} — {}\n", g.id, g.name, g.describes));
            out.push_str(&format!("      captured_at: {}\n", stamp(g.captured_at)));
            out.push_str(&format!("      freshness:   {}\n", g.freshness));
            out.push_str(&format!(
                "      variables:   {} ({} withheld){}\n",
                g.vars.len(),
                g.withheld_count(),
                if g.is_full_env {
                    ""
                } else {
                    " — PARSED subset, never value-diffed against a raw env"
                }
            ));
        }

        out.push_str(&self.render_table());
        out.push_str(&self.render_launch_drift());
        for d in &self.divergences {
            out.push_str(&Self::render_divergence(d));
        }
        out.push_str(&self.render_seams());

        out.push_str("---------------------------------------------------------\n");
        out.push_str(&format!(
            "{} variable readings withheld across this section — their values were dropped at\n\
             classification and never reached this renderer.\n",
            self.total_withheld()
        ));
        out
    }

    /// The side-by-side table. Pipe-delimited on purpose: it is the shape an
    /// operator pastes into an issue, and it is the shape `SECRET_RE` cannot
    /// see — which is why nothing in it may depend on a text redaction pass.
    fn render_table(&self) -> String {
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for g in &self.generations {
            for v in &g.vars {
                if HIGHLIGHT_PREFIXES.iter().any(|p| v.name.starts_with(p)) {
                    names.insert(v.name.as_str());
                }
            }
        }
        if names.is_empty() {
            return String::new();
        }

        let headers: Vec<String> = self.generations.iter().map(|g| g.column_label()).collect();
        let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
        let mut name_width = "variable".len();

        let rows: Vec<(String, Vec<String>)> = names
            .iter()
            .map(|n| {
                let cells: Vec<String> = self
                    .generations
                    .iter()
                    .map(|g| match g.get(n) {
                        Some(v) => elide(&v.cell()),
                        None => "(absent)".to_string(),
                    })
                    .collect();
                ((*n).to_string(), cells)
            })
            .collect();

        for (name, cells) in &rows {
            name_width = name_width.max(name.chars().count());
            for (i, c) in cells.iter().enumerate() {
                widths[i] = widths[i].max(c.chars().count());
            }
        }

        // Lines are trimmed at the right: a padded final column would put
        // invisible trailing whitespace into a report an operator pastes into
        // an issue or diffs against another machine's, and a diff that lights
        // up on whitespace is a diff nobody reads.
        let mut line = |first: &str, cells: &[String]| -> String {
            let mut s = format!("  {}", pad(first, name_width));
            for (i, c) in cells.iter().enumerate() {
                s.push_str(&format!(" | {}", pad(c, widths[i])));
            }
            format!("{}\n", s.trim_end())
        };

        let mut out = String::new();
        out.push_str("\nside by side — the configuration surface\n");
        out.push_str(&line("variable", &headers));
        let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
        out.push_str(&{
            let mut s = format!("  {}", "-".repeat(name_width));
            for r in &rule {
                s.push_str(&format!("-+-{r}"));
            }
            format!("{s}\n")
        });
        for (name, cells) in &rows {
            out.push_str(&line(name, cells));
        }
        out
    }

    /// The launch-snapshot staleness block.
    ///
    /// Rendered even when there is nothing to report. A silently-omitted block
    /// reads as "checked, and no drift"; the two states are different and only
    /// one of them is a finding.
    fn render_launch_drift(&self) -> String {
        let mut out = String::new();
        out.push_str("\nlaunch snapshot vs a re-read now\n");
        let Some(drift) = &self.launch_drift else {
            out.push_str(
                "  NOT AVAILABLE — `RunnerLaunchEnv::read()` has never run in this process, so \
                 there is\n  no launch generation to compare a re-read against. This is the \
                 absence of a check,\n  not a finding that the snapshot is current.\n",
            );
            return out;
        };
        out.push_str(&format!(
            "  snapshot taken: {}\n",
            stamp(drift.captured_at_launch)
        ));
        if drift.differing.is_empty() {
            out.push_str(&format!(
                "  all {} launch fields still agree with a re-read — the runner's typed view of \
                 its\n  own launch env is current.\n",
                drift.fields_compared
            ));
        } else {
            out.push_str(&format!(
                "  {} of {} launch fields DIVERGE — the runner is acting on the launch value \
                 while a\n  fresh read of the same process env yields something else:\n",
                drift.differing.len(),
                drift.fields_compared
            ));
            for f in &drift.differing {
                out.push_str(&format!(
                    "  ~ {} — at launch: {} | now: {}\n",
                    f.field,
                    elide(&f.at_launch.detail()),
                    elide(&f.now.detail())
                ));
            }
        }
        out
    }

    fn render_divergence(d: &EnvDivergence) -> String {
        let mut out = String::new();
        out.push_str(&format!("\ndivergence {} → {}\n", d.left, d.right));
        if d.deltas.is_empty() {
            out.push_str("  (none — the two generations hold identical environments)\n");
        } else {
            for delta in &d.deltas {
                match delta {
                    EnvDelta::Removed { name, left } => out.push_str(&format!(
                        "  - {name} — {} in {}, ABSENT in {}\n",
                        elide(&left.detail()),
                        d.left,
                        d.right
                    )),
                    EnvDelta::Added { name, right } => out.push_str(&format!(
                        "  + {name} — ABSENT in {}, {} in {}\n",
                        d.left,
                        elide(&right.detail()),
                        d.right
                    )),
                    EnvDelta::Changed { name, left, right } => out.push_str(&format!(
                        "  ~ {name} — {}: {} | {}: {}\n",
                        d.left,
                        elide(&left.detail()),
                        d.right,
                        elide(&right.detail())
                    )),
                }
            }
            let withheld = d.deltas.iter().filter(|x| x.involves_withheld()).count();
            out.push_str(&format!(
                "  {} difference(s), {} of them credential-classed (compared by per-run keyed\n  \
                 fingerprint — the values themselves were never kept).\n",
                d.deltas.len(),
                withheld
            ));
        }
        out.push_str(&format!("  {}\n", d.interpretation));
        out
    }

    fn render_seams(&self) -> String {
        if self.seams.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("\nspawn seams — what each seam adds to or removes from a child env\n");
        out.push_str(
            "  Captured by calling each seam's own extracted env-construction function on a\n  \
             throwaway Command and reading the overrides back. Nothing here restates source.\n",
        );
        for (i, s) in self.seams.iter().enumerate() {
            out.push_str(&format!(
                "{:>2}. {} [{}] → {}\n",
                i + 1,
                s.seam,
                s.command_type,
                s.scrub_wrapper
            ));
            if s.sets.is_empty() {
                out.push_str("      sets:   (nothing)\n");
            } else {
                for v in &s.sets {
                    out.push_str(&format!(
                        "      sets:   {} = {}\n",
                        v.name,
                        elide(&v.value.detail())
                    ));
                }
            }
            if s.clears.is_empty() {
                out.push_str("      clears: (nothing)\n");
            } else {
                out.push_str(&format!("      clears: {}\n", s.clears.join(", ")));
            }
        }
        out
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_stamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-22T12:34:56.789Z")
            .expect("literal is valid RFC 3339")
            .with_timezone(&Utc)
    }

    /// An `OsString` that is NOT valid Unicode, built directly rather than
    /// through the process environment.
    ///
    /// Directly, because `std::env::set_var` is a process-global mutation that
    /// races every sibling test reading real settings and is the documented
    /// cause of this suite's existing flake — and because on Windows the
    /// setter would not accept an unpaired surrogate anyway. `from_wide` /
    /// `from_vec` construct exactly the value the OS can hand back from
    /// `vars_os` and `std::env::vars()` panics on.
    fn non_unicode_os_string(tag: &str) -> std::ffi::OsString {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            // 0xD800 is a HIGH surrogate with no following low surrogate — the
            // exact shape `String::from_utf16` rejects.
            let mut units: Vec<u16> = tag.encode_utf16().collect();
            units.push(0xD800);
            std::ffi::OsString::from_wide(&units)
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let mut bytes = tag.as_bytes().to_vec();
            bytes.push(0xFF); // never a valid UTF-8 byte
            std::ffi::OsString::from_vec(bytes)
        }
    }

    /// **The G1 capture survives a non-Unicode environment.**
    ///
    /// `config_report_cmd::env_generations_section` used to hand
    /// `std::env::vars()` to [`EnvGeneration::capture`]. That iterator PANICS —
    /// documented, not incidental — the moment any name or value is not valid
    /// Unicode: an unpaired UTF-16 surrogate on Windows, any non-UTF-8 byte on
    /// Linux. `config_report_run` is a `#[tauri::command]`, so the panic takes
    /// down the IPC handler, on exactly the machine whose environment is odd
    /// enough that someone opened the diagnostic to look at it.
    ///
    /// This drives such a name AND such a value through
    /// [`lossy_env_pairs`] into a real capture. The precondition assertion is
    /// what makes the test able to fail: it proves the fixture really is
    /// un-decodable, so the capture below is not passing on a well-formed
    /// string.
    ///
    /// The mangled value is a **credential**, not a benign one. With a benign
    /// fixture (`value-\u{FFFD}`) the withheld count was carried entirely by a
    /// separate well-formed `POSTGRES_PASSWORD`, so nothing here pinned what
    /// lossiness does to the classifier — and what it did was open a hole: a
    /// 32+ character mixed-case token carrying ONE mangled byte fell out of the
    /// entropy arm (`chars().all(token_charset)` was false) and rendered with
    /// every other character verbatim, on exactly the machine class
    /// [`lossy_env_pairs`] exists for. The mangled byte is INTERIOR, so no
    /// end-trimming can rescue it.
    ///
    /// The mangled NAME is the other direction, and it USED to be a residual
    /// asserted here as `None`: a byte mangled inside a credential token
    /// defeats `contains`, and a short value beside it has no other arm to fall
    /// into. It is now CLOSED by [`name_is_unreadable`] — the literal below is
    /// the closure, not the hole — and the reason is
    /// [`WithholdReason::NameUnreadable`] rather than `Name` precisely because
    /// the classifier cannot claim to have recognised a token it could not
    /// read.
    #[test]
    fn env_generations_g1_capture_survives_a_non_unicode_environment() {
        let bad_name = non_unicode_os_string("QONTINUI_WEIRD_");
        // `AbCdEf…678` + the un-decodable unit + `Ab` — 33 characters of
        // mixed-case entropy token with the mangling in the MIDDLE.
        let mut bad_value = non_unicode_os_string("AbCdEf0123456789AbCdEf012345678");
        bad_value.push("Ab");
        let bad_name_rendered = bad_name.to_string_lossy().into_owned();

        // The same token WITHOUT the mangling is withheld — the control that
        // makes the assertion below about the mangled one meaningful. Literals,
        // both of them.
        assert_eq!(
            value_is_credential("AbCdEf0123456789AbCdEf0123456789Ab"),
            Some(WithholdReason::ValueEntropy),
            "control: the un-mangled token must be a credential"
        );
        assert_eq!(
            value_is_credential("AbCdEf0123456789AbCdEf012345678\u{FFFD}Ab"),
            Some(WithholdReason::ValueEntropy),
            "a mangled entropy token must NOT fall out of the entropy arm"
        );

        // CLOSED, pinned: a mangled NAME loses its credential token and no
        // value arm rescues a short value beside it — so the LAST arm withholds
        // it on the unreadability of the name itself. The control above it is
        // the same name un-mangled, and it must still report the NAME arm: the
        // backstop must not swallow the more diagnostic answer.
        assert_eq!(
            classify_env_var("POSTGRES_PASSWORD", "hunter2"),
            Some(WithholdReason::Name {
                token: "PASSWORD".to_string()
            }),
            "control: the un-mangled name must be withheld by NAME"
        );
        assert_eq!(
            classify_env_var("POSTGRES_PASSW\u{FFFD}RD", "hunter2"),
            Some(WithholdReason::NameUnreadable),
            "a token mangled inside the NAME is closed by the unreadable-name arm"
        );

        // PRECONDITION — both fixtures must genuinely be un-decodable, or this
        // test proves nothing. This is the exact conversion `std::env::vars()`
        // performs before it panics.
        assert!(
            bad_name.clone().into_string().is_err(),
            "fixture precondition: the name must not be valid Unicode"
        );
        assert!(
            bad_value.clone().into_string().is_err(),
            "fixture precondition: the value must not be valid Unicode"
        );

        let pairs = lossy_env_pairs([
            (bad_name, bad_value),
            (
                std::ffi::OsString::from("QONTINUI_API_URL"),
                std::ffi::OsString::from("http://127.0.0.1:8000"),
            ),
            (
                std::ffi::OsString::from("POSTGRES_PASSWORD"),
                std::ffi::OsString::from("hunter2"),
            ),
        ]);
        assert_eq!(pairs.len(), 3);
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k.contains('\u{FFFD}') && v.contains('\u{FFFD}')),
            "the un-decodable pair must survive as replacement characters, got {pairs:?}"
        );

        let captured = EnvGeneration::capture(
            &EnvFingerprinter::new(),
            EnvGenerationSpec {
                id: "G1",
                name: "runner_process",
                describes: "test",
                freshness: "test",
                is_full_env: true,
            },
            fixed_stamp(),
            pairs,
        );
        assert_eq!(captured.vars.len(), 3);
        // The ordinary rows are unaffected — the lossy path is not a downgrade
        // of the classifier — and BOTH credentials are withheld: the
        // well-formed one and the mangled one.
        assert_eq!(captured.withheld_count(), 2);
        assert!(matches!(
            captured.get("POSTGRES_PASSWORD"),
            Some(EnvValue::Withheld { .. })
        ));
        assert!(
            matches!(
                captured.get(&bad_name_rendered),
                Some(EnvValue::Withheld {
                    reason: WithholdReason::ValueEntropy,
                    ..
                })
            ),
            "the mangled entropy token must be withheld: {:?}",
            captured.get(&bad_name_rendered)
        );
        assert!(matches!(
            captured.get("QONTINUI_API_URL"),
            Some(EnvValue::Shown { .. })
        ));
        let debug = format!("{captured:?}");
        assert!(!debug.contains("hunter2"), "got {debug}");
        // Not a prefix of the mangled token either — a reading that kept the
        // readable half would still be a leak.
        assert!(
            !debug.contains("AbCdEf0123456789AbCdEf012345678"),
            "got {debug}"
        );
    }

    /// The classifier catches every name class the plan enumerates, asserted as
    /// LITERAL names rather than by re-deriving them from
    /// [`CREDENTIAL_NAME_TOKENS`] — a test written against its own constant
    /// pins nothing.
    #[test]
    fn env_generations_classifier_withholds_every_enumerated_credential_name() {
        for name in [
            "POSTGRES_PASSWORD",
            "QONTINUI_OPERATOR2_PASSWORD",
            "QONTINUI_TEST_LOGIN_PASSWORD",
            "QONTINUI_TEST_AUTO_LOGIN_PASSWORD",
            "MYSQL_PASSWD",
            "SSH_PASSPHRASE",
            "CLIENT_SECRET",
            "GITHUB_TOKEN",
            "COORD_DEVICE_JWT",
            "SOME_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AUTHORIZATION",
            "HTTP_BEARER",
            "AWS_CREDENTIAL_FILE",
            "CLAUDE_SESSION_ID",
            "SESSIONID",
            "CSRF_NONCE",
            "COORD_PROXY_KEY",
            "DEV_NOTES_PAT",
            "PAT",
        ] {
            assert!(
                classify_env_var(name, "some-ordinary-value").is_some(),
                "{name} must be withheld by NAME"
            );
        }
    }

    /// `PATH` is the trap: `PAT` is a substring of it, and hiding `PATH` would
    /// wreck the diagnostic for no security gain. `*_PAT` is matched as a
    /// suffix precisely so this case survives.
    #[test]
    fn env_generations_classifier_does_not_withhold_ordinary_variables() {
        for (name, value) in [
            (
                "PATH",
                "C:/bin;C:/Windows/system32;C:/Program Files/Git/cmd",
            ),
            ("TERM", "xterm-256color"),
            ("QONTINUI_CONFIG_DIR", "C:/Users/x/AppData/Roaming/qontinui"),
            ("QONTINUI_INSTANCE_NAME", "temp-abc"),
            ("QONTINUI_API_URL", "http://127.0.0.1:8000"),
            // A git SHA is 40 chars of LOWER-case hex — no upper-case, so the
            // entropy arm leaves it alone. This value is one of the report's
            // most useful lines and must not be hidden.
            (
                "QONTINUI_GIT_SHA",
                "7bb1ed7b0c9a1f2e3d4c5b6a7988990011223344",
            ),
            ("USERNAME", "jspinak"),
            ("NUMBER_OF_PROCESSORS", "32"),
        ] {
            assert_eq!(
                classify_env_var(name, value),
                None,
                "{name} must NOT be withheld"
            );
        }
    }

    /// An unrecognised NAME whose VALUE has a credential shape is still
    /// withheld — the arm that catches a variable nobody enumerated.
    #[test]
    fn env_generations_classifier_withholds_on_value_shape_alone() {
        let cases: Vec<(&str, String, WithholdReason)> = vec![
            (
                "SOMETHING_HARMLESS",
                format!("{}{}", "eyJ", "hbGciOiJIUzI1NiJ9.abc.def"),
                WithholdReason::ValuePrefix {
                    prefix: "eyJ".to_string(),
                },
            ),
            (
                "MY_VAR",
                format!("{}{}", "ghp_", "0123456789abcdef"),
                WithholdReason::ValuePrefix {
                    prefix: "ghp_".to_string(),
                },
            ),
            (
                "OPENAI",
                format!("{}{}", "sk-", "liveXYZ"),
                WithholdReason::ValuePrefix {
                    prefix: "sk-".to_string(),
                },
            ),
            (
                "CLOUD_ID",
                format!("{}{}", "AKIA", "IOSFODNN7EXAMPLE"),
                WithholdReason::ValuePrefix {
                    prefix: "AKIA".to_string(),
                },
            ),
            (
                "BLOB",
                "aGVsbG9Xb3JsZDEyMzQ1Njc4OTBBQkNERUZH".to_string(),
                WithholdReason::ValueEntropy,
            ),
        ];
        for (name, value, expected) in cases {
            assert_eq!(
                classify_env_var(name, &value),
                Some(expected),
                "{name} must be withheld by VALUE"
            );
        }
    }

    /// **F1 regression.** A URL that carries a password in its userinfo is
    /// withheld, whatever it is called.
    ///
    /// The exact values are LITERALS and the arms are asserted by variant, not
    /// re-derived from the classifier — and the first case is the one that was
    /// printed verbatim before this arm existed. `value_is_credential`'s
    /// entropy test requires `chars().all(token_charset)` and that charset
    /// contains neither `:` nor `@`, so EVERY connection string short-circuited
    /// to "safe"; no prefix matched; `CREDENTIAL_NAME_TOKENS` has no `URL` /
    /// `URI` / `DSN` / `CONN` / `PROXY` entry. Meanwhile
    /// `QONTINUI_DATABASE_URL` matches the `QONTINUI_` highlight prefix, so the
    /// password went straight into the side-by-side table and
    /// `total_withheld()` did not count it.
    #[test]
    fn env_generations_classifier_withholds_a_url_with_an_embedded_password() {
        let cases: Vec<(&str, &str, WithholdReason)> = vec![
            (
                "QONTINUI_DATABASE_URL",
                "postgresql://qontinui:hunter2@localhost:5432/qontinui",
                WithholdReason::ValueUrlPassword,
            ),
            (
                "HTTPS_PROXY",
                "http://user:pass@proxy:8080",
                WithholdReason::ValueUrlPassword,
            ),
            (
                "REDIS_URL",
                "redis://default:s3cr3t@127.0.0.1:6379/0",
                WithholdReason::ValueUrlPassword,
            ),
            (
                "AMQP_URL",
                "amqp://guest:guest@rabbit:5672/%2f",
                WithholdReason::ValueUrlPassword,
            ),
            (
                "MONGODB_URI",
                "mongodb+srv://svc:pw@cluster0.example.net/db",
                WithholdReason::ValueUrlPassword,
            ),
            // Userinfo with NO password: the account name is still not a thing
            // to print next to its host, and the joint name+value arm catches
            // it. The arm is named so a reader can tell the two apart.
            (
                "QONTINUI_DATABASE_URL",
                "postgresql://qontinui@localhost:5432/qontinui",
                WithholdReason::NameUrlWithUserinfo {
                    token: "_URL".to_string(),
                },
            ),
            (
                "SOME_DSN",
                "https://abc@sentry.example.io/42",
                WithholdReason::NameUrlWithUserinfo {
                    token: "_DSN".to_string(),
                },
            ),
        ];
        for (name, value, expected) in cases {
            assert_eq!(
                classify_env_var(name, value),
                Some(expected),
                "{name}={value} must be withheld"
            );
        }
    }

    /// The other direction of F1: a URL with NO userinfo stays printable.
    ///
    /// Over-withholding is the correct failure direction for a *secret*, but a
    /// backend base URL is the single most-read line of this report and hiding
    /// it would break the diagnostic for no security gain — the same trade
    /// `PATH` gets. These are LITERALS for that reason.
    #[test]
    fn env_generations_classifier_leaves_a_userinfo_free_url_printable() {
        for (name, value) in [
            ("QONTINUI_API_URL", "http://127.0.0.1:8000"),
            ("QONTINUI_WEB_BACKEND_URL", "https://api.qontinui.io"),
            ("SOME_URL", "https://example.com/path?q=1#frag"),
            ("COORD_HTTP_URL", "https://coord.qontinui.io"),
            // An `@` that is NOT in the authority — it is in the path — is not
            // userinfo, and the parse must not be a substring search.
            ("DOCS_URL", "https://example.com/users/me@example.com"),
            // Not a URL at all, despite the `@`. (`GIT_AUTHOR_EMAIL` would be
            // withheld here — but by the pre-existing NAME arm, on the `AUTH`
            // inside `AUTHOR`, which is the documented over-withholding trade
            // and says nothing about the URL parse.)
            ("GIT_COMMITTER_EMAIL", "jspinak@hotmail.com"),
            // A scheme-less value with `://` inside it is not a URL either.
            ("WEIRD", "notascheme_://a:b@c"),
        ] {
            assert_eq!(
                classify_env_var(name, value),
                None,
                "{name}={value} must stay printable"
            );
        }
    }

    /// [`url_userinfo`] itself, arm by arm, with the RFC shape spelled out —
    /// so the parse cannot quietly degrade into "contains an @".
    #[test]
    fn env_generations_url_userinfo_parses_the_authority_not_the_string() {
        assert_eq!(
            url_userinfo("postgresql://u:p@h:5432/db"),
            Some(UrlUserinfo::WithPassword)
        );
        assert_eq!(
            url_userinfo("postgresql://u@h/db"),
            Some(UrlUserinfo::UserOnly)
        );
        // A declared-but-empty password is not a secret; the account is.
        assert_eq!(
            url_userinfo("postgresql://u:@h/db"),
            Some(UrlUserinfo::UserOnly)
        );
        // `@` after the authority is path content. These survive the widened
        // delimiter because the USERNAME half stays strict: the candidate
        // usernames are `h/p`, `h?x=a` and `h#a`, and no account name carries a
        // `/`, `?` or `#`.
        assert_eq!(url_userinfo("https://h/p@q"), None);
        assert_eq!(url_userinfo("https://h?x=a@b"), None);
        assert_eq!(url_userinfo("https://h#a@b"), None);
        // No authority separator at all.
        assert_eq!(url_userinfo("mailto:u@h"), None);
        // An illegal scheme.
        assert_eq!(url_userinfo("1http://u:p@h"), None);
        assert_eq!(url_userinfo("://u:p@h"), None);
        // Whitespace inside the USERNAME is prose, not a URL — `user ` is not
        // an account name, and admitting it would withhold every prose line
        // that mentions a URL and an email address. Whitespace inside the
        // PASSWORD is the opposite call and is caught; the two are asserted
        // side by side because the asymmetry is deliberate, and the row below
        // is the one that used to be missing (this `None` was previously the
        // by-product of a guard that ALSO returned `None` for `hunter 2`).
        assert_eq!(url_userinfo("https://user :pw@h"), None);
        assert_eq!(
            url_userinfo("postgresql://qontinui:hunter 2@localhost:5432/db"),
            Some(UrlUserinfo::WithPassword)
        );
        assert_eq!(
            url_userinfo("connect via https://gateway then ask bob@corp.com"),
            None
        );
        // Multiple `@`: userinfo is everything before the LAST one.
        assert_eq!(
            url_userinfo("scheme://us:p@ss@host/x"),
            Some(UrlUserinfo::WithPassword)
        );
    }

    /// **The three confirmed false negatives of the first-`://`-only,
    /// prefix-anchored shape.** Each is a real environment value shape, each was
    /// printed VERBATIM before, and each is invisible to the other two arms:
    /// [`value_is_credential`]'s entropy charset excludes `:` and `@`, and
    /// [`url_name_with_userinfo`] only runs when this function says `Some`.
    ///
    /// Asserted at three levels — the parser, the value arm, and the full
    /// [`classify_env_var`] with the variable's REAL name — because a fix that
    /// only moved the parser would still leave the report printing the password
    /// if the arms above it had drifted.
    #[test]
    fn env_generations_url_userinfo_scans_every_separator_not_just_the_first() {
        // (1) The credential-free URL comes FIRST — a real `PIP_EXTRA_INDEX_URL`.
        let pip = "https://pypi.org/simple https://ci:tok3n@pkgs.internal/simple";
        assert_eq!(url_userinfo(pip), Some(UrlUserinfo::WithPassword));
        assert_eq!(
            value_is_credential(pip),
            Some(WithholdReason::ValueUrlPassword)
        );
        assert_eq!(
            classify_env_var("PIP_EXTRA_INDEX_URL", pip),
            Some(WithholdReason::ValueUrlPassword)
        );

        // (2) A JDBC URL — the scheme slice `jdbc:postgresql` carries a `:`.
        let jdbc = "jdbc:postgresql://u:hunter2@h:5432/db";
        assert_eq!(url_userinfo(jdbc), Some(UrlUserinfo::WithPassword));
        assert_eq!(
            classify_env_var("SPRING_DATASOURCE_URL", jdbc),
            Some(WithholdReason::ValueUrlPassword)
        );

        // (3) A URL embedded in a flag — the scheme slice `--proxy=http` starts
        // with `-`. The NAME here carries no credential token and no `_URL`
        // suffix, so the value arm is the ONLY thing that can withhold it.
        let curl = "--proxy=http://user:pass@proxy:8080";
        assert_eq!(url_userinfo(curl), Some(UrlUserinfo::WithPassword));
        assert_eq!(
            classify_env_var("CURL_OPTS", curl),
            Some(WithholdReason::ValueUrlPassword)
        );

        // A userinfo-free URL appearing SECOND is still not a finding.
        assert_eq!(
            url_userinfo("https://pypi.org/simple https://mirror.internal/simple"),
            None
        );
        // …and a bare user (no password) in a later position is the weaker arm.
        assert_eq!(
            url_userinfo("https://a/b https://svc@internal/x"),
            Some(UrlUserinfo::UserOnly)
        );
        // WithPassword outranks UserOnly wherever both occur, in either order.
        assert_eq!(
            url_userinfo("https://svc@a/x https://u:pw@b/y"),
            Some(UrlUserinfo::WithPassword)
        );
        assert_eq!(
            url_userinfo("https://u:pw@b/y https://svc@a/x"),
            Some(UrlUserinfo::WithPassword)
        );

        // THE TRUE NEGATIVES the wider scan must not swallow — a `?`-query `@`,
        // a bare `user@host` with no scheme, an IPv6 authority, and a
        // scheme-less `//host`.
        for negative in [
            "scheme://host/path?q=a:b@c",
            "user@host",
            "http://[::1]:8080/",
            "//host",
            "not a url at all: a@b",
            "C:/Users/x/AppData;D:/tools/bin",
        ] {
            assert_eq!(
                url_userinfo(negative),
                None,
                "{negative} is not a userinfo URL"
            );
        }
    }

    /// **The whole [`url_userinfo`] contract in one table** — every row an
    /// EXPECTED LITERAL, so the table can only be satisfied by the parse and
    /// never by the constants the parse is built from.
    ///
    /// It exists because the previous two tests covered the parse in one
    /// direction each and left the interesting boundary — a password carrying a
    /// structural character or a space — untested in BOTH. Every shipped
    /// credentialled fixture happened to put a path (`/simple`, `/x`, `/y`,
    /// `/db`) on the URL, so the authority always truncated at that `/` before
    /// any password-borne `/`, `?`, `#` or space could be reached, and the five
    /// LEAKS group below all returned `None` — a printed password counted as
    /// withheld by `total_withheld()`.
    ///
    /// The groups are the things that can go wrong, and each has to be able to
    /// fail on its own:
    ///
    /// - **LEAKS** — must be `Some`. A regression here PRINTS A PASSWORD. The
    ///   later families (empty username, `@` in the username, RFC 3986
    ///   sub-delims, non-ASCII) are the fourth confirmed round of these: the
    ///   username shape test rejected every one of them, and a rejected
    ///   candidate `@` falls through to earlier `@`s that cannot parse either
    ///   (an earlier `@`'s host span contains the later `@`, which `host_char`
    ///   excludes), so the function returned `None` and the password printed.
    /// - **RESIDUAL** — must stay `None`, and says so out loud. `,` and `=` are
    ///   held out of the username charset because they are what kills the
    ///   OVER-WITHHOLDS group; the cost is a named, visible hole.
    /// - **OVER-WITHHOLDS** — must be `None`. A regression here hides a
    ///   diagnostic line for no security gain.
    /// - **PINNED NEGATIVES** — must stay `None`. These are the shapes the
    ///   widened parse is most likely to swallow, so they are re-asserted here
    ///   rather than only where they were first written.
    ///
    /// Every `WithPassword` row is then re-asserted through `classify_env_var`
    /// and [`EnvVarReading::classify`] under a name NO other arm can rescue, so
    /// a fix that moved only the parser cannot pass.
    #[test]
    fn env_generations_url_userinfo_table_covers_both_failure_directions() {
        // (name, value, expected) — `name` is the group, for the failure message.
        let rows: &[(&str, &str, Option<UrlUserinfo>)] = &[
            // ---- LEAKS: confirmed `None` before the `@`-anchored delimiter --
            // A `#` INSIDE the password. The old authority was `admin:p`.
            (
                "leak/hash-in-password",
                "postgres://admin:p#ssw0rd@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            // A `/` INSIDE the password. The old authority was `admin:pa`.
            (
                "leak/slash-in-password",
                "postgres://admin:pa/ss@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            // Credentialled URL FIRST, no `/?#` before the space, so the old
            // whitespace guard rejected the whole authority.
            (
                "leak/credentialled-url-first",
                "https://ci:tok3n@pkgs.internal https://pypi.org/simple",
                Some(UrlUserinfo::WithPassword),
            ),
            // Same guard, via a space-separated flag rather than `--proxy=`.
            (
                "leak/space-separated-flag",
                "--proxy http://user:pass@proxy:8080 --silent",
                Some(UrlUserinfo::WithPassword),
            ),
            // A SPACE inside the password — the row that makes the asymmetry
            // with `https://user :pw@h` deliberate rather than accidental.
            (
                "leak/space-in-password",
                "postgresql://qontinui:hunter 2@localhost:5432/db",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- LEAKS: an EMPTY username beside a real password ----------
            // The `user.is_empty()` reject printed all four of these. The first
            // is the canonical pre-ACL Redis form, and this crate CONSTRUCTS
            // `redis://` URLs and exports `REDIS_URL` itself
            // (`ci_node::services`, `ci_node::executor`, `bin/qontinui_profile`).
            (
                "leak/empty-user-redis",
                "redis://:s3cretpw@127.0.0.1:6379/0",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/empty-user-rediss",
                "rediss://:s3cretpw@cache.internal:6380",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/empty-user-amqp",
                "amqp://:guestpw@rabbit.internal:5672/%2f",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/empty-user-git-token",
                "https://:ghp_A1b2C3d4E5f6G7h8@github.com/o/r.git",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- LEAKS: an `@` INSIDE the username ------------------------
            // Azure Database for PostgreSQL/MySQL Single Server MANDATES
            // `user@servername`; email-as-account is normal on Atlas and
            // Snowflake. Both were rejected by the username charset and printed.
            (
                "leak/at-in-username-azure",
                "postgres://myadmin@mydemoserver:mypassword@srv.postgres.database.azure.com:5432/db",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/at-in-username-atlas",
                "mongodb+srv://ops@corp.com:hunter2@cluster0.mongodb.net/test",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- LEAKS: RFC 3986 sub-delims in the username ---------------
            // `!$&'()*+,;=` are legal unencoded in userinfo. Each admitted one
            // gets its OWN row: a single combined fixture would pass as soon as
            // any one of them was admitted, and the point is that each was a
            // printed password on its own.
            (
                "leak/subdelim-apostrophe",
                "postgres://o'brien:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-bang",
                "postgres://svc!x:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-dollar",
                "postgres://svc$x:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-ampersand",
                "postgres://svc&x:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-semicolon",
                "postgres://svc;x:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-parens",
                "postgres://svc(x):hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/subdelim-star",
                "postgres://svc*x:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- LEAKS: non-ASCII in either half --------------------------
            // An ASCII-only charset reads an IDN host as "not a URL" and prints
            // the password. Over-withholding is the direction to err in, so
            // non-ASCII is admitted in BOTH halves.
            (
                "leak/idn-host",
                "https://u:pw@münchen.example.com/x",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/idn-host-distinct-password",
                "https://u:idnpassw0rd@münchen.example.com/x",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/non-ascii-username",
                "postgres://jörg:hunter2@db.internal:5432/app",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- LEAKS: a MULTI-HOST authority ----------------------------
            // The host span runs from the `@` to the first `/?#`/whitespace, so
            // a multi-host authority puts a `,` INSIDE it. The host charset
            // omitted `,` while the username charset had been widened to the
            // sub-delims, and the two halves disagreeing printed all four of
            // these verbatim. Each is paired with its SINGLE-host control
            // immediately below it: the control returned `WithPassword`
            // throughout, so the comma is the only difference between a found
            // password and a printed one.
            (
                "leak/multihost-postgres",
                "postgresql://qontinui:hunter2@pg1.internal:5432,pg2.internal:5432/qontinui?target_session_attrs=primary",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "control/singlehost-postgres",
                "postgresql://qontinui:hunter2@pg1.internal:5432/qontinui?target_session_attrs=primary",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "leak/multihost-mongo-replicaset",
                "mongodb://appuser:hunter2@h1.internal:27017,h2.internal:27017,h3.internal:27017/app?replicaSet=rs0",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "control/singlehost-mongo",
                "mongodb://appuser:hunter2@h1.internal:27017/app?replicaSet=rs0",
                Some(UrlUserinfo::WithPassword),
            ),
            // Sentinel, and the empty-username form as well — the two widenings
            // have to compose, not merely each work alone.
            (
                "leak/multihost-redis-sentinel",
                "redis://:s3cretpw@s1.internal:26379,s2.internal:26379/0",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "control/singlehost-redis-sentinel",
                "redis://:s3cretpw@s1.internal:26379/0",
                Some(UrlUserinfo::WithPassword),
            ),
            // No path at all, so the host span runs to the END of the value.
            (
                "leak/multihost-kafka-brokers",
                "kafka://svc:s3cret@b1.internal:9092,b2.internal:9092",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "control/singlehost-kafka-broker",
                "kafka://svc:s3cret@b1.internal:9092",
                Some(UrlUserinfo::WithPassword),
            ),
            // ---- RESIDUAL: a NAMED hole, still `None` on purpose ----------
            // `,` and `=` are the two sub-delims held out of the username
            // charset — they are what kills the three OVER-WITHHOLDS rows
            // below, and `=` is additionally what keeps the admitted `;` safe
            // against semicolon-joined key/value lists. A username containing
            // one therefore still leaks. Pinned so the hole is VISIBLE; if a
            // later cut closes it, delete this group deliberately rather than
            // discovering it as a surprise failure.
            (
                "residual/comma-in-username",
                "postgres://svc,x:hunter2@db.internal:5432/app",
                None,
            ),
            (
                "residual/equals-in-username",
                "postgres://svc=x:hunter2@db.internal:5432/app",
                None,
            ),
            // The HOST half's own named residual, and the reason it is a
            // residual rather than a fix: an option-carrying authority puts
            // `;` and `=` after the port, and admitting those to the host
            // charset is what would withhold the three `over/*-with-user` rows
            // below. This fleet produces no such URL, and the STANDARD SQL
            // Server spelling (no userinfo) is caught anyway — see the row two
            // groups down. Pinned so the hole is VISIBLE.
            (
                "residual/option-authority-semicolon",
                "sqlserver://sa:P@ssw0rd@host:1433;encrypt=true",
                None,
            ),
            // ---- OVER-WITHHOLDS: confirmed `Some` before the shape tests ----
            // Read `api.example.com","contact` as userinfo.
            (
                "over/json-object",
                r#"{"url":"https://api.example.com","contact":"ops@example.com"}"#,
                None,
            ),
            (
                "over/comma-joined",
                "https://a.example.com,mailto:ops@example.com",
                None,
            ),
            (
                "over/key-value-list",
                "service.name=api,repo=https://github.com,owner=a@b.com",
                None,
            ),
            // The three rows that pin the HOST charset's exclusions. Each one
            // has a username that PASSES (`git`, `svc`) and is held at `None`
            // by the host half alone.
            //
            // What each is sensitive to differs, and the difference is the
            // point of having three. `over/key-value-list-with-user` fails the
            // moment `=` is admitted to the host charset (its host span is
            // `github.com,owner=x`); `over/quoted-shell-url` fails the moment
            // `'` is (`host'`). `over/semicolon-key-value-list-with-user` fails
            // on NEITHER alone — its host span is `github.com;x=1`, so it needs
            // `;` AND `=` — and it therefore pins full-RFC alignment rather
            // than either character on its own.
            //
            // Without them the row above would pass while its own class
            // regressed: it survives full RFC alignment only by accident, on
            // the trailing `@b.com` that pushes the last candidate into the
            // username check.
            (
                "over/key-value-list-with-user",
                "service.name=api,repo=https://git@github.com,owner=x",
                None,
            ),
            (
                "over/semicolon-key-value-list-with-user",
                "k=v;repo=https://git@github.com;x=1",
                None,
            ),
            ("over/quoted-shell-url", "curl 'https://svc@host'", None),
            // ---- PINNED NEGATIVES ---------------------------------------
            ("neg/query-at", "scheme://host/path?q=a:b@c", None),
            ("neg/bare-user-host", "user@host", None),
            ("neg/ipv6-no-userinfo", "http://[::1]:8080/", None),
            ("neg/scheme-less-authority", "//host", None),
            ("neg/illegal-scheme-char", "notascheme_://a:b@c", None),
            // ---- and the properties the fix must NOT have cost ------------
            // Brackets are legal in a HOST, so an IPv6 authority that DOES
            // carry a password is still found. A literal reject of `[`/`]`
            // anywhere in the authority would have lost this one.
            (
                "keep/ipv6-with-password",
                "http://u:p@[::1]:8080/db",
                Some(UrlUserinfo::WithPassword),
            ),
            (
                "keep/user-only",
                "postgresql://qontinui@localhost:5432/qontinui",
                Some(UrlUserinfo::UserOnly),
            ),
            (
                "keep/empty-password",
                "postgresql://u:@h/db",
                Some(UrlUserinfo::UserOnly),
            ),
            // The STANDARD SQL Server spelling carries its credentials as
            // authority PROPERTIES and no userinfo at all — and is still found,
            // because the `@` in the password makes `host` the username and
            // `1433;user=sa;password=P` the password. This is what makes the
            // `residual/option-authority-semicolon` row above a narrow hole
            // rather than a whole unprotected family.
            (
                "keep/sqlserver-property-form",
                "jdbc:sqlserver://host:1433;user=sa;password=P@ssw0rd",
                Some(UrlUserinfo::WithPassword),
            ),
            // An empty username is admitted only BESIDE a declared password.
            // These two carry neither an account name nor a secret, so they
            // stay non-findings — and they are what stops the empty-username
            // fix above from degrading into "any `@` after a `://`".
            ("keep/empty-userinfo", "https://@h/x", None),
            ("keep/empty-userinfo-empty-password", "https://:@h/x", None),
        ];

        for (group, value, expected) in rows {
            assert_eq!(
                url_userinfo(value),
                *expected,
                "{group}: url_userinfo({value:?})"
            );
        }

        // EVERY `WithPassword` row must also reach the REPORT, not merely the
        // parser — the three arms above `url_userinfo` are what actually
        // withhold, and two of them are gated on it. The name is deliberately
        // one the OTHER arms cannot rescue: `QONTINUI_CONN_PROBE` matches no
        // `CREDENTIAL_NAME_TOKENS` entry, ends with no `URL_NAME_TOKENS`
        // suffix, and the entropy charset admits neither `:` nor `@` — so the
        // VALUE-shape arm is the only thing that can fire. Driven off the table
        // itself rather than a retyped list, so a row added above cannot be
        // added to the parser check alone.
        //
        // Every password substring in the table, checked against EVERY
        // reading's Debug — not just the one it came from, so a reading that
        // picked up another row's secret is caught too. Substrings rather than
        // whole values: a check on the whole value would pass for a reading
        // that carried the password with one byte changed.
        let secrets = [
            "ssw0rd",
            "pa/ss",
            "tok3n",
            "hunter",
            "s3cretpw",
            "guestpw",
            "ghp_A1b2C3d4E5f6G7h8",
            "mypassword",
            "idnpassw0rd",
            "pass",
            "s3cret",
        ];
        let mut checked = 0usize;
        for (group, value, expected) in rows {
            if *expected != Some(UrlUserinfo::WithPassword) {
                continue;
            }
            checked += 1;
            assert_eq!(
                classify_env_var("QONTINUI_CONN_PROBE", value),
                Some(WithholdReason::ValueUrlPassword),
                "{group}: {value} must be withheld by the report, not just parsed"
            );
            let reading =
                EnvVarReading::classify(&EnvFingerprinter::new(), "QONTINUI_CONN_PROBE", value);
            assert!(
                reading.value.is_withheld(),
                "{group}: got {:?}",
                reading.value
            );
            let debug = format!("{reading:?}");
            for secret in secrets {
                assert!(
                    !debug.contains(secret),
                    "{group}: the reading must not carry {secret:?} anywhere: {debug}"
                );
            }
        }
        // The loop above is only as strong as the number of rows it reached —
        // a filter that selected nothing would assert nothing, and that is the
        // vacuity this whole test exists to refuse. A LITERAL, so adding a
        // `WithPassword` row above is a deliberate bump here rather than a
        // silent widening.
        assert_eq!(
            checked, 31,
            "every WithPassword row must be re-asserted through the classifier"
        );

        // The pre-existing `_URL`-named path, kept as the second arm's control:
        // this one WOULD also be caught by the name+userinfo arm, and it is the
        // contrast that makes `QONTINUI_CONN_PROBE` above the sharper fixture.
        assert_eq!(
            classify_env_var(
                "QONTINUI_DATABASE_URL",
                "postgresql://qontinui:hunter 2@localhost:5432/db"
            ),
            Some(WithholdReason::ValueUrlPassword),
        );

        // The name-less arm too: `CURL_OPTS` carries no credential token and no
        // `_URL` suffix, so only the VALUE arm can withhold it.
        assert_eq!(
            classify_env_var("CURL_OPTS", "--proxy http://user:pass@proxy:8080 --silent"),
            Some(WithholdReason::ValueUrlPassword)
        );
    }

    /// **F6 regression.** The plain classifier is INERT on prose — its entropy
    /// arm dies on the first space — so free text goes through
    /// [`EnvVarReading::classify_free_text`], which tokenises.
    ///
    /// The first assertion is the CONTROL: it shows the plain classifier
    /// missing the secret. If that ever starts passing, the premise changed and
    /// this test must be re-derived rather than deleted.
    #[test]
    fn env_generations_free_text_classifier_catches_a_secret_the_whole_value_test_misses() {
        let fp = EnvFingerprinter::new();
        // Split so the source carries no contiguous high-entropy literal next to a
        // credential keyword — gitleaks' `generic-api-key` rule fires on that shape.
        // `concat!` is compile-time, so the value and type are unchanged.
        let secret = concat!("AbCdEf0123456789", "AbCdEf0123456789xyz");
        let message =
            format!("parse failed: invalid type: string \"{secret}\", expected u16 at line 4");

        // (1) The control — the whole-value classifier does NOT see it.
        assert_eq!(
            classify_env_var("settings_load_error", &message),
            None,
            "premise broken: the whole-value classifier now sees an embedded secret — \
             re-derive this control"
        );

        // (2) The free-text classifier does, and withholds the WHOLE string.
        let reading = EnvVarReading::classify_free_text(&fp, "settings_load_error", &message);
        assert!(reading.value.is_withheld(), "got {:?}", reading.value);
        assert!(!reading.value.detail().contains(secret));
        assert!(!format!("{reading:?}").contains(secret));

        // A connection string embedded in prose is caught by BOTH classifiers.
        //
        // RE-DERIVED, per this test's own instruction. This assertion used to
        // read `assert_eq!(classify_env_var(…), None)` — the whole-value
        // classifier missing an embedded connection string — and widening
        // [`url_userinfo`] to scan every `://` and anchor the scheme by walking
        // BACKWARDS made it start passing. That is a strict improvement, not a
        // regression: the URL arm no longer requires the value to BEGIN with the
        // scheme, so a URL embedded anywhere in a larger string is now found.
        //
        // The control the test is named for is (1) above and is untouched: the
        // ENTROPY arm still dies on the first space, so a bare high-entropy token
        // inside prose is still invisible to the whole-value classifier and still
        // needs `classify_free_text`. The tokenising path is therefore still
        // load-bearing — it just no longer carries the URL case alone.
        let url_msg =
            "read failed: could not open postgresql://q:hunter2@localhost/db (os error 5)";
        assert_eq!(
            classify_env_var("settings_load_error", url_msg),
            Some(WithholdReason::ValueUrlPassword),
            "the widened URL arm finds a connection string embedded in prose"
        );
        assert!(
            EnvVarReading::classify_free_text(&fp, "settings_load_error", url_msg)
                .value
                .is_withheld(),
            "an embedded connection string must be caught in prose too"
        );

        // …and ORDINARY prose stays readable — an error message nobody can act
        // on is the failure mode this whole layer exists to avoid.
        let plain = "parse failed: JSON Data error at line 4 column 12";
        let ok = EnvVarReading::classify_free_text(&fp, "settings_load_error", plain);
        assert!(!ok.value.is_withheld(), "got {:?}", ok.value);
        assert_eq!(ok.value.detail(), plain);
    }

    /// Every [`CREDENTIAL_VALUE_PREFIXES`] entry, with a REAL sample value —
    /// held as `(prefix, suffix)` and concatenated at run time — plus what
    /// happens to that sample when its prefix is broken.
    ///
    /// # Why the samples are split
    ///
    /// Written whole, four of these rows are literal credential shapes that
    /// GitHub push protection detects in the SOURCE and rejects the push for.
    /// Push protection scans source text, not runtime values, so
    /// `format!("{prefix}{suffix}")` rebuilds a BYTE-IDENTICAL sample from two
    /// halves neither of which matches a detector. Every row is split the same
    /// way, including the twelve that did not trip anything: a table with four
    /// special cases invites the next prefix to be added whole and rejected
    /// again.
    ///
    /// The concatenation must reproduce each sample EXACTLY. `expected_backstop`
    /// below is a measurement of the entropy arm against these precise values —
    /// it needs `len() >= 32` plus a lower-case, an upper-case and a digit — so
    /// a suffix edited by even one character silently changes what the row
    /// tests.
    ///
    /// `broken_still_caught` is the entropy arm's reach, measured rather than
    /// assumed: `Some` where a 32+ character mixed-case token carries the value
    /// even with the prefix destroyed, `None` where the prefix arm is the ONLY
    /// defence. The four `None` rows — `xoxp-`'s short form, `AKIA`, `ASIA` and
    /// `-----BEGIN` — are why a leading U+FFFD defeating `starts_with` is a
    /// leak and not a degradation.
    const PREFIX_SAMPLES: &[(&str, &str, Option<WithholdReason>)] = &[
        (
            "eyJ",
            "hbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "gho_",
            "16C7e42F292c6912E7710c838347Ae178B4a",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "ghp_",
            "16C7e42F292c6912E7710c838347Ae178B4a",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "ghs_",
            "16C7e42F292c6912E7710c838347Ae178B4a",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "ghu_",
            "16C7e42F292c6912E7710c838347Ae178B4a",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "ghr_",
            "16C7e42F292c6912E7710c838347Ae178B4a",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "github_pat_",
            "11ABCDE0Y0abcdefghijkl_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "sk-",
            "ABCdef0123456789ABCdef0123456789ABCdef01",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "sk_live_",
            "ABCdef0123456789ABCdef01",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "sk_test_",
            "ABCdef0123456789ABCdef01",
            Some(WithholdReason::ValueEntropy),
        ),
        (
            "xoxb-",
            "1234567890-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx",
            Some(WithholdReason::ValueEntropy),
        ),
        // NO BACKSTOP — the SHORT `xoxp-` form, 17 characters, well under the
        // entropy arm's 32-byte floor. (Long `xoxp-` tokens exist and the
        // entropy arm does reach those; this row pins the short one, which is
        // the shape with nothing underneath it.)
        ("xoxp-", "1234567890AB", None),
        // NO BACKSTOP — 20 characters, and upper-case + digits only, so the
        // entropy arm fails BOTH its length floor and its lower-case test.
        ("AKIA", "IOSFODNN7EXAMPLE", None),
        ("ASIA", "IOSFODNN7EXAMPLE", None),
        (
            "AIza",
            "SyA1B2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q",
            Some(WithholdReason::ValueEntropy),
        ),
        // NO BACKSTOP — 31 characters, one under the floor, and it carries
        // spaces so `chars().all(token_charset)` fails too.
        ("-----BEGIN", " RSA PRIVATE KEY-----", None),
    ];

    /// The same sample with its first character replaced, so the prefix no
    /// longer matches and only the entropy arm can answer. `Z` is in
    /// [`token_base_char`], so the mutation changes the PREFIX and nothing
    /// else about the token's shape.
    fn with_broken_prefix(sample: &str) -> String {
        let mut chars = sample.chars();
        chars.next();
        format!("Z{}", chars.as_str())
    }

    /// **F1 regression, round 6.** A credential PREFIX is still found when the
    /// value arrives with U+FFFD glued to either end.
    ///
    /// # The defect this pins, and why it was fix-induced
    ///
    /// Round 5 closed the entropy arm's mangled-token hole by admitting U+FFFD
    /// to [`token_charset`]. [`trim_token`] shared that charset, so U+FFFD
    /// stopped being trimmed, and a LEADING one then survived into
    /// [`value_is_credential`]'s `starts_with` PREFIX arm, which it defeats
    /// outright. Executed against the round-5 body, with a Windows env value
    /// quoted in CP-1252 (`0x93`/`0x94`) coming through [`lossy_env_pairs`]:
    ///
    /// | body | `classify_free_text(…, "read failed: bad value \u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD} in profile")` |
    /// |---|---|
    /// | round 4 (trim charset excluded U+FFFD) | `ValuePrefix { prefix: "AKIA" }` |
    /// | round 5 (one shared charset) | **`None`** — the key id rendered verbatim |
    ///
    /// The fix is [`token_boundary_char`]: one shared base, one named
    /// difference, in the two directions the two consumers actually need.
    ///
    /// # Why the four `None` rows of [`PREFIX_SAMPLES`] carry the test
    ///
    /// The entropy arm is not a general backstop for a defeated prefix. It
    /// needs `len() >= 32` AND an ASCII lower-case character. This test
    /// MEASURES that per prefix instead of asserting it in prose: each row is
    /// re-run with its prefix broken, and the four rows that go to `None` are
    /// the shapes with no second line of defence at all.
    #[test]
    fn env_generations_credential_prefix_survives_a_mangled_boundary() {
        let fp = EnvFingerprinter::new();

        // The table must cover the constant EXACTLY, or a prefix added later
        // is silently untested. Asserted both ways.
        assert_eq!(
            PREFIX_SAMPLES.len(),
            CREDENTIAL_VALUE_PREFIXES.len(),
            "every credential prefix needs a mangled-boundary row"
        );
        for p in CREDENTIAL_VALUE_PREFIXES {
            assert!(
                PREFIX_SAMPLES.iter().any(|(prefix, _, _)| prefix == p),
                "no mangled-boundary row for prefix {p:?}"
            );
        }

        let mut no_backstop = 0usize;
        for (prefix, suffix, broken_still_caught) in PREFIX_SAMPLES {
            // The guard changed shape with the table. While the sample was
            // stored whole, `sample.starts_with(prefix)` caught a mistyped
            // sample; now that the sample is BUILT from the prefix it would be
            // true by construction and catch nothing. What still has teeth is
            // the suffix: it must carry something, and it must not REPEAT the
            // prefix — `("sk-", "sk-ABCdef…")` would build `sk-sk-ABCdef…` and
            // the row would quietly measure a value other than the one it names.
            assert!(
                !suffix.is_empty() && !suffix.starts_with(prefix),
                "{prefix:?}: the suffix must be non-empty and must not repeat the prefix, or \
                 every row below measures a value other than the one it names"
            );
            let sample = format!("{prefix}{suffix}");
            let expected = Some(WithholdReason::ValuePrefix {
                prefix: (*prefix).to_string(),
            });

            // (1) The control: unmangled, the prefix arm answers.
            assert_eq!(
                value_is_credential(&sample),
                expected,
                "control: {prefix:?} must be found unmangled"
            );

            // (2) The three mangled boundaries, through all three doors.
            for mangled in [
                format!("\u{FFFD}{sample}"),
                format!("{sample}\u{FFFD}"),
                format!("\u{FFFD}{sample}\u{FFFD}"),
            ] {
                assert_eq!(
                    value_is_credential(&mangled),
                    expected,
                    "{prefix:?}: value_is_credential missed a mangled boundary"
                );
                // The NAME is deliberately unremarkable: no credential token,
                // no `_PAT` suffix, no `_URL`/`_URI`/`_DSN` suffix, and no
                // U+FFFD of its own — so ONLY the value arm can withhold it.
                assert_eq!(
                    classify_env_var("MYSTERY_FIELD", &mangled),
                    expected,
                    "{prefix:?}: classify_env_var missed a mangled boundary"
                );
                let prose = format!("read failed: bad value {mangled} in profile");
                let reading = EnvVarReading::classify_free_text(&fp, "MYSTERY_FIELD", &prose);
                assert_eq!(
                    reading.value,
                    EnvValue::Withheld {
                        reason: expected.clone().unwrap(),
                        fingerprint: fp.fingerprint(&prose),
                    },
                    "{prefix:?}: classify_free_text missed a mangled boundary"
                );
                assert!(
                    !reading.value.detail().contains(sample.as_str()),
                    "{prefix:?}: the sample survived into the rendered detail"
                );
            }

            // (3) The entropy arm's ACTUAL reach, measured per prefix.
            let broken = with_broken_prefix(&sample);
            assert!(
                !broken.starts_with(prefix),
                "{prefix:?}: the mutation must really break the prefix"
            );
            assert_eq!(
                value_is_credential(&broken),
                *broken_still_caught,
                "{prefix:?}: the entropy arm's reach is not what the table claims"
            );
            if broken_still_caught.is_none() {
                no_backstop += 1;
            }
        }

        // A LITERAL. Four prefixes have no second line of defence, so widening
        // the entropy arm (or shrinking this set) is a deliberate bump here
        // rather than a silent change to how much the prefix arm is carrying.
        assert_eq!(
            no_backstop, 4,
            "exactly four prefixes are defended by the prefix arm ALONE"
        );
    }

    /// The two token charsets are one base plus ONE named difference, in
    /// opposite directions — and a future edit cannot fork them silently.
    ///
    /// This is the structural half of the F1 fix. The two consumers ask
    /// different questions (what may appear INSIDE a token versus at its
    /// BOUNDARY) and had been collapsed onto one answer, which broke the prefix
    /// arm; before that they were two independent literals, which is how
    /// [`url_userinfo`]'s username and host charsets came to disagree and print
    /// four connection strings. The assertion is that they agree EVERYWHERE
    /// except U+FFFD — so neither a fork nor a re-merge can pass.
    #[test]
    fn env_generations_token_charsets_differ_only_at_the_replacement_character() {
        // U+FFFD is the one disagreement, in both directions.
        assert!(
            token_charset('\u{FFFD}'),
            "interior admits the mangled byte"
        );
        assert!(
            !token_boundary_char('\u{FFFD}'),
            "the boundary charset must strip it, or the PREFIX arm cannot see past it"
        );
        assert!(!token_base_char('\u{FFFD}'), "the shared base is neutral");

        // Everywhere else the two are the same function. The sweep covers all
        // of ASCII plus a spread of non-ASCII, including the neighbours of
        // U+FFFD, so a charset that started admitting (say) `:` or `@` on one
        // side only would fail here.
        let sweep = (0u32..=0x7F)
            .chain([0xA0, 0xE9, 0x3A9, 0x4E2D, 0xFFFC, 0xFFFE, 0x1F600, 0x10FFFF])
            .filter_map(char::from_u32);
        for c in sweep {
            assert_eq!(
                token_charset(c),
                token_boundary_char(c),
                "the two charsets may differ ONLY at U+FFFD, but they differ at {c:?}"
            );
        }

        // And the base is exactly what both agree on — asserted as LITERALS,
        // not re-derived from either function.
        for c in "AZaz09+/=_-.".chars() {
            assert!(token_base_char(c), "{c:?} must be in the shared base");
        }
        for c in ":@ ,;'\"()[]{}\\|?#!$&*~%".chars() {
            assert!(!token_base_char(c), "{c:?} must NOT be in the shared base");
        }

        // The trims that read them: same string, two answers, both needed.
        assert_eq!(
            trim_token("\u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD}"),
            "AKIAIOSFODNN7EXAMPLE"
        );
        assert_eq!(
            trim_token_interior("\u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD}"),
            "\u{FFFD}AKIAIOSFODNN7EXAMPLE\u{FFFD}"
        );
        // Both strip ordinary prose punctuation — that is the shared base.
        assert_eq!(
            trim_token("\"AKIAIOSFODNN7EXAMPLE\","),
            "AKIAIOSFODNN7EXAMPLE"
        );
        assert_eq!(
            trim_token_interior("\"AKIAIOSFODNN7EXAMPLE\","),
            "AKIAIOSFODNN7EXAMPLE"
        );
    }

    /// **The mangled-NAME residual, closed.** A value whose NAME carries U+FFFD
    /// is withheld, whatever the value looks like.
    ///
    /// The other two residuals this module pins (the username `,`/`=` one and
    /// the host `;`/`=` one) each DEFEND something: admitting those characters
    /// costs printed diagnostics on real, ordinary values. This one defended
    /// nothing — no legitimate variable name carries U+FFFD, because it exists
    /// in a name only when [`lossy_env_pairs`] put it there — so it is the one
    /// residual worth closing, and it is closed in the direction the module
    /// errs everywhere else.
    ///
    /// Asserted with the arm ORDER, because that is the half a re-ordering
    /// would break: the backstop is last, so a mangled-name variable that any
    /// real arm catches still reports that arm's reason.
    #[test]
    fn env_generations_a_name_carrying_the_replacement_character_is_withheld() {
        // The shape the residual was named for: the mangled byte REPLACED the
        // `O`, so no amount of stripping recovers `PASSWORD`.
        assert_eq!(
            "POSTGRES_PASSW\u{FFFD}RD".replace('\u{FFFD}', ""),
            "POSTGRES_PASSWRD",
            "the cheap repair really does not work — the byte replaced a letter"
        );
        assert_eq!(
            classify_env_var("POSTGRES_PASSW\u{FFFD}RD", "hunter2"),
            Some(WithholdReason::NameUnreadable)
        );
        // A mangled name whose value is utterly unremarkable — the arm is about
        // the NAME, so this is withheld too.
        assert_eq!(
            classify_env_var("QONTINUI_WEIRD_\u{FFFD}", "8000"),
            Some(WithholdReason::NameUnreadable)
        );
        // …and the control: the same name without the mangling is printable.
        assert_eq!(classify_env_var("QONTINUI_WEIRD_X", "8000"), None);

        // ORDER. Every other arm answers first, so the backstop never masks a
        // more diagnostic reason.
        assert_eq!(
            classify_env_var("POSTGRES_PASSWORD\u{FFFD}", "hunter2"),
            Some(WithholdReason::Name {
                token: "PASSWORD".to_string()
            }),
            "the NAME arm still answers for a name that merely trails a mangled byte"
        );
        assert_eq!(
            classify_env_var(
                "MYSTERY_\u{FFFD}FIELD",
                "AbCdEf0123456789AbCdEf0123456789Ab"
            ),
            Some(WithholdReason::ValueEntropy),
            "the VALUE arm still answers for a mangled name beside a real token"
        );
        assert_eq!(
            classify_env_var(
                "QONTINUI_\u{FFFD}_DATABASE_URL",
                "postgresql://qontinui@localhost:5432/db"
            ),
            Some(WithholdReason::NameUrlWithUserinfo {
                token: "_URL".to_string()
            }),
            "the JOINT arm still answers for a mangled name ending _URL"
        );

        // The reason renders as a reason, and carries no part of the value.
        let fp = EnvFingerprinter::new();
        let reading = EnvVarReading::classify(&fp, "QONTINUI_WEIRD_\u{FFFD}", "hunter2");
        assert!(reading.value.is_withheld());
        assert_eq!(
            WithholdReason::NameUnreadable.describe(),
            "name carries U+FFFD, so the credential-name test cannot be trusted"
        );
        assert!(!format!("{reading:?}").contains("hunter2"));
    }

    /// The INTERIOR trim is load-bearing too: stripping a boundary U+FFFD costs
    /// three bytes, which can drop a mangled entropy token under the 32-byte
    /// floor. Both trims are offered, so neither arm pays for the other.
    ///
    /// U+FFFD is three bytes, so any token whose TOTAL length is 32–34 bytes
    /// *including* a boundary mangled byte lands under the floor once that byte
    /// is trimmed. The last fixture is exactly that shape: 33 bytes mangled, 30
    /// trimmed. Asserted through the two trims first (naming the mechanism) and
    /// then through `classify_free_text` (naming the consequence), so a failure
    /// says which half broke.
    #[test]
    fn env_generations_free_text_keeps_a_boundary_mangled_entropy_token() {
        let fp = EnvFingerprinter::new();
        // Split so the source carries no contiguous high-entropy literal next to a
        // credential keyword — gitleaks' `generic-api-key` fires on that shape.
        // `concat!` is compile-time, so the value and type are unchanged.
        let token = concat!("AbCdEf0123456789", "AbCdEf0123456789");
        assert_eq!(token.len(), 32, "the fixture must sit ON the entropy floor");
        assert_eq!(
            value_is_credential(token),
            Some(WithholdReason::ValueEntropy),
            "control: the un-mangled token is a credential"
        );

        let mangled = format!("\u{FFFD}{token}");
        // The mechanism: the boundary trim shortens it below the floor, the
        // interior trim does not.
        assert_eq!(trim_token(&mangled).len(), 32);
        assert_eq!(trim_token_interior(&mangled).len(), 35);
        assert_eq!(
            value_is_credential(trim_token(&mangled)),
            Some(WithholdReason::ValueEntropy),
        );

        // The case the interior trim exists for: quoted AND mangled, so the
        // boundary trim strips both the quote and the three bytes.
        let quoted = format!("\"\u{FFFD}{token}\"");
        assert_eq!(
            trim_token(&quoted),
            token,
            "the boundary trim takes the quote AND the mangled byte"
        );
        assert_eq!(
            trim_token_interior(&quoted),
            format!("\u{FFFD}{token}"),
            "the interior trim takes the quote and KEEPS the mangled byte"
        );

        // And a token that is 32 bytes only WITH its mangled byte — the shape
        // that the boundary trim alone would lose.
        let short = "AbCdEf0123456789AbCdEf01234567";
        assert_eq!(short.len(), 30);
        assert_eq!(
            value_is_credential(short),
            None,
            "control: 30 bytes is under the entropy floor"
        );
        let prose = format!("parse failed: invalid type: string \"\u{FFFD}{short}\", expected u16");
        let reading = EnvVarReading::classify_free_text(&fp, "settings_load_error", &prose);
        assert_eq!(
            reading.value,
            EnvValue::Withheld {
                reason: WithholdReason::ValueEntropy,
                fingerprint: fp.fingerprint(&prose),
            },
            "a token that reaches 32 bytes only WITH its mangled byte must still be withheld"
        );
        assert!(!reading.value.detail().contains(short));
    }

    /// **F8 regression.** `elide` neutralises control characters BEFORE the
    /// width computation, so a multi-line value cannot put a literal newline in
    /// a pipe-table cell.
    ///
    /// The second half is the one that matters: a table built from a
    /// multi-line value must still have every row the same shape, because this
    /// renderer's contract is "two machines' reports must differ only where
    /// their configuration differs" — and a row broken across lines misaligns
    /// itself and every column after it.
    #[test]
    fn env_generations_elide_escapes_control_characters_before_measuring() {
        // A SHORT multi-line value — the path that returned the value
        // unchanged, and the one `QONTINUI_RUNNER_CONTEXT` is three characters
        // away from taking.
        assert_eq!(elide("first\nsecond"), "first\\nsecond");
        assert_eq!(elide("a\rb\tc"), "a\\rb\\tc");
        assert_eq!(elide("bell\u{7}"), "bell\\u{0007}");
        // Escaping happens first, so the width is measured on what is PRINTED.
        assert!(!elide("first\nsecond").contains('\n'));

        // And the whole table stays rectangular.
        let fp = EnvFingerprinter::new();
        let section = EnvGenerations {
            generations: vec![EnvGeneration::capture(
                &fp,
                EnvGenerationSpec {
                    id: "G1",
                    name: "runner_process",
                    describes: "d",
                    freshness: "f",
                    is_full_env: true,
                },
                fixed_stamp(),
                [
                    (
                        "QONTINUI_RUNNER_CONTEXT",
                        "runner-context v1\nsecond line here",
                    ),
                    ("QONTINUI_CONFIG_DIR", "C:/cfg"),
                ],
            )],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        };
        let rendered = section.render();
        assert!(
            rendered.contains("runner-context v1\\nsecond line here"),
            "the newline must render as an escape inside the cell:\n{rendered}"
        );
        // The row is ONE line. Before the fix the raw newline split it in two,
        // leaving `second line here` as its own line with no `|` at all.
        assert!(
            !rendered.lines().any(|l| l.trim() == "second line here"),
            "the value broke out of its cell onto its own line:
{rendered}"
        );
        let table_rows: Vec<&str> = rendered
            .lines()
            .filter(|l| l.trim_start().starts_with("QONTINUI_"))
            .collect();
        assert_eq!(
            table_rows.len(),
            2,
            "one row per variable:
{rendered}"
        );
        // The name column is padded to one width, so the separator sits at the
        // same offset on every row — the alignment `pad` would have lost by
        // counting a newline as one column of width.
        let bars: Vec<Option<usize>> = table_rows.iter().map(|r| r.find('|')).collect();
        assert_eq!(
            bars[0], bars[1],
            "the column separator moved between rows — got {bars:?}:
{rendered}"
        );
        assert!(
            bars[0].is_some(),
            "a table row has a separator:
{rendered}"
        );
    }

    /// A withheld reading structurally cannot carry the value: not through the
    /// text renderer, not through `Debug`, not through `serde`. This is the
    /// guarantee the whole module exists for.
    #[test]
    fn env_generations_withheld_reading_carries_no_value_anywhere() {
        let fp = EnvFingerprinter::new();
        let reading = EnvVarReading::classify(&fp, "POSTGRES_PASSWORD", "hunter2");
        assert!(reading.value.is_withheld());
        assert!(!reading.value.cell().contains("hunter2"));
        assert!(!reading.value.detail().contains("hunter2"));
        assert!(!format!("{reading:?}").contains("hunter2"));
        let json = serde_json::to_string(&reading).expect("serializes");
        assert!(!json.contains("hunter2"), "value leaked into JSON: {json}");
        assert!(json.contains("withheld"), "state missing: {json}");
    }

    /// The report renders a PIPE TABLE, and a secret never reaches a cell of it
    /// — because the classifier withheld the value at ingestion, long before
    /// the renderer existed.
    ///
    /// The other half of this argument — that `session/redact.rs`'s `SECRET_RE`
    /// provably does NOT catch a secret in this table shape, so nothing here
    /// may lean on it — is the negative control
    /// `config_report_env_table_defeats_redact_secrets_but_not_the_classifier`
    /// in `config_report_cmd.rs`. It lives bin-side because `session` is a
    /// BIN-only module that this lib crate cannot call.
    #[test]
    fn env_generations_table_render_never_carries_a_credential_value() {
        let secret = "hunter2";
        // A MULTI-HOST connection string, planted under a name NO other arm can
        // rescue: `QONTINUI_CONN_PROBE` matches no `CREDENTIAL_NAME_TOKENS`
        // entry and ends with no `URL_NAME_TOKENS` suffix, so only the
        // value-shape arm — i.e. `url_userinfo`'s host charset — can withhold
        // it. Before the host charset admitted `,` this row rendered the whole
        // connection string, password included, and the counter read one lower.
        let multihost =
            "postgresql://qontinui:hunter2@pg1.internal:5432,pg2.internal:5432/qontinui";
        let fp = EnvFingerprinter::new();
        let gen = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G1",
                name: "runner_process",
                describes: "this process's env",
                freshness: "frozen at start",
                is_full_env: true,
            },
            fixed_stamp(),
            [
                ("QONTINUI_POSTGRES_PASSWORD", secret),
                ("QONTINUI_CONFIG_DIR", "C:/cfg"),
                ("QONTINUI_CONN_PROBE", multihost),
            ],
        );
        let section = EnvGenerations {
            generations: vec![gen],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        }
        .render();

        assert!(
            section.contains("QONTINUI_POSTGRES_PASSWORD | <withheld #"),
            "the table must be pipe-delimited — the shape `SECRET_RE` cannot see:\n{section}"
        );
        assert!(
            !section.contains(secret),
            "the secret reached the rendered table:\n{section}"
        );
        assert!(
            section.contains("<withheld #"),
            "the withheld cell must be visible as a row:\n{section}"
        );
        // The multi-host row must be withheld through the SAME render path, and
        // neither its password nor the connection string itself may appear.
        // The name column is padded to its widest entry, so the row is located
        // by name and asserted on content rather than on one spacing literal.
        let probe_row = section
            .lines()
            .find(|l| l.trim_start().starts_with("QONTINUI_CONN_PROBE"))
            .unwrap_or_else(|| panic!("no row for the multi-host probe:\n{section}"));
        assert!(
            probe_row.contains("| <withheld #"),
            "the multi-host connection string must render as a withheld row: {probe_row:?}"
        );
        assert!(
            !section.contains(multihost),
            "the multi-host connection string reached the rendered table:\n{section}"
        );
        assert!(
            !section.contains("pg2.internal"),
            "no part of the multi-host authority may be rendered:\n{section}"
        );
        assert!(
            section.contains("2 variable readings withheld"),
            "the report must state the withheld count:\n{section}"
        );
    }

    /// Divergence is the diagnostic: which variables differ, and in which
    /// direction. Asserted against LITERAL rendered lines.
    #[test]
    fn env_generations_divergence_names_direction_for_every_delta_kind() {
        let fp = EnvFingerprinter::new();
        let older = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G1",
                name: "runner_process",
                describes: "the runner's own env",
                freshness: "frozen when the runner started",
                is_full_env: true,
            },
            fixed_stamp(),
            [
                ("QONTINUI_FLAG", "off"),
                ("QONTINUI_GONE", "yes"),
                ("QONTINUI_SAME", "same"),
            ],
        );
        let newer = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G3",
                name: "pty_child",
                describes: "what a PTY child gets now",
                freshness: "re-read from the registry at spawn",
                is_full_env: true,
            },
            fixed_stamp(),
            [
                ("QONTINUI_FLAG", "on"),
                ("QONTINUI_NEW", "added"),
                ("QONTINUI_SAME", "same"),
            ],
        );
        let d = diff_generations(&older, &newer, "so the flag needs a runner restart.");
        assert_eq!(d.deltas.len(), 3, "{:?}", d.deltas);

        let text = EnvGenerations {
            generations: vec![older, newer],
            divergences: vec![d],
            launch_drift: None,
            seams: vec![],
        }
        .render();

        assert!(
            text.contains("divergence G1 runner_process → G3 pty_child\n"),
            "{text}"
        );
        assert!(
            text.contains("  ~ QONTINUI_FLAG — G1 runner_process: off | G3 pty_child: on\n"),
            "changed delta missing its direction:\n{text}"
        );
        assert!(
            text.contains(
                "  + QONTINUI_NEW — ABSENT in G1 runner_process, added in G3 pty_child\n"
            ),
            "added delta missing:\n{text}"
        );
        assert!(
            text.contains("  - QONTINUI_GONE — yes in G1 runner_process, ABSENT in G3 pty_child\n"),
            "removed delta missing:\n{text}"
        );
        assert!(
            !text.contains("QONTINUI_SAME —"),
            "an unchanged variable must not appear as a delta:\n{text}"
        );
        assert!(
            text.contains("so the flag needs a runner restart."),
            "{text}"
        );
    }

    /// A credential that CHANGED between generations is reported as changed —
    /// without either value ever having been stored. The per-run keyed
    /// fingerprint is what makes that possible, and it must differ for
    /// different values and match for equal ones.
    #[test]
    fn env_generations_credential_change_is_detectable_without_keeping_the_value() {
        let fp = EnvFingerprinter::new();
        let older = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G1",
                name: "runner_process",
                describes: "d",
                freshness: "f",
                is_full_env: true,
            },
            fixed_stamp(),
            [("QONTINUI_DB_PASSWORD", "old-secret-value")],
        );
        let newer = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G3",
                name: "pty_child",
                describes: "d",
                freshness: "f",
                is_full_env: true,
            },
            fixed_stamp(),
            [("QONTINUI_DB_PASSWORD", "new-secret-value")],
        );
        let d = diff_generations(&older, &newer, "i");
        assert_eq!(d.deltas.len(), 1, "the credential change must be visible");
        assert!(d.deltas[0].involves_withheld());

        let rendered = EnvGenerations::render_divergence(&d);
        assert!(!rendered.contains("old-secret-value"), "{rendered}");
        assert!(!rendered.contains("new-secret-value"), "{rendered}");
        assert!(
            rendered.contains("1 of them credential-classed"),
            "{rendered}"
        );

        // Equal values ⇒ equal fingerprints ⇒ no delta at all.
        let same = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G3",
                name: "pty_child",
                describes: "d",
                freshness: "f",
                is_full_env: true,
            },
            fixed_stamp(),
            [("QONTINUI_DB_PASSWORD", "old-secret-value")],
        );
        assert!(
            diff_generations(&older, &same, "i").deltas.is_empty(),
            "identical credential values must not read as a change"
        );
    }

    /// The launch-snapshot block has THREE arms and each says something
    /// different: not checked, checked-and-clean, checked-and-drifting. An
    /// omitted or blank block would collapse the first two, and they are the
    /// pair a reader is most likely to confuse.
    #[test]
    fn env_generations_launch_drift_renders_all_three_arms() {
        let fp = EnvFingerprinter::new();

        let unavailable = EnvGenerations {
            generations: vec![],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        }
        .render();
        assert!(
            unavailable.contains("launch snapshot vs a re-read now\n  NOT AVAILABLE"),
            "an absent check must still render its block:\n{unavailable}"
        );
        assert!(
            unavailable
                .contains("absence of a check,\n  not a finding that the snapshot is current"),
            "the block must refuse the wrong inference:\n{unavailable}"
        );

        let clean = EnvGenerations {
            generations: vec![],
            divergences: vec![],
            launch_drift: Some(LaunchSnapshotDrift {
                fields_compared: 11,
                differing: vec![],
                captured_at_launch: fixed_stamp(),
            }),
            seams: vec![],
        }
        .render();
        assert!(
            clean.contains("all 11 launch fields still agree with a re-read"),
            "{clean}"
        );

        let drifted = EnvGenerations {
            generations: vec![],
            divergences: vec![],
            launch_drift: Some(LaunchSnapshotDrift {
                fields_compared: 11,
                differing: vec![LaunchFieldDrift {
                    field: "api_url".to_string(),
                    at_launch: EnvVarReading::classify(&fp, "api_url", "http://old").value,
                    now: EnvVarReading::classify(&fp, "api_url", "http://new").value,
                }],
                captured_at_launch: fixed_stamp(),
            }),
            seams: vec![],
        }
        .render();
        assert!(
            drifted.contains("1 of 11 launch fields DIVERGE"),
            "{drifted}"
        );
        assert!(
            drifted.contains("  ~ api_url — at launch: http://old | now: http://new\n"),
            "{drifted}"
        );
    }

    /// The seam section prints what a seam sets and what it clears, and a seam
    /// value goes through the SAME classifier as an environment value.
    #[test]
    fn env_generations_seam_section_classifies_seam_values_too() {
        let fp = EnvFingerprinter::new();
        let text = EnvGenerations {
            generations: vec![],
            divergences: vec![],
            launch_drift: None,
            seams: vec![SeamEnvReport {
                seam: "session::TerminalSession::finalize_child_env".to_string(),
                command_type: "portable_pty::CommandBuilder".to_string(),
                scrub_wrapper: "scrub_credential_env_pty".to_string(),
                sets: vec![
                    EnvVarReading::classify(&fp, "CLAUDE_CONFIG_DIR", "C:/claude/acct-a"),
                    EnvVarReading::classify(&fp, "SOME_TOKEN", "leak-me"),
                ],
                clears: vec!["QONTINUI_OPERATOR2_PASSWORD".to_string()],
            }],
        }
        .render();
        assert!(
            text.contains(
                " 1. session::TerminalSession::finalize_child_env \
                 [portable_pty::CommandBuilder] → scrub_credential_env_pty\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("      sets:   CLAUDE_CONFIG_DIR = C:/claude/acct-a\n"),
            "{text}"
        );
        assert!(!text.contains("leak-me"), "a seam value leaked:\n{text}");
        assert!(
            text.contains("      clears: QONTINUI_OPERATOR2_PASSWORD\n"),
            "{text}"
        );
    }

    /// Same inputs ⇒ same bytes, and the table's column alignment is part of
    /// the contract (two machines' reports must differ only where their
    /// configuration differs).
    #[test]
    fn env_generations_render_is_byte_stable() {
        let fp = EnvFingerprinter::new();
        let g1 = EnvGeneration::capture(
            &fp,
            EnvGenerationSpec {
                id: "G1",
                name: "runner_process",
                describes: "the runner's own env",
                freshness: "frozen at runner start",
                is_full_env: true,
            },
            fixed_stamp(),
            [("QONTINUI_A", "1"), ("QONTINUI_LONGER_NAME", "value")],
        );
        let section = EnvGenerations {
            generations: vec![g1],
            divergences: vec![],
            launch_drift: None,
            seams: vec![],
        };
        let expected = "\n\
env generations — the same variable at three ages
=========================================================
There is no single \"the environment\". Each generation below was frozen at a
different moment, and a value only reaches a Claude tool call through ALL of them.

G1 runner_process — the runner's own env
      captured_at: 2026-08-22T12:34:56.789Z
      freshness:   frozen at runner start
      variables:   2 (0 withheld)

side by side — the configuration surface
  variable             | G1 runner_process
  ---------------------+------------------
  QONTINUI_A           | 1
  QONTINUI_LONGER_NAME | value

launch snapshot vs a re-read now
  NOT AVAILABLE — `RunnerLaunchEnv::read()` has never run in this process, so there is
  no launch generation to compare a re-read against. This is the absence of a check,
  not a finding that the snapshot is current.
---------------------------------------------------------
0 variable readings withheld across this section — their values were dropped at
classification and never reached this renderer.
";
        assert_eq!(section.render(), expected);
        assert_eq!(section.render(), section.render());
    }

    /// A truncated cell says so rather than silently shortening a value.
    #[test]
    fn env_generations_long_values_are_elided_not_silently_cut() {
        let long = "x".repeat(200);
        let out = elide(&long);
        assert_eq!(out.chars().count(), CELL_WIDTH);
        assert!(out.ends_with('…'));
        assert_eq!(elide("short"), "short");
    }
}

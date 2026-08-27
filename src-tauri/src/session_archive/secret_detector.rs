//! Long-lived-credential **detector** for the session archive — deliberately
//! NOT a redactor, and deliberately a different module from
//! [`crate::session::redact`]'s live-path sweep (which is bin-crate side and
//! keeps exactly one caller-visible behaviour for the path it already serves).
//!
//! ## Why a detector and not the shipped redactor
//!
//! Plan `2026-08-26-claude-code-session-repository-in-qontinui-web` §5 measured
//! the shipped `redact.rs` patterns over a 41 MB slice of this corpus (1.2% of
//! it): **207 matches, 118 of them (57%) false positives** — `token: String,`,
//! `token: Option<String>,`, `token: None,`, `token = match`, `TOKEN: ${{`, and
//! already-masked `TOKEN: ***`. In a fleet whose transcripts discuss bearer
//! tokens constantly the bearer sweep was worse still: 26 × "bearer fallback",
//! 5 × "bearer survives", 3 × "bearer verbatim" masked, against **2** genuine
//! 64-hex tokens — the 8-character minimum does not exclude the word
//! "fallback".
//!
//! And it missed by shape: the sample held a live Cognito ID token caught only
//! because it happened to be spelled `token=eyJ…`; there was no JWT, `AKIA`,
//! PEM (multi-line — those regexes are line-oriented) or
//! `postgres://user:pass@host` pattern in it at all.
//!
//! So masking would corrupt the corpus this plan exists to make searchable
//! **and** break byte-verbatim export: a `content_sha256` computed over
//! redacted bytes can never be verified against the original file. The archive
//! is therefore written verbatim and this module records
//! `secret_finding_count` + `secret_finding_kinds` on the head row — **a
//! queryable audit signal, never a visibility gate and never a mask**.
//!
//! ## Why JWTs are NOT a target
//!
//! §5 measured the one live credential in the sample as a **3-hour** Cognito ID
//! token (`iat` 1787724433 → `exp` 1787735233) that had already expired three
//! hours before it was found — and it came from the corpus's *newest* day, so
//! everything older is expired by a wider margin. A JWT in this corpus is
//! self-neutralizing, and a detector that fires on every one of them buries the
//! findings that matter. The same reasoning excludes AWS **STS** keys
//! (`ASIA…`), which are session credentials with the same self-expiring
//! property; only the long-term `AKIA…` prefix is a target.
//!
//! ## What IS a target
//!
//! Shapes with **no expiry of their own**, where the only way the credential
//! stops working is somebody rotating it:
//!
//! | kind | shape |
//! |---|---|
//! | [`KIND_AWS_ACCESS_KEY_ID`] | `AKIA` + 16 uppercase alnum |
//! | [`KIND_GITHUB_TOKEN`] | `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_` + ≥36 alnum |
//! | [`KIND_PRIVATE_KEY_PEM`] | a `-----BEGIN … PRIVATE KEY-----` marker |
//! | [`KIND_CONNECTION_STRING`] | `scheme://user:password@host` |
//! | [`KIND_KEYED_HIGH_ENTROPY`] | a credential-naming key followed by a ≥32-char opaque run |
//! | [`KIND_BARE_HIGH_ENTROPY`] | a ≥32-char mixed-alphabet run with no key at all |
//!
//! The two entropy arms carry the false-positive lessons above in their own
//! construction rather than in a comment:
//!
//! - The **keyed** arm requires ≥32 characters of *pure* base64/hex after the
//!   key. That is what excludes `bearer fallback`, `token: String,` and
//!   `TOKEN: ${{` — none of them is followed by 32 opaque characters — while
//!   still catching the two genuine `Bearer <64-hex>` values §5 found.
//! - The **bare** arm carries three more rules, one per false-positive class
//!   this corpus actually contains:
//!   - a *mixed* alphabet (lower AND upper AND digit), which excludes the
//!     single largest shape here — lowercase-hex git object ids and sha256
//!     digests, present by the thousand and not secrets;
//!   - a [`VOWEL_RATIO_CEILING`], which excludes long CamelCase identifiers.
//!     Entropy cannot: measured over this corpus's own vocabulary a camel-case
//!     identifier scores 4.33 bits/char against a real AWS secret key's 4.71;
//!   - a [`MAX_OPAQUE_RUN`] bound, because a real credential is not ten
//!     kilobytes long and an embedded base64 image would otherwise fire on
//!     every screenshot.
//!
//!   The bare arm's alphabet also excludes `-` and `_`, which is what keeps a
//!   `…/agent-worktrees/<uuid>/src-tauri/src/…` path from reading as a
//!   60-character opaque run.
//!
//! ## Cost
//!
//! §5 measured this class of scan at **125 MB/s single-threaded** — about 28
//! seconds for the whole 3.5 GB corpus, less than the gzip pass beside it.
//! Compute was never the constraint. The implementation keeps it that way by
//! running a single [`regex::RegexSet`] pass first and only counting with the
//! individual patterns for the kinds that actually matched, so the ~99% of
//! transcripts with no finding at all pay exactly one DFA sweep.

use std::sync::OnceLock;

use regex::{Regex, RegexSet};

/// Long-term AWS access key id (`AKIA…`). STS/session keys (`ASIA…`) are
/// deliberately excluded — see the module doc.
pub const KIND_AWS_ACCESS_KEY_ID: &str = "aws_access_key_id";
/// GitHub personal-access / OAuth / app token (`ghp_`, `gho_`, `ghu_`, `ghs_`,
/// `ghr_`). Long-lived by construction and unambiguous by shape.
pub const KIND_GITHUB_TOKEN: &str = "github_token";
/// A PEM private-key block marker.
pub const KIND_PRIVATE_KEY_PEM: &str = "private_key_pem";
/// A URI carrying inline credentials (`postgres://user:pass@host`).
pub const KIND_CONNECTION_STRING: &str = "connection_string_credentials";
/// A credential-naming key followed by a long opaque run.
pub const KIND_KEYED_HIGH_ENTROPY: &str = "keyed_high_entropy_secret";
/// A long mixed-alphabet opaque run with no naming key.
pub const KIND_BARE_HIGH_ENTROPY: &str = "bare_high_entropy_secret";

/// Every kind this detector can emit, in a stable order. The order is the one
/// [`scan`] reports kinds in, so a `secret_finding_kinds` array is comparable
/// across runs without sorting it at the call site.
pub const DETECTOR_KINDS: [&str; 6] = [
    KIND_AWS_ACCESS_KEY_ID,
    KIND_GITHUB_TOKEN,
    KIND_PRIVATE_KEY_PEM,
    KIND_CONNECTION_STRING,
    KIND_KEYED_HIGH_ENTROPY,
    KIND_BARE_HIGH_ENTROPY,
];

/// Upper bound on an opaque run the entropy arms will consider a credential.
///
/// A GitHub fine-grained PAT is ~93 characters and an AWS secret access key is
/// 40; nothing anybody rotates is longer than this. The bound is what keeps an
/// embedded base64 image — which is one enormous run — from being reported as
/// a secret in every transcript that contains a screenshot.
const MAX_OPAQUE_RUN: usize = 256;

/// The pattern for each [`DETECTOR_KINDS`] entry, same index.
///
/// `(?s)` is deliberately absent and `(?m)` deliberately unnecessary: every
/// pattern is anchored on its own literal shape rather than on line position,
/// so a PEM marker is found whether the transcript stored the block as real
/// newlines or as the `\n` escapes a JSONL string uses.
fn patterns() -> &'static [&'static str; 6] {
    &[
        // AWS long-term access key id. `AKIA` + exactly 16 uppercase
        // alphanumerics, word-bounded so it is not a substring of a longer
        // identifier.
        r"\bAKIA[0-9A-Z]{16}\b",
        // GitHub token prefixes. The `_` separator plus a ≥36-character body is
        // unique enough that this arm has no plausible false positive.
        r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b",
        // PEM private-key marker. Covers `PRIVATE KEY`, `RSA PRIVATE KEY`,
        // `EC PRIVATE KEY`, `OPENSSH PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`.
        r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----",
        // `scheme://user:password@host`. The user and password parts exclude
        // `:` `/` `@` and whitespace, so this cannot span a URL boundary. The
        // placeholder filter runs afterwards in [`is_placeholder_connection`].
        r"\b[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+:[^\s:/@]+@[^\s/@]+",
        // A credential-naming key, a separator, then ≥32 characters of pure
        // base64/hex. The 32-character opaque-run requirement is the whole
        // point: it is what `bearer fallback` and `token: String,` fail.
        r"(?i)\b(?:bearer|authorization|secret|token|api[-_ ]?key|access[-_ ]?key|client[-_ ]?secret|password|passwd|credential)\b[\s\x22':=]{1,6}([A-Za-z0-9+/_\-]{32,256}={0,2})",
        // A bare opaque run with no key.
        //
        // The alphabet is standard base64 + hex and deliberately EXCLUDES `-`
        // and `_`. That is not a stylistic choice: `-`, `_` and `.` are what
        // break a filesystem path into short segments, and without the
        // exclusion every
        // `agent-worktrees/01a03e5f-.../src-tauri/src/session_archive` in these
        // transcripts is a 60-character mixed-alphabet run. base64url tokens
        // are the cost — they are caught by the KEYED arm when they are
        // labelled, and an unlabelled base64url run is genuinely
        // indistinguishable from a path fragment, which is exactly the
        // false-positive class §5 rejected the redactor over.
        //
        // The upper bound is unbounded here on purpose: `{32,}` is greedy, so
        // the match spans the WHOLE maximal run and [`MAX_OPAQUE_RUN`] can
        // reject it. A `{32,256}` bound would instead match the first 256
        // characters of a 900-character image blob and sail through the
        // length check.
        //
        // Mixed alphabet and entropy are enforced in [`is_mixed_alphabet`] /
        // [`shannon_bits_per_char`] rather than in the regex, because a
        // lookaround-free engine cannot express "contains all three classes"
        // without an unreadable alternation.
        r"[A-Za-z0-9+/]{32,}={0,2}",
    ]
}

/// The compiled prefilter — one DFA over all six patterns.
fn pattern_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        // Every pattern here is a compile-time literal that this module's own
        // tests exercise, so a failure is a build-time bug in THIS file rather
        // than anything a transcript can cause. Panicking with the offending
        // pattern named is the honest response; degrading to "no findings"
        // would report an unscanned corpus as a clean one.
        RegexSet::new(patterns()).expect("session_archive detector patterns must compile")
    })
}

/// The individual patterns, compiled once, same index as [`DETECTOR_KINDS`].
fn compiled() -> &'static [Regex; 6] {
    static ALL: OnceLock<[Regex; 6]> = OnceLock::new();
    ALL.get_or_init(|| {
        let p = patterns();
        std::array::from_fn(|i| {
            Regex::new(p[i]).expect("session_archive detector pattern must compile")
        })
    })
}

/// What the detector found in one transcript.
///
/// `kinds` is `Some(vec![])` when the detector ran and found nothing, and the
/// caller must send it that way: the web schema distinguishes an empty array
/// ("scanned, clean") from `NULL` ("never scanned"), and collapsing the two
/// would make an unscanned row look audited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretFindings {
    /// Total matches across every kind.
    pub count: usize,
    /// The distinct kinds that matched, in [`DETECTOR_KINDS`] order.
    pub kinds: Vec<String>,
}

/// True when a `scheme://user:pass@host` match is documentation rather than a
/// credential.
///
/// The corpus is a fleet of engineering transcripts, so `postgres://user:pass@`
/// and `redis://user:${PASSWORD}@` appear in prose constantly. Reporting those
/// would reproduce the exact false-positive rate §5 rejected the redactor for.
fn is_placeholder_connection(m: &str) -> bool {
    // Isolate the password: between the first `:` after `://` and the `@`.
    let Some(after_scheme) = m.split_once("://").map(|(_, rest)| rest) else {
        return false;
    };
    let Some((userinfo, _host)) = after_scheme.split_once('@') else {
        return false;
    };
    let Some((_user, password)) = userinfo.split_once(':') else {
        return false;
    };
    let lower = password.to_ascii_lowercase();
    // An interpolation (`${PASSWORD}`, `{{secret}}`, `<password>`) is by
    // definition not the secret, and a masked value never was one.
    if password.contains('$')
        || password.contains('{')
        || password.contains('<')
        || password.chars().all(|c| c == '*')
    {
        return true;
    }
    matches!(
        lower.as_str(),
        "pass"
            | "password"
            | "passwd"
            | "secret"
            | "changeme"
            | "hunter2"
            | "xxx"
            | "xxxx"
            | "yourpassword"
            | "your_password"
            | "mypassword"
            | "example"
            | "redacted"
    )
}

/// True when a run uses lowercase, uppercase AND digits.
///
/// This is the single rule that keeps the bare arm usable in THIS corpus: git
/// object ids, sha256 digests and dashless UUIDs are all lowercase hex, so they
/// never satisfy it, while a real opaque credential (base64 of random bytes, an
/// AWS secret access key, a hex string that happens to be mixed-case) does.
fn is_mixed_alphabet(run: &str) -> bool {
    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    for c in run.chars() {
        if c.is_ascii_lowercase() {
            lower = true;
        } else if c.is_ascii_uppercase() {
            upper = true;
        } else if c.is_ascii_digit() {
            digit = true;
        }
    }
    lower && upper && digit
}

/// Shannon entropy in bits per character.
///
/// Applied to the bare arm only. A mixed-alphabet run can still be structured
/// text (`SomeVeryLongCamelCaseIdentifier7`), and structured text sits well
/// below the ~4.5–5.5 bits/char a base64-encoded random key reaches.
fn shannon_bits_per_char(run: &str) -> f64 {
    let bytes = run.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    let mut h = 0.0;
    for &c in counts.iter() {
        if c == 0 {
            continue;
        }
        let p = c as f64 / len;
        h -= p * p.log2();
    }
    h
}

/// Entropy floor for the **bare** arm, in bits per character.
///
/// Kills the degenerate shapes — a repeated `AaAaAa…`, a run of two alternating
/// characters — that satisfy the mixed-alphabet rule without carrying any
/// information. It does NOT separate a key from a long identifier: measured
/// over this corpus's own vocabulary, `SomeVeryLongCamelCaseIdentifierName7`
/// scores 4.33 and a real 40-character AWS secret key scores 4.71, which is far
/// too close to draw a line through. [`VOWEL_RATIO_CEILING`] is what does that.
const BARE_ENTROPY_FLOOR: f64 = 4.2;

/// Vowel-density ceiling for the **bare** arm.
///
/// The one measurement that cleanly separates a random key from a CamelCase
/// English identifier, which entropy and case-transition density both fail to.
/// Sampled 2026-08-26 over both classes:
///
/// | run | vowel ratio |
/// |---|---|
/// | `WeShouldProbablyRefactorTheWholeThingLater2026` | 0.304 |
/// | `ThisIsAVeryLongDescriptiveFunctionNameForTesting1` | 0.347 |
/// | `SomeVeryLongCamelCaseIdentifierName7ThatKeepsGoing` | 0.400 |
/// | AWS secret access key (random 40) | 0.125 |
/// | base64 of random bytes (44) | 0.114 |
/// | GitHub PAT body | 0.143 |
///
/// English is roughly 38% vowels by construction; base64 over a 64-symbol
/// alphabet is about 10/64. 0.28 sits in the empty band between the two with
/// margin on both sides.
///
/// It is not a proof — an identifier can be vowel-poor and a key vowel-rich —
/// but the arm this guards produces an AUDIT SIGNAL, not a mask and not a
/// visibility gate, so a miss costs an unflagged row rather than a corrupted
/// one. The failure this whole module exists to avoid is the OPPOSITE: §5
/// measured the shipped redactor at 57% false positives, and a findings column
/// that fires on every long identifier in an engineering corpus is exactly as
/// unreadable.
const VOWEL_RATIO_CEILING: f64 = 0.28;

/// Fraction of a run that is an ASCII vowel.
fn vowel_ratio(run: &str) -> f64 {
    if run.is_empty() {
        return 0.0;
    }
    let vowels = run
        .chars()
        .filter(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'))
        .count();
    vowels as f64 / run.chars().count() as f64
}

/// Scan one transcript's bytes for long-lived-credential shapes.
///
/// Invalid UTF-8 is scanned as lossy UTF-8 rather than skipped: a transcript
/// that lost bytes in transit is exactly the one you least want to leave
/// unaudited, and the detector's output is an audit signal, not a decision
/// about the archive body (which is always stored verbatim, unmodified, from
/// the ORIGINAL bytes).
pub fn scan_bytes(raw: &[u8]) -> SecretFindings {
    scan(&String::from_utf8_lossy(raw))
}

/// [`scan_bytes`] over text that is already known-good UTF-8.
pub fn scan(text: &str) -> SecretFindings {
    let set = pattern_set();
    let hits = set.matches(text);
    if !hits.matched_any() {
        return SecretFindings {
            count: 0,
            kinds: Vec::new(),
        };
    }

    let regexes = compiled();
    let mut count = 0usize;
    let mut kinds: Vec<String> = Vec::new();
    for (idx, kind) in DETECTOR_KINDS.iter().enumerate() {
        if !hits.matched(idx) {
            continue;
        }
        let matched = match idx {
            // Connection strings: drop the documentation forms.
            3 => regexes[idx]
                .find_iter(text)
                .filter(|m| !is_placeholder_connection(m.as_str()))
                .count(),
            // Keyed entropy: the capture group is the opaque run; the regex
            // already enforced its length and alphabet.
            4 => regexes[idx].captures_iter(text).count(),
            // Bare entropy: the regex found a long run; the alphabet and
            // entropy rules decide whether it is a credential shape.
            5 => regexes[idx]
                .find_iter(text)
                .filter(|m| {
                    let run = m.as_str();
                    run.len() <= MAX_OPAQUE_RUN
                        // A path fragment that survived the alphabet rule is
                        // still a path if it is mostly separators. Two slashes
                        // is more than any real key carries and far fewer than
                        // any directory tree.
                        && run.matches('/').count() <= 2
                        && is_mixed_alphabet(run)
                        && shannon_bits_per_char(run) >= BARE_ENTROPY_FLOOR
                        // The rule that separates a key from a CamelCase
                        // English identifier — see the constant's own doc for
                        // the measurement.
                        && vowel_ratio(run) < VOWEL_RATIO_CEILING
                })
                .count(),
            _ => regexes[idx].find_iter(text).count(),
        };
        if matched > 0 {
            count += matched;
            kinds.push((*kind).to_string());
        }
    }

    SecretFindings { count, kinds }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_transcript_scans_to_zero_findings_and_an_empty_kind_list() {
        let text =
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"run the tests\"}}";
        let f = scan(text);
        assert_eq!(f.count, 0);
        assert!(f.kinds.is_empty());
    }

    #[test]
    fn the_57_percent_false_positive_class_from_the_plan_does_not_fire() {
        // Every one of these was measured as a `redact.rs` false positive over
        // a 41 MB slice of this corpus (plan §5). The detector exists because
        // masking them corrupts the archive; firing on them would reproduce
        // the noise in a different column.
        let text = "\
            token: String,\n\
            token: Option<String>,\n\
            token: None,\n\
            token = match resolve() { .. }\n\
            TOKEN: ${{ secrets.GITHUB_TOKEN }}\n\
            TOKEN: ***\n\
            the bearer fallback survives a restart\n\
            bearer verbatim, bearer survives\n";
        let f = scan(text);
        assert_eq!(f.count, 0, "false-positive class fired: {:?}", f.kinds);
    }

    #[test]
    fn git_object_ids_and_digests_are_not_secrets() {
        // The dominant long-hex shape in this corpus. A 40-char git sha, a
        // 64-char sha256 and a dashless UUID are all lowercase hex, which is
        // exactly what the mixed-alphabet rule excludes.
        let text = "\
            21c60877f3ac4295f39f30b9b441421cab352ab0\n\
            e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
            730de49076324884a42b0cb9aedd6791\n";
        let f = scan(text);
        assert_eq!(f.count, 0, "git-shaped hex fired: {:?}", f.kinds);
    }

    #[test]
    fn jwts_are_deliberately_not_detected() {
        // §5's measurement: the sample's only live credential was a 3-hour
        // Cognito token already expired when it was found, from the corpus's
        // newest day. Self-expiring shapes are not this detector's business.
        let jwt = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImFiYyJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwiZXhwIjoxNzg3NzM1MjMzfQ.c2lnbmF0dXJl";
        let f = scan(jwt);
        assert!(
            !f.kinds.iter().any(|k| k.contains("jwt")),
            "no kind should name a JWT"
        );
    }

    #[test]
    fn aws_sts_session_keys_are_excluded_and_long_term_keys_are_not() {
        assert_eq!(scan("ASIAIOSFODNN7EXAMPLE").count, 0);
        let f = scan("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(f.kinds, vec![KIND_AWS_ACCESS_KEY_ID.to_string()]);
        assert_eq!(f.count, 1);
    }

    #[test]
    fn a_github_token_is_detected() {
        let f = scan("gh auth: ghp_1234567890abcdefABCDEF1234567890abcdef");
        assert!(f.kinds.iter().any(|k| k == KIND_GITHUB_TOKEN));
    }

    #[test]
    fn a_pem_marker_is_detected_through_a_jsonl_escaped_newline() {
        // A PEM block inside a JSONL string has literal `\n` escapes rather
        // than real newlines — the shape a line-oriented regex misses, which
        // §5 named as one of the shipped redactor's misses-by-shape.
        let text = r#"{"text":"-----BEGIN RSA PRIVATE KEY-----\nMIIEow...\n-----END RSA PRIVATE KEY-----"}"#;
        let f = scan(text);
        assert!(f.kinds.iter().any(|k| k == KIND_PRIVATE_KEY_PEM));
    }

    #[test]
    fn a_connection_string_with_a_real_password_fires_and_a_placeholder_does_not() {
        let real = scan("postgres://qontinui:S3cr3tRotateMe@db.internal:5432/coord");
        assert!(real.kinds.iter().any(|k| k == KIND_CONNECTION_STRING));

        for doc in [
            "postgres://user:pass@localhost:5432/db",
            "postgres://user:password@host/db",
            "redis://default:${REDIS_PASSWORD}@cache:6379",
            "mysql://root:***@127.0.0.1/app",
            "mongodb://admin:<password>@mongo:27017",
        ] {
            let f = scan(doc);
            assert!(
                !f.kinds.iter().any(|k| k == KIND_CONNECTION_STRING),
                "documentation form fired: {doc}"
            );
        }
    }

    #[test]
    fn the_two_genuine_bearer_tokens_the_plan_found_are_detected() {
        // §5's residual: two `Bearer <64-hex>` values of unknown lifetime.
        // 64 lowercase hex characters do NOT satisfy the bare arm's mixed
        // alphabet, so the KEYED arm is what has to catch them — which is
        // exactly why the keyed arm exists alongside the bare one.
        let text = "Authorization: Bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let f = scan(text);
        assert!(
            f.kinds.iter().any(|k| k == KIND_KEYED_HIGH_ENTROPY),
            "keyed arm missed a 64-hex bearer: {:?}",
            f.kinds
        );
    }

    #[test]
    fn a_bare_mixed_alphabet_key_is_detected() {
        // An AWS secret access key shape: 40 chars, mixed alphabet, no naming
        // key anywhere near it.
        let f = scan("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        assert!(
            f.kinds.iter().any(|k| k == KIND_BARE_HIGH_ENTROPY),
            "bare arm missed a mixed-alphabet key: {:?}",
            f.kinds
        );
    }

    #[test]
    fn an_embedded_base64_image_blob_does_not_fire_the_bare_arm() {
        // A screenshot pasted into a transcript is one enormous run. The
        // MAX_OPAQUE_RUN bound is what stops every such blob being reported.
        let blob = "iVBORw0KGgoAAAANSUhEUgAA".repeat(40);
        let f = scan(&blob);
        assert!(
            !f.kinds.iter().any(|k| k == KIND_BARE_HIGH_ENTROPY),
            "an image blob fired the bare arm"
        );
    }

    #[test]
    fn ordinary_camel_case_identifiers_do_not_fire_the_bare_arm() {
        // These all clear the entropy floor — entropy does not separate
        // English from base64 (4.33 vs 4.71 measured). The vowel-density rule
        // is what rejects them, and an engineering corpus is full of them.
        for identifier in [
            "SomeVeryLongCamelCaseIdentifierName7ThatKeepsGoing",
            "ThisIsAVeryLongDescriptiveFunctionNameForTesting1",
            "WeShouldProbablyRefactorTheWholeThingLater2026",
            "ManifestMatchesRouteCallsDriftGuardForTheUiBridge",
        ] {
            let f = scan(identifier);
            assert!(
                !f.kinds.iter().any(|k| k == KIND_BARE_HIGH_ENTROPY),
                "structured text fired the bare arm: {identifier}"
            );
        }
    }

    #[test]
    fn a_random_key_shaped_run_still_fires_after_the_prose_rule() {
        // The other side of VOWEL_RATIO_CEILING: rejecting prose must not cost
        // the shapes the arm exists for.
        for key in [
            "7pQ2mVxL9zRt4WnB8cYd3FgH6jSk1NaU5eOiPQwZ",
            "K7xQ2mVpL9zRt4WnB8cYd3FgH6jSk1NaU5eOiPQwZ0Xy",
            "dGhpcyBpcyBhIHRlc3Qgc3RyaW5nIGZvciBlbnRyb3B5MTIz",
        ] {
            let f = scan(key);
            assert!(
                f.kinds.iter().any(|k| k == KIND_BARE_HIGH_ENTROPY),
                "the bare arm missed a key-shaped run: {key}"
            );
        }
    }

    #[test]
    fn worktree_paths_do_not_fire_the_bare_arm() {
        // The single most common long token-shaped string in THIS corpus.
        let text = "D:/qontinui-root/agent-worktrees/01a03e5f-e7d2-70d1-b4f8-0fa5efeeb1db/\
                    qontinui-runner/src-tauri/src/session_archive/secret_detector.rs";
        let f = scan(text);
        assert!(
            !f.kinds.iter().any(|k| k == KIND_BARE_HIGH_ENTROPY),
            "a worktree path fired the bare arm: {:?}",
            f.kinds
        );
    }

    #[test]
    fn kinds_are_reported_in_the_declared_order() {
        let text = "AKIAIOSFODNN7EXAMPLE and -----BEGIN PRIVATE KEY-----";
        let f = scan(text);
        assert_eq!(
            f.kinds,
            vec![
                KIND_AWS_ACCESS_KEY_ID.to_string(),
                KIND_PRIVATE_KEY_PEM.to_string()
            ]
        );
    }

    #[test]
    fn invalid_utf8_is_scanned_lossily_rather_than_skipped() {
        let mut raw = b"AKIAIOSFODNN7EXAMPLE ".to_vec();
        raw.push(0xff); // a lone continuation byte — not valid UTF-8
        let f = scan_bytes(&raw);
        assert_eq!(f.count, 1);
    }
}

//! Regional credential probes from coord's probe catalogue — the `cred` family's
//! `cred:aws-ssm:<region>` / `cred:aws-ecs:<region>` tokens (plan
//! `2026-10-09-coord-knows-which-box-holds-which-credential`, Phase 3, D2/D3).
//!
//! The tenant's specs are served by `GET /coord/devices/credential-probes`. This
//! module fetches them, decides which ones may run, builds the argv and runs it
//! through the family's own probe machinery ([`super::cred_probe_command`],
//! [`super::command_exit_within`], [`super::classify_aws_sts`]). The family's
//! tri-state fold lives with the rest of it, in
//! [`super::CredFamilyState::apply_regional`].
//!
//! # Coord's data is untrusted input
//!
//! A spec becomes a child process's argv, so nothing in one is believed:
//!
//! * The kind must be one this build knows. A kind it does not know is logged by
//!   name and skipped, since a newer coord may add kinds.
//! * The token must be exactly `cred:aws-ssm:<region>` / `cred:aws-ecs:<region>`
//!   for that spec's own kind and region. The token this device then advertises
//!   is rebuilt from the validated kind and region ([`RegionalProbe::token`]), so
//!   no byte of a spec's token field reaches the heartbeat.
//! * Region and canary are re-checked here before any argv is built, whatever
//!   coord's write route and the table's CHECKs did. Two of the refusals go past
//!   the table's canary CHECKs. A leading `-` is refused because aws's parser would
//!   read such a value as a FLAG rather than as `--name`'s argument. A 12-digit
//!   run, dashes ignored, is refused because it is an AWS account id, which D1
//!   keeps out of coord's data. The charset also excludes `:`, which keeps out ARNs
//!   and aws's `file://` / `http://` parameter-value indirection, and it leaves
//!   nothing a `.cmd` shim's `cmd.exe` would re-parse on Windows.
//! * Arguments are discrete argv entries; no shell is involved. No code path can
//!   spell the SSM decrypting flag: [`RegionalProbe::args`] is the only argv
//!   builder, and it interpolates only the two validated values.
//! * At most [`CRED_PROBE_SPEC_RUN_CAP`] specs run per cycle, chosen by token so
//!   the choice does not flap with the order coord lists them in.
//!
//! # A fetch that does not answer is not an empty catalogue
//!
//! Only a 2xx carrying a `specs` ARRAY is an answer. A 404 (the route is not
//! deployed yet), a network error or a malformed body is "no regional answer this
//! cycle" ([`RegionalCycle::NoAnswer`]): tokens already earned follow D2's Unknown
//! rule, and `cred:gh` / `cred:aws-sts` / `probes:cred` are never touched. A spec
//! missing from an ANSWERED catalogue was removed, and its token retracts at once.
//!
//! An answered catalogue holding a row this build cannot READ says less than that.
//! `{"specs":[null]}`, or rows a newer coord renamed or nested, pass the body check
//! but name nothing, so their silence about a token is not a removal. While any row
//! is unreadable, a token no row names folds as Unknown instead. A row that reads
//! and then fails a check still retracts the token it names at once.
//!
//! A row of a kind this build does not know is still LISTED: it runs nothing, and
//! the token it names folds as Unknown, since this build could not measure it. So a
//! newer coord renaming a kind costs an old build the token one TTL later, not at
//! once.
//!
//! # Known limit: one catalogue per device
//!
//! A device bound to several tenants reads only its DEFAULT binding's catalogue,
//! because the route takes the tenant from the bearer and this uses the heartbeat's.
//! Regional tokens the other tenants' specs would earn are not probed here. Reading
//! per binding is separate work, shaped like plan
//! `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`,
//! and is not part of this plan.
//!
//! # What may be logged
//!
//! Token and state, as for the rest of the family. A refused or skipped spec is
//! logged by its kind and a fixed reason, and its token and kind only after each
//! passes [`loggable_token`] / [`loggable_kind`], since both are coord's bytes. A
//! refused canary is logged by kind and reason alone. Region and canary values and
//! transport or parser error text are never logged.

use std::io::Read;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tracing::{debug, info, warn};

use super::{
    aws_has_config, classify_aws_sts, command_exit_within, cred_probe_command,
    log_cred_binary_not_on_path, CredProbeEnv, CredState, ProbeExit, CRED_PROBE_BUDGET,
};

/// The most specs one cycle runs. Each costs up to [`CRED_PROBE_BUDGET`] on the
/// refresh thread, so this also bounds the cycle: 16 × 10 s stays inside the
/// family's 300 s TTL. Coord's write route is planned to cap the catalogue too;
/// this cap holds regardless, as the runner's own bound on data it does not trust.
pub(super) const CRED_PROBE_SPEC_RUN_CAP: usize = 16;

/// Coord's device-JWT catalogue route (own tenant, read from the bearer).
const PROBE_SPECS_PATH: &str = "/coord/devices/credential-probes";

/// Upper bound on the catalogue response. Sixteen rows fit well inside it; a body
/// past it is not a catalogue this build will parse.
const PROBE_SPECS_BODY_CAP: u64 = 256 * 1024;

/// How many refused or skipped specs one cycle logs, so a pathological catalogue
/// cannot flood the log every TTL. The remainder is logged as a count.
const SPEC_REFUSAL_LOG_CAP: usize = 8;

const SSM_GET_PARAMETER_KIND: &str = "aws_ssm_get_parameter";
const ECS_LIST_CLUSTERS_KIND: &str = "aws_ecs_list_clusters";
const SSM_TOKEN_PREFIX: &str = "cred:aws-ssm:";
const ECS_TOKEN_PREFIX: &str = "cred:aws-ecs:";

/// The longest region accepted. No real one comes close.
const REGION_MAX_LEN: usize = 32;

/// The longest canary accepted — the bound on coord's table CHECK.
const CANARY_MAX_LEN: usize = 2048;

/// The longest kind name repeated in a log line. Long enough for any `aws_*` call
/// name, short enough that a long opaque value is not echoed.
const KIND_LOG_MAX_LEN: usize = 40;

/// The length of an AWS account id. A digit run this long is treated as one.
const ACCOUNT_ID_DIGITS: usize = 12;

/// D3's secret-shaped name list, matched case-insensitively. A canary is a
/// non-secret `String` parameter; a name like these would push the probe toward
/// reading a `SecureString`.
const SECRET_SHAPED_NAMES: [&str; 5] = ["password", "secret", "token", "key", "credential"];

/// One catalogue row on the wire. Every field but the kind is optional because a
/// newer coord's kind may not carry them; [`validate_spec`] decides what each
/// known kind requires. Deliberately no `Debug`: nothing should render a row.
#[derive(Deserialize)]
struct WireSpec {
    token: Option<String>,
    probe_kind: String,
    region: Option<String>,
    canary_parameter: Option<String>,
}

/// A spec that passed every check, in the only shape argv is built from.
///
/// Deliberately no `Debug`: the canary is not a secret, but "token and state only"
/// is easier to keep when the value cannot be formatted by accident.
pub(super) enum RegionalProbe {
    /// `aws ssm get-parameter` of a tenant's non-secret canary (D2).
    SsmGetParameter { region: String, canary: String },
    /// `aws ecs list-clusters` — read-only and value-free (D2).
    EcsListClusters { region: String },
}

impl RegionalProbe {
    /// The token this probe earns, rebuilt from the validated kind and region.
    pub(super) fn token(&self) -> String {
        match self {
            RegionalProbe::SsmGetParameter { region, .. } => format!("{SSM_TOKEN_PREFIX}{region}"),
            RegionalProbe::EcsListClusters { region } => format!("{ECS_TOKEN_PREFIX}{region}"),
        }
    }

    /// The one argv builder for a regional probe. Each value is a discrete entry
    /// and goes through no shell. The output goes to the null device unread
    /// ([`cred_probe_command`]), so `--query` and `--output` only shape what aws
    /// discards.
    pub(super) fn args(&self) -> Vec<String> {
        match self {
            RegionalProbe::SsmGetParameter { region, canary } => vec![
                "ssm".into(),
                "get-parameter".into(),
                "--region".into(),
                region.clone(),
                "--name".into(),
                canary.clone(),
                "--query".into(),
                "Parameter.Name".into(),
                "--output".into(),
                "text".into(),
            ],
            RegionalProbe::EcsListClusters { region } => vec![
                "ecs".into(),
                "list-clusters".into(),
                "--region".into(),
                region.clone(),
                "--max-items".into(),
                "1".into(),
            ],
        }
    }
}

/// Why a catalogue row will not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecRefusal {
    /// A kind this build does not know: expected from a newer coord. The row is
    /// still listed, so the token it names is unmeasured, not removed.
    UnknownKind,
    /// A row this build cannot read: not the expected object, or a known kind
    /// missing a field it requires, which is what a renamed field looks like. It
    /// says nothing about which tokens exist, so it cannot justify a retraction.
    Unreadable(&'static str),
    /// A known kind whose row fails a check: a coord bug or a forged row.
    Refused(&'static str),
    /// A served canary that fails its shape check. Logged by kind alone.
    CanaryShape,
}

/// `^[a-z]{2}(-[a-z]+)+-[0-9]{1,2}$`, the table CHECK's region shape, length
/// bounded. The two-letter lead means a value can never read as a flag.
fn region_shape_ok(region: &str) -> bool {
    if region.len() > REGION_MAX_LEN {
        return false;
    }
    fn lowercase(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase())
    }
    let segments: Vec<&str> = region.split('-').collect();
    let [first, middle @ .., last] = segments.as_slice() else {
        return false;
    };
    first.len() == 2
        && lowercase(first)
        && !middle.is_empty()
        && middle.iter().all(|s| lowercase(s))
        && (1..=2).contains(&last.len())
        && last.bytes().all(|b| b.is_ascii_digit())
}

/// True when `s` holds a run of [`ACCOUNT_ID_DIGITS`] digits, dashes ignored, the
/// same reading as coord's `replace(token, '-', '') !~ '[0-9]{12}'`.
fn has_account_id_run(s: &str) -> bool {
    let mut run = 0usize;
    for b in s.bytes().filter(|b| *b != b'-') {
        run = if b.is_ascii_digit() { run + 1 } else { 0 };
        if run >= ACCOUNT_ID_DIGITS {
            return true;
        }
    }
    false
}

/// The table CHECK's canary shape (`^[A-Za-z0-9_./-]+$`, at most
/// [`CANARY_MAX_LEN`], not secret-shaped) plus this runner's own refusal of a
/// leading `-` and of an account-id run. See the module docs for why.
fn canary_shape_ok(canary: &str) -> bool {
    if canary.is_empty()
        || canary.len() > CANARY_MAX_LEN
        || canary.starts_with('-')
        || has_account_id_run(canary)
    {
        return false;
    }
    if !canary
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'))
    {
        return false;
    }
    let lowered = canary.to_ascii_lowercase();
    !SECRET_SHAPED_NAMES.iter().any(|n| lowered.contains(n))
}

/// The row's region, once it has the region shape and the row's token is exactly
/// `<prefix><region>`. Both fields are checked for PRESENCE before either is
/// checked for shape: a row missing a required field is unreadable, which holds
/// the tokens no row names, while a refusal retracts them, so a bad region must
/// not hide a missing token.
fn checked_region(wire: &WireSpec, prefix: &str) -> Result<String, SpecRefusal> {
    let token = wire
        .token
        .as_deref()
        .ok_or(SpecRefusal::Unreadable("token_missing"))?;
    let region = wire
        .region
        .as_deref()
        .ok_or(SpecRefusal::Unreadable("region_missing"))?;
    if !region_shape_ok(region) {
        return Err(SpecRefusal::Refused("region_shape"));
    }
    if token.strip_prefix(prefix) != Some(region) {
        return Err(SpecRefusal::Refused("token_mismatch"));
    }
    Ok(region.to_string())
}

/// Decide whether one catalogue row may run. The `match` is the closed list of
/// kinds this build knows.
fn validate_spec(wire: &WireSpec) -> Result<RegionalProbe, SpecRefusal> {
    match wire.probe_kind.as_str() {
        SSM_GET_PARAMETER_KIND => {
            // Presence before shape, as in `checked_region`.
            let canary = wire
                .canary_parameter
                .as_deref()
                .ok_or(SpecRefusal::Unreadable("canary_missing"))?;
            let region = checked_region(wire, SSM_TOKEN_PREFIX)?;
            if !canary_shape_ok(canary) {
                return Err(SpecRefusal::CanaryShape);
            }
            Ok(RegionalProbe::SsmGetParameter {
                region,
                canary: canary.to_string(),
            })
        }
        ECS_LIST_CLUSTERS_KIND => {
            let region = checked_region(wire, ECS_TOKEN_PREFIX)?;
            if wire.canary_parameter.is_some() {
                return Err(SpecRefusal::Refused("canary_on_kind_without_one"));
            }
            Ok(RegionalProbe::EcsListClusters { region })
        }
        _ => Err(SpecRefusal::UnknownKind),
    }
}

/// A kind name fit for a log line: lowercase snake case, bounded, and no account-id
/// run. Anything else is coord's bytes in a shape no kind has, so it is not repeated.
fn loggable_kind(kind: &str) -> &str {
    let safe = !kind.is_empty()
        && kind.len() <= KIND_LOG_MAX_LEN
        && !has_account_id_run(kind)
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if safe {
        kind
    } else {
        "<unprintable>"
    }
}

/// A token fit for a log line: D1's `cred:<service>[:<scope>]` grammar, which also
/// keeps account ids, ARNs, paths and emails out. Anything else is not repeated.
fn loggable_token(token: &str) -> &str {
    if token_grammar_ok(token) {
        token
    } else {
        "<unprintable>"
    }
}

/// D1's token grammar, as [`loggable_token`] applies it.
fn token_grammar_ok(token: &str) -> bool {
    let segments: Vec<&str> = token.split(':').collect();
    token.len() <= 64
        && !has_account_id_run(token)
        && !token.contains("arn:")
        && (2..=3).contains(&segments.len())
        && segments.first() == Some(&"cred")
        && segments.iter().all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Log one row that will not run, in the bounded, sanitized form.
fn log_spec_refusal(wire: Option<&WireSpec>, refusal: SpecRefusal) {
    let kind = wire.map_or("<unprintable>", |w| loggable_kind(&w.probe_kind));
    match refusal {
        SpecRefusal::UnknownKind => {
            info!("host_capabilities: cred probe spec skipped: kind={kind} is not known to this build");
        }
        SpecRefusal::CanaryShape => {
            warn!("host_capabilities: cred probe spec refused: kind={kind} reason=canary_shape");
        }
        SpecRefusal::Refused(reason) | SpecRefusal::Unreadable(reason) => {
            let token = wire
                .and_then(|w| w.token.as_deref())
                .map_or("<unprintable>", loggable_token);
            warn!(
                "host_capabilities: cred probe spec refused: token={token} kind={kind} reason={reason}"
            );
        }
    }
}

/// What one answered catalogue asks for.
pub(super) struct RegionalPlan {
    /// The specs to run this cycle, ordered by token, at most
    /// [`CRED_PROBE_SPEC_RUN_CAP`].
    pub(super) run: Vec<RegionalProbe>,
    /// Tokens that are listed but that nothing measured, so they fold as Unknown
    /// rather than retract: valid specs past the cap, and tokens named by rows of a
    /// kind this build does not know. A token some row ran, or a readable row named
    /// and failed a check on, is not here. Only values in D1's grammar are kept.
    pub(super) not_run: Vec<String>,
    /// Some row could not be read, so a token no row names may not be removed.
    pub(super) unreadable_rows: bool,
    /// Tokens named by rows that read and then failed a check. Only values in D1's
    /// grammar are kept: anything else cannot match an earned token, and this list
    /// must not carry coord's raw bytes.
    pub(super) refused_tokens: Vec<String>,
}

/// Filter one answered catalogue down to what may run.
///
/// The order is by (token, argv), so a catalogue listed in a different order
/// neither changes which specs the cap admits nor which row wins a duplicated
/// token. Coord's `UNIQUE (tenant_id, token)` should make a duplicate impossible,
/// which is not a reason to run one twice.
pub(super) fn plan_regional_specs(specs: &[serde_json::Value]) -> RegionalPlan {
    let mut valid: Vec<(String, Vec<String>, RegionalProbe)> = Vec::new();
    let mut refused = 0usize;
    let mut unreadable_rows = false;
    let mut refused_tokens: Vec<String> = Vec::new();
    let mut unknown_kind_tokens: Vec<String> = Vec::new();
    for spec in specs {
        // An array row would deserialize positionally (`visit_seq`) and run; only
        // an object is the shape coord serves, so anything else is unreadable.
        let parsed = if spec.is_object() {
            WireSpec::deserialize(spec).ok()
        } else {
            None
        };
        let verdict = match parsed.as_ref() {
            Some(wire) => validate_spec(wire),
            None => Err(SpecRefusal::Unreadable("malformed_row")),
        };
        match verdict {
            Ok(probe) => valid.push((probe.token(), probe.args(), probe)),
            Err(refusal) => {
                let named = parsed
                    .as_ref()
                    .and_then(|w| w.token.as_deref())
                    .filter(|t| token_grammar_ok(t))
                    .map(str::to_string);
                match refusal {
                    SpecRefusal::Unreadable(_) => unreadable_rows = true,
                    SpecRefusal::Refused(_) | SpecRefusal::CanaryShape => {
                        refused_tokens.extend(named);
                    }
                    SpecRefusal::UnknownKind => unknown_kind_tokens.extend(named),
                }
                if refused < SPEC_REFUSAL_LOG_CAP {
                    log_spec_refusal(parsed.as_ref(), refusal);
                }
                refused += 1;
            }
        }
    }
    if refused > SPEC_REFUSAL_LOG_CAP {
        warn!(
            "host_capabilities: cred probe catalogue: {} more specs would not run",
            refused - SPEC_REFUSAL_LOG_CAP
        );
    }
    valid.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    valid.dedup_by(|a, b| a.0 == b.0);
    if valid.len() > CRED_PROBE_SPEC_RUN_CAP {
        info!(
            "host_capabilities: cred probe catalogue lists {} runnable specs; this cycle runs {CRED_PROBE_SPEC_RUN_CAP}",
            valid.len()
        );
    }
    let mut run = Vec::with_capacity(valid.len().min(CRED_PROBE_SPEC_RUN_CAP));
    let mut not_run = Vec::new();
    for (token, _, probe) in valid {
        if run.len() < CRED_PROBE_SPEC_RUN_CAP {
            run.push(probe);
        } else {
            not_run.push(token);
        }
    }
    // After the split, because a verdict or a refusal for the same token outranks
    // this row. Folding Unknown over a fresh verdict would start the hold's clock
    // on a token that was just measured.
    for token in unknown_kind_tokens {
        let ran = run.iter().any(|probe| probe.token() == token);
        if !ran && !refused_tokens.contains(&token) && !not_run.contains(&token) {
            not_run.push(token);
        }
    }
    not_run.sort();
    RegionalPlan {
        run,
        not_run,
        unreadable_rows,
        refused_tokens,
    }
}

/// D2's regional table. `exit` is `None` when aws is not on PATH. It is aws's own
/// table, and it holds for the SSM canary without change: exit 254 is already
/// Unknown there, and has to stay so here because `ParameterNotFound` and
/// `AccessDenied` share it with expired credentials. So an expired credential
/// retracts a regional token through the Unknown arm, at most one TTL late.
///
/// The match is exhaustive so that adding a kind forces a decision about its table.
pub(super) fn classify_regional(
    probe: &RegionalProbe,
    exit: Option<ProbeExit>,
    has_config: impl FnOnce() -> bool,
) -> CredState {
    match probe {
        RegionalProbe::SsmGetParameter { .. } | RegionalProbe::EcsListClusters { .. } => {
            classify_aws_sts(exit, has_config)
        }
    }
}

/// Run one validated spec through the family's bounded, null-stdio, token-withheld
/// probe, and classify by exit code only.
pub(super) fn probe_regional(env: &CredProbeEnv, probe: &RegionalProbe) -> CredState {
    let exit = match env.aws.as_deref() {
        Some(aws) => {
            let args = probe.args();
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            Some(command_exit_within(
                cred_probe_command(aws, &argv),
                env.budget,
            ))
        }
        None => {
            log_cred_binary_not_on_path(&probe.token(), "aws");
            None
        }
    };
    classify_regional(probe, exit, || aws_has_config(env))
}

/// One cycle's regional result, ready to fold into the family's cache. Holds
/// tokens and states only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RegionalCycle {
    /// The catalogue did not answer. Earned tokens follow D2's Unknown rule.
    NoAnswer,
    /// The catalogue answered. `not_run` tokens are listed but unmeasured, and fold
    /// as Unknown. A token in neither `verdicts` nor `not_run` was
    /// removed, unless `unreadable_rows` is set and `refused_tokens` does not name
    /// it: then nothing said it was removed, and it folds as Unknown.
    Answered {
        verdicts: Vec<(String, CredState)>,
        not_run: Vec<String>,
        unreadable_rows: bool,
        refused_tokens: Vec<String>,
    },
}

/// Why a fetch produced no answer. Value-free by construction: it carries a status
/// code at most, never transport or parser text, which can quote the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpecFetchFailure {
    /// No connected coord to ask.
    NoCoord,
    /// No tenant binding, so no credential to ask with.
    NoBinding,
    /// The HTTP client could not be built.
    Client,
    /// Connect, send or body-read failure, a timeout included.
    Network,
    /// A non-2xx answer. A 404 means the route is not deployed yet.
    Status(u16),
    /// A body over [`PROBE_SPECS_BODY_CAP`].
    Oversized,
    /// A 2xx whose body is not `{"specs":[...]}`.
    Malformed,
}

/// Parse a catalogue body. `specs` must be a present ARRAY: `{}` and
/// `{"specs":null}` are malformed rather than empty, because reading them as empty
/// would retract every regional token on a response that said nothing. Rows stay
/// raw here so that one row a newer coord shaped differently cannot make the whole
/// catalogue unreadable; [`plan_regional_specs`] judges them one by one.
///
/// Only an OBJECT body is read: serde's derive also accepts a JSON array for a
/// named struct (`visit_seq`), so `[[]]` would read as an empty catalogue and
/// retract every regional token at once. The derived struct is kept rather than a
/// `Value` because it refuses a repeated `specs` key, where a `Value` map keeps
/// the last one, so `{"specs":[...],"specs":[]}` would read as empty too.
pub(super) fn parse_specs_body(body: &[u8]) -> Result<Vec<serde_json::Value>, SpecFetchFailure> {
    #[derive(Deserialize)]
    struct SpecsBody {
        specs: Vec<serde_json::Value>,
    }
    if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(SpecFetchFailure::Malformed);
    }
    serde_json::from_slice::<SpecsBody>(body)
        .map(|b| b.specs)
        .map_err(|_| SpecFetchFailure::Malformed)
}

/// Read the body up to [`PROBE_SPECS_BODY_CAP`] and `deadline`. reqwest's blocking
/// timeout bounds each read on its own, so this loop is what bounds a trickle.
fn read_specs_body(resp: &mut impl Read, deadline: Instant) -> Result<Vec<u8>, SpecFetchFailure> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match resp.read(&mut chunk) {
            Ok(0) => return Ok(body),
            Ok(n) => {
                body.extend_from_slice(chunk.get(..n).unwrap_or_default());
                if body.len() as u64 > PROBE_SPECS_BODY_CAP {
                    return Err(SpecFetchFailure::Oversized);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(SpecFetchFailure::Network),
        }
        if Instant::now() >= deadline {
            return Err(SpecFetchFailure::Network);
        }
    }
}

/// Send a prepared catalogue request and read its answer. With a client built by
/// [`probe_specs_client`] for the same `budget`, this returns in less than
/// 2 × `budget`: the deadline is checked between reads, and each read has its own
/// timeout, so one read that started before the deadline can run past it.
fn read_specs_response(
    request: reqwest::blocking::RequestBuilder,
    budget: Duration,
) -> Result<Vec<serde_json::Value>, SpecFetchFailure> {
    let deadline = Instant::now() + budget;
    let mut resp = request.send().map_err(|_| SpecFetchFailure::Network)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(SpecFetchFailure::Status(status.as_u16()));
    }
    if resp
        .content_length()
        .is_some_and(|n| n > PROBE_SPECS_BODY_CAP)
    {
        return Err(SpecFetchFailure::Oversized);
    }
    let body = read_specs_body(&mut resp, deadline)?;
    parse_specs_body(&body)
}

/// The catalogue's HTTP client: the OS resolver the heartbeat's client uses, in
/// blocking form because this runs on the family's detached refresh thread, off
/// every runtime. It never follows a redirect, which would not be the catalogue.
fn probe_specs_client(timeout: Duration) -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .dns_resolver(qontinui_runner_lib::outbound_net::os_dns::os_resolver())
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Fetch this tenant's specs the way the heartbeat reaches coord: the connected
/// coord base and the bearer the heartbeat's binding set names.
pub(super) fn fetch_probe_specs() -> Result<Vec<serde_json::Value>, SpecFetchFailure> {
    let base =
        qontinui_runner_lib::profiles::connected_coord_base().ok_or(SpecFetchFailure::NoCoord)?;
    let bindings = super::resolve_heartbeat_binding_set().ok_or(SpecFetchFailure::NoBinding)?;
    let client = probe_specs_client(CRED_PROBE_BUDGET).map_err(|_| SpecFetchFailure::Client)?;
    let url = format!("{base}{PROBE_SPECS_PATH}");
    let request =
        crate::auth::attach_device_auth_blocking(client.get(&url), bindings.bearer_scope());
    read_specs_response(request, CRED_PROBE_BUDGET)
}

/// Log a fetch that did not answer. A 404 is `debug!`: it is every cycle's answer
/// until the route is deployed, and nothing is wrong. Any other failure is `info!`.
fn log_no_answer(cause: SpecFetchFailure) {
    if cause == SpecFetchFailure::Status(404) {
        debug!(
            "host_capabilities: cred probe catalogue: no regional answer this cycle ({cause:?})"
        );
    } else {
        info!("host_capabilities: cred probe catalogue: no regional answer this cycle ({cause:?})");
    }
}

/// Turn one fetch into one cycle's regional result, running every spec that may
/// run. Slow (up to [`CRED_PROBE_SPEC_RUN_CAP`] × `env.budget`), so production
/// calls it only from the refresh thread.
pub(super) fn run_regional_cycle(
    fetched: Result<Vec<serde_json::Value>, SpecFetchFailure>,
    env: &CredProbeEnv,
) -> RegionalCycle {
    let specs = match fetched {
        Ok(specs) => specs,
        Err(cause) => {
            log_no_answer(cause);
            return RegionalCycle::NoAnswer;
        }
    };
    let plan = plan_regional_specs(&specs);
    RegionalCycle::Answered {
        verdicts: plan
            .run
            .iter()
            .map(|probe| (probe.token(), probe_regional(env, probe)))
            .collect(),
        not_run: plan.not_run,
        unreadable_rows: plan.unreadable_rows,
        refused_tokens: plan.refused_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        capture_cred_logs, cred_payload_capabilities, hermetic_cred_env, write_cred_shim, Shim,
        CRED_FAKE_SECRET, CRED_TEST_TTL,
    };
    use super::super::{run_cred_refresh_cycle, CredFamilyState, CRED_PROBES_MARKER};
    use super::*;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    const SSM_EU: &str = "cred:aws-ssm:eu-central-1";
    const ECS_EU: &str = "cred:aws-ecs:eu-central-1";
    const CANARY: &str = "/qontinui/fleet/canary-marker";

    fn ssm_spec(region: &str) -> serde_json::Value {
        json!({
            "token": format!("cred:aws-ssm:{region}"),
            "probe_kind": "aws_ssm_get_parameter",
            "region": region,
            "canary_parameter": CANARY,
        })
    }

    /// An answered cycle over a body with no unreadable or refused row.
    fn answered(verdicts: Vec<(String, CredState)>, not_run: Vec<String>) -> RegionalCycle {
        RegionalCycle::Answered {
            verdicts,
            not_run,
            unreadable_rows: false,
            refused_tokens: Vec::new(),
        }
    }

    /// A row valid in every field but, possibly, the canary under test.
    fn ssm_spec_with_canary(canary: &str) -> serde_json::Value {
        let mut spec = ssm_spec("eu-central-1");
        spec["canary_parameter"] = json!(canary);
        spec
    }

    fn ecs_spec(region: &str) -> serde_json::Value {
        json!({
            "token": format!("cred:aws-ecs:{region}"),
            "probe_kind": "aws_ecs_list_clusters",
            "region": region,
            "canary_parameter": null,
        })
    }

    /// One probe of every variant, through the validator so the fixture cannot
    /// hold what production refuses.
    fn every_probe() -> Vec<RegionalProbe> {
        let probes: Vec<RegionalProbe> = [ssm_spec("eu-central-1"), ecs_spec("eu-central-1")]
            .iter()
            .map(|spec| {
                validate_spec(&WireSpec::deserialize(spec).unwrap())
                    .ok()
                    .unwrap()
            })
            .collect();
        for probe in &probes {
            // A new variant stops compiling here: add it to the fixture above.
            match probe {
                RegionalProbe::SsmGetParameter { .. } | RegionalProbe::EcsListClusters { .. } => {}
            }
        }
        probes
    }

    /// A shim that appends one `args=...` line to `report`, prints the fake secret
    /// on both streams and exits with `code`.
    fn write_recording_shim(dir: &Path, name: &str, report: &Path, code: i32) -> PathBuf {
        #[cfg(windows)]
        {
            let path = dir.join(format!("{name}.cmd"));
            let body = format!(
                "@echo off\r\n\
                 >>\"{report}\" echo args=%*\r\n\
                 echo {CRED_FAKE_SECRET}\r\n\
                 echo {CRED_FAKE_SECRET} 1>&2\r\n\
                 exit /b {code}\r\n",
                report = report.display(),
            );
            std::fs::write(&path, body).unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            let path = dir.join(name);
            let body = format!(
                "#!/bin/sh\n\
                 echo \"args=$*\" >>'{report}'\n\
                 echo {CRED_FAKE_SECRET}\n\
                 echo {CRED_FAKE_SECRET} >&2\n\
                 exit {code}\n",
                report = report.display(),
            );
            super::super::tests::write_executable_script(&path, &body);
            path
        }
    }

    fn recorded_args(report: &Path) -> Vec<String> {
        match std::fs::read_to_string(report) {
            Ok(text) => text.lines().map(|l| l.trim().to_string()).collect(),
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "read the report");
                Vec::new()
            }
        }
    }

    /// A hermetic env under `tmp` with a generous budget: these tests classify
    /// real child exits on a box that may be saturated, and only a Hang spends it.
    fn shim_env(tmp: &Path) -> CredProbeEnv {
        let mut env = hermetic_cred_env(tmp);
        env.budget = Duration::from_secs(60);
        env
    }

    fn tmp_with_bin() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        (tmp, bin)
    }

    #[test]
    fn regional_argv_is_pinned_per_kind_and_each_value_is_one_entry() {
        let expected: [Vec<&str>; 2] = [
            vec![
                "ssm",
                "get-parameter",
                "--region",
                "eu-central-1",
                "--name",
                CANARY,
                "--query",
                "Parameter.Name",
                "--output",
                "text",
            ],
            vec![
                "ecs",
                "list-clusters",
                "--region",
                "eu-central-1",
                "--max-items",
                "1",
            ],
        ];
        let probes = every_probe();
        assert_eq!(probes.len(), expected.len());
        for (probe, want) in probes.iter().zip(expected) {
            assert_eq!(probe.args(), want);
        }
    }

    /// `--with-decryption` is in NO argv the runner can build, over every variant,
    /// and no hostile canary can put it there.
    #[test]
    fn with_decryption_appears_in_no_argv_the_runner_can_build() {
        for probe in every_probe() {
            let args = probe.args();
            assert!(!args.is_empty(), "anti-vacuity: an argv was built");
            assert!(
                !args
                    .iter()
                    .any(|a| a.to_ascii_lowercase().contains("decrypt")),
                "{} built a decrypting argv: {args:?}",
                probe.token()
            );
        }
        for canary in [
            "--with-decryption",
            "-with-decryption",
            "--with-decryption=true",
        ] {
            let mut spec = ssm_spec("eu-central-1");
            spec["canary_parameter"] = json!(canary);
            let plan = plan_regional_specs(&[spec]);
            assert!(
                plan.run.is_empty() && plan.not_run.is_empty(),
                "the canary {canary:?} must run nothing"
            );
        }
    }

    /// The flag is not spelled anywhere in production source, comments included,
    /// so no later builder can inherit it from a constant here.
    #[test]
    fn the_decrypting_flag_is_spelled_nowhere_in_production_source() {
        const MARKER: &str = "\n#[cfg(test)]\nmod tests {";
        let flag = ["--with", "-decryption"].concat();
        for (name, src) in [
            (
                "fleet/cred_probe_specs.rs",
                include_str!("cred_probe_specs.rs"),
            ),
            ("fleet.rs", include_str!("../fleet.rs")),
        ] {
            let normalized = src.replace("\r\n", "\n");
            let (prod, _) = normalized
                .split_once(MARKER)
                .unwrap_or_else(|| panic!("{name}: the test module header must be found"));
            assert!(
                !prod.contains(&flag),
                "{name} spells the flag in production"
            );
        }
    }

    /// Each row's shape, through the same validator production uses.
    #[test]
    fn hostile_region_and_canary_values_are_refused() {
        for region in [
            "",
            "eu",
            "eu-central",
            "EU-central-1",
            "e-central-1",
            "eur-central-1",
            "eu--1",
            "eu-central-123",
            "eu-central-1;x",
            "eu-central-1 ",
            "eu-central-1\n",
            "--region",
            "-eu-central-1",
            "eu-central-1/../x",
            "123456789012",
            "eu-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-1",
        ] {
            assert!(
                !region_shape_ok(region),
                "region {region:?} must be refused"
            );
        }
        for region in [
            "eu-central-1",
            "us-east-1",
            "us-gov-west-1",
            "ap-southeast-2",
        ] {
            assert!(
                region_shape_ok(region),
                "region {region:?} must be accepted"
            );
        }
        for canary in [
            "",
            "-x",
            "--with-decryption",
            "--debug",
            "file:///etc/passwd",
            "fileb:///etc/passwd",
            "https://evil.example/x",
            "arn:aws:ssm:eu-central-1:123456789012:parameter/x",
            "a b",
            "a;b",
            "a&b",
            "a|b",
            "$(x)",
            "`x`",
            "a\nb",
            "%PATH%",
            "\"quoted\"",
            "/prod/db/password",
            "/prod/db/PassWord",
            "/x/client_secret",
            "/x/api-key",
            "/x/monkey",
            "/x/Token",
            "/x/credentials",
        ] {
            assert!(
                !canary_shape_ok(canary),
                "canary {canary:?} must be refused"
            );
        }
        assert!(!canary_shape_ok(&"a".repeat(CANARY_MAX_LEN + 1)));
        assert!(canary_shape_ok(&"a".repeat(CANARY_MAX_LEN)));
        for canary in [CANARY, "qontinui.canary_1", "/a/b-c/d.e"] {
            assert!(
                canary_shape_ok(canary),
                "canary {canary:?} must be accepted"
            );
        }
    }

    /// A mismatched token, region or unknown kind runs NOTHING: the recording shim
    /// is really on `env.aws`, and the anti-vacuity row proves it would have run.
    #[test]
    fn a_mismatched_or_unknown_spec_runs_nothing() {
        let (tmp, bin) = tmp_with_bin();
        let report = tmp.path().join("calls.txt");
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_recording_shim(&bin, "aws", &report, 0));

        let refused: Vec<(&str, serde_json::Value)> = vec![
            (
                "token names another region",
                json!({"token":"cred:aws-ssm:us-east-1","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "token names another kind",
                json!({"token":"cred:aws-ecs:eu-central-1","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "ecs row with an ssm token",
                json!({"token":"cred:aws-ssm:eu-central-1","probe_kind":"aws_ecs_list_clusters",
                       "region":"eu-central-1","canary_parameter":null}),
            ),
            (
                "token has a suffix",
                json!({"token":"cred:aws-ssm:eu-central-1:x","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "built-in token",
                json!({"token":"cred:gh","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "the marker as a token",
                json!({"token":CRED_PROBES_MARKER,"probe_kind":"aws_ecs_list_clusters",
                       "region":"eu-central-1","canary_parameter":null}),
            ),
            (
                "bad region",
                json!({"token":"cred:aws-ssm:eu;x","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu;x","canary_parameter":CANARY}),
            ),
            (
                "unknown kind",
                json!({"token":"cred:aws-lambda:eu-central-1","probe_kind":"aws_lambda_list_functions",
                       "region":"eu-central-1","canary_parameter":null}),
            ),
            (
                "unknown kind carrying a known token",
                json!({"token":SSM_EU,"probe_kind":"aws_ssm_put_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "kind differs by case",
                json!({"token":SSM_EU,"probe_kind":"AWS_SSM_GET_PARAMETER",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "ssm with no canary",
                json!({"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":null}),
            ),
            (
                "canary on a kind without one",
                json!({"token":ECS_EU,"probe_kind":"aws_ecs_list_clusters",
                       "region":"eu-central-1","canary_parameter":CANARY}),
            ),
            (
                "no token",
                json!({"probe_kind":"aws_ecs_list_clusters","region":"eu-central-1"}),
            ),
            (
                "no region",
                json!({"token":ECS_EU,"probe_kind":"aws_ecs_list_clusters"}),
            ),
            (
                "wrong field type",
                json!({"token":7,"probe_kind":"aws_ecs_list_clusters","region":"eu-central-1"}),
            ),
            ("not an object", json!("cred:aws-ssm:eu-central-1")),
            ("no kind", json!({"token":SSM_EU,"region":"eu-central-1"})),
            (
                "canary is the decrypting flag",
                ssm_spec_with_canary("--with-decryption"),
            ),
            (
                "canary is the flag with a value",
                ssm_spec_with_canary("--with-decryption=true"),
            ),
            ("canary starts with a dash", ssm_spec_with_canary("-x")),
            (
                "canary is an account id",
                ssm_spec_with_canary("123456789012"),
            ),
            (
                "canary holds an account id",
                ssm_spec_with_canary("/prod/123456789012/canary"),
            ),
            (
                "canary holds a dashed account id",
                ssm_spec_with_canary("/prod/1234-5678-9012/canary"),
            ),
        ];
        for (case, spec) in refused {
            let RegionalCycle::Answered { verdicts, .. } = run_regional_cycle(Ok(vec![spec]), &env)
            else {
                panic!("{case}: an answered catalogue");
            };
            assert!(verdicts.is_empty(), "{case}");
            assert_eq!(
                recorded_args(&report),
                Vec::<String>::new(),
                "{case} ran aws"
            );
        }

        // Anti-vacuity: a valid row beside a refused one runs, once, as itself.
        let cycle = run_regional_cycle(
            Ok(vec![
                json!({"token":"cred:aws-ssm:us-east-1","probe_kind":"aws_ssm_get_parameter",
                       "region":"eu-central-1","canary_parameter":CANARY}),
                ecs_spec("eu-central-1"),
            ]),
            &env,
        );
        assert_eq!(
            cycle,
            RegionalCycle::Answered {
                verdicts: vec![(ECS_EU.to_string(), CredState::Present)],
                not_run: Vec::new(),
                unreadable_rows: false,
                refused_tokens: vec!["cred:aws-ssm:us-east-1".to_string()],
            }
        );
        assert_eq!(
            recorded_args(&report),
            vec!["args=ecs list-clusters --region eu-central-1 --max-items 1"]
        );

        // The served flag never reached an argv, and a good canary does: the same
        // row with only the canary changed runs, so the refusals above were the
        // canary's and not the fixture's.
        for canary in ["--with-decryption", "--with-decryption=true"] {
            run_regional_cycle(Ok(vec![ssm_spec_with_canary(canary)]), &env);
        }
        assert!(
            !recorded_args(&report)
                .iter()
                .any(|line| line.contains("decryption")),
            "a served canary reached an argv: {:?}",
            recorded_args(&report)
        );
        run_regional_cycle(Ok(vec![ssm_spec_with_canary(CANARY)]), &env);
        let calls = recorded_args(&report);
        assert!(
            calls.last().is_some_and(|l| l.contains(CANARY)),
            "anti-vacuity: the SSM row must run with a good canary, got {calls:?}"
        );
    }

    /// A refused canary is logged by kind and reason alone: neither the value nor
    /// the row's token appears, and an 11-digit run or a split one is not refused.
    #[test]
    fn a_refused_canary_is_logged_by_kind_only() {
        for canary in [
            "--with-decryption",
            "-x",
            "/prod/123456789012/canary",
            "/prod/1234-5678-9012/canary",
        ] {
            let (plan, logs) =
                capture_cred_logs(|| plan_regional_specs(&[ssm_spec_with_canary(canary)]));
            assert!(plan.run.is_empty(), "{canary:?} must run nothing");
            assert!(
                logs.contains("kind=aws_ssm_get_parameter") && logs.contains("reason=canary_shape"),
                "anti-vacuity: {canary:?} must log its kind and reason, got {logs:?}"
            );
            assert!(!logs.contains(canary), "{canary:?} reached a log: {logs:?}");
            assert!(!logs.contains(SSM_EU), "a token reached the line: {logs:?}");
            for fragment in ["123456789012", "1234-5678-9012", "decryption"] {
                assert!(
                    !logs.contains(fragment),
                    "{fragment} reached a log: {logs:?}"
                );
            }
        }
        for canary in [
            "/prod/12345678901/canary",
            "/123456/789012",
            "/1234_5678_9012",
        ] {
            assert!(canary_shape_ok(canary), "{canary:?} must be accepted");
        }
        for canary in ["1234567890123", "/a/12-34-56-78-90-12"] {
            assert!(!canary_shape_ok(canary), "{canary:?} must be refused");
        }
    }

    #[test]
    fn a_kind_is_repeated_only_when_it_is_kind_shaped() {
        for kind in ["aws_lambda_list", "aws_ssm_get_parameter"] {
            assert_eq!(loggable_kind(kind), kind);
        }
        let at_bound = "a".repeat(KIND_LOG_MAX_LEN);
        assert_eq!(loggable_kind(&at_bound), at_bound);
        let over_bound = "a".repeat(KIND_LOG_MAX_LEN + 1);
        for kind in [
            "",
            "123456789012",
            "x_123456789012_y",
            "BAD KIND",
            over_bound.as_str(),
        ] {
            assert_eq!(loggable_kind(kind), "<unprintable>", "{kind:?}");
        }
    }

    /// `{"specs":[null]}` and rows a newer coord renamed or nested pass the body
    /// check and name nothing. Their silence is not a removal: earned tokens hold
    /// for one TTL and then drop, and the built-ins are untouched.
    #[test]
    fn unreadable_rows_hold_earned_tokens_for_one_ttl_then_drop() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_cred_shim(&bin, "aws", Shim::Exit(0)));
        let shapes = [
            json!({"specs":[null]}),
            json!({"specs":["cred:aws-ssm:eu-central-1"]}),
            json!({"specs":[{"spec":ssm_spec("eu-central-1")}]}),
            json!({"specs":[{"token":SSM_EU,"kind":"aws_ssm_get_parameter",
                             "region":"eu-central-1","canary_parameter":CANARY}]}),
            json!({"specs":[{"capability":SSM_EU,"probe_kind":"aws_ssm_get_parameter",
                             "region":"eu-central-1","canary_parameter":CANARY}]}),
            json!({"specs":[{"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter",
                             "aws_region":"eu-central-1","canary_parameter":CANARY}]}),
            json!({"specs":[{"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter",
                             "region":"eu-central-1","canary":CANARY}]}),
        ];
        for shape in shapes {
            let body = serde_json::to_vec(&shape).unwrap();
            let read = || parse_specs_body(&body);
            assert!(read().is_ok(), "the body check passes {shape}");
            let cache = Mutex::new(CredFamilyState::new());
            let t0 = Instant::now();
            let earned = vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU, SSM_EU];
            let bare = vec!["os:linux", "probes:cred", "cred:aws-sts"];
            let both = vec![ssm_spec("eu-central-1"), ecs_spec("eu-central-1")];
            assert_eq!(cycle(&cache, &env, Ok(both), t0), earned);

            let t1 = t0 + CRED_TEST_TTL;
            assert_eq!(cycle(&cache, &env, read(), t1), earned, "{shape} holds");
            assert_eq!(
                cycle(&cache, &env, read(), t1 + CRED_TEST_TTL / 2),
                earned,
                "{shape} still holds inside the TTL"
            );
            assert_eq!(
                cycle(&cache, &env, read(), t1 + CRED_TEST_TTL),
                bare,
                "{shape} drops after one whole TTL, and the built-ins stay"
            );
        }
    }

    /// An unreadable row does not shield a token a READABLE row refuses: that row
    /// named it and failed, so it retracts at once while the unnamed token holds. A
    /// valid row beside them still folds its own verdict.
    #[test]
    fn an_unreadable_row_does_not_shield_a_token_a_readable_row_refuses() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_cred_shim(&bin, "aws", Shim::Exit(0)));
        let cache = Mutex::new(CredFamilyState::new());
        let t0 = Instant::now();
        let start = vec![
            ssm_spec("eu-central-1"),
            ecs_spec("eu-central-1"),
            ecs_spec("us-east-1"),
        ];
        assert_eq!(
            cycle(&cache, &env, Ok(start), t0),
            vec![
                "os:linux",
                "probes:cred",
                "cred:aws-sts",
                ECS_EU,
                "cred:aws-ecs:us-east-1",
                SSM_EU,
            ]
        );
        let mixed = vec![
            serde_json::Value::Null,
            ssm_spec_with_canary("--with-decryption"),
            ecs_spec("eu-central-1"),
        ];
        assert_eq!(
            cycle(&cache, &env, Ok(mixed), t0 + CRED_TEST_TTL),
            vec![
                "os:linux",
                "probes:cred",
                "cred:aws-sts",
                ECS_EU,
                "cred:aws-ecs:us-east-1",
            ],
            "SSM was named and refused; us-east-1 was named by nothing, so it holds"
        );
    }

    /// A listed row of a kind this build does not know could not be measured, so
    /// D2's Unknown applies: the earned token it names holds for one TTL and then
    /// drops, while it runs nothing. A spec REMOVED from the same readable body still
    /// retracts at once, so the hold is the row's and not a general leniency.
    #[test]
    fn a_listed_unknown_kind_holds_the_token_it_names_for_one_ttl_then_drops() {
        let (tmp, bin) = tmp_with_bin();
        let report = tmp.path().join("calls.txt");
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_recording_shim(&bin, "aws", &report, 0));
        let renamed = || {
            json!({"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter_v2",
                   "region":"eu-central-1","canary_parameter":CANARY})
        };
        let listed = || Ok(vec![renamed(), ecs_spec("eu-central-1")]);
        let cache = Mutex::new(CredFamilyState::new());
        let t0 = Instant::now();
        let both = vec![
            ssm_spec("eu-central-1"),
            ecs_spec("eu-central-1"),
            ecs_spec("us-east-1"),
        ];
        let earned = vec![
            "os:linux",
            "probes:cred",
            "cred:aws-sts",
            ECS_EU,
            "cred:aws-ecs:us-east-1",
            SSM_EU,
        ];
        assert_eq!(cycle(&cache, &env, Ok(both), t0), earned);

        // us-east-1 is genuinely gone from this readable body: it retracts here,
        // in the same cycle that holds SSM.
        let held = vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU, SSM_EU];
        let calls_before = recorded_args(&report).len();
        let t1 = t0 + CRED_TEST_TTL;
        assert_eq!(cycle(&cache, &env, listed(), t1), held, "SSM holds");
        assert_eq!(
            cycle(&cache, &env, listed(), t1 + CRED_TEST_TTL / 2),
            held,
            "SSM still holds inside the TTL"
        );
        assert_eq!(
            cycle(&cache, &env, listed(), t1 + CRED_TEST_TTL),
            vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU],
            "SSM drops after one whole TTL, and the rest stay"
        );
        let calls = recorded_args(&report);
        assert!(
            calls
                .get(calls_before..)
                .unwrap_or_default()
                .iter()
                .all(|line| !line.contains("ssm")),
            "the row of an unknown kind must run nothing, got {calls:?}"
        );
    }

    /// The unknown-kind row loses to any row that says more about the same token,
    /// and a token that never entered the D1 grammar is not kept at all.
    #[test]
    fn an_unknown_kind_row_is_outranked_by_a_verdict_or_a_refusal_for_its_token() {
        let renamed = json!({"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter_v2",
                             "region":"eu-central-1","canary_parameter":CANARY});

        let plan = plan_regional_specs(&[renamed.clone()]);
        assert_eq!(
            plan.not_run,
            vec![SSM_EU],
            "alone, it is listed but unmeasured"
        );
        assert_eq!(plan.refused_tokens, Vec::<String>::new());
        assert!(!plan.unreadable_rows, "an unknown kind is a readable row");

        let plan = plan_regional_specs(&[renamed.clone(), ssm_spec("eu-central-1")]);
        let tokens: Vec<String> = plan.run.iter().map(RegionalProbe::token).collect();
        assert_eq!(tokens, vec![SSM_EU], "the valid row still runs");
        assert_eq!(
            plan.not_run,
            Vec::<String>::new(),
            "a fresh verdict must not also fold as Unknown"
        );

        let plan =
            plan_regional_specs(&[renamed.clone(), ssm_spec_with_canary("--with-decryption")]);
        assert_eq!(plan.refused_tokens, vec![SSM_EU]);
        assert_eq!(
            plan.not_run,
            Vec::<String>::new(),
            "a refusal keeps its immediate retraction"
        );

        let plan = plan_regional_specs(&[renamed.clone(), renamed]);
        assert_eq!(plan.not_run, vec![SSM_EU], "named twice, listed once");

        let raw = json!({"token":CRED_FAKE_SECRET,"probe_kind":"aws_lambda_list"});
        let plan = plan_regional_specs(&[raw]);
        assert_eq!(plan.not_run, Vec::<String>::new());

        // A verdict over a held token ends the hold rather than extending it.
        let mut state = CredFamilyState::new();
        let t0 = Instant::now();
        let token = SSM_EU.to_string();
        state.apply_regional(
            &answered(vec![(token.clone(), CredState::Present)], Vec::new()),
            t0,
            CRED_TEST_TTL,
        );
        state.apply_regional(
            &answered(Vec::new(), vec![token.clone()]),
            t0 + CRED_TEST_TTL,
            CRED_TEST_TTL,
        );
        assert!(state
            .regional
            .get(&token)
            .is_some_and(|m| m.unknown_since.is_some()));
        state.apply_regional(
            &answered(vec![(token.clone(), CredState::Present)], Vec::new()),
            t0 + CRED_TEST_TTL * 2,
            CRED_TEST_TTL,
        );
        assert!(state
            .regional
            .get(&token)
            .is_some_and(|m| m.present && m.unknown_since.is_none()));
    }

    /// Which rows set the flag, and that the refused list holds no raw bytes.
    #[test]
    fn only_an_unreadable_row_sets_the_flag_and_refused_tokens_stay_in_grammar() {
        let mismatched = json!({"token":"cred:aws-ssm:us-east-1","probe_kind":"aws_ssm_get_parameter",
                                "region":"eu-central-1","canary_parameter":CANARY});
        let unknown = json!({"token":"cred:aws-lambda:eu-central-1","probe_kind":"aws_lambda_list",
                             "region":"eu-central-1"});
        let readable = [
            mismatched,
            unknown,
            ssm_spec_with_canary("--with-decryption"),
            ecs_spec("eu-central-1"),
        ];
        let plan = plan_regional_specs(&readable);
        assert!(!plan.unreadable_rows);
        assert_eq!(
            plan.refused_tokens,
            vec!["cred:aws-ssm:us-east-1", SSM_EU],
            "the refused rows' own tokens"
        );

        let raw = json!({"token":CRED_FAKE_SECRET,"probe_kind":"aws_ssm_get_parameter",
                         "region":"eu-central-1","canary_parameter":CANARY});
        let plan = plan_regional_specs(&[raw]);
        assert!(!plan.unreadable_rows);
        assert_eq!(plan.refused_tokens, Vec::<String>::new());

        for unreadable in [
            serde_json::Value::Null,
            json!({"token":SSM_EU}),
            json!({"probe_kind":"aws_ecs_list_clusters","region":"eu-central-1"}),
            json!({"token":ECS_EU,"probe_kind":"aws_ecs_list_clusters"}),
            json!({"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter","region":"eu-central-1"}),
            // A positional array row deserializes into `WireSpec` and would run.
            json!([
                "cred:aws-ecs:eu-central-1",
                "aws_ecs_list_clusters",
                "eu-central-1",
                null
            ]),
            // A missing field outranks a bad shape: no token, or no canary,
            // beside a region that would be refused.
            json!({"probe_kind":"aws_ecs_list_clusters","region":"NOT A REGION"}),
            json!({"token":SSM_EU,"probe_kind":"aws_ssm_get_parameter","region":"NOT A REGION"}),
        ] {
            let plan = plan_regional_specs(std::slice::from_ref(&unreadable));
            assert!(plan.unreadable_rows, "{unreadable}");
            assert_eq!(plan.refused_tokens, Vec::<String>::new(), "{unreadable}");
        }
    }

    /// A newer coord's kind is logged by name and skipped; a kind that is not
    /// kind-shaped is not repeated; region and canary values never appear.
    #[test]
    fn an_unknown_kind_is_logged_by_name_and_refusals_repeat_no_values() {
        let unknown = json!({"token":"cred:aws-lambda:eu-central-1","probe_kind":"aws_lambda_list",
                             "region":"eu-central-1","canary_parameter":CANARY});
        let unprintable = json!({"token":"cred:x","probe_kind":"BAD KIND\ninjected log line",
                                 "region":"eu-central-1","canary_parameter":CANARY});
        let mismatched = json!({"token":"cred:aws-ssm:us-east-1","probe_kind":"aws_ssm_get_parameter",
                                "region":"eu-central-1","canary_parameter":CANARY});
        let injected = json!({"token":"cred:aws-ssm:x\ninjected log line","probe_kind":"aws_ssm_get_parameter",
                              "region":"eu-central-1","canary_parameter":CANARY});
        let (plan, logs) = capture_cred_logs(|| {
            plan_regional_specs(&[unknown, unprintable, mismatched, injected])
        });
        assert!(plan.run.is_empty());
        assert!(logs.contains("kind=aws_lambda_list"), "got {logs:?}");
        assert!(logs.contains("reason=token_mismatch"), "got {logs:?}");
        assert!(
            logs.contains("token=cred:aws-ssm:us-east-1"),
            "got {logs:?}"
        );
        assert!(!logs.contains("injected log line"), "got {logs:?}");
        assert!(!logs.contains(CANARY), "the canary reached a log: {logs:?}");
    }

    #[test]
    fn a_pathological_catalogue_logs_a_bounded_number_of_refusals() {
        let specs: Vec<serde_json::Value> = (0..500)
            .map(|i| json!({"probe_kind": format!("kind_{i}")}))
            .collect();
        let (plan, logs) = capture_cred_logs(|| plan_regional_specs(&specs));
        assert!(plan.run.is_empty());
        assert_eq!(
            logs.matches("cred probe spec skipped").count(),
            SPEC_REFUSAL_LOG_CAP
        );
        assert!(logs.contains(&format!("{} more specs", 500 - SPEC_REFUSAL_LOG_CAP)));
    }

    /// D2's table, row by row, per kind.
    #[test]
    fn exit_codes_classify_per_d2_for_every_kind() {
        use CredState::{Absent, Present, Unknown};
        for probe in every_probe() {
            let token = probe.token();
            let never = || -> bool { panic!("{token}: config consulted outside exit 253") };
            assert_eq!(
                classify_regional(&probe, None, never),
                Absent,
                "{token} not on PATH"
            );
            for (exit, want) in [
                (ProbeExit::Code(0), Present),
                (ProbeExit::Code(254), Unknown),
                (ProbeExit::Code(255), Unknown),
                (ProbeExit::Code(1), Unknown),
                (ProbeExit::Code(2), Unknown),
                (ProbeExit::TimedOut, Unknown),
                (ProbeExit::NoCode, Unknown),
                (ProbeExit::SpawnFailed, Unknown),
                (ProbeExit::WaitFailed, Unknown),
            ] {
                assert_eq!(
                    classify_regional(&probe, Some(exit), never),
                    want,
                    "{token} {exit:?}"
                );
            }
            let exit = Some(ProbeExit::Code(253));
            assert_eq!(
                classify_regional(&probe, exit, || false),
                Absent,
                "{token} 253 no config"
            );
            assert_eq!(
                classify_regional(&probe, exit, || true),
                Unknown,
                "{token} 253 config"
            );
        }
    }

    /// The same table end to end: a real child's real exit code, through
    /// `probe_regional`, for every kind.
    #[test]
    fn real_child_exits_classify_per_d2_for_every_kind() {
        let (tmp, bin) = tmp_with_bin();
        for (i, probe) in every_probe().iter().enumerate() {
            let token = probe.token();
            let mut env = shim_env(tmp.path());
            assert_eq!(
                probe_regional(&env, probe),
                CredState::Absent,
                "{token} no aws"
            );
            for (code, profile_set, want) in [
                (0, false, CredState::Present),
                (253, false, CredState::Absent),
                (253, true, CredState::Unknown),
                (254, false, CredState::Unknown),
                (255, false, CredState::Unknown),
                (1, false, CredState::Unknown),
            ] {
                env.aws_profile_set = profile_set;
                env.aws = Some(write_cred_shim(
                    &bin,
                    &format!("aws-{i}-{code}-{profile_set}"),
                    Shim::Exit(code),
                ));
                assert_eq!(probe_regional(&env, probe), want, "{token} exit {code}");
            }
            env.aws = Some(write_cred_shim(&bin, &format!("aws-{i}-hang"), Shim::Hang));
            env.budget = Duration::from_secs(1);
            assert_eq!(
                probe_regional(&env, probe),
                CredState::Unknown,
                "{token} timeout"
            );
        }
    }

    /// The Phase 2 no-leak test, ported: the shim prints the fake secret on both
    /// streams, and the catalogue itself plants it in every field of refused rows
    /// and in a malformed body. Nothing logged, sent, rendered or panicked may hold
    /// it, and the canary name may not reach a log either.
    #[test]
    fn regional_shim_secret_never_reaches_logs_payload_or_panic() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        let report = tmp.path().join("aws-stdio-report.txt");
        #[cfg(any(windows, target_os = "linux"))]
        {
            env.aws = Some(super::super::tests::write_stdio_report_shim(
                &bin, "aws", &report,
            ));
            // The Windows report spins up PowerShell + Add-Type.
            env.budget = Duration::from_secs(90);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            env.aws = Some(write_cred_shim(&bin, "aws", Shim::Exit(0)));
        }
        let hostile: Vec<serde_json::Value> = vec![
            json!({"token": CRED_FAKE_SECRET, "probe_kind": CRED_FAKE_SECRET,
                   "region": CRED_FAKE_SECRET, "canary_parameter": CRED_FAKE_SECRET}),
            json!({"token": format!("cred:aws-ssm:{CRED_FAKE_SECRET}"),
                   "probe_kind": "aws_ssm_get_parameter",
                   "region": CRED_FAKE_SECRET, "canary_parameter": CANARY}),
            json!({"token": SSM_EU, "probe_kind": "aws_ssm_get_parameter",
                   "region": "eu-central-1", "canary_parameter": CRED_FAKE_SECRET}),
            json!({"token": 7, "probe_kind": CRED_FAKE_SECRET}),
        ];
        let malformed = format!("{{\"specs\":\"{CRED_FAKE_SECRET}\"}}");
        let specs: Vec<serde_json::Value> = [ssm_spec("eu-central-1"), ecs_spec("eu-central-1")]
            .into_iter()
            .chain(hostile)
            .collect();
        let body = serde_json::to_vec(&json!({ "specs": specs })).unwrap();

        let cache = Mutex::new(CredFamilyState::new());
        let t0 = Instant::now();
        let (outcome, logs) = capture_cred_logs(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_cred_refresh_cycle(&cache, &env, || parse_specs_body(&body), t0, CRED_TEST_TTL);
                // A body that is malformed AND quotes the secret: the failure path.
                run_cred_refresh_cycle(
                    &cache,
                    &env,
                    || parse_specs_body(malformed.as_bytes()),
                    t0 + CRED_TEST_TTL / 2,
                    CRED_TEST_TTL,
                );
            }))
        });
        if let Err(payload) = outcome {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            assert!(
                !msg.contains(CRED_FAKE_SECRET),
                "a panic message carried the probe's secret"
            );
            panic!("the regional probes panicked: {msg}");
        }

        let state = cache.into_inner().unwrap();
        let (caps, wire) = cred_payload_capabilities(&state);
        assert_eq!(
            caps,
            vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU, SSM_EU,],
            "both valid rows earned their token; the refused ones earned none"
        );

        #[cfg(any(windows, target_os = "linux"))]
        {
            let text = std::fs::read_to_string(&report)
                .expect("anti-vacuity: the aws shim must have written its stdio report");
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            let args: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| l.starts_with("args="))
                .collect();
            let ssm_line = format!(
                "args=ssm get-parameter --region eu-central-1 --name {CANARY} \
                 --query Parameter.Name --output text"
            );
            let sts = "args=sts get-caller-identity --query Account --output text";
            let ecs = "args=ecs list-clusters --region eu-central-1 --max-items 1";
            // Two cycles of the built-in, one of each regional (the second cycle's
            // catalogue was malformed, so it ran no regional probe).
            assert_eq!(args, vec![sts, ecs, ssm_line.as_str(), sts], "got {text:?}");
            #[cfg(target_os = "linux")]
            for fd in ["fd1", "fd2"] {
                let prefix = format!("{fd}=");
                let seen: Vec<&str> = lines
                    .iter()
                    .copied()
                    .filter(|l| l.starts_with(&prefix))
                    .collect();
                assert_eq!(
                    seen.len(),
                    args.len(),
                    "one {fd} line per run, got {text:?}"
                );
                assert!(
                    seen.iter().all(|l| *l == format!("{fd}=/dev/null")),
                    "every probe child's {fd} must be the null device, got {text:?}"
                );
            }
            #[cfg(windows)]
            for handle in lines.iter().filter(|l| l.starts_with("type=")) {
                assert_eq!(
                    *handle, "type=2 console=False",
                    "the probe child's stdout/stderr must be the NUL device, got {text:?}"
                );
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = &report;

        assert!(
            logs.contains(SSM_EU) && logs.contains(ECS_EU),
            "anti-vacuity: the capture must hold the regional tokens' own lines, got {logs:?}"
        );
        assert!(
            logs.contains("reason=canary_shape"),
            "anti-vacuity: the capture must hold a refusal line, got {logs:?}"
        );
        assert!(!logs.contains(CANARY), "the canary reached a log: {logs}");
        for (surface, text) in [
            ("log", logs.clone()),
            ("heartbeat payload", wire.clone()),
            ("state Debug", format!("{state:?}")),
            ("probe env Debug", format!("{env:?}")),
        ] {
            assert!(
                !text.contains(CRED_FAKE_SECRET) && !text.contains("FAKE-SECRET"),
                "the secret reached the {surface}: {text}"
            );
        }
    }

    /// One whole refresh cycle against a local cache, as the refresh thread runs
    /// it, returning the payload's capabilities.
    fn cycle(
        cache: &Mutex<CredFamilyState>,
        env: &CredProbeEnv,
        fetched: Result<Vec<serde_json::Value>, SpecFetchFailure>,
        at: Instant,
    ) -> Vec<String> {
        run_cred_refresh_cycle(cache, env, || fetched, at, CRED_TEST_TTL);
        let g = cache.lock().unwrap();
        cred_payload_capabilities(&g).0
    }

    /// A spec REMOVED from a successful fetch retracts at once, and a successful
    /// EMPTY catalogue is exactly that, for every token.
    #[test]
    fn a_removed_spec_retracts_immediately() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_cred_shim(&bin, "aws", Shim::Exit(0)));
        let cache = Mutex::new(CredFamilyState::new());
        let t0 = Instant::now();
        let both = || vec![ssm_spec("eu-central-1"), ecs_spec("eu-central-1")];

        assert_eq!(
            cycle(&cache, &env, Ok(both()), t0),
            vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU, SSM_EU]
        );
        // Removed well inside one TTL's hold: no hold applies to an answer.
        assert_eq!(
            cycle(
                &cache,
                &env,
                Ok(vec![ssm_spec("eu-central-1")]),
                t0 + Duration::from_secs(1)
            ),
            vec!["os:linux", "probes:cred", "cred:aws-sts", SSM_EU],
            "the removed ecs token must retract on this payload"
        );
        assert_eq!(
            cycle(&cache, &env, Ok(Vec::new()), t0 + Duration::from_secs(2)),
            vec!["os:linux", "probes:cred", "cred:aws-sts"],
            "an answered empty catalogue retracts everything regional, nothing else"
        );

        // A row that stops validating is a removal too: no such spec exists now.
        cycle(&cache, &env, Ok(both()), t0 + Duration::from_secs(3));
        let mut corrupted = ssm_spec("eu-central-1");
        corrupted["token"] = json!("cred:aws-ssm:us-east-1");
        assert_eq!(
            cycle(
                &cache,
                &env,
                Ok(vec![corrupted, ecs_spec("eu-central-1")]),
                t0 + Duration::from_secs(4)
            ),
            vec!["os:linux", "probes:cred", "cred:aws-sts", ECS_EU]
        );
    }

    /// A failed fetch holds an earned token for one TTL and then drops it, for
    /// every failure class, and never touches the built-ins or the marker.
    #[test]
    fn a_failed_fetch_holds_for_one_ttl_then_drops_and_spares_the_builtins() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_cred_shim(&bin, "aws", Shim::Exit(0)));
        let failures = [
            SpecFetchFailure::Status(404),
            SpecFetchFailure::Status(500),
            SpecFetchFailure::Status(401),
            SpecFetchFailure::Network,
            SpecFetchFailure::Malformed,
            SpecFetchFailure::Oversized,
            SpecFetchFailure::NoBinding,
            SpecFetchFailure::NoCoord,
            SpecFetchFailure::Client,
        ];
        for failure in failures {
            let cache = Mutex::new(CredFamilyState::new());
            let t0 = Instant::now();
            let earned = vec!["os:linux", "probes:cred", "cred:aws-sts", SSM_EU];
            let bare = vec!["os:linux", "probes:cred", "cred:aws-sts"];

            assert_eq!(
                cycle(&cache, &env, Ok(vec![ssm_spec("eu-central-1")]), t0),
                earned
            );
            let t1 = t0 + CRED_TEST_TTL;
            assert_eq!(
                cycle(&cache, &env, Err(failure), t1),
                earned,
                "{failure:?} holds"
            );
            assert_eq!(
                cycle(&cache, &env, Err(failure), t1 + CRED_TEST_TTL / 2),
                earned,
                "{failure:?} still holds inside the TTL"
            );
            assert_eq!(
                cycle(&cache, &env, Err(failure), t1 + CRED_TEST_TTL),
                bare,
                "{failure:?} drops after one whole TTL, and the built-ins stay"
            );

            // Recovery earns it back at once.
            assert_eq!(
                cycle(
                    &cache,
                    &env,
                    Ok(vec![ssm_spec("eu-central-1")]),
                    t1 + CRED_TEST_TTL * 2
                ),
                earned,
                "{failure:?} recovers"
            );
        }

        // A fetch that has never answered changes nothing and earns nothing.
        let cache = Mutex::new(CredFamilyState::new());
        assert_eq!(
            cycle(
                &cache,
                &env,
                Err(SpecFetchFailure::Status(404)),
                Instant::now()
            ),
            vec!["os:linux", "probes:cred", "cred:aws-sts"]
        );
    }

    /// D2 per token: an Absent retracts on the next payload, an Unknown (the SSM
    /// canary's 254) holds for one TTL and then drops, and the built-in keeps its
    /// own answer through both.
    #[test]
    fn regional_verdicts_follow_the_family_fold() {
        let (tmp, bin) = tmp_with_bin();
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_cred_shim(&bin, "aws-ok", Shim::Exit(0)));
        let specs = || vec![ssm_spec("eu-central-1")];
        let cache = Mutex::new(CredFamilyState::new());
        let t0 = Instant::now();
        assert_eq!(
            cycle(&cache, &env, Ok(specs()), t0),
            vec!["os:linux", "probes:cred", "cred:aws-sts", SSM_EU]
        );

        env.aws = Some(write_cred_shim(&bin, "aws-254", Shim::Exit(254)));
        let t1 = t0 + CRED_TEST_TTL;
        assert!(
            cycle(&cache, &env, Ok(specs()), t1).contains(&SSM_EU.to_string()),
            "254 is Unknown: the token holds"
        );
        assert!(
            !cycle(&cache, &env, Ok(specs()), t1 + CRED_TEST_TTL).contains(&SSM_EU.to_string()),
            "254 for one whole TTL drops it"
        );

        env.aws = Some(write_cred_shim(&bin, "aws-ok-2", Shim::Exit(0)));
        let t2 = t1 + CRED_TEST_TTL * 2;
        assert!(cycle(&cache, &env, Ok(specs()), t2).contains(&SSM_EU.to_string()));

        // aws uninstalled: a definitive Absent, retracted on this very payload.
        env.aws = None;
        assert_eq!(
            cycle(&cache, &env, Ok(specs()), t2 + Duration::from_secs(1)),
            vec!["os:linux", "probes:cred"]
        );
    }

    /// At most 16 specs run, the same 16 however coord orders them, and a valid
    /// spec past the cap is not retracted but folds as Unknown.
    #[test]
    fn at_most_sixteen_specs_run_and_the_choice_is_order_independent() {
        let (tmp, bin) = tmp_with_bin();
        let report = tmp.path().join("calls.txt");
        let mut env = shim_env(tmp.path());
        env.aws = Some(write_recording_shim(&bin, "aws", &report, 0));
        let specs: Vec<serde_json::Value> = (1..=20)
            .map(|i| ecs_spec(&format!("eu-central-{i}")))
            .collect();
        assert_eq!(CRED_PROBE_SPEC_RUN_CAP, 16);

        let RegionalCycle::Answered {
            verdicts, not_run, ..
        } = run_regional_cycle(Ok(specs.clone()), &env)
        else {
            panic!("an answered catalogue");
        };
        assert_eq!(verdicts.len(), 16);
        assert_eq!(not_run.len(), 4);
        assert_eq!(recorded_args(&report).len(), 16, "exactly 16 children ran");

        let reversed: Vec<serde_json::Value> = specs.iter().rev().cloned().collect();
        let first = plan_regional_specs(&specs);
        let second = plan_regional_specs(&reversed);
        let tokens = |p: &RegionalPlan| p.run.iter().map(RegionalProbe::token).collect::<Vec<_>>();
        assert_eq!(tokens(&first), tokens(&second));
        assert_eq!(first.not_run, second.not_run);

        // A token that fell past the cap was never removed: it holds one TTL.
        let capped = not_run.first().unwrap().clone();
        let mut state = CredFamilyState::new();
        let t0 = Instant::now();
        state.apply_regional(
            &answered(vec![(capped.clone(), CredState::Present)], Vec::new()),
            t0,
            CRED_TEST_TTL,
        );
        let past_cap = answered(Vec::new(), vec![capped.clone()]);
        state.apply_regional(&past_cap, t0 + CRED_TEST_TTL, CRED_TEST_TTL);
        assert!(state.regional.get(&capped).is_some_and(|m| m.present));
        state.apply_regional(&past_cap, t0 + CRED_TEST_TTL * 2, CRED_TEST_TTL);
        assert!(!state.regional.contains_key(&capped));
    }

    #[test]
    fn a_duplicated_token_runs_once_and_the_winner_is_order_independent() {
        let mut other = ssm_spec("eu-central-1");
        other["canary_parameter"] = json!("/qontinui/fleet/another");
        let specs = [ssm_spec("eu-central-1"), other];
        let reversed = [specs[1].clone(), specs[0].clone()];
        let first = plan_regional_specs(&specs);
        let second = plan_regional_specs(&reversed);
        assert_eq!(first.run.len(), 1);
        assert_eq!(first.not_run, Vec::<String>::new());
        assert_eq!(first.run[0].args(), second.run[0].args());
    }

    /// Serve ONE HTTP request on a loopback port, recording its request line.
    fn serve_once(
        status: &'static str,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<String>) {
        serve_once_with_headers(status, String::new(), body)
    }

    /// [`serve_once`] with extra header lines, each ending `\r\n`.
    fn serve_once_with_headers(
        status: &'static str,
        headers: String,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
            let head = format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            request.lines().next().unwrap_or_default().to_string()
        });
        (format!("http://{addr}"), handle)
    }

    /// The request production builds, minus the bearer: which bearer it presents
    /// is `LocalBindingSet::bearer_scope`'s contract, tested with it.
    fn fetch_at(
        client: &reqwest::blocking::Client,
        base: &str,
    ) -> Result<Vec<serde_json::Value>, SpecFetchFailure> {
        let url = format!("{base}{PROBE_SPECS_PATH}");
        read_specs_response(client.get(&url), Duration::from_secs(60))
    }

    /// The transport seam, over real HTTP: the route and method, the response
    /// the coord worker fixed, and every shape that is "no answer".
    #[test]
    fn the_fetch_reads_the_route_and_only_a_specs_array_is_an_answer() {
        let client = probe_specs_client(Duration::from_secs(60)).unwrap();
        let fixed = br#"{"specs":[{"token":"cred:aws-ssm:eu-central-1","probe_kind":"aws_ssm_get_parameter","region":"eu-central-1","canary_parameter":"/some/name"}]}"#;
        let (base, request) = serve_once("200 OK", fixed.to_vec());
        let specs = fetch_at(&client, &base).expect("the fixed response parses");
        assert_eq!(
            request.join().unwrap(),
            "GET /coord/devices/credential-probes HTTP/1.1"
        );
        let plan = plan_regional_specs(&specs);
        let tokens: Vec<String> = plan.run.iter().map(RegionalProbe::token).collect();
        assert_eq!(
            tokens,
            vec![SSM_EU],
            "the fixed response is a runnable spec"
        );

        let (base, _) = serve_once("200 OK", br#"{"specs":[]}"#.to_vec());
        assert_eq!(
            fetch_at(&client, &base),
            Ok(Vec::new()),
            "empty is an answer"
        );

        for (status, body, want) in [
            (
                "404 Not Found",
                b"{\"specs\":[]}".to_vec(),
                SpecFetchFailure::Status(404),
            ),
            (
                "500 Internal Server Error",
                b"{\"specs\":[]}".to_vec(),
                SpecFetchFailure::Status(500),
            ),
            (
                "401 Unauthorized",
                Vec::new(),
                SpecFetchFailure::Status(401),
            ),
            ("200 OK", Vec::new(), SpecFetchFailure::Malformed),
            (
                "200 OK",
                b"<html>gateway</html>".to_vec(),
                SpecFetchFailure::Malformed,
            ),
            ("200 OK", b"{}".to_vec(), SpecFetchFailure::Malformed),
            (
                "200 OK",
                b"{\"specs\":null}".to_vec(),
                SpecFetchFailure::Malformed,
            ),
            (
                "200 OK",
                b"{\"specs\":{}}".to_vec(),
                SpecFetchFailure::Malformed,
            ),
            ("200 OK", b"[]".to_vec(), SpecFetchFailure::Malformed),
            // An array of arrays satisfies a derived struct positionally.
            ("200 OK", b"[[]]".to_vec(), SpecFetchFailure::Malformed),
            // A repeated key must not let the last, empty copy win.
            (
                "200 OK",
                br#"{"specs":[],"specs":[]}"#.to_vec(),
                SpecFetchFailure::Malformed,
            ),
            (
                "200 OK",
                vec![b' '; (PROBE_SPECS_BODY_CAP + 1) as usize],
                SpecFetchFailure::Oversized,
            ),
        ] {
            let (base, _) = serve_once(status, body);
            assert_eq!(fetch_at(&client, &base), Err(want), "{status} {want:?}");
        }

        // A redirect is not followed. The target would answer a valid empty
        // catalogue, so a client that followed it reads Ok, and retracts everything.
        let (target, _) = serve_once("200 OK", br#"{"specs":[]}"#.to_vec());
        let (base, _) = serve_once_with_headers(
            "302 Found",
            format!("Location: {target}{PROBE_SPECS_PATH}\r\n"),
            Vec::new(),
        );
        assert_eq!(
            fetch_at(&client, &base),
            Err(SpecFetchFailure::Status(302)),
            "the production client must not follow a redirect"
        );

        // A port nothing listens on is a network failure, not an empty catalogue.
        let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = unused.local_addr().unwrap();
        drop(unused);
        assert_eq!(
            fetch_at(&client, &format!("http://{addr}")),
            Err(SpecFetchFailure::Network)
        );
    }

    /// A body with no declared length past the cap is refused by the read loop,
    /// and a read past the deadline is a network failure.
    #[test]
    fn the_body_read_is_capped_and_bounded_by_its_deadline() {
        let mut far = Instant::now() + Duration::from_secs(600);
        let mut huge = std::io::repeat(b' ');
        assert_eq!(
            read_specs_body(&mut huge, far),
            Err(SpecFetchFailure::Oversized)
        );
        far = Instant::now();
        let mut huge = std::io::repeat(b' ');
        assert_eq!(
            read_specs_body(&mut huge, far),
            Err(SpecFetchFailure::Network)
        );
    }

    /// OFF THE HEARTBEAT'S CRITICAL PATH: the heartbeat's read of the family
    /// reaches the fetch only through the detached refresh, which is what
    /// `cred_capabilities` starts and never waits on.
    #[test]
    fn the_fetch_is_reached_only_from_the_detached_refresh() {
        const MARKER: &str = "\n#[cfg(test)]\nmod tests {";
        let src = include_str!("../fleet.rs").replace("\r\n", "\n");
        let (prod, _) = src.split_once(MARKER).expect("fleet.rs test module header");
        let body_of = |signature: &str| -> String {
            let (_, from_fn) = prod
                .split_once(signature)
                .unwrap_or_else(|| panic!("{signature} is defined in fleet.rs"));
            from_fn
                .split_once("\n}\n")
                .unwrap_or_else(|| panic!("{signature} ends"))
                .0
                .to_string()
        };
        for sync_path in [
            "fn cred_capabilities(",
            "fn heartbeat_capabilities(",
            "pub async fn heartbeat_to_coord(",
        ] {
            let body = body_of(sync_path);
            assert!(
                !body.contains("fetch_probe_specs") && !body.contains("run_regional_cycle"),
                "{sync_path} must not reach the catalogue fetch or the regional probes"
            );
        }
        let refresh = body_of("fn spawn_cred_refresh(");
        assert_eq!(
            refresh
                .matches("cred_probe_specs::fetch_probe_specs")
                .count(),
            1,
            "the refresh thread is the one production caller of the fetch"
        );
        assert_eq!(
            prod.matches("cred_probe_specs::fetch_probe_specs").count(),
            1,
            "nothing else in fleet.rs may call the fetch"
        );
    }
}

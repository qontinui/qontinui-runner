//! The `.mcp.json` coord-mcp **proxy-header contract**: which header names
//! carry the per-session loopback nonce, and the single pair of resolvers that
//! read it back out of a request or a config document.
//!
//! ## Why this is its own module
//!
//! Phase 2 of plan `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`
//! moved the nonce into `Authorization: Bearer <nonce>` (keeping the legacy
//! custom header). The dangerous half of that change was not the emitter — it
//! was the **readers**, five of which re-derived the header name from a
//! hardcoded literal and every one of which degraded *silently* on the new
//! shape (a `None`, a `continue`, an `unwrap_or("")`). One of those readers,
//! `coord_doctor`, is compiled into the **library** crate, while `coord_mcp`
//! is declared in `main.rs` only — so a resolver living in `coord_mcp` is
//! unreachable from it. This module is the shared home that makes "every
//! reader goes through one function" actually expressible.
//!
//! Declared in BOTH `lib.rs` and `main.rs`, like the other dual-rooted modules
//! (`auth`, `fs_perms`, `secure_storage`).

/// Header carrying the per-session loopback nonce that authenticates a
/// session's MCP client to the runner-local `/coord-mcp` proxy route.
/// Lowercase — HTTP header names are case-insensitive and axum's `HeaderMap`
/// keys are lowercased; the `.mcp.json` writer emits the canonical-case form.
pub const COORD_MCP_PROXY_KEY_HEADER: &str = "x-coord-mcp-proxy-key";

/// Canonical-case spelling of [`COORD_MCP_PROXY_KEY_HEADER`] as it appears as a
/// JSON key inside a `.mcp.json` `headers` object. HTTP lookups use the
/// lowercase constant (axum lowercases `HeaderMap` keys); JSON objects are
/// case-SENSITIVE, so the writer and every config reader need this spelling.
/// Pinned equal (modulo case) to the lowercase constant by a unit test.
pub const COORD_MCP_PROXY_KEY_HEADER_JSON: &str = "X-Coord-Mcp-Proxy-Key";

/// The standard `Authorization` header, as a JSON key in a `.mcp.json`
/// `headers` object.
///
/// **Why the proxy nonce now also travels here (plan
/// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`, Phase 2).** A
/// stale nonce 401s. Measured at client 2.1.236/2.1.237: an MCP client that
/// sees a 401 from an `http`-transport server whose *static* `headers` map has
/// no `Authorization` key attaches an OAuth provider (`hasAuthProvider: true`),
/// runs RFC 9728 → RFC 8414 discovery, finds nothing, and falls back to
/// Dynamic Client Registration at `<origin>/register` — which this runner 404s.
/// That failed DCR then writes a durable `mcpOAuth` entry into the client's
/// `.credentials.json`, after which the client sends the (now healthy) server
/// **zero** requests forever: `Skipping connection (cached needs-auth)`.
///
/// With a static `Authorization` present the client reports the connection as
/// failed and **never constructs an auth provider** (`hasAuthProvider: false`),
/// so no code path can mint a poison entry. The cache key is
/// `<serverName>|sha256({type,url,headers}).slice(0,16)` and the nonce lives
/// inside that hashed `headers` map, so before this change **every rotation
/// minted a new poison entry** — an unbounded accumulator (17 live
/// `coord-mcp` entries were measured on this box). Emitting `Authorization`
/// closes that class structurally.
pub const PROXY_AUTHORIZATION_HEADER_JSON: &str = "Authorization";

/// `Authorization` scheme prefix the proxy nonce travels under.
pub const PROXY_BEARER_PREFIX: &str = "Bearer ";

/// True iff `s` is JWT-shaped: three `.`-separated, non-empty segments.
///
/// The discriminator that keeps "accept the nonce from `Authorization`" from
/// swallowing the OTHER thing that legitimately lives in that header — a real
/// static bearer. A proxy nonce is two v4 UUID simple forms (64 hex chars, no
/// `.`), so the two shapes can never be confused. Every reader below uses this
/// so that:
///   * a static-bearer agent `.mcp.json` keeps classifying as a NON-proxy
///     shape (`read_proxy_nonce` → `None`, `coord doctor` → not-a-proxy), and
///   * a request that presents a genuine JWT in `Authorization` alongside a
///     proxy key in the custom header keeps authenticating off the custom key.
pub fn looks_like_jwt(s: &str) -> bool {
    let mut parts = s.split('.');
    let (a, b, c, extra) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next().is_some(),
    );
    !extra
        && matches!((a, b, c), (Some(a), Some(b), Some(c))
            if !a.is_empty() && !b.is_empty() && !c.is_empty())
}

/// Pull a proxy nonce out of an `Authorization` header VALUE, or `None` when
/// the value is not a nonce-shaped bearer (wrong scheme, empty, or a real JWT
/// — see [`looks_like_jwt`]).
pub fn proxy_nonce_from_authorization(value: &str) -> Option<&str> {
    let tok = value
        .strip_prefix(PROXY_BEARER_PREFIX)
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if tok.is_empty() || looks_like_jwt(tok) {
        return None;
    }
    Some(tok)
}

/// **THE request-side proxy-key resolver.** Every loopback proxy door
/// (`/coord-mcp`, the claims reads, the coord write forwarder, the VCS PR
/// route) resolves its nonce through here so the two accepted shapes can never
/// drift apart door-to-door.
///
/// `Authorization: Bearer <nonce>` is preferred; `X-Coord-Mcp-Proxy-Key` is the
/// legacy shape and stays accepted indefinitely, because `.mcp.json` files are
/// rewritten only on session spawn — never periodically — so configs written
/// before Phase 2 keep validating for as long as their sessions live.
pub fn proxy_nonce_from_request(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(n) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(proxy_nonce_from_authorization)
    {
        return Some(n.to_owned());
    }
    headers
        .get(COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// **THE config-side proxy-key resolver**, over a `.mcp.json` server entry's
/// `headers` OBJECT. Accepts both shapes (case-insensitively — JSON keys are
/// case-sensitive but hand-edited configs are not reliably canonical),
/// preferring `Authorization`.
///
/// A JWT in `Authorization` deliberately resolves to `None`: that is the
/// static-bearer (agent) shape, which the reconcile/self-heal path must never
/// treat as a proxy config.
pub fn proxy_nonce_from_header_object(headers: &serde_json::Value) -> Option<String> {
    let obj = headers.as_object()?;
    let get = |name: &str| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_str())
    };
    if let Some(n) = get(PROXY_AUTHORIZATION_HEADER_JSON).and_then(proxy_nonce_from_authorization) {
        return Some(n.to_owned());
    }
    get(COORD_MCP_PROXY_KEY_HEADER_JSON)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// PREFIX of the per-process environment variable a terminal's session reads
/// its OWN coord-mcp proxy nonce from (plan
/// `2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages`,
/// Phase 2). The full name is workdir-keyed —
/// `QONTINUI_COORD_MCP_NONCE_<K>`, see [`terminal_nonce_env_name`].
///
/// ## Why an environment reference, and why it ALWAYS carries a default
///
/// `<workdir>/.mcp.json` is ONE file read by every session launched in that
/// cwd, so the nonce written into it cannot name a terminal — and the runner's
/// deterministic caller self-id leg (`nonce → terminal_id → lifecycle record →
/// claude_session_id`) needs one. The in-cwd DEVICE document therefore spells
/// its credential as `${QONTINUI_COORD_MCP_NONCE_<K>:-<workdir nonce>}`, so a
/// terminal the runner spawned — which exports a terminal-bound nonce under
/// that name into its PTY — presents THAT, while every other session in the cwd
/// (a hand-launched `claude`, a peer's shell) falls back to the workdir nonce
/// exactly as before.
///
/// **The client behaviour this rests on was measured, not assumed:** measured
/// 2026-09-26 on Claude Code 2.1.283 (plan
/// `2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages`
/// Phase 1): `${VAR:-d}` expands in http headers and stdio args via both
/// `--mcp-config` and a project `.mcp.json`; an unset `${VAR}` with no default
/// is sent literally.
///
/// The default is therefore load-bearing, not decorative: with the variable
/// unset and NO default the client sends the literal text `${VAR}` rather than
/// failing, which would 401 every session the runner did not spawn. Every
/// reference the runner writes carries `:-<default>`.
///
/// ## Why the name is workdir-keyed
///
/// Environment is inherited. A session that `cd`s into ANOTHER worktree (a
/// different tenant's, or one served by another runner's port) and launches
/// `claude` there would otherwise present its spawn terminal's key to a
/// document that names a different workdir — a tenant crossing, or a 401 on a
/// foreign port. With `<K>` derived from the workdir the reference names, that
/// other document references a variable this terminal never set, and falls to
/// its own default.
pub const QONTINUI_COORD_MCP_NONCE_ENV: &str = "QONTINUI_COORD_MCP_NONCE";

/// The stdio twin PREFIX of [`QONTINUI_COORD_MCP_NONCE_ENV`]: the per-process
/// path of the runner-owned credential file the fleet shim should read,
/// referenced from the in-cwd stdio document's `--credential` argument as
/// `${QONTINUI_COORD_MCP_CREDENTIAL_<K>:-<workdir credential file>}`.
pub const QONTINUI_COORD_MCP_CREDENTIAL_ENV: &str = "QONTINUI_COORD_MCP_CREDENTIAL";

/// Hex digits of the workdir key in a terminal env name.
const TERMINAL_ENV_KEY_LEN: usize = 16;

/// The workdir as hashed into credential file names AND terminal env names.
/// Spelling-insensitive: separators unified, trailing separators dropped (a
/// bare root keeps its one), and case-folded on the case-insensitive
/// filesystem — so the writer (called with `primary_wt`) and the identity seam
/// (called with the terminal's `cwd`) derive the same key for one directory.
pub fn credential_workdir_key(workdir: &str) -> String {
    let mut k = workdir.trim().replace('\\', "/");
    while k.len() > 1 && k.ends_with('/') && !k.ends_with(":/") {
        k.pop();
    }
    if cfg!(windows) {
        k = k.to_lowercase();
    }
    k
}

/// `<K>`: the first 16 hex digits, UPPERCASE, of
/// `sha256(credential_workdir_key(workdir))`. Uppercase so the full name is a
/// conventional shell identifier.
fn terminal_env_key(workdir: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = hex::encode_upper(Sha256::digest(credential_workdir_key(workdir).as_bytes()));
    digest.chars().take(TERMINAL_ENV_KEY_LEN).collect()
}

/// The workdir-keyed nonce variable name, `QONTINUI_COORD_MCP_NONCE_<K>` —
/// THE one derivation both the in-cwd writer and the identity seam use.
pub fn terminal_nonce_env_name(workdir: &str) -> String {
    format!(
        "{QONTINUI_COORD_MCP_NONCE_ENV}_{}",
        terminal_env_key(workdir)
    )
}

/// The workdir-keyed credential-file variable name,
/// `QONTINUI_COORD_MCP_CREDENTIAL_<K>`.
pub fn terminal_credential_env_name(workdir: &str) -> String {
    format!(
        "{QONTINUI_COORD_MCP_CREDENTIAL_ENV}_{}",
        terminal_env_key(workdir)
    )
}

/// True iff `name` is a terminal key variable of EITHER kind for ANY workdir:
/// one of the two prefixes, `_`, then exactly 16 uppercase hex digits. The
/// shape every strip (the runner's own env at boot, the PTY and headless child
/// seams, the shim's nested-`claude` pass-through) matches on.
pub fn is_terminal_key_env_name(name: &str) -> bool {
    [
        QONTINUI_COORD_MCP_NONCE_ENV,
        QONTINUI_COORD_MCP_CREDENTIAL_ENV,
    ]
    .iter()
    .any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|k| {
                k.len() == TERMINAL_ENV_KEY_LEN
                    && k.bytes()
                        .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
            })
    })
}

/// Every terminal key variable name among `names` — for a caller that strips
/// them from an environment it enumerates.
pub fn terminal_key_env_names<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter(|n| is_terminal_key_env_name(n.as_ref()))
        .map(|n| n.as_ref().to_owned())
        .collect()
}

/// The result of resolving every `${NAME:-default}` reference in a string to
/// its DEFAULT arm — what the MCP client sends when `NAME` is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRefExpansion<'a> {
    /// The string with every defaulted reference replaced by its default. A
    /// reference with no default (`${NAME}`) is kept VERBATIM — that is what the
    /// client sends with the variable unset — and named in `unresolved`.
    pub value: std::borrow::Cow<'a, str>,
    /// Names of the `${NAME}` references that carry no default.
    pub unresolved: Vec<String>,
}

/// One parsed `${...}` reference: its byte span, name and optional default.
struct EnvRef<'a> {
    start: usize,
    end: usize,
    name: &'a str,
    default: Option<&'a str>,
}

/// Every well-formed `${NAME}` / `${NAME:-default}` reference in `s`, in order.
///
/// Grammar, matching the client's expansion: `NAME` is a non-empty run of
/// ASCII alphanumerics and `_`; the default runs to the first `}` and may be
/// empty. **Limit:** a default that itself contains `}` ends early at that
/// brace — the runner never writes one (a hex nonce, an absolute file path under
/// `~/.qontinui`), so this is a stated boundary rather than a live case. Anything else — an unterminated `${`, an empty or invalid name, a
/// `:` not followed by `-` — is not a reference and stays literal text.
fn parse_env_refs(s: &str) -> Vec<EnvRef<'_>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = s.get(from..).and_then(|rest| rest.find("${")) {
        let start = from + rel;
        let body_start = start + 2;
        let Some(close_rel) = s.get(body_start..).and_then(|rest| rest.find('}')) else {
            break;
        };
        let end = body_start + close_rel + 1;
        let body = s.get(body_start..end - 1).unwrap_or("");
        let (name, default) = match body.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (body, None),
        };
        let valid =
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if valid {
            out.push(EnvRef {
                start,
                end,
                name,
                default,
            });
            from = end;
        } else {
            from = body_start;
        }
    }
    out
}

/// Resolve every `${NAME:-default}` in `s` to its default arm, reporting any
/// `${NAME}` that has none. **Never reads the environment** — the runner has no
/// terminal's variable, and every runner reader reasons about the WORKDIR key.
pub fn expand_env_ref_defaults(s: &str) -> EnvRefExpansion<'_> {
    let refs = parse_env_refs(s);
    if refs.is_empty() {
        return EnvRefExpansion {
            value: std::borrow::Cow::Borrowed(s),
            unresolved: Vec::new(),
        };
    }
    let mut value = String::with_capacity(s.len());
    let mut unresolved = Vec::new();
    let mut at = 0;
    for r in &refs {
        value.push_str(s.get(at..r.start).unwrap_or(""));
        match r.default {
            Some(d) => value.push_str(d),
            None => {
                value.push_str(s.get(r.start..r.end).unwrap_or(""));
                unresolved.push(r.name.to_owned());
            }
        }
        at = r.end;
    }
    value.push_str(s.get(at..).unwrap_or(""));
    EnvRefExpansion {
        value: std::borrow::Cow::Owned(value),
        unresolved,
    }
}

/// `s` as the client would send it with every referenced variable UNSET: the
/// default arm of each `${NAME:-default}`, and the literal text of a `${NAME}`
/// that has none.
pub fn env_ref_client_default(s: &str) -> std::borrow::Cow<'_, str> {
    expand_env_ref_defaults(s).value
}

/// The names of every well-formed reference in `s` (with or without default).
pub fn env_ref_names(s: &str) -> Vec<&str> {
    parse_env_refs(s).into_iter().map(|r| r.name).collect()
}

/// True iff the RAW `coord-mcp` entry (not the effective one — the credential
/// file stays literal) references a terminal key variable
/// ([`is_terminal_key_env_name`]) for ANY workdir, in a `headers` value or in
/// `args`.
///
/// That is the runner's own in-cwd DEVICE document and nothing else: the agent
/// writer, the per-terminal `--mcp-config` and the `provision-session` route all
/// emit literal credentials, and a hand-written or foreign document does not
/// name these variables.
pub fn config_doc_references_terminal_env(doc: &serde_json::Value) -> bool {
    config_doc_references_terminal_env_matching(doc, is_terminal_key_env_name)
}

/// [`config_doc_references_terminal_env`] narrowed to THIS workdir's two names
/// ([`terminal_nonce_env_name`], [`terminal_credential_env_name`]). The identity
/// seam keys its terminal-bound mint on this: exporting `workdir`'s names into a
/// PTY only helps a document that references those exact names, so a document
/// copied in from another workdir (a different `<K>`) does not qualify.
pub fn config_doc_references_terminal_env_for(doc: &serde_json::Value, workdir: &str) -> bool {
    let (nonce, credential) = (
        terminal_nonce_env_name(workdir),
        terminal_credential_env_name(workdir),
    );
    config_doc_references_terminal_env_matching(doc, |n| n == nonce || n == credential)
}

fn config_doc_references_terminal_env_matching(
    doc: &serde_json::Value,
    matches: impl Fn(&str) -> bool,
) -> bool {
    let Some(entry) = coord_mcp_entry(doc) else {
        return false;
    };
    let names_ours = |s: &str| env_ref_names(s).into_iter().any(&matches);
    let in_headers = entry
        .get("headers")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|h| {
            h.values()
                .filter_map(serde_json::Value::as_str)
                .any(names_ours)
        });
    let in_args = entry
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .any(names_ours)
        });
    in_headers || in_args
}

/// Basename of the fleet's stdio MCP shim
/// (`<qontinui-root>/qontinui-claude-config/scripts/coord-mcp-shim.py`) — the
/// `args[0]` of a stdio-shaped `coord-mcp` entry. Plan
/// `2026-09-05-coord-mcp-transport-death-must-fall-through-not-be-reported`,
/// Phase 3.
pub const COORD_MCP_STDIO_SHIM_FILE: &str = "coord-mcp-shim.py";

/// The shim's ONE argv flag: the absolute path of the runner-owned credential
/// file it re-reads on every call. Nothing else ever travels on its argv — no
/// URL, no nonce, no door (plan Design decision 4).
pub const COORD_MCP_STDIO_SHIM_CREDENTIAL_FLAG: &str = "--credential";

/// The `mcpServers.coord-mcp` entry, whatever its transport.
fn coord_mcp_entry(doc: &serde_json::Value) -> Option<&serde_json::Value> {
    doc.pointer("/mcpServers/coord-mcp")
}

/// True iff the `coord-mcp` entry is the STDIO shape whose `args[0]` is the
/// fleet shim ([`COORD_MCP_STDIO_SHIM_FILE`]). Reads no value that could be a
/// credential — the stdio document carries none; its credential lives in the
/// file named after [`COORD_MCP_STDIO_SHIM_CREDENTIAL_FLAG`].
pub fn config_doc_is_stdio_shim(doc: &serde_json::Value) -> bool {
    let Some(entry) = coord_mcp_entry(doc) else {
        return false;
    };
    if entry.get("type").and_then(serde_json::Value::as_str) != Some("stdio") {
        return false;
    }
    entry
        .get("args")
        .and_then(serde_json::Value::as_array)
        .and_then(|a| a.first())
        .and_then(serde_json::Value::as_str)
        .and_then(|first| std::path::Path::new(first).file_name())
        .map(|name| name == COORD_MCP_STDIO_SHIM_FILE)
        .unwrap_or(false)
}

/// The credential-file path a stdio-shaped entry hands the shim (the value
/// after [`COORD_MCP_STDIO_SHIM_CREDENTIAL_FLAG`] in `args`), or `None` for
/// any other shape.
pub fn stdio_shim_credential_path(doc: &serde_json::Value) -> Option<std::path::PathBuf> {
    if !config_doc_is_stdio_shim(doc) {
        return None;
    }
    let args = coord_mcp_entry(doc)?.get("args")?.as_array()?;
    let mut it = args.iter().filter_map(serde_json::Value::as_str);
    while let Some(arg) = it.next() {
        if arg == COORD_MCP_STDIO_SHIM_CREDENTIAL_FLAG {
            // The in-cwd device document spells this argument as
            // `${QONTINUI_COORD_MCP_CREDENTIAL_<K>:-<workdir file>}`; the runner
            // reads the WORKDIR file (the default arm), never its own env.
            return it
                .next()
                .map(|raw| std::path::PathBuf::from(env_ref_client_default(raw).as_ref()));
        }
    }
    None
}

/// **THE object every config reader looks at**: the `{url, headers}` pair the
/// proxy contract is about, resolved through whichever transport the document
/// spells.
///
/// * `type: "http"` — the entry itself (`url` and `headers` are inline).
/// * `type: "stdio"` (the fleet shim) — the runner-owned credential file the
///   entry names, which is a flat `{url, headers}` object written by the same
///   builder that wrote the document. An absent or unparseable credential file
///   resolves to `None`, exactly as an absent `.mcp.json` does: the readers
///   built on this (`read_proxy_nonce`, `read_proxy_port`, the header-shape
///   and principal-marker probes) then answer "no proxy config here", which is
///   the fail-closed arm each of them already had.
///
/// Without this indirection every reader that keyed on
/// `/mcpServers/coord-mcp/headers` would have gone blind on the stdio shape —
/// the boot reconcile would classify a healthy stdio config as non-proxy, the
/// in-cwd nonce reuse would mint on every spawn, the rotation log would carry
/// an empty `key_prefix`, and the agent-principal write refusal would stop
/// seeing the marker. One resolver keeps one contract.
///
/// **Environment references resolve to their DEFAULT arm here** (plan
/// `2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages`).
/// The in-cwd device document spells its credential as
/// `${QONTINUI_COORD_MCP_NONCE_<K>:-<workdir nonce>}` (http `headers` values) or
/// `${QONTINUI_COORD_MCP_CREDENTIAL_<K>:-<workdir file>}` (the stdio `--credential`
/// argument). Every runner reader reasons about the WORKDIR key, so both are
/// read as their defaults — the runner process has no terminal's variable and
/// this function never consults its own environment. A `${NAME}` with no default
/// is kept verbatim, which is exactly what the client would send.
pub fn effective_coord_mcp_entry(
    doc: &serde_json::Value,
) -> Option<std::borrow::Cow<'_, serde_json::Value>> {
    let entry = coord_mcp_entry(doc)?;
    match stdio_shim_credential_path(doc) {
        None => Some(resolve_header_env_defaults(std::borrow::Cow::Borrowed(
            entry,
        ))),
        Some(path) => {
            let raw = std::fs::read_to_string(path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            v.is_object()
                .then(|| resolve_header_env_defaults(std::borrow::Cow::Owned(v)))
        }
    }
}

/// Any MCP server entry (not only `coord-mcp`) with its `headers` values
/// resolved to their default arm — for a reader that walks every server, such as
/// the `qontinui_cli` walk-up. The `coord-mcp` entry itself is read through
/// [`effective_coord_mcp_entry`], which also follows the stdio indirection.
pub fn entry_with_env_defaults(
    entry: &serde_json::Value,
) -> std::borrow::Cow<'_, serde_json::Value> {
    resolve_header_env_defaults(std::borrow::Cow::Borrowed(entry))
}

/// Rewrite every `headers` VALUE of an entry to its default arm
/// ([`env_ref_client_default`]). Borrowed through untouched when no value
/// carries a reference, so the common literal document costs no clone.
fn resolve_header_env_defaults(
    entry: std::borrow::Cow<'_, serde_json::Value>,
) -> std::borrow::Cow<'_, serde_json::Value> {
    let has_ref = entry
        .get("headers")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|h| {
            h.values()
                .filter_map(serde_json::Value::as_str)
                .any(|v| !env_ref_names(v).is_empty())
        });
    if !has_ref {
        return entry;
    }
    let mut owned = entry.into_owned();
    if let Some(h) = owned
        .get_mut("headers")
        .and_then(serde_json::Value::as_object_mut)
    {
        for v in h.values_mut() {
            if let Some(s) = v.as_str() {
                let resolved = env_ref_client_default(s).into_owned();
                *v = serde_json::Value::String(resolved);
            }
        }
    }
    std::borrow::Cow::Owned(owned)
}

/// The `url` the effective entry addresses — inline for http, from the
/// credential file for stdio.
pub fn effective_coord_mcp_url(doc: &serde_json::Value) -> Option<String> {
    effective_coord_mcp_entry(doc)?
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// [`proxy_nonce_from_header_object`] over a whole `.mcp.json` document, through
/// [`effective_coord_mcp_entry`] so the stdio shape reads back too.
pub fn proxy_nonce_from_config_doc(doc: &serde_json::Value) -> Option<String> {
    let entry = effective_coord_mcp_entry(doc)?;
    proxy_nonce_from_header_object(entry.get("headers")?)
}

/// True iff the `coord-mcp` entry's `headers` object carries an `Authorization`
/// key at all — whatever its value.
///
/// This is a question about the **static headers map's SHAPE**, not about the
/// credential in it, and that is exactly the distinction the DCR escape turns
/// on: the MCP client's exemption predicate reads whether the static map has an
/// `Authorization` key, and attaches an OAuth provider when it does not. A
/// config that is otherwise perfectly healthy — right port, live registered
/// nonce — but carries only the legacy custom header is therefore still
/// DCR-escalating for the next client launched against it.
///
/// Used by the boot self-heal to tell "healthy AND non-escalating" (leave it)
/// from "healthy but still legacy-shaped" (rewrite in place, same nonce). See
/// `coord_mcp::RootReconcileAction::UpgradeHeaders`.
pub fn config_doc_has_static_authorization(doc: &serde_json::Value) -> bool {
    effective_coord_mcp_entry(doc)
        .and_then(|entry| {
            entry.get("headers").and_then(|h| h.as_object()).map(|o| {
                o.keys()
                    .any(|k| k.eq_ignore_ascii_case(PROXY_AUTHORIZATION_HEADER_JSON))
            })
        })
        .unwrap_or(false)
}

/// JSON key of the **principal-class marker** stamped into the `coord-mcp`
/// `headers` object by the AGENT-path `.mcp.json` writer
/// (`coord_mcp::write_coord_mcp_agent_proxy_config`) — and by nothing else.
///
/// ## Why a marker exists at all
///
/// Three emitters produce a **byte-identical** proxy `.mcp.json` (they all
/// funnel through `coord_mcp::coord_mcp_proxy_config_json`), and their nonces
/// are three different security classes. (Under the stdio arm of that builder
/// — plan 2026-09-05 transport-death, Phase 3 — the `headers` object, marker
/// included, lives in the runner-owned credential file the document names
/// rather than inline; every reader here resolves it through
/// [`effective_coord_mcp_entry`], so the marker keeps the same meaning.)
///
/// | emitter | principal | persisted | re-registered after a restart |
/// |---|---|---|---|
/// | `write_coord_mcp_proxy_config` | Device/Persistent | yes | usually |
/// | `write_coord_mcp_agent_proxy_config` | **Agent{id}** | never | **never — by design** |
/// | `provision_session_proxy_config` | Device/**Ephemeral** | never | **never — by design** |
///
/// Rows 2 and 3 are *guaranteed* to be unregistered after a restart, which is
/// exactly the predicate the boot adopt arm keys on
/// (`coord_mcp::ReconcileAction::AdoptNonce`). Adoption hard-codes
/// `principal: Device, lifetime: Persistent` — so without a marker the boot
/// reconcile would re-register an **agent-scoped** credential as a **device**
/// one, and the proxy would then inject the live DEVICE JWT for a nonce whose
/// whole point was to inject one agent's token. The ephemeral case is the same
/// shape: adoption would convert a TTL-bounded, opt-in-gated, never-persisted
/// credential into an unbounded persistent one.
///
/// The principal class is **not inferable** from the boot reconcile's inputs —
/// the file is byte-identical and a lifecycle record carries no principal-class
/// field — so the fix is to remove the unknowability at the SOURCE: the agent
/// writer self-identifies, and the reconcile refuses to touch what it cannot
/// vouch for. A legacy agent config written before this marker existed carries
/// nothing and is therefore still indistinguishable; that residual drains on its
/// own, because an agent config is rewritten at every agent spawn.
///
/// ## Why a header rather than a sibling field on the server object
///
/// The `headers` map is already an arbitrary string→string map that every MCP
/// client forwards verbatim to the server named in `url` — here, the runner's
/// OWN loopback `/coord-mcp` route, which ignores header names it does not
/// know. A new key beside `type`/`url`/`headers` would instead have to survive
/// whatever schema the client validates the server entry against, and a client
/// that rejects unknown keys would take coord-mcp away from every agent
/// session. The header is inert by construction; a sibling field is inert only
/// by assumption.
///
/// It carries no secret (the literal string `agent`), so emitting it costs
/// nothing even in `claude --debug mcp` output, where custom headers are
/// printed in the clear.
pub const COORD_MCP_PRINCIPAL_HEADER_JSON: &str = "X-Coord-Mcp-Principal";

/// The only value [`COORD_MCP_PRINCIPAL_HEADER_JSON`] is ever emitted with. The
/// DEVICE shape omits the header entirely rather than spelling a `device`
/// value — absence must keep meaning exactly what it meant before the marker
/// existed, so that not one already-written device config changes class.
pub const COORD_MCP_PRINCIPAL_AGENT: &str = "agent";

/// True iff the `coord-mcp` entry's `headers` object carries the
/// [`COORD_MCP_PRINCIPAL_HEADER_JSON`] marker naming the AGENT class.
///
/// Case-insensitive on both key and value, matching every other config reader
/// here (JSON keys are case-sensitive, hand-edited configs are not reliably
/// canonical). Absent / unparseable / any other value ⇒ `false`, which is the
/// pre-marker reading: **not marked is not proof of device class**, only proof
/// that this file cannot vouch for itself. Callers that need a safety property
/// must treat `true` as "refuse", never `false` as "permit anything".
pub fn config_doc_is_agent_marked(doc: &serde_json::Value) -> bool {
    effective_coord_mcp_entry(doc)
        .and_then(|entry| {
            entry.get("headers").and_then(|h| h.as_object()).map(|o| {
                o.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case(COORD_MCP_PRINCIPAL_HEADER_JSON)
                        && v.as_str()
                            .map(|s| s.trim().eq_ignore_ascii_case(COORD_MCP_PRINCIPAL_AGENT))
                            .unwrap_or(false)
                })
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    /// A real proxy nonce: two v4 UUID simple forms, 64 hex chars, no `.`.
    fn nonce() -> String {
        format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// A JWT-SHAPED string (three non-empty dot-separated segments). Only the
    /// shape matters to every discriminator in this module.
    const JWT_SHAPED: &str = "eyJhbGciOiJFZERTQSJ9.eyJzdWJfdHlwZSI6ImFnZW50In0.c2ln";

    /// The two spellings of the legacy header are the same name — the HTTP one
    /// is lowercase because axum lowercases `HeaderMap` keys, the JSON one is
    /// canonical-case because JSON object keys are case-SENSITIVE. Pinned so a
    /// later edit to one cannot silently fork them.
    #[test]
    fn json_and_http_spellings_of_the_legacy_header_are_the_same_name() {
        assert_eq!(
            COORD_MCP_PROXY_KEY_HEADER_JSON.to_ascii_lowercase(),
            COORD_MCP_PROXY_KEY_HEADER
        );
        assert_eq!(PROXY_BEARER_PREFIX, "Bearer ");
        assert_eq!(PROXY_AUTHORIZATION_HEADER_JSON, "Authorization");
    }

    /// The nonce-vs-JWT discriminator: the whole "accept the nonce from
    /// `Authorization`" change is safe only because these two shapes cannot be
    /// confused.
    #[test]
    fn looks_like_jwt_separates_a_bearer_token_from_a_proxy_nonce() {
        assert!(looks_like_jwt(JWT_SHAPED));
        assert!(looks_like_jwt("a.b.c"));
        assert!(!looks_like_jwt(&nonce()), "a 64-hex nonce has no dots");
        assert!(!looks_like_jwt("a.b"), "two segments is not a JWT");
        assert!(!looks_like_jwt("a.b.c.d"), "four segments is not a JWT");
        assert!(!looks_like_jwt("a..c"), "an empty segment is not a JWT");
        assert!(!looks_like_jwt(""));
    }

    #[test]
    fn authorization_yields_a_nonce_but_never_a_jwt_or_a_foreign_scheme() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_authorization(&format!("Bearer {n}")),
            Some(n.as_str())
        );
        // Lowercase scheme (some hand-written clients) still resolves.
        assert_eq!(
            proxy_nonce_from_authorization(&format!("bearer {n}")),
            Some(n.as_str())
        );
        // A genuine static bearer is NOT a proxy nonce — this is what keeps the
        // agent-path config classifying as a non-proxy shape.
        assert_eq!(
            proxy_nonce_from_authorization(&format!("Bearer {JWT_SHAPED}")),
            None
        );
        assert_eq!(proxy_nonce_from_authorization("Bearer "), None);
        assert_eq!(proxy_nonce_from_authorization(&format!("Basic {n}")), None);
        assert_eq!(proxy_nonce_from_authorization(&n), None);
    }

    // -- Request side --------------------------------------------------------

    fn req_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// The Phase 2 shape authenticates on its own.
    #[test]
    fn request_resolves_the_nonce_from_authorization() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("authorization", &format!("Bearer {n}"))])),
            Some(n)
        );
    }

    /// The legacy shape keeps authenticating — every `.mcp.json` already on
    /// disk carries only that header, and configs are rewritten on session
    /// spawn, never periodically.
    #[test]
    fn request_resolves_the_nonce_from_the_legacy_custom_header() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("x-coord-mcp-proxy-key", &n)])),
            Some(n)
        );
    }

    /// When both are present and DISAGREE, `Authorization` wins.
    #[test]
    fn request_prefers_authorization_when_both_are_present_and_disagree() {
        let auth = nonce();
        let legacy = nonce();
        assert_ne!(auth, legacy);
        let got = proxy_nonce_from_request(&req_headers(&[
            ("authorization", &format!("Bearer {auth}")),
            ("x-coord-mcp-proxy-key", &legacy),
        ]));
        assert_eq!(got, Some(auth));
    }

    /// ...but a genuine JWT in `Authorization` does NOT shadow a valid custom
    /// header. A caller that legitimately carries a bearer keeps authenticating
    /// off the proxy key rather than 401ing on its own bearer.
    #[test]
    fn request_falls_back_to_the_custom_header_when_authorization_is_a_real_jwt() {
        let legacy = nonce();
        let got = proxy_nonce_from_request(&req_headers(&[
            ("authorization", &format!("Bearer {JWT_SHAPED}")),
            ("x-coord-mcp-proxy-key", &legacy),
        ]));
        assert_eq!(got, Some(legacy));
    }

    #[test]
    fn request_with_no_recognised_header_resolves_none() {
        assert_eq!(proxy_nonce_from_request(&req_headers(&[])), None);
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("authorization", "Bearer ")])),
            None
        );
    }

    // -- Config side ---------------------------------------------------------

    #[test]
    fn config_resolves_both_shapes_and_prefers_authorization() {
        let n = nonce();
        // Phase 2 shape (Authorization only).
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Legacy shape (custom header only) - every pre-Phase-2 file on disk.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Both, disagreeing -> Authorization wins.
        let other = nonce();
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{other}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Header-name matching is case-insensitive - hand-edited configs are
        // not reliably canonical.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"x-coord-mcp-proxy-key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n));
    }

    /// The static-bearer (agent-path) config must keep reading as a NON-proxy
    /// shape. Getting this wrong would make the boot reconcile treat an agent
    /// config as one of ours and rewrite it - and would feed a JWT into the
    /// registry lookup as if it were a nonce.
    #[test]
    fn config_static_bearer_agent_shape_is_not_a_proxy_config() {
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://coord.example.test/mcp","headers":{{"Authorization":"Bearer {JWT_SHAPED}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
    }

    /// The header-SHAPE predicate the boot self-heal's upgrade-in-place arm
    /// keys on. It asks only whether the static map has the key — a legacy-only
    /// config is what leaves the next client DCR-escalating, regardless of how
    /// healthy its nonce is.
    #[test]
    fn static_authorization_presence_is_a_shape_question_not_a_credential_one() {
        let n = nonce();
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(
            !config_doc_has_static_authorization(&doc),
            "a legacy-only config is the DCR-escalating shape"
        );

        // Both shapes present (what the writer emits today).
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));

        // A JWT counts too — the predicate is about the KEY, not the value.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {JWT_SHAPED}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));

        // Case-insensitive, and absent shapes are false rather than a panic.
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"authorization":"x"}}}}"#,
        )
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));
        let doc: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert!(!config_doc_has_static_authorization(&doc));
    }

    /// The agent principal marker: recognised case-insensitively on key AND
    /// value, absent on the device shape, and — the load-bearing part —
    /// invisible to every OTHER reader in this module, so stamping it cannot
    /// change how a config's port, nonce or header shape is classified.
    #[test]
    fn agent_principal_marker_is_recognised_and_inert_to_every_other_reader() {
        let n = nonce();
        let marked: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}","X-Coord-Mcp-Principal":"agent"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_is_agent_marked(&marked));
        // Inert: the nonce and the header SHAPE read exactly as they do without it.
        assert_eq!(proxy_nonce_from_config_doc(&marked), Some(n.clone()));
        assert!(config_doc_has_static_authorization(&marked));

        // The DEVICE shape carries no marker — absence must keep meaning what it
        // meant before the marker existed.
        let device: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(!config_doc_is_agent_marked(&device));

        // Case-insensitive on key and value; whitespace-tolerant on the value.
        let odd: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"x-coord-mcp-principal":" AGENT "}}}}"#,
        )
        .unwrap();
        assert!(config_doc_is_agent_marked(&odd));

        // Any other value, and every absent shape, is NOT a marker.
        let other: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"X-Coord-Mcp-Principal":"device"}}}}"#,
        )
        .unwrap();
        assert!(!config_doc_is_agent_marked(&other));
        let none: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert!(!config_doc_is_agent_marked(&none));
    }

    #[test]
    fn config_absent_or_empty_shapes_resolve_none() {
        let doc: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
        let doc: serde_json::Value =
            serde_json::from_str(r#"{"mcpServers":{"coord-mcp":{"headers":{}}}}"#).unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"X-Coord-Mcp-Proxy-Key":"  "}}}}"#,
        )
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
    }

    // -- Environment references (plan 2026-09-22 one-nonce-per-terminal) ----

    #[test]
    fn env_ref_defaults_resolve_embedded_whole_absent_malformed_and_undefaulted() {
        // Embedded in a bearer value.
        let e = expand_env_ref_defaults("Bearer ${X:-abc}");
        assert_eq!(e.value, "Bearer abc");
        assert!(e.unresolved.is_empty());
        // The whole string.
        assert_eq!(
            env_ref_client_default("${QONTINUI_COORD_MCP_NONCE:-n0nce}"),
            "n0nce"
        );
        // An empty default is still a default.
        assert_eq!(env_ref_client_default("a${X:-}b"), "ab");
        // No reference: borrowed through untouched.
        let e = expand_env_ref_defaults("Bearer plain");
        assert!(matches!(
            e.value,
            std::borrow::Cow::Borrowed("Bearer plain")
        ));
        assert!(e.unresolved.is_empty());
        // Malformed: unterminated, empty name, invalid name, `:` without `-`.
        for m in [
            "${X:-abc",
            "${}",
            "${:-abc}",
            "${A B:-x}",
            "${X:abc}",
            "$X",
            "${",
        ] {
            let e = expand_env_ref_defaults(m);
            assert_eq!(e.value, m, "{m} is not a reference and stays literal");
            assert!(e.unresolved.is_empty(), "{m}");
        }
        // No default: kept verbatim (what the client sends) and REPORTED.
        let e = expand_env_ref_defaults("Bearer ${X}");
        assert_eq!(e.value, "Bearer ${X}");
        assert_eq!(e.unresolved, vec!["X".to_string()]);
        // Mixed: the defaulted one resolves, the bare one is reported.
        let e = expand_env_ref_defaults("${A:-1}-${B}-${C:-3}");
        assert_eq!(e.value, "1-${B}-3");
        assert_eq!(e.unresolved, vec!["B".to_string()]);
        assert_eq!(env_ref_names("${A:-1}${B}x"), vec!["A", "B"]);
    }

    /// The chokepoint never reads the process environment: even for a
    /// variable SET in this process, every reader sees the default arm.
    #[test]
    fn config_readers_resolve_env_refs_to_the_default_and_ignore_the_process_env() {
        let n = nonce();
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer ${{QONTINUI_COORD_MCP_NONCE_0123456789ABCDEF:-{n}}}","X-Coord-Mcp-Proxy-Key":"${{QONTINUI_COORD_MCP_NONCE_0123456789ABCDEF:-{n}}}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_references_terminal_env(&doc));
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));
        assert!(config_doc_has_static_authorization(&doc));
        assert!(!config_doc_is_agent_marked(&doc));
        assert_eq!(
            effective_coord_mcp_url(&doc).as_deref(),
            Some("http://127.0.0.1:9876/coord-mcp")
        );
        // `PATH` is always set in this process; the reader must still answer
        // the default arm, because it never consults the environment.
        let doc_other: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer ${{PATH:-{n}}}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(std::env::var_os("PATH").is_some());
        assert_eq!(proxy_nonce_from_config_doc(&doc_other), Some(n.clone()));
        assert!(
            !config_doc_references_terminal_env(&doc_other),
            "only OUR two variables mark the runner's env-ref document"
        );

        // A literal document does not reference them.
        let literal: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(!config_doc_references_terminal_env(&literal));
    }

    /// The stdio `--credential` argument resolves to its default arm before the
    /// credential file is read, and the file's own headers read back.
    #[test]
    fn stdio_credential_argument_env_ref_resolves_to_the_workdir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cred = tmp.path().join("cred.json");
        let n = nonce();
        std::fs::write(
            &cred,
            format!(
                r#"{{"url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}"}}}}"#
            ),
        )
        .unwrap();
        let arg = format!(
            "${{QONTINUI_COORD_MCP_CREDENTIAL_0123456789ABCDEF:-{}}}",
            cred.display()
        );
        let doc = serde_json::json!({
            "mcpServers": {"coord-mcp": {
                "type": "stdio",
                "command": "/usr/bin/python3",
                "args": ["/x/coord-mcp-shim.py", "--credential", arg],
            }}
        });
        assert!(config_doc_references_terminal_env(&doc));
        assert_eq!(
            stdio_shim_credential_path(&doc).as_deref(),
            Some(cred.as_path())
        );
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n));
        // No default: the literal `${...}` path does not exist, so the entry is
        // unreadable — the fail-closed arm every reader already has.
        let bare = serde_json::json!({
            "mcpServers": {"coord-mcp": {
                "type": "stdio",
                "command": "/usr/bin/python3",
                "args": ["/x/coord-mcp-shim.py", "--credential", "${QONTINUI_COORD_MCP_CREDENTIAL}"],
            }}
        });
        assert!(effective_coord_mcp_entry(&bare).is_none());
    }

    /// The workdir-keyed names: one derivation, spelling-insensitive, distinct
    /// across workdirs, and recognised by the shape matcher every strip uses.
    #[test]
    fn terminal_env_names_are_workdir_keyed_and_spelling_insensitive() {
        let a = terminal_nonce_env_name("/w/repo-a");
        assert!(a.starts_with("QONTINUI_COORD_MCP_NONCE_"), "{a}");
        assert_eq!(a.len(), "QONTINUI_COORD_MCP_NONCE_".len() + 16);
        assert!(is_terminal_key_env_name(&a));
        // Trailing separator and backslash spellings of ONE dir: one name.
        assert_eq!(a, terminal_nonce_env_name("/w/repo-a/"));
        assert_eq!(
            terminal_nonce_env_name("C:\\w\\repo-a"),
            terminal_nonce_env_name("C:/w/repo-a")
        );
        // A different workdir: a different name.
        assert_ne!(a, terminal_nonce_env_name("/w/repo-b"));
        let c = terminal_credential_env_name("/w/repo-a");
        assert!(c.starts_with("QONTINUI_COORD_MCP_CREDENTIAL_"), "{c}");
        assert!(is_terminal_key_env_name(&c));
        assert_eq!(a.rsplit('_').next(), c.rsplit('_').next(), "same <K>");
        // The shape matcher: prefixes alone, lowercase hex, wrong length, and
        // unrelated names are NOT terminal key variables.
        for not in [
            "QONTINUI_COORD_MCP_NONCE",
            "QONTINUI_COORD_MCP_CREDENTIAL",
            "QONTINUI_COORD_MCP_NONCE_0123456789abcdef",
            "QONTINUI_COORD_MCP_NONCE_0123",
            "QONTINUI_COORD_MCP_NONCE_0123456789ABCDEFG",
            "QONTINUI_MCP_CONFIG",
            "PATH",
        ] {
            assert!(!is_terminal_key_env_name(not), "{not}");
        }
        assert_eq!(
            terminal_key_env_names(["PATH", a.as_str(), c.as_str(), "HOME"]),
            vec![a.clone(), c.clone()]
        );

        // Detection: the generic form matches any <K>; the per-workdir form
        // matches only that workdir's names.
        let doc = serde_json::json!({"mcpServers":{"coord-mcp":{"headers":{
            "Authorization": format!("Bearer ${{{a}:-n}}")
        }}}});
        assert!(config_doc_references_terminal_env(&doc));
        assert!(config_doc_references_terminal_env_for(&doc, "/w/repo-a"));
        assert!(!config_doc_references_terminal_env_for(&doc, "/w/repo-b"));
        // The bare prefix (the pre-workdir-key spelling) is not ours.
        let bare = serde_json::json!({"mcpServers":{"coord-mcp":{"headers":{
            "Authorization": "Bearer ${QONTINUI_COORD_MCP_NONCE:-n}"
        }}}});
        assert!(!config_doc_references_terminal_env(&bare));
    }

    /// Stated limit of the resolver: a default containing `}` ends early.
    #[test]
    fn a_default_containing_a_closing_brace_ends_early() {
        assert_eq!(env_ref_client_default("${X:-a}b}"), "ab}");
    }
}

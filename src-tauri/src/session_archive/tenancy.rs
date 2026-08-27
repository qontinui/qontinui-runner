//! Tenant attribution for archived sessions — plan
//! `2026-08-26-claude-code-session-repository-in-qontinui-web` §3.6, which
//! calls this "the hardest correctness problem here".
//!
//! ## The problem
//!
//! **The transcript carries no tenant.** Claude Code's JSONL has `cwd`,
//! `gitBranch` and `sessionId`; it has never heard of tenants. Attribution is
//! therefore always external to the content, and for historical sessions every
//! source is degraded. So this module's job is not to *find* the tenant — it is
//! to record, for every row, **how** the tenant was arrived at, so a guess can
//! never render identically to a declaration.
//!
//! ## The five labels, and who may emit them
//!
//! | label | means |
//! |---|---|
//! | `declared` | the session's SPAWN INPUT carried an explicit tenant |
//! | `derived_repo` | coord's D2 repo rule named exactly one candidate |
//! | `derived_sole_binding` | the device (or coord, on its behalf) had exactly one binding |
//! | `ambiguous` | the repo rule named more than one candidate |
//! | `unknown` | nothing could establish it |
//!
//! ### `declared` is deliberately unreachable from this scanner
//!
//! Not an omission — a measurement. Two independent facts settle it:
//!
//! 1. `register_sniffed_session` (`claude_session/coord_register.rs:396-398`)
//!    builds its `Started` payload with `tenant_id` **omitted on purpose**, so
//!    coord resolves it from the device registration. Every interactive pane —
//!    the exact session class this repository exists to archive — therefore has
//!    a coord tenant that coord DERIVED, never one the operator declared.
//! 2. The runner's own record collapses the two anyway.
//!    `session::stamp_session_tenant` (`session/mod.rs:392-405`) stores
//!    `intent.tenant_id` if present and otherwise the machine-wide pin, into
//!    the SAME field — so `TerminalSessionRecord::tenant_id` cannot answer
//!    "was this declared?" even for a runner-spawned session.
//!
//! Recording a coord-derived value as `declared` is precisely the defect
//! §3.6 rule 2 exists to prevent, so this module never emits it. The variant
//! exists because the column's vocabulary is shared with writers that CAN
//! prove it (a future spawn path that records the provenance alongside the
//! value).
//!
//! ### `derived_repo` reuses coord's D2 rule and never invents a second one
//!
//! §3.6 rule 3 is explicit that the derivation must be coord's — `cwd`→repo,
//! name-normalized against `coord.tenant_repos`, **intersected with the
//! device's bindings** (`coord.tenant_devices`) — and that the candidate
//! function is what to call, NOT `POST /agents/spawn`'s handler, whose three
//! arms are wrong to inherit here:
//!
//! | `|T|` | the spawn handler does | this module records |
//! |---|---|---|
//! | 1 | uses it | `derived_repo` |
//! | 0 | falls back to the legacy `coord.devices.tenant_id` pointer, else `422` | sole binding, else `unknown` |
//! | >1 | returns **`400 tenant_ambiguous`** — a rejection | `ambiguous` — a recorded label |
//!
//! A backfill that called the handler would 400 on exactly the sessions rule 2
//! exists to *label*.
//!
//! ### Where the candidate vector comes from, and why it is often UNAVAILABLE
//!
//! `tenant_scope::repo_derived_tenants` is a **coord-internal** function: it
//! takes a `&Arc<AppState>` and queries `coord.tenant_repos ⋈
//! coord.tenant_devices` over coord's own Postgres pool. The runner has no such
//! pool, and coord serves that join over HTTP only behind the `TenantId`
//! extractor, which resolves **solely** from an `auth_sso::OperatorContext`
//! (`tenant_scope.rs`) — a forwarded Cognito operator bearer. The runner holds
//! a **device** JWT, which that extractor rejects with `403
//! tenant_not_resolved`. Verified 2026-08-26 against the coord checkout:
//! `/pr-merge/repos`, `/coord/ci/status` and every other `tenant_repos` reader
//! is on that extractor, and no device-authed candidate route exists.
//!
//! So on this machine the candidate vector is normally
//! [`RepoTenantCandidates::Unavailable`] — which is **not** the empty vector.
//! "coord's rule said no tenant owns this repo" and "coord's rule could not be
//! asked" are different facts, and collapsing them would report the second as
//! the first, the same absence-is-not-zero error the fleet's
//! `silent-empty-is-unknown` rule names. `Unavailable` therefore falls through
//! to the sole-binding arm rather than to `ambiguous`.
//!
//! The escape hatch is [`RepoTenantMap`]: an operator who CAN read coord's
//! repo ownership (they hold the Cognito bearer that door requires) exports it
//! to a JSON file and points the backfill at it with `--tenant-repo-map`. That
//! is still coord's D2 data and still intersected with the device's bindings
//! here — one rule, read through a door the runner is allowed to use.
//!
//! ### The last resort: an empty binding list is UNKNOWN, not zero
//!
//! [`sole_binding`] takes the device's cached `coord.tenant_devices` set first
//! and `machine.json::active_tenant_id` only when that set is EMPTY. It has to:
//! measured on the operator box 2026-08-27, `paired_user.json` does not exist
//! at all while `machine.json` carries a pin, so treating the absent file as
//! "zero bindings" would file the entire 6,400-session corpus under `unknown`
//! while the machine plainly knows which tenant it runs as. The pin's own
//! documentation ("the default for NEW sessions", not "the only tenant this
//! device serves") is why it is labelled `derived_sole_binding` and never
//! anything stronger — the same reading §3.6 rule 3 gives coord's legacy
//! `coord.devices.tenant_id` pointer.

use std::collections::HashMap;
use std::path::Path;

use uuid::Uuid;

/// How a row's `tenant_id` was established. The wire values are the web
/// schema's `SessionTenantSource` literal and the `ck_session_artifacts_tenant_source`
/// CHECK, so a typo here is a 422 rather than a silently mislabelled row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TenantSource {
    /// The spawn input carried an explicit tenant. See the module doc — this
    /// scanner cannot prove it and therefore never emits it.
    Declared,
    /// coord's D2 repo rule named exactly one candidate.
    DerivedRepo,
    /// Exactly one binding existed, so there was nothing to choose between.
    DerivedSoleBinding,
    /// The repo rule named more than one candidate. A LABEL, not an error.
    Ambiguous,
    /// Nothing could establish it.
    Unknown,
}

impl TenantSource {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::DerivedRepo => "derived_repo",
            Self::DerivedSoleBinding => "derived_sole_binding",
            Self::Ambiguous => "ambiguous",
            Self::Unknown => "unknown",
        }
    }

    /// Every label, in report order. Used for the run's histogram, which §3.6
    /// rule 5 requires as a first-class output — including the buckets that
    /// are empty, because "no ambiguous sessions" and "the histogram does not
    /// mention ambiguity" read very differently to an operator reviewing it.
    pub const ALL: [TenantSource; 5] = [
        TenantSource::Declared,
        TenantSource::DerivedRepo,
        TenantSource::DerivedSoleBinding,
        TenantSource::Ambiguous,
        TenantSource::Unknown,
    ];
}

/// The result of coord's D2 candidate rule for one repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoTenantCandidates {
    /// The rule ran. The vector is coord's raw candidate set — possibly
    /// empty, which is a real answer.
    Evaluated(Vec<Uuid>),
    /// The rule could NOT be run from here, naming why. Not an empty set.
    Unavailable,
}

/// An operator-supplied projection of `coord.tenant_repos`: repo name → the
/// tenants that own it.
///
/// Keys are matched after the same normalization coord's own SQL applies
/// (`tr.repo = ANY($2) OR split_part(tr.repo, '/', 2) = ANY($2)`), so both
/// `qontinui/qontinui-runner` and `qontinui-runner` resolve.
#[derive(Debug, Clone, Default)]
pub struct RepoTenantMap {
    by_repo: HashMap<String, Vec<Uuid>>,
}

impl RepoTenantMap {
    /// Load from a JSON object of `{"<repo>": ["<tenant-uuid>", …]}`.
    ///
    /// Errors are returned rather than swallowed: an operator who passed
    /// `--tenant-repo-map` asked for repo-derived attribution, and silently
    /// giving them `unknown` for 8,000 rows because the file had a typo is the
    /// kind of quiet degradation this plan is full of warnings about.
    pub fn from_json_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read tenant-repo map {}: {e}", path.display()))?;
        let raw: HashMap<String, Vec<String>> = serde_json::from_str(&text).map_err(|e| {
            format!(
                "parse tenant-repo map {}: {e} — expected {{\"<repo>\": [\"<tenant-uuid>\", …]}}",
                path.display()
            )
        })?;
        let mut by_repo: HashMap<String, Vec<Uuid>> = HashMap::with_capacity(raw.len());
        for (repo, tenants) in raw {
            let mut parsed = Vec::with_capacity(tenants.len());
            for t in &tenants {
                parsed.push(
                    Uuid::parse_str(t.trim())
                        .map_err(|e| format!("tenant-repo map: repo {repo:?} tenant {t:?}: {e}"))?,
                );
            }
            // Both spellings coord's own predicate accepts.
            by_repo.insert(normalize_repo(&repo), parsed.clone());
            by_repo.insert(repo.trim().to_ascii_lowercase(), parsed);
        }
        Ok(Self { by_repo })
    }

    /// True when the map holds no repos at all.
    pub fn is_empty(&self) -> bool {
        self.by_repo.is_empty()
    }

    /// coord's D2 candidate vector for `repo`, intersected with the device's
    /// bindings — the `coord.tenant_devices` half of the rule, applied here
    /// because the exported map carries only the `tenant_repos` half.
    pub fn candidates(&self, repo: Option<&str>, device_bindings: &[Uuid]) -> RepoTenantCandidates {
        let Some(repo) = repo else {
            // No repo means the rule has no input, not that it ran and found
            // nothing. Fall through to the sole-binding arm.
            return RepoTenantCandidates::Unavailable;
        };
        let owners = self
            .by_repo
            .get(&normalize_repo(repo))
            .cloned()
            .unwrap_or_default();
        let mut hit: Vec<Uuid> = owners
            .into_iter()
            .filter(|t| device_bindings.contains(t))
            .collect();
        hit.sort();
        hit.dedup();
        RepoTenantCandidates::Evaluated(hit)
    }
}

/// The repo-name normalization coord's D2 predicate performs in SQL: an
/// `owner/name` is also matched by its bare `name`.
pub fn normalize_repo(repo: &str) -> String {
    let trimmed = repo.trim().trim_end_matches('/');
    let tail = trimmed.rsplit('/').next().unwrap_or(trimmed);
    tail.trim_end_matches(".git").to_ascii_lowercase()
}

/// One session's tenant attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantAttribution {
    /// The tenant, when one was established. `None` for `ambiguous` and
    /// `unknown` — the web schema permits a `tenant_source` with no
    /// `tenant_id`, and picking one of several candidates to avoid a null
    /// would be the guess the label exists to refuse.
    pub tenant_id: Option<Uuid>,
    pub source: TenantSource,
}

/// Resolve one session's tenant, recording HOW.
///
/// Precedence, and why:
///
/// 1. **The session record's own tenant** (`session_tenant`, read from the
///    runner's `terminal-sessions.json`) wins when present. §3.6's source
///    table ranks it first for a reason: it is the tenant the session
///    ACTUALLY ran under — the value coord stamped and the coord-sync loop
///    presented credentials for — whereas everything below it is inference
///    from the working directory. It is labelled `derived_sole_binding`, never
///    `declared`; see the module doc for the two facts that force that.
/// 2. **coord's D2 repo candidates**, mapped by rule 3's three arms.
/// 3. **Sole binding** — reached when the rule ran and named nothing, and when
///    it could not be run at all.
/// 4. **`unknown`** — a device with zero or several bindings and no other
///    signal. An honest null, not a default.
pub fn resolve_tenant(
    session_tenant: Option<Uuid>,
    candidates: &RepoTenantCandidates,
    device_bindings: &[Uuid],
    machine_pin: Option<Uuid>,
) -> TenantAttribution {
    if let Some(t) = session_tenant {
        return TenantAttribution {
            tenant_id: Some(t),
            source: TenantSource::DerivedSoleBinding,
        };
    }
    match candidates {
        RepoTenantCandidates::Evaluated(t) if t.len() == 1 => TenantAttribution {
            tenant_id: Some(t[0]),
            source: TenantSource::DerivedRepo,
        },
        RepoTenantCandidates::Evaluated(t) if t.len() > 1 => TenantAttribution {
            tenant_id: None,
            source: TenantSource::Ambiguous,
        },
        // `Evaluated([])` and `Unavailable` both land here, and they mean
        // different things — but they have the SAME remaining option, which is
        // the device's own binding set. The distinction is preserved where it
        // matters (an `Unavailable` run never reports `ambiguous`, so an
        // operator reading the histogram cannot mistake "we could not ask" for
        // "we asked and it was contested").
        _ => sole_binding(device_bindings, machine_pin),
    }
}

/// The device's own binding set as a last resort, then the machine pin.
///
/// Two sources, in that order, and the second one needs justifying because it
/// is weaker than it looks:
///
/// - **`coord.tenant_devices` as cached in `paired_user.json`.** Exactly one
///   binding means there was nothing to choose between, which is what
///   `derived_sole_binding` says. More than one means the choice is real and
///   nothing here can make it — `unknown`, never a pick.
/// - **`machine.json::active_tenant_id`, but only when the binding list is
///   EMPTY.** An empty list is *unknown*, not *zero*: on this fleet
///   `paired_user.json` may not exist at all while `machine.json` still carries
///   a pin (measured on the operator box 2026-08-27 — no `paired_user.json`,
///   `active_tenant_id` present). The pin is documented as the default for NEW
///   sessions rather than "the only tenant this device serves", so it is
///   evidence of a sole binding only on a single-tenant install and cannot be
///   told apart from a default here. §3.6 rule 3 already settles how to label
///   exactly this class of pointer: coord's legacy `coord.devices.tenant_id` is
///   "`derived_sole_binding` at best and must never read as declared". Same
///   reading, same label.
///
///   Deliberately NOT consulted when the binding list is non-empty and
///   ambiguous: a device that is provably bound to several tenants must not
///   have the tie broken by a pin that only says which one is next.
fn sole_binding(device_bindings: &[Uuid], machine_pin: Option<Uuid>) -> TenantAttribution {
    match device_bindings {
        [only] => TenantAttribution {
            tenant_id: Some(*only),
            source: TenantSource::DerivedSoleBinding,
        },
        [] => match machine_pin {
            Some(pin) => TenantAttribution {
                tenant_id: Some(pin),
                source: TenantSource::DerivedSoleBinding,
            },
            None => TenantAttribution {
                tenant_id: None,
                source: TenantSource::Unknown,
            },
        },
        _ => TenantAttribution {
            tenant_id: None,
            source: TenantSource::Unknown,
        },
    }
}

/// The run's `tenant_source` histogram — §3.6 rule 5 makes this a first-class
/// output of the backfill, not a debug line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenantSourceHistogram {
    counts: HashMap<TenantSource, usize>,
}

impl TenantSourceHistogram {
    pub fn record(&mut self, source: TenantSource) {
        *self.counts.entry(source).or_insert(0) += 1;
    }

    pub fn get(&self, source: TenantSource) -> usize {
        self.counts.get(&source).copied().unwrap_or(0)
    }

    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }

    /// Render every bucket, including the empty ones.
    pub fn render(&self) -> String {
        let mut out = String::from("tenant_source histogram:\n");
        for s in TenantSource::ALL {
            out.push_str(&format!("  {:<22} {}\n", s.as_str(), self.get(s)));
        }
        out.push_str(&format!("  {:<22} {}\n", "(total)", self.total()));
        if self.get(TenantSource::Ambiguous) > 0 {
            out.push_str(
                "\nnote: the `ambiguous` bucket is an operator review item, not an error — \
                 §3.6 rule 5 makes reviewing it an exit criterion. Query it back with \
                 GET /api/v1/session-repository?tenant_source=ambiguous\n",
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn a_session_record_tenant_is_never_reported_as_declared() {
        // The whole point of §3.6's correction: `terminal_claude` panes carry
        // a tenant coord DERIVED, and the runner's own field collapses spawn
        // input with the machine pin. Neither can prove a declaration.
        let got = resolve_tenant(Some(u(1)), &RepoTenantCandidates::Unavailable, &[u(1), u(2)], None);
        assert_eq!(got.tenant_id, Some(u(1)));
        assert_eq!(got.source, TenantSource::DerivedSoleBinding);
        assert_ne!(got.source, TenantSource::Declared);
    }

    #[test]
    fn one_candidate_is_derived_repo() {
        let got = resolve_tenant(None, &RepoTenantCandidates::Evaluated(vec![u(7)]), &[], None);
        assert_eq!(
            got,
            TenantAttribution {
                tenant_id: Some(u(7)),
                source: TenantSource::DerivedRepo
            }
        );
    }

    #[test]
    fn several_candidates_are_labelled_ambiguous_and_never_rejected() {
        // The spawn handler answers 400 here. A backfill must not: these are
        // exactly the sessions the label exists to mark.
        let got = resolve_tenant(
            None,
            &RepoTenantCandidates::Evaluated(vec![u(7), u(8)]),
            &[u(9)],
            None,
        );
        assert_eq!(got.source, TenantSource::Ambiguous);
        assert_eq!(got.tenant_id, None, "ambiguous must not pick a winner");
    }

    #[test]
    fn no_candidates_falls_to_a_sole_binding() {
        let got = resolve_tenant(None, &RepoTenantCandidates::Evaluated(vec![]), &[u(4)], None);
        assert_eq!(
            got,
            TenantAttribution {
                tenant_id: Some(u(4)),
                source: TenantSource::DerivedSoleBinding
            }
        );
    }

    #[test]
    fn no_candidates_and_several_bindings_is_unknown_not_ambiguous() {
        // §3.6 rule 3's `|T|=0` arm: sole binding if the device has exactly
        // one, else unknown. `ambiguous` belongs to the `|T|>1` arm only.
        let got = resolve_tenant(None, &RepoTenantCandidates::Evaluated(vec![]), &[u(4), u(5)], None);
        assert_eq!(got.source, TenantSource::Unknown);
        assert_eq!(got.tenant_id, None);
    }

    #[test]
    fn an_unavailable_rule_is_not_an_empty_candidate_set() {
        // Both fall through to sole binding, but an UNAVAILABLE rule must
        // never be able to produce `ambiguous` — that would report "we could
        // not ask coord" as "coord said it was contested".
        let got = resolve_tenant(None, &RepoTenantCandidates::Unavailable, &[u(4), u(5)], None);
        assert_eq!(got.source, TenantSource::Unknown);
    }

    #[test]
    fn an_empty_binding_list_falls_through_to_the_machine_pin() {
        // Measured on the operator box 2026-08-27: no `paired_user.json` at
        // all, but `machine.json::active_tenant_id` present. An empty binding
        // list is UNKNOWN, not zero, so the pin is the remaining evidence.
        let got = resolve_tenant(
            None,
            &RepoTenantCandidates::Unavailable,
            &[],
            Some(u(11)),
        );
        assert_eq!(
            got,
            TenantAttribution {
                tenant_id: Some(u(11)),
                source: TenantSource::DerivedSoleBinding
            }
        );
    }

    #[test]
    fn a_machine_pin_never_breaks_a_real_binding_tie() {
        // A device provably bound to two tenants is ambiguous. The pin only
        // says which one is next for a NEW session; letting it decide here
        // would file historical sessions under a default they never ran in.
        let got = resolve_tenant(
            None,
            &RepoTenantCandidates::Unavailable,
            &[u(4), u(5)],
            Some(u(4)),
        );
        assert_eq!(got.source, TenantSource::Unknown);
        assert_eq!(got.tenant_id, None);
    }

    #[test]
    fn the_legacy_device_pointer_arm_is_not_inherited() {
        // The spawn handler's `|T|=0` arm reaches for
        // `coord.devices.tenant_id`. Nothing here does; with no bindings and
        // no candidates the answer is `unknown`.
        let got = resolve_tenant(None, &RepoTenantCandidates::Evaluated(vec![]), &[], None);
        assert_eq!(got.source, TenantSource::Unknown);
    }

    #[test]
    fn the_repo_map_applies_coords_name_normalization_and_binding_intersection() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("map.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"qontinui/qontinui-runner": ["{}", "{}"], "solo-repo": ["{}"]}}"#,
                u(1),
                u(2),
                u(3)
            ),
        )
        .unwrap();
        let map = RepoTenantMap::from_json_file(&path).unwrap();

        // Both spellings resolve, and the device-binding intersection is what
        // narrows two owners to one candidate.
        assert_eq!(
            map.candidates(Some("qontinui-runner"), &[u(2)]),
            RepoTenantCandidates::Evaluated(vec![u(2)])
        );
        assert_eq!(
            map.candidates(Some("qontinui/qontinui-runner"), &[u(1), u(2)]),
            RepoTenantCandidates::Evaluated(vec![u(1), u(2)])
        );
        // A repo the map does not mention is an EVALUATED empty set — the
        // rule ran and named nobody.
        assert_eq!(
            map.candidates(Some("some-other-repo"), &[u(1)]),
            RepoTenantCandidates::Evaluated(vec![])
        );
        // No repo at all is UNAVAILABLE — the rule had no input.
        assert_eq!(map.candidates(None, &[u(1)]), RepoTenantCandidates::Unavailable);
    }

    #[test]
    fn a_malformed_repo_map_is_an_error_not_a_silent_empty_map() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("map.json");
        std::fs::write(&path, r#"{"repo": ["not-a-uuid"]}"#).unwrap();
        let err = RepoTenantMap::from_json_file(&path).unwrap_err();
        assert!(err.contains("not-a-uuid"), "unhelpful error: {err}");
    }

    #[test]
    fn the_histogram_renders_every_bucket_including_the_empty_ones() {
        let mut h = TenantSourceHistogram::default();
        h.record(TenantSource::Unknown);
        h.record(TenantSource::Unknown);
        h.record(TenantSource::DerivedRepo);
        let rendered = h.render();
        for s in TenantSource::ALL {
            assert!(rendered.contains(s.as_str()), "missing bucket {}", s.as_str());
        }
        assert_eq!(h.total(), 3);
        assert_eq!(h.get(TenantSource::Unknown), 2);
        assert_eq!(h.get(TenantSource::Ambiguous), 0);
    }
}

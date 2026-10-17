//! The fan-out ledger's value types and the pure rules over them.
//!
//! Nothing here performs I/O. The dispatcher ([`super::dispatcher`]) owns the
//! mutation order; these types are what it mutates, what the store persists and
//! what the HTTP routes serialize.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Most members one run may carry. The preview (`promptMatrix.ts`) refuses
/// above its own, lower ceiling; this is the server's backstop against a
/// caller that skipped the preview.
pub const MAX_MEMBERS: usize = 64;

/// Largest [`prompt_argv_cost`] a member's prompt may have. The prompt rides
/// the spawn argv as one positional argument, and the OS command-line limit
/// (32,767 UTF-16 units on Windows, shared with every other flag) is the real
/// ceiling; this leaves room for the flags. A costlier prompt is refused at
/// create, never truncated. The preview (`promptMatrix.ts`) enforces the same
/// bound with the same measure.
pub const MAX_PROMPT_ARGV_COST: usize = 24 * 1024;

/// The worst-case length the prompt can occupy on a Windows command line,
/// computed identically on every platform so the preview and the server agree.
///
/// `CreateProcessW` takes ONE string, so each argv element is quoted the MSVC
/// way: wrapped in `"…"`, every `"` escaped as `\"`, and a run of backslashes
/// doubled when a quote or the closing quote follows it. Counting every quote
/// and every backslash once more is therefore an upper bound on the escaped length, and
/// UTF-8 bytes bound UTF-16 units from above. A prompt of 24 KiB of quotes —
/// 24 KiB of bytes, but 48 KiB once escaped — no longer passes a byte bound and
/// then fails at spawn.
pub fn prompt_argv_cost(prompt: &str) -> usize {
    let escapes = prompt.bytes().filter(|b| *b == b'"' || *b == b'\\').count();
    prompt.len() + escapes + 2
}

/// Longest member title, in characters.
pub const MAX_TITLE_CHARS: usize = 200;

/// Why a slot was released or a member is waiting. Stable wire words.
pub mod reason {
    /// The member's terminal exited or was closed.
    pub const TERMINAL_EXIT: &str = "terminal_exit";
    /// The session carries the runner-local finished marker.
    pub const FINISHED: &str = "finished";
    /// An operator released the slot through the route.
    pub const OPERATOR_RELEASE: &str = "operator_release";
    /// The runner restarted and the member's terminal did not survive it.
    pub const RUNNER_RESTARTED: &str = "runner_restarted";
    /// Coord has drained this device (or its drain state is unknown); the
    /// member stays queued until autonomous spawns may run again.
    pub const RUNNER_DRAINING: &str = "runner_draining";
    /// The `parallel_fanout` admission bound was fully occupied this tick.
    pub const FANOUT_BOUND_OCCUPIED: &str = "fanout_bound_occupied";
    /// The ledger could not record the admission, so the spawn was not made.
    pub const STORE_UNAVAILABLE: &str = "store_unavailable";
    /// An operator cancelled the queued member.
    pub const CANCELLED: &str = "cancelled";
    /// The member was admitted but the runner restarted before its spawn was
    /// recorded, and no session record exists for it: it may never have run.
    pub const SPAWN_UNCONFIRMED: &str = "spawn_unconfirmed";
    /// Loaded at runner start from a run idle for longer than the staleness
    /// bound (a torn-down temp runner's run, picked up by a later runner
    /// reusing its instance name): its waiting members are cancelled rather
    /// than admitted days after anyone asked for them.
    pub const STALE_AFTER_RESTART: &str = "stale_after_restart";
}

/// A member's place in the queue.
///
/// `queued → admitted → released`, plus `cancelled` (only from `queued`) and
/// `refused` (a resource-guard, authorization or spawn error; it returns to
/// `queued` on the next tick with its reason kept). There is no `seeded` state:
/// the prompt rides the spawn argv, so an admitted member is already seeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    Queued,
    Admitted,
    Released,
    Cancelled,
    Refused,
}

impl MemberState {
    /// The persisted word — the same as the wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            MemberState::Queued => "queued",
            MemberState::Admitted => "admitted",
            MemberState::Released => "released",
            MemberState::Cancelled => "cancelled",
            MemberState::Refused => "refused",
        }
    }

    /// Parse a persisted word. `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => MemberState::Queued,
            "admitted" => MemberState::Admitted,
            "released" => MemberState::Released,
            "cancelled" => MemberState::Cancelled,
            "refused" => MemberState::Refused,
            _ => return None,
        })
    }

    /// No further transition is possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, MemberState::Released | MemberState::Cancelled)
    }
}

/// A run is `active` while any member can still reach a terminal, and
/// `completed` once every member is `released` or `cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Active,
    Completed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Active => "active",
            RunState::Completed => "completed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(RunState::Active),
            "completed" => Some(RunState::Completed),
            _ => None,
        }
    }
}

/// Which Claude account each member launches under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConfigDirPolicy {
    /// Re-pick the best-headroom account for EACH member at its admission —
    /// the runner's own `pick_best_account`, the rule the Terminal page's
    /// launcher applies.
    BestHeadroom,
    /// Every member launches under this one `CLAUDE_CONFIG_DIR`.
    #[serde(rename_all = "camelCase")]
    Fixed { config_dir: String },
}

/// One fan-out run: the run-level fields every member spawn reads.
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutRun {
    pub id: Uuid,
    /// The tenant admitted ONCE at create (`admit_spawn_tenant`) and carried to
    /// every member spawn. `None` is the device-default binding.
    pub tenant_id: Option<Uuid>,
    pub template_slug: Option<String>,
    pub template_version: Option<i32>,
    /// Already clamped to the `parallel_fanout` bound at create / PATCH.
    pub max_concurrent: u32,
    pub config_dir_policy: ConfigDirPolicy,
    pub working_dir: String,
    pub created_at: DateTime<Utc>,
    pub state: RunState,
    /// The runner instance driving this run. A temp runner and the primary
    /// share one PG cluster; only the owner admits, releases or reconciles.
    pub owner_instance: String,
    /// When a run-level field was last written (`None`: never since create,
    /// or a row written before the column existed).
    pub updated_at: Option<DateTime<Utc>>,
}

/// One member of a run. `index` is its position in the POSTED list (0-based,
/// contiguous) — the address the release route and the admission order use.
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutMember {
    pub index: u32,
    /// Its row number in the operator's preview (0-based), when the caller
    /// sent one. Differs from `index` once rows were unticked before create:
    /// the preview showed `#5`, and the strip must show `#5` too, while the
    /// posted list — and so `index` — is renumbered from 0.
    pub preview_index: Option<u32>,
    pub title: String,
    pub prompt: String,
    pub state: MemberState,
    pub terminal_id: Option<String>,
    /// The `--session-id` the member was spawned under, minted fresh at its
    /// admission.
    pub claude_session_id: Option<String>,
    pub reason: Option<String>,
    pub admitted_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    /// When the member's row was last written — by EVERY write, a refusal or a
    /// drain deferral included, so a run's staleness is measured from what
    /// actually last happened to it rather than from `admitted_at` (which an
    /// undone admission clears) or `released_at`.
    pub updated_at: Option<DateTime<Utc>>,
}

impl FanoutMember {
    /// A freshly queued member.
    pub fn queued(index: u32, preview_index: Option<u32>, title: String, prompt: String) -> Self {
        Self {
            index,
            preview_index,
            title,
            prompt,
            state: MemberState::Queued,
            terminal_id: None,
            claude_session_id: None,
            reason: None,
            admitted_at: None,
            released_at: None,
            updated_at: None,
        }
    }
}

/// The member list a caller posts: exactly the previewed rows.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInput {
    pub title: String,
    pub prompt: String,
    /// The row's number in the preview it was ticked in. Optional; when any
    /// member carries one, every member must, strictly ascending.
    #[serde(default)]
    pub preview_index: Option<u32>,
}

/// Validate a posted member list and build the queued members.
///
/// The server never re-expands a matrix: what was previewed is what runs, so
/// every refusal here names the offending row instead of repairing it.
pub fn build_members(inputs: &[MemberInput]) -> Result<Vec<FanoutMember>, String> {
    if inputs.is_empty() {
        return Err("members: at least one member is required".to_string());
    }
    if inputs.len() > MAX_MEMBERS {
        return Err(format!(
            "members: {} members exceeds the ceiling of {MAX_MEMBERS}",
            inputs.len()
        ));
    }
    check_preview_indices(inputs)?;
    inputs
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let title = m.title.trim();
            if title.is_empty() {
                return Err(format!("members[{i}].title: empty"));
            }
            if title.chars().count() > MAX_TITLE_CHARS {
                return Err(format!(
                    "members[{i}].title: longer than {MAX_TITLE_CHARS} characters"
                ));
            }
            if m.prompt.trim().is_empty() {
                return Err(format!("members[{i}].prompt: empty"));
            }
            let cost = prompt_argv_cost(&m.prompt);
            if cost > MAX_PROMPT_ARGV_COST {
                return Err(format!(
                    "members[{i}].prompt: its escaped argv length is {cost}, over the bound of \
                     {MAX_PROMPT_ARGV_COST} — refused rather than truncated"
                ));
            }
            if m.prompt.contains('\0') {
                return Err(format!(
                    "members[{i}].prompt: contains a NUL byte, which no argv can carry"
                ));
            }
            let index = u32::try_from(i).map_err(|_| format!("members[{i}]: index overflow"))?;
            Ok(FanoutMember::queued(
                index,
                m.preview_index,
                title.to_string(),
                m.prompt.clone(),
            ))
        })
        .collect()
}

/// Preview indices are all-or-none and strictly ascending: the posted list is
/// the ticked preview rows in preview order, so anything else means the caller
/// numbered rows that are not the ones it previewed — refused, never repaired.
fn check_preview_indices(inputs: &[MemberInput]) -> Result<(), String> {
    let carried = inputs.iter().filter(|m| m.preview_index.is_some()).count();
    if carried == 0 {
        return Ok(());
    }
    if carried != inputs.len() {
        let i = inputs
            .iter()
            .position(|m| m.preview_index.is_none())
            .unwrap_or(0);
        return Err(format!(
            "members[{i}].previewIndex: missing — when any member carries a preview index, \
             every member must"
        ));
    }
    let mut prev: Option<u32> = None;
    for (i, m) in inputs.iter().enumerate() {
        let p = m.preview_index.unwrap_or(0);
        if prev.is_some_and(|q| p <= q) {
            return Err(format!(
                "members[{i}].previewIndex: {p} is not greater than the previous row's — \
                 preview indices must be strictly ascending"
            ));
        }
        prev = Some(p);
    }
    Ok(())
}

/// `requested` clamped to `[1, bound]`. A bound of 0 cannot occur
/// (`effective_fanout_bound` floors it at 1), but the floor is restated here so
/// a zero can never deadlock a run.
pub fn clamp_max_concurrent(requested: u32, bound: u32) -> u32 {
    requested.clamp(1, bound.max(1))
}

/// Count of members per state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberCounts {
    pub queued: u32,
    pub admitted: u32,
    pub released: u32,
    pub cancelled: u32,
    pub refused: u32,
}

impl MemberCounts {
    pub fn of(members: &[FanoutMember]) -> Self {
        let mut c = Self::default();
        for m in members {
            match m.state {
                MemberState::Queued => c.queued += 1,
                MemberState::Admitted => c.admitted += 1,
                MemberState::Released => c.released += 1,
                MemberState::Cancelled => c.cancelled += 1,
                MemberState::Refused => c.refused += 1,
            }
        }
        c
    }
}

/// The run's state as its members make it: `completed` iff every member is
/// terminal.
pub fn derived_run_state(members: &[FanoutMember]) -> RunState {
    if members.iter().all(|m| m.state.is_terminal()) {
        RunState::Completed
    } else {
        RunState::Active
    }
}

/// Wire shape of one member (`GET /fanout/{id}`, the `fanout-changed` event).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub index: u32,
    /// The member's row number in the preview, when the creator sent one —
    /// what the strip shows as `#n`. `null` for a run created without it.
    pub preview_index: Option<u32>,
    pub title: String,
    pub prompt: String,
    pub state: MemberState,
    pub terminal_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub reason: Option<String>,
    pub admitted_at: Option<String>,
    pub released_at: Option<String>,
    /// Consecutive refusals for the same reason class (0 when not refused).
    pub refusals: u32,
    /// When a refused member next returns to the queue (backoff), if it is
    /// waiting on one.
    pub next_retry_at: Option<String>,
}

/// Wire shape of one run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub template_slug: Option<String>,
    pub template_version: Option<i32>,
    pub max_concurrent: u32,
    pub config_dir_policy: ConfigDirPolicy,
    pub working_dir: String,
    pub created_at: String,
    pub state: RunState,
    pub counts: MemberCounts,
    pub members: Vec<MemberView>,
}

impl RunView {
    pub fn of(run: &FanoutRun, members: &[FanoutMember]) -> Self {
        Self {
            id: run.id,
            tenant_id: run.tenant_id,
            template_slug: run.template_slug.clone(),
            template_version: run.template_version,
            max_concurrent: run.max_concurrent,
            config_dir_policy: run.config_dir_policy.clone(),
            working_dir: run.working_dir.clone(),
            created_at: run.created_at.to_rfc3339(),
            state: run.state,
            counts: MemberCounts::of(members),
            members: members
                .iter()
                .map(|m| MemberView {
                    index: m.index,
                    preview_index: m.preview_index,
                    title: m.title.clone(),
                    prompt: m.prompt.clone(),
                    state: m.state,
                    terminal_id: m.terminal_id.clone(),
                    claude_session_id: m.claude_session_id.clone(),
                    reason: m.reason.clone(),
                    admitted_at: m.admitted_at.map(|t| t.to_rfc3339()),
                    released_at: m.released_at.map(|t| t.to_rfc3339()),
                    refusals: 0,
                    next_retry_at: None,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(title: &str, prompt: &str) -> MemberInput {
        MemberInput {
            title: title.to_string(),
            prompt: prompt.to_string(),
            preview_index: None,
        }
    }

    fn previewed(title: &str, preview_index: u32) -> MemberInput {
        MemberInput {
            preview_index: Some(preview_index),
            ..input(title, "p")
        }
    }

    #[test]
    fn fanout_preview_index_is_kept_beside_the_renumbered_index() {
        // Rows #1, #3 and #6 of the preview were ticked.
        let members =
            build_members(&[previewed("a", 0), previewed("b", 2), previewed("c", 5)]).unwrap();
        let got: Vec<(u32, Option<u32>)> =
            members.iter().map(|m| (m.index, m.preview_index)).collect();
        assert_eq!(got, vec![(0, Some(0)), (1, Some(2)), (2, Some(5))]);
        let run = FanoutRun {
            id: Uuid::nil(),
            tenant_id: None,
            template_slug: None,
            template_version: None,
            max_concurrent: 1,
            config_dir_policy: ConfigDirPolicy::BestHeadroom,
            working_dir: "/w".to_string(),
            created_at: Utc::now(),
            state: RunState::Active,
            owner_instance: "primary".to_string(),
            updated_at: None,
        };
        let wire = serde_json::to_value(RunView::of(&run, &members)).unwrap();
        assert_eq!(wire["members"][1]["index"], 1);
        assert_eq!(wire["members"][1]["previewIndex"], 2);
        // Without one it is null on the wire, and the input parses without it.
        let plain = build_members(&[input("a", "p")]).unwrap();
        let wire = serde_json::to_value(RunView::of(&run, &plain)).unwrap();
        assert!(wire["members"][0]["previewIndex"].is_null());
        let parsed: MemberInput =
            serde_json::from_str(r#"{"title":"t","prompt":"p","previewIndex":4}"#).unwrap();
        assert_eq!(parsed.preview_index, Some(4));
    }

    #[test]
    fn fanout_preview_indices_are_all_or_none_and_strictly_ascending() {
        let err = build_members(&[previewed("a", 0), input("b", "p")]).unwrap_err();
        assert!(err.starts_with("members[1].previewIndex: missing"), "{err}");
        let err = build_members(&[previewed("a", 3), previewed("b", 3)]).unwrap_err();
        assert!(err.starts_with("members[1].previewIndex"), "{err}");
        let err = build_members(&[previewed("a", 4), previewed("b", 1)]).unwrap_err();
        assert!(err.contains("strictly ascending"), "{err}");
    }

    #[test]
    fn fanout_members_take_their_preview_position_as_index() {
        let members = build_members(&[input("a", "p0"), input(" b ", "p1")]).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!((members[0].index, members[1].index), (0, 1));
        assert_eq!(members[1].title, "b");
        assert!(members.iter().all(|m| m.state == MemberState::Queued));
    }

    #[test]
    fn fanout_member_list_refusals_name_the_row() {
        assert!(build_members(&[]).is_err());
        let err = build_members(&[input("a", "ok"), input("b", "  ")]).unwrap_err();
        assert!(err.starts_with("members[1].prompt"), "{err}");
        let err = build_members(&[input("", "p")]).unwrap_err();
        assert!(err.starts_with("members[0].title"), "{err}");
        let long = "x".repeat(MAX_PROMPT_ARGV_COST);
        let err = build_members(&[input("a", &long)]).unwrap_err();
        assert!(err.contains("refused rather than truncated"), "{err}");
        let many: Vec<MemberInput> = (0..=MAX_MEMBERS)
            .map(|i| input("t", &i.to_string()))
            .collect();
        assert!(build_members(&many).is_err());
    }

    /// The bound is on the Windows-escaped length, and the measure is pinned to
    /// the same numbers `promptMatrix.test.ts` pins for the preview.
    #[test]
    fn fanout_prompt_bound_counts_windows_quoting() {
        assert_eq!(prompt_argv_cost("abc"), 5);
        assert_eq!(prompt_argv_cost(r#"say "hi" \ there"#), 16 + 3 + 2);
        assert_eq!(prompt_argv_cost("é"), 2 + 2);
        // At the bound: accepted.
        let at = "x".repeat(MAX_PROMPT_ARGV_COST - 2);
        assert!(build_members(&[input("a", &at)]).is_ok());
        // A quote-heavy prompt well under the byte bound is refused: escaped,
        // it would not fit the command line.
        let quotes = "\"".repeat(MAX_PROMPT_ARGV_COST / 2);
        assert!(quotes.len() < MAX_PROMPT_ARGV_COST);
        let err = build_members(&[input("a", &quotes)]).unwrap_err();
        assert!(err.contains("escaped argv length"), "{err}");
        // Worst case still fits Windows' 32,767-unit command line with room.
        const { assert!(MAX_PROMPT_ARGV_COST < 32_767) };
    }

    #[test]
    fn fanout_cap_is_clamped_to_the_bound_and_never_zero() {
        assert_eq!(clamp_max_concurrent(40, 15), 15);
        assert_eq!(clamp_max_concurrent(3, 15), 3);
        assert_eq!(clamp_max_concurrent(0, 15), 1);
        assert_eq!(clamp_max_concurrent(5, 0), 1);
    }

    #[test]
    fn fanout_states_round_trip_their_persisted_words() {
        for s in [
            MemberState::Queued,
            MemberState::Admitted,
            MemberState::Released,
            MemberState::Cancelled,
            MemberState::Refused,
        ] {
            assert_eq!(MemberState::parse(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_value(s).unwrap(),
                serde_json::Value::String(s.as_str().to_string())
            );
        }
        assert_eq!(MemberState::parse("seeded"), None);
        for s in [RunState::Active, RunState::Completed] {
            assert_eq!(RunState::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn fanout_config_dir_policy_wire_shape() {
        let fixed: ConfigDirPolicy =
            serde_json::from_str(r#"{"kind":"fixed","configDir":"/home/x/.claude-a"}"#).unwrap();
        assert_eq!(
            fixed,
            ConfigDirPolicy::Fixed {
                config_dir: "/home/x/.claude-a".to_string()
            }
        );
        let best: ConfigDirPolicy = serde_json::from_str(r#"{"kind":"bestHeadroom"}"#).unwrap();
        assert_eq!(best, ConfigDirPolicy::BestHeadroom);
        assert_eq!(
            serde_json::to_value(&best).unwrap(),
            serde_json::json!({"kind": "bestHeadroom"})
        );
    }

    #[test]
    fn fanout_run_completes_only_when_every_member_is_terminal() {
        let mut members = build_members(&[input("a", "p"), input("b", "p")]).unwrap();
        assert_eq!(derived_run_state(&members), RunState::Active);
        members[0].state = MemberState::Released;
        members[1].state = MemberState::Refused;
        assert_eq!(derived_run_state(&members), RunState::Active);
        members[1].state = MemberState::Cancelled;
        assert_eq!(derived_run_state(&members), RunState::Completed);
    }
}

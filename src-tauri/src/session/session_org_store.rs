//! Operator-defined Session Manager organization — groups, colors, pins, tags,
//! order, collapse, mute, snooze, last-seen, recently closed, saved views and
//! dismissed group suggestions (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 2, D3).
//!
//! ## Why a runner store
//!
//! Pins and labels used to live in `localStorage`: per-origin, port-suffixed,
//! invisible to agents and to the UI Bridge, and not shared reliably between
//! Terminal windows. This store is the ONE authority per runner instance. The
//! Tauri commands (`commands::session_org`) and the loopback HTTP twins
//! (`GET/POST /terminal/session-org`, `mcp::session_org`) both go through the
//! same [`SessionOrgStore`], so a group an agent files itself into over HTTP is
//! the group the operator sees.
//!
//! ## Session keys
//!
//! A member is keyed by its [`session_key`]: the Claude session id when known,
//! else `pane:` + the window-aware [`PaneKey`]. Terminal ids are a fresh
//! `uuid_v4` per launch and are NOT stable (`pane_store.rs` module doc), so they
//! are never a key. The `pane:` prefix keeps the two spaces disjoint.
//!
//! ## Concurrency: per-op merge, never a whole-file write from a client
//!
//! Clients never send an org; they send [`SessionOrgOp`]s. Every op names the
//! fields it changes, and is applied under the store lock against the CURRENT
//! state. Two windows applying concurrently are serialized and each op lands on
//! the other's result — a pin from window B cannot erase a group window A just
//! created, because no op carries a stale copy of anything it does not change.
//! Within one apply, ops run in order and each reports its own outcome; a
//! refused op does not stop the ones after it.
//!
//! ## Storage and the corrupt-file arm
//!
//! One JSON file (`session-org.json`, instance-scoped beside
//! `pane-sessions.json`), rewritten atomically (temp file + rename) after each
//! successful apply. A batch is persisted before it is published: if the write
//! fails, the in-memory org is left as it was and the apply errors, so memory
//! and disk never disagree.
//!
//! A MISSING file is a fresh install: a known, empty org. An UNREADABLE file is
//! not: it loads as [`SessionOrgStatus::Unknown`] and is LEFT IN PLACE, so every
//! later start reads it as Unknown too. Every op is refused until a client sends
//! [`SessionOrgOp::ResetAfterUnknown`], which moves the bytes aside to
//! `session-org.corrupt-<ms>.json` before writing a fresh org, so nothing is
//! lost. An automatic op (a focus-driven `markSeen`) — or a restart — therefore
//! cannot silently turn "we could not read your groups" into "you have no
//! groups". A file whose `version` is newer than this build's
//! [`SCHEMA_VERSION`] is Unknown the same way: an older build would drop the
//! fields it does not know on its first write.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::session::pane_store::PaneKey;

/// On-disk schema version. Bumped only for a change an older build would
/// misread. A file carrying a NEWER version loads as Unknown (see the module
/// doc): serde ignores unknown fields, so an older build would otherwise drop
/// them on its first write.
pub const SCHEMA_VERSION: u32 = 1;

/// Recently-closed entries kept. The list is reported as `shown / total`
/// (`recentlyClosed.len()` / `recentlyClosedTotal`) so a trimmed list never
/// reads as the whole history.
pub const RECENTLY_CLOSED_CAP: usize = 200;

/// Longest group title / view name / tag accepted, in chars.
const MAX_LABEL_CHARS: usize = 120;
/// Longest saved-view query accepted, in chars.
const MAX_QUERY_CHARS: usize = 2_000;

/// The event every window listens on to refresh its copy of the org.
pub const SESSION_ORG_CHANGED_EVENT: &str = "session-org-changed";

/// Canonical file name, joined onto the instance-scoped runner dir by the
/// boot wiring in `main.rs`.
pub const STORE_FILE_NAME: &str = "session-org.json";

/// A group's color, as a ROLE name — never a hex value (D4).
///
/// Served `domain_spec/visual-system` reserves red and amber to the attention
/// layer, so group colors are drawn only from non-attention tokens that already
/// exist in `@qontinui/design-tokens` (`colors.ts`). Each variant names the
/// token it resolves from; the frontend owns the role → token mapping. Closed
/// on purpose: an unknown role in a request is a deserialization error, not a
/// silently-defaulted color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorRole {
    /// `brand.primary`.
    BrandPrimary,
    /// `brand.secondary`.
    BrandSecondary,
    /// `brand.success` (teal).
    BrandSuccess,
    /// `semantic.info`.
    Info,
    /// `border.strong`.
    BorderStrong,
    /// `text.muted`.
    TextMuted,
}

impl ColorRole {
    /// Every role, in the order a new group takes the next unused one.
    pub const ALL: [ColorRole; 6] = [
        ColorRole::BrandPrimary,
        ColorRole::BrandSecondary,
        ColorRole::BrandSuccess,
        ColorRole::Info,
        ColorRole::BorderStrong,
        ColorRole::TextMuted,
    ];
}

/// One operator-named group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroup {
    pub id: String,
    pub title: String,
    pub color_role: ColorRole,
    pub collapsed: bool,
    /// Position among groups; renormalized to `0..n` by every reorder.
    pub order: i64,
    /// Unix millis.
    pub created_at: i64,
}

/// Per-session organization state. A member whose every field is at its
/// default is dropped from the map (it carries no information).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MemberOrg {
    pub group_id: Option<String>,
    /// Position within its group (or within Ungrouped).
    pub order: i64,
    pub pinned: bool,
    /// Sorted, deduplicated.
    pub tags: Vec<String>,
    pub muted: bool,
    /// Unix millis; `None` = not snoozed.
    pub snoozed_until: Option<i64>,
    /// Unix millis of the last time the operator focused this session.
    pub last_seen_at: Option<i64>,
}

impl MemberOrg {
    fn is_default(&self) -> bool {
        *self == MemberOrg::default()
    }
}

/// One closed session, for the Recently closed list (Chrome Ctrl+Shift+T).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedSession {
    pub session_key: String,
    pub title: String,
    /// Unix millis.
    pub closed_at: i64,
    pub resume_cmd: Option<String>,
    /// The group the session was in when it closed, so a reopen can restore
    /// membership (Phase 5).
    #[serde(default)]
    pub group_id: Option<String>,
}

/// A named query string (Phase 4's grammar). Keyed by `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedView {
    pub name: String,
    pub query: String,
    /// Unix millis of the last save.
    pub saved_at: i64,
}

/// The whole organization. Also the on-disk shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOrg {
    pub version: u32,
    /// Bumped by every apply that changed something; lets a client drop an
    /// event older than the state it already holds.
    pub revision: u64,
    pub groups: Vec<SessionGroup>,
    pub members: BTreeMap<String, MemberOrg>,
    /// Newest first, at most [`RECENTLY_CLOSED_CAP`].
    pub recently_closed: Vec<ClosedSession>,
    /// Every close ever recorded, including the ones trimmed off the list.
    pub recently_closed_total: u64,
    pub saved_views: Vec<SavedView>,
    /// Deterministic suggestion keys (Phase 6) the operator dismissed.
    pub dismissed_suggestions: BTreeSet<String>,
}

impl Default for SessionOrg {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            revision: 0,
            groups: Vec::new(),
            members: BTreeMap::new(),
            recently_closed: Vec::new(),
            recently_closed_total: 0,
            saved_views: Vec::new(),
            dismissed_suggestions: BTreeSet::new(),
        }
    }
}

/// Whether the store could read its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionOrgStatus {
    Known,
    Unknown,
}

/// What `session_org_get`, `GET /terminal/session-org` and the change event
/// carry. `org` is `None` exactly when `status` is `Unknown` — an unreadable
/// org is never rendered as an empty one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOrgSnapshot {
    pub status: SessionOrgStatus,
    pub org: Option<SessionOrg>,
    /// Why the org is Unknown.
    pub reason: Option<String>,
    /// Where an unreadable file was preserved, if it could be moved.
    pub preserved_at: Option<String>,
}

/// One mutation. Every op names only the fields it changes (see the module
/// doc on per-op merge). Session keys come from [`session_key`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionOrgOp {
    /// Create a group. `groupId` lets an optimistic client name it up front;
    /// absent, the store mints one. `colorRole` absent takes the next role no
    /// group uses yet. `sessionKeys` are moved into the new group.
    CreateGroup {
        #[serde(default)]
        group_id: Option<String>,
        title: String,
        #[serde(default)]
        color_role: Option<ColorRole>,
        #[serde(default)]
        session_keys: Vec<String>,
    },
    RenameGroup {
        group_id: String,
        title: String,
    },
    RecolorGroup {
        group_id: String,
        color_role: ColorRole,
    },
    SetGroupCollapsed {
        group_id: String,
        collapsed: bool,
    },
    /// Move a group to `index` among groups (clamped).
    MoveGroup {
        group_id: String,
        index: usize,
    },
    /// Release every member but keep the (now empty) group.
    UngroupGroup {
        group_id: String,
    },
    /// Remove the group. Its members become ungrouped — never removed.
    DeleteGroup {
        group_id: String,
    },
    /// Append sessions to the end of a group.
    AddMembers {
        group_id: String,
        session_keys: Vec<String>,
    },
    /// Move sessions to Ungrouped.
    RemoveMembers {
        session_keys: Vec<String>,
    },
    /// Place one session at `index` within `groupId` (`None` = Ungrouped).
    MoveMember {
        session_key: String,
        #[serde(default)]
        group_id: Option<String>,
        index: usize,
    },
    SetPinned {
        session_keys: Vec<String>,
        pinned: bool,
    },
    AddTags {
        session_keys: Vec<String>,
        tags: Vec<String>,
    },
    RemoveTags {
        session_keys: Vec<String>,
        tags: Vec<String>,
    },
    SetMuted {
        session_keys: Vec<String>,
        muted: bool,
    },
    /// `until` absent clears the snooze.
    Snooze {
        session_keys: Vec<String>,
        #[serde(default)]
        until: Option<i64>,
    },
    /// Monotonic: an older `at` than the stored one is a no-op, so two
    /// windows racing never move last-seen backwards.
    MarkSeen {
        session_key: String,
        at: i64,
    },
    RecordClosed {
        session_key: String,
        title: String,
        closed_at: i64,
        #[serde(default)]
        resume_cmd: Option<String>,
    },
    /// Save (or overwrite, by name) a named query.
    SaveView {
        name: String,
        query: String,
    },
    DeleteView {
        name: String,
    },
    DismissSuggestion {
        key: String,
    },
    /// The one op accepted while the org is Unknown: acknowledge that the
    /// unreadable file (already preserved aside) is abandoned and start an
    /// empty org. A no-op when the org is Known.
    ResetAfterUnknown,
}

/// Outcome of one op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOrgOpResult {
    pub ok: bool,
    /// Machine-readable refusal code (`group_not_found`, `invalid_title`,
    /// `org_unknown`, …) when `ok` is false.
    pub error: Option<String>,
    /// The id of a group this op created.
    pub group_id: Option<String>,
}

impl SessionOrgOpResult {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            group_id: None,
        }
    }
    fn created(group_id: String) -> Self {
        Self {
            ok: true,
            error: None,
            group_id: Some(group_id),
        }
    }
    fn refused(code: &str) -> Self {
        Self {
            ok: false,
            error: Some(code.to_string()),
            group_id: None,
        }
    }
}

/// What `session_org_apply` / `POST /terminal/session-org` return: one result
/// per op, in order, plus the resulting state to reconcile to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOrgApplyResponse {
    pub results: Vec<SessionOrgOpResult>,
    pub snapshot: SessionOrgSnapshot,
}

/// The key a session's organization is stored under: the Claude session id
/// when known, else `pane:` + the window-aware [`PaneKey`]. Mirrored by
/// `sessionKeyFor` in `src/components/terminal/sessionOrgTypes.ts`; the pinned
/// literals in both test suites keep the two in step.
pub fn session_key(claude_session_id: Option<&str>, pane: &PaneKey) -> String {
    match claude_session_id.map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("pane:{}", pane.as_str()),
    }
}

/// Load result: the org, or why it could not be read.
enum Loaded {
    Known(SessionOrg),
    Unknown {
        reason: String,
        preserved_at: Option<PathBuf>,
    },
}

/// Called with the new snapshot after every apply that changed something.
pub type ChangeNotifier = Box<dyn Fn(&SessionOrgSnapshot) + Send + Sync>;

/// The store. Share it as `Arc<SessionOrgStore>` (Tauri `manage`).
pub struct SessionOrgStore {
    path: PathBuf,
    state: Mutex<Loaded>,
    notifier: Mutex<Option<ChangeNotifier>>,
}

impl std::fmt::Debug for SessionOrgStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionOrgStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SessionOrgStore {
    /// Open the store at `path`. A missing file is a known, empty org; an
    /// unreadable one is Unknown, left in place (see the module doc).
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let loaded = load(&path);
        Ok(Self {
            path,
            state: Mutex::new(loaded),
            notifier: Mutex::new(None),
        })
    }

    /// Install the change notifier (the Tauri event emitter at boot).
    pub fn set_notifier(&self, notifier: ChangeNotifier) {
        match self.notifier.lock() {
            Ok(mut slot) => *slot = Some(notifier),
            Err(e) => warn!(error = %e, "session_org: notifier lock poisoned"),
        }
    }

    pub fn snapshot(&self) -> SessionOrgSnapshot {
        match self.state.lock() {
            Ok(state) => snapshot_of(&state),
            Err(e) => unknown_snapshot(format!("store lock poisoned: {e}"), None),
        }
    }

    /// Apply `ops` in order against the current state. See the module doc.
    pub fn apply(&self, ops: &[SessionOrgOp]) -> Result<SessionOrgApplyResponse, String> {
        self.apply_at(ops, now_ms())
    }

    fn apply_at(&self, ops: &[SessionOrgOp], now: i64) -> Result<SessionOrgApplyResponse, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| format!("session_org: store lock poisoned: {e}"))?;

        let mut working: Option<SessionOrg> = match &*state {
            Loaded::Known(org) => {
                let mut org = org.clone();
                // A file written by hand (or by an older build) may hold
                // default members; prune them up front so `before` below
                // compares like with like.
                prune_default_members(&mut org);
                Some(org)
            }
            Loaded::Unknown { .. } => None,
        };
        let mut results = Vec::with_capacity(ops.len());
        let mut changed = false;
        for op in ops {
            match working.as_mut() {
                Some(org) => {
                    let before = org.clone();
                    let result = apply_op(org, op, now);
                    // Ops reach members through `entry().or_default()`, so a
                    // no-op (`setPinned false` on a non-member) leaves a
                    // default member behind. It carries no information and is
                    // dropped on persist anyway — prune it BEFORE comparing,
                    // or a no-op bumps the revision, rewrites the file and
                    // broadcasts a change for nothing.
                    prune_default_members(org);
                    if *org != before {
                        changed = true;
                    }
                    results.push(result);
                }
                None if matches!(op, SessionOrgOp::ResetAfterUnknown) => {
                    // A file left IN PLACE (a read error, or a failed move)
                    // is moved aside now, before the fresh org overwrites it;
                    // if it still cannot move, the reset is refused.
                    if self.path.exists() {
                        let aside = corrupt_aside_path(&self.path, now);
                        if let Err(e) = std::fs::rename(&self.path, &aside) {
                            warn!(error = %e, "session_org: reset refused — unreadable file could not be preserved");
                            results.push(SessionOrgOpResult::refused("preserve_failed"));
                            continue;
                        }
                        // The bytes now live at `aside`. Record that at once:
                        // if the persist below fails the org stays Unknown,
                        // and its `preservedAt` must name where the bytes
                        // ARE, not the path they just left.
                        if let Loaded::Unknown { preserved_at, .. } = &mut *state {
                            *preserved_at = Some(aside);
                        }
                    }
                    working = Some(SessionOrg::default());
                    changed = true;
                    results.push(SessionOrgOpResult::ok());
                }
                None => results.push(SessionOrgOpResult::refused("org_unknown")),
            }
        }

        if changed {
            if let Some(org) = working.as_mut() {
                prune_default_members(org);
                org.revision += 1;
                write_atomic(&self.path, org)
                    .map_err(|e| format!("session_org: persist failed: {e}"))?;
                *state = Loaded::Known(org.clone());
            }
        }
        let snapshot = snapshot_of(&state);
        drop(state);

        if changed {
            if let Ok(notifier) = self.notifier.lock() {
                if let Some(notify) = notifier.as_ref() {
                    notify(&snapshot);
                }
            }
        }
        Ok(SessionOrgApplyResponse { results, snapshot })
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn snapshot_of(state: &Loaded) -> SessionOrgSnapshot {
    match state {
        Loaded::Known(org) => SessionOrgSnapshot {
            status: SessionOrgStatus::Known,
            org: Some(org.clone()),
            reason: None,
            preserved_at: None,
        },
        Loaded::Unknown {
            reason,
            preserved_at,
        } => unknown_snapshot(reason.clone(), preserved_at.as_deref()),
    }
}

fn unknown_snapshot(reason: String, preserved_at: Option<&Path>) -> SessionOrgSnapshot {
    SessionOrgSnapshot {
        status: SessionOrgStatus::Unknown,
        org: None,
        reason: Some(reason),
        preserved_at: preserved_at.map(|p| p.display().to_string()),
    }
}

fn load(path: &Path) -> Loaded {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded::Known(SessionOrg::default())
        }
        Err(e) => {
            // Left in place: an I/O error is not evidence the bytes are bad.
            warn!(error = %e, path = %path.display(), "session_org: read failed — org is Unknown");
            return Loaded::Unknown {
                reason: format!("read failed: {e}"),
                preserved_at: Some(path.to_path_buf()),
            };
        }
    };
    // Both Unknown arms leave the file IN PLACE: moving it aside here would
    // let the next start find no file and load a known, empty org. The bytes
    // move aside only on an explicit `ResetAfterUnknown`.
    match serde_json::from_slice::<SessionOrg>(&bytes) {
        Ok(org) if org.version > SCHEMA_VERSION => {
            warn!(
                version = org.version,
                supported = SCHEMA_VERSION,
                path = %path.display(),
                "session_org: file written by a newer build — org is Unknown, file left in place"
            );
            Loaded::Unknown {
                reason: format!(
                    "written by a newer build (schema version {}, this build reads up to {SCHEMA_VERSION})",
                    org.version
                ),
                preserved_at: Some(path.to_path_buf()),
            }
        }
        Ok(org) => Loaded::Known(org),
        Err(e) => {
            warn!(error = %e, path = %path.display(), "session_org: unreadable file — org is Unknown, file left in place");
            Loaded::Unknown {
                reason: format!("unreadable file: {e}"),
                preserved_at: Some(path.to_path_buf()),
            }
        }
    }
}

fn corrupt_aside_path(path: &Path, now: i64) -> PathBuf {
    path.with_file_name(format!("session-org.corrupt-{now}.json"))
}

/// Drop members whose every field is at its default — they carry nothing.
fn prune_default_members(org: &mut SessionOrg) {
    org.members.retain(|_, m| !m.is_default());
}

/// Temp file + `sync_all` + rename: the bytes are durable BEFORE the rename
/// makes them the file, so a crash cannot leave a renamed-but-empty org.
fn write_atomic(path: &Path, org: &SessionOrg) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(org).map_err(std::io::Error::other)?;
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)
}

/// Trimmed, non-empty, bounded label — or `None`.
fn clean_label(raw: &str, max_chars: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max_chars {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Trimmed, non-empty session keys, deduplicated in first-seen order — or
/// `None` when any key is blank (a blank key is a caller bug, not a skip).
fn clean_keys(raw: &[String]) -> Option<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for key in raw {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        if seen.insert(key.to_string()) {
            out.push(key.to_string());
        }
    }
    Some(out)
}

fn group_exists(org: &SessionOrg, group_id: &str) -> bool {
    org.groups.iter().any(|g| g.id == group_id)
}

/// Members of `group_id` (`None` = Ungrouped) in display order.
fn ordered_members(org: &SessionOrg, group_id: Option<&str>) -> Vec<String> {
    let mut keys: Vec<(&String, i64)> = org
        .members
        .iter()
        .filter(|(_, m)| m.group_id.as_deref() == group_id)
        .map(|(k, m)| (k, m.order))
        .collect();
    keys.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    keys.into_iter().map(|(k, _)| k.clone()).collect()
}

/// Write `order = position` for every key in `keys`.
fn renumber(org: &mut SessionOrg, keys: &[String]) {
    for (i, key) in keys.iter().enumerate() {
        org.members.entry(key.clone()).or_default().order = i as i64;
    }
}

/// Move `keys` (already cleaned) to the end of `target` (`None` = Ungrouped),
/// keeping their relative order, and renumber both the source groups and the
/// target.
fn move_to_end(org: &mut SessionOrg, keys: &[String], target: Option<&str>) {
    let mut sources: BTreeSet<Option<String>> = BTreeSet::new();
    for key in keys {
        let member = org.members.entry(key.clone()).or_default();
        if member.group_id.as_deref() != target {
            sources.insert(member.group_id.clone());
        }
    }
    let mut target_order: Vec<String> = ordered_members(org, target)
        .into_iter()
        .filter(|k| !keys.contains(k))
        .collect();
    target_order.extend(keys.iter().cloned());
    for key in keys {
        org.members.entry(key.clone()).or_default().group_id = target.map(str::to_string);
    }
    renumber(org, &target_order);
    for source in sources {
        let order = ordered_members(org, source.as_deref());
        renumber(org, &order);
    }
}

fn next_color_role(org: &SessionOrg) -> ColorRole {
    let used: Vec<ColorRole> = org.groups.iter().map(|g| g.color_role).collect();
    ColorRole::ALL
        .iter()
        .copied()
        .find(|r| !used.contains(r))
        // All six in use: cycle by group count so colors still spread.
        .unwrap_or(ColorRole::ALL[org.groups.len() % ColorRole::ALL.len()])
}

fn renumber_groups(org: &mut SessionOrg) {
    org.groups.sort_by_key(|g| g.order);
    for (i, g) in org.groups.iter_mut().enumerate() {
        g.order = i as i64;
    }
}

/// Apply one op to `org`. Pure apart from `now`; refusals leave `org`
/// untouched.
fn apply_op(org: &mut SessionOrg, op: &SessionOrgOp, now: i64) -> SessionOrgOpResult {
    use SessionOrgOp as Op;
    match op {
        Op::CreateGroup {
            group_id,
            title,
            color_role,
            session_keys,
        } => {
            let Some(title) = clean_label(title, MAX_LABEL_CHARS) else {
                return SessionOrgOpResult::refused("invalid_title");
            };
            let Some(keys) = clean_keys(session_keys) else {
                return SessionOrgOpResult::refused("invalid_session_key");
            };
            let id = match group_id.as_deref().map(str::trim) {
                Some("") => return SessionOrgOpResult::refused("invalid_group_id"),
                Some(id) => id.to_string(),
                None => uuid::Uuid::new_v4().to_string(),
            };
            if group_exists(org, &id) {
                return SessionOrgOpResult::refused("group_exists");
            }
            let order = org.groups.iter().map(|g| g.order + 1).max().unwrap_or(0);
            let color_role = color_role.unwrap_or_else(|| next_color_role(org));
            org.groups.push(SessionGroup {
                id: id.clone(),
                title,
                color_role,
                collapsed: false,
                order,
                created_at: now,
            });
            move_to_end(org, &keys, Some(&id));
            SessionOrgOpResult::created(id)
        }
        Op::RenameGroup { group_id, title } => {
            let Some(title) = clean_label(title, MAX_LABEL_CHARS) else {
                return SessionOrgOpResult::refused("invalid_title");
            };
            match org.groups.iter_mut().find(|g| &g.id == group_id) {
                Some(g) => {
                    g.title = title;
                    SessionOrgOpResult::ok()
                }
                None => SessionOrgOpResult::refused("group_not_found"),
            }
        }
        Op::RecolorGroup {
            group_id,
            color_role,
        } => match org.groups.iter_mut().find(|g| &g.id == group_id) {
            Some(g) => {
                g.color_role = *color_role;
                SessionOrgOpResult::ok()
            }
            None => SessionOrgOpResult::refused("group_not_found"),
        },
        Op::SetGroupCollapsed {
            group_id,
            collapsed,
        } => match org.groups.iter_mut().find(|g| &g.id == group_id) {
            Some(g) => {
                g.collapsed = *collapsed;
                SessionOrgOpResult::ok()
            }
            None => SessionOrgOpResult::refused("group_not_found"),
        },
        Op::MoveGroup { group_id, index } => {
            renumber_groups(org);
            let Some(from) = org.groups.iter().position(|g| &g.id == group_id) else {
                return SessionOrgOpResult::refused("group_not_found");
            };
            let group = org.groups.remove(from);
            let to = (*index).min(org.groups.len());
            org.groups.insert(to, group);
            for (i, g) in org.groups.iter_mut().enumerate() {
                g.order = i as i64;
            }
            SessionOrgOpResult::ok()
        }
        Op::UngroupGroup { group_id } => {
            if !group_exists(org, group_id) {
                return SessionOrgOpResult::refused("group_not_found");
            }
            let members = ordered_members(org, Some(group_id));
            move_to_end(org, &members, None);
            SessionOrgOpResult::ok()
        }
        Op::DeleteGroup { group_id } => {
            if !group_exists(org, group_id) {
                return SessionOrgOpResult::refused("group_not_found");
            }
            let members = ordered_members(org, Some(group_id));
            move_to_end(org, &members, None);
            org.groups.retain(|g| &g.id != group_id);
            renumber_groups(org);
            SessionOrgOpResult::ok()
        }
        Op::AddMembers {
            group_id,
            session_keys,
        } => {
            if !group_exists(org, group_id) {
                return SessionOrgOpResult::refused("group_not_found");
            }
            let Some(keys) = clean_keys(session_keys) else {
                return SessionOrgOpResult::refused("invalid_session_key");
            };
            move_to_end(org, &keys, Some(group_id));
            SessionOrgOpResult::ok()
        }
        Op::RemoveMembers { session_keys } => {
            let Some(keys) = clean_keys(session_keys) else {
                return SessionOrgOpResult::refused("invalid_session_key");
            };
            let grouped: Vec<String> = keys
                .into_iter()
                .filter(|k| org.members.get(k).is_some_and(|m| m.group_id.is_some()))
                .collect();
            move_to_end(org, &grouped, None);
            SessionOrgOpResult::ok()
        }
        Op::MoveMember {
            session_key,
            group_id,
            index,
        } => {
            let Some(keys) = clean_keys(std::slice::from_ref(session_key)) else {
                return SessionOrgOpResult::refused("invalid_session_key");
            };
            let target = group_id.as_deref();
            if let Some(g) = target {
                if !group_exists(org, g) {
                    return SessionOrgOpResult::refused("group_not_found");
                }
            }
            move_to_end(org, &keys, target);
            let mut order = ordered_members(org, target);
            if let Some(pos) = order.iter().position(|k| k == &keys[0]) {
                let key = order.remove(pos);
                let to = (*index).min(order.len());
                order.insert(to, key);
            }
            renumber(org, &order);
            SessionOrgOpResult::ok()
        }
        Op::SetPinned {
            session_keys,
            pinned,
        } => for_each_member(org, session_keys, |m| m.pinned = *pinned),
        Op::AddTags { session_keys, tags } => {
            let Some(tags) = clean_tags(tags) else {
                return SessionOrgOpResult::refused("invalid_tag");
            };
            for_each_member(org, session_keys, |m| {
                let mut set: BTreeSet<String> = m.tags.drain(..).collect();
                set.extend(tags.iter().cloned());
                m.tags = set.into_iter().collect();
            })
        }
        Op::RemoveTags { session_keys, tags } => {
            let Some(tags) = clean_tags(tags) else {
                return SessionOrgOpResult::refused("invalid_tag");
            };
            for_each_member(org, session_keys, |m| m.tags.retain(|t| !tags.contains(t)))
        }
        Op::SetMuted {
            session_keys,
            muted,
        } => for_each_member(org, session_keys, |m| m.muted = *muted),
        Op::Snooze {
            session_keys,
            until,
        } => for_each_member(org, session_keys, |m| m.snoozed_until = *until),
        Op::MarkSeen { session_key, at } => {
            for_each_member(org, std::slice::from_ref(session_key), |m| {
                if m.last_seen_at.is_none_or(|prev| *at > prev) {
                    m.last_seen_at = Some(*at);
                }
            })
        }
        Op::RecordClosed {
            session_key,
            title,
            closed_at,
            resume_cmd,
        } => {
            let Some(keys) = clean_keys(std::slice::from_ref(session_key)) else {
                return SessionOrgOpResult::refused("invalid_session_key");
            };
            let group_id = org.members.get(&keys[0]).and_then(|m| m.group_id.clone());
            org.recently_closed.insert(
                0,
                ClosedSession {
                    session_key: keys[0].clone(),
                    title: title.trim().to_string(),
                    closed_at: *closed_at,
                    resume_cmd: resume_cmd.clone(),
                    group_id,
                },
            );
            org.recently_closed.truncate(RECENTLY_CLOSED_CAP);
            org.recently_closed_total += 1;
            SessionOrgOpResult::ok()
        }
        Op::SaveView { name, query } => {
            let Some(name) = clean_label(name, MAX_LABEL_CHARS) else {
                return SessionOrgOpResult::refused("invalid_view_name");
            };
            if query.chars().count() > MAX_QUERY_CHARS {
                return SessionOrgOpResult::refused("query_too_long");
            }
            let query = query.trim().to_string();
            match org.saved_views.iter_mut().find(|v| v.name == name) {
                Some(view) => {
                    view.query = query;
                    view.saved_at = now;
                }
                None => org.saved_views.push(SavedView {
                    name,
                    query,
                    saved_at: now,
                }),
            }
            SessionOrgOpResult::ok()
        }
        Op::DeleteView { name } => {
            let before = org.saved_views.len();
            org.saved_views.retain(|v| v.name != name.trim());
            if org.saved_views.len() == before {
                SessionOrgOpResult::refused("view_not_found")
            } else {
                SessionOrgOpResult::ok()
            }
        }
        Op::DismissSuggestion { key } => match clean_label(key, MAX_QUERY_CHARS) {
            Some(key) => {
                org.dismissed_suggestions.insert(key);
                SessionOrgOpResult::ok()
            }
            None => SessionOrgOpResult::refused("invalid_suggestion_key"),
        },
        Op::ResetAfterUnknown => SessionOrgOpResult::ok(),
    }
}

fn clean_tags(raw: &[String]) -> Option<Vec<String>> {
    raw.iter()
        .map(|t| clean_label(t, MAX_LABEL_CHARS))
        .collect::<Option<Vec<_>>>()
}

fn for_each_member(
    org: &mut SessionOrg,
    session_keys: &[String],
    mut f: impl FnMut(&mut MemberOrg),
) -> SessionOrgOpResult {
    let Some(keys) = clean_keys(session_keys) else {
        return SessionOrgOpResult::refused("invalid_session_key");
    };
    for key in keys {
        f(org.members.entry(key).or_default());
    }
    SessionOrgOpResult::ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::tempdir;

    const T0: i64 = 1_760_000_000_000;

    fn store_in(dir: &Path) -> SessionOrgStore {
        SessionOrgStore::open(dir.join(STORE_FILE_NAME)).unwrap()
    }

    fn create(id: &str, title: &str, keys: &[&str]) -> SessionOrgOp {
        SessionOrgOp::CreateGroup {
            group_id: Some(id.into()),
            title: title.into(),
            color_role: None,
            session_keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn keys(ks: &[&str]) -> Vec<String> {
        ks.iter().map(|k| k.to_string()).collect()
    }

    fn known(store: &SessionOrgStore) -> SessionOrg {
        let snap = store.snapshot();
        assert_eq!(snap.status, SessionOrgStatus::Known, "{snap:?}");
        snap.org.unwrap()
    }

    #[test]
    fn session_org_missing_file_is_a_known_empty_org() {
        let dir = tempdir().unwrap();
        let org = known(&store_in(dir.path()));
        assert_eq!(org, SessionOrg::default());
    }

    #[test]
    fn session_org_key_prefers_claude_id_and_pins_the_pane_spelling() {
        let pane = PaneKey::from_create_in_window("p", "Terminal 1", "/w", "term-2");
        assert_eq!(session_key(Some(" abc "), &pane), "abc");
        // Pinned literal — `sessionOrgTypes.test.ts` asserts the same string.
        assert_eq!(
            session_key(None, &pane),
            "pane:p\u{1f}Terminal 1\u{1f}/w\u{1f}term-2"
        );
        assert_eq!(session_key(Some("  "), &pane), session_key(None, &pane));
        let main = PaneKey::from_create_in_window("p", "T", "/w", "main");
        assert_eq!(session_key(None, &main), "pane:p\u{1f}T\u{1f}/w");
    }

    #[test]
    fn session_org_apply_persists_and_survives_reopen() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let resp = store
            .apply_at(
                &[
                    create("g1", "Coord work", &["s1", "s2"]),
                    SessionOrgOp::SetPinned {
                        session_keys: keys(&["s3"]),
                        pinned: true,
                    },
                ],
                T0,
            )
            .unwrap();
        assert!(resp.results.iter().all(|r| r.ok), "{:?}", resp.results);
        assert_eq!(resp.results[0].group_id.as_deref(), Some("g1"));
        let org = known(&store_in(dir.path()));
        assert_eq!(org.groups.len(), 1);
        assert_eq!(org.groups[0].color_role, ColorRole::BrandPrimary);
        assert_eq!(org.groups[0].created_at, T0);
        assert_eq!(org.members["s1"].group_id.as_deref(), Some("g1"));
        assert_eq!(org.members["s2"].order, 1);
        assert!(org.members["s3"].pinned);
        assert_eq!(org.revision, 1);
    }

    /// Enumerate: concurrent apply from two windows. Both windows read the
    /// org at revision N, then each applies an op; neither op carries the
    /// other's fields, so both effects survive regardless of order — and
    /// truly concurrent threads on one store never lose an op.
    #[test]
    fn session_org_concurrent_applies_from_two_windows_merge_per_op() {
        let dir = tempdir().unwrap();
        let store = Arc::new(store_in(dir.path()));
        store.apply_at(&[create("g1", "A", &["s1"])], T0).unwrap();

        // Window A renames; window B, holding the same stale revision, pins
        // and tags the same member.
        store
            .apply_at(
                &[SessionOrgOp::RenameGroup {
                    group_id: "g1".into(),
                    title: "Renamed".into(),
                }],
                T0 + 1,
            )
            .unwrap();
        store
            .apply_at(
                &[
                    SessionOrgOp::SetPinned {
                        session_keys: keys(&["s1"]),
                        pinned: true,
                    },
                    SessionOrgOp::AddTags {
                        session_keys: keys(&["s1"]),
                        tags: keys(&["review"]),
                    },
                ],
                T0 + 2,
            )
            .unwrap();
        let org = known(&store);
        assert_eq!(org.groups[0].title, "Renamed");
        assert!(org.members["s1"].pinned);
        assert_eq!(org.members["s1"].group_id.as_deref(), Some("g1"));
        assert_eq!(org.members["s1"].tags, keys(&["review"]));

        // Real threads: 2 windows × 25 pins each, every one lands.
        let handles: Vec<_> = (0..2)
            .map(|w| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for i in 0..25 {
                        store
                            .apply(&[SessionOrgOp::SetPinned {
                                session_keys: vec![format!("w{w}-{i}")],
                                pinned: true,
                            }])
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let org = known(&store_in(dir.path()));
        let pinned = org.members.values().filter(|m| m.pinned).count();
        assert_eq!(pinned, 51, "1 earlier pin + 50 concurrent pins");
        assert_eq!(org.groups[0].title, "Renamed");
    }

    /// Enumerate: a corrupt file loads Unknown, stays in place byte for byte
    /// (so a restart reads it as Unknown again, never as an empty org), refuses
    /// every op (including an automatic markSeen), and only an explicit reset
    /// moves it aside and starts a new org.
    #[test]
    fn session_org_corrupt_file_is_unknown_preserved_and_never_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(STORE_FILE_NAME);
        let bad: &[u8] = b"{\"groups\": [ not json";
        std::fs::write(&path, bad).unwrap();
        let store = SessionOrgStore::open(&path).unwrap();

        let snap = store.snapshot();
        assert_eq!(snap.status, SessionOrgStatus::Unknown);
        assert!(
            snap.org.is_none(),
            "an unreadable org must not render as empty"
        );
        assert_eq!(
            snap.preserved_at.as_deref(),
            Some(path.display().to_string().as_str())
        );
        assert_eq!(std::fs::read(&path).unwrap(), bad);

        // A restart before anyone resets reads the same bytes as Unknown.
        let restarted = SessionOrgStore::open(&path).unwrap();
        assert_eq!(restarted.snapshot().status, SessionOrgStatus::Unknown);
        drop(restarted);

        let resp = store
            .apply_at(
                &[SessionOrgOp::MarkSeen {
                    session_key: "s1".into(),
                    at: T0,
                }],
                T0,
            )
            .unwrap();
        assert_eq!(resp.results[0].error.as_deref(), Some("org_unknown"));
        assert_eq!(resp.snapshot.status, SessionOrgStatus::Unknown);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bad,
            "a refused op must not write a fresh file"
        );

        let resp = store
            .apply_at(
                &[
                    SessionOrgOp::ResetAfterUnknown,
                    create("g1", "Fresh", &["s1"]),
                ],
                T0,
            )
            .unwrap();
        assert!(resp.results.iter().all(|r| r.ok));
        assert_eq!(resp.snapshot.status, SessionOrgStatus::Known);
        assert_eq!(known(&store_in(dir.path())).groups.len(), 1);
        let aside = corrupt_aside_path(&path, T0);
        assert_eq!(
            std::fs::read(&aside).unwrap(),
            bad,
            "the reset preserves the unreadable bytes aside"
        );
    }

    /// A file written by a newer build (a higher schema version) is Unknown and
    /// left in place: an older build would drop the fields it does not know on
    /// its first write.
    #[test]
    fn session_org_newer_schema_version_is_unknown_and_left_in_place() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(STORE_FILE_NAME);
        let mut newer = serde_json::to_value(SessionOrg::default()).unwrap();
        newer["version"] = serde_json::json!(SCHEMA_VERSION + 1);
        newer["futureField"] = serde_json::json!({"keep": true});
        let bytes = serde_json::to_vec(&newer).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let store = SessionOrgStore::open(&path).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.status, SessionOrgStatus::Unknown);
        assert!(snap.org.is_none());
        assert!(snap.reason.unwrap().contains("newer build"));
        let resp = store
            .apply_at(
                &[SessionOrgOp::MarkSeen {
                    session_key: "s1".into(),
                    at: T0,
                }],
                T0,
            )
            .unwrap();
        assert_eq!(resp.results[0].error.as_deref(), Some("org_unknown"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// A read error (here: a directory where the file should be) is Unknown
    /// with the entry left in place; the reset moves it aside before writing.
    #[test]
    fn session_org_read_error_is_unknown_and_reset_preserves_in_place_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(STORE_FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("marker"), b"keep").unwrap();
        let store = SessionOrgStore::open(&path).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.status, SessionOrgStatus::Unknown);
        assert_eq!(
            snap.preserved_at.as_deref(),
            Some(path.display().to_string().as_str())
        );

        let resp = store
            .apply_at(&[SessionOrgOp::ResetAfterUnknown], T0)
            .unwrap();
        assert!(resp.results[0].ok, "{:?}", resp.results);
        let aside = corrupt_aside_path(&path, T0);
        assert_eq!(std::fs::read(aside.join("marker")).unwrap(), b"keep");
        assert!(path.is_file(), "fresh org written");
        assert_eq!(known(&store_in(dir.path())).revision, 1);
    }

    /// A no-op apply is a no-op: `setPinned false` (and `unmute`, `snooze`
    /// clear, `removeTags`) on a session that is not a member leaves the
    /// revision alone, writes no file and emits no change event.
    #[test]
    fn session_org_a_no_op_on_a_non_member_changes_nothing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(STORE_FILE_NAME);
        let store = store_in(dir.path());
        let fired = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fired);
        store.set_notifier(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let no_ops = [
            SessionOrgOp::SetPinned {
                session_keys: keys(&["ghost"]),
                pinned: false,
            },
            SessionOrgOp::SetMuted {
                session_keys: keys(&["ghost"]),
                muted: false,
            },
            SessionOrgOp::Snooze {
                session_keys: keys(&["ghost"]),
                until: None,
            },
            SessionOrgOp::RemoveTags {
                session_keys: keys(&["ghost"]),
                tags: keys(&["x"]),
            },
        ];
        let resp = store.apply_at(&no_ops, T0).unwrap();
        assert!(resp.results.iter().all(|r| r.ok), "{:?}", resp.results);
        assert_eq!(resp.snapshot.org.as_ref().unwrap().revision, 0);
        assert!(!path.exists(), "a no-op must not write the file");
        assert_eq!(fired.load(Ordering::SeqCst), 0, "no change event");

        // Same on an org that already has state on disk.
        store
            .apply_at(
                &[SessionOrgOp::SetPinned {
                    session_keys: keys(&["real"]),
                    pinned: true,
                }],
                T0,
            )
            .unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        let resp = store.apply_at(&no_ops, T0 + 1).unwrap();
        assert_eq!(resp.snapshot.org.as_ref().unwrap().revision, 1);
        assert_eq!(std::fs::read(&path).unwrap(), written);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    /// The reset moves an in-place unreadable file aside, then its persist
    /// fails: the org stays Unknown and `preservedAt` names the aside path —
    /// where the bytes now are — not the path they left.
    #[test]
    fn session_org_failed_persist_after_reset_reports_where_the_bytes_are() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(STORE_FILE_NAME);
        std::fs::write(&path, b"not json").unwrap();
        let store = SessionOrgStore::open(&path).unwrap();
        // The bytes reappear in place (e.g. restored by hand) so the reset
        // has a file to move.
        std::fs::write(&path, b"second copy").unwrap();
        // A directory at the temp path makes the persist fail.
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();

        let reset_at = T0 + 7;
        let err = store
            .apply_at(&[SessionOrgOp::ResetAfterUnknown], reset_at)
            .unwrap_err();
        assert!(err.contains("persist failed"), "{err}");
        let aside = corrupt_aside_path(&path, reset_at);
        assert_eq!(std::fs::read(&aside).unwrap(), b"second copy");
        assert!(!path.exists());
        let snap = store.snapshot();
        assert_eq!(snap.status, SessionOrgStatus::Unknown);
        assert_eq!(
            snap.preserved_at.as_deref(),
            Some(aside.display().to_string().as_str()),
            "preservedAt must name where the bytes actually are"
        );
    }

    /// Enumerate: deleting a group with members ungroups them — every member
    /// keeps its pins and tags, and lands after the existing ungrouped rows.
    #[test]
    fn session_org_delete_group_with_members_ungroups_not_loses() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .apply_at(
                &[
                    SessionOrgOp::SetPinned {
                        session_keys: keys(&["loose"]),
                        pinned: true,
                    },
                    create("g1", "A", &["s1", "s2"]),
                    SessionOrgOp::AddTags {
                        session_keys: keys(&["s2"]),
                        tags: keys(&["keep"]),
                    },
                    create("g2", "B", &["s3"]),
                ],
                T0,
            )
            .unwrap();
        let resp = store
            .apply_at(
                &[SessionOrgOp::DeleteGroup {
                    group_id: "g1".into(),
                }],
                T0,
            )
            .unwrap();
        assert!(resp.results[0].ok);
        let org = known(&store);
        assert_eq!(org.groups.len(), 1);
        assert_eq!(org.groups[0].id, "g2");
        assert_eq!(org.groups[0].order, 0, "group orders renumbered");
        for k in ["s1", "s2"] {
            assert_eq!(org.members[k].group_id, None, "{k} ungrouped");
        }
        assert_eq!(org.members["s2"].tags, keys(&["keep"]));
        assert_eq!(org.members["s3"].group_id.as_deref(), Some("g2"));
        assert_eq!(
            ordered_members(&org, None),
            keys(&["loose", "s1", "s2"]),
            "former members append after existing ungrouped rows, in order"
        );
    }

    #[test]
    fn session_org_ungroup_keeps_the_empty_group() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store.apply_at(&[create("g1", "A", &["s1"])], T0).unwrap();
        store
            .apply_at(
                &[SessionOrgOp::UngroupGroup {
                    group_id: "g1".into(),
                }],
                T0,
            )
            .unwrap();
        let org = known(&store);
        assert_eq!(org.groups.len(), 1);
        assert!(!org.members.contains_key("s1"), "default member pruned");
    }

    #[test]
    fn session_org_refusals_name_their_cause_and_change_nothing() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let resp = store
            .apply_at(
                &[
                    SessionOrgOp::RenameGroup {
                        group_id: "nope".into(),
                        title: "x".into(),
                    },
                    create("g1", "   ", &[]),
                    SessionOrgOp::SetPinned {
                        session_keys: keys(&["ok", " "]),
                        pinned: true,
                    },
                    SessionOrgOp::DeleteView { name: "v".into() },
                    create("g1", "A", &[]),
                    create("g1", "dup", &[]),
                ],
                T0,
            )
            .unwrap();
        let codes: Vec<Option<&str>> = resp.results.iter().map(|r| r.error.as_deref()).collect();
        assert_eq!(
            codes,
            vec![
                Some("group_not_found"),
                Some("invalid_title"),
                Some("invalid_session_key"),
                Some("view_not_found"),
                None,
                Some("group_exists"),
            ]
        );
        let org = known(&store);
        assert_eq!(org.groups.len(), 1);
        assert!(org.members.is_empty(), "the refused pin wrote no member");
    }

    #[test]
    fn session_org_move_member_and_group_reorder() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .apply_at(
                &[
                    create("g1", "A", &["a", "b", "c"]),
                    create("g2", "B", &[]),
                    create("g3", "C", &[]),
                ],
                T0,
            )
            .unwrap();
        store
            .apply_at(
                &[
                    SessionOrgOp::MoveMember {
                        session_key: "c".into(),
                        group_id: Some("g1".into()),
                        index: 0,
                    },
                    SessionOrgOp::MoveMember {
                        session_key: "a".into(),
                        group_id: Some("g2".into()),
                        index: 99,
                    },
                    SessionOrgOp::MoveGroup {
                        group_id: "g3".into(),
                        index: 0,
                    },
                ],
                T0,
            )
            .unwrap();
        let org = known(&store);
        assert_eq!(ordered_members(&org, Some("g1")), keys(&["c", "b"]));
        assert_eq!(ordered_members(&org, Some("g2")), keys(&["a"]));
        let mut groups = org.groups.clone();
        groups.sort_by_key(|g| g.order);
        let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["g3", "g1", "g2"]);
    }

    #[test]
    fn session_org_colors_take_the_next_unused_role() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let ops: Vec<SessionOrgOp> = (0..7).map(|i| create(&format!("g{i}"), "G", &[])).collect();
        store.apply_at(&ops, T0).unwrap();
        let org = known(&store);
        let roles: Vec<ColorRole> = org.groups.iter().map(|g| g.color_role).collect();
        assert_eq!(&roles[..6], &ColorRole::ALL[..]);
        assert_eq!(roles[6], ColorRole::ALL[0]);
        // The wire spelling is the role NAME, never a color value.
        assert_eq!(
            serde_json::to_value(ColorRole::BrandSuccess).unwrap(),
            "brand-success"
        );
        assert!(serde_json::from_value::<ColorRole>(serde_json::json!("red")).is_err());
    }

    #[test]
    fn session_org_mark_seen_is_monotonic_and_mute_snooze_set_fields() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .apply_at(
                &[
                    SessionOrgOp::MarkSeen {
                        session_key: "s".into(),
                        at: T0 + 10,
                    },
                    SessionOrgOp::MarkSeen {
                        session_key: "s".into(),
                        at: T0,
                    },
                    SessionOrgOp::SetMuted {
                        session_keys: keys(&["s"]),
                        muted: true,
                    },
                    SessionOrgOp::Snooze {
                        session_keys: keys(&["s"]),
                        until: Some(T0 + 99),
                    },
                ],
                T0,
            )
            .unwrap();
        let m = known(&store).members["s"].clone();
        assert_eq!(m.last_seen_at, Some(T0 + 10));
        assert!(m.muted);
        assert_eq!(m.snoozed_until, Some(T0 + 99));
    }

    #[test]
    fn session_org_recently_closed_is_bounded_and_reports_total() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store.apply_at(&[create("g1", "A", &["s0"])], T0).unwrap();
        let ops: Vec<SessionOrgOp> = (0..RECENTLY_CLOSED_CAP + 5)
            .map(|i| SessionOrgOp::RecordClosed {
                session_key: format!("s{i}"),
                title: format!("t{i}"),
                closed_at: T0 + i as i64,
                resume_cmd: Some(format!("claude --resume s{i}")),
            })
            .collect();
        store.apply_at(&ops, T0).unwrap();
        let org = known(&store);
        assert_eq!(org.recently_closed.len(), RECENTLY_CLOSED_CAP);
        assert_eq!(org.recently_closed_total, (RECENTLY_CLOSED_CAP + 5) as u64);
        assert_eq!(
            org.recently_closed[0].session_key,
            format!("s{}", RECENTLY_CLOSED_CAP + 4)
        );
        let oldest_kept = org.recently_closed.last().unwrap();
        assert_eq!(oldest_kept.session_key, "s5", "the 5 oldest were trimmed");
    }

    #[test]
    fn session_org_close_records_group_membership() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .apply_at(
                &[
                    create("g1", "A", &["s1"]),
                    SessionOrgOp::RecordClosed {
                        session_key: "s1".into(),
                        title: "One".into(),
                        closed_at: T0,
                        resume_cmd: None,
                    },
                ],
                T0,
            )
            .unwrap();
        let org = known(&store);
        assert_eq!(org.recently_closed[0].group_id.as_deref(), Some("g1"));
    }

    #[test]
    fn session_org_saved_views_and_dismissed_suggestions() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .apply_at(
                &[
                    SessionOrgOp::SaveView {
                        name: "Needs me".into(),
                        query: "is:needs-you".into(),
                    },
                    SessionOrgOp::SaveView {
                        name: "Needs me".into(),
                        query: "is:needs-you -is:muted".into(),
                    },
                    SessionOrgOp::SaveView {
                        name: "Coord".into(),
                        query: "repo:qontinui-coord".into(),
                    },
                    SessionOrgOp::DeleteView {
                        name: "Coord".into(),
                    },
                    SessionOrgOp::DismissSuggestion {
                        key: "plan:2026-10-13-x".into(),
                    },
                ],
                T0,
            )
            .unwrap();
        let org = known(&store);
        assert_eq!(org.saved_views.len(), 1);
        assert_eq!(org.saved_views[0].query, "is:needs-you -is:muted");
        assert!(org.dismissed_suggestions.contains("plan:2026-10-13-x"));
    }

    #[test]
    fn session_org_notifier_fires_once_per_changing_apply_only() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        store.set_notifier(Box::new(move |snap| {
            assert_eq!(snap.status, SessionOrgStatus::Known);
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        store
            .apply_at(&[create("g1", "A", &[]), create("g2", "B", &[])], T0)
            .unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        // A refused-only apply changes nothing and emits nothing.
        let resp = store
            .apply_at(
                &[SessionOrgOp::DeleteGroup {
                    group_id: "missing".into(),
                }],
                T0,
            )
            .unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(resp.snapshot.org.unwrap().revision, 1);
    }

    #[test]
    fn session_org_ops_deserialize_from_the_wire_spelling() {
        let ops: Vec<SessionOrgOp> = serde_json::from_value(serde_json::json!([
            {"op": "createGroup", "title": "A", "colorRole": "info", "sessionKeys": ["s1"]},
            {"op": "setPinned", "sessionKeys": ["s1"], "pinned": true},
            {"op": "moveMember", "sessionKey": "s1", "groupId": null, "index": 0},
            {"op": "resetAfterUnknown"}
        ]))
        .unwrap();
        assert_eq!(ops.len(), 4);
        assert!(matches!(
            &ops[0],
            SessionOrgOp::CreateGroup {
                color_role: Some(ColorRole::Info),
                group_id: None,
                ..
            }
        ));
        let bad = serde_json::from_value::<SessionOrgOp>(serde_json::json!({"op": "nuke"}));
        assert!(bad.is_err(), "an unknown op is refused, not ignored");
    }
}

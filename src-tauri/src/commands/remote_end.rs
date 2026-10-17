//! END a session on another device — the SOURCE side of plan
//! `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 3.
//!
//! Closing a remote tab only DETACHES (`RemotePaneIo::kill`); this is the
//! separate, named action (D1). `remote_session_end {device_id, session_id,
//! force}` sends `remote_terminal_end` through the backend relay under an
//! attach grant — the authority the user already holds over that session
//! (D3) — and returns the target's typed outcome.
//!
//! Which grant:
//! - an OPEN tab onto that session already holds one, attached on this relay
//!   socket — reuse it (no mint, no second grant), sent as `grant_jti`;
//! - otherwise mint a fresh one through the same coord door attach uses
//!   (`POST /coord/sessions/{id}/attach-grants`), sent as the `grant` JWT.
//!
//! An open tab's grant the relay reports stale (expired, no longer
//! registered) falls back to a mint once — the tab's grant lives 15 minutes
//! and a tab can outlive it.
//!
//! Honesty rule: a timeout or transport failure is `unknown`, NEVER `ended`.

use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use tracing::{info, warn};

use super::remote_attach::{coord_base_for, coord_places_session_on, AttachGrantResponse};
use crate::mcp::remote_terminal::{client, EndGrant, EndOutcome, EndReply, END_TIMEOUT};
use crate::terminal::types::RemoteTabIdentity;
use crate::terminal::TerminalManager;

/// Where the grant an end was sent under came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndGrantSource {
    /// An open tab's grant, already bound on this socket.
    OpenTab,
    /// A grant minted for this end.
    Minted,
    /// No grant was sent — the mint itself failed or was refused.
    None,
}

/// What `remote_session_end` returns. `outcome` is the wire vocabulary
/// (`ended` | `refused` | `still_running` | `unknown` | `not_found`); `via`
/// says how an `ended` ended (`graceful` | `no_live_claude` | `force`),
/// `reason` why anything else did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionEndResult {
    pub outcome: EndOutcome,
    pub device_id: String,
    pub session_id: String,
    /// The TARGET's terminal id, when the target reported one.
    pub terminal_id: Option<String>,
    pub via: Option<String>,
    pub reason: Option<String>,
    pub grant_source: EndGrantSource,
}

/// One remote tab this runner holds, as the grant selector sees it.
#[derive(Debug, Clone)]
pub(crate) struct OpenTabCandidate {
    pub identity: RemoteTabIdentity,
    /// False once the pane has settled (exited / errored / detached) or when
    /// the tab has no pane at all.
    pub live: bool,
}

/// The grant an open tab lends to an end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenTabGrant {
    pub grant_jti: String,
    pub remote_terminal_id: String,
}

/// The grant a `remote_terminal_end` is sent under, owned — the injected
/// sender's argument (see [`EndGrant`] for the wire shape of each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendGrant {
    OpenTab {
        grant_jti: String,
        terminal_id: String,
    },
    Fresh {
        grant: String,
    },
}

/// Pick an open, live tab onto `(device_id, session_id)` whose grant can carry
/// the end. Device ids compare trimmed and case-insensitively (coord's
/// spelling may differ in case from the picker's); session ids compare as
/// uuids. A dead tab never lends its grant — the relay dropped its attachment.
pub(crate) fn select_open_tab_grant<I>(
    candidates: I,
    device_id: &str,
    session_id: uuid::Uuid,
) -> Option<OpenTabGrant>
where
    I: IntoIterator<Item = OpenTabCandidate>,
{
    let device = device_id.trim();
    candidates.into_iter().find_map(|c| {
        let same_device = c.identity.device_id.trim().eq_ignore_ascii_case(device);
        let same_session =
            uuid::Uuid::parse_str(c.identity.session_id.trim()).is_ok_and(|s| s == session_id);
        (c.live && same_device && same_session && !c.identity.grant_jti.trim().is_empty()).then(
            || OpenTabGrant {
                grant_jti: c.identity.grant_jti.clone(),
                remote_terminal_id: c.identity.remote_terminal_id.clone(),
            },
        )
    })
}

/// Relay refusal codes that mean "this grant can no longer carry anything on
/// this socket" — an open tab's grant outlived, or lost, its attachment. Only
/// these send an end that reused a tab's grant back through a mint; every
/// other refusal (a target's mismatch, the device preference) is final.
const STALE_GRANT_CODES: &[&str] = &[
    "attach_grant_expired",
    "attach_grant_invalid",
    "attach_not_registered",
    "attach_grant_consumed",
];

/// Did the end under a reused tab grant fail only because that grant is stale?
pub(crate) fn reused_grant_is_stale(reply: &EndReply) -> bool {
    reply.outcome == EndOutcome::Refused
        && reply.reason.as_deref().is_some_and(|r| {
            let code = r.split(':').next().unwrap_or("").trim();
            STALE_GRANT_CODES.contains(&code)
        })
}

/// A mint that failed sent nothing to the target. Coord saying the session
/// does not exist is `not_found`; a transport failure, a coord 5xx or a rate
/// limit (bulk close mints per session) is `unknown`; any other coord answer
/// is a refusal carrying coord's code.
pub(crate) fn mint_error_outcome(err: &str) -> EndOutcome {
    if err.starts_with("remote_attach:session_not_found") {
        EndOutcome::NotFound
    } else if err.starts_with("remote_attach:coord_unreachable")
        || err.starts_with("remote_attach:coord_client_unavailable")
        || err.starts_with("remote_attach:coord_parse")
        || err.contains("coord answered 429")
        || err.contains("coord answered 5")
    {
        EndOutcome::Unknown
    } else {
        EndOutcome::Refused
    }
}

/// The open-tab candidates `tm` holds right now.
fn open_tab_candidates(tm: &TerminalManager) -> Vec<OpenTabCandidate> {
    tm.remote_identities()
        .into_iter()
        .map(|(local_id, identity)| {
            let live = tm.remote_pane(&local_id).is_some_and(|p| !p.is_finished());
            OpenTabCandidate { identity, live }
        })
        .collect()
}

fn result_from(
    reply: EndReply,
    device_id: &str,
    session_id: uuid::Uuid,
    fallback_terminal: Option<String>,
    grant_source: EndGrantSource,
) -> RemoteSessionEndResult {
    RemoteSessionEndResult {
        outcome: reply.outcome,
        device_id: device_id.trim().to_string(),
        session_id: reply.session_id.unwrap_or_else(|| session_id.to_string()),
        terminal_id: reply.terminal_id.or(fallback_terminal),
        via: reply.via,
        reason: reply.reason,
        grant_source,
    }
}

/// The whole decision, with the mint and the send injected so grant reuse,
/// the stale-grant fallback and every mint failure are unit-testable without
/// coord or a relay. `send_end(grant)` is `RemoteAttachClient::end`; `mint()`
/// is the coord mint.
pub(crate) async fn end_remote_session_with<M, MF, S, SF>(
    candidates: Vec<OpenTabCandidate>,
    device_id: &str,
    session_id: uuid::Uuid,
    mint: M,
    send_end: S,
) -> RemoteSessionEndResult
where
    M: FnOnce() -> MF,
    MF: Future<Output = Result<AttachGrantResponse, String>>,
    S: Fn(SendGrant) -> SF,
    SF: Future<Output = EndReply>,
{
    if let Some(tab) = select_open_tab_grant(candidates, device_id, session_id) {
        let reply = send_end(SendGrant::OpenTab {
            grant_jti: tab.grant_jti.clone(),
            terminal_id: tab.remote_terminal_id.clone(),
        })
        .await;
        if !reused_grant_is_stale(&reply) {
            return result_from(
                reply,
                device_id,
                session_id,
                Some(tab.remote_terminal_id),
                EndGrantSource::OpenTab,
            );
        }
        info!(
            session = %session_id,
            reason = ?reply.reason,
            "remote end: the open tab's grant is stale — minting a fresh one"
        );
    }

    let minted = match mint().await {
        Ok(minted) => minted,
        Err(e) => {
            warn!(session = %session_id, error = %e, "remote end: grant mint failed");
            return result_from(
                EndReply::local(mint_error_outcome(&e), e),
                device_id,
                session_id,
                None,
                EndGrantSource::None,
            );
        }
    };
    if !coord_places_session_on(device_id, minted.target_device_id.as_deref()) {
        let target = minted.target_device_id.as_deref().unwrap_or("<unreported>");
        return result_from(
            EndReply::local(
                EndOutcome::Refused,
                format!(
                    "target_mismatch: coord places session {session_id} on device {target}, not \
                     {} — refresh the fleet list",
                    device_id.trim()
                ),
            ),
            device_id,
            session_id,
            None,
            EndGrantSource::Minted,
        );
    }
    let reply = send_end(SendGrant::Fresh {
        grant: minted.grant,
    })
    .await;
    result_from(reply, device_id, session_id, None, EndGrantSource::Minted)
}

/// The shared body of the Tauri command and the `/ui-bridge/tauri/invoke`
/// door. `Err` only for an argument that is not a session uuid.
pub(crate) async fn end_remote_session(
    tm: &Arc<TerminalManager>,
    coord_base: &str,
    device_id: &str,
    session_id: &str,
    force: bool,
) -> Result<RemoteSessionEndResult, String> {
    let session_uuid = uuid::Uuid::parse_str(session_id.trim()).map_err(|e| {
        format!("remote_end:invalid_session_id: {session_id:?} is not a session uuid: {e}")
    })?;
    info!(
        device = %device_id,
        session = %session_uuid,
        force,
        "remote end: requested"
    );
    let result = end_remote_session_with(
        open_tab_candidates(tm),
        device_id,
        session_uuid,
        || super::remote_attach::mint_attach_grant(coord_base, session_uuid),
        |grant| async move {
            let wire = match &grant {
                SendGrant::OpenTab {
                    grant_jti,
                    terminal_id,
                } => EndGrant::Attached {
                    grant_jti,
                    terminal_id: Some(terminal_id),
                },
                SendGrant::Fresh { grant } => EndGrant::Fresh { grant },
            };
            client().end(wire, force, END_TIMEOUT).await
        },
    )
    .await;
    info!(
        device = %device_id,
        session = %session_uuid,
        outcome = result.outcome.as_str(),
        via = ?result.via,
        reason = ?result.reason,
        grant_source = ?result.grant_source,
        "remote end: answered"
    );
    Ok(result)
}

/// END a session running on another device (plan
/// `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 3).
///
/// `force: false` (the default) asks the target for a graceful `/exit`, which
/// it refuses at a busy prompt or an unsent draft; `force: true` is a hard
/// close. The returned `outcome` is never `ended` unless the target said so.
#[tauri::command]
pub async fn remote_session_end(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    device_id: String,
    session_id: String,
    force: Option<bool>,
) -> Result<RemoteSessionEndResult, String> {
    let base = coord_base_for(&app_handle);
    end_remote_session(
        terminal_manager.inner(),
        &base,
        &device_id,
        &session_id,
        force.unwrap_or(false),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const SID: uuid::Uuid = uuid::Uuid::from_u128(0x5e55_1011);

    fn identity(device: &str, session: &str, jti: &str) -> RemoteTabIdentity {
        RemoteTabIdentity {
            device_id: device.to_string(),
            device_label: "dev".to_string(),
            session_id: session.to_string(),
            remote_terminal_id: "remote-term-1".to_string(),
            grant_jti: jti.to_string(),
            history_available: false,
        }
    }

    fn tab(device: &str, session: &str, live: bool) -> OpenTabCandidate {
        OpenTabCandidate {
            identity: identity(device, session, "tab-jti"),
            live,
        }
    }

    fn minted(target: &str) -> AttachGrantResponse {
        serde_json::from_value(serde_json::json!({
            "grant": "minted-grant-jwt",
            "grant_jti": "minted-jti",
            "target_device_id": target,
        }))
        .expect("grant response")
    }

    fn ended(via: &str) -> EndReply {
        EndReply {
            outcome: EndOutcome::Ended,
            session_id: Some(SID.to_string()),
            terminal_id: Some("remote-term-1".to_string()),
            via: Some(via.to_string()),
            reason: None,
        }
    }

    /// Records each `send_end` call and answers from a queue.
    struct Sender {
        calls: Mutex<Vec<SendGrant>>,
        replies: Mutex<Vec<EndReply>>,
    }

    impl Sender {
        fn new(mut replies: Vec<EndReply>) -> Self {
            replies.reverse();
            Self {
                calls: Mutex::new(Vec::new()),
                replies: Mutex::new(replies),
            }
        }
        fn send(&self, grant: SendGrant) -> impl Future<Output = EndReply> {
            self.calls.lock().unwrap().push(grant);
            let reply = self.replies.lock().unwrap().pop().expect("an answer");
            async move { reply }
        }
        fn calls(&self) -> Vec<SendGrant> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[test]
    fn an_open_live_tab_onto_the_session_lends_its_grant() {
        let picked = select_open_tab_grant(
            vec![
                tab("other-device", &SID.to_string(), true),
                tab(" DEVICE-A ", &SID.to_string().to_uppercase(), true),
            ],
            "device-a",
            SID,
        )
        .expect("the matching tab");
        assert_eq!(picked.grant_jti, "tab-jti");
        assert_eq!(picked.remote_terminal_id, "remote-term-1");
    }

    #[test]
    fn a_dead_tab_or_another_session_lends_nothing() {
        let other = uuid::Uuid::from_u128(99).to_string();
        assert_eq!(
            select_open_tab_grant(
                vec![
                    tab("device-a", &SID.to_string(), false),
                    tab("device-a", &other, true),
                ],
                "device-a",
                SID,
            ),
            None
        );
    }

    #[tokio::test]
    async fn an_open_tab_is_reused_and_nothing_is_minted() {
        let sender = Sender::new(vec![ended("graceful")]);
        let minted_called = std::sync::atomic::AtomicBool::new(false);
        let result = end_remote_session_with(
            vec![tab("device-a", &SID.to_string(), true)],
            "device-a",
            SID,
            || async {
                minted_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(minted("device-a"))
            },
            |g| sender.send(g),
        )
        .await;
        assert!(!minted_called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(result.outcome, EndOutcome::Ended);
        assert_eq!(result.via.as_deref(), Some("graceful"));
        assert_eq!(result.grant_source, EndGrantSource::OpenTab);
        assert_eq!(
            sender.calls(),
            vec![SendGrant::OpenTab {
                grant_jti: "tab-jti".to_string(),
                terminal_id: "remote-term-1".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn with_no_open_tab_a_grant_is_minted_and_no_terminal_is_named() {
        let sender = Sender::new(vec![ended("no_live_claude")]);
        let result = end_remote_session_with(
            vec![],
            "device-a",
            SID,
            || async { Ok(minted("device-a")) },
            |g| sender.send(g),
        )
        .await;
        assert_eq!(result.outcome, EndOutcome::Ended);
        assert_eq!(result.grant_source, EndGrantSource::Minted);
        assert_eq!(
            sender.calls(),
            vec![SendGrant::Fresh {
                grant: "minted-grant-jwt".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn a_stale_tab_grant_falls_back_to_one_mint() {
        let sender = Sender::new(vec![
            EndReply::local(
                EndOutcome::Refused,
                "attach_grant_expired: the grant expired",
            ),
            ended("graceful"),
        ]);
        let result = end_remote_session_with(
            vec![tab("device-a", &SID.to_string(), true)],
            "device-a",
            SID,
            || async { Ok(minted("device-a")) },
            |g| sender.send(g),
        )
        .await;
        assert_eq!(result.outcome, EndOutcome::Ended);
        assert_eq!(result.grant_source, EndGrantSource::Minted);
        assert_eq!(sender.calls().len(), 2);
        assert_eq!(
            sender.calls()[1],
            SendGrant::Fresh {
                grant: "minted-grant-jwt".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn a_target_refusal_under_a_tab_grant_is_final() {
        let sender = Sender::new(vec![EndReply::local(
            EndOutcome::Refused,
            "attach_terminal_mismatch: not the grant's terminal",
        )]);
        let result = end_remote_session_with(
            vec![tab("device-a", &SID.to_string(), true)],
            "device-a",
            SID,
            || async { panic!("a final refusal must not mint") },
            |g| sender.send(g),
        )
        .await;
        assert_eq!(result.outcome, EndOutcome::Refused);
        assert_eq!(result.grant_source, EndGrantSource::OpenTab);
    }

    #[tokio::test]
    async fn a_session_coord_places_elsewhere_is_refused_without_sending() {
        let sender = Sender::new(vec![]);
        let result = end_remote_session_with(
            vec![],
            "device-a",
            SID,
            || async { Ok(minted("device-b")) },
            |g| sender.send(g),
        )
        .await;
        assert_eq!(result.outcome, EndOutcome::Refused);
        assert!(result
            .reason
            .as_deref()
            .unwrap()
            .starts_with("target_mismatch"));
        assert!(sender.calls().is_empty());
    }

    #[test]
    fn mint_failures_map_to_the_outcome_vocabulary() {
        let url = "https://coord/coord/sessions/x/attach-grants";
        assert_eq!(
            mint_error_outcome(&format!(
                "remote_attach:session_not_found: (coord answered 404 for POST {url})"
            )),
            EndOutcome::NotFound
        );
        assert_eq!(
            mint_error_outcome("remote_attach:coord_unreachable: POST x: connect refused"),
            EndOutcome::Unknown
        );
        assert_eq!(
            mint_error_outcome(&format!(
                "remote_attach:rate_limited: (coord answered 429 for POST {url})"
            )),
            EndOutcome::Unknown
        );
        assert_eq!(
            mint_error_outcome(&format!(
                "remote_attach:coord_error: (coord answered 503 for POST {url})"
            )),
            EndOutcome::Unknown
        );
        assert_eq!(
            mint_error_outcome(&format!(
                "remote_attach:attach_forbidden:same_user: (coord answered 403 for POST {url})"
            )),
            EndOutcome::Refused
        );
    }

    #[tokio::test]
    async fn a_failed_mint_sends_nothing_and_never_reads_as_ended() {
        let sender = Sender::new(vec![]);
        let result = end_remote_session_with(
            vec![],
            "device-a",
            SID,
            || async { Err("remote_attach:coord_unreachable: POST x: timed out".to_string()) },
            |g| sender.send(g),
        )
        .await;
        assert_eq!(result.outcome, EndOutcome::Unknown);
        assert_eq!(result.grant_source, EndGrantSource::None);
        assert!(sender.calls().is_empty());
    }

    #[test]
    fn the_result_serializes_in_the_wire_vocabulary() {
        let result = RemoteSessionEndResult {
            outcome: EndOutcome::StillRunning,
            device_id: "d".to_string(),
            session_id: SID.to_string(),
            terminal_id: None,
            via: None,
            reason: Some("exit_stuck".to_string()),
            grant_source: EndGrantSource::OpenTab,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["outcome"], "still_running");
        assert_eq!(v["grantSource"], "open_tab");
        assert_eq!(v["deviceId"], "d");
        assert!(v["terminalId"].is_null());
    }
}

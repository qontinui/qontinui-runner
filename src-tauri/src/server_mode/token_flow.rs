//! Pending browser-login token flow state (Phase 3G-web-polish).
//!
//! Tracks an in-flight "connect with web login" flow started from the
//! runner's Settings UI:
//!
//! 1. The [`start_web_token_flow`] Tauri command generates a random `state`
//!    + records the current `backend_url`, stores it here, and opens the
//!    user's browser at `{backend_url}/connect-runner?state=...&callback=...`.
//! 2. The web page confirms, creates a runner token, and redirects the
//!    browser back to `http://127.0.0.1:<runner-port>/auth/runner-token-callback`.
//! 3. That callback handler pulls the state via [`TokenFlowStore::consume`],
//!    validates it, and applies the token via
//!    `commands::web_integration::apply_web_integration_settings` (which
//!    triggers re-registration).
//!
//! # Single-slot
//!
//! The store holds **one** pending flow at a time. Starting a new flow
//! cancels any previous in-progress flow. This matches the UX: only one
//! runner Settings page can be open, and clicking "Connect with web login"
//! again replaces the pending flow. It also means we don't need to deal
//! with UI state indirection to cancel partially-leaked flows.
//!
//! # TTL
//!
//! Pending flows expire 5 minutes after creation. [`TokenFlowStore::consume`]
//! checks the deadline and returns `None` if expired (also clearing the slot
//! so the stale entry doesn't linger).
//!
//! # Security
//!
//! - The `state` is 32 random bytes hex-encoded (64 chars) — long enough
//!   that even a targeted attacker who guesses the runner port cannot brute
//!   it.
//! - Consume is one-shot: after a successful `consume()` the slot is
//!   cleared, so a replayed callback from the browser's forward cache is
//!   rejected as 404 (which the handler renders as a friendly "flow
//!   expired" page).

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a pending flow remains valid after being started.
const PENDING_FLOW_TTL: Duration = Duration::from_secs(5 * 60);

/// A single pending browser-login flow.
#[derive(Debug, Clone)]
pub struct PendingTokenFlow {
    /// Hex-encoded random state (64 chars).
    pub state: String,
    /// Backend URL captured when the flow was started. Reused verbatim
    /// when the callback lands so the user's eventual registration uses
    /// the same backend they clicked "Connect" on.
    pub backend_url: String,
    /// Deadline after which the flow is considered expired.
    pub expires_at: Instant,
}

/// Single-slot store for the pending token flow.
///
/// The inner `Mutex<Option<...>>` is a deliberately simple shape: we have
/// at most one flow at a time, so a full map would be over-engineered.
pub struct TokenFlowStore {
    pending: Arc<Mutex<Option<PendingTokenFlow>>>,
}

impl Default for TokenFlowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenFlowStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
        }
    }

    /// Start a new flow, replacing any prior pending flow.
    ///
    /// Generates a fresh 32-byte random state, hex-encodes it, stores the
    /// `(state, backend_url, deadline)` tuple, and returns the state for
    /// embedding in the URL the runner opens in the browser.
    pub fn start(&self, backend_url: String) -> String {
        let mut bytes = [0u8; 32];
        // rand 0.9: `fill` on a mutable slice via thread_rng.
        use rand::RngCore;
        rand::rng().fill_bytes(&mut bytes);
        let state = hex::encode(bytes);

        let flow = PendingTokenFlow {
            state: state.clone(),
            backend_url,
            expires_at: Instant::now() + PENDING_FLOW_TTL,
        };

        let mut guard = self
            .pending
            .lock()
            .expect("TokenFlowStore mutex poisoned");
        *guard = Some(flow);
        state
    }

    /// Consume a pending flow by its state.
    ///
    /// Returns `Some(flow)` only if:
    /// 1. A flow is pending.
    /// 2. Its state matches the presented value exactly (case-sensitive).
    /// 3. The deadline has not passed.
    ///
    /// On any success path the slot is cleared — so a replay of the same
    /// callback URL returns `None`. On an expired match the slot is also
    /// cleared, preventing the stale entry from blocking a subsequent flow.
    /// On a mismatch the pending flow is left intact (so a spurious hit
    /// with a different state doesn't cancel the real flow).
    pub fn consume(&self, state: &str) -> Option<PendingTokenFlow> {
        let mut guard = self
            .pending
            .lock()
            .expect("TokenFlowStore mutex poisoned");

        let matched = match guard.as_ref() {
            Some(flow) if flow.state == state => true,
            _ => return None,
        };

        if !matched {
            return None;
        }

        // Pull the flow out of the slot unconditionally — either it's valid
        // (return Some) or it's expired (drop + return None). Either way the
        // slot ends up empty.
        let flow = guard.take().expect("matched branch implies Some");
        if Instant::now() > flow.expires_at {
            return None;
        }
        Some(flow)
    }

    /// Cancel any pending flow.
    ///
    /// Used when the Settings UI wants to abort (e.g., the user closed the
    /// browser tab without confirming). Currently only exposed for future
    /// use — the main flow relies on TTL expiry + replacement on re-start.
    #[allow(dead_code)]
    pub fn cancel(&self) {
        let mut guard = self
            .pending
            .lock()
            .expect("TokenFlowStore mutex poisoned");
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_produces_hex_64() {
        let store = TokenFlowStore::new();
        let state = store.start("https://example.test".to_string());
        assert_eq!(state.len(), 64);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn consume_returns_flow_when_state_matches() {
        let store = TokenFlowStore::new();
        let state = store.start("https://example.test".to_string());
        let flow = store.consume(&state).expect("should find pending flow");
        assert_eq!(flow.backend_url, "https://example.test");
        assert_eq!(flow.state, state);
    }

    #[test]
    fn consume_is_one_shot() {
        let store = TokenFlowStore::new();
        let state = store.start("https://example.test".to_string());
        assert!(store.consume(&state).is_some());
        assert!(store.consume(&state).is_none());
    }

    #[test]
    fn consume_returns_none_on_mismatch_and_keeps_pending() {
        let store = TokenFlowStore::new();
        let state = store.start("https://example.test".to_string());
        assert!(store.consume("wrong").is_none());
        // Original still valid.
        assert!(store.consume(&state).is_some());
    }

    #[test]
    fn start_replaces_prior_flow() {
        let store = TokenFlowStore::new();
        let state1 = store.start("https://one.test".to_string());
        let state2 = store.start("https://two.test".to_string());
        assert_ne!(state1, state2);
        // The first state is no longer valid.
        assert!(store.consume(&state1).is_none());
        // The second is.
        let flow = store.consume(&state2).expect("second flow should be valid");
        assert_eq!(flow.backend_url, "https://two.test");
    }

    #[test]
    fn cancel_clears_pending() {
        let store = TokenFlowStore::new();
        let state = store.start("https://example.test".to_string());
        store.cancel();
        assert!(store.consume(&state).is_none());
    }
}

//! Bearer-attached coord POST helpers for the install-effects producer.
//!
//! Every coord write attaches the device-JWT bearer via
//! [`crate::auth::attach_device_auth`] (the bin-target data-plane helper the
//! rest of the runner's coord writes use), degrading to anonymous when the
//! runner is unpaired — forward-compat with coord's multiuser Phase-2
//! enforcement, even though the install routes are anonymous today.
//!
//! declare and predict-and-check responses are LOAD-BEARING (the gate + the
//! returned outcome), so their errors are surfaced to the route caller (a
//! `Result`, never fire-and-forget). The later FS-observation push (Phase 3)
//! is the only fire-and-forget call in this producer.

use std::time::Duration;

use super::types::{
    DeclareRequest, FsObservationsRequest, PredictAndCheckResponse, VerifyOutcome, VerifyRequest,
};

/// Error talking to coord (surfaced to the route as a 502).
#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    #[error("build http client: {0}")]
    Client(String),
    #[error("POST {url}: {source}")]
    Send {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("POST {url} returned {status}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("parse {url} response: {source}")]
    Parse {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("decode {url} response shape: {message}")]
    DecodeShape { url: String, message: String },
}

/// A thin client bound to a coord base URL. Builds one `reqwest::Client` with a
/// generous timeout (coord predict/declare are pure + fast, but the network
/// path can be slow) and attaches the bearer per request.
pub struct CoordInstallClient {
    base: String,
    client: reqwest::Client,
}

impl CoordInstallClient {
    pub fn new(base: impl Into<String>) -> Result<Self, CoordError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CoordError::Client(e.to_string()))?;
        Ok(Self {
            base: base.into(),
            client,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    /// `POST /coord/installs/declare`. Persists the signature coord-side; the
    /// response (the predicted signature) is returned verbatim as JSON.
    pub async fn declare(&self, req: &DeclareRequest) -> Result<serde_json::Value, CoordError> {
        self.post_json(self.url("/coord/installs/declare"), req)
            .await
    }

    /// `POST /coord/installs/predict-and-check` — the autonomy gate. The
    /// returned [`PredictAndCheckResponse`] carries the typed `resolution` the
    /// route gates execution on.
    pub async fn predict_and_check(
        &self,
        req: &DeclareRequest,
    ) -> Result<PredictAndCheckResponse, CoordError> {
        let url = self.url("/coord/installs/predict-and-check");
        let body = self.post_json(url.clone(), req).await?;
        serde_json::from_value(body).map_err(|e| CoordError::DecodeShape {
            url,
            message: e.to_string(),
        })
    }

    /// `POST /coord/installs/verify` — the LOAD-BEARING post-install
    /// composition. Coord composes the D3 outcome from the FS/dep/security/type
    /// divergence and returns the [`VerifyOutcome`] the route surfaces. Errors
    /// are surfaced (not swallowed) — a verify failure is a real producer
    /// failure (the install ran, but we could not record/compose its outcome).
    pub async fn verify(&self, req: &VerifyRequest) -> Result<VerifyOutcome, CoordError> {
        let url = self.url("/coord/installs/verify");
        let body = self.post_json(url.clone(), req).await?;
        serde_json::from_value(body).map_err(|e| CoordError::DecodeShape {
            url,
            message: e.to_string(),
        })
    }

    /// `POST /coord/fs/observations` — FIRE-AND-FORGET best-effort. Pushes the
    /// manifest/lockfile pre/post blob shas under the run's correlation_id so
    /// coord's verify can read the observed Ξ_FS post-image. Unlike
    /// `agent_worktree::fs_observer` this attaches the device bearer (same as
    /// the other coord writes here). A failure logs and returns `Ok(())` — it
    /// NEVER fails the run. A short 10s timeout bounds the push.
    pub async fn push_fs_observations(&self, req: &FsObservationsRequest) {
        if req.observations.is_empty() {
            return;
        }
        let url = self.url("/coord/fs/observations");
        // Use a tighter client than the 30s declare/verify one — this is
        // best-effort and must not stall the route.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("install-effects: fs-observations client build failed: {e}");
                return;
            }
        };
        let rb = crate::auth::attach_device_auth(client.post(&url));
        match rb.json(req).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        url = %url,
                        status = status.as_u16(),
                        body = %body,
                        "install-effects: fs-observations push non-2xx (soft failure)"
                    );
                } else {
                    tracing::debug!(
                        url = %url,
                        n = req.observations.len(),
                        "install-effects: pushed fs observations"
                    );
                }
            }
            Err(e) => {
                tracing::debug!(
                    url = %url,
                    error = %e,
                    "install-effects: fs-observations push failed (coord unreachable)"
                );
            }
        }
    }

    async fn post_json<T: serde::Serialize>(
        &self,
        url: String,
        body: &T,
    ) -> Result<serde_json::Value, CoordError> {
        // Bin-target `crate::auth` (NOT the lib copy) so this producer's coord
        // writes feed the SAME `DATA_PLANE_TOTAL`/`AUTHED` coverage counters as
        // the rest of the runner's bin-target data plane (the dogfood auth
        // signal). Both copies read the same on-disk token, but only one set of
        // counters is the one operators watch.
        let rb = crate::auth::attach_device_auth(self.client.post(&url));
        let resp = rb
            .json(body)
            .send()
            .await
            .map_err(|source| CoordError::Send {
                url: url.clone(),
                source,
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CoordError::Status {
                url,
                status: status.as_u16(),
                body,
            });
        }
        resp.json::<serde_json::Value>()
            .await
            .map_err(|source| CoordError::Parse { url, source })
    }
}

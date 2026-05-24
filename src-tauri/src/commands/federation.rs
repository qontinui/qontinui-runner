//! Tauri command for fetching federation reports from coord.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFederationReportsArgs {
    pub since: Option<String>,
    pub limit: Option<u32>,
    pub device_id: Option<String>,
    pub has_failures: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FederationReportItem {
    pub report_id: String,
    pub tenant_id: Option<String>,
    pub device_id: String,
    pub session_id: String,
    pub account_name: String,
    pub pushed: u32,
    pub pulled: u32,
    pub unchanged: u32,
    pub failed: u32,
    pub failed_names: Option<Vec<String>>,
    pub reported_at: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FederationReportsResult {
    pub items: Vec<FederationReportItem>,
}

#[tauri::command]
pub async fn get_federation_reports(
    args: GetFederationReportsArgs,
) -> Result<FederationReportsResult, String> {
    get_federation_reports_impl(args)
        .await
        .map_err(|e| e.to_string())
}

async fn get_federation_reports_impl(
    args: GetFederationReportsArgs,
) -> anyhow::Result<FederationReportsResult> {
    use qontinui_runner_lib::observable_bridge::memory_client;

    let base = memory_client::coord_http_base()
        .ok_or_else(|| anyhow::anyhow!("no coord_url in active profile"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut url = format!("{base}/coord/federation/reports");
    let mut params = Vec::new();
    if let Some(since) = &args.since {
        params.push(format!("since={since}"));
    }
    if let Some(limit) = args.limit {
        params.push(format!("limit={limit}"));
    }
    if let Some(device_id) = &args.device_id {
        params.push(format!("device_id={device_id}"));
    }
    if let Some(true) = args.has_failures {
        params.push("has_failures=true".to_string());
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    let tenant_id =
        qontinui_runner_lib::pair::read_paired_tenant_id_from_disk().and_then(|s| {
            uuid::Uuid::parse_str(s.trim()).ok()
        });

    let mut req = client.get(&url);
    if let Some(tid) = tenant_id {
        req = req.header("X-Qontinui-Tenant-Id", tid.to_string());
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let len = body.len().min(400);
        anyhow::bail!("GET {url} -> HTTP {status}: {}", &body[..len]);
    }

    let result: FederationReportsResult = resp.json().await?;
    Ok(result)
}
